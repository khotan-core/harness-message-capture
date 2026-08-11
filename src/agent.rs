use crate::config::home;
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const LABEL: &str = "com.harness.messagecapture";

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
        .join("harness-message-capture.log")
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
        <string>watch</string>
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

pub fn install() -> Result<()> {
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
    println!("installed LaunchAgent: {}", plist.display());
    println!("logs: {}", log_path().display());
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let plist = plist_path();
    let _ = Command::new("launchctl").arg("unload").arg(&plist).status();
    if plist.exists() {
        fs::remove_file(&plist)?;
    }
    println!("removed LaunchAgent: {}", plist.display());
    Ok(())
}
