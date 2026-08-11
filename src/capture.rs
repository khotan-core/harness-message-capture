use crate::config::state_dir;
use crate::record::{now_ms, Record};
use crate::redact;
use crate::sources::{jsonl_files, Source};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Cap the bytes read from a single file per scan pass so one enormous append
/// can't spike memory. Anything beyond is picked up on the next pass.
const MAX_READ_PER_FILE: u64 = 4 * 1024 * 1024;

/// Persistent per-file byte offsets so restarts resume instead of re-sending.
pub struct Offsets {
    map: HashMap<String, u64>,
    path: PathBuf,
}

impl Offsets {
    pub fn load() -> Offsets {
        let path = state_dir().join("offsets.json");
        let map = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, u64>>(&s).ok())
            .unwrap_or_default();
        Offsets { map, path }
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, serde_json::to_string(&self.map)?)?;
        Ok(())
    }

    fn get(&self, key: &str) -> u64 {
        *self.map.get(key).unwrap_or(&0)
    }

    fn set(&mut self, key: String, val: u64) {
        self.map.insert(key, val);
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}

/// Read newly-appended complete lines across all sources, redact them, and
/// return the resulting records. Offsets are advanced only past whole lines.
pub fn collect_new(sources: &[Source], offsets: &mut Offsets) -> Vec<Record> {
    let mut records = Vec::new();
    for src in sources {
        for file in jsonl_files(&src.root) {
            if let Err(_e) = read_file(src, &file, offsets, &mut records) {
                // Non-fatal: skip this file this pass (e.g. transient perm error).
                continue;
            }
        }
    }
    records
}

fn read_file(
    src: &Source,
    file: &Path,
    offsets: &mut Offsets,
    out: &mut Vec<Record>,
) -> Result<()> {
    let key = file.to_string_lossy().to_string();
    let meta = fs::metadata(file).with_context(|| format!("stat {}", file.display()))?;
    let len = meta.len();
    let mut offset = offsets.get(&key);

    // File shrank (rotation/truncation) — restart from the beginning.
    if len < offset {
        offset = 0;
    }
    if len == offset {
        return Ok(());
    }

    let mut f = fs::File::open(file)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    f.take(MAX_READ_PER_FILE).read_to_end(&mut buf)?;

    // Only advance past the last complete (newline-terminated) line.
    let last_nl = match buf.iter().rposition(|&b| b == b'\n') {
        Some(p) => p,
        None => return Ok(()), // no complete line yet
    };

    let (project, session_id) = provenance(src.tool, file, &src.root);
    let complete = &buf[..=last_nl];
    for raw in complete.split(|&b| b == b'\n') {
        if raw.is_empty() {
            continue;
        }
        let line = String::from_utf8_lossy(raw);
        let scrubbed = redact::scrub(&line);
        out.push(Record {
            schema: "v1".to_string(),
            tool: src.tool.to_string(),
            project: project.clone(),
            session_id: session_id.clone(),
            captured_at_ms: now_ms(),
            line: scrubbed,
        });
    }

    offsets.set(key, offset + last_nl as u64 + 1);
    Ok(())
}

/// Derive a best-effort (project, session) pair from the transcript path.
/// `project` is a human-readable workspace/thread label when we can recover one
/// (e.g. `harness-message-capture` from Cursor's encoded project dir); `session`
/// is the file stem (usually the chat/session id).
fn provenance(tool: &str, file: &Path, root: &Path) -> (String, String) {
    let session = file
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let project = match tool {
        "cursor" | "claude" => workspace_label(file, root),
        "codex" => "codex".to_string(),
        _ => parent_dir_name(file),
    };
    (project, session)
}

fn parent_dir_name(file: &Path) -> String {
    file.parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// First path component under the tool root is the encoded workspace slug.
fn workspace_label(file: &Path, root: &Path) -> String {
    let slug = file
        .strip_prefix(root)
        .ok()
        .and_then(|rel| rel.components().next())
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .unwrap_or_else(|| parent_dir_name(file));
    humanize_workspace_slug(&slug)
}

/// Turn Cursor/Claude encoded path dirs into a short chat/workspace label.
///
/// Examples:
/// - `Users-adeep-Developer-harness-message-capture` → `harness-message-capture`
/// - `-Users-adeep-Developer-khotan--claude-worktrees-foo` → `foo`
pub fn humanize_workspace_slug(slug: &str) -> String {
    let s = slug.trim().trim_start_matches('-');
    if s.is_empty() {
        return "unknown".into();
    }
    // Prefer the leaf after a worktrees marker when present.
    for marker in ["-worktrees-", "--claude-worktrees-", "-claude-worktrees-"] {
        if let Some(idx) = s.rfind(marker) {
            let leaf = &s[idx + marker.len()..];
            if !leaf.is_empty() {
                return leaf.to_string();
            }
        }
    }
    for marker in ["Developer-", "Projects-", "repos-", "code-"] {
        if let Some(idx) = s.rfind(marker) {
            let rest = &s[idx + marker.len()..];
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    s.to_string()
}

/// Compact summary of which workspaces/threads a capture batch touched, e.g.
/// `harness-message-capture×3, khotan×1`.
pub fn thread_summary(records: &[Record]) -> String {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for r in records {
        let label = if r.project.is_empty() {
            r.tool.clone()
        } else {
            r.project.clone()
        };
        *counts.entry(label).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(label, n)| {
            if n == 1 {
                label
            } else {
                format!("{label}×{n}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn humanizes_cursor_developer_slug() {
        assert_eq!(
            humanize_workspace_slug("Users-adeep-Developer-harness-message-capture"),
            "harness-message-capture"
        );
    }

    #[test]
    fn humanizes_claude_worktree_slug() {
        assert_eq!(
            humanize_workspace_slug(
                "-Users-adeep-Developer-notanerp--claude-worktrees-blissful-roentgen"
            ),
            "blissful-roentgen"
        );
    }

    #[test]
    fn cursor_provenance_uses_workspace_not_session_dir() {
        let root = PathBuf::from("/Users/adeep/.cursor/projects");
        let file = root
            .join("Users-adeep-Developer-harness-message-capture")
            .join("agent-transcripts")
            .join("76a56200-c845-4f62-b741-ca6237573ade")
            .join("76a56200-c845-4f62-b741-ca6237573ade.jsonl");
        let (project, session) = provenance("cursor", &file, &root);
        assert_eq!(project, "harness-message-capture");
        assert_eq!(session, "76a56200-c845-4f62-b741-ca6237573ade");
    }

    #[test]
    fn thread_summary_counts_per_project() {
        let rec = |project: &str| Record {
            schema: "v1".into(),
            tool: "cursor".into(),
            project: project.into(),
            session_id: "s".into(),
            captured_at_ms: 1,
            line: "{}".into(),
        };
        let s = thread_summary(&[
            rec("harness-message-capture"),
            rec("harness-message-capture"),
            rec("khotan"),
        ]);
        assert_eq!(s, "harness-message-capture×2, khotan");
    }
}
