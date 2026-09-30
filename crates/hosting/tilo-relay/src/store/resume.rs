//! What each device has received of a workspace, for resumable subscriptions.
//!
//! A resuming subscriber's durable deliveries are recorded in its resume log,
//! `resume/<device>/<log>.log`: one BLAKE3 digest per delivered frame, in
//! delivery order. A [`crate::position`] names a log and a length; the frames
//! behind the first `len` digests are what a replay for that position leaves
//! out. The log is appended off the durability path: losing its newest
//! entries only makes a resume replay a little more.
//!
//! The store generation, minted once per workspace store, binds positions to
//! this state: after the store is replaced, every position is refused.

use std::collections::HashSet;
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;

use super::{StoreError, WorkspaceStore};

const GENERATION_FILE: &str = "generation";
const RESUME_DIR: &str = "resume";
const DIGEST_BYTES: usize = 32;
/// A log longer than this starts over: the device replays everything once,
/// and the log it then keeps is bounded by what that replay carried.
pub(crate) const MAX_RESUME_ENTRIES: u64 = 1 << 20;
/// Logs kept per device: the one each recent subscription started or forked.
const RETAINED_LOGS: usize = 4;

/// The digest a resume log records for a delivered frame: the frame's
/// canonical transport encoding.
pub(crate) fn frame_digest(frame: &crate::transport::Envelope) -> Option<[u8; 32]> {
    let encoded = crate::transport::encode(frame).ok()?;
    Some(*blake3::hash(&encoded).as_bytes())
}

impl WorkspaceStore {
    /// This store's generation, minted on first use.
    ///
    /// # Errors
    /// Returns an error if the generation can't be read or written.
    pub fn generation(&self) -> Result<[u8; 16], StoreError> {
        let path = self.root.join(GENERATION_FILE);
        if let Ok(bytes) = fs::read(&path)
            && let Ok(generation) = <[u8; 16]>::try_from(bytes.as_slice())
        {
            return Ok(generation);
        }
        // Concurrent subscribers may both find none: the first link wins and
        // every caller returns what it published, never its own guess.
        let minted: [u8; 16] = rand::random();
        let scratch = self
            .root
            .join(format!(".{GENERATION_FILE}-{}", hex::encode(minted)));
        atomic_file::write(&scratch, &minted)?;
        let linked = fs::hard_link(&scratch, &path);
        let _ = fs::remove_file(&scratch);
        match linked {
            Ok(()) => Ok(minted),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                <[u8; 16]>::try_from(fs::read(&path)?.as_slice())
                    .map_err(|_| std::io::Error::other("store generation is malformed").into())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn resume_dir(&self, device: &[u8; 32]) -> PathBuf {
        self.root.join(RESUME_DIR).join(hex::encode(device))
    }

    fn resume_path(&self, device: &[u8; 32], log: &[u8; 16]) -> PathBuf {
        self.resume_dir(device)
            .join(format!("{}.log", hex::encode(log)))
    }

    /// Start a new, empty resume log for `device`. Its older logs stay within
    /// [`RETAINED_LOGS`]: a session the device already left may subscribe
    /// after the one it uses, and must not take that session's log away.
    ///
    /// # Errors
    /// Returns an error if the log directory can't be prepared.
    pub fn start_resume(&self, device: &[u8; 32]) -> Result<[u8; 16], StoreError> {
        fs::create_dir_all(self.resume_dir(device))?;
        let log: [u8; 16] = rand::random();
        fs::File::create(self.resume_path(device, &log))?;
        self.prune_resume(device, &[log]);
        Ok(log)
    }

    /// Remove `device`'s logs beyond the newest [`RETAINED_LOGS`], never one
    /// of `keep`.
    fn prune_resume(&self, device: &[u8; 32], keep: &[[u8; 16]]) {
        let Ok(entries) = fs::read_dir(self.resume_dir(device)) else {
            return;
        };
        let keep: Vec<PathBuf> = keep
            .iter()
            .map(|log| self.resume_path(device, log))
            .collect();
        let mut others: Vec<(std::time::SystemTime, PathBuf)> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| !keep.contains(path))
            .map(|path| {
                let modified = fs::metadata(&path)
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                (modified, path)
            })
            .collect();
        others.sort_unstable_by_key(|(modified, _)| std::cmp::Reverse(*modified));
        for (_, path) in others
            .into_iter()
            .skip(RETAINED_LOGS.saturating_sub(keep.len()))
        {
            let _ = fs::remove_file(path);
        }
    }

    /// The digests of the first `len` entries of `device`'s log `log`, or
    /// `None` when that log does not hold them (removed, or cut short).
    ///
    /// # Errors
    /// Returns an error if the log exists but can't be read.
    pub fn load_resume(
        &self,
        device: &[u8; 32],
        log: &[u8; 16],
        len: u64,
    ) -> Result<Option<HashSet<[u8; 32]>>, StoreError> {
        if len > MAX_RESUME_ENTRIES {
            return Ok(None);
        }
        let Ok(mut file) = fs::File::open(self.resume_path(device, log)) else {
            return Ok(None);
        };
        let want = usize::try_from(len)
            .unwrap_or(usize::MAX)
            .saturating_mul(DIGEST_BYTES);
        let mut bytes = Vec::with_capacity(want.min(64 * 1024 * 1024));
        std::io::Read::by_ref(&mut file)
            .take(u64::try_from(want).unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)?;
        if bytes.len() < want {
            return Ok(None);
        }
        Ok(Some(
            bytes
                .chunks_exact(DIGEST_BYTES)
                .filter_map(|chunk| <[u8; 32]>::try_from(chunk).ok())
                .collect(),
        ))
    }

    /// Continue `device`'s log `log` from its first `len` entries under a
    /// new log. A session the device left may still be writing to the old
    /// log what the device never received; the new log records only what the
    /// resumed session delivers. `log` itself stays: the session that forked
    /// it may be lost before the device learns the new position, and the
    /// device then presents `log` again. Returns `None` when `log` does not
    /// hold `len` entries.
    ///
    /// # Errors
    /// Returns an error if the logs can't be read or written.
    pub fn fork_resume(
        &self,
        device: &[u8; 32],
        log: &[u8; 16],
        len: u64,
    ) -> Result<Option<[u8; 16]>, StoreError> {
        let Ok(bytes) = fs::read(self.resume_path(device, log)) else {
            return Ok(None);
        };
        let Some(held) = usize::try_from(len)
            .ok()
            .and_then(|len| len.checked_mul(DIGEST_BYTES))
            .and_then(|want| bytes.get(..want))
        else {
            return Ok(None);
        };
        let fork: [u8; 16] = rand::random();
        fs::write(self.resume_path(device, &fork), held)?;
        self.prune_resume(device, &[*log, fork]);
        Ok(Some(fork))
    }

    /// Append delivered frames' digests to `device`'s log `log`.
    ///
    /// # Errors
    /// Returns an error if the log can't be opened or written.
    pub fn append_resume(
        &self,
        device: &[u8; 32],
        log: &[u8; 16],
        digests: &[[u8; 32]],
    ) -> Result<(), StoreError> {
        let mut bytes = Vec::with_capacity(digests.len().saturating_mul(DIGEST_BYTES));
        for digest in digests {
            bytes.extend_from_slice(digest);
        }
        fs::OpenOptions::new()
            .append(true)
            .open(self.resume_path(device, log))?
            .write_all(&bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_first_uses_agree_on_one_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let minted: Vec<[u8; 16]> = std::thread::scope(|scope| {
            #[expect(
                clippy::needless_collect,
                reason = "every worker starts before the first is joined, or they do not race"
            )]
            let workers: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| store.generation().unwrap()))
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect()
        });
        assert!(minted.iter().all(|generation| *generation == minted[0]));
        assert_eq!(store.generation().unwrap(), minted[0]);
    }

    #[test]
    fn a_resume_log_keeps_what_was_delivered_and_starts_over_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let generation = store.generation().unwrap();
        assert_eq!(store.generation().unwrap(), generation, "minted once");
        let device = [2; 32];
        let log = store.start_resume(&device).unwrap();
        store
            .append_resume(&device, &log, &[[1; 32], [2; 32]])
            .unwrap();
        store.append_resume(&device, &log, &[[3; 32]]).unwrap();
        let held = store.load_resume(&device, &log, 2).unwrap().unwrap();
        assert_eq!(held, HashSet::from([[1; 32], [2; 32]]));
        assert!(
            store.load_resume(&device, &log, 4).unwrap().is_none(),
            "a position beyond the log is refused"
        );
        let fork = store.fork_resume(&device, &log, 2).unwrap().unwrap();
        store.append_resume(&device, &fork, &[[4; 32]]).unwrap();
        assert_eq!(
            store.load_resume(&device, &fork, 3).unwrap().unwrap(),
            HashSet::from([[1; 32], [2; 32], [4; 32]])
        );
        store.append_resume(&device, &log, &[[5; 32]]).unwrap();
        assert_eq!(
            store.load_resume(&device, &fork, 3).unwrap().unwrap(),
            HashSet::from([[1; 32], [2; 32], [4; 32]]),
            "a session left behind never writes to what the device resumes from"
        );
        assert!(store.fork_resume(&device, &fork, 9).unwrap().is_none());
        let fresh = store.start_resume(&device).unwrap();
        assert_eq!(
            store.load_resume(&device, &fresh, 0).unwrap().unwrap(),
            HashSet::new()
        );
    }

    /// The session that forked a position can be lost before the device
    /// hears the new one; the device then presents the old position again,
    /// and a session it already left may subscribe after the one it uses.
    #[test]
    fn a_position_survives_a_fork_the_device_never_heard_of() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let device = [2; 32];
        let log = store.start_resume(&device).unwrap();
        store
            .append_resume(&device, &log, &[[1; 32], [2; 32]])
            .unwrap();
        let lost = store.fork_resume(&device, &log, 2).unwrap().unwrap();
        store.append_resume(&device, &lost, &[[3; 32]]).unwrap();
        let resumed = store.fork_resume(&device, &log, 2).unwrap().unwrap();
        assert_eq!(
            store.load_resume(&device, &resumed, 2).unwrap().unwrap(),
            HashSet::from([[1; 32], [2; 32]])
        );
        assert!(store.fork_resume(&device, &log, 2).unwrap().is_some());
        store.start_resume(&device).unwrap();
        assert!(
            store.load_resume(&device, &resumed, 2).unwrap().is_some(),
            "a late subscription of a session the device left"
        );
        for _ in 0..2 * RETAINED_LOGS {
            store.start_resume(&device).unwrap();
        }
        assert_eq!(
            fs::read_dir(store.resume_dir(&device)).unwrap().count(),
            RETAINED_LOGS
        );
    }
}
