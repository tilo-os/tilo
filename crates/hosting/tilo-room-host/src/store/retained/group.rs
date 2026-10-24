//! Group commit: the records a workspace owner appended since its last
//! barrier, made durable together.
//!
//! A delta's record is written beside its final name, unsynced, and joins
//! the owner's group ([`CommitGroup::defer`]). Nothing reads it there: every
//! listing skips dot-files, and the tail index alone tells the next append
//! of its lineage which ordinal comes next. [`CommitGroup::flush`] then
//! syncs every staged record at once, renames them into place in append
//! order and syncs each generation directory once: one round of file
//! barriers, issued together so they share the file system's journal
//! commit, and one directory barrier per generation, however many records
//! the group holds. A crash before the renames leaves only dot-files no
//! reader sees; after them, every record is whole.
//!
//! The owner flushes before it acknowledges or fans out any record of the
//! group, and before any other barrier ([`tilo_relay_store::with_barrier_hook`]):
//! nothing made durable later can depend on a record that is not.
use super::{StoreError, generation_stamp, remember_digest, sync_dir, tail_index};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tilo_lock::MutexRecover as _;

/// Most file barriers one flush issues at once.
const PARALLEL_BARRIERS: usize = 8;

/// One staged record.
struct Staged {
    /// Where it is written, unsynced: a dot-file no listing reads.
    staging: PathBuf,
    file: fs::File,
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
            file,
            path,
            lineage: lineage.to_owned(),
            generation: generation.to_owned(),
            digest,
        });
        Ok(())
    }

    /// Make every staged record durable under its name: sync the staged
    /// files together, rename them in append order, then sync each
    /// generation directory once.
    ///
    /// # Errors
    ///
    /// Returns the first barrier or rename that failed. Records not yet
    /// renamed are removed; what was renamed stays, as a record whose
    /// directory barrier failed always did.
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
    sync_files(staged)?;
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
    for record in staged {
        fs::rename(&record.staging, &record.path)?;
        remember_digest(&record.path, &record.digest);
    }
    // The index followed each staged append; it follows the renames too.
    for (generation, lineage, before) in &generations {
        match (before, generation_stamp(generation)) {
            (Some(before), Some(after)) => tail_index::restamp(lineage, before, after),
            _ => tail_index::forget(lineage),
        }
    }
    let mut directories: Vec<&mut (dyn FnMut() -> Result<(), StoreError> + Send)> = Vec::new();
    let mut syncs: Vec<_> = generations
        .iter()
        .map(|(generation, ..)| move || sync_dir(generation))
        .collect();
    for sync in &mut syncs {
        directories.push(sync);
    }
    super::barriers(&mut directories)
}

/// Sync every staged file, at most [`PARALLEL_BARRIERS`] at a time.
fn sync_files(staged: &[Staged]) -> Result<(), StoreError> {
    let sync = |record: &Staged| -> Result<(), StoreError> {
        #[cfg(test)]
        super::super::test_barrier::check(&record.lineage, "group-sync-file")?;
        crate::store::fsync(&record.file)?;
        Ok(())
    };
    let chunk = staged.len().div_ceil(PARALLEL_BARRIERS).max(1);
    let mut work: Vec<_> = staged
        .chunks(chunk)
        .map(|records| move || records.iter().try_for_each(sync))
        .collect();
    let mut barriers: Vec<&mut (dyn FnMut() -> Result<(), StoreError> + Send)> = Vec::new();
    for barrier in &mut work {
        barriers.push(barrier);
    }
    super::barriers(&mut barriers)
}
