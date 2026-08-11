use crate::config::home;
use std::path::PathBuf;

/// A watched transcript root for a specific coding agent.
#[derive(Debug, Clone)]
pub struct Source {
    pub tool: &'static str,
    pub root: PathBuf,
}

/// Known local transcript locations. Only roots that exist are returned, so an
/// employee who doesn't use a given tool simply contributes nothing for it.
pub fn discover() -> Vec<Source> {
    let h = home();
    let candidates = [
        ("claude", h.join(".claude").join("projects")),
        ("codex", h.join(".codex").join("sessions")),
        ("cursor", h.join(".cursor").join("projects")),
    ];
    candidates
        .into_iter()
        .filter(|(_, p)| p.is_dir())
        .map(|(tool, root)| Source { tool, root })
        .collect()
}

/// Recursively collect `*.jsonl` files under a root, depth-limited and without
/// pulling in a directory-walking crate.
pub fn jsonl_files(root: &PathBuf) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, 0, &mut out);
    out
}

fn walk(dir: &PathBuf, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 8 {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => walk(&path, depth + 1, out),
            Ok(ft) if ft.is_file() && path.extension().map(|e| e == "jsonl").unwrap_or(false) => {
                out.push(path);
            }
            _ => {}
        }
    }
}
