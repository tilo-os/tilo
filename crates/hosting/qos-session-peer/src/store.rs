//! One OS-custodied writer, atomic replacement, and a mandatory directory barrier.
use crate::action_timing::Timing;
use anyhow::{Context, Result, bail};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

pub const MAX_AGGREGATE_BYTES: usize = 64 * 1024 * 1024;

pub struct SessionStore {
    root: PathBuf,
    _lock: Custody,
    poisoned: bool,
}

/// Custody belongs to this owner, rather than a fork child's inherited open
/// file description. Normal drop explicitly releases the lock before close.
/// Abrupt process death still follows the operating system's descriptor rules.
pub(crate) struct Custody {
    file: File,
    acquired_by: u32,
}
impl Drop for Custody {
    fn drop(&mut self) {
        // An inherited guard must not unlock a still-live acquiring process.
        if self.acquired_by == std::process::id() {
            let _ = self.file.unlock();
        }
    }
}
pub(crate) fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    sync_dir(path.parent().context("directory has no parent")?)
}
pub(crate) fn check_dir(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        bail!("state directory must be a real directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            bail!("state directory must be private and owned by this operator");
        }
    }
    Ok(())
}
pub(crate) fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}
pub(crate) fn custody(path: &Path) -> Result<Custody> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        bail!("custody path must be a regular file");
    }
    file.try_lock()
        .context("another process owns this native session or identity")?;
    Ok(Custody {
        file,
        acquired_by: std::process::id(),
    })
}
impl SessionStore {
    pub fn create(root: &Path) -> Result<Self> {
        private_dir(root).context("initialization requires a new state directory")?;
        Self::open(root)
    }
    pub fn open(root: &Path) -> Result<Self> {
        check_dir(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            _lock: custody(&root.join("owner.lock"))?,
            poisoned: false,
        })
    }
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn read(&self) -> Result<Vec<u8>> {
        let path = self.root.join("session.bin");
        if !fs::symlink_metadata(&path)?.is_file() {
            bail!("session aggregate must be a regular file");
        }
        let mut file = File::open(path)?;
        if file.metadata()?.len() > MAX_AGGREGATE_BYTES as u64 {
            bail!("aggregate exceeds the local storage size limit");
        }
        let mut bytes = vec![];
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    pub fn commit(&mut self, bytes: &[u8]) -> Result<()> {
        self.commit_with_barrier(bytes, sync_dir, None)
    }
    pub(crate) fn commit_timed(&mut self, bytes: &[u8], timing: &Timing) -> Result<()> {
        self.commit_with_barrier(bytes, sync_dir, Some(timing))
    }
    fn commit_with_barrier(
        &mut self,
        bytes: &[u8],
        barrier: impl FnOnce(&Path) -> Result<()>,
        timing: Option<&Timing>,
    ) -> Result<()> {
        if self.poisoned {
            bail!("durable barrier failed; reopen the owner before proceeding");
        }
        if bytes.len() > MAX_AGGREGATE_BYTES {
            bail!("aggregate exceeds the local storage size limit");
        }
        let result = (|| {
            if let Some(timing) = timing {
                timing.mark_count("aggregate_file_bytes", bytes.len());
                timing.mark("atomic_write_start");
            }
            atomic_file::write_with_mode(&self.root.join("session.bin"), bytes, 0o600)?;
            if let Some(timing) = timing {
                timing.mark("atomic_write_end");
                timing.mark("directory_sync_start");
            }
            barrier(&self.root)?;
            if let Some(timing) = timing {
                timing.mark("directory_sync_end");
            }
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inherited_custody_cleanup_cannot_unlock_the_acquiring_process() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("custody.lock");
        let mut inherited = custody(&path).unwrap();
        // A duplicate keeps the shared open-file description alive exactly as
        // the acquiring process would after fork. Simulate only the inherited
        // guard's process identity; no Rust code runs inside a fork child.
        let acquiring_process_file = inherited.file.try_clone().unwrap();
        inherited.acquired_by = std::process::id().wrapping_add(1);
        drop(inherited);
        assert!(
            custody(&path).is_err(),
            "non-acquiring cleanup unlocked a live owner's description"
        );
        drop(acquiring_process_file);
        assert!(
            custody(&path).is_ok(),
            "final descriptor close must release custody"
        );
    }
    #[test]
    fn oversize_commit_cannot_replace_reopenable_state() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = SessionStore::create(&temp.path().join("state")).unwrap();
        store.commit(b"retained").unwrap();
        assert!(store.commit(&vec![0; MAX_AGGREGATE_BYTES + 1]).is_err());
        assert_eq!(store.read().unwrap(), b"retained");
        assert!(!store.is_poisoned());
    }
    #[test]
    fn directory_barrier_failure_is_not_an_acknowledgement() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = SessionStore::create(&temp.path().join("state")).unwrap();
        store.commit(b"old").unwrap();
        let missing = temp.path().join("missing-directory");
        assert!(
            store
                .commit_with_barrier(b"new", |_| sync_dir(&missing), None)
                .is_err()
        );
        assert!(store.commit(b"ignored").is_err());
        // Rename can have happened; restart recovers a complete value, never a receipt.
        drop(store);
        let mut reopened = SessionStore::open(&temp.path().join("state")).unwrap();
        assert_eq!(reopened.read().unwrap(), b"new");
        reopened.commit(b"new").unwrap();
    }
}
