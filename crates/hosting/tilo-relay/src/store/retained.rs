//! Shared retained coverage. Callers serialize writes with the workspace lock.
//!
//! A lineage is an authenticated envelope author plus channel, never a timestamp.
//! CURRENT selects durable shared replay coverage. Opaque records append in
//! receipt order; only proven raw Automerge ancestry permits collection. A
//! replacement generation is collected only after its CURRENT selector is synced.
//! An opaque checkpoint cannot prove that a queue entry is covered merely
//! because its filename or timestamp is older.

use super::{RETAINED_DIR, StoreError, WorkspaceStore};
use crate::store::bounded::Invariant;
use crate::{coedit::CoeditPayload, transport::Envelope};
use automerge::ReadDoc;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

fn invalid(message: &str) -> StoreError {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}

/// Parse framing separately from proof: metadata routing may preserve a known
/// prefix with unknown trailing bytes, but that prefix never grants coverage.
fn framed_envelope(bytes: &[u8]) -> Option<(Envelope, &[u8])> {
    let prefix: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
    if usize::try_from(u32::from_le_bytes(prefix)).ok()? != bytes.len().checked_sub(4)? {
        return None;
    }
    postcard::take_from_bytes(bytes.get(4..)?).ok()
}

/// Validate EVERY enclosing representation before granting proof authority.
/// Authentication happens at ingress, not in this disk decoder.
pub(crate) fn payload(bytes: &[u8], workspace: u64) -> Option<([u8; 32], CoeditPayload)> {
    let (
        Envelope::Delta {
            workspace_id,
            author_device_public_key,
            delta,
            ..
        },
        rest,
    ) = framed_envelope(bytes)?
    else {
        return None;
    };
    if workspace_id != workspace || !rest.is_empty() {
        return None;
    }
    Some((author_device_public_key, complete_coedit_payload(&delta)?))
}

/// The co-edit body of a retained frame of `workspace`.
pub(crate) fn body_of(bytes: &[u8], workspace: u64) -> Option<Vec<u8>> {
    match payload(bytes, workspace)?.1 {
        CoeditPayload::Checkpoint { body, .. } | CoeditPayload::RetainedDelta { body, .. } => {
            Some(body)
        }
    }
}

/// Used with an independently validated complete outer frame during replay.
pub(super) fn complete_coedit_payload(delta: &[u8]) -> Option<CoeditPayload> {
    let (payload, rest) = postcard::take_from_bytes(delta).ok()?;
    rest.is_empty().then_some(payload)
}

/// Routing metadata only. Preserve the entire original frame even when its
/// wrappers have unknown suffixes; callers must use `payload` for proof.
fn identity(bytes: &[u8], workspace: u64) -> Option<([u8; 32], [u8; 32], bool)> {
    let (
        Envelope::Delta {
            workspace_id,
            author_device_public_key,
            delta,
            ..
        },
        _,
    ) = framed_envelope(bytes)?
    else {
        return None;
    };
    if workspace_id != workspace {
        return None;
    }
    let (channel, checkpoint) = match CoeditPayload::decode(&delta)? {
        CoeditPayload::Checkpoint { channel, .. } => (channel, true),
        CoeditPayload::RetainedDelta { channel, .. } => (channel, false),
    };
    Some((author_device_public_key, channel, checkpoint))
}

fn checkpoint_key(bytes: &[u8], workspace: u64) -> Option<blake3::Hash> {
    let (author, CoeditPayload::Checkpoint { channel, body }) = payload(bytes, workspace)? else {
        return None;
    };
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-relay-retained-checkpoint-body-v1\0");
    hash.update(&author);
    hash.update(&channel);
    hash.update(&body);
    Some(hash.finalize())
}

/// Frame the pinned Automerge chunk format only; Automerge itself validates
/// checksums, columns, operations and document heads. Unknown future formats
/// fail closed. The length must be canonical ULEB128 with checked arithmetic.
fn first_chunk(bytes: &[u8]) -> Option<(&[u8], &[u8], u8)> {
    if !bytes.starts_with(&[0x85, 0x6f, 0x4a, 0x83]) {
        return None;
    }
    let kind = *bytes.get(8)?;
    if kind > 3 {
        return None;
    }
    let mut header_len = 9usize;
    let mut data_len = 0usize;
    let mut shift = 0u32;
    loop {
        let byte = *bytes.get(header_len)?;
        header_len = header_len.checked_add(1)?;
        let low = usize::from(byte & 0x7f);
        data_len = data_len.checked_add(low.checked_mul(1usize.checked_shl(shift)?)?)?;
        if byte & 0x80 == 0 {
            if shift != 0 && low == 0 {
                return None;
            }
            break;
        }
        shift = shift.checked_add(7)?;
    }
    let (chunk, rest) = bytes.split_at_checked(header_len.checked_add(data_len)?)?;
    Some((chunk, rest, kind))
}

/// Every document chunk must be reconstructed in isolation, otherwise the
/// loader may skip an appended document whose declared heads are already known.
/// Valid incremental changes/bundles may lack their dependencies in isolation;
/// the later containment check resolves those against the candidate snapshot.
fn independently_valid_chunks(mut bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    while !bytes.is_empty() {
        let Some((chunk, rest, kind)) = first_chunk(bytes) else {
            return false;
        };
        match automerge::Automerge::load(chunk) {
            Ok(_) => {}
            Err(automerge::AutomergeError::MissingDeps) if kind != 0 => {}
            Err(_) => return false,
        }
        bytes = rest;
    }
    true
}

/// Only complete, strictly decoded raw Automerge snapshots prove coverage.
/// In particular `load_incremental` is unsuitable here: it accepts partial
/// input, so unchanged heads after that operation can hide malformed data.
fn covers(checkpoint: &[u8], prior: &[u8], workspace: u64) -> bool {
    let Some((author, CoeditPayload::Checkpoint { channel, body })) =
        payload(checkpoint, workspace)
    else {
        return false;
    };
    let Some((old_author, old_payload)) = payload(prior, workspace) else {
        return false;
    };
    let (old_channel, old_body) = match old_payload {
        CoeditPayload::Checkpoint { channel, body }
        | CoeditPayload::RetainedDelta { channel, body } => (channel, body),
    };
    if author != old_author || channel != old_channel || body.is_empty() || old_body.is_empty() {
        return false;
    }
    if !independently_valid_chunks(&body) || !independently_valid_chunks(&old_body) {
        return false;
    }
    let Ok(document) = automerge::Automerge::load(&body) else {
        return false;
    };
    if !document.get_missing_deps(&[]).is_empty() {
        return false;
    }
    let heads = document.get_heads();
    // A full snapshot provides dependencies for incremental tail chunks.
    // Strict load rejects truncated/opaque suffixes; checking missing deps
    // rejects well-formed changes whose unknown ancestors cannot be applied.
    let mut combined = document.save();
    combined.extend_from_slice(&old_body);
    automerge::Automerge::load(&combined).is_ok_and(|combined| {
        combined.get_missing_deps(&[]).is_empty() && combined.get_heads() == heads
    })
}

#[cfg(test)]
thread_local! { static RECORD_BYTES: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) }; }

struct Record {
    path: PathBuf,
    bytes: Vec<u8>,
}

impl Record {
    fn read(path: PathBuf) -> Result<Self, StoreError> {
        Ok(Self::from_bytes(path.clone(), fs::read(path)?))
    }
    fn read_for_proof(path: PathBuf) -> Result<Self, StoreError> {
        let bytes = super::replay::frame_bytes(&path)?.unwrap_or_default();
        Ok(Self::from_bytes(path, bytes))
    }
    fn from_bytes(path: PathBuf, bytes: Vec<u8>) -> Self {
        #[cfg(test)]
        RECORD_BYTES.with(|counter| {
            let (live, peak) = counter.get();
            counter.set((
                live.saturating_add(bytes.len()),
                peak.max(live.saturating_add(bytes.len())),
            ));
        });
        Self { path, bytes }
    }
}
#[cfg(test)]
impl Drop for Record {
    fn drop(&mut self) {
        RECORD_BYTES.with(|counter| {
            let (live, peak) = counter.get();
            counter.set((live.saturating_sub(self.bytes.len()), peak));
        });
    }
}

/// Record digests by device and inode, with the size and change times each
/// was hashed at.
#[cfg(unix)]
type Digests = std::collections::HashMap<[u64; 2], ([i64; 5], [u8; 32])>;

/// Digests of retained files by identity. A record is never rewritten once
/// published, so a file whose identity, size and change times are the ones
/// it was hashed with still holds those bytes.
#[cfg(unix)]
fn digests() -> std::sync::MutexGuard<'static, Digests> {
    use std::sync::{Mutex, OnceLock, PoisonError};
    static DIGESTS: OnceLock<Mutex<Digests>> = OnceLock::new();
    DIGESTS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

#[cfg(unix)]
fn file_key(metadata: &fs::Metadata) -> ([u64; 2], [i64; 5]) {
    use std::os::unix::fs::MetadataExt as _;
    (
        [metadata.dev(), metadata.ino()],
        [
            i64::from_ne_bytes(metadata.len().to_ne_bytes()),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        ],
    )
}

/// Past this many remembered digests the cache starts over.
#[cfg(unix)]
const MAX_DIGESTS: usize = 65_536;

/// The digest of the record at `path`, read only when not remembered.
fn record_digest(path: &Path, metadata: &fs::Metadata) -> Result<[u8; 32], StoreError> {
    #[cfg(unix)]
    let (identity, times) = file_key(metadata);
    #[cfg(unix)]
    let known = digests().get(&identity).copied();
    #[cfg(unix)]
    if let Some((known, digest)) = known
        && known == times
    {
        return Ok(digest);
    }
    #[cfg(not(unix))]
    let _ = metadata;
    let bytes = super::replay::frame_bytes(path)?.unwrap_or_default();
    let digest = *blake3::hash(&bytes).as_bytes();
    #[cfg(unix)]
    {
        let mut cache = digests();
        if cache.len() >= MAX_DIGESTS {
            cache.clear();
        }
        cache.insert(identity, (times, digest));
    }
    Ok(digest)
}

/// Remember `digest` for the record just published at `path`.
fn remember_digest(path: &Path, digest: &[u8; 32]) {
    #[cfg(unix)]
    if let Ok(metadata) = fs::symlink_metadata(path) {
        let (identity, times) = file_key(&metadata);
        let mut cache = digests();
        if cache.len() >= MAX_DIGESTS {
            cache.clear();
        }
        cache.insert(identity, (times, *digest));
    }
    #[cfg(not(unix))]
    let _ = (path, digest);
}

/// One prior record of a delta's lineage: its ordinal, and whether it holds
/// the delta's exact bytes. Only a record of the same size whose digest
/// matches is read, and then compared in full.
fn inspect_delta(
    entry: &fs::DirEntry,
    name: &str,
    (bytes, digest): (&[u8], &[u8; 32]),
    last: &mut u64,
    duplicate: &mut bool,
    stats: &mut super::bounded::WorkStats,
) -> Result<(), StoreError> {
    if let Some(prefix) = name.get(..16)
        && let Ok(ordinal) = u64::from_str_radix(prefix, 16)
    {
        *last = (*last).max(ordinal);
    }
    if *duplicate {
        return Ok(());
    }
    let metadata = entry.metadata()?;
    if metadata.len() != bytes.len() as u64 || record_digest(&entry.path(), &metadata)? != *digest {
        return Ok(());
    }
    let record = Record::read_for_proof(entry.path())?;
    super::bounded::increment(&mut stats.preparation_reads)?;
    stats.peak_frame_bytes = stats
        .peak_frame_bytes
        .max(record.bytes.len().saturating_add(bytes.len()));
    *duplicate = record.bytes == bytes;
    Ok(())
}

fn records(generation: &Path) -> Result<Vec<Record>, StoreError> {
    let mut paths = fs::read_dir(generation)?.collect::<Result<Vec<_>, _>>()?;
    paths.sort_by_key(fs::DirEntry::file_name);
    let mut out = Vec::new();
    let checkpoint = generation.join("checkpoint.delta");
    if checkpoint.exists() {
        out.push(Record::read(checkpoint)?);
    }
    for entry in paths {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name != "checkpoint.delta"
            && name.ends_with(".delta")
            && !name.starts_with('.')
            && entry.file_type()?.is_file()
        {
            out.push(Record::read(entry.path())?);
        }
    }
    Ok(out)
}

pub(super) fn sync_dir(path: &Path) -> Result<(), StoreError> {
    #[cfg(test)]
    super::test_barrier::check(path, "sync-dir")?;
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

/// An entry's identity, size and change times: writing a file, or adding,
/// removing or renaming a directory's entries, changes them.
#[cfg(unix)]
fn stamp(path: &Path) -> io::Result<[i64; 7]> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = fs::metadata(path)?;
    Ok([
        i64::from_ne_bytes(metadata.dev().to_ne_bytes()),
        i64::from_ne_bytes(metadata.ino().to_ne_bytes()),
        i64::from_ne_bytes(metadata.size().to_ne_bytes()),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    ])
}

/// [`sync_dir`], unless this process synced `path` before and its entries
/// have not changed since: a barrier every commit repeats over the same
/// ancestors then costs a `stat` instead of an fsync.
pub(super) fn sync_dir_once(path: &Path) -> Result<(), StoreError> {
    #[cfg(test)]
    super::test_barrier::check(path, "sync-dir")?;
    sync_once(path)
}

/// `fsync` of the file `path`, unless this process synced the same inode
/// before and it has not been written since. Readers and collection quanta
/// re-establish the durability of an unchanged selection this way.
pub(super) fn sync_file_once(path: &Path) -> Result<(), StoreError> {
    sync_once(path)
}

fn sync_once(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock, PoisonError};
        static SYNCED: OnceLock<Mutex<HashMap<PathBuf, [i64; 7]>>> = OnceLock::new();
        let synced = SYNCED.get_or_init(Mutex::default);
        // Taken before the barrier: a change racing it leaves a stale stamp.
        let before = stamp(path)?;
        if synced
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(path)
            == Some(&before)
        {
            return Ok(());
        }
        fs::File::open(path)?.sync_all()?;
        synced
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.to_owned(), before);
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::File::open(path)?.sync_all()?;
        Ok(())
    }
}

pub(super) fn create_dir(path: &Path) -> Result<(), StoreError> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {}
        Err(error) => return Err(error.into()),
    }
    // Also sync existing directories: a previous attempt may have returned
    // after creation but before the parent directory reached stable storage.
    if let Some(parent) = path.parent() {
        sync_dir_once(parent)?;
    }
    Ok(())
}

pub(super) fn durable_write(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    tilo_files::native::atomic::write(path, bytes)?;
    // native::atomic fsyncs the parent and fails when that fails. Apple's fsync
    // leaves the drive's cache unflushed, so there the barrier is repeated
    // with F_FULLFSYNC before destructive retirement relies on it.
    #[cfg(any(test, target_vendor = "apple"))]
    if let Some(parent) = path.parent() {
        #[cfg(target_vendor = "apple")]
        sync_dir(parent)?;
        #[cfg(all(test, not(target_vendor = "apple")))]
        super::test_barrier::check(parent, "sync-dir")?;
    }
    Ok(())
}

fn generation_name(name: &str) -> bool {
    name.len() == 32 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(super) fn current(lineage: &Path) -> Result<Option<PathBuf>, StoreError> {
    use std::io::Read;
    let mut name = String::new();
    match fs::File::open(lineage.join("CURRENT")) {
        Ok(file) => {
            file.take(33).read_to_string(&mut name)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    if !generation_name(&name) {
        return Err(invalid("invalid retained generation"));
    }
    let generation = lineage.join(name);
    if !fs::symlink_metadata(&generation)?.is_dir() {
        return Err(invalid("missing retained generation"));
    }
    Ok(Some(generation))
}

fn commit_generation(
    lineage: &Path,
    retained: &[&Record],
    checkpoint: Option<&[u8]>,
) -> Result<PathBuf, StoreError> {
    let name = hex::encode(rand::random::<[u8; 16]>());
    let generation = lineage.join(&name);
    create_dir(&generation)?;
    // A proven raw baseline precedes uncovered dependent tails. Survivors
    // keep their relative order, and hard links avoid copying immutable bytes.
    // Opaque checkpoint histories never enter this replacement path.
    if let Some(bytes) = checkpoint {
        durable_write(&generation.join("checkpoint.delta"), bytes)?;
    }
    for (index, entry) in retained.iter().enumerate() {
        fs::hard_link(&entry.path, generation.join(format!("{index:016x}.delta")))?;
    }
    sync_dir(&generation)?;
    durable_write(&lineage.join("CURRENT"), name.as_bytes())?;
    Ok(generation)
}

/// Bounded incremental GC; no recursion and no authority over unknown content.
pub(crate) struct GenerationGc {
    lineage: PathBuf,
    entries: super::bounded::DirectoryCursor,
    child: Option<(PathBuf, super::bounded::DirectoryCursor)>,
}
impl GenerationGc {
    pub(crate) fn new(lineage: PathBuf) -> Result<Self, StoreError> {
        Ok(Self {
            entries: super::bounded::DirectoryCursor::open(&lineage)?,
            lineage,
            child: None,
        })
    }
    pub(crate) fn step(
        &mut self,
        limit: super::bounded::WorkLimit,
    ) -> Result<super::bounded::Step<()>, StoreError> {
        use super::bounded::Step;
        let limit = limit.checked()?;
        // Reestablish Task 1's mandatory barrier after reopen and before every quantum.
        sync_file_once(&self.lineage.join("CURRENT"))?;
        sync_dir_once(&self.lineage)?;
        let active = current(&self.lineage)?.ok_or_else(|| invalid("missing CURRENT during GC"))?;
        for _ in 0..limit.record_entries() {
            if let Some((path, entries)) = &mut self.child {
                if *path == active {
                    self.child = None;
                    continue;
                }
                if let Some(entry) = entries.peek()? {
                    let name = entry.file_name();
                    if (known_record(&name)
                        || (self
                            .lineage
                            .file_name()
                            .is_some_and(|v| v == super::shell_selection::DIRECTORY)
                            && (name == "basis.manifest"
                                || name == "live.manifest"
                                || name == "gc.manifest"
                                || name
                                    .to_str()
                                    .and_then(|s| s.strip_prefix("gc-"))
                                    .is_some_and(|s| known_record(std::ffi::OsStr::new(s))))))
                        && entry.file_type()?.is_file()
                    {
                        fs::remove_file(entry.path())?;
                    }
                    entries.advance();
                } else {
                    match fs::remove_dir(&*path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => {}
                        Err(e) => return Err(e.into()),
                    }
                    self.child = None;
                }
            } else if let Some(entry) = self.entries.peek()? {
                if entry.path() != active
                    && entry.file_type()?.is_dir()
                    && generation_name(&entry.file_name().to_string_lossy())
                {
                    self.child = Some((
                        entry.path(),
                        super::bounded::DirectoryCursor::open(&entry.path())?,
                    ));
                }
                self.entries.advance();
            } else {
                sync_dir_once(&self.lineage)?;
                return Ok(Step::Done(()));
            }
        }
        Ok(Step::More)
    }
}
fn known_record(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        let name = name.strip_suffix(".continuity-v1").unwrap_or(name);
        name == "checkpoint.delta"
            || (name.strip_suffix(".delta").is_some_and(|prefix| {
                matches!(prefix.len(), 8 | 16) && prefix.bytes().all(|b| b.is_ascii_hexdigit())
            }))
    })
}

enum RetainPhase {
    Scan(fs::ReadDir),
    Sort,
    Inspect(fs::File),
    Stage(fs::File),
    Commit,
    Collect(Box<GenerationGc>),
    Done,
}
/// Owner must retain this transaction's ordered position across every step.
/// Drop only closes handles: abandoned staging files are reclaimed separately.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent step-machine flags of one resumable retention pass"
)]
pub(crate) struct RetainBuild {
    lineage: PathBuf,
    active: PathBuf,
    scratch: PathBuf,
    bytes: Vec<u8>,
    /// Digest of `bytes`, which a prior record must share to be read.
    digest: [u8; 32],
    checkpoint: bool,
    workspace: u64,
    sorter: super::bounded::Sorter,
    sorted: Option<PathBuf>,
    phase: RetainPhase,
    duplicate: bool,
    opaque: bool,
    last: u64,
    key: Option<blake3::Hash>,
    staged: Option<PathBuf>,
    staged_append: bool,
    survivors: u64,
    written: u64,
    shell_store: Option<WorkspaceStore>,
    selected_read: Option<super::shell_selection::SelectedRead>,
    selection: Option<super::shell_selection::Selection>,
    publication: Option<super::shell_selection::Publication>,
    selected_obsolete: bool,
    staged_only: bool,
    continuity: Option<super::continuity::Receipt>,
    store: WorkspaceStore,
}
pub(crate) struct CheckpointStage {
    build: RetainBuild,
}
pub(crate) struct CheckpointDiscard {
    root: PathBuf,
    entries: Option<super::bounded::DirectoryCursor>,
}
impl CheckpointDiscard {
    pub(crate) fn new(lineage: &Path, staged: PathBuf) -> Result<Option<Self>, StoreError> {
        if current(lineage)?.as_ref() == Some(&staged) {
            return Ok(None);
        }
        if staged.parent() != Some(lineage) {
            return Err(invalid("checkpoint stage is outside its retained lineage"));
        }
        let entries = match super::bounded::DirectoryCursor::open(&staged) {
            Ok(entries) => Some(entries),
            Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        Ok(Some(Self {
            root: staged,
            entries,
        }))
    }
    pub(crate) fn step(
        &mut self,
        limit: super::bounded::WorkLimit,
    ) -> Result<super::bounded::Step<()>, StoreError> {
        use super::bounded::Step;
        let limit = limit.checked()?;
        let Some(entries) = &mut self.entries else {
            return Ok(Step::Done(()));
        };
        for _ in 0..limit.record_entries() {
            let Some(entry) = entries.peek()? else {
                self.entries = None;
                match fs::remove_dir(&self.root) {
                    Ok(()) => return Ok(Step::Done(())),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(Step::Done(()));
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            if !entry.file_type()?.is_file() {
                return Err(invalid("checkpoint stage contained a non-file entry"));
            }
            match fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            entries.advance();
        }
        Ok(Step::More)
    }
}
impl CheckpointStage {
    pub(crate) fn with_continuity(mut self, receipt: Option<super::continuity::Receipt>) -> Self {
        self.build.continuity = receipt;
        self
    }

    #[cfg(test)]
    pub(super) fn stats(&self) -> &super::bounded::WorkStats {
        &self.build.sorter.stats
    }
    pub(crate) fn projected_generation(&self) -> &Path {
        self.build.staged.as_deref().unwrap_or(&self.build.active)
    }
    pub(crate) fn lineage_name(&self) -> Result<&std::ffi::OsStr, StoreError> {
        self.build
            .lineage
            .file_name()
            .ok_or_else(|| invalid("retained lineage has no name"))
    }
    pub(crate) fn written(&self) -> u64 {
        self.build.written
    }
    pub(crate) fn cleanup_paths(&self) -> Option<(PathBuf, PathBuf)> {
        self.build
            .staged
            .as_ref()
            .map(|staged| (self.build.lineage.clone(), staged.clone()))
    }
    pub(crate) fn step(
        &mut self,
        limit: super::bounded::WorkLimit,
    ) -> Result<super::bounded::Step<bool>, StoreError> {
        use super::bounded::Step;
        if matches!(
            self.build.phase,
            RetainPhase::Commit | RetainPhase::Collect(_) | RetainPhase::Done
        ) {
            return Ok(Step::Done(
                self.build.duplicate || self.build.staged.is_some(),
            ));
        }
        match self.build.step(limit)? {
            Step::More => Ok(Step::More),
            Step::Done(_) => Ok(Step::Done(
                self.build.duplicate || self.build.staged.is_some(),
            )),
        }
    }
    /// Publish only the lineage CURRENT. Selection CURRENT must be durable
    /// before `finish_gc` may retire any prior generation.
    pub(crate) fn promote(&mut self) -> Result<(), StoreError> {
        if matches!(self.build.phase, RetainPhase::Commit) {
            let staged = self
                .build
                .staged
                .as_ref()
                .invariant("staged missing")?
                .clone();
            sync_dir(&staged)?;
            durable_write(
                &self.build.lineage.join("CURRENT"),
                staged
                    .file_name()
                    .invariant("staged missing")?
                    .as_encoded_bytes(),
            )?;
            self.build.written = self.build.bytes.len() as u64;
            self.build.phase =
                RetainPhase::Collect(Box::new(GenerationGc::new(self.build.lineage.clone())?));
        }
        Ok(())
    }
    pub(crate) fn finish_gc(
        &mut self,
        limit: super::bounded::WorkLimit,
    ) -> Result<super::bounded::Step<()>, StoreError> {
        use super::bounded::Step;
        #[cfg(test)]
        super::test_barrier::check(&self.build.lineage, "checkpoint-stage-finish-gc")?;
        match &mut self.build.phase {
            RetainPhase::Collect(gc) => {
                if let Step::Done(()) = gc.step(limit)? {
                    self.build.phase = RetainPhase::Done;
                    Ok(Step::Done(()))
                } else {
                    Ok(Step::More)
                }
            }
            RetainPhase::Done => Ok(Step::Done(())),
            _ => Err(invalid("checkpoint stage was not promoted")),
        }
    }
}
impl RetainBuild {
    pub(crate) fn with_continuity(mut self, receipt: Option<super::continuity::Receipt>) -> Self {
        self.continuity = receipt;
        self
    }

    pub(crate) fn scratch(&self) -> &Path {
        &self.scratch
    }
    pub(crate) fn publication_on_error(&self) -> Option<super::shell_selection::Publication> {
        self.selection
            .as_ref()
            .and_then(super::shell_selection::Selection::publication_on_error)
    }
    #[allow(
        dead_code,
        reason = "aggregate work statistics remain available alongside phase-specific diagnostics"
    )]
    pub(crate) fn stats(&self) -> &super::bounded::WorkStats {
        self.selection
            .as_ref()
            .map_or(&self.sorter.stats, |s| s.stats())
    }
    pub(crate) fn retention_stats(&self) -> &super::bounded::WorkStats {
        &self.sorter.stats
    }
    pub(crate) fn selection_stats(&self) -> Option<&super::bounded::WorkStats> {
        self.selection
            .as_ref()
            .map(super::shell_selection::Selection::stats)
    }
    pub(crate) fn selected_obsolete(&self) -> bool {
        self.selected_obsolete
    }
    /// Every prior record was inspected: collect a duplicate, stage a
    /// checkpoint, or append.
    fn inspected(&mut self) -> Result<(), StoreError> {
        if self.duplicate && self.continuity.is_none() {
            self.phase = if self.staged_only {
                RetainPhase::Done
            } else {
                RetainPhase::Collect(Box::new(GenerationGc::new(self.lineage.clone())?))
            };
        } else if self.continuity.is_none()
            && self.checkpoint
            && !self.opaque
            && covers(&self.bytes, &self.bytes, self.workspace)
        {
            let staged = self.lineage.join(hex::encode(rand::random::<[u8; 16]>()));
            create_dir(&staged)?;
            durable_write(&staged.join("checkpoint.delta"), &self.bytes)?;
            self.staged = Some(staged);
            #[cfg(test)]
            super::test_barrier::check(&self.lineage, "checkpoint-stage-after-allocation")?;
            self.phase = RetainPhase::Stage(fs::File::open(
                self.sorted.as_ref().invariant("sorted missing")?,
            )?);
        } else if self.staged_only {
            // Preserve opaque/unproved history exactly in an
            // inactive generation. The live generation is not
            // mutated until the caller promotes this stage.
            let staged = self.lineage.join(hex::encode(rand::random::<[u8; 16]>()));
            create_dir(&staged)?;
            self.staged = Some(staged);
            #[cfg(test)]
            super::test_barrier::check(&self.lineage, "checkpoint-stage-after-allocation")?;
            self.staged_append = true;
            self.phase = RetainPhase::Stage(fs::File::open(
                self.sorted.as_ref().invariant("sorted missing")?,
            )?);
        } else {
            let next = self
                .last
                .checked_add(1)
                .ok_or_else(|| invalid("retained record sequence exhausted"))?;
            let path = self.active.join(format!("{next:016x}.delta"));
            if let Some(receipt) = &self.continuity {
                super::continuity::account_validation(&mut self.sorter.stats, self.bytes.len());
                super::continuity::write(&self.store, &path, &self.bytes, receipt)?;
            }
            durable_write(&path, &self.bytes)?;
            remember_digest(&path, &self.digest);
            self.written = self.bytes.len() as u64;
            self.phase = RetainPhase::Done;
        }
        Ok(())
    }
    #[expect(
        clippy::too_many_lines,
        reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
    )]
    pub(crate) fn step(
        &mut self,
        limit: super::bounded::WorkLimit,
    ) -> Result<super::bounded::Step<super::shell_selection::RetainOutcome>, StoreError> {
        use super::bounded::{Meta, Step, increment, read_meta};
        let limit = limit.checked()?;
        if let Some(reader) = &mut self.selected_read {
            if let Step::Done(()) = reader.step()? {
                if reader.obsolete(&self.bytes)? {
                    self.publication = Some(super::shell_selection::Publication::Suppress);
                    self.phase = RetainPhase::Done;
                    self.selected_obsolete = true;
                }
                self.selected_read = None;
            }
            return Ok(Step::More);
        }
        self.sorter.stats.last_step_work = 0;
        #[cfg(test)]
        if matches!(self.phase, RetainPhase::Sort) {
            super::test_barrier::pause(
                self.scratch.parent().unwrap().parent().unwrap(),
                "retain-sort",
            );
        }
        match &mut self.phase {
            RetainPhase::Scan(entries) => {
                if self.sorter.spilling() {
                    self.sorter.spill_step(limit)?;
                    return Ok(Step::More);
                }
                // A delta holds no metadata per entry, so its scan takes the
                // whole entry quantum and needs no ordered pass after it.
                let quantum = if self.checkpoint {
                    limit.record_entries()
                } else {
                    limit.entries
                };
                for _ in 0..quantum {
                    self.sorter.stats.last_step_work =
                        self.sorter.stats.last_step_work.saturating_add(1);
                    let Some(entry) = entries.next() else {
                        if self.checkpoint {
                            self.phase = RetainPhase::Sort;
                        } else {
                            self.inspected()?;
                        }
                        return Ok(Step::More);
                    };
                    let entry = entry?;
                    let name = entry.file_name();
                    let lossy = name.to_string_lossy();
                    if let Some(slot) = super::continuity::reserved_slot(&lossy) {
                        self.last = self.last.max(slot);
                    }
                    if entry.file_type()?.is_file()
                        && lossy.ends_with(".delta")
                        && !lossy.starts_with('.')
                    {
                        if !self.checkpoint {
                            inspect_delta(
                                &entry,
                                &lossy,
                                (&self.bytes, &self.digest),
                                &mut self.last,
                                &mut self.duplicate,
                                &mut self.sorter.stats,
                            )?;
                            continue;
                        }
                        let order = if name == "checkpoint.delta" { "0" } else { "1" };
                        self.sorter.push(Meta {
                            key: vec![order.into(), name.clone()],
                            source: name.into(),
                            pin: 0,
                            len: entry.metadata()?.len(),
                            pending: false,
                        })?;
                        if self.sorter.spilling() {
                            return Ok(Step::More);
                        }
                    }
                }
            }
            RetainPhase::Sort => {
                if let Step::Done(path) = self.sorter.step(limit)? {
                    self.phase = RetainPhase::Inspect(fs::File::open(&path)?);
                    self.sorted = Some(path);
                }
            }
            RetainPhase::Inspect(file) => {
                for _ in 0..limit.record_entries() {
                    self.sorter.stats.last_step_work =
                        self.sorter.stats.last_step_work.saturating_add(1);
                    let Some(meta) = read_meta(file)? else {
                        self.inspected()?;
                        return Ok(Step::More);
                    };
                    if let Some(name) = meta.source.file_name().and_then(|n| n.to_str())
                        && let Some(prefix) = name.get(..16)
                        && let Ok(ordinal) = u64::from_str_radix(prefix, 16)
                    {
                        self.last = self.last.max(ordinal);
                    }
                    let record = Record::read_for_proof(self.active.join(meta.source))?;
                    increment(&mut self.sorter.stats.preparation_reads)?;
                    self.sorter.stats.peak_frame_bytes = self
                        .sorter
                        .stats
                        .peak_frame_bytes
                        .max(record.bytes.len().saturating_add(self.bytes.len()));
                    if self.checkpoint {
                        // Preserve reviewed proof verbatim; hold only this prior record.
                        let old_key = checkpoint_key(&record.bytes, self.workspace);
                        self.duplicate |= old_key.is_some_and(|old| Some(old) == self.key)
                            || covers(&record.bytes, &self.bytes, self.workspace);
                        // Prefix metadata may only make retention more conservative:
                        // an unproved checkpoint keeps opaque receipt ordering.
                        self.opaque |= identity(&record.bytes, self.workspace)
                            .is_some_and(|(_, _, checkpoint)| checkpoint)
                            && !covers(&record.bytes, &record.bytes, self.workspace);
                    } else {
                        self.duplicate |= record.bytes == self.bytes;
                    }
                }
            }
            RetainPhase::Stage(file) => {
                for _ in 0..limit.record_entries() {
                    self.sorter.stats.last_step_work =
                        self.sorter.stats.last_step_work.saturating_add(1);
                    let Some(meta) = read_meta(file)? else {
                        if self.staged_append {
                            let next = self
                                .last
                                .checked_add(1)
                                .ok_or_else(|| invalid("retained record sequence exhausted"))?;
                            let path = self
                                .staged
                                .as_ref()
                                .invariant("staged missing")?
                                .join(format!("{next:016x}.delta"));
                            if let Some(receipt) = &self.continuity {
                                super::continuity::account_validation(
                                    &mut self.sorter.stats,
                                    self.bytes.len(),
                                );
                                super::continuity::write(&self.store, &path, &self.bytes, receipt)?;
                            }
                            durable_write(&path, &self.bytes)?;
                        }
                        self.phase = RetainPhase::Commit;
                        return Ok(Step::More);
                    };
                    let record = Record::read_for_proof(self.active.join(meta.source))?;
                    increment(&mut self.sorter.stats.preparation_reads)?;
                    self.sorter.stats.peak_frame_bytes = self
                        .sorter
                        .stats
                        .peak_frame_bytes
                        .max(record.bytes.len().saturating_add(self.bytes.len()));
                    let destination = if self.staged_append {
                        Some(
                            self.staged.as_ref().invariant("staged missing")?.join(
                                record
                                    .path
                                    .file_name()
                                    .ok_or_else(|| invalid("retained record name"))?,
                            ),
                        )
                    } else if !covers(&self.bytes, &record.bytes, self.workspace) {
                        let path = self
                            .staged
                            .as_ref()
                            .invariant("staged missing")?
                            .join(format!("{:016x}.delta", self.survivors));
                        increment(&mut self.survivors)?;
                        Some(path)
                    } else {
                        None
                    };
                    if let Some(destination) = destination {
                        let source = record.path.clone();
                        drop(record);
                        fs::hard_link(&source, &destination)?;
                        super::continuity::copy_measured(
                            &self.store,
                            &source,
                            &destination,
                            &mut self.sorter.stats,
                            self.bytes.capacity(),
                        )?;
                    }
                }
            }
            RetainPhase::Commit => {
                let staged = self.staged.as_ref().invariant("staged missing")?;
                sync_dir(staged)?;
                durable_write(
                    &self.lineage.join("CURRENT"),
                    staged
                        .file_name()
                        .invariant("staged missing")?
                        .as_encoded_bytes(),
                )?;
                self.written = self.bytes.len() as u64;
                self.phase =
                    RetainPhase::Collect(Box::new(GenerationGc::new(self.lineage.clone())?));
            }
            RetainPhase::Collect(gc) => {
                if let Step::Done(()) = gc.step(limit)? {
                    self.phase = RetainPhase::Done;
                }
            }
            RetainPhase::Done => {
                if self.publication.is_none() {
                    if let Some(store) = &self.shell_store {
                        if self.selection.is_none() {
                            let incoming = super::shell_selection::incoming_identity(
                                &self.bytes,
                                self.workspace,
                            );
                            self.selection = Some(super::shell_selection::Selection::new(
                                store,
                                self.scratch.clone(),
                                incoming,
                            )?);
                        }
                        match self.selection.as_mut().invariant("selection missing")?.step(limit)? {
                            Step::More => return Ok(Step::More),
                            Step::Done(super::shell_selection::SelectionOutcome::Published(
                                publication,
                            )) => self.publication = Some(publication),
                            Step::Done(
                                super::shell_selection::SelectionOutcome::BatchPublished { .. },
                            ) => return Err(invalid("single retain returned batch selection")),
                            Step::Done(
                                super::shell_selection::SelectionOutcome::UnresolvedTailDependencies,
                            ) => {
                                return Err(invalid(
                                    "unresolved shell tail floor or dependencies prevent coherent selection",
                                ));
                            }
                        }
                    } else {
                        self.publication = Some(super::shell_selection::Publication::Original);
                    }
                }
                return Ok(Step::Done(super::shell_selection::RetainOutcome {
                    written: self.written,
                    publication: self.publication.clone().invariant("publication missing")?,
                }));
            }
        }
        Ok(Step::More)
    }
}
impl WorkspaceStore {
    pub(crate) fn checkpoint_lineage_exists(
        &self,
        channel: &[u8; 32],
        bytes: &[u8],
    ) -> Result<bool, StoreError> {
        let (author, actual_channel, checkpoint) = identity(bytes, self.workspace_id)
            .ok_or_else(|| invalid("invalid retained envelope"))?;
        if actual_channel != *channel || !checkpoint {
            return Err(invalid("retained envelope channel or kind mismatch"));
        }
        let lineage = self.root.join(RETAINED_DIR).join("v2").join(format!(
            "{}-{}",
            hex::encode(author),
            hex::encode(channel)
        ));
        Ok(current(&lineage)?.is_some())
    }

    pub(crate) fn start_retain(
        &self,
        channel: &[u8; 32],
        seq: Option<u64>,
        bytes: Vec<u8>,
    ) -> Result<RetainBuild, StoreError> {
        self.start_retain_at(channel, seq, bytes, None)
    }
    pub(crate) fn start_retain_in(
        &self,
        channel: &[u8; 32],
        seq: Option<u64>,
        bytes: Vec<u8>,
        scratch: PathBuf,
    ) -> Result<RetainBuild, StoreError> {
        self.start_retain_at(channel, seq, bytes, Some(scratch))
    }
    pub(crate) fn start_staged_checkpoint(
        &self,
        channel: &[u8; 32],
        bytes: Vec<u8>,
        scratch: PathBuf,
    ) -> Result<CheckpointStage, StoreError> {
        let (author, actual_channel, checkpoint) = identity(&bytes, self.workspace_id)
            .ok_or_else(|| invalid("invalid retained envelope"))?;
        if actual_channel != *channel || !checkpoint {
            return Err(invalid("retained envelope channel or kind mismatch"));
        }
        let lineage = self.root.join(RETAINED_DIR).join("v2").join(format!(
            "{}-{}",
            hex::encode(author),
            hex::encode(channel)
        ));
        if current(&lineage)?.is_none() {
            return Err(invalid(
                "checkpoint projection requires an existing lineage",
            ));
        }
        let mut build = self.start_retain_at(channel, None, bytes, Some(scratch))?;
        build.shell_store = None;
        build.selected_read = None;
        build.staged_only = true;
        Ok(CheckpointStage { build })
    }
    fn start_retain_at(
        &self,
        channel: &[u8; 32],
        seq: Option<u64>,
        bytes: Vec<u8>,
        scratch: Option<PathBuf>,
    ) -> Result<RetainBuild, StoreError> {
        let (author, actual_channel, checkpoint) = identity(&bytes, self.workspace_id)
            .ok_or_else(|| invalid("invalid retained envelope"))?;
        if actual_channel != *channel || checkpoint != seq.is_none() {
            return Err(invalid("retained envelope channel or kind mismatch"));
        }
        for directory in self.root.ancestors().take(3) {
            sync_dir_once(directory)?;
        }
        let v2 = self.root.join(RETAINED_DIR).join("v2");
        create_dir(&v2)?;
        let lineage = v2.join(format!("{}-{}", hex::encode(author), hex::encode(channel)));
        create_dir(&lineage)?;
        let active = match current(&lineage)? {
            Some(active) => active,
            None => commit_generation(&lineage, &[], None)?,
        };
        let scratch_root = self.root.join("store-work-v1");
        fs::create_dir_all(&scratch_root)?;
        let scratch = scratch.unwrap_or_else(|| {
            scratch_root.join(format!(
                "retain-{}",
                hex::encode(rand::random::<[u8; 16]>())
            ))
        });
        fs::create_dir(&scratch)?;
        let sorter = super::bounded::Sorter::new(scratch.join("sort"))?;
        let phase = RetainPhase::Scan(fs::read_dir(&active)?);
        let key = checkpoint_key(&bytes, self.workspace_id);
        Ok(RetainBuild {
            lineage,
            active,
            scratch,
            digest: *blake3::hash(&bytes).as_bytes(),
            bytes,
            checkpoint,
            workspace: self.workspace_id,
            sorter,
            sorted: None,
            phase,
            duplicate: false,
            opaque: false,
            last: 0,
            key,
            staged: None,
            staged_append: false,
            survivors: 0,
            written: 0,
            shell_store: self.shell_authority().map(|_| self.clone()),
            selected_read: super::shell_selection::SelectedRead::new(self)?,
            selection: None,
            publication: None,
            selected_obsolete: false,
            staged_only: false,
            continuity: None,
            store: self.clone(),
        })
    }
    pub(super) fn retain_shared(
        &self,
        channel: &[u8; 32],
        seq: Option<u64>,
        bytes: &[u8],
    ) -> Result<u64, StoreError> {
        // Compatibility synchronous driver. Serving owners release worker permits
        // between these same bounded transaction steps.
        let mut build = self.start_retain(channel, seq, bytes.to_vec())?;
        let written = loop {
            if let super::bounded::Step::Done(written) =
                build.step(super::bounded::WorkLimit::default())?
            {
                break written.written;
            }
        };
        let mut discard = super::replay::WorkDiscard::new(build.scratch.clone())?;
        drop(build);
        while matches!(
            discard.step(super::bounded::WorkLimit::default())?,
            super::bounded::Step::More
        ) {}
        Ok(written)
    }

    pub(super) fn load_shared(&self) -> Result<Vec<Vec<u8>>, StoreError> {
        let mut out = Vec::new();
        // Compatibility/admin reads do not grant proof or GC authority, but
        // must retain visibility of a previously committed local selection.
        if let Some(selected) = current(&self.root.join(super::shell_selection::DIRECTORY))? {
            let mut manifest = fs::File::open(selected.join("basis.manifest"))?;
            while let Some(meta) = super::bounded::read_meta(&mut manifest)? {
                out.push(fs::read(self.root.join(meta.source))?);
            }
        }
        let v2 = self.root.join(RETAINED_DIR).join("v2");
        if !v2.exists() {
            return Ok(out);
        }
        let mut lineages = fs::read_dir(v2)?.collect::<Result<Vec<_>, _>>()?;
        lineages.sort_by_key(fs::DirEntry::file_name);
        for lineage in lineages {
            if !lineage.file_type()?.is_dir() {
                continue;
            }
            let Some(active) = current(&lineage.path())? else {
                continue;
            };
            out.extend(
                records(&active)?
                    .into_iter()
                    .map(|mut entry| std::mem::take(&mut entry.bytes)),
            );
        }
        Ok(out)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::bounded::{Step, WorkLimit};
    use super::*;

    fn message(checkpoint: bool, value: u8) -> Vec<u8> {
        let payload = if checkpoint {
            CoeditPayload::Checkpoint {
                channel: [9; 32],
                body: vec![value],
            }
        } else {
            CoeditPayload::RetainedDelta {
                channel: [9; 32],
                body: vec![value],
            }
        };
        crate::transport::encode(&Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: [1; 32],
            authored_at_ns: 1,
            delta: payload.encode().unwrap(),
            signature: [0; 64],
        })
        .unwrap()
    }

    fn frame(checkpoint: bool, body: Vec<u8>, timestamp: u64) -> Vec<u8> {
        let payload = if checkpoint {
            CoeditPayload::Checkpoint {
                channel: [9; 32],
                body,
            }
        } else {
            CoeditPayload::RetainedDelta {
                channel: [9; 32],
                body,
            }
        };
        crate::transport::encode(&Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: [1; 32],
            authored_at_ns: timestamp,
            delta: payload.encode().unwrap(),
            signature: [0; 64],
        })
        .unwrap()
    }

    pub(crate) fn wrapper_suffix(bytes: &[u8], outer: bool) -> Vec<u8> {
        let mut bytes = bytes.to_vec();
        if outer {
            bytes.extend_from_slice(b"fixture extension");
            let length = u32::try_from(bytes.len().checked_sub(4).unwrap()).unwrap();
            bytes[..4].copy_from_slice(&length.to_le_bytes());
        } else {
            let mut envelope = crate::transport::decode(&bytes[4..]).unwrap();
            let Envelope::Delta { delta, .. } = &mut envelope else {
                panic!()
            };
            delta.extend_from_slice(b"fixture extension");
            bytes = crate::transport::encode(&envelope).unwrap();
        }
        bytes
    }

    fn unproved_wrapper_is_preserved(outer: bool) {
        use automerge::transaction::Transactable;
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "first", 1).unwrap();
        let initial = frame(true, document.save(), 1);
        let unproved = wrapper_suffix(&initial, outer);
        store.retain_checkpoint(&[9; 32], &unproved).unwrap();
        document.put(automerge::ROOT, "next", 2).unwrap();
        let later = frame(true, document.save(), 2);
        store.retain_checkpoint(&[9; 32], &later).unwrap();
        let replay = WorkspaceStore::open(tmp.path(), 7, false)
            .unwrap()
            .load_retained()
            .unwrap();
        assert!(
            replay.contains(&unproved),
            "unproved enclosing bytes must survive collection"
        );
        assert!(
            replay == vec![unproved, later.clone()],
            "unproved checkpoints must preserve opaque receipt order"
        );
        let incoming = wrapper_suffix(&later, outer);
        store.retain_checkpoint(&[9; 32], &incoming).unwrap();
        assert!(
            store.load_retained().unwrap().contains(&incoming),
            "unproved incoming wrapper must not be deduplicated by its prefix"
        );
    }

    #[test]
    fn unproved_outer_wrapper_is_preserved() {
        unproved_wrapper_is_preserved(true);
    }
    #[test]
    fn unproved_coedit_wrapper_is_preserved() {
        unproved_wrapper_is_preserved(false);
    }

    // Two one-change snapshots from the review probe: the malformed document
    // keeps its unrelated operations but declares the candidate's head. Its
    // four-byte SHA-256 chunk checksum was recomputed after replacing that head.
    // Fixed bytes avoid a test-only hashing dependency or an external fixture.
    fn inconsistent_heads_fixture() -> (Vec<u8>, Vec<u8>) {
        (hex::decode("856f4a83c3fea776007601100f75efc4a706295a6e71caea86e1a64e017a07c67c94f7ee52107ad9ad2e5ea2b79cc169c7a806544ae3fbd291f52ea14d0601020302130223024002560208150b2102230234014202560257018001027f007f017f017f007f007f077f0963616e6469646174657f007f01017f017f14017f0000").unwrap(), hex::decode("856f4a8360dd956a00760110fb33a1cfd10ffd7d878aa85a2d64767c017a07c67c94f7ee52107ad9ad2e5ea2b79cc169c7a806544ae3fbd291f52ea14d0601020302130223024002560208150b2102230234014202560257018001027f007f017f017f007f007f077f09756e636f76657265647f007f01017f017f14027f0000").unwrap())
    }

    #[test]
    fn retention_hydrates_one_prior_record_at_a_time() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let initial = frame(false, vec![0; 8192], 0);
        store.retain_delta(&[9; 32], 0, &initial).unwrap();
        let lineage = store.root.join("retained/v2").join(format!(
            "{}-{}",
            hex::encode([1; 32]),
            hex::encode([9; 32])
        ));
        let active = current(&lineage).unwrap().unwrap();
        for index in 2..514 {
            fs::write(active.join(format!("{index:016x}.delta")), &initial).unwrap();
        }
        RECORD_BYTES.with(|counter| counter.set((0, 0)));
        store
            .retain_delta(&[9; 32], 999, &frame(false, vec![1; 8192], 999))
            .unwrap();
        let (live, peak) = RECORD_BYTES.with(std::cell::Cell::get);
        assert_eq!(live, 0);
        assert!(
            peak <= initial.len(),
            "hydrated prior bytes {peak}, single frame {}",
            initial.len()
        );
    }

    #[test]
    fn chunk_framing_rejects_unknown_truncated_overflow_and_noncanonical_lengths() {
        let (valid, _) = inconsistent_heads_fixture();
        assert!(first_chunk(&valid).is_some());
        let mut unknown = valid.clone();
        unknown[8] = 4;
        let mut noncanonical = valid.clone();
        assert!(noncanonical[9] < 128);
        noncanonical[9] |= 0x80;
        noncanonical.insert(10, 0);
        let mut overflow = valid[..9].to_vec();
        overflow.extend_from_slice(&[0xff; 10]);
        let mut unterminated = valid[..9].to_vec();
        unterminated.push(0x80);
        let mut wrong_magic = valid.clone();
        wrong_magic[0] = 0;
        for invalid in [
            unknown,
            noncanonical,
            overflow,
            unterminated,
            wrong_magic,
            valid[..8].to_vec(),
            valid[..valid.len() - 1].to_vec(),
        ] {
            assert!(first_chunk(&invalid).is_none());
        }
        let mut trailing = valid;
        trailing.push(0);
        assert!(
            !independently_valid_chunks(&trailing),
            "all bytes must belong to independently valid chunks"
        );
    }

    #[test]
    fn coverage_accepts_document_change_compressed_change_and_bundle_chunks() {
        use automerge::transaction::Transactable;
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "anchor", 1).unwrap();
        let baseline = frame(true, document.save(), 1);
        document
            .put(automerge::ROOT, "large", "compressible".repeat(1024))
            .unwrap();
        let mut change = document.get_last_local_change().unwrap();
        let raw = change.raw_bytes().to_vec();
        let compressed = change.bytes().to_vec();
        let bundle = document.bundle([change.hash()]).unwrap().bytes().to_vec();
        let snapshot = document.save();
        let checkpoint = frame(true, snapshot.clone(), 3);
        for (kind, body) in [(0, snapshot), (1, raw), (2, compressed), (3, bundle)] {
            assert_eq!(
                body[8], kind,
                "fixture must exercise the advertised chunk kind"
            );
            if kind != 0 {
                assert!(
                    matches!(
                        automerge::Automerge::load(&body),
                        Err(automerge::AutomergeError::MissingDeps)
                    ),
                    "incremental fixture must genuinely require baseline dependencies"
                );
            }
            let tmp = tempfile::tempdir().unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            store.retain_checkpoint(&[9; 32], &baseline).unwrap();
            store
                .retain_delta(&[9; 32], 2, &frame(false, body, 2))
                .unwrap();
            store.retain_checkpoint(&[9; 32], &checkpoint).unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            assert_eq!(store.load_retained().unwrap(), vec![checkpoint.clone()]);
        }
    }

    #[test]
    fn malformed_prior_document_is_not_covered_even_after_a_valid_prefix() {
        let (candidate, malformed) = inconsistent_heads_fixture();
        assert!(automerge::Automerge::load(&candidate).is_ok());
        assert!(automerge::Automerge::load(&malformed).is_err());
        for valid_prefix in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            let mut prior = if valid_prefix {
                candidate.clone()
            } else {
                Vec::new()
            };
            prior.extend_from_slice(&malformed);
            let prior_frame = frame(false, prior, 1);
            store.retain_delta(&[9; 32], 1, &prior_frame).unwrap();
            let checkpoint = frame(true, candidate.clone(), 2);
            store.retain_checkpoint(&[9; 32], &checkpoint).unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            let replay = store.load_retained().unwrap();
            assert!(
                replay.contains(&prior_frame),
                "malformed prior was collected through known-head fast path (prefix={valid_prefix})"
            );
            assert!(replay.contains(&checkpoint));
        }
    }

    #[test]
    fn malformed_candidate_suffix_cannot_be_omitted_as_a_covered_retry() {
        let (candidate, malformed) = inconsistent_heads_fixture();
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let first = frame(true, candidate.clone(), 1);
        store.retain_checkpoint(&[9; 32], &first).unwrap();
        let mut combined = candidate;
        combined.extend_from_slice(&malformed);
        let unknown = frame(true, combined, 2);
        store.retain_checkpoint(&[9; 32], &unknown).unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        assert_eq!(store.load_retained().unwrap(), vec![first, unknown]);
    }

    #[test]
    fn delayed_checkpoint_retry_cannot_erase_newer_checkpoint_and_tail_after_reopen() {
        use automerge::transaction::Transactable;
        for fresh_timestamp in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            let mut document = automerge::AutoCommit::new();
            document.put(automerge::ROOT, "first", 1).unwrap();
            let first_body = document.save();
            let first = frame(true, first_body.clone(), 1);
            store.retain_checkpoint(&[9; 32], &first).unwrap();
            document.put(automerge::ROOT, "second", 2).unwrap();
            let second = frame(true, document.save(), 2);
            store.retain_checkpoint(&[9; 32], &second).unwrap();
            document.put(automerge::ROOT, "tail", 3).unwrap();
            let tail = frame(false, document.save_incremental(), 3);
            store.retain_delta(&[9; 32], 3, &tail).unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            let retry = frame(true, first_body, if fresh_timestamp { 99 } else { 1 });
            store.retain_checkpoint(&[9; 32], &retry).unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            assert_eq!(
                store.load_retained().unwrap(),
                vec![second, tail],
                "delayed checkpoint retry discarded uncovered state (fresh timestamp={fresh_timestamp})"
            );
        }
    }

    #[test]
    fn opaque_checkpoints_preserve_receipt_order_and_body_retry_keeps_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let first = frame(true, b"opaque first".to_vec(), 1);
        let second = frame(true, b"opaque second".to_vec(), 2);
        let tail = frame(false, b"opaque tail".to_vec(), 3);
        store.retain_checkpoint(&[9; 32], &first).unwrap();
        store.retain_checkpoint(&[9; 32], &second).unwrap();
        store.retain_delta(&[9; 32], 3, &tail).unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        store
            .retain_checkpoint(&[9; 32], &frame(true, b"opaque first".to_vec(), 99))
            .unwrap();
        store
            .retain_checkpoint(&[9; 32], &frame(true, b"opaque second".to_vec(), 100))
            .unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        assert_eq!(store.load_retained().unwrap(), vec![first, second, tail]);
    }

    #[test]
    fn checkpoint_without_causal_tail_coverage_preserves_uncovered_tail() {
        use automerge::transaction::Transactable;
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "first", 1).unwrap();
        let first = frame(true, document.save(), 1);
        store.retain_checkpoint(&[9; 32], &first).unwrap();
        let mut branch = document.fork();
        branch.save();
        branch.put(automerge::ROOT, "uncovered", 2).unwrap();
        let tail = frame(false, branch.save_incremental(), 2);
        store.retain_delta(&[9; 32], 2, &tail).unwrap();
        document.put(automerge::ROOT, "other", 3).unwrap();
        let second = frame(true, document.save(), 3);
        store.retain_checkpoint(&[9; 32], &second).unwrap();
        let retained = store.load_retained().unwrap();
        assert!(retained.contains(&second));
        assert!(
            retained.contains(&tail),
            "checkpoint without tail ancestry erased that tail"
        );
        assert!(
            !retained.contains(&first),
            "covered raw Automerge checkpoint was not compacted"
        );
        assert_eq!(
            retained.first(),
            Some(&second),
            "proven replacement checkpoint must precede uncovered tails"
        );
        tilo_automerge::ApplyIncoming::merge_incoming(&mut document, &mut branch).unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let mut recovered = automerge::AutoCommit::new();
        for bytes in store.load_retained().unwrap() {
            let (_, message) = payload(&bytes, 7).unwrap();
            let body = match message {
                CoeditPayload::Checkpoint { body, .. }
                | CoeditPayload::RetainedDelta { body, .. } => body,
            };
            tilo_automerge::ApplyIncoming::load_incoming(&mut recovered, &body).unwrap();
        }
        assert_eq!(
            recovered.get_heads(),
            document.get_heads(),
            "fresh receiver lost an uncovered dependent tail"
        );
    }

    #[test]
    fn opaque_checkpoint_order_is_preserved_even_between_raw_snapshots() {
        use automerge::transaction::Transactable;
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "first", 1).unwrap();
        let first = frame(true, document.save(), 1);
        let opaque = frame(true, b"unknown checkpoint".to_vec(), 2);
        document.put(automerge::ROOT, "next", 2).unwrap();
        let next = frame(true, document.save(), 3);
        for checkpoint in [&first, &opaque, &next] {
            store.retain_checkpoint(&[9; 32], checkpoint).unwrap();
        }
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        assert_eq!(store.load_retained().unwrap(), vec![first, opaque, next]);
    }

    #[test]
    fn causal_proof_rejects_partial_or_missing_dependency_tails_and_keeps_publication_identity() {
        use automerge::transaction::Transactable;
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "first", 1).unwrap();
        let first = frame(true, document.save(), 1);
        store.retain_checkpoint(&[9; 32], &first).unwrap();
        let mut partial = document.save();
        partial.extend_from_slice(b"invalid suffix");
        let mut branch = document.fork();
        branch
            .put(automerge::ROOT, "missing dependency", 1)
            .unwrap();
        branch.save();
        branch.put(automerge::ROOT, "orphan", 2).unwrap();
        let missing = branch.save_incremental();
        let tails = [
            frame(false, partial, 2),
            frame(false, missing, 3),
            frame(false, b"opaque".to_vec(), 4),
            frame(false, b"opaque".to_vec(), 5),
        ];
        for (index, tail) in tails.iter().enumerate() {
            store.retain_delta(&[9; 32], index as u64, tail).unwrap();
        }
        document.put(automerge::ROOT, "new", 2).unwrap();
        let checkpoint = frame(true, document.save(), 6);
        store.retain_checkpoint(&[9; 32], &checkpoint).unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let replay = store.load_retained().unwrap();
        assert_eq!(replay.len(), 5);
        assert_eq!(replay[0], checkpoint);
        assert_eq!(&replay[1..], &tails);
    }

    #[test]
    fn failed_generation_commit_reopens_old_checkpoint_and_tail_then_resumes() {
        use automerge::transaction::Transactable;
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "old", 1).unwrap();
        let old = frame(true, document.save(), 1);
        document.put(automerge::ROOT, "tail", 2).unwrap();
        let tail = frame(false, document.save_incremental(), 2);
        document.put(automerge::ROOT, "new", 3).unwrap();
        let new = frame(true, document.save(), 3);
        store.retain_checkpoint(&[9; 32], &old).unwrap();
        store.retain_delta(&[9; 32], 1, &tail).unwrap();
        let uncovered = frame(false, b"opaque survivor".to_vec(), 4);
        store.retain_delta(&[9; 32], 2, &uncovered).unwrap();
        let lineage = store.root().join("retained/v2").join(format!(
            "{}-{}",
            hex::encode([1; 32]),
            hex::encode([9; 32])
        ));
        let active = current(&lineage).unwrap().unwrap();
        // Drive the real retention operation through staging so the injected
        // failure can target only CURRENT publication, before its rename.
        let mut build = store.start_retain(&[9; 32], None, new.clone()).unwrap();
        for _ in 0..20_000 {
            if matches!(build.phase, RetainPhase::Commit) {
                break;
            }
            assert!(matches!(
                build.step(WorkLimit::default()).unwrap(),
                Step::More
            ));
        }
        assert!(matches!(build.phase, RetainPhase::Commit));
        let failed = tilo_files::native::atomic::test_support::with_failure(
            tilo_files::native::atomic::test_support::Checkpoint::BeforePublish,
            || build.step(WorkLimit::default()),
        );
        assert!(failed.is_err());
        drop(build);
        drop(store);
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        assert_eq!(
            store.load_retained().unwrap(),
            vec![old, tail, uncovered.clone()]
        );
        assert_eq!(current(&lineage).unwrap().unwrap(), active);
        store.retain_checkpoint(&[9; 32], &new).unwrap();
        assert_eq!(
            store.load_retained().unwrap(),
            vec![new.clone(), uncovered.clone()]
        );
        document.put(automerge::ROOT, "next", 4).unwrap();
        let next_tail = frame(false, document.save_incremental(), 4);
        store.retain_delta(&[9; 32], 2, &next_tail).unwrap();
        // A crash after CURRENT commits but before old-generation GC leaves
        // obsolete files. Only CURRENT may select replay data after reopening.
        fs::create_dir(&active).unwrap();
        fs::write(active.join("checkpoint.delta"), message(true, 0)).unwrap();
        fs::write(active.join("00000000.delta"), message(false, 0)).unwrap();
        fs::create_dir(lineage.join("unknown-directory")).unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        assert_eq!(
            store.load_retained().unwrap(),
            vec![new.clone(), uncovered.clone(), next_tail.clone()]
        );
        store.retain_checkpoint(&[9; 32], &new).unwrap();
        assert!(!active.exists());
        assert!(lineage.join("unknown-directory").exists());
        assert_eq!(
            store.load_retained().unwrap(),
            vec![new, uncovered, next_tail]
        );
    }

    #[test]
    fn retained_metadata_mismatch_never_mutates_existing_coverage() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let cp = message(true, 1);
        store.retain_checkpoint(&[9; 32], &cp).unwrap();
        assert!(store.retain_checkpoint(&[8; 32], &cp).is_err());
        assert!(store.retain_delta(&[9; 32], 1, &cp).is_err());
        assert!(store.retain_checkpoint(&[9; 32], b"unknown").is_err());
        let wrong_workspace = WorkspaceStore::open(tmp.path(), 8, false).unwrap();
        assert!(wrong_workspace.retain_checkpoint(&[9; 32], &cp).is_err());
        assert!(wrong_workspace.load_retained().unwrap().is_empty());
        assert_eq!(store.load_retained().unwrap(), vec![cp]);
    }

    #[test]
    fn same_timestamp_tails_do_not_overwrite_distinct_payloads() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let first = message(false, 1);
        let second = message(false, 2);
        store.retain_delta(&[9; 32], 1, &first).unwrap();
        store.retain_delta(&[9; 32], 1, &second).unwrap();
        store.retain_delta(&[9; 32], 1, &first).unwrap();
        let replay = store.load_retained().unwrap();
        assert_eq!(replay.len(), 2);
        assert!(replay.contains(&first));
        assert!(replay.contains(&second));
    }

    #[test]
    fn checkpoint_discard_reclaims_only_inactive_flat_generations() {
        let tmp = tempfile::tempdir().unwrap();
        let lineage = tmp.path().join("lineage");
        let active_name = "11".repeat(16);
        let staged_name = "22".repeat(16);
        let active = lineage.join(&active_name);
        let staged = lineage.join(&staged_name);
        fs::create_dir_all(&active).unwrap();
        fs::create_dir(&staged).unwrap();
        fs::write(lineage.join("CURRENT"), active_name.as_bytes()).unwrap();
        fs::write(active.join("checkpoint.delta"), b"active").unwrap();
        for index in 0..3 {
            fs::write(staged.join(format!("{index:02}.delta")), b"staged").unwrap();
        }

        let mut discard = CheckpointDiscard::new(&lineage, staged.clone())
            .unwrap()
            .unwrap();
        let mut quanta = 0;
        loop {
            quanta += 1;
            if matches!(
                discard
                    .step(WorkLimit {
                        entries: 1,
                        ..Default::default()
                    })
                    .unwrap(),
                Step::Done(())
            ) {
                break;
            }
        }
        assert!(quanta > 1, "candidate cleanup must remain bounded");
        assert!(!staged.exists());
        assert!(active.join("checkpoint.delta").exists());

        fs::create_dir(&staged).unwrap();
        fs::write(staged.join("checkpoint.delta"), b"selected").unwrap();
        fs::write(lineage.join("CURRENT"), staged_name.as_bytes()).unwrap();
        assert!(
            CheckpointDiscard::new(&lineage, staged.clone())
                .unwrap()
                .is_none()
        );
        assert!(staged.join("checkpoint.delta").exists());

        fs::remove_file(lineage.join("CURRENT")).unwrap();
        fs::create_dir(lineage.join("CURRENT")).unwrap();
        assert!(CheckpointDiscard::new(&lineage, staged.clone()).is_err());
        assert!(
            staged.join("checkpoint.delta").exists(),
            "an unreadable CURRENT must preserve the candidate"
        );
    }
}
