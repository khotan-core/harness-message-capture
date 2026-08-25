use crate::config::state_dir;
use crate::destination::RouteRef;
use crate::record::Record;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RouteMetadata {
    route: RouteRef,
}

/// Where a route's delivery stands, kept beside its records so that dropping a
/// delivered batch is a small write instead of a rewrite of the whole queue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Cursor {
    /// Byte offset of the first record that has not been delivered.
    offset: u64,
    /// Records between `offset` and `scanned_to`.
    pending: usize,
    /// How far into the file `pending` has already counted.
    scanned_to: u64,
}

/// Delivered bytes tolerated in front of the cursor before the file is
/// rewritten without them. A copy of a large queue is worth doing rarely.
const COMPACT_AFTER: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct RouteQueue {
    pub route: RouteRef,
    pub pending: usize,
}

/// Where a route's records go: an existing queue matched by its recorded
/// identity, or a fresh directory to be created under the computed name.
enum QueueDir {
    Matched { dir: PathBuf, recorded: RouteRef },
    Fresh(PathBuf),
}

/// Whether a queue's recorded identity is the one a route reaches. The origin
/// must match, and then either the recorded key fingerprint matches, or the
/// queue predates the fingerprint and its recorded organization matches the one
/// the route already knows. The second arm is what adopts pre-change queues.
fn queue_matches(recorded: &RouteRef, route: &RouteRef) -> bool {
    if recorded.api_url != route.api_url {
        return false;
    }
    match recorded.key_fingerprint {
        Some(_) => recorded.key_fingerprint == route.key_fingerprint,
        None => match (&recorded.org_id, &route.org_id) {
            (Some(recorded_org), Some(route_org)) => recorded_org == route_org,
            _ => false,
        },
    }
}

/// Atomically write a queue's metadata. Credentials are referenced by path and
/// never appear here; the key fingerprint is not the key.
fn write_metadata(dir: &Path, route: &RouteRef) -> Result<()> {
    fs::create_dir_all(dir)?;
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
    fs::rename(temp, dir.join("route.json"))?;
    Ok(())
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

    fn cursor_path(&self, route: &RouteRef) -> PathBuf {
        self.route_dir(route).join("cursor.json")
    }

    fn read_cursor(&self, route: &RouteRef) -> Cursor {
        read_cursor_at(&self.cursor_path(route))
    }

    fn write_cursor(&self, route: &RouteRef, cursor: &Cursor) -> Result<()> {
        write_cursor_at(&self.route_dir(route), cursor)
    }

    pub fn append(&self, route: &RouteRef, records: &[Record]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        // The directory is found by what a queue recorded, not by recomputing a
        // name, so a queue written before identity moved to the key is appended
        // to rather than orphaned beside a freshly named one.
        let dir = match self.resolve_queue_dir(route)? {
            QueueDir::Matched { dir, recorded } => {
                // Adopt a legacy queue the first time its route is matched by
                // stamping the key fingerprint into it. Nothing else moves.
                if recorded.key_fingerprint.is_none() && route.key_fingerprint.is_some() {
                    write_metadata(
                        &dir,
                        &RouteRef {
                            key_fingerprint: route.key_fingerprint,
                            ..recorded
                        },
                    )?;
                }
                dir
            }
            QueueDir::Fresh(dir) => {
                fs::create_dir_all(&dir)?;
                write_metadata(&dir, route)?;
                dir
            }
        };
        let records_path = dir.join("records.ndjson");
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&records_path)?;
        for r in records {
            f.write_all(serde_json::to_string(r)?.as_bytes())?;
            f.write_all(b"\n")?;
        }
        f.sync_data()?;
        let mut cursor = read_cursor_at(&dir.join("cursor.json"));
        scan_tail(&records_path, &mut cursor);
        write_cursor_at(&dir, &cursor)?;
        Ok(())
    }

    /// Find the queue a route belongs to by reading each queue's recorded
    /// identity, or name a fresh directory when none matches. Never renames.
    fn resolve_queue_dir(&self, route: &RouteRef) -> Result<QueueDir> {
        if let Ok(entries) = fs::read_dir(&self.root) {
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
                if queue_matches(&metadata.route, route) {
                    return Ok(QueueDir::Matched {
                        dir,
                        recorded: metadata.route,
                    });
                }
            }
        }
        let target = self.root.join(&route.id);
        if target.join("route.json").exists() {
            // The computed name is taken by a queue that does not match — a hash
            // collision. Fail closed rather than mix two destinations.
            bail!("route metadata does not match queued destination")
        }
        Ok(QueueDir::Fresh(target))
    }

    /// Record the organization the endpoint named for a queue that carried none,
    /// so a later disagreement is enforced. A no-op once it already holds one.
    pub fn pin_org(&self, route: &RouteRef, org: &str) -> Result<()> {
        let dir = self.route_dir(route);
        let metadata_path = dir.join("route.json");
        let mut metadata: RouteMetadata =
            serde_json::from_str(&fs::read_to_string(&metadata_path)?)
                .context("route metadata is malformed")?;
        if metadata.route.org_id.as_deref() == Some(org) {
            return Ok(());
        }
        metadata.route.org_id = Some(org.to_string());
        write_metadata(&dir, &metadata.route)
    }

    /// Point a queue at a different destination file that carries the same
    /// identity, after the one it was pinned to stopped producing credentials.
    pub fn repoint(&self, route: &RouteRef, credential_path: &Path) -> Result<()> {
        let dir = self.route_dir(route);
        let metadata_path = dir.join("route.json");
        let mut metadata: RouteMetadata =
            serde_json::from_str(&fs::read_to_string(&metadata_path)?)
                .context("route metadata is malformed")?;
        metadata.route.credential_path = credential_path.to_path_buf();
        write_metadata(&dir, &metadata.route)
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
            // The cursor already knows the count. Only bytes appended since the
            // last look are read here; counting 90 MB of lines every pass was
            // most of what a pass spent its time on.
            let mut cursor = read_cursor_at(&dir.join("cursor.json"));
            if scan_tail(&dir.join("records.ndjson"), &mut cursor) {
                let _ = write_cursor_at(&dir, &cursor);
            }
            if cursor.pending > 0 {
                routes.push(RouteQueue {
                    route: metadata.route,
                    pending: cursor.pending,
                });
            }
        }
        routes.sort_by(|left, right| left.route.label.cmp(&right.route.label));
        routes
    }

    /// Records still waiting for this one route.
    pub fn pending_for(&self, route: &RouteRef) -> usize {
        let mut cursor = self.read_cursor(route);
        scan_tail(&self.records_path(route), &mut cursor);
        cursor.pending
    }

    /// Read the front of the queue, stopping at `max_records` or once the
    /// serialized bytes would pass `max_bytes`. A record wider than the whole
    /// budget still comes back on its own: an outsized line must not wedge the
    /// route behind it.
    pub fn peek_batch(
        &self,
        route: &RouteRef,
        max_records: usize,
        max_bytes: usize,
    ) -> Result<Vec<Record>> {
        if max_records == 0 {
            return Ok(Vec::new());
        }
        let path = self.records_path(route);
        let cursor = self.read_cursor(route);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(cursor.offset))?;

        let mut batch = Vec::new();
        let mut bytes = 0usize;
        let mut line = String::new();
        while batch.len() < max_records {
            line.clear();
            let read = reader.read_line(&mut line)?;
            if read == 0 {
                break;
            }
            if line.trim().is_empty() {
                continue;
            }
            if !batch.is_empty() && bytes + read > max_bytes {
                break;
            }
            bytes += read;
            let parsed = serde_json::from_str(line.trim_end());
            batch.push(parsed.context("queued record is malformed")?);
        }
        Ok(batch)
    }

    /// Step the cursor past `n` delivered records. The queue file itself is
    /// left alone until the delivered prefix is large enough to be worth a copy.
    pub fn drop_front(&self, route: &RouteRef, n: usize) -> Result<()> {
        if n == 0 {
            return Ok(());
        }
        let path = self.records_path(route);
        let mut cursor = self.read_cursor(route);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(_) => return Ok(()),
        };
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(cursor.offset))?;

        let mut dropped = 0usize;
        let mut offset = cursor.offset;
        let mut line = Vec::new();
        while dropped < n {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            offset += read as u64;
            if !line.iter().all(u8::is_ascii_whitespace) {
                dropped += 1;
            }
        }
        cursor.offset = offset;
        cursor.pending = cursor.pending.saturating_sub(dropped);

        if cursor.pending == 0 && cursor.offset >= file_len(&path) {
            // Nothing left to deliver: give the disk back and start clean.
            let _ = fs::remove_file(&path);
            cursor = Cursor::default();
        } else if cursor.offset > COMPACT_AFTER {
            compact(&path, &mut cursor)?;
        }
        self.write_cursor(route, &cursor)
    }

    /// Park the front record and step over it. One record the server will never
    /// accept must not hold the rest of a customer's queue hostage.
    pub fn quarantine_front(&self, route: &RouteRef) -> Result<()> {
        let path = self.records_path(route);
        let cursor = self.read_cursor(route);
        let mut reader = BufReader::new(File::open(&path)?);
        reader.seek(SeekFrom::Start(cursor.offset))?;
        let mut line = Vec::new();
        reader.read_until(b'\n', &mut line)?;
        if line.is_empty() {
            return Ok(());
        }
        let state = self
            .root
            .parent()
            .context("spool root has no state parent")?;
        let dir = state.join("quarantine");
        fs::create_dir_all(&dir)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("oversize-{}.ndjson", route.id)))?;
        if !line.ends_with(b"\n") {
            line.push(b'\n');
        }
        file.write_all(&line)?;
        file.sync_data()?;
        self.drop_front(route, 1)
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

fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn read_cursor_at(path: &Path) -> Cursor {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_cursor_at(dir: &Path, cursor: &Cursor) -> Result<()> {
    fs::create_dir_all(dir)?;
    let temp = dir.join("cursor.json.tmp");
    fs::write(&temp, format!("{}\n", serde_json::to_string(cursor)?))?;
    fs::rename(temp, dir.join("cursor.json"))?;
    Ok(())
}

/// Count the records appended since the cursor last looked. Returns whether the
/// cursor moved, so a pass that changes nothing writes nothing.
fn scan_tail(path: &Path, cursor: &mut Cursor) -> bool {
    let before = *cursor;
    let Ok(len) = fs::metadata(path).map(|meta| meta.len()) else {
        // The file is gone, so nothing is pending. A fresh append starts over.
        *cursor = Cursor::default();
        return *cursor != before;
    };
    if len < cursor.scanned_to || cursor.offset > len {
        // Truncated or replaced underneath us. Trust the bytes, not the memory.
        *cursor = Cursor::default();
    }
    if len == cursor.scanned_to {
        return *cursor != before;
    }
    let Ok(file) = File::open(path) else {
        return *cursor != before;
    };
    let mut reader = BufReader::new(file);
    if reader.seek(SeekFrom::Start(cursor.scanned_to)).is_err() {
        return *cursor != before;
    }
    let mut scanned_to = cursor.scanned_to;
    let mut added = 0usize;
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(read) => {
                // A line without its newline is a half-written append. Leave it
                // for the next pass rather than counting a record twice.
                if !line.ends_with(b"\n") {
                    break;
                }
                scanned_to += read as u64;
                if !line.iter().all(u8::is_ascii_whitespace) {
                    added += 1;
                }
            }
            Err(_) => break,
        }
    }
    cursor.pending += added;
    cursor.scanned_to = scanned_to;
    *cursor != before
}

/// Rewrite the queue without the records that already left the machine.
fn compact(path: &Path, cursor: &mut Cursor) -> Result<()> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(cursor.offset))?;
    let temp = path.with_extension("ndjson.tmp");
    let mut out = File::create(&temp)?;
    std::io::copy(&mut file, &mut out)?;
    out.sync_data()?;
    fs::rename(&temp, path)?;
    cursor.scanned_to = cursor.scanned_to.saturating_sub(cursor.offset);
    cursor.offset = 0;
    Ok(())
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
            org_id: Some(format!("org-{id}")),
            api_url: format!("https://{id}.example"),
            key_fingerprint: Some(crate::destination::key_fingerprint(id)),
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
            thread_id: None,
            agent_role: None,
            seq: None,
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
        assert_eq!(spool.peek_batch(&one, 10, usize::MAX).unwrap()[0].line, "b");
        assert_eq!(spool.peek_batch(&two, 10, usize::MAX).unwrap()[0].line, "c");
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn corrupt_line_blocks_instead_of_skewing_drop_count() {
        let state = temp_state("corrupt");
        let spool = Spool::at(state.clone());
        let route = route("one", "one");
        spool.append(&route, &[record("a")]).unwrap();
        fs::write(spool.records_path(&route), "not-json\n").unwrap();
        assert!(spool.peek_batch(&route, 10, usize::MAX).is_err());
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn dropping_the_front_leaves_the_file_alone_until_it_is_worth_compacting() {
        let state = temp_state("cursor");
        let spool = Spool::at(state.clone());
        let route = route("one", "one");
        let records: Vec<Record> = (0..50).map(|i| record(&format!("line-{i}"))).collect();
        spool.append(&route, &records).unwrap();
        let before = file_len(&spool.records_path(&route));

        spool.drop_front(&route, 10).unwrap();

        let cursor = spool.read_cursor(&route);
        assert_eq!(cursor.pending, 40);
        assert!(cursor.offset > 0);
        assert_eq!(cursor.scanned_to, before);
        assert_eq!(file_len(&spool.records_path(&route)), before);
        assert_eq!(
            spool.peek_batch(&route, 1, usize::MAX).unwrap()[0].line,
            "line-10"
        );
        assert_eq!(spool.routes()[0].pending, 40);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn counts_only_what_was_appended_since_the_last_look() {
        let state = temp_state("tail");
        let spool = Spool::at(state.clone());
        let route = route("one", "one");
        spool.append(&route, &[record("a")]).unwrap();
        assert_eq!(spool.routes()[0].pending, 1);
        spool.append(&route, &[record("b"), record("c")]).unwrap();
        assert_eq!(spool.routes()[0].pending, 3);
        spool.drop_front(&route, 3).unwrap();
        assert!(spool.routes().is_empty());
        assert_eq!(spool.pending(), 0);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn compaction_reclaims_the_delivered_prefix() {
        let state = temp_state("compact");
        let spool = Spool::at(state.clone());
        let route = route("one", "one");
        let wide = "x".repeat(200 * 1024);
        let records: Vec<Record> = (0..60).map(|_| record(&wide)).collect();
        spool.append(&route, &records).unwrap();
        let before = file_len(&spool.records_path(&route));
        assert!(before > COMPACT_AFTER);

        spool.drop_front(&route, 50).unwrap();

        let cursor = spool.read_cursor(&route);
        assert_eq!(cursor.offset, 0);
        assert_eq!(cursor.pending, 10);
        assert!(file_len(&spool.records_path(&route)) < before / 2);
        assert_eq!(
            spool.peek_batch(&route, 1, usize::MAX).unwrap()[0].line,
            wide
        );
        assert_eq!(spool.routes()[0].pending, 10);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn a_batch_stops_at_the_byte_budget_but_never_returns_nothing() {
        let state = temp_state("budget");
        let spool = Spool::at(state.clone());
        let route = route("one", "one");
        let wide = "y".repeat(4096);
        spool
            .append(
                &route,
                &[
                    record(&wide),
                    record(&wide),
                    record("small"),
                    record("small"),
                ],
            )
            .unwrap();

        let batch = spool.peek_batch(&route, 100, 5000).unwrap();
        assert_eq!(batch.len(), 1);
        let batch = spool.peek_batch(&route, 100, 1).unwrap();
        assert_eq!(batch.len(), 1, "an oversized record still ships alone");
        let batch = spool.peek_batch(&route, 100, usize::MAX).unwrap();
        assert_eq!(batch.len(), 4);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn quarantines_one_record_the_server_will_not_take() {
        let state = temp_state("oversize");
        let spool = Spool::at(state.clone());
        let route = route("one", "one");
        spool
            .append(&route, &[record("too-wide"), record("next")])
            .unwrap();

        spool.quarantine_front(&route).unwrap();

        assert_eq!(
            spool.peek_batch(&route, 10, usize::MAX).unwrap()[0].line,
            "next"
        );
        assert_eq!(spool.routes()[0].pending, 1);
        let parked =
            fs::read_to_string(state.join("quarantine").join("oversize-one.ndjson")).unwrap();
        assert!(parked.contains("too-wide"));
        assert!(!spool.has_quarantine(), "only the v1 queue flips that flag");
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

    /// A queue written before identity moved to the key: named from the org, its
    /// metadata carrying no fingerprint, part of it already delivered.
    fn legacy_queue(spool: &Spool, dir_name: &str, api_url: &str, org: &str) -> PathBuf {
        let dir = spool.root.join(dir_name);
        fs::create_dir_all(&dir).unwrap();
        let meta = serde_json::json!({
            "route": {
                "id": dir_name,
                "org_id": org,
                "api_url": api_url,
                "credential_path": "/tmp/legacy.env",
                "label": "legacy"
            }
        });
        fs::write(dir.join("route.json"), format!("{meta}\n")).unwrap();
        dir
    }

    #[test]
    fn upgrades_a_legacy_queue_in_place_without_renaming_or_moving_its_cursor() {
        let state = temp_state("legacy-adopt");
        let spool = Spool::at(state.clone());
        let dir = legacy_queue(
            &spool,
            "old-name-from-org",
            "https://acme.example",
            "org-acme",
        );

        let r1 = format!("{}\n", serde_json::to_string(&record("delivered")).unwrap());
        let r2 = format!("{}\n", serde_json::to_string(&record("pending-1")).unwrap());
        let r3 = format!("{}\n", serde_json::to_string(&record("pending-2")).unwrap());
        fs::write(dir.join("records.ndjson"), format!("{r1}{r2}{r3}")).unwrap();
        let offset = r1.len() as u64;
        let scanned_to = (r1.len() + r2.len() + r3.len()) as u64;
        write_cursor_at(
            &dir,
            &Cursor {
                offset,
                pending: 2,
                scanned_to,
            },
        )
        .unwrap();

        // A live route to the same origin, declaring the same org, now carrying
        // a key. It must land in the legacy directory, not a fresh one.
        let route = RouteRef {
            id: "new-name-from-key".into(),
            org_id: Some("org-acme".into()),
            api_url: "https://acme.example".into(),
            key_fingerprint: Some(crate::destination::key_fingerprint("acme-key")),
            credential_path: PathBuf::from("/tmp/acme.env"),
            label: "acme".into(),
        };
        spool.append(&route, &[record("new-line")]).unwrap();

        let dirs = fs::read_dir(&spool.root).unwrap().flatten().count();
        assert_eq!(dirs, 1, "no fresh directory was created");

        let adopted: RouteMetadata =
            serde_json::from_str(&fs::read_to_string(dir.join("route.json")).unwrap()).unwrap();
        assert_eq!(adopted.route.id, "old-name-from-org", "never renamed");
        assert_eq!(adopted.route.key_fingerprint, route.key_fingerprint);
        assert_eq!(adopted.route.org_id.as_deref(), Some("org-acme"));

        let after = read_cursor_at(&dir.join("cursor.json"));
        assert_eq!(after.offset, offset, "delivery position untouched");
        assert_eq!(after.pending, 3);

        let lines: Vec<String> = spool
            .peek_batch(&adopted.route, 10, usize::MAX)
            .unwrap()
            .into_iter()
            .map(|r| r.line)
            .collect();
        assert_eq!(lines, ["pending-1", "pending-2", "new-line"]);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn a_declared_org_is_needed_to_match_a_fingerprintless_legacy_queue() {
        let state = temp_state("legacy-undeclared");
        let spool = Spool::at(state.clone());
        legacy_queue(&spool, "old-name", "https://acme.example", "org-acme");

        // Same origin, but the file no longer declares an org, so it cannot be
        // matched offline. A fresh queue is created; nothing is lost.
        let route = RouteRef {
            id: "fresh".into(),
            org_id: None,
            api_url: "https://acme.example".into(),
            key_fingerprint: Some(7),
            credential_path: PathBuf::from("/tmp/acme.env"),
            label: "acme".into(),
        };
        spool.append(&route, &[record("new")]).unwrap();
        assert_eq!(fs::read_dir(&spool.root).unwrap().flatten().count(), 2);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn pins_an_org_once_then_leaves_it() {
        let state = temp_state("pin");
        let spool = Spool::at(state.clone());
        let route = RouteRef {
            id: "q".into(),
            org_id: None,
            api_url: "https://acme.example".into(),
            key_fingerprint: Some(1),
            credential_path: PathBuf::from("/tmp/x.env"),
            label: "acme".into(),
        };
        spool.append(&route, &[record("a")]).unwrap();
        spool.pin_org(&route, "org-acme").unwrap();
        spool.pin_org(&route, "org-acme").unwrap();
        let meta: RouteMetadata = serde_json::from_str(
            &fs::read_to_string(spool.route_dir(&route).join("route.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(meta.route.org_id.as_deref(), Some("org-acme"));
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn repoints_a_queue_at_a_new_credential_path() {
        let state = temp_state("repoint");
        let spool = Spool::at(state.clone());
        let route = route("one", "one");
        spool.append(&route, &[record("a")]).unwrap();
        spool
            .repoint(&route, Path::new("/tmp/sibling.env"))
            .unwrap();
        let meta: RouteMetadata = serde_json::from_str(
            &fs::read_to_string(spool.route_dir(&route).join("route.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            meta.route.credential_path,
            PathBuf::from("/tmp/sibling.env")
        );
        let _ = fs::remove_dir_all(state);
    }
}
