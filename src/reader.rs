use crate::store::{self, ReadFilter};
use anyhow::{Context, Result};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ReadOpts {
    pub dir: std::path::PathBuf,
    pub filter: ReadFilter,
    pub limit: usize,
    pub raw: bool,
}

/// Print a bounded recent window of stored messages for local proof/inspection.
pub fn run(opts: ReadOpts) -> Result<()> {
    let mut records = store::list_records(&opts.dir, &opts.filter)
        .with_context(|| format!("read inbox {}", opts.dir.display()))?;
    if records.is_empty() {
        println!(
            "no records in {} (filters: tool={:?} project={:?} session={:?} thread={:?})",
            opts.dir.display(),
            opts.filter.tool,
            opts.filter.project,
            opts.filter.session_id,
            opts.filter.thread_id
        );
        return Ok(());
    }

    if opts.limit > 0 && records.len() > opts.limit {
        let skip = records.len() - opts.limit;
        records = records.split_off(skip);
    }

    println!(
        "showing {} record(s) from {}",
        records.len(),
        opts.dir.display()
    );
    println!();

    let mut last_thread = String::new();
    for r in &records {
        let thread = r
            .thread_id
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(&r.session_id);
        let thread_key = format!("{}/{}/{}", r.tool, r.project, thread);
        if thread_key != last_thread {
            println!("── {} · {} · thread {} ──", r.tool, r.project, thread);
            last_thread = thread_key;
        }
        let role_prefix = match r.agent_role.as_deref() {
            Some("subagent") => format!("subagent {} ", r.session_id),
            _ => String::new(),
        };
        if opts.raw {
            println!("[{}] {role_prefix}{}", r.captured_at_ms, r.line);
            continue;
        }
        match extract_message(&r.line) {
            Some((role, text)) => {
                let preview = truncate(&collapse_ws(&text), 400);
                println!("  {role_prefix}[{role}] {preview}");
            }
            None => {
                let preview = truncate(&r.line, 240);
                println!(
                    "  {role_prefix}[raw {}/{}] {}",
                    r.tool, r.session_id, preview
                );
            }
        }
    }
    Ok(())
}

/// Best-effort extraction for Cursor-style (and similar) transcript lines.
/// Returns (role, text) when recognizable; otherwise None so the caller can
/// fall back to showing the preserved redacted JSONL.
pub fn extract_message(line: &str) -> Option<(String, String)> {
    let v: Value = serde_json::from_str(line).ok()?;

    // Cursor agent transcripts: {"role":"user"|"assistant","message":{"content":[...]}}
    if let Some(role) = v.get("role").and_then(|r| r.as_str()) {
        if let Some(text) =
            message_text(v.get("message")).or_else(|| content_text(v.get("content")))
        {
            if !text.trim().is_empty() {
                return Some((role.to_string(), text));
            }
        }
        // role present but no text — still useful for tool/system events
        if matches!(role, "user" | "assistant" | "system" | "tool") {
            return Some((role.to_string(), format!("<{role} event>")));
        }
    }

    // Claude Code-ish: {"type":"user"|"assistant", "message":{...}}
    if let Some(ty) = v.get("type").and_then(|t| t.as_str()) {
        if matches!(ty, "user" | "assistant" | "system") {
            if let Some(text) =
                message_text(v.get("message")).or_else(|| content_text(v.get("content")))
            {
                if !text.trim().is_empty() {
                    return Some((ty.to_string(), text));
                }
            }
        }
    }

    None
}

fn message_text(message: Option<&Value>) -> Option<String> {
    let message = message?;
    content_text(message.get("content")).or_else(|| {
        message
            .get("text")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
    })
}

fn content_text(content: Option<&Value>) -> Option<String> {
    let content = content?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    let arr = content.as_array()?;
    let mut parts = Vec::new();
    for item in arr {
        if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
            parts.push(t.to_string());
            continue;
        }
        if item.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                parts.push(t.to_string());
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::new();
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Helper used by tests that want records without printing.
#[cfg(test)]
pub fn load(
    dir: &std::path::Path,
    filter: &ReadFilter,
    limit: usize,
) -> Result<Vec<store::StoredRecord>> {
    let mut records = store::list_records(dir, filter)?;
    if limit > 0 && records.len() > limit {
        let skip = records.len() - limit;
        records = records.split_off(skip);
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Record;
    use crate::store;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn extracts_cursor_user_and_assistant() {
        let user =
            r#"{"role":"user","message":{"content":[{"type":"text","text":"hello world"}]}}"#;
        let (role, text) = extract_message(user).unwrap();
        assert_eq!(role, "user");
        assert!(text.contains("hello world"));

        let asst =
            r#"{"role":"assistant","message":{"content":[{"type":"text","text":"hi back"}]}}"#;
        let (role, text) = extract_message(asst).unwrap();
        assert_eq!(role, "assistant");
        assert!(text.contains("hi back"));
    }

    #[test]
    fn falls_back_for_unknown_shapes() {
        assert!(extract_message(r#"{"foo":1}"#).is_none());
        assert!(extract_message("not-json").is_none());
    }

    #[test]
    fn extracts_claude_type_shape() {
        let line = r#"{"type":"user","message":{"content":[{"type":"text","text":"ping"}]}}"#;
        let (role, text) = extract_message(line).unwrap();
        assert_eq!(role, "user");
        assert!(text.contains("ping"));
    }

    #[test]
    fn read_limit_keeps_newest() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("khotan-reader-test-{nanos}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let records: Vec<Record> = (1..=5)
            .map(|i| Record {
                schema: "v1".into(),
                tool: "cursor".into(),
                project: "p".into(),
                session_id: "s".into(),
                thread_id: None,
                agent_role: None,
                seq: None,
                captured_at_ms: i,
                line: format!(
                    r#"{{"role":"user","message":{{"content":[{{"type":"text","text":"m{i}"}}]}}}}"#
                ),
            })
            .collect();
        store::append_batch(&dir, "dev", &records).unwrap();
        let got = load(&dir, &ReadFilter::default(), 2).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got[0].line.contains("m4"));
        assert!(got[1].line.contains("m5"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
