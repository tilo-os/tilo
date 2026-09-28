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

    /// Start a new, empty resume log for `device`, removing its older logs:
    /// a subscription without a usable position replays everything, so
    /// nothing an older log recorded is needed any more.
    ///
    /// # Errors
    /// Returns an error if the log directory can't be prepared.
    pub fn start_resume(&self, device: &[u8; 32]) -> Result<[u8; 16], StoreError> {
        let dir = self.resume_dir(device);
        if dir.exists() {
            for entry in fs::read_dir(&dir)?.flatten() {
                let _ = fs::remove_file(entry.path());
            }
        }
        fs::create_dir_all(&dir)?;
        let log: [u8; 16] = rand::random();
        fs::File::create(self.resume_path(device, &log))?;
        Ok(log)
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

    /// Cut `device`'s log `log` back to `len` entries, so what it records
    /// matches the position a resume presented. Returns whether it held
    /// that many.
    ///
    /// # Errors
    /// Returns an error if the log can't be opened or truncated.
    pub fn truncate_resume(
        &self,
        device: &[u8; 32],
        log: &[u8; 16],
        len: u64,
    ) -> Result<bool, StoreError> {
        let Ok(file) = fs::OpenOptions::new()
            .write(true)
            .open(self.resume_path(device, log))
        else {
            return Ok(false);
        };
        let want = len.saturating_mul(DIGEST_BYTES as u64);
        if file.metadata()?.len() < want {
            return Ok(false);
        }
        file.set_len(want)?;
        Ok(true)
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
        assert!(store.truncate_resume(&device, &log, 2).unwrap());
        store.append_resume(&device, &log, &[[4; 32]]).unwrap();
        assert_eq!(
            store.load_resume(&device, &log, 3).unwrap().unwrap(),
            HashSet::from([[1; 32], [2; 32], [4; 32]])
        );
        let fresh = store.start_resume(&device).unwrap();
        assert!(store.load_resume(&device, &log, 1).unwrap().is_none());
        assert_eq!(
            store.load_resume(&device, &fresh, 0).unwrap().unwrap(),
            HashSet::new()
        );
    }
}
