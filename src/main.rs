mod agent;
mod capture;
mod config;
mod log;
mod reader;
mod receiver;
mod record;
mod redact;
mod sources;
mod spool;
mod store;
mod uploader;

use anyhow::{Context, Result};
use capture::Offsets;
use config::Config;
use notify::{RecursiveMode, Watcher};
use spool::Spool;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

fn main() {
    if let Err(e) = run() {
        eprintln!("khotan-observer: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("help");
    match cmd {
        "configure" => configure(&args[2..]),
        "run" => watch(),
        "start" => agent::start(),
        "stop" => agent::stop(),
        "uninstall" => agent::uninstall(),
        "logs" => agent::logs(!args.iter().any(|a| a == "--no-follow")),
        "run-once" => run_once(),
        "status" => status(),
        "receive" => receive_cmd(&args[2..]),
        "read" => read_cmd(&args[2..]),
        _ => {
            print_help();
            Ok(())
        }
    }
}

fn print_help() {
    eprintln!(
        "khotan-observer — capture local AI coding-agent transcripts\n\
         \n\
         USAGE:\n\
           khotan-observer configure --endpoint <url> [--token <tok>]\n\
           khotan-observer run          Capture in the foreground (Ctrl-C to stop)\n\
           khotan-observer start        Install & start the background LaunchAgent\n\
           khotan-observer stop         Stop the background LaunchAgent\n\
           khotan-observer logs         Follow the background log\n\
           khotan-observer uninstall    Stop & remove the LaunchAgent\n\
           khotan-observer status       Show config, sources, and spool state\n\
           khotan-observer run-once     Single scan + upload pass, then exit\n\
           khotan-observer receive      Local ingest server (writes to an inbox dir)\n\
           khotan-observer read         Inspect messages stored in the inbox\n"
    );
}

#[derive(Debug, PartialEq, Eq)]
struct ConfigureArgs {
    endpoint: String,
    token: Option<String>,
    poll: Option<u64>,
    batch: Option<usize>,
}

fn parse_configure_args(args: &[String]) -> Result<ConfigureArgs> {
    let mut endpoint = None;
    let mut token = None;
    let mut poll = None;
    let mut batch = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--endpoint" => {
                endpoint = args.get(i + 1).cloned();
                i += 2;
            }
            "--token" => {
                token = args.get(i + 1).cloned();
                i += 2;
            }
            "--poll" => {
                poll = args.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            "--batch" => {
                batch = args.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            other => anyhow::bail!("unknown flag: {other}"),
        }
    }
    Ok(ConfigureArgs {
        endpoint: endpoint.context("--endpoint is required")?,
        token,
        poll,
        batch,
    })
}

fn configure(args: &[String]) -> Result<()> {
    let parsed = parse_configure_args(args)?;
    let token = match parsed.token {
        Some(t) if !t.is_empty() => t,
        _ => prompt_token()?,
    };
    if token.is_empty() {
        anyhow::bail!("token is required");
    }

    // Preserve an existing device_id across re-configuration.
    let device_id = Config::load()
        .ok()
        .map(|c| c.device_id)
        .unwrap_or(config::random_id()?);

    let cfg = Config {
        endpoint: parsed.endpoint,
        token,
        device_id,
        poll_secs: parsed.poll.unwrap_or(45),
        batch: parsed.batch.unwrap_or(200),
    };
    cfg.save()?;
    println!("configured. device_id={}", cfg.device_id);
    println!("config: {}", config::config_path().display());
    println!("next: `khotan-observer run` (foreground) or `khotan-observer start` (background)");
    Ok(())
}

/// Prompt for a token on /dev/tty with echo disabled (macOS / Unix).
fn prompt_token() -> Result<String> {
    let mut tty_out = fs_open_tty_write()?;
    write!(tty_out, "Enrollment token: ")?;
    tty_out.flush()?;

    // Disable echo so the token isn't visible in the terminal.
    let _ = std::process::Command::new("stty")
        .args(["-echo"])
        .stdin(std::process::Stdio::inherit())
        .status();

    let mut line = String::new();
    let result = io::BufReader::new(fs_open_tty_read()?).read_line(&mut line);

    let _ = std::process::Command::new("stty")
        .args(["echo"])
        .stdin(std::process::Stdio::inherit())
        .status();
    writeln!(tty_out)?;

    result.context("read token from terminal")?;
    Ok(line.trim().to_string())
}

fn fs_open_tty_read() -> Result<std::fs::File> {
    std::fs::File::open("/dev/tty").context("open /dev/tty for reading")
}

fn fs_open_tty_write() -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .context("open /dev/tty for writing")
}

fn status() -> Result<()> {
    let cfg = Config::load()?;
    let masked = if cfg.token.len() > 6 {
        format!("{}…", &cfg.token[..6])
    } else {
        "set".into()
    };
    println!("endpoint : {}", cfg.endpoint);
    println!("token    : {masked}");
    println!("device_id: {}", cfg.device_id);
    println!("poll_secs: {}", cfg.poll_secs);
    println!("batch    : {}", cfg.batch);
    let running = agent::is_loaded();
    println!(
        "background: {}",
        if running {
            log::green("running")
        } else {
            log::dim("stopped")
        }
    );
    println!("log file : {}", agent::log_file().display());
    println!("sources  :");
    for s in sources::discover() {
        println!("  [{}] {}", s.tool, s.root.display());
    }
    let offsets = Offsets::load();
    println!("tracked files: {}", offsets.len());
    println!("spool pending: {}", Spool::open().pending());
    println!("inbox dir    : {}", receiver::default_inbox().display());
    Ok(())
}

/// Single pass: capture new lines, spool them, drain the spool. Handy for tests.
fn run_once() -> Result<()> {
    let cfg = Config::load()?;
    let srcs = sources::discover();
    let mut offsets = Offsets::load();
    let spool = Spool::open();
    let pass = scan_and_ship(&cfg, &srcs, &mut offsets, &spool);
    if !report(pass, &spool) {
        log::activity(0, 0, spool.pending(), None, Some("nothing new to capture"));
    }
    Ok(())
}

/// How long the loop may sit quiet before printing proof-of-life.
const IDLE_HEARTBEAT: Duration = Duration::from_secs(300);

fn watch() -> Result<()> {
    let started = std::time::Instant::now();
    let cfg = Config::load()?;
    let srcs = sources::discover();
    let mut offsets = Offsets::load();
    let spool = Spool::open();

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .context("create fs watcher")?;

    for s in &srcs {
        // Best-effort: a missing/again-permissioned dir shouldn't kill the daemon.
        let _ = watcher.watch(&s.root, RecursiveMode::Recursive);
    }

    let tools: Vec<&str> = srcs.iter().map(|s| s.tool).collect();
    log::banner(
        &cfg.endpoint,
        &cfg.device_id,
        &tools,
        offsets.len(),
        started.elapsed().as_millis(),
    );
    if srcs.is_empty() {
        log::warn("no coding-agent transcript directories found — nothing to capture");
    }

    // Catch up on anything appended while we were stopped.
    report(scan_and_ship(&cfg, &srcs, &mut offsets, &spool), &spool);

    let mut last_output = std::time::Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_secs(cfg.poll_secs)) {
            Ok(_evt) => {
                // Debounce a burst of writes, then coalesce into one scan.
                std::thread::sleep(Duration::from_millis(300));
                while rx.try_recv().is_ok() {}
                if report(scan_and_ship(&cfg, &srcs, &mut offsets, &spool), &spool) {
                    last_output = std::time::Instant::now();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Fallback pass: covers missed events and retries the spool.
                if report(scan_and_ship(&cfg, &srcs, &mut offsets, &spool), &spool) {
                    last_output = std::time::Instant::now();
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if last_output.elapsed() >= IDLE_HEARTBEAT {
            log::idle(offsets.len(), spool.pending());
            last_output = std::time::Instant::now();
        }
    }
    Ok(())
}

/// Outcome of one capture + upload pass.
#[derive(Default)]
struct Pass {
    captured: usize,
    uploaded: usize,
    /// Human-readable workspace/thread labels touched this pass.
    threads: Option<String>,
    warn: Option<String>,
}

/// Print a line for any pass that did something. Returns whether it printed.
fn report(pass: Pass, spool: &Spool) -> bool {
    if pass.captured == 0 && pass.uploaded == 0 && pass.warn.is_none() {
        return false;
    }
    log::activity(
        pass.captured,
        pass.uploaded,
        spool.pending(),
        pass.threads.as_deref(),
        pass.warn.as_deref(),
    );
    true
}

fn scan_and_ship(
    cfg: &Config,
    srcs: &[sources::Source],
    offsets: &mut Offsets,
    spool: &Spool,
) -> Pass {
    let mut pass = Pass::default();
    let records = capture::collect_new(srcs, offsets);
    if !records.is_empty() {
        pass.threads = Some(capture::thread_summary(&records));
        if let Err(e) = spool.append(&records) {
            pass.warn = Some(format!("could not write to spool: {e}"));
            return pass; // don't advance offsets if we couldn't persist
        }
        pass.captured = records.len();
        if let Err(e) = offsets.save() {
            pass.warn = Some(format!("could not save offsets: {e}"));
        }
    }
    let (uploaded, warn) = drain(cfg, spool);
    pass.uploaded = uploaded;
    pass.warn = pass.warn.or(warn);
    pass
}

/// Ship spooled records until the spool is empty or the endpoint pushes back.
fn drain(cfg: &Config, spool: &Spool) -> (usize, Option<String>) {
    let mut uploaded = 0;
    loop {
        let batch = spool.peek(cfg.batch);
        if batch.is_empty() {
            return (uploaded, None);
        }
        match uploader::send(cfg, &batch) {
            uploader::Upload::Ok => {
                let _ = spool.drop_front(batch.len());
                uploaded += batch.len();
            }
            uploader::Upload::Drop(reason) => {
                let n = batch.len();
                let _ = spool.drop_front(n);
                return (uploaded, Some(format!("dropped {n} record(s): {reason}")));
            }
            uploader::Upload::Retry(reason) => {
                // Leave everything spooled; try again on the next pass.
                return (uploaded, Some(format!("{reason} — retrying")));
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ReceiveArgs {
    bind: String,
    dir: PathBuf,
    token: Option<String>,
}

fn parse_receive_args(args: &[String]) -> Result<ReceiveArgs> {
    let mut bind = None;
    let mut dir = None;
    let mut token = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                bind = args.get(i + 1).cloned();
                i += 2;
            }
            "--dir" => {
                dir = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--token" => {
                token = args.get(i + 1).cloned();
                i += 2;
            }
            other => anyhow::bail!("unknown flag: {other}"),
        }
    }
    Ok(ReceiveArgs {
        bind: bind.unwrap_or_else(|| "127.0.0.1:8787".into()),
        dir: dir.unwrap_or_else(receiver::default_inbox),
        token,
    })
}

fn receive_cmd(args: &[String]) -> Result<()> {
    let parsed = parse_receive_args(args)?;
    let token = match parsed.token {
        Some(t) if !t.is_empty() => t,
        _ => Config::load()
            .map(|c| c.token)
            .context("token required — pass --token or run configure first")?,
    };
    if token.is_empty() {
        anyhow::bail!("token is required");
    }
    receiver::serve(receiver::ReceiveOpts {
        bind: parsed.bind,
        dir: parsed.dir,
        token,
    })
}

#[derive(Debug, PartialEq, Eq)]
struct ReadArgs {
    dir: PathBuf,
    tool: Option<String>,
    project: Option<String>,
    session: Option<String>,
    device: Option<String>,
    limit: usize,
    raw: bool,
}

fn parse_read_args(args: &[String]) -> Result<ReadArgs> {
    let mut dir = None;
    let mut tool = None;
    let mut project = None;
    let mut session = None;
    let mut device = None;
    let mut limit = None;
    let mut raw = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                dir = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--tool" => {
                tool = args.get(i + 1).cloned();
                i += 2;
            }
            "--project" => {
                project = args.get(i + 1).cloned();
                i += 2;
            }
            "--session" => {
                session = args.get(i + 1).cloned();
                i += 2;
            }
            "--device" => {
                device = args.get(i + 1).cloned();
                i += 2;
            }
            "--limit" => {
                let raw = args.get(i + 1).context("--limit requires a positive integer")?;
                limit = Some(
                    raw.parse::<usize>()
                        .context("--limit requires a positive integer")?,
                );
                i += 2;
            }
            "--raw" => {
                raw = true;
                i += 1;
            }
            other => anyhow::bail!("unknown flag: {other}"),
        }
    }
    Ok(ReadArgs {
        dir: dir.unwrap_or_else(receiver::default_inbox),
        tool,
        project,
        session,
        device,
        limit: limit.unwrap_or(50),
        raw,
    })
}

fn read_cmd(args: &[String]) -> Result<()> {
    let parsed = parse_read_args(args)?;
    reader::run(reader::ReadOpts {
        dir: parsed.dir,
        filter: store::ReadFilter {
            device_id: parsed.device,
            tool: parsed.tool,
            project: parsed.project,
            session_id: parsed.session,
        },
        limit: parsed.limit,
        raw: parsed.raw,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn parse_configure_requires_endpoint() {
        let err = parse_configure_args(&s(&["--token", "abc"])).unwrap_err();
        assert!(err.to_string().contains("--endpoint"));
    }

    #[test]
    fn parse_configure_with_token() {
        let parsed =
            parse_configure_args(&s(&["--endpoint", "http://x/ingest", "--token", "tok"])).unwrap();
        assert_eq!(parsed.endpoint, "http://x/ingest");
        assert_eq!(parsed.token.as_deref(), Some("tok"));
        assert_eq!(parsed.poll, None);
        assert_eq!(parsed.batch, None);
    }

    #[test]
    fn parse_configure_optional_flags() {
        let parsed = parse_configure_args(&s(&[
            "--endpoint",
            "http://x/ingest",
            "--poll",
            "10",
            "--batch",
            "50",
        ]))
        .unwrap();
        assert!(parsed.token.is_none());
        assert_eq!(parsed.poll, Some(10));
        assert_eq!(parsed.batch, Some(50));
    }

    #[test]
    fn parse_configure_rejects_unknown_flag() {
        let err = parse_configure_args(&s(&["--endpoint", "http://x", "--nope"])).unwrap_err();
        assert!(err.to_string().contains("unknown flag"));
    }

    #[test]
    fn parse_receive_defaults() {
        let parsed = parse_receive_args(&s(&[])).unwrap();
        assert_eq!(parsed.bind, "127.0.0.1:8787");
        assert!(parsed.token.is_none());
        assert!(parsed.dir.ends_with("inbox"));
    }

    #[test]
    fn parse_receive_flags() {
        let parsed = parse_receive_args(&s(&[
            "--bind",
            "127.0.0.1:9000",
            "--dir",
            "/tmp/inbox",
            "--token",
            "t",
        ]))
        .unwrap();
        assert_eq!(parsed.bind, "127.0.0.1:9000");
        assert_eq!(parsed.dir, PathBuf::from("/tmp/inbox"));
        assert_eq!(parsed.token.as_deref(), Some("t"));
    }

    #[test]
    fn parse_read_flags() {
        let parsed = parse_read_args(&s(&[
            "--tool",
            "cursor",
            "--session",
            "abc",
            "--limit",
            "10",
            "--raw",
        ]))
        .unwrap();
        assert_eq!(parsed.tool.as_deref(), Some("cursor"));
        assert_eq!(parsed.session.as_deref(), Some("abc"));
        assert_eq!(parsed.limit, 10);
        assert!(parsed.raw);
    }
}
