use crate::config::home;
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const LABEL: &str = "com.khotan.observer";

fn plist_path() -> PathBuf {
    home()
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

fn log_path() -> PathBuf {
    home()
        .join("Library")
        .join("Logs")
        .join("khotan-observer.log")
}

fn plist_contents(exe: &str, log: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>run</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
    <key>LowPriorityIO</key>
    <true/>
    <key>Nice</key>
    <integer>10</integer>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#
    )
}

/// Install (or refresh) the LaunchAgent plist and load it so capture runs in the background.
pub fn start() -> Result<()> {
    // When no LaunchAgent is loaded, a foreground observer may own the lock.
    // Refuse to load a KeepAlive agent that would otherwise repeatedly restart
    // and contend for the same transcript files.
    let already_loaded = is_loaded();
    if !already_loaded {
        crate::singleton::ensure_available()?;
    }

    let exe = std::env::current_exe()
        .context("resolve current executable path")?
        .to_string_lossy()
        .to_string();
    let plist = plist_path();
    if let Some(parent) = plist.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = log_path().parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&plist, plist_contents(&exe, &log_path().to_string_lossy()))?;

    // Only unload when something is actually loaded; unloading a label that
    // isn't registered makes launchctl print a confusing I/O error.
    if already_loaded {
        let _ = Command::new("launchctl")
            .arg("unload")
            .arg(&plist)
            .output();
    }
    let out = Command::new("launchctl")
        .arg("load")
        .arg("-w")
        .arg(&plist)
        .output()
        .context("run launchctl load")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("launchctl load failed for {}: {}", plist.display(), err.trim());
    }
    println!("{} background observer running", crate::log::green("✓"));
    println!("  logs:   khotan-observer logs");
    println!("  status: khotan-observer status");
    println!("  stop:   khotan-observer stop");
    println!("  file:   {}", log_path().display());
    Ok(())
}

/// Unload the LaunchAgent but leave the plist in place for a later `start`.
pub fn stop() -> Result<()> {
    let plist = plist_path();
    if !plist.exists() {
        println!("observer is not installed (no LaunchAgent at {})", plist.display());
        return Ok(());
    }
    if !is_loaded() {
        println!("background observer is already stopped");
        return Ok(());
    }
    let _ = Command::new("launchctl")
        .arg("unload")
        .arg(&plist)
        .output()
        .context("run launchctl unload")?;
    println!("stopped background observer");
    Ok(())
}

/// Stream the background log, like `tail -f`.
pub fn logs(follow: bool) -> Result<()> {
    let log = log_path();
    if !log.exists() {
        anyhow::bail!(
            "no log yet at {} — run `khotan-observer start` first",
            log.display()
        );
    }
    let mut cmd = Command::new("tail");
    cmd.arg("-n").arg("80");
    if follow {
        cmd.arg("-f");
    }
    cmd.arg(&log);
    cmd.status().context("run tail")?;
    Ok(())
}

/// Stop the agent and remove the LaunchAgent plist entirely.
pub fn uninstall() -> Result<()> {
    let plist = plist_path();
    let _ = Command::new("launchctl").arg("unload").arg(&plist).status();
    if plist.exists() {
        fs::remove_file(&plist)?;
    }
    println!("uninstalled LaunchAgent: {}", plist.display());
    Ok(())
}

pub fn is_loaded() -> bool {
    Command::new("launchctl")
        .args(["list", LABEL])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn log_file() -> PathBuf {
    log_path()
}
