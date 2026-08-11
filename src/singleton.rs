use crate::config::state_dir;
use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::path::Path;

/// An exclusive, process-lifetime lock that prevents two observers from reading
/// the same offsets and appending duplicate records to the spool.
pub struct ObserverLock {
    _file: File,
}

/// Acquire the observer's single-instance lock.
///
/// The operating system releases this advisory lock automatically when its
/// process exits, including after a crash, so it cannot leave a stale lock.
pub fn acquire() -> Result<ObserverLock> {
    let dir = state_dir();
    fs::create_dir_all(&dir).context("create observer state directory")?;
    let path = dir.join("observer.lock");
    acquire_at(&path)
}

fn acquire_at(path: &Path) -> Result<ObserverLock> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("open observer lock {}", path.display()))?;

    file.try_lock_exclusive().map_err(|e| {
        anyhow::anyhow!(
            "another khotan-observer is already running; stop it before starting a second observer ({e})"
        )
    })?;

    Ok(ObserverLock { _file: file })
}

/// Check that no observer is active without retaining the lock.
pub fn ensure_available() -> Result<()> {
    drop(acquire()?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::acquire_at;
    use std::fs;

    #[test]
    fn rejects_a_second_observer_lock() {
        let path = std::env::temp_dir().join(format!(
            "khotan-observer-lock-test-{}",
            std::process::id()
        ));
        let first = acquire_at(&path).unwrap();
        assert!(acquire_at(&path).is_err());
        drop(first);
        assert!(acquire_at(&path).is_ok());
        let _ = fs::remove_file(path);
    }
}
