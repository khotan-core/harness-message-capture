use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_SCAN_DEPTH: usize = 8;
const MAX_METADATA_BYTES: u64 = 256 * 1024;

/// Local repository/worktree paths used to reverse harness-specific path
/// encodings without guessing where hyphens were originally path separators.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceIndex {
    candidates: Vec<PathBuf>,
}

impl WorkspaceIndex {
    pub fn discover(search_roots: &[PathBuf]) -> WorkspaceIndex {
        let mut candidates = BTreeSet::new();
        for root in search_roots.iter().filter(|root| root.is_dir()) {
            scan_candidates(root, 0, &mut candidates);
        }
        WorkspaceIndex {
            candidates: candidates.into_iter().collect(),
        }
    }

    #[cfg(test)]
    pub fn from_candidates(candidates: Vec<PathBuf>) -> WorkspaceIndex {
        WorkspaceIndex { candidates }
    }

    pub fn candidates(&self) -> &[PathBuf] {
        &self.candidates
    }

    pub fn resolve(&self, tool: &str, transcript: &Path, source_root: &Path) -> Option<PathBuf> {
        if matches!(tool, "claude" | "codex") {
            if let Some(cwd) = transcript_cwd(transcript) {
                return Some(cwd);
            }
        }

        let slug = transcript
            .strip_prefix(source_root)
            .ok()?
            .components()
            .next()?
            .as_os_str()
            .to_string_lossy();

        self.candidates
            .iter()
            .find(|candidate| encoded_path(candidate, tool) == slug)
            .cloned()
    }
}

fn scan_candidates(dir: &Path, depth: usize, out: &mut BTreeSet<PathBuf>) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let has_env = dir.join("env.khotan.local").is_file()
        || dir.join(".env.khotan.local").is_file();
    let has_git = dir.join(".git").exists();
    if has_env || has_git {
        out.insert(dir.to_path_buf());
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if matches!(
            name.as_ref(),
            ".git" | "node_modules" | "target" | ".next" | "dist" | "build"
        ) {
            continue;
        }
        scan_candidates(&entry.path(), depth + 1, out);
    }
}

/// Cursor drops the leading slash; Claude preserves it as a leading dash.
pub fn encoded_path(path: &Path, tool: &str) -> String {
    let raw = path.to_string_lossy().replace('/', "-");
    if tool == "cursor" {
        raw.trim_start_matches('-').to_string()
    } else {
        raw
    }
}

/// Read only the beginning of a transcript and recursively locate a `cwd`
/// metadata field. Claude and Codex both put it in their initial events.
fn transcript_cwd(path: &Path) -> Option<PathBuf> {
    let mut file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_METADATA_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    for line in bytes.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
        let value: Value = serde_json::from_slice(line).ok()?;
        if let Some(cwd) = find_string_key(&value, "cwd") {
            let path = PathBuf::from(cwd);
            if path.is_absolute() {
                return Some(path);
            }
        }
    }
    None
}

fn find_string_key<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(value)) = map.get(key) {
                return Some(value);
            }
            map.values().find_map(|value| find_string_key(value, key))
        }
        Value::Array(values) => values.iter().find_map(|value| find_string_key(value, key)),
        _ => None,
    }
}

/// Resolve the primary checkout for a linked git worktree by parsing its `.git`
/// pointer. This is deliberately filesystem-only: the observer must not depend
/// on a globally installed git executable.
pub fn primary_repo_for_worktree(workspace: &Path) -> Result<Option<PathBuf>> {
    let marker = workspace.join(".git");
    if marker.is_dir() {
        return Ok(Some(workspace.to_path_buf()));
    }
    if !marker.is_file() {
        return Ok(None);
    }
    let contents =
        fs::read_to_string(&marker).with_context(|| format!("read {}", marker.display()))?;
    let raw = contents
        .trim()
        .strip_prefix("gitdir:")
        .map(str::trim)
        .context("worktree .git file is malformed")?;
    let gitdir = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        workspace.join(raw)
    };
    let components: Vec<_> = gitdir.components().collect();
    let marker_index = components
        .windows(2)
        .position(|pair| pair[0].as_os_str() == ".git" && pair[1].as_os_str() == "worktrees");
    let Some(index) = marker_index else {
        return Ok(None);
    };
    let mut root = PathBuf::new();
    for component in &components[..index] {
        root.push(component.as_os_str());
    }
    Ok(Some(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("hmc-workspace-{name}-{stamp}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn cursor_encoding_drops_only_the_leading_separator() {
        assert_eq!(
            encoded_path(Path::new("/Users/a/Developer/chief-nutrition"), "cursor"),
            "Users-a-Developer-chief-nutrition"
        );
        assert_eq!(
            encoded_path(Path::new("/Users/a/Developer/chief-nutrition"), "claude"),
            "-Users-a-Developer-chief-nutrition"
        );
    }

    #[test]
    fn resolves_cursor_slug_against_real_candidates() {
        let workspace = PathBuf::from("/Users/a/Developer/chief-nutrition");
        let root = PathBuf::from("/Users/a/.cursor/projects");
        let transcript = root
            .join("Users-a-Developer-chief-nutrition")
            .join("agent-transcripts")
            .join("session.jsonl");
        let index = WorkspaceIndex::from_candidates(vec![workspace.clone()]);
        assert_eq!(
            index.resolve("cursor", &transcript, &root),
            Some(workspace)
        );
    }

    #[test]
    fn codex_uses_session_meta_cwd() {
        let dir = temp_dir("codex");
        let transcript = dir.join("rollout.jsonl");
        fs::write(
            &transcript,
            r#"{"type":"session_meta","payload":{"cwd":"/tmp/customer"}}"#,
        )
        .unwrap();
        let index = WorkspaceIndex::default();
        assert_eq!(
            index.resolve("codex", &transcript, &dir),
            Some(PathBuf::from("/tmp/customer"))
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn resolves_primary_repo_from_worktree_pointer() {
        let dir = temp_dir("worktree");
        let repo = dir.join("customer");
        let worktree = dir.join("worktrees").join("branch");
        fs::create_dir_all(repo.join(".git").join("worktrees").join("branch")).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        fs::write(
            worktree.join(".git"),
            format!(
                "gitdir: {}\n",
                repo.join(".git").join("worktrees").join("branch").display()
            ),
        )
        .unwrap();
        assert_eq!(
            primary_repo_for_worktree(&worktree).unwrap(),
            Some(repo.clone())
        );
        let _ = fs::remove_dir_all(dir);
    }
}
