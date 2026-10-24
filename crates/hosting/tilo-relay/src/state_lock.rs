//! One relay process per state directory.
//!
//! The relay that serves a state holds an exclusive lock on it for as long as
//! it runs, and what must work on a state no relay is changing (the re-seal's
//! verbatim copy, `crate::reseal`) takes the same lock first.

use std::fs::OpenOptions;

use std::path::Path;
use tilo_files::native::lock::{FileLock, TryLockError};

/// The lock file, in the state directory.
pub const LOCK_FILE: &str = "relay.lock";

/// The exclusive lock on a state directory, released when dropped.
#[derive(Debug)]
pub struct StateLock {
    _held: FileLock,
}

impl StateLock {
    /// Lock `state_root`, creating it when it does not exist.
    ///
    /// # Errors
    ///
    /// Another process holds the lock (a relay runs on this state), or the
    /// lock file cannot be opened.
    pub fn acquire(state_root: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(state_root)
            .map_err(|error| format!("create {}: {error}", state_root.display()))?;
        let path = state_root.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|error| format!("open {}: {error}", path.display()))?;
        match FileLock::try_exclusive(file) {
            Ok(held) => Ok(Self { _held: held }),
            Err(TryLockError::WouldBlock) => Err(format!(
                "a relay is running on {}: stop it first",
                state_root.display()
            )),
            Err(TryLockError::Error(error)) => Err(format!("lock {}: {error}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_is_locked_by_one_holder_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let held = StateLock::acquire(dir.path()).unwrap();
        assert!(StateLock::acquire(dir.path()).is_err());
        drop(held);
        StateLock::acquire(dir.path()).unwrap();
    }
}
