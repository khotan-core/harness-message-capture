use crate::record::Record;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// One durable line written under the inbox directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRecord {
    pub content_key: String,
    pub device_id: String,
    pub schema: String,
    pub tool: String,
    pub project: String,
    pub session_id: String,
    pub captured_at_ms: u128,
    pub line: String,
}

impl StoredRecord {
    pub fn from_batch(device_id: &str, record: &Record) -> Result<StoredRecord> {
        Ok(StoredRecord {
            content_key: content_key(device_id, record),
            device_id: device_id.to_string(),
            schema: record.schema.clone(),
            tool: sanitize_segment(&record.tool).context("invalid tool")?,
            project: sanitize_segment(&record.project).context("invalid project")?,
            session_id: sanitize_segment(&record.session_id).context("invalid session_id")?,
            captured_at_ms: record.captured_at_ms,
            line: record.line.clone(),
        })
    }

    pub fn path_under(&self, root: &Path) -> PathBuf {
        root.join(&self.device_id)
            .join(&self.tool)
            .join(&self.project)
            .join(format!("{}.ndjson", self.session_id))
    }
}

/// Stable (for this binary) content key so retries / mirrored copies do not
/// duplicate rows. Deliberately omits `captured_at_ms` — the same JSONL line
/// re-captured later is still the same message.
pub fn content_key(device_id: &str, record: &Record) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    device_id.hash(&mut h);
    record.schema.hash(&mut h);
    record.tool.hash(&mut h);
    record.project.hash(&mut h);
    record.session_id.hash(&mut h);
    record.line.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Reject path traversal and empty segments before joining into the inbox tree.
pub fn sanitize_segment(raw: &str) -> Result<String> {
    let s = raw.trim();
    if s.is_empty() {
        bail!("empty path segment");
    }
    if s.contains('/') || s.contains('\\') || s.contains('\0') {
        bail!("path segment contains separator: {s}");
    }
    if s == "." || s == ".." {
        bail!("path segment not allowed: {s}");
    }
    if s.chars().any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))) {
        // Soften: allow a broader set but still block separators; map unsafe chars.
        let cleaned: String = s
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
            bail!("path segment not allowed after sanitize: {s}");
        }
        return Ok(cleaned);
    }
    Ok(s.to_string())
}

pub fn sanitize_device_id(raw: &str) -> Result<String> {
    sanitize_segment(raw)
}

/// Append records to the inbox. Returns (written, skipped_duplicates).
pub fn append_batch(root: &Path, device_id: &str, records: &[Record]) -> Result<(usize, usize)> {
    let device_id = sanitize_device_id(device_id)?;
    let mut written = 0;
    let mut skipped = 0;

    // Group by destination file so we load each keys set once.
    let mut by_path: Vec<(PathBuf, Vec<StoredRecord>)> = Vec::new();
    for record in records {
        let stored = StoredRecord::from_batch(&device_id, record)?;
        let path = stored.path_under(root);
        if let Some((_, bucket)) = by_path.iter_mut().find(|(p, _)| *p == path) {
            bucket.push(stored);
        } else {
            by_path.push((path, vec![stored]));
        }
    }

    for (path, batch) in by_path {
        let keys = load_keys(&path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        let mut new_keys = HashSet::new();
        for stored in batch {
            if keys.contains(&stored.content_key) || new_keys.contains(&stored.content_key) {
                skipped += 1;
                continue;
            }
            f.write_all(serde_json::to_string(&stored)?.as_bytes())?;
            f.write_all(b"\n")?;
            new_keys.insert(stored.content_key);
            written += 1;
        }
        f.flush()?;
    }
    Ok((written, skipped))
}

fn load_keys(path: &Path) -> Result<HashSet<String>> {
    let mut keys = HashSet::new();
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(keys),
        Err(e) => return Err(e.into()),
    };
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(stored) = serde_json::from_str::<StoredRecord>(&line) {
            keys.insert(stored.content_key);
        }
    }
    Ok(keys)
}

#[derive(Debug, Default, Clone)]
pub struct ReadFilter {
    pub device_id: Option<String>,
    pub tool: Option<String>,
    pub project: Option<String>,
    pub session_id: Option<String>,
}

/// Walk the inbox and return stored records matching the filter, oldest first.
pub fn list_records(root: &Path, filter: &ReadFilter) -> Result<Vec<StoredRecord>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    walk_ndjson(root, 0, &mut |path| {
        if path.extension().map(|e| e == "ndjson").unwrap_or(false) {
            if let Ok(file) = fs::File::open(path) {
                for line in BufReader::new(file).lines().map_while(|l| l.ok()) {
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Ok(stored) = serde_json::from_str::<StoredRecord>(&line) {
                        if matches_filter(&stored, filter) {
                            out.push(stored);
                        }
                    }
                }
            }
        }
    });
    out.sort_by_key(|r| r.captured_at_ms);
    Ok(out)
}

fn matches_filter(r: &StoredRecord, f: &ReadFilter) -> bool {
    if let Some(ref d) = f.device_id {
        if &r.device_id != d {
            return false;
        }
    }
    if let Some(ref t) = f.tool {
        if &r.tool != t {
            return false;
        }
    }
    if let Some(ref p) = f.project {
        if &r.project != p {
            return false;
        }
    }
    if let Some(ref s) = f.session_id {
        if &r.session_id != s {
            return false;
        }
    }
    true
}

fn walk_ndjson(dir: &Path, depth: usize, visit: &mut dyn FnMut(&Path)) {
    if depth > 8 {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => walk_ndjson(&path, depth + 1, visit),
            Ok(ft) if ft.is_file() => visit(&path),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Record;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("khotan-store-test-{nanos}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample(line: &str) -> Record {
        Record {
            schema: "v1".into(),
            tool: "cursor".into(),
            project: "my-project".into(),
            session_id: "sess-1".into(),
            captured_at_ms: 1,
            line: line.into(),
        }
    }

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize_segment("..").is_err());
        assert!(sanitize_segment("a/b").is_err());
        assert!(sanitize_segment("").is_err());
    }

    #[test]
    fn sanitize_allows_safe_names() {
        assert_eq!(sanitize_segment("cursor").unwrap(), "cursor");
        assert_eq!(
            sanitize_segment("76a56200-c845-4f62-b741-ca6237573ade").unwrap(),
            "76a56200-c845-4f62-b741-ca6237573ade"
        );
    }

    #[test]
    fn append_and_dedupe() {
        let root = tmp();
        let rec = sample(r#"{"role":"user","message":{"content":[{"type":"text","text":"hi"}]}}"#);
        let (w1, s1) = append_batch(&root, "dev1", &[rec.clone()]).unwrap();
        assert_eq!((w1, s1), (1, 0));
        let (w2, s2) = append_batch(&root, "dev1", &[rec.clone()]).unwrap();
        assert_eq!((w2, s2), (0, 1));
        let listed = list_records(&root, &ReadFilter::default()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].line, rec.line);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn filter_by_tool() {
        let root = tmp();
        let mut a = sample("line-a");
        a.tool = "cursor".into();
        let mut b = sample("line-b");
        b.tool = "claude".into();
        b.session_id = "sess-2".into();
        append_batch(&root, "dev1", &[a, b]).unwrap();
        let only_cursor = list_records(
            &root,
            &ReadFilter {
                tool: Some("cursor".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(only_cursor.len(), 1);
        assert_eq!(only_cursor[0].tool, "cursor");
        let _ = fs::remove_dir_all(&root);
    }
}
