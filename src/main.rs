mod agent;
mod capture;
mod config;
mod record;
mod redact;
mod sources;
mod spool;
mod uploader;

use anyhow::{Context, Result};
use capture::Offsets;
use config::Config;
use notify::{RecursiveMode, Watcher};
use spool::Spool;
use std::sync::mpsc;
use std::time::Duration;

fn main() {
    if let Err(e) = run() {
        eprintln!("hmc: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("help");
    match cmd {
        "enroll" => enroll(&args[2..]),
        "install" => agent::install(),
        "uninstall" => agent::uninstall(),
        "watch" => watch(),
        "run-once" => run_once(),
        "status" => status(),
        _ => {
            print_help();
            Ok(())
        }
    }
}

fn print_help() {
    eprintln!(
        "harness-message-capture (hmc)\n\
         \n\
         USAGE:\n\
           hmc enroll --endpoint <url> --token <tok> [--poll <secs>] [--batch <n>]\n\
           hmc install       Install & start the background LaunchAgent\n\
           hmc uninstall     Stop & remove the LaunchAgent\n\
           hmc watch         Run the capture daemon in the foreground\n\
           hmc run-once      Do a single scan + upload pass, then exit\n\
           hmc status        Show config, sources, and spool state\n"
    );
}

fn enroll(args: &[String]) -> Result<()> {
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
    let endpoint = endpoint.context("--endpoint is required")?;
    let token = token.context("--token is required")?;

    // Preserve an existing device_id across re-enrollment.
    let device_id = Config::load()
        .ok()
        .map(|c| c.device_id)
        .unwrap_or(config::random_id()?);

    let cfg = Config {
        endpoint,
        token,
        device_id,
        poll_secs: poll.unwrap_or(45),
        batch: batch.unwrap_or(200),
    };
    cfg.save()?;
    println!("enrolled. device_id={}", cfg.device_id);
    println!("config: {}", config::config_path().display());
    println!("next: `hmc install` to start capturing in the background");
    Ok(())
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
    println!("sources  :");
    for s in sources::discover() {
        println!("  [{}] {}", s.tool, s.root.display());
    }
    let offsets = Offsets::load();
    println!("tracked files: {}", offsets.len());
    println!("spool pending: {}", Spool::open().pending());
    Ok(())
}

/// Single pass: capture new lines, spool them, drain the spool. Handy for tests.
fn run_once() -> Result<()> {
    let cfg = Config::load()?;
    let srcs = sources::discover();
    let mut offsets = Offsets::load();
    let spool = Spool::open();
    scan_and_ship(&cfg, &srcs, &mut offsets, &spool);
    Ok(())
}

fn watch() -> Result<()> {
    let cfg = Config::load()?;
    let srcs = sources::discover();
    let mut offsets = Offsets::load();
    let spool = Spool::open();

    let (tx, rx) = mpsc::channel();
    let mut watcher =
        notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })
        .context("create fs watcher")?;

    for s in &srcs {
        // Best-effort: a missing/again-permissioned dir shouldn't kill the daemon.
        let _ = watcher.watch(&s.root, RecursiveMode::Recursive);
    }
    eprintln!(
        "hmc watch: {} source(s), poll every {}s",
        srcs.len(),
        cfg.poll_secs
    );

    // Catch up on anything appended while we were stopped.
    scan_and_ship(&cfg, &srcs, &mut offsets, &spool);

    loop {
        match rx.recv_timeout(Duration::from_secs(cfg.poll_secs)) {
            Ok(_evt) => {
                // Debounce a burst of writes, then coalesce into one scan.
                std::thread::sleep(Duration::from_millis(300));
                while rx.try_recv().is_ok() {}
                scan_and_ship(&cfg, &srcs, &mut offsets, &spool);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Fallback pass: covers missed events and retries the spool.
                scan_and_ship(&cfg, &srcs, &mut offsets, &spool);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

fn scan_and_ship(cfg: &Config, srcs: &[sources::Source], offsets: &mut Offsets, spool: &Spool) {
    let records = capture::collect_new(srcs, offsets);
    if !records.is_empty() {
        if let Err(e) = spool.append(&records) {
            eprintln!("hmc: spool append failed: {e:#}");
            return; // don't advance offsets if we couldn't persist
        }
        if let Err(e) = offsets.save() {
            eprintln!("hmc: offsets save failed: {e:#}");
        }
    }
    drain(cfg, spool);
}

fn drain(cfg: &Config, spool: &Spool) {
    loop {
        let batch = spool.peek(cfg.batch);
        if batch.is_empty() {
            return;
        }
        match uploader::send(cfg, &batch) {
            uploader::Upload::Ok => {
                let _ = spool.drop_front(batch.len());
            }
            uploader::Upload::Drop => {
                eprintln!("hmc: server rejected {} record(s), dropping", batch.len());
                let _ = spool.drop_front(batch.len());
            }
            uploader::Upload::Retry => {
                // Leave everything spooled; try again on the next pass.
                return;
            }
        }
    }
}
