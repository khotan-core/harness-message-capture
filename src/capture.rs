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

    let (project, session_id) = provenance(file, &src.root);
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

/// Derive a best-effort (project, session) pair from the path: session is the
/// file stem, project is the immediate parent directory name.
fn provenance(file: &Path, _root: &Path) -> (String, String) {
    let session = file
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let project = file
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    (project, session)
}
