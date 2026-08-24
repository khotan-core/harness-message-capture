use crate::config;
use crate::record::Record;
use crate::store;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

const MAX_BODY: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ReceiveOpts {
    pub bind: String,
    pub dir: PathBuf,
    pub token: String,
}

#[derive(Deserialize)]
struct Batch {
    device_id: String,
    records: Vec<Record>,
}

pub fn default_inbox() -> PathBuf {
    config::state_dir().join("inbox")
}

/// Blocking local ingest loop. Returns only on bind/fatal I/O errors.
pub fn serve(opts: ReceiveOpts) -> Result<()> {
    std::fs::create_dir_all(&opts.dir)
        .with_context(|| format!("create inbox {}", opts.dir.display()))?;
    let listener = TcpListener::bind(&opts.bind).with_context(|| format!("bind {}", opts.bind))?;
    eprintln!(
        "  khotan-observer receive\n\
         \n\
             Bind     {}\n\
             Inbox    {}\n\
             Token    set\n\
         \n\
           ✓ Listening  · Ctrl-C to stop\n",
        opts.bind,
        opts.dir.display()
    );

    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                if let Err(e) = handle_connection(stream, &opts) {
                    crate::log::warn(&format!("request error: {e:#}"));
                }
            }
            Err(e) => crate::log::warn(&format!("accept error: {e}")),
        }
    }
    Ok(())
}

fn handle_connection(mut stream: TcpStream, opts: &ReceiveOpts) -> Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    let mut buf = Vec::with_capacity(8192);
    let mut chunk = [0u8; 4096];
    let header_end;
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            bail!("client closed before headers");
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_header_end(&buf) {
            header_end = pos;
            break;
        }
        if buf.len() > 64 * 1024 {
            respond(&mut stream, 431, "headers too large")?;
            return Ok(());
        }
    }

    let header_bytes = &buf[..header_end];
    let header_text = String::from_utf8_lossy(header_bytes);
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    let mut content_length = 0usize;
    let mut authorization = String::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            let value = value.trim();
            if name.eq_ignore_ascii_case("Content-Length") {
                content_length = value.parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("Authorization") {
                authorization = value.to_string();
            }
        }
    }

    if method != "POST" || path != "/ingest" {
        respond(&mut stream, 404, "not found")?;
        return Ok(());
    }

    if !auth_ok(&authorization, &opts.token) {
        respond(&mut stream, 401, "unauthorized")?;
        return Ok(());
    }

    if content_length == 0 {
        respond(&mut stream, 400, "empty body")?;
        return Ok(());
    }
    if content_length > MAX_BODY {
        respond(&mut stream, 413, "body too large")?;
        return Ok(());
    }

    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            bail!("client closed before body complete");
        }
        body.extend_from_slice(&chunk[..n]);
        if body.len() > MAX_BODY {
            respond(&mut stream, 413, "body too large")?;
            return Ok(());
        }
    }
    body.truncate(content_length);

    let batch: Batch = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => {
            respond(&mut stream, 400, &format!("invalid json: {e}"))?;
            return Ok(());
        }
    };

    if batch.device_id.trim().is_empty() {
        respond(&mut stream, 400, "device_id required")?;
        return Ok(());
    }
    for (i, r) in batch.records.iter().enumerate() {
        if r.schema.trim().is_empty() || r.tool.trim().is_empty() || r.session_id.trim().is_empty()
        {
            respond(
                &mut stream,
                400,
                &format!("record[{i}] missing schema/tool/session_id"),
            )?;
            return Ok(());
        }
        if store::sanitize_segment(&r.tool).is_err()
            || store::sanitize_segment(&r.project).is_err()
            || store::sanitize_segment(&r.session_id).is_err()
            || r.thread_id
                .as_deref()
                .is_some_and(|thread| store::sanitize_segment(thread).is_err())
        {
            respond(
                &mut stream,
                400,
                &format!("record[{i}] has unsafe path fields"),
            )?;
            return Ok(());
        }
    }

    match store::append_batch(&opts.dir, &batch.device_id, &batch.records) {
        Ok((written, skipped)) => {
            eprintln!(
                "  {}   ingested {}   skipped {}   device {}",
                crate::log::dim(&crate::log::clock()),
                written,
                skipped,
                batch.device_id
            );
            respond(&mut stream, 204, "")?;
        }
        Err(e) => {
            // 5xx so the observer keeps the spool and retries.
            respond(&mut stream, 500, &format!("persist failed: {e:#}"))?;
        }
    }
    Ok(())
}

fn auth_ok(header: &str, expected: &str) -> bool {
    let prefix = "Bearer ";
    if let Some(tok) = header.strip_prefix(prefix) {
        return tok == expected;
    }
    if let Some(tok) = header.strip_prefix("bearer ") {
        return tok == expected;
    }
    false
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) -> Result<()> {
    let reason = match status {
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let body_bytes = body.as_bytes();
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body_bytes.len()
    );
    stream.write_all(header.as_bytes())?;
    if !body_bytes.is_empty() {
        stream.write_all(body_bytes)?;
    }
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Record;
    use crate::store;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_inbox() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("khotan-recv-test-{nanos}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn spawn_server(dir: PathBuf, token: &str) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let token = token.to_string();
        let (ready_tx, ready_rx) = mpsc::channel();
        let bind = addr.clone();
        let handle = thread::spawn(move || {
            ready_tx.send(()).unwrap();
            if let Ok((stream, _)) = listener.accept() {
                let opts = ReceiveOpts { bind, dir, token };
                let _ = handle_connection(stream, &opts);
            }
        });
        ready_rx.recv().unwrap();
        (addr, handle)
    }

    fn post(addr: &str, token: &str, body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(addr).unwrap();
        let req = format!(
            "POST /ingest HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        let status = resp
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, resp)
    }

    #[test]
    fn rejects_bad_token() {
        let dir = tmp_inbox();
        let (addr, handle) = spawn_server(dir.clone(), "secret");
        let (status, _) = post(&addr, "wrong", r#"{"device_id":"d","records":[]}"#);
        assert_eq!(status, 401);
        handle.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accepts_and_persists() {
        let dir = tmp_inbox();
        let (addr, handle) = spawn_server(dir.clone(), "secret");
        let rec = Record {
            schema: "v1".into(),
            tool: "cursor".into(),
            project: "proj".into(),
            session_id: "s1".into(),
            thread_id: None,
            agent_role: None,
            seq: None,
            captured_at_ms: 42,
            line: r#"{"role":"user","message":{"content":[{"type":"text","text":"hello"}]}}"#
                .into(),
        };
        let body = serde_json::json!({
            "device_id": "devabc",
            "records": [rec]
        })
        .to_string();
        let (status, _) = post(&addr, "secret", &body);
        assert_eq!(status, 204);
        handle.join().unwrap();
        let listed = store::list_records(&dir, &store::ReadFilter::default()).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].line.contains("hello"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auth_ok_bearer() {
        assert!(auth_ok("Bearer tok", "tok"));
        assert!(!auth_ok("Bearer other", "tok"));
        assert!(!auth_ok("tok", "tok"));
    }
}
