use serde::{Deserialize, Serialize};

/// One captured, redacted transcript line plus provenance. Deliberately generic:
/// the client stays dumb and ships redacted raw JSONL lines; the server parses
/// per-tool semantics later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub schema: String,
    pub tool: String,
    pub project: String,
    pub session_id: String,
    pub captured_at_ms: u128,
    /// Redacted JSONL line content.
    pub line: String,
}

pub fn now_ms() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
