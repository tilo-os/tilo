//! Small monotone owner records that ride beside the signed aggregate.
//!
//! `session.acks` holds framed, checksummed records the owner appends with
//! one file barrier instead of re-encoding, signing and committing the whole
//! aggregate (consumer delivery ACK progress). The store only frames and
//! barriers bytes; the owner authenticates and interprets them.
//!
//! Records must be *idempotent and monotone* against the aggregate: applying
//! any subset of genuine records, in any order, to any later aggregate must
//! be harmless (the owner validates each against the current ledger and only
//! ever advances). That is what makes the log independent of checkpoints and
//! journal compaction: it is never bound to one checkpoint, nothing replays it
//! "onto" state, and a stale or duplicated record is inert.
//!
//! Frame: `payload length u32 | payload | blake3(payload)`. Parsing stops at
//! the first truncated or mismatching frame; only a torn tail of an
//! unacknowledged append can end that way (single appender, every
//! acknowledged record fully written and barriered before its caller
//! returns). A torn tail is cut before the next append, and that append's
//! barrier makes the cut durable.
//!
//! After a full aggregate commit that folds every applied record (only an
//! owner opened through recovery commits such aggregates), the log is
//! truncated without a barrier: a truncation lost in a crash only leaves
//! folded, inert records behind.
use super::*;

pub(super) const NAME: &str = "session.acks";
/// Appends beyond this bound are refused (without writing or poisoning) so
/// the caller folds the log with one ordinary aggregate commit instead.
pub(super) const MAX_BYTES: u64 = 256 * 1024;
const MAX_RECORD: usize = 4096;

pub(super) struct SideLog {
    pub file: Option<File>,
    pub valid_len: u64,
    pub dirty_tail: bool,
}

/// An append refused before any write; the store stays healthy.
#[derive(Debug)]
pub(crate) struct SideLogFull;
impl std::fmt::Display for SideLogFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("session side log is full; fold it with an aggregate commit")
    }
}
impl std::error::Error for SideLogFull {}

pub(super) fn frame(payload: &[u8]) -> Vec<u8> {
    let mut framed = Vec::with_capacity(4 + payload.len() + 32);
    framed.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    framed.extend_from_slice(payload);
    framed.extend_from_slice(blake3::hash(payload).as_bytes());
    framed
}

/// Complete records and the byte length of the valid prefix.
pub(super) fn parse(bytes: &[u8]) -> (Vec<Vec<u8>>, u64) {
    let mut records = vec![];
    let mut offset = 0usize;
    while let Some(len) = bytes
        .get(offset..offset + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
    {
        if len == 0 || len > MAX_RECORD {
            break;
        }
        let Some(payload) = bytes.get(offset + 4..offset + 4 + len) else {
            break;
        };
        let Some(digest) = bytes.get(offset + 4 + len..offset + 4 + len + 32) else {
            break;
        };
        if blake3::hash(payload).as_bytes() != digest {
            break;
        }
        records.push(payload.to_vec());
        offset += 4 + len + 32;
    }
    (records, offset as u64)
}

impl SessionStore {
    /// Owner recovery: every complete side record, and prime appends. Only
    /// an owner that applies these records to the aggregate it commits may
    /// call this; its later aggregate commits then fold (and truncate) them.
    pub(crate) fn read_side_log_for_owner(&mut self) -> Result<Vec<Vec<u8>>> {
        let directory = self.checked_directory()?;
        let (records, valid_len, file_len, exists) =
            match directory.open_read(OsStr::new(NAME), Privacy::OwnerOnly) {
                Ok(file) => {
                    let bytes = Self::read_bounded(file)?;
                    let (records, valid_len) = parse(&bytes);
                    (records, valid_len, bytes.len() as u64, true)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (vec![], 0, 0, false),
                Err(error) => return Err(error.into()),
            };
        self.checked_directory()?;
        if file_len != valid_len {
            tracing::info!(
                records = records.len(),
                discarded_tail_bytes = file_len - valid_len,
                "session side log torn tail"
            );
        }
        self.side_log = Some(SideLog {
            file: if exists {
                Some(open_side_log(&directory)?)
            } else {
                None
            },
            valid_len,
            dirty_tail: file_len != valid_len,
        });
        Ok(records)
    }
    /// Whether side records may replace aggregate commits (same switch as the
    /// journal; `QOS_HOST_PERF_BASELINE=1` keeps whole aggregate commits,
    /// `QOS_HOST_PERF2_BASELINE=1` keeps journaled aggregate ACK commits).
    pub(crate) fn side_log_enabled(&self) -> bool {
        self.journal_enabled && self.side_log.is_some() && !self.poisoned && !perf2_baseline()
    }
    /// Durably append one record: returns only after its file barrier.
    /// `SideLogFull` performs no write; any other failure poisons the store.
    pub(crate) fn append_side_record(&mut self, payload: &[u8]) -> Result<()> {
        if self.poisoned {
            bail!("durable barrier failed; reopen the owner before proceeding");
        }
        anyhow::ensure!(
            !payload.is_empty() && payload.len() <= MAX_RECORD,
            "side record size out of bounds"
        );
        let framed = frame(payload);
        let log = self
            .side_log
            .as_ref()
            .context("side log not primed by owner recovery")?;
        if log.valid_len + framed.len() as u64 > MAX_BYTES {
            return Err(SideLogFull.into());
        }
        self.preflight(framed.len())?;
        let result = self.append_side_inner(&framed);
        if result.is_err() {
            self.poisoned = true;
            self.journal = None;
            self.side_log = None;
        } else {
            count(&SIDE_APPENDS, 1);
        }
        result
    }
    fn append_side_inner(&mut self, framed: &[u8]) -> Result<()> {
        let directory = self.checked_directory()?;
        #[cfg(test)]
        test_fault::checkpoint(&self.root, test_fault::Point::BeforeWrite)?;
        let log = self.side_log.as_mut().expect("checked above");
        match log.file.as_mut() {
            Some(file) => {
                if log.dirty_tail {
                    file.set_len(log.valid_len)?;
                    log.dirty_tail = false;
                }
                file.write_all(framed)?;
            }
            None => {
                // First record: publish complete contents atomically (file
                // barrier included) and make the name durable.
                directory.publish(
                    OsStr::new(NAME),
                    framed,
                    PublishMode::Replace,
                    Privacy::OwnerOnly,
                )?;
                count(&FULL_BARRIERS, 1);
                full_barrier(&self.directory)?;
                log.file = Some(open_side_log(&directory)?);
            }
        }
        count(&BYTES_WRITTEN, framed.len() as u64);
        log.valid_len += framed.len() as u64;
        #[cfg(test)]
        test_fault::checkpoint(&self.root, test_fault::Point::AfterAtomicWrite)?;
        #[cfg(test)]
        test_fault::checkpoint(&self.root, test_fault::Point::DirectoryBarrier)?;
        full_barrier(log.file.as_ref().expect("side log installed"))?;
        self.checked_directory()?;
        Ok(())
    }
    /// After a durable aggregate commit that folded every applied record.
    pub(super) fn truncate_folded_side_log(&mut self) {
        let Some(log) = self.side_log.as_mut() else {
            return;
        };
        if log.valid_len == 0 && !log.dirty_tail {
            return;
        }
        let Some(file) = log.file.as_ref() else {
            return;
        };
        // No barrier: surviving folded records are inert. The next append's
        // barrier makes the cut durable together with the new record.
        match file.set_len(0) {
            Ok(()) => {
                log.valid_len = 0;
                log.dirty_tail = false;
            }
            Err(error) => {
                // Keep appending after the known-valid prefix; nothing lost.
                tracing::debug!(%error, "session side log truncation deferred");
            }
        }
    }
}

fn perf2_baseline() -> bool {
    #[cfg(test)]
    {
        false
    }
    #[cfg(not(test))]
    {
        static BASELINE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *BASELINE.get_or_init(|| {
            std::env::var_os("QOS_HOST_PERF2_BASELINE").is_some_and(|value| value == "1")
        })
    }
}
#[cfg(unix)]
fn open_side_log(directory: &StorageDirectory) -> Result<File> {
    Ok(directory.open_append_existing(OsStr::new(NAME), Privacy::OwnerOnly)?)
}
#[cfg(not(unix))]
fn open_side_log(_: &StorageDirectory) -> Result<File> {
    bail!("session side log requires a Unix storage directory")
}
