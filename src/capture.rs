use crate::config::state_dir;
use crate::destination::{self, RouteRef};
use crate::record::{now_ms, Record, SCHEMA};
use crate::redact;
use crate::sources::{jsonl_files, Source};
use crate::workspace::WorkspaceIndex;
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

    pub fn get(&self, key: &str) -> u64 {
        *self.map.get(key).unwrap_or(&0)
    }

    pub fn set(&mut self, key: String, val: u64) {
        self.map.insert(key, val);
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteWarning {
    pub label: String,
    pub tool: String,
    pub means: String,
}

impl RouteWarning {
    fn new(
        label: impl Into<String>,
        tool: impl Into<String>,
        means: impl Into<String>,
    ) -> RouteWarning {
        RouteWarning {
            label: label.into(),
            tool: tool.into(),
            means: means.into(),
        }
    }
}

#[derive(Debug)]
pub struct CapturedFile {
    pub tool: &'static str,
    pub route: Option<RouteRef>,
    pub records: Vec<Record>,
    pub offset_key: String,
    pub next_offset: u64,
    pub route_warning: Option<RouteWarning>,
    pub advance_unrouted: bool,
}

/// Read newly-appended complete lines without mutating offsets. The caller
/// commits each returned offset only after the records are durably queued, or
/// intentionally when the workspace has no valid customer destination.
pub fn collect_new(
    sources: &[Source],
    offsets: &Offsets,
    workspaces: &WorkspaceIndex,
    allow_repos: &[String],
) -> Vec<CapturedFile> {
    let mut captured = Vec::new();
    for src in sources {
        for file in jsonl_files(&src.root) {
            match read_file(src, &file, offsets, workspaces, allow_repos) {
                Ok(Some(result)) => captured.push(result),
                Ok(None) | Err(_) => continue,
            }
        }
    }
    captured
}

fn read_file(
    src: &Source,
    file: &Path,
    offsets: &Offsets,
    workspaces: &WorkspaceIndex,
    allow_repos: &[String],
) -> Result<Option<CapturedFile>> {
    let key = file.to_string_lossy().to_string();
    let meta = fs::metadata(file).with_context(|| format!("stat {}", file.display()))?;
    let len = meta.len();
    let mut offset = offsets.get(&key);

    // File shrank (rotation/truncation) — restart from the beginning.
    if len < offset {
        offset = 0;
    }
    if len == offset {
        return Ok(None);
    }

    let mut f = fs::File::open(file)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    f.take(MAX_READ_PER_FILE).read_to_end(&mut buf)?;

    // Only advance past the last complete (newline-terminated) line.
    let last_nl = match buf.iter().rposition(|&b| b == b'\n') {
        Some(p) => p,
        None => return Ok(None), // no complete line yet
    };

    let workspace_result = workspaces.resolve_checked(src.tool, file, &src.root);
    let workspace = workspace_result
        .as_ref()
        .ok()
        .and_then(|path| path.as_deref());
    let provenance = provenance(src.tool, file, &src.root, workspace);
    let (route, route_warning, advance_unrouted) = match workspace_result {
        Err(_) => (
            None,
            Some(RouteWarning::new(
                &provenance.project,
                src.tool,
                "Same encoded path matches two checkouts",
            )),
            false,
        ),
        Ok(Some(workspace)) if !destination::workspace_allowed(&workspace, allow_repos) => {
            let name = workspace
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| provenance.project.clone());
            (
                None,
                Some(RouteWarning::new(
                    name,
                    src.tool,
                    "Repo is real, but not on the allow list",
                )),
                true,
            )
        }
        Ok(Some(workspace)) => match destination::resolve(&workspace) {
            Ok(route) => (route, None, true),
            Err(_) => (
                None,
                Some(RouteWarning::new(
                    &provenance.project,
                    src.tool,
                    "Repo found, dest file missing fields or conflicts",
                )),
                false,
            ),
        },
        Ok(None) => (
            None,
            Some(RouteWarning::new(
                &provenance.project,
                src.tool,
                "Chat has no project folder",
            )),
            true,
        ),
    };
    let mut records = Vec::new();
    let complete = &buf[..=last_nl];
    let mut cursor = 0usize;
    while cursor < complete.len() {
        let rest = &complete[cursor..];
        let line_end = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
        let raw = &rest[..line_end];
        if !raw.is_empty() {
            let line = String::from_utf8_lossy(raw);
            let scrubbed = redact::scrub(&line);
            records.push(Record {
                schema: SCHEMA.to_string(),
                tool: src.tool.to_string(),
                project: provenance.project.clone(),
                session_id: provenance.session_id.clone(),
                thread_id: Some(provenance.thread_id.clone()),
                agent_role: Some(provenance.agent_role.clone()),
                seq: Some(offset + cursor as u64),
                captured_at_ms: now_ms(),
                line: scrubbed,
            });
        }
        cursor += line_end + 1;
    }

    Ok(Some(CapturedFile {
        tool: src.tool,
        route,
        records,
        offset_key: key,
        next_offset: offset + last_nl as u64 + 1,
        route_warning,
        advance_unrouted,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Provenance {
    project: String,
    session_id: String,
    thread_id: String,
    agent_role: String,
}

/// Derive project, session, and thread from the transcript path.
/// `project` is a human-readable workspace label when we can recover one
/// (e.g. `harness-message-capture` from Cursor's encoded project dir).
/// `session_id` is the file stem. `thread_id` is the root session, so a
/// subagent inherits its parent.
fn provenance(tool: &str, file: &Path, root: &Path, workspace: Option<&Path>) -> Provenance {
    let session_id = file
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let project = match tool {
        "cursor" | "claude" => workspace
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| workspace_label(file, root)),
        "codex" => workspace
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "codex".to_string()),
        _ => parent_dir_name(file),
    };
    let (thread_id, agent_role) = thread_of(file, root, &session_id);
    Provenance {
        project,
        session_id,
        thread_id,
        agent_role,
    }
}

/// If any path component below the tool root is `subagents`, the transcript
/// is a subagent and its thread is the directory above `subagents`.
fn thread_of(file: &Path, root: &Path, session_id: &str) -> (String, String) {
    let Ok(rel) = file.strip_prefix(root) else {
        return (session_id.to_string(), "root".to_string());
    };
    let components: Vec<_> = rel
        .iter()
        .map(|c| c.to_string_lossy().into_owned())
        .collect();
    if let Some(idx) = components.iter().position(|c| c == "subagents") {
        if idx > 0 {
            return (components[idx - 1].clone(), "subagent".to_string());
        }
    }
    (session_id.to_string(), "root".to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{encoded_path, WorkspaceIndex};
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("hmc-capture-{name}-{stamp}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

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
        let got = provenance("cursor", &file, &root, None);
        assert_eq!(got.project, "harness-message-capture");
        assert_eq!(got.session_id, "76a56200-c845-4f62-b741-ca6237573ade");
        assert_eq!(got.thread_id, "76a56200-c845-4f62-b741-ca6237573ade");
        assert_eq!(got.agent_role, "root");
    }

    #[test]
    fn thread_of_cursor_root() {
        let root = PathBuf::from("/Users/adeep/.cursor/projects");
        let file = root
            .join("Users-adeep-Developer-harness-message-capture")
            .join("agent-transcripts")
            .join("76a56200-c845-4f62-b741-ca6237573ade")
            .join("76a56200-c845-4f62-b741-ca6237573ade.jsonl");
        let (thread, role) = thread_of(&file, &root, "76a56200-c845-4f62-b741-ca6237573ade");
        assert_eq!(thread, "76a56200-c845-4f62-b741-ca6237573ade");
        assert_eq!(role, "root");
    }

    #[test]
    fn thread_of_cursor_subagent() {
        let root = PathBuf::from("/Users/adeep/.cursor/projects");
        let file = root
            .join("Users-adeep-Developer-harness-message-capture")
            .join("agent-transcripts")
            .join("76a56200-c845-4f62-b741-ca6237573ade")
            .join("subagents")
            .join("470b9dee-3acb-421d-96e5-20a4aa2b3811.jsonl");
        let (thread, role) = thread_of(&file, &root, "470b9dee-3acb-421d-96e5-20a4aa2b3811");
        assert_eq!(thread, "76a56200-c845-4f62-b741-ca6237573ade");
        assert_eq!(role, "subagent");
    }

    #[test]
    fn thread_of_claude_root() {
        let root = PathBuf::from("/Users/adeep/.claude/projects");
        let file = root
            .join("-Users-adeep-Developer-khotan")
            .join("878fdf14-322f-4772-bf37-99daaf983ce2.jsonl");
        let (thread, role) = thread_of(&file, &root, "878fdf14-322f-4772-bf37-99daaf983ce2");
        assert_eq!(thread, "878fdf14-322f-4772-bf37-99daaf983ce2");
        assert_eq!(role, "root");
    }

    #[test]
    fn thread_of_claude_subagent() {
        let root = PathBuf::from("/Users/adeep/.claude/projects");
        let file = root
            .join("-Users-adeep-Developer-khotan")
            .join("878fdf14-322f-4772-bf37-99daaf983ce2")
            .join("subagents")
            .join("agent-a299bcb2ba0808226.jsonl");
        let (thread, role) = thread_of(&file, &root, "agent-a299bcb2ba0808226");
        assert_eq!(thread, "878fdf14-322f-4772-bf37-99daaf983ce2");
        assert_eq!(role, "subagent");
    }

    #[test]
    fn thread_of_codex_is_always_root() {
        let root = PathBuf::from("/Users/adeep/.codex/sessions");
        let file = root
            .join("2026")
            .join("08")
            .join("23")
            .join("rollout-2026-08-23T11-38-01-01a02c44-7e9c-7db3-a9f0-64c4f742c79c.jsonl");
        let stem = "rollout-2026-08-23T11-38-01-01a02c44-7e9c-7db3-a9f0-64c4f742c79c";
        let (thread, role) = thread_of(&file, &root, stem);
        assert_eq!(thread, stem);
        assert_eq!(role, "root");
    }

    #[test]
    fn collection_does_not_commit_offset_before_durable_queue() {
        let temp = temp_dir("offset");
        let workspace = temp.join("customer");
        fs::create_dir_all(workspace.join(".git")).unwrap();
        fs::write(
            workspace.join("env.khotan.local"),
            "KHOTAN_API_URL='https://customer.example'\nKHOTAN_API_KEY='fake-key'\nKHOTAN_ORG_ID='org-test'\n",
        )
        .unwrap();
        let source_root = temp.join("cursor-projects");
        let transcript = source_root
            .join(encoded_path(&workspace, "cursor"))
            .join("agent-transcripts")
            .join("session")
            .join("session.jsonl");
        fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        fs::write(&transcript, "{\"role\":\"user\"}\n").unwrap();

        let offsets = Offsets {
            map: HashMap::new(),
            path: temp.join("offsets.json"),
        };
        let source = Source {
            tool: "cursor",
            root: source_root,
        };
        let workspaces = WorkspaceIndex::from_candidates(vec![workspace]);
        let captured = collect_new(&[source], &offsets, &workspaces, &["customer".into()]);

        assert_eq!(captured.len(), 1);
        assert!(captured[0].route.is_some());
        assert_eq!(offsets.get(&transcript.to_string_lossy()), 0);
        assert!(captured[0].next_offset > 0);

        let workspace = workspaces.candidates()[0].clone();
        // A destination file missing a required value (no API key) stays
        // blocked, so the offset must not advance past lines it never queued.
        fs::write(
            workspace.join("env.khotan.local"),
            "KHOTAN_API_URL='https://customer.example'\n",
        )
        .unwrap();
        let blocked = collect_new(
            &[Source {
                tool: "cursor",
                root: transcript.ancestors().nth(4).unwrap().to_path_buf(),
            }],
            &offsets,
            &workspaces,
            &["customer".into()],
        );
        assert_eq!(blocked.len(), 1);
        assert!(!blocked[0].advance_unrouted);
        assert_eq!(
            blocked[0].route_warning,
            Some(RouteWarning::new(
                "customer",
                "cursor",
                "Repo found, dest file missing fields or conflicts"
            ))
        );

        let skipped = collect_new(
            &[Source {
                tool: "cursor",
                root: transcript.ancestors().nth(4).unwrap().to_path_buf(),
            }],
            &offsets,
            &workspaces,
            &["other-customer".into()],
        );
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].route.is_none());
        assert!(skipped[0].advance_unrouted);
        assert_eq!(
            skipped[0].route_warning,
            Some(RouteWarning::new(
                "customer",
                "cursor",
                "Repo is real, but not on the allow list"
            ))
        );
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn unresolved_workspace_explains_skip() {
        let temp = temp_dir("unresolved");
        let source_root = temp.join("cursor-projects");
        let transcript = source_root
            .join("Users-adeep-Developer-empty-window")
            .join("agent-transcripts")
            .join("session")
            .join("session.jsonl");
        fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        fs::write(&transcript, "{\"role\":\"user\"}\n").unwrap();
        let offsets = Offsets {
            map: HashMap::new(),
            path: temp.join("offsets.json"),
        };
        let captured = collect_new(
            &[Source {
                tool: "cursor",
                root: source_root,
            }],
            &offsets,
            &WorkspaceIndex::from_candidates(vec![]),
            &["empty-window".into()],
        );
        assert_eq!(captured.len(), 1);
        assert!(captured[0].advance_unrouted);
        assert_eq!(
            captured[0].route_warning,
            Some(RouteWarning::new(
                "empty-window",
                "cursor",
                "Chat has no project folder"
            ))
        );
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn seq_is_byte_offset_and_continues_after_partial_read() {
        let temp = temp_dir("seq");
        let workspace = temp.join("customer");
        fs::create_dir_all(workspace.join(".git")).unwrap();
        fs::write(
            workspace.join("env.khotan.local"),
            "KHOTAN_API_URL='https://customer.example'\nKHOTAN_API_KEY='fake-key'\nKHOTAN_ORG_ID='org-test'\n",
        )
        .unwrap();
        let source_root = temp.join("cursor-projects");
        let transcript = source_root
            .join(encoded_path(&workspace, "cursor"))
            .join("agent-transcripts")
            .join("session")
            .join("session.jsonl");
        fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        let first = "{\"role\":\"user\"}\n";
        fs::write(&transcript, first).unwrap();

        let mut offsets = Offsets {
            map: HashMap::new(),
            path: temp.join("offsets.json"),
        };
        let source = Source {
            tool: "cursor",
            root: source_root.clone(),
        };
        let workspaces = WorkspaceIndex::from_candidates(vec![workspace]);
        let first_pass = collect_new(
            std::slice::from_ref(&source),
            &offsets,
            &workspaces,
            &["customer".into()],
        );
        assert_eq!(first_pass.len(), 1);
        assert_eq!(first_pass[0].records.len(), 1);
        assert_eq!(first_pass[0].records[0].seq, Some(0));
        assert_eq!(first_pass[0].records[0].schema, SCHEMA);
        assert_eq!(
            first_pass[0].records[0].thread_id.as_deref(),
            Some("session")
        );
        assert_eq!(first_pass[0].records[0].agent_role.as_deref(), Some("root"));

        offsets.set(first_pass[0].offset_key.clone(), first_pass[0].next_offset);
        let second = "{\"role\":\"assistant\"}\n";
        fs::write(&transcript, format!("{first}{second}")).unwrap();
        let second_pass = collect_new(&[source], &offsets, &workspaces, &["customer".into()]);
        assert_eq!(second_pass.len(), 1);
        assert_eq!(second_pass[0].records.len(), 1);
        assert_eq!(second_pass[0].records[0].seq, Some(first.len() as u64));
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn identical_turn_ended_lines_keep_distinct_seq() {
        let temp = temp_dir("turns");
        let workspace = temp.join("customer");
        fs::create_dir_all(workspace.join(".git")).unwrap();
        fs::write(
            workspace.join("env.khotan.local"),
            "KHOTAN_API_URL='https://customer.example'\nKHOTAN_API_KEY='fake-key'\nKHOTAN_ORG_ID='org-test'\n",
        )
        .unwrap();
        let source_root = temp.join("cursor-projects");
        let transcript = source_root
            .join(encoded_path(&workspace, "cursor"))
            .join("agent-transcripts")
            .join("session")
            .join("session.jsonl");
        fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        let line = r#"{"type":"turn_ended","status":"success"}"#;
        fs::write(&transcript, format!("{line}\n{line}\n")).unwrap();
        let offsets = Offsets {
            map: HashMap::new(),
            path: temp.join("offsets.json"),
        };
        let captured = collect_new(
            &[Source {
                tool: "cursor",
                root: source_root,
            }],
            &offsets,
            &WorkspaceIndex::from_candidates(vec![workspace]),
            &["customer".into()],
        );
        assert_eq!(captured[0].records.len(), 2);
        assert_eq!(captured[0].records[0].line, captured[0].records[1].line);
        assert_ne!(captured[0].records[0].seq, captured[0].records[1].seq);
        assert_eq!(captured[0].records[0].seq, Some(0));
        assert_eq!(captured[0].records[1].seq, Some((line.len() + 1) as u64));
        let _ = fs::remove_dir_all(temp);
    }
}
