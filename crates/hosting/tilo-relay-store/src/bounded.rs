//! Disk-spilled metadata ordering shared by replay and retention.
use super::StoreError;
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs::{self, File},
    io::{self, Read, Write},
    path::PathBuf,
};

pub const RUN_ENTRIES: usize = 128;
pub const RUN_BYTES: usize = 256 * 1024;
pub const MERGE_FAN_IN: usize = 8;
pub const MAX_METADATA: usize = 16 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct WorkLimit {
    pub entries: usize,
    pub metadata_bytes: usize,
}
impl Default for WorkLimit {
    fn default() -> Self {
        Self {
            entries: 128,
            metadata_bytes: RUN_BYTES,
        }
    }
}
impl WorkLimit {
    /// A metadata decode may use its entire validated record bound. Charging
    /// that bound avoids overrunning the caller's byte quantum on lookahead.
    pub fn record_entries(self) -> usize {
        self.entries.min(self.metadata_bytes / MAX_METADATA)
    }
    /// This limit, refused when it allows no work or more than the store's
    /// bounds.
    pub fn checked(self) -> Result<Self, StoreError> {
        if self.entries == 0 || self.metadata_bytes < MAX_METADATA {
            return Err(invalid("invalid work quantum"));
        }
        Ok(Self {
            entries: self.entries.min(RUN_ENTRIES),
            metadata_bytes: self.metadata_bytes.min(RUN_BYTES),
        })
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Step<T> {
    More,
    Done(T),
}
pub fn invalid(message: &str) -> StoreError {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}
/// Turn a broken step-machine invariant into a storage error instead of a
/// panic: the owner fails the operation and keeps serving other workspaces.
pub trait Invariant<T> {
    /// The value, or an `InvalidData` storage error naming what was missing.
    fn invariant(self, what: &str) -> Result<T, StoreError>;
}
fn broken(what: &str) -> StoreError {
    invalid(&format!("storage step invariant broken: {what}"))
}
impl<T> Invariant<T> for Option<T> {
    fn invariant(self, what: &str) -> Result<T, StoreError> {
        self.ok_or_else(|| broken(what))
    }
}
impl<T, E> Invariant<T> for Result<T, E> {
    fn invariant(self, what: &str) -> Result<T, StoreError> {
        self.map_err(|_| broken(what))
    }
}
/// Add one to a work counter.
pub fn increment(value: &mut u64) -> Result<(), StoreError> {
    *value = value
        .checked_add(1)
        .ok_or_else(|| invalid("storage counter exhausted"))?;
    Ok(())
}

#[derive(Default, Debug, Clone)]
pub struct WorkStats {
    pub staging_full_flushes: u64,
    pub peak_metadata_entries: usize,
    pub peak_metadata_bytes: usize,
    pub peak_open_files: usize,
    pub last_step_work: usize,
    pub preparation_reads: u64,
    pub hydration_reads: u64,
    pub peak_frame_bytes: usize,
}
impl WorkStats {
    pub fn metadata(&mut self, entries: usize, bytes: usize, handles: usize) {
        self.peak_metadata_entries = self.peak_metadata_entries.max(entries);
        self.peak_metadata_bytes = self.peak_metadata_bytes.max(bytes);
        self.peak_open_files = self.peak_open_files.max(handles);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    #[serde(deserialize_with = "bounded_keys")]
    pub key: Vec<OsString>,
    #[serde(with = "os_path")]
    pub source: PathBuf,
    pub pin: u64,
    pub len: u64,
    pub pending: bool,
}
fn bounded_keys<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<Vec<OsString>, D::Error> {
    struct Keys;
    impl<'de> serde::de::Visitor<'de> for Keys {
        type Value = Vec<OsString>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("at most four filesystem key components")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = Vec::with_capacity(4);
            for _ in 0..4 {
                match seq.next_element::<OsString>()? {
                    Some(value) => out.push(value),
                    None => return Ok(out),
                }
            }
            if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                return Err(serde::de::Error::custom("too many key components"));
            }
            Ok(out)
        }
    }
    decoder.deserialize_seq(Keys)
}
mod os_path {
    use super::{Deserialize, OsString, PathBuf, Serialize};
    pub fn serialize<S: serde::Serializer>(
        path: &std::path::Path,
        encoder: S,
    ) -> Result<S::Ok, S::Error> {
        path.as_os_str().serialize(encoder)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<PathBuf, D::Error> {
        OsString::deserialize(decoder).map(PathBuf::from)
    }
}
impl Meta {
    pub fn charge(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(
                self.key
                    .capacity()
                    .saturating_mul(std::mem::size_of::<OsString>()),
            )
            .saturating_add(self.key.iter().map(OsString::capacity).sum::<usize>())
            .saturating_add(self.source.capacity())
    }
    fn validate(&self) -> Result<(), StoreError> {
        if self.key.len() > 4
            || self.key.iter().any(|part| part.len() > 255)
            || self.source.as_os_str().len() > 4096
            || self.source.components().count() > 8
            || self
                .source
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(invalid("invalid storage metadata path"));
        }
        Ok(())
    }
}
/// Append one length-prefixed metadata record to `file`.
pub fn write_meta(file: &mut (impl Write + ?Sized), meta: &Meta) -> Result<(), StoreError> {
    meta.validate()?;
    let bytes = serde_json::to_vec(meta).map_err(|e| invalid(&e.to_string()))?;
    if bytes.len() > MAX_METADATA {
        return Err(invalid("oversized storage metadata"));
    }
    let len = u32::try_from(bytes.len()).map_err(|_| invalid("oversized storage metadata"))?;
    file.write_all(&len.to_le_bytes())?;
    file.write_all(&bytes)?;
    Ok(())
}
/// The next metadata record of `file`, `None` at its end.
pub fn read_meta(file: &mut File) -> Result<Option<Meta>, StoreError> {
    let mut prefix = [0; 4];
    if file.read(&mut prefix[..1])? == 0 {
        return Ok(None);
    }
    file.read_exact(&mut prefix[1..])?;
    let len = u32::from_le_bytes(prefix) as usize;
    if len > MAX_METADATA {
        return Err(invalid("oversized storage metadata"));
    }
    let mut bytes = vec![0; len];
    file.read_exact(&mut bytes)?;
    let meta: Meta = serde_json::from_slice(&bytes).map_err(|e| invalid(&e.to_string()))?;
    meta.validate()?;
    Ok(Some(meta))
}

/// One-entry acknowledgement cursor for destructive scans. Failed metadata,
/// child-open or unlink operations retain the exact entry for the next quantum.
/// An iterator error has no entry identity, so retry reopens this directory and
/// scans its remaining contents in bounded quanta instead of skipping the error.
pub struct DirectoryCursor {
    path: PathBuf,
    entries: Option<fs::ReadDir>,
    pending: Option<fs::DirEntry>,
}
impl DirectoryCursor {
    /// A cursor over the entries of the directory `path`.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be listed.
    pub fn open(path: &std::path::Path) -> Result<Self, StoreError> {
        Ok(Self {
            path: path.into(),
            entries: Some(fs::read_dir(path)?),
            pending: None,
        })
    }
    /// The current entry, `None` past the last.
    pub fn peek(&mut self) -> Result<Option<&fs::DirEntry>, StoreError> {
        if self.pending.is_none() {
            if self.entries.is_none() {
                self.entries = Some(fs::read_dir(&self.path)?);
            }
            match self.entries.as_mut().invariant("entries missing")?.next() {
                Some(Ok(entry)) => self.pending = Some(entry),
                Some(Err(error)) => {
                    self.entries = None;
                    return Err(error.into());
                }
                None => return Ok(None),
            }
        }
        Ok(self.pending.as_ref())
    }
    pub fn advance(&mut self) {
        self.pending = None;
    }
}

struct Merge {
    inputs: Vec<(File, Option<Meta>)>,
    output: File,
    first: u64,
    count: u64,
}
pub struct Sorter {
    root: PathBuf,
    buffer: Vec<Meta>,
    bytes: usize,
    runs: u64,
    spill: Option<File>,
    pending: Option<Meta>,
    pass: u64,
    next: u64,
    output_runs: u64,
    merge: Option<Merge>,
    cleaning: Option<(u64, u64)>,
    sealed: bool,
    pub stats: WorkStats,
}
impl Sorter {
    /// A sorter spilling its runs under `root`, which it creates.
    ///
    /// # Errors
    ///
    /// Returns an error when `root` cannot be created.
    pub fn new(root: PathBuf) -> Result<Self, StoreError> {
        fs::create_dir(&root)?;
        let mut sorter = Self::unspilled(root);
        sorter.buffer.reserve_exact(RUN_ENTRIES);
        Ok(sorter)
    }
    /// A sorter that has created nothing on disk, for a pass that pushes no
    /// metadata (a delta's retention). Spilling a run fails: its directory
    /// does not exist.
    #[must_use]
    pub fn unspilled(root: PathBuf) -> Self {
        Self {
            root,
            buffer: Vec::new(),
            bytes: 0,
            runs: 0,
            spill: None,
            pending: None,
            pass: 0,
            next: 0,
            output_runs: 0,
            merge: None,
            cleaning: None,
            sealed: false,
            stats: WorkStats::default(),
        }
    }
    fn path(&self, pass: u64, run: u64) -> PathBuf {
        self.root.join(format!("{pass:016x}-{run:016x}.run"))
    }
    pub fn spilling(&self) -> bool {
        self.spill.is_some()
    }
    /// Add one record, starting a spill when the in-memory run is full.
    ///
    /// # Errors
    ///
    /// Returns an error when `meta` is invalid or a spill cannot start.
    pub fn push(&mut self, meta: Meta) -> Result<(), StoreError> {
        if self.sealed || self.spilling() {
            return Err(invalid("metadata sorter requires spill progress"));
        }
        meta.validate()?;
        let charge = meta.charge();
        if charge > MAX_METADATA {
            return Err(invalid("oversized metadata allocation"));
        }
        if self.bytes.saturating_add(charge) > RUN_BYTES {
            self.pending = Some(meta);
            self.start_spill()?;
        } else {
            self.bytes = self.bytes.saturating_add(charge);
            self.buffer.push(meta);
            if self.buffer.len() == RUN_ENTRIES {
                self.start_spill()?;
            }
        }
        self.stats.metadata(
            self.buffer
                .len()
                .saturating_add(usize::from(self.pending.is_some())),
            self.bytes
                .saturating_add(RUN_ENTRIES.saturating_mul(std::mem::size_of::<Meta>()))
                .saturating_add(MAX_METADATA.saturating_mul(2)),
            1,
        );
        Ok(())
    }
    fn start_spill(&mut self) -> Result<(), StoreError> {
        // A run has at most 128 entries. Sorting this one run is the bounded
        // non-preemptible metadata operation; emission itself yields per record.
        self.buffer.sort_unstable_by(|a, b| b.key.cmp(&a.key));
        self.spill = Some(File::create(self.path(0, self.runs))?);
        Ok(())
    }
    /// One quantum of writing the full run to disk.
    ///
    /// # Errors
    ///
    /// Returns an error when the run cannot be written.
    pub fn spill_step(&mut self, limit: WorkLimit) -> Result<Step<()>, StoreError> {
        let limit = limit.checked()?;
        self.stats.last_step_work = 0;
        let Some(file) = &mut self.spill else {
            return Ok(Step::Done(()));
        };
        let mut emitted = 0;
        while self.stats.last_step_work < limit.record_entries() && emitted < limit.metadata_bytes {
            let Some(meta) = self.buffer.pop() else {
                self.spill = None;
                self.bytes = 0;
                increment(&mut self.runs)?;
                if let Some(meta) = self.pending.take() {
                    self.push(meta)?;
                }
                return Ok(Step::Done(()));
            };
            emitted = emitted.saturating_add(meta.charge());
            write_meta(file, &meta)?;
            self.stats.last_step_work = self.stats.last_step_work.saturating_add(1);
        }
        Ok(Step::More)
    }
    /// One quantum of merging; done with the path of the sorted output.
    ///
    /// # Errors
    ///
    /// Returns an error when a run cannot be read or the output written.
    pub fn step(&mut self, limit: WorkLimit) -> Result<Step<PathBuf>, StoreError> {
        let limit = limit.checked()?;
        self.stats.last_step_work = 0;
        if !self.sealed {
            if self.spilling() {
                self.spill_step(limit)?;
                return Ok(Step::More);
            }
            if !self.buffer.is_empty() {
                self.start_spill()?;
                return Ok(Step::More);
            }
            self.sealed = true;
            if self.runs == 0 {
                File::create(self.path(0, 0))?;
                self.runs = 1;
            }
            return Ok(Step::More);
        }
        if self.runs == 1 {
            return Ok(Step::Done(self.path(self.pass, 0)));
        }
        let mut emitted = 0;
        while self.stats.last_step_work < limit.record_entries() && emitted < limit.metadata_bytes {
            if let Some((next, end)) = self.cleaning {
                if next < end {
                    fs::remove_file(self.path(self.pass, next))?;
                    self.cleaning = Some((next.saturating_add(1), end));
                    self.stats.last_step_work = self.stats.last_step_work.saturating_add(1);
                } else {
                    self.cleaning = None;
                    increment(&mut self.output_runs)?;
                }
                continue;
            }
            if self.merge.is_none() {
                if self.next == self.runs {
                    increment(&mut self.pass)?;
                    self.runs = self.output_runs;
                    self.output_runs = 0;
                    self.next = 0;
                    return Ok(Step::More);
                }
                let first = self.next;
                let count = self.runs.saturating_sub(first).min(MERGE_FAN_IN as u64);
                self.merge = Some(Merge {
                    inputs: Vec::with_capacity(MERGE_FAN_IN),
                    output: File::create(self.path(self.pass.saturating_add(1), self.output_runs))?,
                    first,
                    count,
                });
            }
            let merge = self.merge.as_mut().invariant("merge missing")?;
            if (merge.inputs.len() as u64) < merge.count {
                let path = self
                    .root
                    .join(format!("{:016x}-{:016x}.run", self.pass, self.next));
                let mut file = File::open(path)?;
                let head = read_meta(&mut file)?;
                merge.inputs.push((file, head));
                increment(&mut self.next)?;
                self.stats.last_step_work = self.stats.last_step_work.saturating_add(1);
                continue;
            }
            self.stats.metadata(
                merge.inputs.len(),
                merge
                    .inputs
                    .iter()
                    .filter_map(|(_, m)| m.as_ref())
                    .map(Meta::charge)
                    .sum::<usize>()
                    .saturating_add(MAX_METADATA.saturating_mul(2))
                    .saturating_add(RUN_ENTRIES.saturating_mul(std::mem::size_of::<Meta>())),
                merge.inputs.len().saturating_add(1),
            );
            let choice = merge
                .inputs
                .iter()
                .enumerate()
                .filter_map(|(i, (_, m))| m.as_ref().map(|m| (i, m)))
                .min_by(|(_, a), (_, b)| a.key.cmp(&b.key))
                .map(|(i, _)| i);
            let Some(choice) = choice else {
                let merge = self.merge.take().invariant("merge missing")?;
                self.cleaning = Some((merge.first, merge.first.saturating_add(merge.count)));
                drop(merge);
                continue;
            };
            let (file, head) = merge
                .inputs
                .get_mut(choice)
                .invariant("merge input missing")?;
            let meta = head.take().invariant("head missing")?;
            emitted = emitted.saturating_add(meta.charge());
            write_meta(&mut merge.output, &meta)?;
            drop(meta);
            *head = read_meta(file)?;
            self.stats.last_step_work = self.stats.last_step_work.saturating_add(1);
        }
        Ok(Step::More)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(unix)]
    fn metadata_roundtrips_nonunicode_filesystem_names() {
        use std::io::{Seek, SeekFrom};
        use std::os::unix::ffi::OsStringExt;
        let name = OsString::from_vec(vec![0xff, b'a']);
        let mut file = tempfile::tempfile().unwrap();
        let meta = Meta {
            key: vec![name.clone()],
            source: PathBuf::from(name.clone()),
            pin: 0,
            len: 0,
            pending: false,
        };
        write_meta(&mut file, &meta).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let decoded = read_meta(&mut file).unwrap().unwrap();
        assert_eq!(decoded.key, vec![name.clone()]);
        assert_eq!(decoded.source, PathBuf::from(name));
    }
    #[test]
    fn spilled_sort_preserves_filename_order_and_bounds_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let mut sorter = Sorter::new(tmp.path().join("sort")).unwrap();
        for i in (0..4097).rev() {
            sorter
                .push(Meta {
                    key: vec![format!("{i:08}").into()],
                    source: format!("{i}.delta").into(),
                    pin: i,
                    len: 1,
                    pending: false,
                })
                .unwrap();
            while sorter.spilling() {
                sorter.spill_step(WorkLimit::default()).unwrap();
            }
        }
        let path = loop {
            if let Step::Done(path) = sorter
                .step(WorkLimit {
                    entries: 7,
                    ..WorkLimit::default()
                })
                .unwrap()
            {
                break path;
            }
            assert!(sorter.stats.last_step_work <= 7);
        };
        let mut file = File::open(path).unwrap();
        for i in 0..4097 {
            assert_eq!(read_meta(&mut file).unwrap().unwrap().pin, i);
        }
        assert!(read_meta(&mut file).unwrap().is_none());
        assert!(sorter.stats.peak_metadata_entries <= 128);
        assert!(
            sorter.stats.peak_metadata_bytes
                <= RUN_BYTES + RUN_ENTRIES * std::mem::size_of::<Meta>() + 2 * MAX_METADATA
        );
        assert!(sorter.stats.peak_open_files <= 9);
    }
    #[test]
    fn corrupt_metadata_length_is_rejected_before_allocation() {
        use std::io::{Seek, SeekFrom};
        let tmp = tempfile::tempfile().unwrap();
        let mut file = tmp;
        file.write_all(&u32::MAX.to_le_bytes()).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        assert!(read_meta(&mut file).is_err());
    }
}
