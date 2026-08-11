use crate::config::state_dir;
use crate::destination::RouteRef;
use crate::record::Record;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RouteMetadata {
    route: RouteRef,
}

#[derive(Debug, Clone)]
pub struct RouteQueue {
    pub route: RouteRef,
    pub pending: usize,
}

/// Durable route-partitioned queues. Credentials are referenced by path and are
/// never copied into queue records or metadata.
pub struct Spool {
    root: PathBuf,
}

impl Spool {
    pub fn open() -> Spool {
        Spool::at(state_dir())
    }

    pub fn at(state_root: PathBuf) -> Spool {
        Spool {
            root: state_root.join("spool"),
        }
    }

    fn route_dir(&self, route: &RouteRef) -> PathBuf {
        self.root.join(&route.id)
    }

    fn records_path(&self, route: &RouteRef) -> PathBuf {
        self.route_dir(route).join("records.ndjson")
    }

    pub fn append(&self, route: &RouteRef, records: &[Record]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let dir = self.route_dir(route);
        fs::create_dir_all(&dir)?;
        let metadata_path = dir.join("route.json");
        if metadata_path.exists() {
            let metadata: RouteMetadata =
                serde_json::from_str(&fs::read_to_string(&metadata_path)?)
                    .context("route metadata is malformed")?;
            if metadata.route.org_id != route.org_id
                || metadata.route.api_url != route.api_url
                || metadata.route.id != route.id
            {
                bail!("route metadata does not match queued destination")
            }
        } else {
            let temp = dir.join("route.json.tmp");
            fs::write(
                &temp,
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(&RouteMetadata {
                        route: route.clone()
                    })?
                ),
            )?;
            fs::rename(temp, metadata_path)?;
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.records_path(route))?;
        for r in records {
            f.write_all(serde_json::to_string(r)?.as_bytes())?;
            f.write_all(b"\n")?;
        }
        f.sync_data()?;
        Ok(())
    }

    pub fn routes(&self) -> Vec<RouteQueue> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut routes = Vec::new();
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Ok(raw) = fs::read_to_string(dir.join("route.json")) else {
                continue;
            };
            let Ok(metadata) = serde_json::from_str::<RouteMetadata>(&raw) else {
                continue;
            };
            let pending = count_lines(&dir.join("records.ndjson"));
            if pending > 0 {
                routes.push(RouteQueue {
                    route: metadata.route,
                    pending,
                });
            }
        }
        routes.sort_by(|left, right| left.route.label.cmp(&right.route.label));
        routes
    }

    /// Read up to `limit` pending records without hiding corrupt queue lines.
    pub fn peek(&self, route: &RouteRef, limit: usize) -> Result<Vec<Record>> {
        let path = self.records_path(route);
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        BufReader::new(file)
            .lines()
            .filter_map(|line| match line {
                Ok(line) if line.trim().is_empty() => None,
                other => Some(other),
            })
            .take(limit)
            .map(|line| {
                let line = line?;
                serde_json::from_str(&line).context("queued record is malformed")
            })
            .collect()
    }

    pub fn drop_front(&self, route: &RouteRef, n: usize) -> Result<()> {
        if n == 0 {
            return Ok(());
        }
        let path = self.records_path(route);
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        let remaining: Vec<&str> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .skip(n)
            .collect();
        if remaining.is_empty() {
            let _ = fs::remove_dir_all(self.route_dir(route));
        } else {
            let mut body = remaining.join("\n");
            body.push('\n');
            let temp = path.with_extension("ndjson.tmp");
            fs::write(&temp, body)?;
            fs::rename(temp, path)?;
        }
        Ok(())
    }

    pub fn pending(&self) -> usize {
        self.routes().iter().map(|route| route.pending).sum()
    }

    pub fn clear(&self) -> Result<usize> {
        let count = self.pending();
        match fs::remove_dir_all(&self.root) {
            Ok(()) => Ok(count),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(count),
            Err(error) => Err(error.into()),
        }
    }

    /// Move the v1 unrouted queue out of the delivery path exactly once.
    pub fn quarantine_legacy(&self) -> Result<Option<PathBuf>> {
        let state = self
            .root
            .parent()
            .context("spool root has no state parent")?;
        let legacy = state.join("spool.ndjson");
        if !legacy.is_file() {
            return Ok(None);
        }
        let quarantine = state.join("quarantine");
        fs::create_dir_all(&quarantine)?;
        let target = quarantine.join(format!("legacy-spool-{}.ndjson", crate::record::now_ms()));
        fs::rename(&legacy, &target)?;
        Ok(Some(target))
    }

    pub fn has_quarantine(&self) -> bool {
        let Some(state) = self.root.parent() else {
            return false;
        };
        fs::read_dir(state.join("quarantine"))
            .map(|entries| {
                entries.flatten().any(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("legacy-spool-")
                })
            })
            .unwrap_or(false)
    }
}

fn count_lines(path: &Path) -> usize {
    fs::File::open(path)
        .map(|file| {
            BufReader::new(file)
                .lines()
                .map_while(|line| line.ok())
                .filter(|line| !line.trim().is_empty())
                .count()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_state(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("hmc-spool-{name}-{stamp}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn route(id: &str, label: &str) -> RouteRef {
        RouteRef {
            id: id.into(),
            org_id: format!("org-{id}"),
            api_url: format!("https://{id}.example"),
            credential_path: PathBuf::from(format!("/tmp/{id}.env")),
            label: label.into(),
        }
    }

    fn record(line: &str) -> Record {
        Record {
            schema: "v1".into(),
            tool: "cursor".into(),
            project: "customer".into(),
            session_id: "session".into(),
            captured_at_ms: 1,
            line: line.into(),
        }
    }

    #[test]
    fn isolates_routes_and_drops_only_successful_front() {
        let state = temp_state("routes");
        let spool = Spool::at(state.clone());
        let one = route("one", "one");
        let two = route("two", "two");
        spool.append(&one, &[record("a"), record("b")]).unwrap();
        spool.append(&two, &[record("c")]).unwrap();
        assert_eq!(spool.pending(), 3);
        spool.drop_front(&one, 1).unwrap();
        assert_eq!(spool.peek(&one, 10).unwrap()[0].line, "b");
        assert_eq!(spool.peek(&two, 10).unwrap()[0].line, "c");
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn corrupt_line_blocks_instead_of_skewing_drop_count() {
        let state = temp_state("corrupt");
        let spool = Spool::at(state.clone());
        let route = route("one", "one");
        spool.append(&route, &[record("a")]).unwrap();
        fs::write(spool.records_path(&route), "not-json\n").unwrap();
        assert!(spool.peek(&route, 10).is_err());
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn quarantines_legacy_queue_once() {
        let state = temp_state("legacy");
        fs::write(state.join("spool.ndjson"), "legacy\n").unwrap();
        let spool = Spool::at(state.clone());
        assert!(spool.quarantine_legacy().unwrap().is_some());
        assert!(spool.quarantine_legacy().unwrap().is_none());
        assert!(spool.has_quarantine());
        let _ = fs::remove_dir_all(state);
    }
}
