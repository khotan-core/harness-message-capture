mod agent;
mod capture;
mod config;
mod destination;
mod log;
mod reader;
mod receiver;
mod record;
mod redact;
mod singleton;
mod sources;
mod spool;
mod store;
mod uploader;
mod workspace;

use anyhow::{Context, Result};
use capture::Offsets;
use config::Config;
use notify::{RecursiveMode, Watcher};
use spool::Spool;
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
        "clear-queue" => clear_queue(&args[2..]),
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
           khotan-observer configure [--poll <seconds>] [--batch <count>] [--search-root <path>]\n\
           khotan-observer run          Capture in the foreground (Ctrl-C stops and returns to the shell)\n\
           khotan-observer start        Install & start the background LaunchAgent\n\
           khotan-observer stop         Stop the background LaunchAgent\n\
           khotan-observer logs         Follow the background log\n\
           khotan-observer uninstall    Stop & remove the LaunchAgent\n\
           khotan-observer status       Show config, sources, and spool state\n\
           khotan-observer run-once     Single scan + upload pass, then exit\n\
           khotan-observer receive      Local ingest server (writes to an inbox dir)\n\
           khotan-observer read         Inspect messages stored in the inbox\n\
           khotan-observer clear-queue --yes  Permanently discard queued records\n"
    );
}

#[derive(Debug, PartialEq, Eq)]
struct ConfigureArgs {
    poll: Option<u64>,
    batch: Option<usize>,
    search_roots: Vec<PathBuf>,
}

fn parse_configure_args(args: &[String]) -> Result<ConfigureArgs> {
    let mut poll = None;
    let mut batch = None;
    let mut search_roots = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--poll" => {
                let value = args.get(i + 1).context("--poll requires seconds")?;
                poll = Some(value.parse().context("--poll requires seconds")?);
                i += 2;
            }
            "--batch" => {
                let value = args.get(i + 1).context("--batch requires a count")?;
                batch = Some(value.parse().context("--batch requires a count")?);
                i += 2;
            }
            "--search-root" => {
                let value = args.get(i + 1).context("--search-root requires a path")?;
                search_roots.push(PathBuf::from(value));
                i += 2;
            }
            other => anyhow::bail!("unknown flag: {other}"),
        }
    }
    Ok(ConfigureArgs {
        poll,
        batch,
        search_roots,
    })
}

fn configure(args: &[String]) -> Result<()> {
    let parsed = parse_configure_args(args)?;
    let mut cfg = Config::load().unwrap_or(Config::fresh(config::random_id()?));
    if let Some(poll) = parsed.poll {
        cfg.poll_secs = poll;
    }
    if let Some(batch) = parsed.batch {
        cfg.batch = batch;
    }
    if !parsed.search_roots.is_empty() {
        cfg.search_roots = parsed.search_roots;
    }
    cfg.endpoint = None;
    cfg.token = None;
    cfg.save()?;
    println!("configured. device_id={}", cfg.device_id);
    println!("config: {}", config::config_path().display());
    println!("edit allow_repos in that file to choose which repositories upload");
    Ok(())
}

fn status() -> Result<()> {
    let cfg = Config::load()?;
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
    println!("search roots:");
    for root in &cfg.search_roots {
        println!("  {}", root.display());
    }
    println!("config    : {}", config::config_path().display());
    if cfg.allow_repos.is_empty() {
        println!("allow     : none — edit allow_repos in the config file");
    } else {
        println!("allow     :");
        for name in &cfg.allow_repos {
            println!("  {name}");
        }
    }
    let workspaces = workspace::WorkspaceIndex::discover(&cfg.search_roots);
    let routes = destination::discover_routes(workspaces.candidates(), &cfg.allow_repos);
    println!("customer routes: {}", routes.len());
    for route in routes {
        println!("  {}", route.label);
    }
    let offsets = Offsets::load();
    println!("tracked files: {}", offsets.len());
    let spool = Spool::open();
    println!("spool pending: {}", spool.pending());
    println!(
        "legacy quarantine: {}",
        if spool.has_quarantine() {
            "present"
        } else {
            "none"
        }
    );
    println!("inbox dir    : {}", receiver::default_inbox().display());
    Ok(())
}

/// Single pass: capture new lines, spool them, drain the spool. Handy for tests.
fn run_once() -> Result<()> {
    let _lock = singleton::acquire()?;
    let cfg = Config::load()?;
    let srcs = sources::discover();
    let mut offsets = Offsets::load();
    let spool = Spool::open();
    let pass = scan_and_ship(&cfg, &srcs, &mut offsets, &spool);
    if !report(pass, &spool) {
        log::activity(
            0,
            0,
            0,
            spool.pending(),
            None,
            Some("nothing new to capture"),
        );
    }
    Ok(())
}

/// How long the loop may sit quiet before printing proof-of-life.
const IDLE_HEARTBEAT: Duration = Duration::from_secs(300);

fn acquire_foreground_lock() -> Result<singleton::ObserverLock> {
    let released = agent::release_for_foreground()?;
    if !released {
        return singleton::acquire();
    }
    // launchd needs a moment to exit the previous `run` and drop the lock.
    const ATTEMPTS: u32 = 20;
    let mut last = None;
    for _ in 0..ATTEMPTS {
        match singleton::acquire() {
            Ok(lock) => return Ok(lock),
            Err(error) => {
                last = Some(error);
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    Err(last.expect("lock retry always records an error"))
}

fn watch() -> Result<()> {
    let _lock = acquire_foreground_lock()?;
    let started = std::time::Instant::now();
    let mut cfg = Config::load()?;
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
    let workspaces = workspace::WorkspaceIndex::discover(&cfg.search_roots);
    let route_count = destination::discover_routes(workspaces.candidates(), &cfg.allow_repos).len();
    log::banner(
        &cfg.device_id,
        &tools,
        offsets.len(),
        route_count,
        started.elapsed().as_millis(),
    );
    if srcs.is_empty() {
        log::warn("no coding-agent transcript directories found — nothing to capture");
    }

    // Catch up on anything appended while we were stopped.
    report(scan_and_ship(&cfg, &srcs, &mut offsets, &spool), &spool);

    let mut last_output = std::time::Instant::now();
    loop {
        if let Ok(next) = Config::load() {
            cfg = next;
        }
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
    skipped: usize,
    /// Human-readable workspace/thread labels touched this pass.
    threads: Option<String>,
    warn: Option<String>,
}

/// Print a line for any pass that did something. Returns whether it printed.
fn report(pass: Pass, spool: &Spool) -> bool {
    if pass.captured == 0 && pass.uploaded == 0 && pass.skipped == 0 && pass.warn.is_none() {
        return false;
    }
    log::activity(
        pass.captured,
        pass.uploaded,
        pass.skipped,
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
    let mut warnings = Vec::new();
    match spool.quarantine_legacy() {
        Ok(Some(_)) => warnings.push("legacy unrouted queue moved to quarantine".to_string()),
        Ok(None) => {}
        Err(error) => warnings.push(format!("could not quarantine legacy queue: {error}")),
    }

    let workspaces = workspace::WorkspaceIndex::discover(&cfg.search_roots);
    let files = capture::collect_new(srcs, offsets, &workspaces, &cfg.allow_repos);
    let mut summary_records = Vec::new();
    let mut offsets_changed = false;
    for file in files {
        if let Some(warning) = file.route_warning {
            warnings.push(format!("destination blocked: {warning}"));
        }
        match file.route {
            Some(route) => match spool.append(&route, &file.records) {
                Ok(()) => {
                    pass.captured += file.records.len();
                    summary_records.extend(file.records);
                    offsets.set(file.offset_key, file.next_offset);
                    offsets_changed = true;
                }
                Err(error) => warnings.push(format!(
                    "could not queue {} records for {}: {error}",
                    file.records.len(),
                    route.label
                )),
            },
            None => {
                if file.advance_unrouted {
                    pass.skipped += file.records.len();
                    offsets.set(file.offset_key, file.next_offset);
                    offsets_changed = true;
                }
            }
        }
    }
    if !summary_records.is_empty() {
        pass.threads = Some(capture::thread_summary(&summary_records));
    }
    if offsets_changed {
        if let Err(e) = offsets.save() {
            warnings.push(format!("could not save offsets: {e}"));
        }
    }
    let (uploaded, drain_warnings) = drain(cfg, spool);
    pass.uploaded = uploaded;
    warnings.extend(drain_warnings);
    if !warnings.is_empty() {
        const MAX_WARNINGS: usize = 3;
        let extra = warnings.len().saturating_sub(MAX_WARNINGS);
        warnings.truncate(MAX_WARNINGS);
        if extra > 0 {
            warnings.push(format!("{extra} more warning(s)"));
        }
        pass.warn = Some(warnings.join("; "));
    }
    pass
}

/// Drain each customer independently so one blocked route cannot stall others.
fn drain(cfg: &Config, spool: &Spool) -> (usize, Vec<String>) {
    let mut uploaded = 0;
    let mut warnings = Vec::new();
    for queued in spool.routes() {
        if !destination::route_allowed(&queued.route, &cfg.allow_repos) {
            continue;
        }
        loop {
            let batch = match spool.peek(&queued.route, cfg.batch) {
                Ok(batch) => batch,
                Err(error) => {
                    warnings.push(format!(
                        "{} queue is blocked by corrupt data: {error}",
                        queued.route.label
                    ));
                    break;
                }
            };
            if batch.is_empty() {
                break;
            }
            match uploader::send(&cfg.device_id, &queued.route, &batch) {
                uploader::Upload::Ok => {
                    if let Err(error) = spool.drop_front(&queued.route, batch.len()) {
                        warnings.push(format!(
                            "{} uploaded but queue could not advance: {error}",
                            queued.route.label
                        ));
                        break;
                    }
                    uploaded += batch.len();
                }
                uploader::Upload::Retry(reason) => {
                    warnings.push(format!("{reason} — retrying"));
                    break;
                }
                uploader::Upload::Blocked(reason) => {
                    warnings.push(format!("{reason} — records retained"));
                    break;
                }
            }
        }
    }
    (uploaded, warnings)
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
            .ok()
            .and_then(|c| c.token)
            .context("token required — pass --token explicitly")?,
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
                let raw = args
                    .get(i + 1)
                    .context("--limit requires a positive integer")?;
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

/// Permanently discard records that have not yet reached the ingest endpoint.
fn clear_queue(args: &[String]) -> Result<()> {
    if args != ["--yes"] {
        anyhow::bail!(
            "refusing to discard queued records; rerun with `khotan-observer clear-queue --yes`"
        );
    }
    let _lock = singleton::acquire()?;
    let count = Spool::open().clear()?;
    println!("cleared {count} queued record(s)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn parse_configure_defaults() {
        let parsed = parse_configure_args(&s(&[])).unwrap();
        assert_eq!(parsed.poll, None);
        assert_eq!(parsed.batch, None);
        assert!(parsed.search_roots.is_empty());
    }

    #[test]
    fn parse_configure_optional_flags() {
        let parsed = parse_configure_args(&s(&[
            "--poll",
            "10",
            "--batch",
            "50",
            "--search-root",
            "/work/customers",
        ]))
        .unwrap();
        assert_eq!(parsed.poll, Some(10));
        assert_eq!(parsed.batch, Some(50));
        assert_eq!(parsed.search_roots, vec![PathBuf::from("/work/customers")]);
    }

    #[test]
    fn parse_configure_rejects_unknown_flag() {
        let err = parse_configure_args(&s(&["--nope"])).unwrap_err();
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

    #[test]
    fn clear_queue_requires_explicit_confirmation() {
        let err = clear_queue(&s(&[])).unwrap_err();
        assert!(err.to_string().contains("--yes"));
    }
}
