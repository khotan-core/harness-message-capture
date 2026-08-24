use serde::Deserialize;
use std::time::Duration;

const LATEST_RELEASE: &str =
    "https://api.github.com/repos/khotan-core/harness-message-capture/releases/latest";
const CHECK_SECS: u64 = 2;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
}

/// Ask GitHub for the latest tagged release after the watcher is up.
/// A miss or a timeout stays quiet so capture is never blocked.
pub fn warn_if_stale() {
    let current = env!("CARGO_PKG_VERSION");
    let _ = std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            if let Some(msg) = stale_message(current, LATEST_RELEASE) {
                crate::log::warn(&msg);
            }
        });
}

fn stale_message(current: &str, url: &str) -> Option<String> {
    let latest = fetch_latest_tag(url)?;
    if !is_newer(&latest, current) {
        return None;
    }
    Some(format!(
        "Newer observer {latest} is out (this binary is {current})"
    ))
}

fn fetch_latest_tag(url: &str) -> Option<String> {
    let resp = ureq::get(url)
        .set("User-Agent", "khotan-observer")
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(CHECK_SECS))
        .call()
        .ok()?;
    tag_from_body(&resp.into_string().ok()?)
}

fn tag_from_body(body: &str) -> Option<String> {
    let release: Release = serde_json::from_str(body).ok()?;
    let tag = release.tag_name.trim();
    if tag.is_empty() {
        None
    } else {
        Some(tag.to_string())
    }
}

fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn reads_tag_name_from_github_json() {
        assert_eq!(
            tag_from_body(r#"{"tag_name":"v0.1.17"}"#).as_deref(),
            Some("v0.1.17")
        );
    }

    #[test]
    fn ignores_empty_or_junk_bodies() {
        assert_eq!(tag_from_body("{}"), None);
        assert_eq!(tag_from_body(""), None);
        assert_eq!(tag_from_body(r#"{"tag_name":"  "}"#), None);
    }

    #[test]
    fn newer_tag_beats_this_binary() {
        assert!(is_newer("v0.1.17", "0.1.16"));
        assert!(is_newer("0.2.0", "0.1.16"));
        assert!(!is_newer("v0.1.16", "0.1.16"));
        assert!(!is_newer("v0.1.15", "0.1.16"));
        assert!(!is_newer("latest", "0.1.16"));
    }

    #[test]
    fn warn_text_names_both_versions() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/releases/latest", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let body = r#"{"tag_name":"v0.1.17"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        assert_eq!(
            stale_message("0.1.16", &url).as_deref(),
            Some("Newer observer v0.1.17 is out (this binary is 0.1.16)")
        );
        server.join().unwrap();
    }

    #[test]
    fn same_release_stays_quiet() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/releases/latest", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let body = r#"{"tag_name":"v0.1.16"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        assert_eq!(stale_message("0.1.16", &url), None);
        server.join().unwrap();
    }

    #[test]
    fn a_dead_host_stays_quiet() {
        assert_eq!(
            stale_message("0.1.16", "http://127.0.0.1:1/releases/latest"),
            None
        );
    }
}
