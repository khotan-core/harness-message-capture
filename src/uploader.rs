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
pub enum Upload {
    Ok,
    /// Server rejected the batch (4xx) — drop it rather than loop forever.
    Drop,
    Retry,
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
        Err(_) => return Upload::Drop,
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
                Upload::Retry
            } else {
                Upload::Drop
            }
        }
        // Transport error (offline, DNS, timeout) — keep and retry.
        Err(_) => Upload::Retry,
    }
}
