//! Per-inode staging of FUSE writes.
//!
//! A stage holds what writers changed, not a copy of the file. Bytes that
//! were written (or later filled in) live in a backing: a sparse file in the
//! mount's stage directory, or memory for documents and for mounts without
//! one. Every other byte below the stage's length is the committed content
//! the stage was opened over (its base) at the same offset.
//!
//! Opening a stage, writing to it and truncating it therefore never read the
//! base, so they never wait on a peer. Only a read of a range nobody wrote
//! and the commit, which needs every byte, read the base, and only the
//! ranges that are still missing.
//!
//! Invariant: every byte below `len` that is not in `valid` lies below the
//! base's length. Truncation removes ranges; growing the stage (a write past
//! the end, or a larger `set_len`) marks the new bytes valid, and the backing
//! reads zeros there.

use std::borrow::Cow;
use std::io;
use std::ops::Range;
use std::os::unix::fs::FileExt as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use iroh_blobs::Hash;

use super::VersionSource;
use crate::content::RangeSet;
use crate::hydration::Hydrator;
use tilo_lock::MutexRecover as _;

/// File names of stage files, so a restart can remove what a crash left.
pub(super) const STAGE_FILE_PREFIX: &str = "stage-";

/// One inode's stage, shared by every FUSE worker that touches the inode.
pub(super) type SharedStage = Arc<Stage>;

/// One inode's [`WriteStage`] behind its own lock.
pub struct Stage {
    inner: Mutex<WriteStage>,
    /// The stage's length, readable while a commit holds the lock, so
    /// `stat` of a file being committed does not wait for the commit.
    len: AtomicU64,
}

impl Stage {
    /// Lock the stage. A poisoned stage still holds the bytes writers were
    /// acknowledged for, so it is used rather than dropped.
    pub(super) fn lock(&self) -> MutexGuard<'_, WriteStage> {
        self.inner.lock_recover()
    }

    pub(super) fn len(&self) -> u64 {
        self.len.load(Ordering::Acquire)
    }

    /// Publish the length of `stage`, this stage's locked contents, after a
    /// write or truncation changed it.
    pub(super) fn note_len(&self, stage: &WriteStage) {
        self.len.store(stage.len, Ordering::Release);
    }
}

/// The writes staged for one inode and the content they apply to.
pub struct WriteStage {
    backing: Backing,
    len: u64,
    /// Bytes of `backing` that hold the file's content.
    valid: RangeSet,
    /// The committed content the stage was opened over.
    base: Option<Hash>,
}

/// Shared by all memory stages of one mount. Reservations include deferred
/// zero filling and stay charged until the backing drops: truncating a Vec
/// does not release its allocation. No inode or global map lock is needed.
#[derive(Default)]
pub(super) struct MemoryBudget {
    reserved: AtomicU64,
}

struct MemoryBacking {
    bytes: Vec<u8>,
    budget: Arc<MemoryBudget>,
    reserved: u64,
}

impl MemoryBacking {
    fn reserve(&mut self, len: u64) -> io::Result<()> {
        super::check_staged_size(len).map_err(io::Error::from_raw_os_error)?;
        let growth = len.saturating_sub(self.reserved);
        if growth == 0 {
            return Ok(());
        }
        let mut total = self.budget.reserved.load(Ordering::Acquire);
        loop {
            super::check_staged_growth(total, growth).map_err(io::Error::from_raw_os_error)?;
            match self.budget.reserved.compare_exchange_weak(
                total,
                total.saturating_add(growth),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.reserved = len;
                    return Ok(());
                }
                Err(current) => total = current,
            }
        }
    }

    fn resize(&mut self, len: u64) -> io::Result<()> {
        let old_reservation = self.reserved;
        self.reserve(len)?;
        let result = resize_bytes(&mut self.bytes, len);
        if result.is_err() {
            self.budget.reserved.fetch_sub(
                self.reserved.saturating_sub(old_reservation),
                Ordering::AcqRel,
            );
            self.reserved = old_reservation;
        }
        result
    }
}

impl Drop for MemoryBacking {
    fn drop(&mut self) {
        // Free before returning the reservation to concurrent writers.
        drop(std::mem::take(&mut self.bytes));
        self.budget
            .reserved
            .fetch_sub(self.reserved, Ordering::AcqRel);
    }
}

fn resize_bytes(bytes: &mut Vec<u8>, len: u64) -> io::Result<()> {
    let len = usize::try_from(len).map_err(|_| io::Error::from_raw_os_error(libc::EFBIG))?;
    if len > bytes.len() {
        bytes
            .try_reserve_exact(len.saturating_sub(bytes.len()))
            .map_err(|_| io::Error::from_raw_os_error(libc::ENOSPC))?;
    }
    bytes.resize(len, 0);
    Ok(())
}

enum Backing {
    /// Bytes at their file offsets; shorter than `len` when the tail is
    /// zeros or still the base's.
    Memory(MemoryBacking),
    /// A private, sparse file removed when the stage is dropped.
    File(tempfile::NamedTempFile),
}

impl WriteStage {
    pub(super) fn empty(budget: Arc<MemoryBudget>) -> Self {
        Self {
            backing: Backing::Memory(MemoryBacking {
                bytes: Vec::new(),
                budget,
                reserved: 0,
            }),
            len: 0,
            valid: RangeSet::new(),
            base: None,
        }
    }

    /// A stage holding all of `bytes` (a document's export, a new file).
    pub(super) fn complete(
        bytes: &[u8],
        base: Option<Hash>,
        budget: Arc<MemoryBudget>,
    ) -> io::Result<Self> {
        let mut stage = Self::empty(budget);
        stage.write(0, bytes)?;
        stage.base = base;
        Ok(stage)
    }

    /// A stage over `base_len` bytes of committed content `base`, of which
    /// nothing is read yet. With `dir` the backing is a sparse file there.
    ///
    /// # Errors
    ///
    /// Returns an error when the stage file cannot be created.
    pub(super) fn over(
        base: Option<Hash>,
        base_len: u64,
        dir: Option<&Path>,
        budget: Arc<MemoryBudget>,
    ) -> io::Result<Self> {
        let len = if base.is_some() { base_len } else { 0 };
        let backing = match dir {
            Some(dir) => {
                let file = tempfile::Builder::new()
                    .prefix(STAGE_FILE_PREFIX)
                    .tempfile_in(dir)?;
                file.as_file().set_len(len)?;
                Backing::File(file)
            }
            None => Backing::Memory(MemoryBacking {
                bytes: Vec::new(),
                budget,
                reserved: 0,
            }),
        };
        Ok(Self {
            backing,
            len,
            valid: RangeSet::new(),
            base,
        })
    }

    pub(super) fn shared(self) -> SharedStage {
        Arc::new(Stage {
            len: AtomicU64::new(self.len),
            inner: Mutex::new(self),
        })
    }

    #[cfg(test)]
    pub(super) const fn len(&self) -> u64 {
        self.len
    }

    pub(super) const fn base(&self) -> Option<Hash> {
        self.base
    }

    /// Whether the stage keeps its bytes in a file rather than in memory.
    #[cfg(test)]
    pub(super) const fn is_file_backed(&self) -> bool {
        matches!(self.backing, Backing::File(_))
    }

    /// Ranges below `len` that are still the base's.
    pub(super) fn gaps(&self) -> Vec<Range<u64>> {
        self.valid.missing(&(0..self.len))
    }

    fn gaps_in(&self, range: &Range<u64>) -> Vec<Range<u64>> {
        self.valid.missing(range)
    }

    /// Whether every byte is in the backing, so the stage can be committed.
    pub(super) fn is_complete(&self) -> bool {
        self.gaps().is_empty()
    }

    pub(super) fn write(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        let previous_reservation = if let Backing::Memory(memory) = &mut self.backing {
            // Even a small write into a huge base eventually materializes the
            // whole memory stage at commit. Refuse it before allocating.
            let previous = memory.reserved;
            memory.reserve(self.len.max(end))?;
            Some(previous)
        } else {
            None
        };
        if let Err(error) = self.put(offset, data) {
            if let (Backing::Memory(memory), Some(previous)) =
                (&mut self.backing, previous_reservation)
            {
                memory
                    .budget
                    .reserved
                    .fetch_sub(memory.reserved.saturating_sub(previous), Ordering::AcqRel);
                memory.reserved = previous;
            }
            return Err(error);
        }
        if offset > self.len {
            // The hole between the old end and the write reads as zeros.
            self.valid.insert(self.len..offset);
        }
        self.valid.insert(offset..end);
        self.len = self.len.max(end);
        Ok(())
    }

    /// Truncate or extend to `len` bytes; an extension reads as zeros.
    pub(super) fn set_len(&mut self, len: u64) -> io::Result<()> {
        if let Backing::Memory(memory) = &mut self.backing {
            memory.reserve(len)?;
        }
        if len < self.len {
            match &mut self.backing {
                Backing::Memory(memory) => {
                    memory
                        .bytes
                        .truncate(usize::try_from(len).unwrap_or(usize::MAX));
                }
                Backing::File(file) => file.as_file().set_len(len)?,
            }
            self.valid.truncate(len);
        } else if len > self.len {
            if let Backing::File(file) = &self.backing {
                file.as_file().set_len(len)?;
            }
            self.valid.insert(self.len..len);
        }
        self.len = len;
        Ok(())
    }

    /// Store `bytes`, read from the base at `offset`, where the stage still
    /// lacks them. Ranges written or truncated away meanwhile are skipped.
    pub(super) fn fill(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        if let Backing::Memory(memory) = &mut self.backing {
            memory.reserve(self.len)?;
        }
        let end = offset.saturating_add(bytes.len() as u64).min(self.len);
        for gap in self.gaps_in(&(offset.min(end)..end)) {
            let from = usize::try_from(gap.start.saturating_sub(offset)).unwrap_or(usize::MAX);
            let to = usize::try_from(gap.end.saturating_sub(offset)).unwrap_or(usize::MAX);
            let part = bytes
                .get(from..to)
                .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
            self.put(gap.start, part)?;
            self.valid.insert(gap);
        }
        Ok(())
    }

    fn put(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        match &mut self.backing {
            Backing::Memory(memory) => {
                let start = usize::try_from(offset)
                    .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
                let end = start
                    .checked_add(data.len())
                    .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
                if memory.bytes.len() < end {
                    memory.resize(end as u64)?;
                }
                memory
                    .bytes
                    .get_mut(start..end)
                    .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?
                    .copy_from_slice(data);
                Ok(())
            }
            Backing::File(file) => file.as_file().write_all_at(data, offset),
        }
    }

    /// The backing's bytes in `range` (clipped to `len`); gaps read as zeros
    /// and are the caller's to fill from the base.
    fn read_backing(&self, range: &Range<u64>) -> io::Result<Vec<u8>> {
        let end = range.end.min(self.len);
        let start = range.start.min(end);
        let want = usize::try_from(end.saturating_sub(start))
            .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        let mut out = Vec::new();
        resize_bytes(&mut out, want as u64)?;
        match &self.backing {
            Backing::Memory(memory) => {
                let bytes = &memory.bytes;
                let from = usize::try_from(start)
                    .unwrap_or(usize::MAX)
                    .min(bytes.len());
                let to = from.saturating_add(want).min(bytes.len());
                if let (Some(src), Some(dst)) =
                    (bytes.get(from..to), out.get_mut(..to.saturating_sub(from)))
                {
                    dst.copy_from_slice(src);
                }
            }
            Backing::File(file) => {
                let mut done = 0_usize;
                while let Some(rest) = out.get_mut(done..).filter(|rest| !rest.is_empty()) {
                    // A file shorter than `len` reads as zeros past its end.
                    match file
                        .as_file()
                        .read_at(rest, start.saturating_add(done as u64))
                    {
                        Ok(0) => break,
                        Ok(n) => done = done.saturating_add(n),
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        Ok(out)
    }

    /// Up to `size` bytes from `offset`: what the stage holds, with the ranges
    /// that are still the base's listed for the caller to read.
    pub(super) fn read(&self, offset: u64, size: u64) -> io::Result<StagedRead> {
        let end = offset.saturating_add(size).min(self.len);
        let start = offset.min(end);
        let bytes = self.read_backing(&(start..end))?;
        let gaps = self.gaps_in(&(start..end));
        if gaps.is_empty() {
            return Ok(StagedRead::Ready(bytes));
        }
        let base = self
            .base
            .ok_or_else(|| io::Error::other("staged file lacks bytes but has no base content"))?;
        Ok(StagedRead::Missing(MissingRanges {
            bytes,
            start,
            base,
            gaps,
        }))
    }

    /// The complete content as a version source; the stage must be complete.
    pub(super) fn source(&mut self) -> io::Result<VersionSource<'_>> {
        if !self.is_complete() {
            return Err(io::Error::other("staged file still lacks base content"));
        }
        let len = self.len;
        match &mut self.backing {
            Backing::Memory(memory) => {
                memory.resize(len)?;
                Ok(VersionSource::Bytes(&memory.bytes))
            }
            Backing::File(file) => {
                file.as_file().set_len(len)?;
                Ok(VersionSource::File {
                    path: file.path(),
                    manifest: None,
                })
            }
        }
    }

    /// The complete content in one buffer (documents are imported whole).
    ///
    /// # Errors
    ///
    /// As [`Self::source`], or when the file cannot be read.
    pub(super) fn contiguous(&mut self) -> io::Result<Cow<'_, [u8]>> {
        match self.source()? {
            VersionSource::Bytes(bytes) => Ok(Cow::Borrowed(bytes)),
            VersionSource::File { path, .. } => std::fs::read(path).map(Cow::Owned),
        }
    }

    /// The blake3 hash of the complete content, read in bounded memory.
    ///
    /// # Errors
    ///
    /// As [`Self::source`], or when the file cannot be read.
    pub(super) fn digest(&mut self) -> io::Result<blake3::Hash> {
        match self.source()? {
            VersionSource::Bytes(bytes) => Ok(blake3::hash(bytes)),
            VersionSource::File { path, .. } => {
                crate::unsaved::file_digest(path).map(|(digest, _)| digest)
            }
        }
    }

    /// The first `size` bytes, which must be in the backing.
    pub(super) fn prefix(&self, size: u64) -> io::Result<Vec<u8>> {
        match self.read(0, size)? {
            StagedRead::Ready(bytes) => Ok(bytes),
            StagedRead::Missing(_) => Err(io::Error::other("staged prefix is incomplete")),
        }
    }
}

pub(super) enum StagedRead {
    /// Every byte was staged (or local base content).
    Ready(Vec<u8>),
    /// Some ranges must still be read from the base.
    Missing(MissingRanges),
}

/// A read of a stage whose bytes are partly the base's.
pub(super) struct MissingRanges {
    bytes: Vec<u8>,
    start: u64,
    base: Hash,
    gaps: Vec<Range<u64>>,
}

impl MissingRanges {
    fn place(&mut self, gap: &Range<u64>, part: &[u8]) -> io::Result<()> {
        let from = usize::try_from(gap.start.saturating_sub(self.start)).unwrap_or(usize::MAX);
        let to = usize::try_from(gap.end.saturating_sub(self.start)).unwrap_or(usize::MAX);
        let dst = self
            .bytes
            .get_mut(from..to)
            .filter(|dst| dst.len() == part.len())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "committed content is shorter than the file it backs",
                )
            })?;
        dst.copy_from_slice(part);
        Ok(())
    }

    /// The bytes, when every missing range is local. Never touches the
    /// network; `Err(self)` when a range would have to come from a peer.
    pub(super) async fn finish_local(mut self, hydrator: &Hydrator) -> Result<Vec<u8>, Self> {
        let mut parts = Vec::with_capacity(self.gaps.len());
        for gap in &self.gaps {
            let len = gap.end.saturating_sub(gap.start);
            match hydrator.read_local(self.base, gap.start, len).await {
                Some(part) => parts.push(part),
                None => return Err(self),
            }
        }
        let gaps = std::mem::take(&mut self.gaps);
        for (gap, part) in gaps.iter().zip(&parts) {
            if self.place(gap, part).is_err() {
                self.gaps = gaps;
                return Err(self);
            }
        }
        Ok(self.bytes)
    }

    /// The bytes, reading the missing ranges from the base (from a peer when
    /// they are not local).
    ///
    /// # Errors
    ///
    /// Returns an error when a range cannot be hydrated.
    pub(super) async fn finish(mut self, hydrator: &Hydrator) -> io::Result<Vec<u8>> {
        let gaps = std::mem::take(&mut self.gaps);
        for gap in &gaps {
            let len = gap.end.saturating_sub(gap.start);
            let part = hydrator
                .read(self.base, gap.start, len)
                .await
                .map_err(|error| io::Error::other(error.to_string()))?;
            self.place(gap, &part)?;
        }
        Ok(self.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes_of(stage: &mut WriteStage) -> Vec<u8> {
        stage.contiguous().unwrap().into_owned()
    }

    #[test]
    fn concurrent_memory_stages_share_one_budget_and_charge_document_copies() {
        let budget = Arc::new(MemoryBudget::default());
        let stages = std::thread::scope(|scope| {
            let workers: [_; 3] = std::array::from_fn(|_| {
                let budget = Arc::clone(&budget);
                scope.spawn(move || {
                    let mut stage = WriteStage::over(None, 0, None, budget).unwrap();
                    let result = stage.set_len(super::super::MAX_STAGED_FILE_BYTES);
                    (stage, result)
                })
            });
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            stages.iter().filter(|(_, result)| result.is_ok()).count(),
            2
        );
        let failure = stages
            .iter()
            .find_map(|(_, result)| result.as_ref().err())
            .unwrap();
        assert_eq!(failure.raw_os_error(), Some(libc::ENOSPC));
        assert_eq!(
            budget.reserved.load(Ordering::Acquire),
            super::super::MAX_STAGED_TOTAL_BYTES
        );
        assert_eq!(
            WriteStage::complete(b"document export", None, Arc::clone(&budget))
                .err()
                .unwrap()
                .raw_os_error(),
            Some(libc::ENOSPC),
        );
        drop(stages);
        assert_eq!(budget.reserved.load(Ordering::Acquire), 0);
        let mut document = WriteStage::complete(b"export", None, budget).unwrap();
        assert_eq!(bytes_of(&mut document), b"export");
    }

    #[test]
    fn filling_or_writing_a_huge_memory_base_fails_before_allocating() {
        let budget = Arc::new(MemoryBudget::default());
        let mut stage = WriteStage::over(
            Some(Hash::from([3; 32])),
            super::super::MAX_STAGED_FILE_BYTES.saturating_add(1),
            None,
            Arc::clone(&budget),
        )
        .unwrap();
        assert_eq!(
            stage.write(0, b"x").unwrap_err().raw_os_error(),
            Some(libc::EFBIG)
        );
        assert_eq!(
            stage.fill(0, b"x").unwrap_err().raw_os_error(),
            Some(libc::EFBIG)
        );
        assert_eq!(budget.reserved.load(Ordering::Acquire), 0);
        stage.set_len(1).unwrap();
        stage.fill(0, b"x").unwrap();
        assert_eq!(bytes_of(&mut stage), b"x");
    }

    #[test]
    fn a_stage_tracks_written_holes_truncation_and_base_ranges_in_both_backings() {
        let dir = tempfile::tempdir().unwrap();
        for file_backed in [false, true] {
            let base = Hash::from([3; 32]);
            let mut stage = WriteStage::over(
                Some(base),
                10,
                file_backed.then(|| dir.path()),
                Arc::new(MemoryBudget::default()),
            )
            .unwrap();
            assert_eq!(stage.is_file_backed(), file_backed);
            assert_eq!(stage.gaps(), vec![0..10]);
            stage.write(4, b"ab").unwrap();
            assert_eq!(stage.gaps(), vec![0..4, 6..10]);
            // An append past the end leaves a zero hole that is not the base's.
            stage.write(12, b"Z").unwrap();
            assert_eq!(stage.len(), 13);
            assert_eq!(stage.gaps(), vec![0..4, 6..10]);
            // A truncation drops the base's tail for good; growing again reads zeros.
            stage.set_len(8).unwrap();
            stage.set_len(11).unwrap();
            assert_eq!(stage.gaps(), vec![0..4, 6..8]);
            let StagedRead::Missing(missing) = stage.read(0, 100).unwrap() else {
                panic!("base ranges are missing");
            };
            assert_eq!(missing.gaps, vec![0..4, 6..8]);
            assert_eq!(missing.bytes.len(), 11);
            assert!(stage.source().is_err(), "an incomplete stage never commits");
            stage.write(0, b"W").unwrap();
            stage.fill(0, b"basebase").unwrap();
            assert!(stage.is_complete());
            assert_eq!(bytes_of(&mut stage), b"Waseabse\0\0\0");
        }
    }
}
