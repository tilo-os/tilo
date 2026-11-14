//! Group commit: the records a workspace owner appended since its last
//! barrier, made durable together.
//!
//! A delta's record is written beside its final name, unsynced, and joins
//! the owner's group ([`CommitGroup::defer`]). Nothing reads it there: every
//! listing skips dot-files, and the tail index alone tells the next append
//! of its lineage which ordinal comes next. [`CommitGroup::flush`] then
//! appends every staged record to its generation's journal and syncs it
//! once, and only then renames the records into place in append order: one
//! barrier per generation, however many records the group holds, and none
//! for the records' own files or the directory entries their renames add.
//! The journal holds them through a crash ([`super::journal`]).
//!
//! The owner flushes before it acknowledges or fans out any record of the
//! group, and before any other barrier ([`tilo_relay_store::with_barrier_hook`]):
//! nothing made durable later can depend on a record that is not.
use super::{StoreError, generation_stamp, journal, remember_digest, tail_index};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tilo_lock::MutexRecover as _;

/// One staged record.
struct Staged {
    /// Where it is written, unsynced: a dot-file no listing reads.
    staging: PathBuf,
    bytes: Vec<u8>,
    /// Its name once durable.
    path: PathBuf,
    lineage: PathBuf,
    generation: PathBuf,
    digest: [u8; 32],
}

/// The records a workspace owner appended whose barriers are still due.
#[derive(Default)]
pub(crate) struct CommitGroup {
    staged: Vec<Staged>,
    /// Flushes that issued barriers, for tests that count them.
    #[cfg(test)]
    flushes: usize,
}

/// An owner's group, shared with the retained builds of its turns.
#[derive(Clone, Default)]
pub(crate) struct SharedGroup(Arc<Mutex<CommitGroup>>);

impl SharedGroup {
    /// How many flushes issued barriers.
    #[cfg(test)]
    pub(crate) fn flushes(&self) -> usize {
        self.0.lock_recover().flushes
    }

    /// Whether records wait for their barriers.
    pub(crate) fn pending(&self) -> bool {
        !self.0.lock_recover().staged.is_empty()
    }

    /// Whether a record of `lineage` waits for its barriers.
    pub(super) fn holds(&self, lineage: &Path) -> bool {
        self.0
            .lock_recover()
            .staged
            .iter()
            .any(|staged| staged.lineage == lineage)
    }

    /// Write `bytes` as the record at `path` of `lineage`'s active
    /// `generation`, whose barriers the next [`Self::flush`] issues.
    pub(super) fn defer(
        &self,
        lineage: &Path,
        generation: &Path,
        path: PathBuf,
        bytes: &[u8],
        digest: [u8; 32],
    ) -> Result<(), StoreError> {
        use std::io::Write as _;
        let name = path
            .file_name()
            .ok_or_else(|| StoreError::Io(std::io::Error::other("retained record name")))?
            .to_string_lossy()
            .into_owned();
        // Never ending in `.delta`: no listing takes it for a record.
        let staging = generation.join(format!(".{name}.{}.group", std::process::id()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&staging)?;
        file.write_all(bytes)?;
        self.0.lock_recover().staged.push(Staged {
            staging,
            bytes: bytes.to_vec(),
            path,
            lineage: lineage.to_owned(),
            generation: generation.to_owned(),
            digest,
        });
        Ok(())
    }

    /// Make every staged record durable under its name: append them to
    /// their generations' journals, sync each journal once, then rename the
    /// records into place in append order.
    ///
    /// # Errors
    ///
    /// Returns the first barrier or rename that failed. Records not yet
    /// renamed are removed; what a journal already holds may still be
    /// restored after a crash, as a record whose barrier failed after its
    /// rename always could be.
    pub(crate) fn flush(&self) -> Result<(), StoreError> {
        let staged = {
            let mut group = self.0.lock_recover();
            #[cfg(test)]
            if !group.staged.is_empty() {
                group.flushes = group.flushes.saturating_add(1);
            }
            std::mem::take(&mut group.staged)
        };
        if staged.is_empty() {
            return Ok(());
        }
        let result = publish(&staged);
        if result.is_err() {
            for record in &staged {
                let _ = fs::remove_file(&record.staging);
                tail_index::forget(&record.lineage);
            }
        }
        result
    }
}

fn publish(staged: &[Staged]) -> Result<(), StoreError> {
    let mut generations: Vec<(&Path, &Path, Option<tail_index::Stamp>)> = Vec::new();
    for record in staged {
        if !generations
            .iter()
            .any(|(generation, ..)| *generation == record.generation)
        {
            generations.push((
                &record.generation,
                &record.lineage,
                generation_stamp(&record.generation),
            ));
        }
    }
    journal_all(staged, &generations)?;
    for record in staged {
        fs::rename(&record.staging, &record.path)?;
        remember_digest(&record.path, &record.digest);
        tilo_relay_store::durable::journaled(&record.path);
    }
    // The index followed each staged append; it follows the renames and a
    // journal's creation too.
    for (generation, lineage, before) in &generations {
        match (before, generation_stamp(generation)) {
            (Some(before), Some(after)) => tail_index::restamp(lineage, before, after),
            _ => tail_index::forget(lineage),
        }
    }
    Ok(())
}

/// Append each generation's staged records to its journal, the journals'
/// barriers issued together.
fn journal_all(
    staged: &[Staged],
    generations: &[(&Path, &Path, Option<tail_index::Stamp>)],
) -> Result<(), StoreError> {
    let mut appends = Vec::new();
    for (generation, ..) in generations {
        let mut entries = Vec::new();
        for record in staged
            .iter()
            .filter(|record| record.generation == *generation)
        {
            entries.push(journal::Entry {
                name: record
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| StoreError::Io(std::io::Error::other("retained record name")))?,
                bytes: &record.bytes,
                digest: &record.digest,
            });
        }
        appends.push((*generation, journal::encode(&entries)?));
    }
    let mut work: Vec<_> = appends
        .iter()
        .map(|(generation, bytes)| move || journal::append(generation, bytes))
        .collect();
    let mut barriers: Vec<&mut (dyn FnMut() -> Result<(), StoreError> + Send)> = Vec::new();
    for barrier in &mut work {
        barriers.push(barrier);
    }
    super::barriers(&mut barriers)
}
