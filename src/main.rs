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
           khotan-observer configure --allow-repo <folder> [...]   Replace the list, without a prompt\n\
           khotan-observer configure --add-repo <folder> [...]     Add to the list, keeping the rest\n\
           khotan-observer configure --remove-repo <folder> [...]  Drop from the list, keeping the rest\n\
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

#[derive(Debug, Default, PartialEq, Eq)]
struct ConfigureArgs {
    /// `--allow-repo`: replace the whole list, as it has always done.
    allow_repos: Option<Vec<String>>,
    /// `--add-repo`: merge these into the stored list.
    add_repos: Vec<String>,
    /// `--remove-repo`: drop these from the stored list.
    remove_repos: Vec<String>,
}

fn parse_configure_args(args: &[String]) -> Result<ConfigureArgs> {
    let mut parsed = ConfigureArgs::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--allow-repo" => {
                let value = value_after(args, i, "--allow-repo")?;
                parsed.allow_repos.get_or_insert_with(Vec::new).push(value);
                i += 2;
            }
            "--add-repo" => {
                parsed.add_repos.push(value_after(args, i, "--add-repo")?);
                i += 2;
            }
            "--remove-repo" => {
                parsed
                    .remove_repos
                    .push(value_after(args, i, "--remove-repo")?);
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
    if parsed.allow_repos.is_some()
        && !(parsed.add_repos.is_empty() && parsed.remove_repos.is_empty())
    {
        anyhow::bail!(
            "--allow-repo replaces the list; use --add-repo/--remove-repo to adjust it, not both"
        );
    }
    Ok(parsed)
}

/// The folder name after a repo flag, rejecting a missing value or another flag.
fn value_after(args: &[String], i: usize, flag: &str) -> Result<String> {
    let value = args
        .get(i + 1)
        .with_context(|| format!("{flag} requires a folder name"))?;
    if value.starts_with('-') || value.is_empty() {
        anyhow::bail!("{flag} requires a folder name");
    }
    Ok(value.clone())
}

/// The allow list a non-interactive `configure` should store: the replacement
/// as given, or the stored list with additions and removals folded in. `None`
/// means no list flags were passed, so the checkbox picker decides.
fn next_allow_repos(existing: &[String], parsed: &ConfigureArgs) -> Option<Vec<String>> {
    if let Some(replacement) = &parsed.allow_repos {
        return Some(replacement.clone());
    }
    if !parsed.add_repos.is_empty() || !parsed.remove_repos.is_empty() {
        return Some(merge_allow(
            existing,
            &parsed.add_repos,
            &parsed.remove_repos,
        ));
    }
    None
}

/// Fold `--add-repo`/`--remove-repo` into the stored list. Removals win over
/// the current list, then additions land unless already present. An add of an
/// entry already there, or a remove of one that is absent, changes nothing.
fn merge_allow(existing: &[String], add: &[String], remove: &[String]) -> Vec<String> {
    let norm = |value: &str| value.trim().to_ascii_lowercase();
    let dropped: std::collections::BTreeSet<String> = remove.iter().map(|e| norm(e)).collect();
    let mut result: Vec<String> = existing
        .iter()
        .filter(|entry| !entry.trim().is_empty())
        .filter(|entry| !dropped.contains(&norm(entry)))
        .cloned()
        .collect();
    for entry in add {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !result.iter().any(|kept| norm(kept) == norm(trimmed)) {
            result.push(trimmed.to_string());
        }
    }
    result
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
    match next_allow_repos(&cfg.allow_repos, &parsed) {
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
    // A destination file that cannot upload is worth naming: without this a
    // half-filled file looks the same as a repository that was never set up.
    let found = repos_with_destination(&cfg);
    let blocked: Vec<&Found> = found.iter().filter(|repo| repo.blocked.is_some()).collect();
    if blocked.is_empty() {
        println!("blocked repos : none — every destination file is usable");
    } else {
        println!("blocked repos :");
        for repo in blocked {
            let reason = repo.blocked.as_deref().unwrap_or("blocked");
            println!("  {} ({reason})", leaf(&repo.path));
        }
    }
    // An allow-list entry that names no repository with a destination file is a
    // typo or a checkout that moved, and would otherwise upload nothing quietly.
    let orphans: Vec<&str> = cfg
        .allow_repos
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
        .filter(|entry| {
            !found
                .iter()
                .any(|repo| destination::workspace_allowed(&repo.path, &[entry.to_string()]))
        })
        .collect();
    if !orphans.is_empty() {
        println!("allowed but no destination found:");
        for entry in orphans {
            println!("  {entry}");
        }
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
    let pass = scan_and_ship(&cfg, &srcs, &mut offsets, &spool, parsed.all_logs);
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
        scan_and_ship(&cfg, &srcs, &mut offsets, &spool, parsed.all_logs),
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
                    scan_and_ship(&cfg, &srcs, &mut offsets, &spool, parsed.all_logs),
                    &cfg.allow_repos,
                    parsed.all_logs,
                ) {
                    last_output = std::time::Instant::now();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Fallback pass: covers missed events and retries the spool.
                if report(
                    scan_and_ship(&cfg, &srcs, &mut offsets, &spool, parsed.all_logs),
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

fn activity_key(label: &str, tool: Option<&str>) -> (String, Option<String>) {
    (label.to_string(), tool.map(|tool| tool.to_string()))
}

fn repo_entry<'a>(
    repos: &'a mut BTreeMap<(String, Option<String>), log::Activity>,
    label: &str,
    tool: Option<&str>,
) -> &'a mut log::Activity {
    repos
        .entry(activity_key(label, tool))
        .or_insert_with(|| match tool {
            Some(tool) => log::Activity::with_tool(label, tool),
            None => log::Activity::new(label),
        })
}

/// Put leftover queue counts and delivery failures on the sole activity
/// line for this repo. Split them out when more than one source printed.
fn attach_repo_status(
    repos: &mut BTreeMap<(String, Option<String>), log::Activity>,
    label: &str,
    queued: usize,
    means: Option<(log::Tone, String)>,
) {
    if queued == 0 && means.is_none() {
        return;
    }
    let keys: Vec<_> = repos
        .keys()
        .filter(|(name, _)| name == label)
        .cloned()
        .collect();
    let entry = match keys.as_slice() {
        [key] => repos.get_mut(key).expect("key came from this map"),
        _ => repo_entry(repos, label, None),
    };
    if queued > 0 {
        entry.queued = queued;
    }
    if let Some((tone, means)) = means {
        entry.set_means(tone, means);
    }
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
    all_logs: bool,
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
    let mut repos: BTreeMap<(String, Option<String>), log::Activity> = BTreeMap::new();
    let mut offsets_changed = false;
    for file in files {
        match file.route {
            Some(route) => match spool.append(&route, &file.records) {
                Ok(()) => {
                    repo_entry(&mut repos, &route.label, Some(file.tool)).captured +=
                        file.records.len();
                    offsets.set(file.offset_key, file.next_offset);
                    offsets_changed = true;
                }
                Err(_) => {
                    repo_entry(&mut repos, &route.label, Some(file.tool))
                        .set_means(log::Tone::Error, "Disk write to the spool failed");
                }
            },
            None => {
                if let Some(warn) = file.route_warning {
                    let entry = repo_entry(&mut repos, &warn.label, Some(&warn.tool));
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
    let drained = drain(cfg, spool, all_logs);
    for (label, uploaded) in &drained {
        for (tool, count) in &uploaded.by_tool {
            if *count > 0 {
                repo_entry(&mut repos, label, Some(tool)).uploaded += count;
            }
        }
    }
    let mut status: BTreeMap<String, (usize, Option<(log::Tone, String)>)> = BTreeMap::new();
    for (label, uploaded) in drained {
        if let Some(means) = uploaded.means {
            status.entry(label).or_insert((0, None)).1 = Some(means);
        }
    }
    for queued in spool.routes() {
        if queued.pending == 0 {
            continue;
        }
        let has_line = repos.keys().any(|(name, _)| name == &queued.route.label)
            || status.contains_key(&queued.route.label);
        if has_line {
            status
                .entry(queued.route.label.clone())
                .or_insert((0, None))
                .0 = queued.pending;
        }
    }
    for (label, (queued, means)) in status {
        attach_repo_status(&mut repos, &label, queued, means);
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
    by_tool: BTreeMap<String, usize>,
    means: Option<(log::Tone, String)>,
}

/// Bytes of JSON one request may carry. An ingest endpoint commonly refuses a
/// body over 1 MiB, and 400 captured lines average just past that edge. The
/// byte ceiling, not the record count, is what a batch has to respect.
const PAYLOAD_BUDGET: usize = 900 * 1024;

/// Smallest budget a size refusal may drive a route down to. Below this it is
/// the front record, not the batch, that the server will not take.
const MIN_PAYLOAD_BUDGET: usize = 32 * 1024;

/// How long one drain may run before the pass returns to capturing. Long enough
/// for every route to take many turns, short enough that new lines on disk do
/// not sit unread.
const DRAIN_BUDGET: Duration = Duration::from_secs(120);

/// One customer's place in the round robin.
struct Lane {
    route: destination::RouteRef,
    /// Bytes this route may put in one request. Halves on a size refusal.
    budget: usize,
    delivered: BTreeMap<String, usize>,
    means: Option<(log::Tone, String)>,
}

/// What one batch did, so the round robin knows what the lane needs next.
enum Turn {
    Sent(BTreeMap<String, usize>),
    Empty,
    /// The server refused the size. Halve the budget and take another turn.
    Smaller,
    /// One record no batch size can carry. Parked, so the queue can move.
    Parked(String),
    Stop(log::Tone, String),
}

/// Give every customer a turn every cycle until the queues empty or the pass
/// budget runs out. Draining one route to the bottom before starting the next
/// starved every customer whose label sorted later.
fn drain(cfg: &Config, spool: &Spool, all_logs: bool) -> BTreeMap<String, DrainResult> {
    let deadline = std::time::Instant::now() + DRAIN_BUDGET;
    // Live destinations on the machine, used to re-point a queue whose pinned
    // file died at a sibling checkout carrying the same key.
    let workspaces = workspace::WorkspaceIndex::discover(&cfg.search_roots);
    let candidates = destination::discover_routes(workspaces.candidates(), &cfg.allow_repos);
    let mut lanes: Vec<Lane> = spool
        .routes()
        .into_iter()
        .filter(|queued| destination::route_allowed(&queued.route, &cfg.allow_repos))
        .map(|queued| Lane {
            route: queued.route,
            budget: PAYLOAD_BUDGET,
            delivered: BTreeMap::new(),
            means: None,
        })
        .collect();

    let mut done: Vec<Lane> = Vec::new();
    let mut cycle = 0usize;
    while !lanes.is_empty() && std::time::Instant::now() < deadline {
        // One batch per route, all routes at once.
        let candidates = &candidates;
        let turns: Vec<Turn> = std::thread::scope(|scope| {
            let handles: Vec<_> = lanes
                .iter()
                .map(|lane| scope.spawn(move || one_batch(cfg, spool, candidates, lane)))
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle.join().unwrap_or(Turn::Stop(
                        log::Tone::Error,
                        "An upload thread died".to_string(),
                    ))
                })
                .collect()
        });

        let mut progress: Vec<log::Activity> = Vec::new();
        let mut active: Vec<Lane> = Vec::new();
        for (mut lane, turn) in lanes.into_iter().zip(turns) {
            let mut keep = true;
            match turn {
                Turn::Sent(by_tool) => {
                    let sent: usize = by_tool.values().sum();
                    for (tool, count) in by_tool {
                        *lane.delivered.entry(tool).or_default() += count;
                    }
                    if cycle > 0 {
                        // The first cycle already reads as the end-of-pass line.
                        // A drain that runs for minutes should not stay silent
                        // until it is over.
                        progress.push(progress_line(&lane, sent, spool.pending_for(&lane.route)));
                    }
                }
                Turn::Empty => keep = false,
                Turn::Smaller => lane.budget = (lane.budget / 2).max(MIN_PAYLOAD_BUDGET),
                Turn::Parked(reason) => {
                    lane.budget = PAYLOAD_BUDGET;
                    lane.means = Some((log::Tone::Warning, reason));
                }
                Turn::Stop(tone, reason) => {
                    lane.means = Some((tone, reason));
                    keep = false;
                }
            }
            if keep {
                active.push(lane);
            } else {
                done.push(lane);
            }
        }
        if !progress.is_empty() {
            log::activities(&log::for_display(progress, &cfg.allow_repos, all_logs));
        }
        lanes = active;
        cycle += 1;
    }
    done.extend(lanes);

    done.into_iter()
        .filter(|lane| !lane.delivered.is_empty() || lane.means.is_some())
        .map(|lane| {
            (
                lane.route.label.clone(),
                DrainResult {
                    by_tool: lane.delivered,
                    means: lane.means,
                },
            )
        })
        .collect()
}

/// The key for a queue, and the path to record if the pinned file was replaced.
/// The pinned file is tried first; only when it stops producing credentials does
/// a discovered destination with the same identity stand in for it.
fn resolve_credentials(
    route: &destination::RouteRef,
    candidates: &[destination::RouteRef],
) -> Result<(String, Option<PathBuf>), String> {
    if let Ok(credentials) = destination::read_credentials(route) {
        return Ok((credentials.api_key, None));
    }
    for candidate in candidates {
        if destination::same_identity(candidate, route) {
            if let Ok(credentials) = destination::read_credentials(candidate) {
                return Ok((credentials.api_key, Some(candidate.credential_path.clone())));
            }
        }
    }
    Err("Dest file gone and no repo with the same key was found".to_string())
}

/// One route's turn: send the front of its queue once.
fn one_batch(
    cfg: &Config,
    spool: &Spool,
    candidates: &[destination::RouteRef],
    lane: &Lane,
) -> Turn {
    // Once the budget is at its floor, send one record per request. The next
    // refusal then names the record itself rather than the batch around it,
    // which is the only way a route with an unsendable line keeps moving.
    let max_records = if lane.budget <= MIN_PAYLOAD_BUDGET {
        1
    } else {
        cfg.batch
    };
    let batch = match spool.peek_batch(&lane.route, max_records, lane.budget) {
        Ok(batch) => batch,
        Err(_) => return Turn::Stop(log::Tone::Error, "A queued file is unreadable".to_string()),
    };
    if batch.is_empty() {
        return Turn::Empty;
    }

    // The key that reaches this queue. If the file it was pinned to no longer
    // produces one, deliver through a sibling checkout that carries the same
    // identity rather than stranding the queue behind a repurposed file.
    let (api_key, repointed) = match resolve_credentials(&lane.route, candidates) {
        Ok(pair) => pair,
        Err(reason) => return Turn::Stop(log::Tone::Error, reason),
    };
    if let Some(path) = &repointed {
        let _ = spool.repoint(&lane.route, path);
    }

    // Establish the organization from the key, enforcing any already bound to
    // the queue, and pin it the first time a queue that carried none resolves.
    let org = match uploader::resolve_org(&lane.route, &api_key) {
        uploader::OrgOutcome::Resolved(org) => org,
        uploader::OrgOutcome::Retry(reason) => return Turn::Stop(log::Tone::Warning, reason),
        uploader::OrgOutcome::Blocked(reason) => return Turn::Stop(log::Tone::Error, reason),
    };
    if lane.route.org_id.is_none() {
        let _ = spool.pin_org(&lane.route, &org);
    }

    match uploader::post_batch(&cfg.device_id, &lane.route, &api_key, &org, &batch) {
        uploader::Upload::Ok => {
            if spool.drop_front(&lane.route, batch.len()).is_err() {
                return Turn::Stop(
                    log::Tone::Error,
                    "Send worked, local delete failed".to_string(),
                );
            }
            let mut by_tool: BTreeMap<String, usize> = BTreeMap::new();
            for record in &batch {
                *by_tool.entry(record.tool.clone()).or_default() += 1;
            }
            Turn::Sent(by_tool)
        }
        uploader::Upload::TooLarge(reason) => {
            if batch.len() > 1 {
                return Turn::Smaller;
            }
            // A single record the server refuses at any size. Park it, or the
            // whole customer queue stops behind one line.
            match spool.quarantine_front(&lane.route) {
                Ok(()) => Turn::Parked(format!("One record was too big to send · {reason}")),
                Err(_) => Turn::Stop(log::Tone::Error, reason),
            }
        }
        uploader::Upload::Retry(reason) => Turn::Stop(log::Tone::Warning, reason),
        uploader::Upload::Blocked(reason) => Turn::Stop(log::Tone::Error, reason),
    }
}

/// What one route has delivered so far, printed while the drain is still going.
fn progress_line(lane: &Lane, sent: usize, queued: usize) -> log::Activity {
    let mut line = log::Activity::new(lane.route.label.as_str());
    line.uploaded = sent;
    line.queued = queued;
    line
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
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    fn stamp() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    /// A customer endpoint that answers the identity check once, then accepts
    /// `ingests` batches, writing the label of every batch it takes into the
    /// shared arrival order.
    fn spawn_customer(
        label: &'static str,
        org_id: String,
        ingests: usize,
        order: Arc<Mutex<Vec<&'static str>>>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            for _ in 0..ingests + 1 {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut data = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let Ok(count) = stream.read(&mut chunk) else {
                        return;
                    };
                    if count == 0 {
                        break;
                    }
                    data.extend_from_slice(&chunk[..count]);
                    if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&data[..end + 4]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if data.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8_lossy(&data).to_string();
                if request.starts_with("POST /ingest") {
                    order.lock().unwrap().push(label);
                    let _ = write!(
                        stream,
                        "HTTP/1.1 204 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                } else {
                    let body = format!("{{\"organizationId\":\"{org_id}\"}}");
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                }
            }
        });
        (origin, handle)
    }

    fn customer_route(root: &Path, label: &'static str, origin: &str) -> destination::RouteRef {
        let repo = root.join(label);
        std::fs::create_dir_all(&repo).unwrap();
        let env_path = repo.join("env.khotan.local");
        std::fs::write(
            &env_path,
            format!(
                "KHOTAN_API_URL='{origin}'\nKHOTAN_API_KEY='test-secret-key'\nKHOTAN_ORG_ID='org-{label}'\n"
            ),
        )
        .unwrap();
        destination::RouteRef {
            id: format!("route-{label}"),
            org_id: Some(format!("org-{label}")),
            api_url: origin.to_string(),
            key_fingerprint: Some(destination::key_fingerprint("test-secret-key")),
            credential_path: env_path,
            label: label.to_string(),
        }
    }

    fn queued(line: &str) -> record::Record {
        record::Record {
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

    /// The bug this replaced: routes were drained one at a time in label order,
    /// so a customer that never emptied kept every later customer at zero.
    #[test]
    fn every_route_gets_a_turn_before_the_first_one_finishes() {
        let root = std::env::temp_dir().join(format!("hmc-drain-{}", stamp()));
        std::fs::create_dir_all(&root).unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let (first_origin, first_server) = spawn_customer(
            "aaa-sorts-first",
            "org-aaa-sorts-first".into(),
            3,
            Arc::clone(&order),
        );
        let (last_origin, last_server) = spawn_customer(
            "zzz-sorts-last",
            "org-zzz-sorts-last".into(),
            1,
            Arc::clone(&order),
        );
        let first = customer_route(&root, "aaa-sorts-first", &first_origin);
        let last = customer_route(&root, "zzz-sorts-last", &last_origin);

        let spool = Spool::at(root.join("state"));
        spool
            .append(
                &first,
                &[
                    queued("a"),
                    queued("b"),
                    queued("c"),
                    queued("d"),
                    queued("e"),
                    queued("f"),
                ],
            )
            .unwrap();
        spool.append(&last, &[queued("z"), queued("y")]).unwrap();

        let cfg = Config {
            endpoint: None,
            token: None,
            device_id: "device".into(),
            poll_secs: 45,
            batch: 2,
            search_roots: vec![root.clone()],
            allow_repos: vec!["aaa-sorts-first".into(), "zzz-sorts-last".into()],
        };
        let drained = drain(&cfg, &spool, false);

        first_server.join().unwrap();
        last_server.join().unwrap();
        let order = order.lock().unwrap().clone();
        let last_batch_of_first = order
            .iter()
            .rposition(|label| *label == "aaa-sorts-first")
            .unwrap();
        let only_batch_of_last = order
            .iter()
            .position(|label| *label == "zzz-sorts-last")
            .unwrap();
        assert!(
            only_batch_of_last < last_batch_of_first,
            "the later label waited for the earlier one to empty: {order:?}"
        );
        assert_eq!(drained["aaa-sorts-first"].by_tool["cursor"], 6);
        assert_eq!(drained["zzz-sorts-last"].by_tool["cursor"], 2);
        assert_eq!(spool.pending(), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    fn read_meta(state: &Path, id: &str) -> serde_json::Value {
        let raw = std::fs::read_to_string(state.join("spool").join(id).join("route.json")).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    #[test]
    fn a_route_with_no_declared_org_pins_the_one_the_key_resolves_to() {
        let root = std::env::temp_dir().join(format!("hmc-pin-{}", stamp()));
        std::fs::create_dir_all(&root).unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let (origin, server) = spawn_customer("pin-me", "org-resolved".into(), 1, order);
        let mut route = customer_route(&root, "pin-me", &origin);
        // The destination declared no organization when the queue was created.
        route.org_id = None;

        let state = root.join("state");
        let spool = Spool::at(state.clone());
        spool.append(&route, &[queued("a")]).unwrap();

        let cfg = Config {
            endpoint: None,
            token: None,
            device_id: "device".into(),
            poll_secs: 45,
            batch: 200,
            search_roots: vec![root.clone()],
            allow_repos: vec!["pin-me".into()],
        };
        let drained = drain(&cfg, &spool, false);

        server.join().unwrap();
        assert_eq!(drained["pin-me"].by_tool["cursor"], 1);
        assert_eq!(spool.pending(), 0);
        // The endpoint's organization was pinned into the queue's metadata.
        let meta = read_meta(&state, "route-pin-me");
        assert_eq!(meta["route"]["org_id"].as_str(), Some("org-resolved"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_declared_org_that_the_key_contradicts_blocks_and_keeps_the_records() {
        let root = std::env::temp_dir().join(format!("hmc-declared-{}", stamp()));
        std::fs::create_dir_all(&root).unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        // customer_route declares org-declared-mismatch; the key resolves to another.
        let (origin, server) = spawn_customer("declared-mismatch", "org-other".into(), 0, order);
        let route = customer_route(&root, "declared-mismatch", &origin);

        let state = root.join("state");
        let spool = Spool::at(state.clone());
        spool.append(&route, &[queued("a")]).unwrap();

        let cfg = Config {
            endpoint: None,
            token: None,
            device_id: "device".into(),
            poll_secs: 45,
            batch: 200,
            search_roots: vec![root.clone()],
            allow_repos: vec!["declared-mismatch".into()],
        };
        let drained = drain(&cfg, &spool, false);

        server.join().unwrap();
        assert!(drained["declared-mismatch"].means.is_some());
        assert!(drained["declared-mismatch"].by_tool.is_empty());
        assert_eq!(spool.pending(), 1, "the records stay queued");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_key_swapped_after_pinning_blocks_rather_than_delivering_to_the_new_org() {
        let root = std::env::temp_dir().join(format!("hmc-pinned-{}", stamp()));
        std::fs::create_dir_all(&root).unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let (origin, server) = spawn_customer("pinned", "org-endpoint".into(), 0, order);
        let mut route = customer_route(&root, "pinned", &origin);
        route.org_id = None;

        let state = root.join("state");
        let spool = Spool::at(state.clone());
        spool.append(&route, &[queued("a")]).unwrap();
        // An earlier pass pinned a different organization to this queue.
        spool.pin_org(&route, "org-was-pinned").unwrap();

        let cfg = Config {
            endpoint: None,
            token: None,
            device_id: "device".into(),
            poll_secs: 45,
            batch: 200,
            search_roots: vec![root.clone()],
            allow_repos: vec!["pinned".into()],
        };
        let drained = drain(&cfg, &spool, false);

        server.join().unwrap();
        assert!(drained["pinned"].means.is_some());
        assert_eq!(spool.pending(), 1, "records stay behind the mismatch");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_repurposed_file_delivers_through_a_sibling_that_holds_the_same_key() {
        let root = std::env::temp_dir().join(format!("hmc-repoint-{}", stamp()));
        std::fs::create_dir_all(&root).unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let (origin, server) = spawn_customer("acme", "org-acme".into(), 1, Arc::clone(&order));

        // The file the queue was pinned to, now repurposed: no Khotan keys left.
        let dead_repo = root.join("acme-dead");
        std::fs::create_dir_all(dead_repo.join(".git")).unwrap();
        let dead_env = dead_repo.join("env.khotan.local");
        std::fs::write(&dead_env, "SOME_OTHER_TOOL=1\n").unwrap();

        // A sibling checkout that still carries the same origin and key.
        let live_repo = root.join("acme-live");
        std::fs::create_dir_all(live_repo.join(".git")).unwrap();
        let live_env = live_repo.join("env.khotan.local");
        std::fs::write(
            &live_env,
            format!(
                "KHOTAN_API_URL='{origin}'\nKHOTAN_API_KEY='shared-key'\nKHOTAN_ORG_ID='org-acme'\n"
            ),
        )
        .unwrap();

        // A queue pinned to the dead file, identified by the shared key.
        let queue_route = destination::RouteRef {
            id: "acme-queue".into(),
            org_id: Some("org-acme".into()),
            api_url: origin.clone(),
            key_fingerprint: Some(destination::key_fingerprint("shared-key")),
            credential_path: dead_env.clone(),
            label: "acme-dead".into(),
        };
        let state = root.join("state");
        let spool = Spool::at(state.clone());
        spool.append(&queue_route, &[queued("stranded")]).unwrap();

        let cfg = Config {
            endpoint: None,
            token: None,
            device_id: "device".into(),
            poll_secs: 45,
            batch: 200,
            search_roots: vec![root.clone()],
            allow_repos: vec!["acme-dead".into(), "acme-live".into()],
        };
        let drained = drain(&cfg, &spool, false);

        server.join().unwrap();
        assert_eq!(drained["acme-dead"].by_tool["cursor"], 1);
        assert_eq!(spool.pending(), 0, "the stranded record was delivered");
        // The queue now points at the live sibling's file.
        let meta = read_meta(&state, "acme-queue");
        assert_eq!(meta["route"]["credential_path"].as_str(), live_env.to_str());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_queue_with_no_working_file_reports_a_reason_and_keeps_its_records() {
        let dead = destination::RouteRef {
            id: "q".into(),
            org_id: Some("org".into()),
            api_url: "https://acme.example".into(),
            key_fingerprint: Some(destination::key_fingerprint("shared-key")),
            credential_path: PathBuf::from("/nonexistent/dead.env"),
            label: "acme".into(),
        };
        // A candidate at the same origin, but a different key: not a match.
        let stranger = destination::RouteRef {
            id: "other".into(),
            org_id: Some("org".into()),
            api_url: "https://acme.example".into(),
            key_fingerprint: Some(destination::key_fingerprint("different-key")),
            credential_path: PathBuf::from("/nonexistent/other.env"),
            label: "other".into(),
        };
        let outcome = resolve_credentials(&dead, &[stranger]);
        assert!(outcome.is_err(), "nothing matches, so the queue is blocked");
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
    fn parse_configure_add_and_remove() {
        let parsed = parse_configure_args(&s(&[
            "--add-repo",
            "alpha",
            "--remove-repo",
            "beta",
            "--add-repo",
            "gamma",
        ]))
        .unwrap();
        assert_eq!(parsed.allow_repos, None);
        assert_eq!(parsed.add_repos, vec!["alpha", "gamma"]);
        assert_eq!(parsed.remove_repos, vec!["beta"]);
    }

    #[test]
    fn parse_configure_rejects_replace_mixed_with_adjust() {
        let err =
            parse_configure_args(&s(&["--allow-repo", "alpha", "--add-repo", "beta"])).unwrap_err();
        assert!(err.to_string().contains("--allow-repo"), "{err}");
    }

    #[test]
    fn adjusting_the_list_adds_removes_and_no_ops_without_disturbing_the_rest() {
        let existing = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        // Add one to a list of three: four entries, the original three intact.
        let added = next_allow_repos(
            &existing,
            &ConfigureArgs {
                add_repos: vec!["four".into()],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(added, vec!["one", "two", "three", "four"]);
        // Remove one: only that entry is gone.
        let removed = next_allow_repos(
            &existing,
            &ConfigureArgs {
                remove_repos: vec!["two".into()],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(removed, vec!["one", "three"]);
        // Both in one command.
        let both = next_allow_repos(
            &existing,
            &ConfigureArgs {
                add_repos: vec!["four".into()],
                remove_repos: vec!["one".into()],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(both, vec!["two", "three", "four"]);
        // A duplicate add and an absent remove each change nothing.
        assert_eq!(merge_allow(&existing, &["TWO".into()], &[]), existing);
        assert_eq!(merge_allow(&existing, &[], &["absent".into()]), existing);
    }

    #[test]
    fn the_replace_form_still_replaces() {
        let existing = vec!["one".to_string(), "two".to_string()];
        let replaced = next_allow_repos(
            &existing,
            &ConfigureArgs {
                allow_repos: Some(vec!["only".into()]),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(replaced, vec!["only"]);
        // No list flags: the picker decides, so nothing is returned here.
        assert_eq!(next_allow_repos(&existing, &ConfigureArgs::default()), None);
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

    #[test]
    fn attach_status_joins_a_single_source_line() {
        let mut repos = BTreeMap::new();
        repo_entry(&mut repos, "dev-serve-robotics", Some("cursor")).captured = 5;
        attach_repo_status(
            &mut repos,
            "dev-serve-robotics",
            3,
            Some((
                log::Tone::Warning,
                "Host is up in DNS, port is closed".into(),
            )),
        );
        assert_eq!(repos.len(), 1);
        let line = repos.values().next().unwrap();
        assert_eq!(line.tool.as_deref(), Some("cursor"));
        assert_eq!(line.queued, 3);
        assert_eq!(
            line.means.as_deref(),
            Some("Host is up in DNS, port is closed")
        );
    }

    #[test]
    fn attach_status_splits_when_two_sources_print() {
        let mut repos = BTreeMap::new();
        repo_entry(&mut repos, "dev-serve-robotics", Some("cursor")).captured = 5;
        repo_entry(&mut repos, "dev-serve-robotics", Some("claude")).captured = 2;
        attach_repo_status(
            &mut repos,
            "dev-serve-robotics",
            10,
            Some((
                log::Tone::Warning,
                "Host is up in DNS, port is closed".into(),
            )),
        );
        assert_eq!(repos.len(), 3);
        let shared = repos
            .get(&activity_key("dev-serve-robotics", None))
            .unwrap();
        assert!(shared.tool.is_none());
        assert_eq!(shared.queued, 10);
        assert_eq!(
            shared.means.as_deref(),
            Some("Host is up in DNS, port is closed")
        );
    }
}
