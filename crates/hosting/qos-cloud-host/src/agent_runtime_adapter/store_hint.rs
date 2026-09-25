//! Cheap, unlocked change hints for the agent runtime's Store reads.
//!
//! A [`StoreStamp`] names the exact aggregate files (`state.json` plus the
//! optional `state.journal`) by device, inode, length and change times. It is
//! taken under the Store lock when a result is verified and compared unlocked
//! afterwards: checkpoints are only replaced (new inode) and the journal only
//! grows, is cut back or is replaced, so any commit by any process changes the
//! stamp. A matching stamp is only a hint: every cached verdict also expires at
//! the next roster/authority/revocation time boundary (the same rule the host's
//! own retained peer uses) and is re-proved under the lock at least every
//! [`STORE_RECHECK`].
use std::{
    os::unix::fs::MetadataExt,
    path::Path,
    time::{Duration, Instant},
};

/// Longest interval a cached verdict is reused without re-proving it under the
/// Store lock, even when no Store file changed.
pub(crate) const STORE_RECHECK: Duration = Duration::from_secs(2);
/// Longest interval a restored committed peer is reused at one generation.
const PEER_REUSE_NS: u64 = 30_000_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FileStamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}
impl FileStamp {
    fn of(path: &Path) -> std::io::Result<Option<Self>> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => Ok(Some(Self {
                dev: metadata.dev(),
                ino: metadata.ino(),
                len: metadata.len(),
                mtime: (metadata.mtime(), metadata.mtime_nsec()),
                ctime: (metadata.ctime(), metadata.ctime_nsec()),
            })),
            Ok(_) => Err(std::io::Error::other("Store file is not a regular file")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// Identity of the Store aggregate files at one observation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct StoreStamp {
    checkpoint: FileStamp,
    journal: Option<FileStamp>,
}
impl StoreStamp {
    /// `None` when the files cannot be observed; callers then take the locked path.
    pub(crate) fn of(root: &Path) -> Option<Self> {
        Some(Self {
            checkpoint: FileStamp::of(&root.join("state.json")).ok()??,
            journal: FileStamp::of(&root.join("state.journal")).ok()?,
        })
    }
}

/// End of the interval in which a peer restored at `at_ns` from `state` stays
/// exact: the next certificate issue/expiry or revocation instant, and at most
/// [`PEER_REUSE_NS`] after the restore.
pub(crate) fn peer_reuse_until(state: &crate::State, at_ns: u64) -> u64 {
    let mut until = at_ns.saturating_add(PEER_REUSE_NS);
    if let Some(bound) = &state.bound {
        let mut bound_at = |instant: Option<u64>| {
            if let Some(instant) = instant.filter(|instant| *instant > at_ns) {
                until = until.min(instant);
            }
        };
        for certificate in bound.roster.iter().chain(std::iter::once(&bound.authority)) {
            bound_at(certificate.expires_at_ns);
            bound_at(Some(certificate.issued_at_ns));
        }
        for revocation in &bound.revocations {
            bound_at(Some(revocation.revoked_at_ns));
        }
    }
    until
}

/// One verdict proved under the Store lock, reusable while the Store files are
/// unchanged, the policy time stays inside `[at_ns, until_ns)` and the last
/// locked proof is younger than [`STORE_RECHECK`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Verified {
    stamp: StoreStamp,
    at_ns: u64,
    until_ns: u64,
    proved: Instant,
}
impl Verified {
    /// Call while the Store that produced the verdict is still open (locked).
    pub(crate) fn under_lock(root: &Path, state: &crate::State, now_ns: u64) -> Option<Self> {
        Some(Self {
            stamp: StoreStamp::of(root)?,
            at_ns: now_ns,
            until_ns: peer_reuse_until(state, now_ns),
            proved: Instant::now(),
        })
    }
    pub(crate) fn current(&self, root: &Path, now_ns: u64) -> bool {
        self.at_ns <= now_ns
            && now_ns < self.until_ns
            && self.proved.elapsed() < STORE_RECHECK
            && StoreStamp::of(root) == Some(self.stamp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn agent_runtime_store_stamp_observes_replace_append_and_cut() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(StoreStamp::of(dir.path()), None, "missing checkpoint");
        std::fs::write(dir.path().join("state.json"), b"{}").unwrap();
        let first = StoreStamp::of(dir.path()).unwrap();
        assert_eq!(StoreStamp::of(dir.path()), Some(first));
        // Journal creation and append.
        std::fs::write(dir.path().join("state.journal"), b"a").unwrap();
        let appended = StoreStamp::of(dir.path()).unwrap();
        assert_ne!(appended, first);
        {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .append(true)
                .open(dir.path().join("state.journal"))
                .unwrap()
                .write_all(b"b")
                .unwrap();
        }
        let grown = StoreStamp::of(dir.path()).unwrap();
        assert_ne!(grown, appended);
        // A cut back to the same length and a same-length rewrite still differ
        // by change time; a checkpoint replace changes the inode.
        std::thread::sleep(Duration::from_millis(5));
        let journal = std::fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join("state.journal"))
            .unwrap();
        journal.set_len(1).unwrap();
        journal.set_len(2).unwrap();
        assert_ne!(StoreStamp::of(dir.path()).unwrap(), grown);
        let replaced = dir.path().join("state.json.new");
        std::fs::write(&replaced, b"{}").unwrap();
        let before = StoreStamp::of(dir.path()).unwrap();
        std::fs::rename(&replaced, dir.path().join("state.json")).unwrap();
        assert_ne!(StoreStamp::of(dir.path()).unwrap(), before);
    }
}
