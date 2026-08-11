use crate::config::state_dir;
use crate::record::Record;
use anyhow::Result;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

/// A durable newline-delimited JSON buffer of records awaiting upload. Capture
/// appends here first, so a failed or offline upload never loses data.
pub struct Spool {
    path: PathBuf,
}

impl Spool {
    pub fn open() -> Spool {
        Spool {
            path: state_dir().join("spool.ndjson"),
        }
    }

    pub fn append(&self, records: &[Record]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        for r in records {
            f.write_all(serde_json::to_string(r)?.as_bytes())?;
            f.write_all(b"\n")?;
        }
        Ok(())
    }

    /// Read up to `limit` pending records (leaving the rest for the next batch).
    pub fn peek(&self, limit: usize) -> Vec<Record> {
        let file = match fs::File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        BufReader::new(file)
            .lines()
            .map_while(|l| l.ok())
            .filter(|l| !l.trim().is_empty())
            .take(limit)
            .filter_map(|l| serde_json::from_str::<Record>(&l).ok())
            .collect()
    }

    /// Drop the first `n` records from the spool after a successful upload.
    pub fn drop_front(&self, n: usize) -> Result<()> {
        if n == 0 {
            return Ok(());
        }
        let content = match fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        let remaining: Vec<&str> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .skip(n)
            .collect();
        if remaining.is_empty() {
            let _ = fs::remove_file(&self.path);
        } else {
            let mut body = remaining.join("\n");
            body.push('\n');
            fs::write(&self.path, body)?;
        }
        Ok(())
    }

    pub fn pending(&self) -> usize {
        fs::File::open(&self.path)
            .map(|f| {
                BufReader::new(f)
                    .lines()
                    .map_while(|l| l.ok())
                    .filter(|l| !l.trim().is_empty())
                    .count()
            })
            .unwrap_or(0)
    }

    /// Permanently discard every record still waiting for delivery.
    pub fn clear(&self) -> Result<usize> {
        let count = self.pending();
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(count)
    }
}
