use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::PathBuf;

/// On-disk configuration written by `hmc enroll` and read by the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Fully-qualified ingest endpoint, e.g. https://example.com/ingest
    pub endpoint: String,
    /// Per-machine bearer token issued at enrollment.
    pub token: String,
    /// Stable identifier for this install, generated once at enrollment.
    pub device_id: String,
    /// How often (seconds) to run the fallback rescan even without fs events.
    #[serde(default = "default_poll_secs")]
    pub poll_secs: u64,
    /// Max records per upload batch.
    #[serde(default = "default_batch")]
    pub batch: usize,
}

fn default_poll_secs() -> u64 {
    45
}

fn default_batch() -> usize {
    200
}

impl Config {
    pub fn load() -> Result<Config> {
        let path = config_path();
        let mut s = String::new();
        fs::File::open(&path)
            .with_context(|| format!("no config at {} — run `hmc enroll` first", path.display()))?
            .read_to_string(&mut s)?;
        let cfg: Config = toml::from_str(&s).context("config.toml is malformed")?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let body = toml::to_string_pretty(self)?;
        fs::write(&path, body)?;
        Ok(())
    }
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

pub fn config_dir() -> PathBuf {
    home().join(".config").join("harness-message-capture")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn state_dir() -> PathBuf {
    home()
        .join(".local")
        .join("state")
        .join("harness-message-capture")
}

/// Generate a 128-bit random hex id from the OS RNG, no external crate.
pub fn random_id() -> Result<String> {
    let mut buf = [0u8; 16];
    let mut f = fs::File::open("/dev/urandom").context("open /dev/urandom")?;
    f.read_exact(&mut buf)?;
    let mut out = String::with_capacity(32);
    for b in buf {
        out.push_str(&format!("{b:02x}"));
    }
    Ok(out)
}
