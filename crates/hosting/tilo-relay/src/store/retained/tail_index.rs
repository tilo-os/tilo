//! What a lineage's active generation holds, as far as committing a delta
//! needs it: the highest ordinal any entry reserves, and which
//! records it holds. A delta's commit used to list the whole generation for
//! those two facts, so every keystroke cost a `read_dir` and a `stat` per
//! record the document had retained.
//!
//! The index lives in this process only and is a cache, never a source of
//! truth: a full scan of the generation builds it, this process's own appends
//! keep it current, and it is used only while the generation directory's
//! identity, size and change times are the ones it was kept at. A new
//! generation (checkpoint, compaction), another writer, a restart or a hand
//! edit therefore sends the next commit back to the scan. It stores no
//! names: an incoming frame whose digest it may hold is checked by the scan,
//! so a retried frame is found exactly as before.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use tilo_lock::MutexRecover as _;

/// Records remembered over every lineage (eight bytes each, plus the set's
/// overhead: a few MiB at most). Past it, lineages are forgotten whole.
const MAX_RECORDS: usize = 262_144;
/// Lineages remembered. Past it, lineages are forgotten whole.
const MAX_LINEAGES: usize = 16_384;

/// A directory's identity, size and change times (`retained::stamp`):
/// adding, removing or renaming an entry changes them.
pub(super) type Stamp = [i64; 7];

struct Generation {
    /// The active generation's stamp the summary below describes.
    stamp: Stamp,
    /// Highest ordinal a record or continuity marker in it reserves.
    last: u64,
    /// The first eight bytes of every record's digest.
    digests: HashSet<u64>,
}

#[derive(Default)]
struct Index {
    /// By lineage directory.
    lineages: HashMap<PathBuf, Generation>,
    /// Digests held over all lineages.
    records: usize,
}

impl Index {
    fn remove(&mut self, lineage: &Path) {
        if let Some(generation) = self.lineages.remove(lineage) {
            self.records = self.records.saturating_sub(generation.digests.len());
        }
    }

    /// Forget whole lineages until `records` more digests and one more
    /// lineage fit.
    fn make_room(&mut self, records: usize) {
        while !self.lineages.is_empty()
            && (self.records.saturating_add(records) > MAX_RECORDS
                || self.lineages.len() >= MAX_LINEAGES)
        {
            let Some(victim) = self.lineages.keys().next().cloned() else {
                break;
            };
            self.remove(&victim);
        }
    }
}

fn index() -> MutexGuard<'static, Index> {
    static INDEX: OnceLock<Mutex<Index>> = OnceLock::new();
    INDEX.get_or_init(Mutex::default).lock_recover()
}

/// The part of a record digest the index keeps. A collision only sends a
/// commit to the scan.
pub(super) fn short(digest: &[u8; 32]) -> u64 {
    digest
        .first_chunk::<8>()
        .map_or(0, |prefix| u64::from_le_bytes(*prefix))
}

/// The highest ordinal in `lineage`'s active generation, when the index
/// describes that generation as it is now (`stamp`) and holds no record that
/// may equal the frame with `digest`. `None` means: scan.
pub(super) fn last(lineage: &Path, stamp: &Stamp, digest: &[u8; 32]) -> Option<u64> {
    let prefix = short(digest);
    index().lineages.get(lineage).and_then(|generation| {
        (generation.stamp == *stamp && !generation.digests.contains(&prefix))
            .then_some(generation.last)
    })
}

/// Remember what a full scan of the generation stamped `stamp` (taken before
/// the scan began) found.
pub(super) fn install(lineage: &Path, stamp: Stamp, last: u64, digests: HashSet<u64>) {
    let mut index = index();
    index.remove(lineage);
    if digests.len() > MAX_RECORDS {
        return;
    }
    index.make_room(digests.len());
    index.records = index.records.saturating_add(digests.len());
    index.lineages.insert(
        lineage.to_path_buf(),
        Generation {
            stamp,
            last,
            digests,
        },
    );
}

/// Account for the record this process just appended at `ordinal`. The
/// index follows only when it described the generation as it was before the
/// append (`before`); `after` is the generation's stamp with the record in it.
pub(super) fn appended(
    lineage: &Path,
    (before, after): (&Stamp, Stamp),
    ordinal: u64,
    digest: &[u8; 32],
) {
    let mut index = index();
    let Some(generation) = index.lineages.get_mut(lineage) else {
        return;
    };
    if generation.stamp != *before {
        index.remove(lineage);
        return;
    }
    generation.stamp = after;
    generation.last = generation.last.max(ordinal);
    if generation.digests.insert(short(digest)) {
        index.records = index.records.saturating_add(1);
        if index.records > MAX_RECORDS {
            index.remove(lineage);
        }
    }
}

/// Stop trusting what the index holds for `lineage`.
pub(super) fn forget(lineage: &Path) {
    index().remove(lineage);
}
