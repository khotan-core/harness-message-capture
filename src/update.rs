use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const LATEST_RELEASE: &str =
    "https://api.github.com/repos/khotan-core/harness-message-capture/releases/latest";
const DEFAULT_REPO: &str = "khotan-core/harness-message-capture";
const CHECK_SECS: u64 = 2;
const DOWNLOAD_SECS: u64 = 60;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
}

#[derive(Debug, PartialEq, Eq)]
struct UpdateArgs {
    version: Option<String>,
}

/// Ask GitHub for the latest tagged release after the watcher is up.
/// A miss or a timeout stays quiet so capture is never blocked.
pub fn warn_if_stale() {
    let current = env!("CARGO_PKG_VERSION");
    let _ = std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            if let Some(msg) = stale_message(current, LATEST_RELEASE) {
                crate::log::alert(&msg);
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

fn fetch_latest_tag_err(url: &str) -> Result<String> {
    let resp = ureq::get(url)
        .set("User-Agent", "khotan-observer")
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(10))
        .call()
        .with_context(|| format!("ask GitHub for the latest release: {url}"))?;
    tag_from_body(&resp.into_string().context("read latest-release body")?)
        .context("latest release has no tag_name")
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

fn normalize_tag(s: &str) -> String {
    let s = s.trim();
    if s.starts_with('v') {
        s.to_string()
    } else {
        format!("v{s}")
    }
}

fn parse_update_args(args: &[String]) -> Result<UpdateArgs> {
    let mut version = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--version" => {
                let value = args
                    .get(i + 1)
                    .context("--version requires a release tag")?;
                if value.starts_with('-') || value.is_empty() {
                    bail!("--version requires a release tag");
                }
                version = Some(normalize_tag(value));
                i += 2;
            }
            other => bail!("unknown flag {other}"),
        }
    }
    Ok(UpdateArgs { version })
}

fn bin_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("KHOTAN_OBSERVER_BIN_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    crate::config::home().join(".local").join("bin")
}

fn default_install_path() -> PathBuf {
    crate::config::home()
        .join(".local")
        .join("bin")
        .join("khotan-observer")
}

fn apple_target() -> Result<&'static str> {
    if !cfg!(target_os = "macos") {
        bail!("only macOS is supported");
    }
    match std::env::consts::ARCH {
        "aarch64" => Ok("aarch64-apple-darwin"),
        "x86_64" => Ok("x86_64-apple-darwin"),
        other => bail!("unsupported architecture: {other}"),
    }
}

fn repo() -> String {
    std::env::var("KHOTAN_OBSERVER_REPO")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_REPO.to_string())
}

fn release_base(tag: &str) -> String {
    if let Ok(base) = std::env::var("KHOTAN_OBSERVER_RELEASE_BASE") {
        let base = base.trim().trim_end_matches('/').to_string();
        if !base.is_empty() {
            return base;
        }
    }
    let repo = repo();
    format!("https://github.com/{repo}/releases/download/{tag}")
}

fn latest_api_url() -> String {
    if let Ok(url) = std::env::var("KHOTAN_OBSERVER_LATEST_API") {
        let url = url.trim().to_string();
        if !url.is_empty() {
            return url;
        }
    }
    format!("https://api.github.com/repos/{}/releases/latest", repo())
}

fn checksum_hash(text: &str) -> Option<String> {
    let hash = text.split_whitespace().next()?.trim();
    if hash.is_empty() {
        None
    } else {
        Some(hash.to_string())
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let out = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .context("run shasum")?;
    if !out.status.success() {
        bail!("shasum failed for {}", path.display());
    }
    checksum_hash(&String::from_utf8_lossy(&out.stdout)).context("shasum printed no hash")
}

fn installed_tag(dest: &Path) -> Option<String> {
    let out = Command::new(dest).arg("version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let tag = String::from_utf8_lossy(&out.stdout).trim().to_string();
    parse_version(&tag)?;
    Some(normalize_tag(&tag))
}

fn download(url: &str, dest: &Path) -> Result<()> {
    let resp = ureq::get(url)
        .set("User-Agent", "khotan-observer")
        .timeout(Duration::from_secs(DOWNLOAD_SECS))
        .call()
        .with_context(|| format!("download {url}"))?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {url}"))?;
    fs::write(dest, bytes).with_context(|| format!("write {}", dest.display()))?;
    Ok(())
}

struct UpdatePlan {
    version: Option<String>,
    bin_dir: PathBuf,
    release_base: Option<String>,
    latest_api: String,
}

fn plan_from_env(args: UpdateArgs) -> UpdatePlan {
    let version = args.version.or_else(|| {
        std::env::var("KHOTAN_OBSERVER_VERSION")
            .ok()
            .map(|raw| raw.trim().to_string())
            .filter(|raw| !raw.is_empty() && raw != "latest")
            .map(|raw| normalize_tag(&raw))
    });
    UpdatePlan {
        version,
        bin_dir: bin_dir(),
        release_base: std::env::var("KHOTAN_OBSERVER_RELEASE_BASE")
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty()),
        latest_api: latest_api_url(),
    }
}

/// Replace `~/.local/bin/khotan-observer` with a GitHub Release binary.
pub fn run(args: &[String]) -> Result<()> {
    apply(plan_from_env(parse_update_args(args)?))
}

fn apply(plan: UpdatePlan) -> Result<()> {
    let tag = match plan.version {
        Some(tag) => tag,
        None => fetch_latest_tag_err(&plan.latest_api)?,
    };
    let dest = plan.bin_dir.join("khotan-observer");
    if installed_tag(&dest).as_deref() == Some(tag.as_str()) {
        println!("already on {tag}");
        return Ok(());
    }

    let target = apple_target()?;
    let asset = format!("khotan-observer-{target}");
    let base = plan.release_base.unwrap_or_else(|| release_base(&tag));
    let tmp = std::env::temp_dir().join(format!("khotan-observer-update-{}", std::process::id()));
    fs::create_dir_all(&tmp).context("create update temp dir")?;
    let bin_tmp = tmp.join(&asset);
    let sum_tmp = tmp.join(format!("{asset}.sha256"));
    let result = (|| {
        println!("downloading {asset} ({tag})");
        download(&format!("{base}/{asset}"), &bin_tmp)?;
        download(&format!("{base}/{asset}.sha256"), &sum_tmp)?;
        let expected = checksum_hash(&fs::read_to_string(&sum_tmp).context("read checksum file")?)
            .context("empty checksum file")?;
        let actual = sha256_file(&bin_tmp)?;
        if expected != actual {
            bail!("checksum mismatch (expected {expected}, got {actual})");
        }
        replace_install(&dest, &bin_tmp)
    })();
    let _ = fs::remove_dir_all(&tmp);
    result?;
    println!("{} updated to {tag}", crate::log::green("✓"));
    println!("  binary: {}", dest.display());
    Ok(())
}

fn replace_install(dest: &Path, src: &Path) -> Result<()> {
    let manage_agent = dest == default_install_path() && crate::agent::is_loaded();
    if manage_agent {
        println!("stopping background observer");
        crate::agent::release_for_foreground()?;
        std::thread::sleep(Duration::from_secs(1));
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    // New inode. Overwriting in place makes later execs die with SIGKILL.
    let _ = fs::remove_file(dest);
    fs::copy(src, dest).with_context(|| format!("install {}", dest.display()))?;
    fs::set_permissions(dest, fs::Permissions::from_mode(0o755))?;
    let _ = Command::new(dest).args(["docs", "--write"]).status();
    if manage_agent {
        println!("restarting background observer");
        let status = Command::new(dest)
            .arg("start")
            .status()
            .context("start updated observer")?;
        if !status.success() {
            bail!("updated binary is in place, but `start` failed");
        }
    }
    Ok(())
}

pub fn print_version() {
    println!("v{}", env!("CARGO_PKG_VERSION"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{bail, Context, Result};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

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

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn parse_update_defaults_to_latest() {
        assert_eq!(
            parse_update_args(&s(&[])).unwrap(),
            UpdateArgs { version: None }
        );
    }

    #[test]
    fn parse_update_pins_a_tag() {
        assert_eq!(
            parse_update_args(&s(&["--version", "0.1.21"])).unwrap(),
            UpdateArgs {
                version: Some("v0.1.21".into())
            }
        );
    }

    #[test]
    fn parse_update_rejects_unknown_flag() {
        let err = parse_update_args(&s(&["--nope"])).unwrap_err();
        assert!(err.to_string().contains("unknown flag"));
    }

    #[test]
    fn checksum_reads_hash_or_gnu_line() {
        assert_eq!(checksum_hash("abc123\n").as_deref(), Some("abc123"));
        assert_eq!(
            checksum_hash("abc123  khotan-observer-aarch64-apple-darwin\n").as_deref(),
            Some("abc123")
        );
        assert_eq!(checksum_hash("   \n"), None);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("hmc-update-{name}-{stamp}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn verify_pair(bin: &Path, checksum_text: &str) -> Result<()> {
        let expected = checksum_hash(checksum_text).context("empty checksum")?;
        let actual = sha256_file(bin)?;
        if expected != actual {
            bail!("checksum mismatch (expected {expected}, got {actual})");
        }
        Ok(())
    }

    #[test]
    fn replace_install_writes_a_new_inode() {
        let root = temp_dir("replace");
        let src = root.join("payload");
        fs::write(&src, "#!/bin/sh\necho v9.9.9\n").unwrap();
        let dest = root.join("bin").join("khotan-observer");
        replace_install(&dest, &src).unwrap();
        assert_eq!(installed_tag(&dest).as_deref(), Some("v9.9.9"));
        assert_eq!(dest.metadata().unwrap().permissions().mode() & 0o111, 0o111);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn update_is_a_noop_when_the_install_already_matches() {
        let root = temp_dir("noop");
        let dest_dir = root.join("bin");
        fs::create_dir_all(&dest_dir).unwrap();
        let dest = dest_dir.join("khotan-observer");
        fs::write(&dest, "#!/bin/sh\necho v9.9.9\n").unwrap();
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
        apply(UpdatePlan {
            version: Some("v9.9.9".into()),
            bin_dir: dest_dir,
            release_base: Some("http://127.0.0.1:1".into()),
            latest_api: String::new(),
        })
        .unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn update_rejects_a_bad_checksum() {
        let root = temp_dir("bad");
        let bin = root.join("payload");
        fs::write(&bin, b"nope").unwrap();
        let err = verify_pair(&bin, "deadbeef\n").unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn update_accepts_a_matching_checksum() {
        let root = temp_dir("sum");
        let bin = root.join("payload");
        fs::write(&bin, b"ok").unwrap();
        let hash = sha256_file(&bin).unwrap();
        verify_pair(&bin, &format!("{hash}\n")).unwrap();
        let _ = fs::remove_dir_all(root);
    }
}
