use serde::{Deserialize, Serialize};

/// Wire schema for newly captured records. Pending spool rows may still be `v1`.
pub const SCHEMA: &str = "v2";

/// One captured, redacted transcript line plus provenance. Deliberately generic:
/// the client stays dumb and ships redacted raw JSONL lines; the server parses
/// per-tool semantics later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub schema: String,
    pub tool: String,
    pub project: String,
    pub session_id: String,
    /// Root chat. Equals `session_id` for a root transcript. `None` only on a
    /// v1 row that was queued before this field existed.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// `"root"` or `"subagent"`. `None` only on a queued v1 row.
    #[serde(default)]
    pub agent_role: Option<String>,
    /// Byte offset of this line in the source file. `None` only on a queued v1
    /// row. Serialized as JSON `null` so the server hash treats it as absent.
    #[serde(default)]
    pub seq: Option<u64>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_record_deserializes_and_reserializes_null_fields() {
        let json = r#"{"schema":"v1","tool":"cursor","project":"p","session_id":"s","captured_at_ms":1,"line":"{}"}"#;
        let record: Record = serde_json::from_str(json).unwrap();
        assert!(record.thread_id.is_none());
        assert!(record.agent_role.is_none());
        assert!(record.seq.is_none());
        let out = serde_json::to_value(&record).unwrap();
        assert_eq!(out["seq"], serde_json::Value::Null);
        assert_eq!(out["thread_id"], serde_json::Value::Null);
        assert_eq!(out["agent_role"], serde_json::Value::Null);
        assert_ne!(out["seq"], serde_json::json!(0));
    }
}
