use crate::config::Config;
use crate::record::Record;
use serde::Serialize;

#[derive(Serialize)]
struct Batch<'a> {
    device_id: &'a str,
    records: &'a [Record],
}

/// Result of an upload attempt. `Retry` means keep the records spooled and try
/// again later (network/5xx); `Ok`/`Drop` both mean stop retrying this batch.
/// The failure variants carry a short reason so the daemon can surface it.
pub enum Upload {
    Ok,
    /// Server rejected the batch (4xx) — drop it rather than loop forever.
    Drop(String),
    Retry(String),
}

pub fn send(cfg: &Config, records: &[Record]) -> Upload {
    if records.is_empty() {
        return Upload::Ok;
    }
    let body = match serde_json::to_string(&Batch {
        device_id: &cfg.device_id,
        records,
    }) {
        Ok(b) => b,
        Err(e) => return Upload::Drop(format!("could not serialize batch: {e}")),
    };

    let resp = ureq::post(&cfg.endpoint)
        .set("Authorization", &format!("Bearer {}", cfg.token))
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(20))
        .send_string(&body);

    match resp {
        Ok(_) => Upload::Ok,
        Err(ureq::Error::Status(code, _)) => {
            if (500..=599).contains(&code) || code == 429 {
                Upload::Retry(format!("server returned {code}"))
            } else {
                Upload::Drop(format!("server rejected batch with {code}"))
            }
        }
        // Transport error (offline, DNS, timeout) — keep and retry.
        Err(e) => Upload::Retry(transport_reason(&e)),
    }
}

/// Collapse ureq's verbose transport errors into something readable on one line.
fn transport_reason(e: &ureq::Error) -> String {
    let raw = e.to_string();
    let lower = raw.to_lowercase();
    if lower.contains("connection refused") {
        "endpoint unreachable (connection refused)".into()
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "endpoint timed out".into()
    } else if lower.contains("dns") || lower.contains("resolve") {
        "could not resolve endpoint host".into()
    } else {
        format!("upload failed: {raw}")
    }
}
