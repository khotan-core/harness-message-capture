use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::PathBuf;

/// On-disk configuration written by `configure` and read by the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Legacy global endpoint, accepted only so v1 configs still load.
    #[serde(default, skip_serializing)]
    pub endpoint: Option<String>,
    /// Legacy global bearer token, accepted only for the local proof receiver.
    #[serde(default, skip_serializing)]
    pub token: Option<String>,
    /// Stable identifier for this install, generated once at enrollment.
    pub device_id: String,
    /// How often (seconds) to run the fallback rescan even without fs events.
    #[serde(default = "default_poll_secs")]
    pub poll_secs: u64,
    /// Max records per upload batch.
    #[serde(default = "default_batch")]
    pub batch: usize,
    /// Roots searched for customer repositories and worktrees.
    #[serde(default = "default_search_roots")]
    pub search_roots: Vec<PathBuf>,
}

fn default_poll_secs() -> u64 {
    45
}

fn default_batch() -> usize {
    200
}

pub fn default_search_roots() -> Vec<PathBuf> {
    let h = home();
    [
        h.join("Developer"),
        h.join("Projects"),
        h.join("repos"),
        h.join("code"),
        h.join("conductor").join("workspaces"),
        h.join(".cursor").join("worktrees"),
    ]
    .into_iter()
    .filter(|path| path.is_dir())
    .collect()
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

    pub fn fresh(device_id: String) -> Config {
        Config {
            endpoint: None,
            token: None,
            device_id,
            poll_secs: default_poll_secs(),
            batch: default_batch(),
            search_roots: default_search_roots(),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_global_fields_load_but_are_not_saved() {
        let cfg: Config = toml::from_str(
            r#"
endpoint = "http://old.example/ingest"
token = "old-secret"
device_id = "device"
poll_secs = 10
batch = 20
"#,
        )
        .unwrap();
        assert_eq!(cfg.endpoint.as_deref(), Some("http://old.example/ingest"));
        assert_eq!(cfg.token.as_deref(), Some("old-secret"));

        let saved = toml::to_string(&cfg).unwrap();
        assert!(!saved.contains("old.example"));
        assert!(!saved.contains("old-secret"));
    }
}
