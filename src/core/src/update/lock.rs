//! Non-blocking cross-process lock for update work.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

const LOCK_FILE: &str = ".vct-update.lock";

/// An acquired update lock. Dropping it releases the lock for another process.
pub(super) struct UpdateLock {
    _file: File,
}

impl UpdateLock {
    /// Tries to acquire the update lock without waiting.
    pub(super) fn try_acquire(lock_dir: &Path) -> Result<Option<Self>> {
        let path = update_lock_path(lock_dir);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("Failed to open update lock: {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error).context("Failed to claim update lock"),
        }
    }
}

pub(super) fn update_lock_path(lock_dir: &Path) -> PathBuf {
    lock_dir.join(LOCK_FILE)
}

// Both tests drop a lock and claim it again, so they join the update module's
// `update_spawn` serial group; the note above its own tests says why a
// concurrently forked child would otherwise keep the released `flock`.
#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial(update_spawn)]
    fn only_one_process_claims_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let first = UpdateLock::try_acquire(dir.path())
            .unwrap()
            .expect("first lock");
        assert!(UpdateLock::try_acquire(dir.path()).unwrap().is_none());
        drop(first);
        assert!(UpdateLock::try_acquire(dir.path()).unwrap().is_some());
    }

    #[test]
    #[serial(update_spawn)]
    fn concurrent_claims_have_one_winner() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().to_path_buf());
        let start = Arc::new(Barrier::new(3));
        let attempted = Arc::new(Barrier::new(3));
        let winners = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();

        for _ in 0..2 {
            let path = Arc::clone(&path);
            let start = Arc::clone(&start);
            let attempted = Arc::clone(&attempted);
            let winners = Arc::clone(&winners);
            threads.push(std::thread::spawn(move || {
                start.wait();
                let lock = UpdateLock::try_acquire(&path).unwrap();
                if lock.is_some() {
                    winners.fetch_add(1, Ordering::SeqCst);
                }
                attempted.wait();
                drop(lock);
            }));
        }

        start.wait();
        attempted.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(winners.load(Ordering::SeqCst), 1);
    }
}
