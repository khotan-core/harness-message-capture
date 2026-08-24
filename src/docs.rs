use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

/// Glossary that ships inside the binary. The installer writes the same text
/// to disk so a machine without this repo can still read it.
pub const HELP: &str = include_str!("../dist/help.md");

pub fn docs_path() -> PathBuf {
    crate::config::home()
        .join(".local")
        .join("share")
        .join("khotan-observer")
        .join("help.md")
}

pub fn print() {
    print!("{HELP}");
}

pub fn write() -> Result<PathBuf> {
    let path = docs_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&path, HELP).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::HELP;

    #[test]
    fn glossary_covers_skip_lines() {
        assert!(HELP.contains("no repo on this machine, nothing sent"));
        assert!(HELP.contains("dest file broken, nothing sent"));
        assert!(HELP.contains("matched two folders, nothing sent"));
        assert!(HELP.contains("khotan-observer docs"));
    }
}
