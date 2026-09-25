//! Advisory locks between concurrent `tilo-vmd rpc` processes, one per SSH
//! connection. VM-wide work (vm-scope swaps, gc, upgrades, shutdown) takes
//! the VM lock exclusively; everything else takes it shared plus an
//! exclusive lock on the slot it changes. Locks are never waited for: a
//! busy lock is reported so the caller can retry.
//!
//! Host-side tooling takes the same lock around guest commands with
//! `flock <root>/locks/vm.lock <command>`.

use advisory_lock::{FileLock, LockMode, TryLockError};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use crate::error::DaemonError;

pub struct Locks {
    dir: PathBuf,
}

/// Held until dropped.
#[must_use]
pub struct Guard {
    _lock: FileLock,
}

impl Locks {
    pub fn new(root: &Path) -> Self {
        Self {
            dir: root.join("locks"),
        }
    }

    fn take(&self, name: &str, exclusive: bool, what: &str) -> Result<Guard, DaemonError> {
        fs::create_dir_all(&self.dir).map_err(DaemonError::io("create lock directory"))?;
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(name))
            .map_err(DaemonError::io(format!("open lock {name}")))?;
        let mode = if exclusive {
            LockMode::Exclusive
        } else {
            LockMode::Shared
        };
        match FileLock::try_lock(file, mode) {
            Ok(lock) => Ok(Guard { _lock: lock }),
            Err(TryLockError::WouldBlock) => Err(DaemonError::Busy(what.to_owned())),
            Err(TryLockError::Error(e)) => Err(DaemonError::io(format!("lock {name}"))(e)),
        }
    }

    /// # Errors
    /// [`DaemonError::Busy`] while any other operation runs.
    pub fn vm_exclusive(&self) -> Result<Guard, DaemonError> {
        self.take(
            "vm.lock",
            true,
            "the VM (a VM-wide swap, gc, upgrade or shutdown)",
        )
    }

    /// # Errors
    /// [`DaemonError::Busy`] while VM-wide work runs.
    pub fn vm_shared(&self) -> Result<Guard, DaemonError> {
        self.take(
            "vm.lock",
            false,
            "the VM (a VM-wide swap, gc, upgrade or shutdown)",
        )
    }

    /// Held by an in-VM build (and the rsync before it) of one worktree;
    /// `gc` never evicts a build dir whose lock is held.
    ///
    /// # Errors
    /// [`DaemonError::Busy`] while another build of this worktree runs.
    pub fn src(&self, worktree: &str) -> Result<Guard, DaemonError> {
        self.take(
            &format!("src-{worktree}.lock"),
            true,
            &format!("the in-VM build dir of worktree '{worktree}'"),
        )
    }

    /// # Errors
    /// [`DaemonError::Busy`] while another operation changes this slot.
    pub fn slot(&self, name: &str) -> Result<Guard, DaemonError> {
        self.take(
            &format!("slot-{name}.lock"),
            true,
            &format!("slot '{name}'"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_excludes_and_shared_shares() {
        let root = tempfile::tempdir().unwrap();
        let locks = Locks::new(root.path());
        let a = locks.vm_shared().unwrap();
        let b = locks.vm_shared().unwrap();
        assert!(matches!(locks.vm_exclusive(), Err(DaemonError::Busy(_))));
        drop((a, b));
        let held = locks.vm_exclusive().unwrap();
        assert!(matches!(locks.vm_shared(), Err(DaemonError::Busy(_))));
        drop(held);
        let slot = locks.slot("a").unwrap();
        assert!(matches!(locks.slot("a"), Err(DaemonError::Busy(_))));
        assert!(locks.slot("b").is_ok());
        drop(slot);
    }
}
