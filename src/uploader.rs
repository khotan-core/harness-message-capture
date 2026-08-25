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

/// A near-megabyte batch measured 15 seconds on the wire with four customers
/// uploading at once, so the old 20 second ceiling turned a healthy upload into
/// `No answer in time` and cost the route the rest of its pass.
const INGEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The identity check carries no records, so it has no reason to be slow.
const IDENTITY_TIMEOUT: Duration = Duration::from_secs(20);

/// Endpoint answers to `GET /api/v1/me`, keyed by origin and key fingerprint and
/// holding the organization the endpoint named, so a later pass reuses it
/// instead of asking again.
static VERIFIED: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();

pub enum Upload {
    Ok,
    Retry(String),
    Blocked(String),
    /// The server refused the body for its size, not for its contents. The
    /// caller should send fewer records rather than give up on the route.
    TooLarge(String),
}

/// What the endpoint says a key's organization is, once any organization already
/// bound to the queue has been enforced against it.
pub enum OrgOutcome {
    /// The organization to stamp on the batch. Concrete, because the endpoint
    /// requires one.
    Resolved(String),
    Retry(String),
    Blocked(String),
}

/// Ask the endpoint which organization a key belongs to, enforce any
/// organization already bound to the route, and remember the answer. The caller
/// pins the resolved organization to the queue when the route carried none.
pub fn resolve_org(route: &RouteRef, api_key: &str) -> OrgOutcome {
    let cache_key = format!(
        "{}\n{:016x}",
        route.api_url,
        destination::key_fingerprint(api_key)
    );
    let cache = VERIFIED.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(org) = cache.lock().ok().and_then(|verified| {
        verified
            .get(&cache_key)
            .filter(|(_, at)| at.elapsed() < VERIFY_TTL)
            .map(|(org, _)| org.clone())
    }) {
        return enforce(route, org);
    }

    let url = format!("{}/api/v1/me", route.api_url);
    let response = ureq::get(&url)
        .set("x-api-key", api_key)
        .set("Accept", "application/json")
        .timeout(IDENTITY_TIMEOUT)
        .call();
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            return match classify_error(error, "identity check") {
                // A size refusal on a GET says nothing about the batch waiting
                // behind it, so it must not shrink one.
                Upload::TooLarge(reason) | Upload::Blocked(reason) => OrgOutcome::Blocked(reason),
                Upload::Retry(reason) => OrgOutcome::Retry(reason),
                Upload::Ok => OrgOutcome::Blocked("identity check gave no answer".into()),
            };
        }
    };
    let principal: Principal = match response
        .into_string()
        .map_err(|error| error.to_string())
        .and_then(|body| serde_json::from_str(&body).map_err(|error| error.to_string()))
    {
        Ok(principal) => principal,
        Err(_) => return OrgOutcome::Blocked("The /api/v1/me body was not usable".into()),
    };
    let org = match principal.organization_id {
        Some(org) if !org.trim().is_empty() => org,
        _ => return OrgOutcome::Blocked("The identity check did not name an organization".into()),
    };
    if let Ok(mut verified) = cache.lock() {
        verified.insert(cache_key, (org.clone(), Instant::now()));
    }
    enforce(route, org)
}

/// Hold the endpoint's answer against any organization already bound to the
/// queue. A route with none takes whatever the key resolves to.
fn enforce(route: &RouteRef, org: String) -> OrgOutcome {
    match &route.org_id {
        Some(bound) if bound != &org => OrgOutcome::Blocked(
            "Key resolves to a different org than this queue is bound to".into(),
        ),
        _ => OrgOutcome::Resolved(org),
    }
}

/// Ship one batch with a concrete organization. The caller has already resolved
/// credentials and the organization; this only puts them on the wire.
pub fn post_batch(
    device_id: &str,
    route: &RouteRef,
    api_key: &str,
    organization_id: &str,
    records: &[Record],
) -> Upload {
    if records.is_empty() {
        return Upload::Ok;
    }
    let body = match serde_json::to_string(&Batch {
        device_id,
        organization_id,
        records,
    }) {
        Ok(b) => b,
        Err(_) => return Upload::Blocked("Could not serialize the batch".into()),
    };

    let endpoint = format!("{}{INGEST_PATH}", route.api_url);
    let resp = ureq::post(&endpoint)
        .set("x-api-key", api_key)
        .set("Content-Type", "application/json")
        .timeout(INGEST_TIMEOUT)
        .send_string(&body);

    classify_response(resp, "ingest")
}

fn classify_response(response: Result<ureq::Response, ureq::Error>, operation: &str) -> Upload {
    match response {
        Ok(_) => Upload::Ok,
        Err(error) => classify_error(error, operation),
    }
}

fn classify_error(error: ureq::Error, operation: &str) -> Upload {
    match error {
        ureq::Error::Status(code, response) => {
            let detail = describe(operation, code, response);
            if (500..=599).contains(&code) || code == 429 {
                Upload::Retry(format!("Server error or rate limit · {detail}"))
            } else if code == 413 || reads_as_size_refusal(&detail) {
                Upload::TooLarge(detail)
            } else {
                Upload::Blocked(format!(
                    "Key or request refused · {detail} · queue keeps the lines"
                ))
            }
        }
        error => Upload::Retry(transport_reason(&error)),
    }
}

/// `ingest 413 · Body exceeded 1mb limit`. The status code and the server's own
/// words are the difference between a reason someone can act on and a shrug.
fn describe(operation: &str, code: u16, response: ureq::Response) -> String {
    match body_snippet(response) {
        Some(snippet) => format!("{operation} {code} · {snippet}"),
        None => format!("{operation} {code}"),
    }
}

/// Proxies and frameworks disagree on the status for an oversized body. Read
/// the words too, so a 400 that means "too big" is not filed as a bad key.
fn reads_as_size_refusal(detail: &str) -> bool {
    const HINTS: [&str; 6] = [
        "too large",
        "too big",
        "entity too large",
        "body exceeded",
        "payload size",
        "request size",
    ];
    let lower = detail.to_lowercase();
    HINTS.iter().any(|hint| lower.contains(hint))
}

/// One short line of the error body. It is the server's text, not ours, so it
/// goes through the redactor before it can reach a log file.
fn body_snippet(response: ureq::Response) -> Option<String> {
    let body = response.into_string().ok()?;
    let flat = crate::redact::scrub(&body)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.is_empty() {
        return None;
    }
    Some(clip(&flat, SNIPPET_CHARS))
}

/// Longest error body we quote. A whole HTML error page on one log line helps
/// nobody.
const SNIPPET_CHARS: usize = 80;

fn clip(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(chars).collect();
    format!("{}…", kept.trim_end())
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
                org_id: Some(org_id.into()),
                api_url: origin,
                key_fingerprint: Some(destination::key_fingerprint("test-secret-key")),
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

    fn resolved(route: &RouteRef, key: &str) -> String {
        match resolve_org(route, key) {
            OrgOutcome::Resolved(org) => org,
            OrgOutcome::Retry(reason) | OrgOutcome::Blocked(reason) => {
                panic!("expected a resolved org: {reason}")
            }
        }
    }

    #[test]
    fn resolves_org_then_sends_homogeneous_batch_without_secret_body() {
        let (origin, requests, handle) =
            spawn_server(vec![(200, r#"{"organizationId":"org-test"}"#), (204, "")]);
        let (route, env_path) = fixture_route(origin, "org-test");
        let org = resolved(&route, "test-secret-key");
        assert_eq!(org, "org-test");
        assert!(matches!(
            post_batch("device", &route, "test-secret-key", &org, &[record()]),
            Upload::Ok
        ));
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
    fn an_undeclared_route_takes_whatever_org_the_key_resolves_to() {
        let (origin, _requests, handle) =
            spawn_server(vec![(200, r#"{"organizationId":"org-from-key"}"#)]);
        let (mut route, env_path) = fixture_route(origin, "unused");
        route.org_id = None;
        assert_eq!(resolved(&route, "test-secret-key"), "org-from-key");
        handle.join().unwrap();
        let _ = fs::remove_file(env_path);
    }

    #[test]
    fn organization_mismatch_blocks() {
        let (origin, _requests, handle) =
            spawn_server(vec![(200, r#"{"organizationId":"wrong-org"}"#)]);
        let (route, env_path) = fixture_route(origin, "expected-org");
        assert!(matches!(
            resolve_org(&route, "test-secret-key"),
            OrgOutcome::Blocked(_)
        ));
        handle.join().unwrap();
        let _ = fs::remove_file(env_path);
    }

    #[test]
    fn an_endpoint_that_names_no_org_blocks() {
        let (origin, _requests, handle) = spawn_server(vec![(200, r#"{"role":"admin"}"#)]);
        let (mut route, env_path) = fixture_route(origin, "unused");
        route.org_id = None;
        assert!(matches!(
            resolve_org(&route, "test-secret-key"),
            OrgOutcome::Blocked(_)
        ));
        handle.join().unwrap();
        let _ = fs::remove_file(env_path);
    }

    #[test]
    fn identity_server_failure_is_retryable() {
        let (origin, _requests, handle) = spawn_server(vec![(500, "{}")]);
        let (route, env_path) = fixture_route(origin, "org-test");
        assert!(matches!(
            resolve_org(&route, "test-secret-key"),
            OrgOutcome::Retry(_)
        ));
        handle.join().unwrap();
        let _ = fs::remove_file(env_path);
    }

    #[test]
    fn a_refusal_names_its_status_and_the_server_words() {
        let (origin, _requests, handle) =
            spawn_server(vec![(403, r#"{"error":"route is not enrolled"}"#)]);
        let (route, env_path) = fixture_route(origin, "org-test");
        let reason = match post_batch("device", &route, "test-secret-key", "org-test", &[record()])
        {
            Upload::Blocked(reason) => reason,
            _ => panic!("a 403 blocks"),
        };
        assert!(reason.contains("ingest 403"), "{reason}");
        assert!(reason.contains("route is not enrolled"), "{reason}");
        handle.join().unwrap();
        let _ = fs::remove_file(env_path);
    }

    #[test]
    fn an_oversized_body_asks_for_a_smaller_batch_instead_of_blocking() {
        let (origin, _requests, handle) = spawn_server(vec![(413, "Body exceeded 1mb limit")]);
        let (route, env_path) = fixture_route(origin, "org-test");
        let reason = match post_batch("device", &route, "test-secret-key", "org-test", &[record()])
        {
            Upload::TooLarge(reason) => reason,
            _ => panic!("a 413 is a size refusal, not a bad key"),
        };
        assert!(reason.contains("ingest 413"), "{reason}");
        assert!(reason.contains("Body exceeded 1mb limit"), "{reason}");
        handle.join().unwrap();
        let _ = fs::remove_file(env_path);
    }

    #[test]
    fn a_400_that_means_too_big_is_read_as_a_size_refusal() {
        assert!(reads_as_size_refusal(
            "ingest 400 · Request entity too large"
        ));
        assert!(reads_as_size_refusal("ingest 400 · payload size exceeded"));
        assert!(!reads_as_size_refusal("ingest 401 · invalid key"));
    }

    #[test]
    fn a_long_error_body_is_clipped_to_one_line() {
        let clipped = clip(&"x".repeat(200), SNIPPET_CHARS);
        assert_eq!(clipped.chars().count(), SNIPPET_CHARS + 1);
        assert!(clipped.ends_with('…'));
        assert_eq!(clip("short", SNIPPET_CHARS), "short");
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

        let org_one = resolved(&route_one, "test-secret-key");
        assert!(matches!(
            post_batch("device", &route_one, "test-secret-key", &org_one, &[first]),
            Upload::Ok
        ));
        let org_two = resolved(&route_two, "test-secret-key");
        assert!(matches!(
            post_batch("device", &route_two, "test-secret-key", &org_two, &[second]),
            Upload::Ok
        ));
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
