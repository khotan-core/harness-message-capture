mod agent;
mod capture;
mod config;
mod destination;
mod docs;
mod log;
mod picker;
mod reader;
mod receiver;
mod record;
mod redact;
mod singleton;
mod sources;
mod spool;
mod store;
mod update;
mod uploader;
mod workspace;

use anyhow::{Context, Result};
use capture::Offsets;
use config::Config;
use notify::{RecursiveMode, Watcher};
use spool::Spool;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
        "run" => watch(&args[2..]),
        "start" => agent::start(),
        "stop" => agent::stop(),
        "uninstall" => agent::uninstall(),
        "logs" => agent::logs(!args.iter().any(|a| a == "--no-follow")),
        "run-once" => run_once(&args[2..]),
        "status" => status(),
        "receive" => receive_cmd(&args[2..]),
        "read" => read_cmd(&args[2..]),
        "clear-queue" => clear_queue(&args[2..]),
        "docs" => docs_cmd(&args[2..]),
        "update" | "install" => update::run(&args[2..]),
        "version" => {
            update::print_version();
            Ok(())
        }
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
           khotan-observer configure    Pick the repositories to observe from a checkbox list\n\
           khotan-observer configure --allow-repo <folder> [...]   Same choice, without a prompt\n\
           khotan-observer run          Capture in the foreground (allowed repos only)\n\
           khotan-observer run --all-logs   Same, and print skip lines for other repos\n\
           khotan-observer start        Install & start the background LaunchAgent\n\
           khotan-observer stop         Stop the background LaunchAgent\n\
           khotan-observer logs         Follow the background log\n\
           khotan-observer update       Replace ~/.local/bin/khotan-observer with the latest GitHub Release\n\
           khotan-observer uninstall    Stop & remove the LaunchAgent\n\
           khotan-observer status       Show config, sources, and spool state\n\
           khotan-observer docs         What status and log lines mean\n\
           khotan-observer version      Print this binary's release tag\n\
           khotan-observer run-once     Single scan + upload pass, then exit\n\
           khotan-observer receive      Local ingest server (writes to an inbox dir)\n\
           khotan-observer read         Inspect inbox messages (--thread, --session, --tool)\n\
           khotan-observer clear-queue --yes  Permanently discard queued records\n"
    );
}

#[derive(Debug, PartialEq, Eq)]
struct ConfigureArgs {
    allow_repos: Option<Vec<String>>,
}

fn parse_configure_args(args: &[String]) -> Result<ConfigureArgs> {
    let mut allow_repos = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--allow-repo" => {
                let value = args
                    .get(i + 1)
                    .context("--allow-repo requires a folder name")?;
                if value.starts_with('-') || value.is_empty() {
                    anyhow::bail!("--allow-repo requires a folder name");
                }
                allow_repos.get_or_insert_with(Vec::new).push(value.clone());
                i += 2;
            }
            "--poll" | "--batch" | "--search-root" => {
                anyhow::bail!(
                    "poll, batch, and search roots are presets; use --allow-repo <folder>"
                )
            }
            other => anyhow::bail!("unknown flag: {other}"),
        }
    }
    Ok(ConfigureArgs { allow_repos })
}

#[derive(Debug, Default, PartialEq, Eq)]
struct RunArgs {
    all_logs: bool,
}

fn parse_run_args(args: &[String]) -> Result<RunArgs> {
    let mut parsed = RunArgs::default();
    for arg in args {
        match arg.as_str() {
            "--all-logs" => parsed.all_logs = true,
            other => anyhow::bail!("unknown flag: {other}"),
        }
    }
    Ok(parsed)
}

fn configure(args: &[String]) -> Result<()> {
    let parsed = parse_configure_args(args)?;
    let mut cfg = Config::load().unwrap_or(Config::fresh(config::random_id()?));
    match parsed.allow_repos {
        Some(allow_repos) => cfg.allow_repos = allow_repos,
        None => {
            if let Some(chosen) = choose_allow_repos(&cfg)? {
                cfg.allow_repos = chosen;
            }
        }
    }
    cfg.endpoint = None;
    cfg.token = None;
    cfg.save()?;
    println!("configured. device_id={}", cfg.device_id);
    println!("config: {}", config::config_path().display());
    match docs::write() {
        Ok(path) => println!("docs: {}", path.display()),
        Err(error) => eprintln!("could not write docs: {error}"),
    }
    if cfg.allow_repos.is_empty() {
        println!("allow_repos: none — nothing is observed until you select a repository");
    } else {
        println!("allow_repos:");
        for name in &cfg.allow_repos {
            println!("  {name}");
        }
    }
    Ok(())
}

/// Open the checkbox picker for a bare `configure`. `Ok(None)` leaves the saved
/// allowlist alone: either the person cancelled, or nothing here can answer.
fn choose_allow_repos(cfg: &Config) -> Result<Option<Vec<String>>> {
    if !picker::is_interactive() {
        eprintln!("no terminal here, so the picker did not open");
        eprintln!("select repositories with: khotan-observer configure --allow-repo <folder>");
        return Ok(None);
    }
    let repos = repos_with_destination(cfg);
    let rows = build_choices(&repos, &cfg.allow_repos);
    if rows.is_empty() {
        eprintln!("no repository under the search roots has a Khotan destination file");
        eprintln!("add env.khotan.local to a repository, then run configure again");
        return Ok(None);
    }
    picker::run(rows)
}

/// A repository that carries a destination file, whether or not that file
/// works. A broken one still earns a row, because a silent omission is what
/// makes a typo hard to find.
struct Found {
    path: PathBuf,
    blocked: Option<String>,
}

/// Every repository with a destination file, nearest first by name. A worktree
/// collapses into its primary checkout, which is what both the selection and
/// the destination already key on.
fn repos_with_destination(cfg: &Config) -> Vec<Found> {
    let index = workspace::WorkspaceIndex::discover(&cfg.search_roots);
    let mut repos: Vec<Found> = Vec::new();
    for candidate in index.candidates() {
        let path = match workspace::primary_repo_for_worktree(candidate) {
            Ok(Some(primary)) => primary,
            _ => candidate.clone(),
        };
        if repos.iter().any(|found| found.path == path) {
            continue;
        }
        match destination::readiness(&path) {
            destination::Readiness::NoFile => {}
            destination::Readiness::Ready => repos.push(Found {
                path,
                blocked: None,
            }),
            destination::Readiness::Blocked(reason) => repos.push(Found {
                path,
                blocked: Some(reason),
            }),
        }
    }
    repos.sort_by_key(|found| leaf(&found.path).to_ascii_lowercase());
    repos
}

/// Rows for the picker. Discovered repositories first, then any repository
/// already selected that matches none of them, still checked, so saving the
/// list never drops an entry the person cannot see.
fn build_choices(repos: &[Found], allow: &[String]) -> Vec<picker::Choice> {
    let paths: Vec<PathBuf> = repos.iter().map(|found| found.path.clone()).collect();
    let mut rows: Vec<picker::Choice> = repos
        .iter()
        .map(|found| {
            let selected = destination::workspace_allowed(&found.path, allow);
            picker::Choice {
                entry: allow_entry(&found.path, &paths, allow),
                label: leaf(&found.path),
                detail: match &found.blocked {
                    Some(reason) => reason.clone(),
                    None => pretty_path(&found.path),
                },
                selected,
                // A repository already selected stays tickable so it can be
                // removed. A broken one nobody chose cannot be turned on.
                disabled: found.blocked.is_some() && !selected,
            }
        })
        .collect();
    for entry in allow {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let claims_a_repo = repos
            .iter()
            .any(|found| destination::workspace_allowed(&found.path, &[entry.to_string()]));
        if claims_a_repo {
            continue;
        }
        rows.push(picker::Choice {
            entry: entry.to_string(),
            label: leaf(Path::new(entry)),
            detail: "selected, but no destination found".to_string(),
            selected: true,
            disabled: false,
        });
    }
    rows
}

/// The string to write for a repository. Two checkouts can share a folder name
/// and a bare name would allow both, so those rows keep the full path. An
/// absolute entry already in the config stays exactly as the person wrote it.
fn allow_entry(repo: &Path, repos: &[PathBuf], allow: &[String]) -> String {
    let existing = allow
        .iter()
        .map(|entry| entry.trim())
        .find(|entry| Path::new(entry).is_absolute() && Path::new(entry) == repo);
    if let Some(entry) = existing {
        return entry.to_string();
    }
    let name = leaf(repo);
    if repos.iter().filter(|other| leaf(other) == name).count() > 1 {
        repo.display().to_string()
    } else {
        name
    }
}

fn leaf(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// `~/Developer/customer` fits a list column better than the absolute path.
fn pretty_path(path: &Path) -> String {
    match path.strip_prefix(config::home()) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
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
    println!("docs      : {}", docs::docs_path().display());
    if cfg.allow_repos.is_empty() {
        println!("allow     : none — run khotan-observer configure to select repositories");
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
fn run_once(args: &[String]) -> Result<()> {
    let parsed = parse_run_args(args)?;
    let _lock = singleton::acquire()?;
    let cfg = Config::load()?;
    let srcs = sources::discover();
    let mut offsets = Offsets::load();
    let spool = Spool::open();
    let pass = scan_and_ship(&cfg, &srcs, &mut offsets, &spool);
    if !report(pass, &cfg.allow_repos, parsed.all_logs) {
        log::idle(offsets.len(), spool.pending());
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

fn watch(args: &[String]) -> Result<()> {
    let parsed = parse_run_args(args)?;
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
        &tools,
        route_count,
        &cfg.allow_repos,
        started.elapsed().as_millis(),
    );
    if srcs.is_empty() {
        log::warn("No Cursor, Claude, or Codex folders");
    }
    update::warn_if_stale();

    // Catch up on anything appended while we were stopped.
    report(
        scan_and_ship(&cfg, &srcs, &mut offsets, &spool),
        &cfg.allow_repos,
        parsed.all_logs,
    );

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
                if report(
                    scan_and_ship(&cfg, &srcs, &mut offsets, &spool),
                    &cfg.allow_repos,
                    parsed.all_logs,
                ) {
                    last_output = std::time::Instant::now();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Fallback pass: covers missed events and retries the spool.
                if report(
                    scan_and_ship(&cfg, &srcs, &mut offsets, &spool),
                    &cfg.allow_repos,
                    parsed.all_logs,
                ) {
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
    lines: Vec<log::Activity>,
}

fn repo_entry<'a>(
    repos: &'a mut BTreeMap<String, log::Activity>,
    label: &str,
) -> &'a mut log::Activity {
    repos
        .entry(label.to_string())
        .or_insert_with(|| log::Activity::new(label))
}

/// Print one line per workspace that did something. Returns whether it printed.
fn report(pass: Pass, allow: &[String], all_logs: bool) -> bool {
    let lines = log::for_display(pass.lines, allow, all_logs);
    if lines.is_empty() {
        return false;
    }
    log::activities(&lines);
    true
}

fn scan_and_ship(
    cfg: &Config,
    srcs: &[sources::Source],
    offsets: &mut Offsets,
    spool: &Spool,
) -> Pass {
    let mut observer: Vec<log::Activity> = Vec::new();
    match spool.quarantine_legacy() {
        Ok(Some(_)) => {
            let mut line = log::Activity::new("queue");
            line.set_means(log::Tone::Warning, "Old pre-route queue was set aside");
            observer.push(line);
        }
        Ok(None) => {}
        Err(_) => {
            let mut line = log::Activity::new("queue");
            line.set_means(log::Tone::Error, "Old pre-route queue could not be moved");
            observer.push(line);
        }
    }

    let workspaces = workspace::WorkspaceIndex::discover(&cfg.search_roots);
    let files = capture::collect_new(srcs, offsets, &workspaces, &cfg.allow_repos);
    let mut repos: BTreeMap<String, log::Activity> = BTreeMap::new();
    let mut offsets_changed = false;
    for file in files {
        match file.route {
            Some(route) => match spool.append(&route, &file.records) {
                Ok(()) => {
                    repo_entry(&mut repos, &route.label).captured += file.records.len();
                    offsets.set(file.offset_key, file.next_offset);
                    offsets_changed = true;
                }
                Err(_) => {
                    repo_entry(&mut repos, &route.label)
                        .set_means(log::Tone::Error, "Disk write to the spool failed");
                }
            },
            None => {
                if let Some(warn) = file.route_warning {
                    let entry = repo_entry(&mut repos, &warn.label);
                    entry.skipped += file.records.len();
                    entry.set_means(log::Tone::Warning, warn.means);
                }
                if file.advance_unrouted {
                    offsets.set(file.offset_key, file.next_offset);
                    offsets_changed = true;
                }
            }
        }
    }
    if offsets_changed {
        if offsets.save().is_err() {
            let mut line = log::Activity::new("observer");
            line.set_means(log::Tone::Error, "Progress file did not write");
            observer.push(line);
        }
    }
    for (label, uploaded) in drain(cfg, spool) {
        let entry = repo_entry(&mut repos, &label);
        if uploaded.count > 0 {
            entry.uploaded += uploaded.count;
        }
        if let Some((tone, means)) = uploaded.means {
            entry.set_means(tone, means);
        }
    }
    for queued in spool.routes() {
        if let Some(entry) = repos.get_mut(&queued.route.label) {
            entry.queued = queued.pending;
        }
    }
    let mut lines: Vec<log::Activity> = repos
        .into_values()
        .filter(|line| !line.is_empty())
        .collect();
    lines = log::fold_skips(lines, log::MAX_SKIP_LINES);
    lines.extend(observer);
    Pass { lines }
}

struct DrainResult {
    count: usize,
    means: Option<(log::Tone, String)>,
}

/// Drain each customer independently so one blocked route cannot stall others.
fn drain(cfg: &Config, spool: &Spool) -> BTreeMap<String, DrainResult> {
    let mut by_label: BTreeMap<String, DrainResult> = BTreeMap::new();
    for queued in spool.routes() {
        if !destination::route_allowed(&queued.route, &cfg.allow_repos) {
            continue;
        }
        let mut count = 0;
        let mut means = None;
        loop {
            let batch = match spool.peek(&queued.route, cfg.batch) {
                Ok(batch) => batch,
                Err(_) => {
                    means = Some((log::Tone::Error, "A queued file is unreadable".to_string()));
                    break;
                }
            };
            if batch.is_empty() {
                break;
            }
            match uploader::send(&cfg.device_id, &queued.route, &batch) {
                uploader::Upload::Ok => {
                    if spool.drop_front(&queued.route, batch.len()).is_err() {
                        means = Some((
                            log::Tone::Error,
                            "Send worked, local delete failed".to_string(),
                        ));
                        break;
                    }
                    count += batch.len();
                }
                uploader::Upload::Retry(reason) => {
                    means = Some((log::Tone::Warning, reason));
                    break;
                }
                uploader::Upload::Blocked(reason) => {
                    means = Some((log::Tone::Error, reason));
                    break;
                }
            }
        }
        if count > 0 || means.is_some() {
            by_label.insert(queued.route.label.clone(), DrainResult { count, means });
        }
    }
    by_label
}

fn docs_cmd(args: &[String]) -> Result<()> {
    match args {
        [] => {
            docs::print();
            Ok(())
        }
        [flag] if flag == "--write" => {
            let path = docs::write()?;
            println!("docs: {}", path.display());
            Ok(())
        }
        _ => anyhow::bail!("usage: khotan-observer docs [--write]"),
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
    thread: Option<String>,
    device: Option<String>,
    limit: usize,
    raw: bool,
}

fn parse_read_args(args: &[String]) -> Result<ReadArgs> {
    let mut dir = None;
    let mut tool = None;
    let mut project = None;
    let mut session = None;
    let mut thread = None;
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
            "--thread" => {
                thread = args.get(i + 1).cloned();
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
        thread,
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
            thread_id: parsed.thread,
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
        assert_eq!(parsed.allow_repos, None);
    }

    #[test]
    fn parse_configure_allow_repos() {
        let parsed = parse_configure_args(&s(&[
            "--allow-repo",
            "podium-automation",
            "--allow-repo",
            "chief-nutrition",
        ]))
        .unwrap();
        assert_eq!(
            parsed.allow_repos,
            Some(vec![
                "podium-automation".to_string(),
                "chief-nutrition".to_string()
            ])
        );
    }

    #[test]
    fn parse_configure_rejects_preset_flags() {
        let err = parse_configure_args(&s(&["--poll", "30"])).unwrap_err();
        assert!(err.to_string().contains("presets"));
    }

    #[test]
    fn parse_configure_rejects_unknown_flag() {
        let err = parse_configure_args(&s(&["--nope"])).unwrap_err();
        assert!(err.to_string().contains("unknown flag"));
    }

    #[test]
    fn parse_run_defaults_to_allowed_repos_only() {
        assert_eq!(
            parse_run_args(&s(&[])).unwrap(),
            RunArgs { all_logs: false }
        );
    }

    #[test]
    fn parse_run_all_logs() {
        assert_eq!(
            parse_run_args(&s(&["--all-logs"])).unwrap(),
            RunArgs { all_logs: true }
        );
    }

    #[test]
    fn parse_run_rejects_unknown_flag() {
        let err = parse_run_args(&s(&["--verbose"])).unwrap_err();
        assert!(err.to_string().contains("unknown flag"));
    }

    fn repos(paths: &[&str]) -> Vec<Found> {
        paths
            .iter()
            .map(|path| Found {
                path: PathBuf::from(path),
                blocked: None,
            })
            .collect()
    }

    fn blocked(path: &str, reason: &str) -> Found {
        Found {
            path: PathBuf::from(path),
            blocked: Some(reason.to_string()),
        }
    }

    #[test]
    fn discovered_repos_start_checked_when_they_are_already_allowed() {
        let found = repos(&["/Users/a/Developer/podium", "/Users/a/Developer/chief"]);
        let rows = build_choices(&found, &["podium".into()]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "podium");
        assert!(rows[0].selected);
        assert!(!rows[1].selected);
        assert_eq!(picker::selected_entries(&rows), vec!["podium"]);
    }

    #[test]
    fn saving_never_drops_an_allowed_repo_the_scan_missed() {
        let found = repos(&["/Users/a/Developer/podium"]);
        let rows = build_choices(&found, &["podium".into(), "retired-repo".into()]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].label, "retired-repo");
        assert!(rows[1].selected);
        assert_eq!(rows[1].detail, "selected, but no destination found");
        assert_eq!(
            picker::selected_entries(&rows),
            vec!["podium", "retired-repo"]
        );
    }

    #[test]
    fn a_shared_folder_name_is_written_as_a_full_path() {
        let found = repos(&["/Users/a/Developer/api", "/Users/a/code/api"]);
        let rows = build_choices(&found, &[]);
        assert_eq!(rows[0].entry, "/Users/a/Developer/api");
        assert_eq!(rows[1].entry, "/Users/a/code/api");
    }

    #[test]
    fn a_unique_folder_name_is_written_as_a_bare_name() {
        let found = repos(&["/Users/a/Developer/podium"]);
        assert_eq!(build_choices(&found, &[])[0].entry, "podium");
    }

    #[test]
    fn an_existing_absolute_entry_keeps_its_exact_text() {
        let found = repos(&["/Users/a/Developer/podium"]);
        let rows = build_choices(&found, &["/Users/a/Developer/podium".into()]);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].selected);
        assert_eq!(rows[0].entry, "/Users/a/Developer/podium");
    }

    #[test]
    fn a_broken_destination_file_shows_its_reason_instead_of_vanishing() {
        let mut found = repos(&["/Users/a/Developer/podium"]);
        found.push(blocked("/Users/a/Developer/typo", "missing KHOTAN_ORG_ID"));
        let rows = build_choices(&found, &[]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].label, "typo");
        assert_eq!(rows[1].detail, "missing KHOTAN_ORG_ID");
        assert!(rows[1].disabled);
        assert!(!rows[1].selected);
    }

    #[test]
    fn a_selected_repo_stays_tickable_after_its_destination_breaks() {
        let found = vec![blocked(
            "/Users/a/Developer/podium",
            "missing KHOTAN_API_KEY",
        )];
        let rows = build_choices(&found, &["podium".into()]);
        assert!(rows[0].selected);
        assert!(!rows[0].disabled);
        assert_eq!(rows[0].detail, "missing KHOTAN_API_KEY");
        assert_eq!(picker::selected_entries(&rows), vec!["podium"]);
    }

    #[test]
    fn blank_allowlist_entries_do_not_become_rows() {
        let rows = build_choices(&repos(&["/Users/a/Developer/podium"]), &["".into()]);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].selected);
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
            "--thread",
            "thr",
            "--limit",
            "10",
            "--raw",
        ]))
        .unwrap();
        assert_eq!(parsed.tool.as_deref(), Some("cursor"));
        assert_eq!(parsed.session.as_deref(), Some("abc"));
        assert_eq!(parsed.thread.as_deref(), Some("thr"));
        assert_eq!(parsed.limit, 10);
        assert!(parsed.raw);
    }

    #[test]
    fn clear_queue_requires_explicit_confirmation() {
        let err = clear_queue(&s(&[])).unwrap_err();
        assert!(err.to_string().contains("--yes"));
    }
}
