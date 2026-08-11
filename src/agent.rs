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

    // Reload if already present, then load.
    let _ = Command::new("launchctl").arg("unload").arg(&plist).status();
    let status = Command::new("launchctl")
        .arg("load")
        .arg("-w")
        .arg(&plist)
        .status()
        .context("run launchctl load")?;
    if !status.success() {
        anyhow::bail!("launchctl load failed for {}", plist.display());
    }
    println!("started background observer");
    println!("plist: {}", plist.display());
    println!("logs:  {}", log_path().display());
    Ok(())
}

/// Unload the LaunchAgent but leave the plist in place for a later `start`.
pub fn stop() -> Result<()> {
    let plist = plist_path();
    if !plist.exists() {
        println!("observer is not installed (no LaunchAgent at {})", plist.display());
        return Ok(());
    }
    let status = Command::new("launchctl")
        .arg("unload")
        .arg(&plist)
        .status()
        .context("run launchctl unload")?;
    if !status.success() {
        // Already unloaded is fine — launchctl may exit non-zero.
        eprintln!("note: launchctl unload returned {}", status);
    }
    println!("stopped background observer");
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
