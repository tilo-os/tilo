//! Who put which content into which room: the attributions in
//! `content-puts.jsonl`, beside the journal's records and under its
//! single-writer lock. No bytes are kept here, only the attribution; the
//! uploader's identity is never consulted for access.
//!
//! The file is compacted the way the journal's own store is: a snapshot line
//! of the attributions that still matter, then one appended line per put.
//! Once the appended lines would outgrow the snapshot, the next put rewrites
//! the whole file through an atomic rename, so it stays bounded instead of
//! growing by one line per put for the life of the device. A compaction keeps
//! every attribution that still refers to content a room holds, asked through
//! [`ContentHolds`], and drops the ones whose content no room holds any more.
//!
//! A put is answered only once its line is durable, so an appended line is an
//! attribution whose put was answered. A line a failed write left half-made
//! has no newline: a store opened after it reads only up to the last complete
//! line and writes that file whole with its next put.

use std::fs::File;
use std::io::{Read, Seek as _, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::writer::{Injected, Write as Staged};
use super::{Durable, InvocationJournal, private_file};

/// The file the attributions are appended to.
pub(crate) const CONTENT_PUTS_FILE: &str = "content-puts.jsonl";

/// One finished content put.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContentPutRecord {
    /// Seconds since the Unix epoch, on the broker's policy clock.
    pub(crate) at: u64,
    /// The broker actor that put it (`agent:<id>`, a person's id).
    pub(crate) actor: String,
    pub(crate) room: String,
    pub(crate) hash: String,
    pub(crate) len: u64,
}

/// Whether the room a put names still holds its content: what a compaction
/// keeps an attribution by.
pub(crate) trait ContentHolds {
    fn holds(&mut self, room: &str, hash: &str) -> bool;
}

/// The file's first line: the attributions that still mattered when the file
/// was last compacted.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PutSnapshot {
    version: u32,
    puts: Vec<ContentPutRecord>,
}

/// One line appended after the snapshot.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Appended {
    version: u32,
    put: ContentPutRecord,
}

/// The snapshot format this writer makes, which every later one reads.
const VERSION: u32 = 1;

/// What the attribution file holds, as the journal stages it.
pub(super) struct ContentPutsStore {
    /// The snapshot line, as last compacted.
    snapshot: Vec<u8>,
    /// The puts appended after it, which a compaction folds into the snapshot
    /// so that it keeps the ones a room still holds.
    appended: Vec<ContentPutRecord>,
    /// Length of the snapshot plus every line appended after it, as staged.
    len: u64,
}

impl ContentPutsStore {
    /// The store `root`'s attribution file holds.
    ///
    /// # Errors
    ///
    /// Returns why the file cannot be read as this store.
    pub(super) fn read(root: &Path) -> Result<Self, String> {
        let mut file = match File::open(root.join(CONTENT_PUTS_FILE)) {
            Ok(file) => file,
            // Nothing put yet: the first put finds nothing to compact.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Self::empty();
            }
            Err(_) => return Err("content put attribution unreadable".into()),
        };
        private_file(&file).map_err(|_| "content put attribution unreadable")?;
        let size = file
            .metadata()
            .map_err(|_| "content put attribution unreadable")?
            .len();
        if size > InvocationJournal::MAX_BYTES {
            return Err("content put attribution exceeds storage limit".into());
        }
        let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or_default());
        file.seek(SeekFrom::Start(0))
            .map_err(|_| "content put attribution unreadable")?;
        Read::by_ref(&mut file)
            .take(InvocationJournal::MAX_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|_| "content put attribution unreadable")?;
        // A crash mid-line leaves its final line unterminated or with unwritten
        // (zero) blocks. Its put was never answered, so it is no attribution.
        let complete = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |at| at.saturating_add(1));
        let (bytes, torn) = bytes
            .split_at_checked(complete)
            .ok_or("content put attribution is corrupt or unsupported")?;
        if torn.iter().any(|byte| *byte != 0) {
            return Err("content put attribution is corrupt or unsupported".into());
        }
        // The first line is the snapshot once the file was compacted; a file
        // whose writes all predate that holds one bare put per line.
        let snapshot_len = if bytes.starts_with(b"{\"version\"") {
            let line = bytes
                .split_inclusive(|byte| *byte == b'\n')
                .next()
                .ok_or("content put attribution is corrupt or unsupported")?;
            let snapshot: PutSnapshot = serde_json::from_slice(line.trim_ascii())
                .map_err(|_| "content put attribution is corrupt or unsupported")?;
            if !matches!(snapshot.version, 1..=VERSION) {
                return Err("content put attribution version unsupported".into());
            }
            line.len()
        } else {
            0
        };
        let compacted = snapshot_len != 0;
        let (snapshot, puts) = bytes
            .split_at_checked(snapshot_len)
            .ok_or("content put attribution is corrupt or unsupported")?;
        let mut appended = Vec::new();
        for line in puts.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let put: ContentPutRecord = if compacted {
                let appended: Appended = serde_json::from_slice(line)
                    .map_err(|_| "content put attribution is corrupt or unsupported")?;
                if appended.version != VERSION {
                    return Err("content put attribution version unsupported".into());
                }
                appended.put
            } else {
                serde_json::from_slice(line)
                    .map_err(|_| "content put attribution is corrupt or unsupported")?
            };
            appended.push(put);
        }
        if !compacted {
            return Self::of(&appended);
        }
        Ok(Self {
            snapshot: snapshot.to_vec(),
            appended,
            len: complete as u64,
        })
    }

    /// A store that holds exactly `puts`: its snapshot line.
    fn of(puts: &[ContentPutRecord]) -> Result<Self, String> {
        let snapshot = snapshot_of(puts)?;
        Ok(Self {
            len: snapshot.len() as u64,
            snapshot,
            appended: Vec::new(),
        })
    }

    /// The store with the empty snapshot, as the first put finds it.
    fn empty() -> Result<Self, String> {
        Self::of(&[])
    }

    /// Whether a line of `bytes` is appended rather than compacted: the
    /// appended lines never outgrow their snapshot, which is what keeps the
    /// file bounded, and never the store's own limit.
    fn takes(&self, bytes: u64) -> bool {
        let appended = self
            .len
            .saturating_sub(self.snapshot.len() as u64)
            .saturating_add(bytes);
        appended <= self.snapshot.len() as u64
            && self.len.saturating_add(bytes) <= InvocationJournal::MAX_BYTES
    }
    /// Every attribution the file holds: the snapshot's, then those appended
    /// after it.
    fn held(&self) -> Result<Vec<ContentPutRecord>, String> {
        let mut kept = Vec::new();
        for line in self.snapshot.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let snapshot: PutSnapshot = serde_json::from_slice(line)
                .map_err(|_| "content put attribution is corrupt or unsupported")?;
            kept.extend(snapshot.puts);
        }
        for put in &self.appended {
            match kept
                .iter_mut()
                .find(|kept| kept.room == put.room && kept.hash == put.hash)
            {
                Some(kept) => kept.clone_from(put),
                None => kept.push(put.clone()),
            }
        }
        Ok(kept)
    }

    /// The write `record` needs: one appended line, or a compaction of every
    /// attribution a room still holds, this one included.
    fn put(
        &mut self,
        record: &ContentPutRecord,
        holds: &mut dyn ContentHolds,
    ) -> Result<Staged, String> {
        let mut line = serde_json::to_vec(&Appended {
            version: VERSION,
            put: record.clone(),
        })
        .map_err(|_| "content put attribution encoding failed")?;
        line.push(b'\n');
        if self.takes(line.len() as u64) {
            self.len = self.len.saturating_add(line.len() as u64);
            self.appended.push(record.clone());
            return Ok(Staged::Beside {
                name: CONTENT_PUTS_FILE,
                line,
            });
        }
        let mut kept: Vec<ContentPutRecord> = self
            .held()?
            .into_iter()
            .filter(|put| holds.holds(&put.room, &put.hash))
            .collect();
        // The put this compaction records, and one already kept for the same
        // room and content: the later of the two times.
        match kept
            .iter_mut()
            .find(|put| put.room == record.room && put.hash == record.hash)
        {
            Some(kept) => {
                kept.at = kept.at.max(record.at);
                kept.len = record.len;
                kept.actor.clone_from(&record.actor);
            }
            None => kept.push(record.clone()),
        }
        let snapshot = snapshot_of(&kept)?;
        if snapshot.len() as u64 > InvocationJournal::MAX_BYTES {
            return Err("content put attribution storage limit reached".into());
        }
        self.len = snapshot.len() as u64;
        self.snapshot.clone_from(&snapshot);
        self.appended.clear();
        Ok(Staged::Replace {
            name: CONTENT_PUTS_FILE,
            bytes: snapshot,
        })
    }
}

/// The snapshot line for `puts`.
fn snapshot_of(puts: &[ContentPutRecord]) -> Result<Vec<u8>, String> {
    let mut line = serde_json::to_vec(&PutSnapshot {
        version: VERSION,
        puts: puts.to_vec(),
    })
    .map_err(|_| "content put attribution encoding failed")?;
    line.push(b'\n');
    Ok(line)
}

/// Every attribution `root`'s file holds, as a journal opened there reads
/// them: what a restart keeps.
///
/// # Errors
///
/// Returns why the file cannot be read.
#[cfg(test)]
pub(crate) fn attributions(root: &Path) -> Result<Vec<ContentPutRecord>, String> {
    ContentPutsStore::read(root)?.held()
}

impl InvocationJournal {
    /// Stage `record` for the journal's writer, after every write staged
    /// before it; its ticket resolves once the line is durable.
    ///
    /// # Errors
    ///
    /// Returns why it cannot be staged.
    pub(crate) fn record_content_put(
        &mut self,
        record: &ContentPutRecord,
        holds: &mut dyn ContentHolds,
    ) -> Result<Durable, String> {
        self.ready()?;
        let mut store = self
            .content_puts
            .take()
            .ok_or("invocation journal unavailable")?;
        let staged = store.put(record, holds);
        // The store as staged is what the next put compacts from, whether or
        // not this write is made: a compaction it makes is the file it
        // describes, and a line that failed is one the next put bumps.
        self.content_puts = Some(store);
        self.stage(staged?, Injected::default())
    }
}
