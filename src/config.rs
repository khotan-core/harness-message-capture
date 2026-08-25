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
    /// Fallback rescan interval. Preset. Not a user-facing setting.
    #[serde(default = "default_poll_secs")]
    pub poll_secs: u64,
    /// Max records per upload batch. Preset. Not a user-facing setting.
    #[serde(default = "default_batch")]
    pub batch: usize,
    /// Roots searched for customer repositories and worktrees.
    #[serde(default = "default_search_roots")]
    pub search_roots: Vec<PathBuf>,
    /// Directory names or absolute paths that may upload chats. Names match
    /// the folder leaf exactly. An empty list sends nothing.
    #[serde(default)]
    pub allow_repos: Vec<String>,
}

fn default_poll_secs() -> u64 {
    45
}

/// Records per request. The real ceiling on a batch is bytes, not lines; this
/// only keeps a burst of small lines from building an unbounded request.
fn default_batch() -> usize {
    2000
}

/// `batch` is a preset. Installs carry ceilings from before the uploader sized a
/// request in bytes, and those numbers now only cap throughput.
fn preset_batch(stored: usize) -> usize {
    stored.max(default_batch())
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
        let mut cfg: Config = toml::from_str(&s).context("config.toml is malformed")?;
        cfg.batch = preset_batch(cfg.batch);
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, self.render())?;
        Ok(())
    }

    /// Commented TOML so the allow list is obvious after install.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("# khotan-observer machine config\n");
        out.push_str("# The next scan reads this file. Restart is not required.\n");
        out.push_str("# poll_secs, batch, and search_roots are presets.\n");
        out.push_str("# Edit only allow_repos, or run: khotan-observer configure\n\n");
        out.push_str(&format!("device_id = {}\n", toml_quote(&self.device_id)));
        out.push_str(&format!("poll_secs = {}\n", self.poll_secs));
        out.push_str(&format!("batch = {}\n\n", self.batch));
        out.push_str("search_roots = [\n");
        for root in &self.search_roots {
            out.push_str(&format!("    {},\n", toml_quote(&root.to_string_lossy())));
        }
        out.push_str("]\n\n");
        out.push_str("# Repositories that may upload chats from this machine.\n");
        out.push_str("# Use the exact folder name, such as podium-automation.\n");
        out.push_str("# An empty list sends nothing.\n");
        out.push_str("allow_repos = [\n");
        for name in &self.allow_repos {
            out.push_str(&format!("    {},\n", toml_quote(name)));
        }
        out.push_str("]\n");
        out
    }

    pub fn fresh(device_id: String) -> Config {
        Config {
            endpoint: None,
            token: None,
            device_id,
            poll_secs: default_poll_secs(),
            batch: default_batch(),
            search_roots: default_search_roots(),
            allow_repos: Vec::new(),
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
fn toml_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

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

    #[test]
    fn a_stale_batch_ceiling_moves_up_to_the_preset() {
        assert_eq!(preset_batch(200), default_batch());
        assert_eq!(preset_batch(600), default_batch());
        assert_eq!(preset_batch(default_batch() * 2), default_batch() * 2);
    }

    #[test]
    fn render_round_trips_and_documents_the_allow_list() {
        let cfg = Config {
            endpoint: None,
            token: None,
            device_id: "abc".into(),
            poll_secs: 45,
            batch: 200,
            search_roots: vec![PathBuf::from("/tmp/work")],
            allow_repos: vec!["podium".into(), "chief".into()],
        };
        let body = cfg.render();
        assert!(body.contains("An empty list sends nothing."));
        assert!(body.contains("\"podium\""));
        let parsed: Config = toml::from_str(&body).unwrap();
        assert_eq!(parsed.device_id, "abc");
        assert_eq!(parsed.allow_repos, vec!["podium", "chief"]);
        assert_eq!(parsed.search_roots, vec![PathBuf::from("/tmp/work")]);
    }
}
