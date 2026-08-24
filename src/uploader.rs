use crate::destination::{self, RouteRef};
use crate::record::Record;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Serialize)]
struct Batch<'a> {
    device_id: &'a str,
    organization_id: &'a str,
    records: &'a [Record],
}

#[derive(Deserialize)]
struct Principal {
    #[serde(rename = "organizationId")]
    organization_id: Option<String>,
}

const VERIFY_TTL: Duration = Duration::from_secs(5 * 60);
const INGEST_PATH: &str = "/ingest";
static VERIFIED: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

pub enum Upload {
    Ok,
    Retry(String),
    Blocked(String),
}

pub fn send(device_id: &str, route: &RouteRef, records: &[Record]) -> Upload {
    if records.is_empty() {
        return Upload::Ok;
    }
    let credentials = match destination::read_credentials(route) {
        Ok(credentials) => credentials,
        Err(_) => return Upload::Blocked("Dest file gone or URL/org changed after queue".into()),
    };
    match verify_org(route, &credentials.api_key) {
        Upload::Ok => {}
        outcome => return outcome,
    }

    let body = match serde_json::to_string(&Batch {
        device_id,
        organization_id: &route.org_id,
        records,
    }) {
        Ok(b) => b,
        Err(_) => return Upload::Blocked("Could not serialize the batch".into()),
    };

    let endpoint = format!("{}{INGEST_PATH}", route.api_url);
    let resp = ureq::post(&endpoint)
        .set("x-api-key", &credentials.api_key)
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(20))
        .send_string(&body);

    classify_response(resp, "ingest")
}

fn verify_org(route: &RouteRef, api_key: &str) -> Upload {
    let cache_key = format!(
        "{}\n{}\n{:016x}",
        route.api_url,
        route.org_id,
        key_fingerprint(api_key)
    );
    let cache = VERIFIED.get_or_init(|| Mutex::new(HashMap::new()));
    if cache
        .lock()
        .ok()
        .and_then(|verified| verified.get(&cache_key).copied())
        .is_some_and(|at| at.elapsed() < VERIFY_TTL)
    {
        return Upload::Ok;
    }

    let url = format!("{}/api/v1/me", route.api_url);
    let response = ureq::get(&url)
        .set("x-api-key", api_key)
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(20))
        .call();
    let response = match response {
        Ok(response) => response,
        Err(error) => return classify_error(error, "identity check"),
    };
    let principal: Principal = match response
        .into_string()
        .map_err(|error| error.to_string())
        .and_then(|body| serde_json::from_str(&body).map_err(|error| error.to_string()))
    {
        Ok(principal) => principal,
        Err(_) => return Upload::Blocked("The /api/v1/me body was not usable".into()),
    };
    if principal.organization_id.as_deref() != Some(route.org_id.as_str()) {
        return Upload::Blocked("Key's org does not match KHOTAN_ORG_ID".into());
    }
    if let Ok(mut verified) = cache.lock() {
        verified.insert(cache_key, Instant::now());
    }
    Upload::Ok
}

fn key_fingerprint(api_key: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in api_key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn classify_response(response: Result<ureq::Response, ureq::Error>, operation: &str) -> Upload {
    match response {
        Ok(_) => Upload::Ok,
        Err(error) => classify_error(error, operation),
    }
}

fn classify_error(error: ureq::Error, _operation: &str) -> Upload {
    match error {
        ureq::Error::Status(code, _) if (500..=599).contains(&code) || code == 429 => {
            Upload::Retry("Server error or rate limit".into())
        }
        ureq::Error::Status(_, _) => {
            Upload::Blocked("Key or request refused. Queue keeps the lines".into())
        }
        error => Upload::Retry(transport_reason(&error)),
    }
}

/// Collapse ureq's verbose transport errors into something readable on one line.
fn transport_reason(e: &ureq::Error) -> String {
    let raw = e.to_string();
    let lower = raw.to_lowercase();
    if lower.contains("connection refused") {
        "Host is up in DNS, port is closed".into()
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "No answer in time".into()
    } else if lower.contains("dns") || lower.contains("resolve") {
        "DNS failed".into()
    } else {
        let _ = raw;
        "Upload failed".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture_route(origin: String, org_id: &str) -> (RouteRef, PathBuf) {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("hmc-upload-{stamp}.env"));
        fs::write(
            &path,
            format!(
                "KHOTAN_API_URL='{origin}'\nKHOTAN_API_KEY='test-secret-key'\nKHOTAN_ORG_ID='{org_id}'\n"
            ),
        )
        .unwrap();
        (
            RouteRef {
                id: format!("route-{stamp}"),
                org_id: org_id.into(),
                api_url: origin,
                credential_path: path.clone(),
                label: "customer".into(),
            },
            path,
        )
    }

    fn record() -> Record {
        Record {
            schema: "v1".into(),
            tool: "cursor".into(),
            project: "customer".into(),
            session_id: "session".into(),
            thread_id: None,
            agent_role: None,
            seq: None,
            captured_at_ms: 1,
            line: "{}".into(),
        }
    }

    fn spawn_server(
        responses: Vec<(u16, &'static str)>,
    ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let handle = thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut data = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let count = stream.read(&mut chunk).unwrap();
                    if count == 0 {
                        break;
                    }
                    data.extend_from_slice(&chunk[..count]);
                    if let Some(header_end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&data[..header_end + 4]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if data.len() >= header_end + 4 + length {
                            break;
                        }
                    }
                }
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&data).to_string());
                let reason = if status == 200 { "OK" } else { "Error" };
                write!(
                    stream,
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (origin, requests, handle)
    }

    #[test]
    fn verifies_org_then_sends_homogeneous_batch_without_secret_body() {
        let (origin, requests, handle) =
            spawn_server(vec![(200, r#"{"organizationId":"org-test"}"#), (204, "")]);
        let (route, env_path) = fixture_route(origin, "org-test");
        assert!(matches!(send("device", &route, &[record()]), Upload::Ok));
        handle.join().unwrap();
        let requests = requests.lock().unwrap();
        assert!(requests[0].starts_with("GET /api/v1/me "));
        assert!(requests[0]
            .to_ascii_lowercase()
            .contains("x-api-key: test-secret-key"));
        assert!(requests[1].starts_with("POST /ingest "));
        assert!(requests[1].contains(r#""organization_id":"org-test""#));
        assert!(!requests[1]
            .split("\r\n\r\n")
            .nth(1)
            .unwrap_or("")
            .contains("test-secret-key"));
        let _ = fs::remove_file(env_path);
    }

    #[test]
    fn organization_mismatch_blocks_and_never_posts() {
        let (origin, _requests, handle) =
            spawn_server(vec![(200, r#"{"organizationId":"wrong-org"}"#)]);
        let (route, env_path) = fixture_route(origin, "expected-org");
        assert!(matches!(
            send("device", &route, &[record()]),
            Upload::Blocked(_)
        ));
        handle.join().unwrap();
        let _ = fs::remove_file(env_path);
    }

    #[test]
    fn identity_server_failure_is_retryable() {
        let (origin, _requests, handle) = spawn_server(vec![(500, "{}")]);
        let (route, env_path) = fixture_route(origin, "org-test");
        assert!(matches!(
            send("device", &route, &[record()]),
            Upload::Retry(_)
        ));
        handle.join().unwrap();
        let _ = fs::remove_file(env_path);
    }

    #[test]
    fn two_customer_origins_receive_only_their_own_records() {
        let (origin_one, requests_one, handle_one) =
            spawn_server(vec![(200, r#"{"organizationId":"org-one"}"#), (204, "")]);
        let (origin_two, requests_two, handle_two) =
            spawn_server(vec![(200, r#"{"organizationId":"org-two"}"#), (204, "")]);
        let (route_one, env_one) = fixture_route(origin_one, "org-one");
        let (route_two, env_two) = fixture_route(origin_two, "org-two");
        let mut first = record();
        first.line = "only-customer-one".into();
        let mut second = record();
        second.line = "only-customer-two".into();

        assert!(matches!(send("device", &route_one, &[first]), Upload::Ok));
        assert!(matches!(send("device", &route_two, &[second]), Upload::Ok));
        handle_one.join().unwrap();
        handle_two.join().unwrap();

        let one = requests_one.lock().unwrap().join("\n");
        let two = requests_two.lock().unwrap().join("\n");
        assert!(one.contains("only-customer-one"));
        assert!(!one.contains("only-customer-two"));
        assert!(two.contains("only-customer-two"));
        assert!(!two.contains("only-customer-one"));
        let _ = fs::remove_file(env_one);
        let _ = fs::remove_file(env_two);
    }
}
