//! Shared retained coverage. Callers serialize writes with the workspace lock.
//!
//! A lineage is an authenticated envelope author plus channel, never a timestamp.
//! CURRENT selects durable shared replay coverage. Opaque records append in
//! receipt order; only proven raw Automerge ancestry permits collection. A
//! replacement generation is collected only after its CURRENT selector is synced.
//! An opaque checkpoint cannot prove that a queue entry is covered merely
//! because its filename or timestamp is older, unless the retention
//! classifier calls its message standalone: then it is the whole of its
//! author's route, and replaces what that author retained there before.

use super::{RETAINED_DIR, StoreError, WorkspaceStore};
use crate::store::bounded::Invariant;
use crate::{coedit::CoeditPayload, transport::Envelope};
use automerge::ReadDoc;
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use tilo_automerge_isolate::{Applied, Dependencies};
use tilo_relay_store::durable::SELECTION_PREFIX;
#[cfg(unix)]
pub(crate) use tilo_relay_store::durable::stamp;
#[cfg(feature = "agents")]
use tilo_relay_store::durable::sync_file_once;
pub(crate) use tilo_relay_store::durable::{
    GenerationGc, create_dir, current, durable_write, sync_dir, sync_dir_once,
};
use tilo_relay_store::frames::framed_envelope;
pub(crate) use tilo_relay_store::frames::{body_of, complete_coedit_payload, payload};

mod tail_index;

fn invalid(message: &str) -> StoreError {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
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

/// The author and channel of a checkpoint whose message is the whole of its
/// route ([`crate::payloads::Checkpoint::standalone`]), as `payloads` reads it.
fn standalone(
    payloads: &dyn crate::payloads::ShellPayloads,
    bytes: &[u8],
    workspace: u64,
) -> Option<([u8; 32], [u8; 32])> {
    let (author, CoeditPayload::Checkpoint { channel, body }) = payload(bytes, workspace)? else {
        return None;
    };
    payloads
        .checkpoint(&channel, &body)?
        .standalone
        .then_some((author, channel))
}

/// Whether standalone `checkpoint` replaces `prior`: retained earlier by the
/// same author on the same channel.
fn replaces(
    payloads: &dyn crate::payloads::ShellPayloads,
    checkpoint: &[u8],
    prior: &[u8],
    workspace: u64,
) -> bool {
    standalone(payloads, checkpoint, workspace).is_some_and(|(author, channel)| {
        identity(prior, workspace).is_some_and(|(old_author, old_channel, _)| {
            old_author == author && old_channel == channel
        })
    })
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
///
/// Retained bodies are peer-supplied. Reading their frames decodes nothing;
/// every Automerge decode of them goes through `tilo_automerge_isolate`, so a
/// hostile body costs a worker process, never the relay.
fn independently_valid_chunks(bytes: &[u8]) -> bool {
    if bytes.is_empty()
        || tilo_automerge_isolate::precheck::check(
            bytes,
            tilo_automerge_isolate::Isolate::shared().limits(),
        )
        .is_err()
    {
        return false;
    }
    let mut rest = bytes;
    while !rest.is_empty() {
        let Some((chunk, after, kind)) = first_chunk(rest) else {
            return false;
        };
        match tilo_automerge_isolate::apply(
            &mut automerge::AutoCommit::new(),
            chunk,
            Dependencies::RequireComplete,
        ) {
            Ok(Applied::Changed(_)) => {}
            Ok(Applied::MissingDependencies) if kind != 0 => {}
            Ok(Applied::MissingDependencies) | Err(_) => return false,
        }
        rest = after;
    }
    true
}

/// Only complete, strictly decoded raw Automerge snapshots prove coverage.
/// In particular a bare `load_incremental` is unsuitable here: it accepts
/// partial input, so unchanged heads after that operation can hide malformed
/// data. The isolate refuses an input with any malformed chunk whole.
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
    let Ok(mut document) = tilo_automerge_isolate::load(&body) else {
        return false;
    };
    if !document.get_missing_deps(&[]).is_empty() {
        return false;
    }
    // A full snapshot provides dependencies for incremental tail chunks.
    // Contained: applied to a copy of the snapshot, the prior body leaves its
    // heads where they were and nothing waiting for a dependency.
    matches!(
        tilo_automerge_isolate::apply(&mut document, &old_body, Dependencies::RequireComplete),
        Ok(Applied::Changed(false))
    )
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
/// matches is read, and then compared in full. A scan that builds the tail
/// index (`collected`) takes every record's digest.
fn inspect_delta(
    entry: &fs::DirEntry,
    name: &str,
    (bytes, digest): (&[u8], &[u8; 32]),
    last: &mut u64,
    duplicate: &mut bool,
    collected: Option<&mut std::collections::HashSet<u64>>,
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
    let known = if let Some(collected) = collected {
        let record = record_digest(&entry.path(), &metadata)?;
        collected.insert(tail_index::short(&record));
        Some(record)
    } else {
        None
    };
    if metadata.len() != bytes.len() as u64 {
        return Ok(());
    }
    let known = if let Some(known) = known {
        known
    } else {
        record_digest(&entry.path(), &metadata)?
    };
    if known != *digest {
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

/// The stamp of an active generation the tail index is kept at; none where
/// there are no stamps, which leaves every commit to the scan.
fn generation_stamp(path: &Path) -> Option<tail_index::Stamp> {
    #[cfg(unix)]
    {
        stamp(path).ok()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Run independent barriers at once. Concurrent fsyncs share the file
/// system's journal commits: a commit whose barriers order nothing between
/// them waits about as long as for one of them instead of for each in turn.
fn barriers(
    work: &mut [&mut (dyn FnMut() -> Result<(), StoreError> + Send)],
) -> Result<(), StoreError> {
    let Some((first, rest)) = work.split_first_mut() else {
        return Ok(());
    };
    std::thread::scope(|scope| {
        let others: Vec<_> = rest
            .iter_mut()
            .map(|barrier| {
                scope.spawn(move || {
                    let result = barrier();
                    (result, crate::store::helper_fsyncs())
                })
            })
            .collect();
        let mut result = first();
        for other in others {
            let other = match other.join() {
                Ok((other, fsyncs)) => {
                    crate::store::credit_fsyncs(fsyncs);
                    other
                }
                Err(_) => Err(invalid("a barrier panicked")),
            };
            if result.is_ok() {
                result = other;
            }
        }
        result
    })
}

/// [`create_dir`] for `lineage` in `v2`, except that a lineage this
/// creates leaves its entry unsynced: returns `v2` then, for the caller to
/// sync beside its own barriers.
fn create_lineage(v2: PathBuf, lineage: &Path) -> Result<Option<PathBuf>, StoreError> {
    if !lineage.is_dir() {
        match fs::create_dir(lineage) {
            Ok(()) => return Ok(Some(v2)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    create_dir(lineage)?;
    Ok(None)
}

/// Whether anything is at `path`.
fn exists(path: &Path) -> Result<bool, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Create `path` with `bytes` and sync it; its entry is the caller's to
/// sync.
fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    use std::io::Write as _;
    #[cfg(test)]
    super::test_barrier::check(path, "sync-file")?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    crate::store::fsync(&file)?;
    Ok(())
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
    // A checkpoint alone was synced with its directory by `durable_write`,
    // and an empty generation has no entries to sync.
    if !retained.is_empty() {
        sync_dir(&generation)?;
    }
    durable_write(&lineage.join("CURRENT"), name.as_bytes())?;
    Ok(generation)
}

enum RetainPhase {
    Scan(fs::ReadDir),
    /// A delta whose lineage the tail index describes: nothing to list.
    Indexed,
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
pub struct RetainBuild {
    lineage: PathBuf,
    active: PathBuf,
    /// The lineage has no generation yet. `active` names the one an
    /// append selects and writes into; a staged checkpoint becomes the
    /// first instead.
    fresh: bool,
    /// The directory holding the lineage, when this build created the
    /// lineage and has not yet made its entry durable: a checkpoint does
    /// that beside its own barriers.
    unsynced_parent: Option<PathBuf>,
    /// The CURRENT naming `staged`, durable beside the lineage's own: the
    /// commit renames it over CURRENT.
    pending_current: Option<PathBuf>,
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
    /// Every entry of `staged` is durable: the last change to it was a
    /// [`durable_write`] into it, which synced it after its rename.
    staged_synced: bool,
    staged_append: bool,
    survivors: u64,
    written: u64,
    shell_store: Option<WorkspaceStore>,
    // Boxed, as `fast` is: inline they made a build 54 KB, and the build
    // moves through the blocking pool on every quantum of the edit it serves.
    selected_read: Option<Box<dyn super::shell_selection::SelectedReplay>>,
    selection: Option<Box<dyn super::shell_selection::ShellSelection>>,
    publication: Option<super::shell_selection::Publication>,
    /// A plain route tail the in-process tail cache can publish without a
    /// selection; `fast_admitted` once the selected snapshot found it safe.
    fast: Option<Box<dyn super::shell_selection::ShellTail>>,
    fast_admitted: bool,
    selected_obsolete: bool,
    staged_only: bool,
    continuity: Option<super::continuity::Receipt>,
    store: WorkspaceStore,
    /// A delta's active generation, stamped when the build began.
    stamp: Option<tail_index::Stamp>,
    /// `last` came from the tail index, without listing the generation.
    indexed: bool,
    /// Every record digest a delta's full scan saw, to install as the index.
    collected: Option<std::collections::HashSet<u64>>,
}
/// The durable stage of a lineage's first checkpoint, written outside the
/// workspace's order ([`WorkspaceStore::stage_fresh_checkpoint`]) and not
/// yet selected.
pub(crate) struct FreshCheckpoint {
    lineage: PathBuf,
    staged: PathBuf,
    selection: PathBuf,
    name: String,
}
impl FreshCheckpoint {
    /// The lineage and the stage, for a [`CheckpointDiscard`]: it leaves the
    /// stage alone once the lineage selected it.
    pub(crate) fn discard_paths(&self) -> (PathBuf, PathBuf) {
        (self.lineage.clone(), self.staged.clone())
    }
    /// Remove a stage no owner turn took: its checkpoint, its directory and
    /// the CURRENT that would have named it. One the lineage selected stays.
    /// Best effort: what is left is collected with the lineage's next commit.
    pub(crate) fn discard(self) {
        if current(&self.lineage).ok().flatten().as_ref() == Some(&self.staged) {
            return;
        }
        for result in [
            fs::remove_file(self.staged.join("checkpoint.delta")),
            fs::remove_dir(&self.staged),
            fs::remove_file(&self.selection),
        ] {
            if let Err(error) = result
                && error.kind() != io::ErrorKind::NotFound
            {
                tracing::debug!(%error, "an untaken checkpoint stage was left for collection");
            }
        }
    }
}
/// A checkpoint staged as a projected lineage generation, promoted with the
/// batch it belongs to.
pub struct CheckpointStage {
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
    /// The generation this stage projects for its lineage.
    pub fn projected_generation(&self) -> &Path {
        self.build.staged.as_deref().unwrap_or(&self.build.active)
    }
    /// The lineage directory's name.
    ///
    /// # Errors
    ///
    /// Returns an error when the lineage path has no name.
    pub fn lineage_name(&self) -> Result<&std::ffi::OsStr, StoreError> {
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
    /// One quantum of staging; done with whether the projected generation
    /// may be promoted with its batch.
    ///
    /// # Errors
    ///
    /// Returns an error when the lineage cannot be read or staged.
    pub fn step(
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
    /// Make the projected generation the lineage's `CURRENT`.
    ///
    /// # Errors
    ///
    /// Returns an error when `CURRENT` cannot be switched durably.
    pub fn promote(&mut self) -> Result<(), StoreError> {
        if matches!(self.build.phase, RetainPhase::Commit) {
            let staged = self
                .build
                .staged
                .as_ref()
                .invariant("staged missing")?
                .clone();
            tail_index::forget(&self.build.lineage);
            sync_dir(&staged)?;
            durable_write(
                &self.build.lineage.join("CURRENT"),
                staged
                    .file_name()
                    .invariant("staged missing")?
                    .as_encoded_bytes(),
            )?;
            self.build.written = self.build.bytes.len() as u64;
            self.build.phase = self.build.collect(false)?;
        }
        Ok(())
    }
    /// Collect what the promotion replaced.
    ///
    /// # Errors
    ///
    /// Returns an error when collection fails.
    pub fn finish_gc(
        &mut self,
        limit: super::bounded::WorkLimit,
    ) -> Result<super::bounded::Step<()>, StoreError> {
        use super::bounded::Step;
        #[cfg(test)]
        super::test_barrier::check(&self.build.lineage, "checkpoint-stage-finish-gc")?;
        match &mut self.build.phase {
            RetainPhase::Collect(gc) => {
                if gc.step(limit)? == Step::Done(()) {
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
    /// Whether this build made its scratch directory: a checkpoint's ordered
    /// pass and a shell selection do; a plain delta never does, so its owner
    /// has nothing to dispose of afterwards.
    pub(crate) fn holds_scratch(&self) -> bool {
        self.checkpoint || self.selection.is_some()
    }
    /// What may already be visible after a failed [`Self::step`].
    pub fn publication_on_error(&self) -> Option<super::shell_selection::Publication> {
        self.selection
            .as_deref()
            .and_then(super::shell_selection::ShellSelection::publication_on_error)
    }
    #[cfg(test)]
    pub(crate) fn stats(&self) -> &super::bounded::WorkStats {
        self.selection
            .as_ref()
            .map_or(&self.sorter.stats, |s| s.stats())
    }
    /// The retention pass's own work.
    pub fn retention_stats(&self) -> &super::bounded::WorkStats {
        &self.sorter.stats
    }
    /// The shell selection's work, when one ran.
    pub fn selection_stats(&self) -> Option<&super::bounded::WorkStats> {
        self.selection
            .as_deref()
            .map(super::shell_selection::ShellSelection::stats)
    }
    pub(crate) fn selected_obsolete(&self) -> bool {
        self.selected_obsolete
    }
    /// Collect the generations the lineage's CURRENT no longer names; done
    /// at once while a capture is leased, which collects them later
    /// ([`WorkspaceStore::collect_or_defer`]).
    fn collect(&self, after_commit: bool) -> Result<RetainPhase, StoreError> {
        Ok(
            match self
                .store
                .collect_or_defer(self.lineage.clone(), after_commit)?
            {
                Some(gc) => RetainPhase::Collect(Box::new(gc)),
                None => RetainPhase::Done,
            },
        )
    }
    /// Every prior record was inspected: collect a duplicate, stage a
    /// checkpoint, or append.
    fn inspected(&mut self) -> Result<(), StoreError> {
        let unsynced_parent = self.unsynced_parent.take();
        let stage_checkpoint = self.continuity.is_none()
            && self.checkpoint
            && !self.opaque
            && (covers(&self.bytes, &self.bytes, self.workspace)
                || standalone(&**self.store.payloads(), &self.bytes, self.workspace).is_some());
        if let Some(parent) = unsynced_parent.as_deref()
            && (self.duplicate || !stage_checkpoint)
        {
            sync_dir_once(parent)?;
        }
        if self.duplicate && self.continuity.is_none() {
            self.phase = if self.staged_only {
                RetainPhase::Done
            } else {
                self.collect(false)?
            };
        } else if stage_checkpoint {
            let name = hex::encode(rand::random::<[u8; 16]>());
            let staged = self.lineage.join(&name);
            fs::create_dir(&staged)?;
            // The checkpoint, the CURRENT that will name its stage and a new
            // lineage's own entry order nothing between them, nor do the
            // entries of the first two after them; all hold before the
            // commit renames the CURRENT into place.
            let checkpoint = staged.join("checkpoint.delta");
            let selection = self.lineage.join(format!("{SELECTION_PREFIX}{name}"));
            let bytes = &self.bytes;
            barriers(&mut [
                &mut || write_synced(&checkpoint, bytes),
                &mut || write_synced(&selection, name.as_bytes()),
                &mut || unsynced_parent.as_deref().map_or(Ok(()), sync_dir_once),
            ])?;
            let lineage = &self.lineage;
            barriers(&mut [&mut || sync_dir(&staged), &mut || sync_dir(lineage)])?;
            self.pending_current = Some(selection);
            self.staged = Some(staged);
            self.staged_synced = true;
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
            if self.indexed && exists(&path)? {
                // Written behind the index's back: never overwrite it; list.
                return self.rescan();
            }
            if std::mem::take(&mut self.fresh) {
                // An append goes into the selected generation, where a
                // continuity marker reserves its slot: select it first.
                create_dir(&self.active)?;
                let name = self
                    .active
                    .file_name()
                    .invariant("active generation name")?;
                durable_write(&self.lineage.join("CURRENT"), name.as_encoded_bytes())?;
            }
            if let Some(receipt) = &self.continuity {
                super::continuity::account_validation(
                    &**self.store.payloads(),
                    &mut self.sorter.stats,
                    self.bytes.len(),
                );
                super::continuity::write(&self.store, &path, &self.bytes, receipt)?;
            }
            durable_write(&path, &self.bytes)?;
            remember_digest(&path, &self.digest);
            if let (Some(before), Some(after)) = (&self.stamp, generation_stamp(&self.active)) {
                tail_index::appended(&self.lineage, (before, after), next, &self.digest);
            } else {
                tail_index::forget(&self.lineage);
            }
            self.written = self.bytes.len() as u64;
            self.phase = RetainPhase::Done;
        }
        Ok(())
    }
    /// Install what this delta's full scan found as the lineage's index.
    fn install_index(&mut self) {
        if let (Some(stamp), Some(digests)) = (self.stamp, self.collected.take())
            && !self.duplicate
        {
            tail_index::install(&self.lineage, stamp, self.last, digests);
        }
    }
    /// The index no longer matches the generation: list it after all.
    fn rescan(&mut self) -> Result<(), StoreError> {
        tail_index::forget(&self.lineage);
        self.indexed = false;
        self.last = 0;
        self.stamp = generation_stamp(&self.active);
        self.collected = self.stamp.map(|_| std::collections::HashSet::new());
        self.phase = RetainPhase::Scan(fs::read_dir(&self.active)?);
        Ok(())
    }
    #[expect(
        clippy::too_many_lines,
        reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
    )]
    /// One quantum of retention; done with what to publish.
    ///
    /// # Errors
    ///
    /// Returns an error when the lineage cannot be read or written, or the
    /// shell selection fails; [`Self::publication_on_error`] then says what
    /// may already be visible.
    pub fn step(
        &mut self,
        limit: super::bounded::WorkLimit,
    ) -> Result<super::bounded::Step<super::shell_selection::RetainOutcome>, StoreError> {
        use super::bounded::{Meta, Step, increment, read_meta};
        let limit = limit.checked()?;
        if let Some(fast) = &self.fast
            && !self.fast_admitted
        {
            use super::shell_selection::Admission;
            match fast.admission(&self.bytes, self.workspace) {
                Admission::Obsolete => {
                    self.publication = Some(super::shell_selection::Publication::Suppress);
                    self.phase = RetainPhase::Done;
                    self.selected_obsolete = true;
                    let fast = self.fast.take().invariant("fast tail missing")?;
                    fast.finish(&self.store.tail_cache);
                }
                Admission::Tail => self.fast_admitted = true,
                Admission::Select => {
                    self.fast = None;
                    self.store.tail_cache.invalidate();
                    self.selected_read = self.store.selected_read()?;
                }
            }
            return Ok(Step::More);
        }
        if let Some(reader) = &mut self.selected_read {
            if reader.step()? == Step::Done(()) {
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
        if matches!(self.phase, RetainPhase::Sort)
            && let Some(root) = self.scratch.parent().and_then(Path::parent)
        {
            super::test_barrier::pause(root, "retain-sort");
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
                            self.install_index();
                            self.inspected()?;
                        }
                        return Ok(Step::More);
                    };
                    let entry = entry?;
                    crate::store::count_listed_entry();
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
                                self.collected.as_mut(),
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
            RetainPhase::Indexed => self.inspected()?,
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
                            && !covers(&record.bytes, &record.bytes, self.workspace)
                            && !replaces(
                                &**self.store.payloads(),
                                &self.bytes,
                                &record.bytes,
                                self.workspace,
                            );
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
                                    &**self.store.payloads(),
                                    &mut self.sorter.stats,
                                    self.bytes.len(),
                                );
                                super::continuity::write(&self.store, &path, &self.bytes, receipt)?;
                            }
                            durable_write(&path, &self.bytes)?;
                            self.staged_synced = true;
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
                    } else if !covers(&self.bytes, &record.bytes, self.workspace)
                        && !replaces(
                            &**self.store.payloads(),
                            &self.bytes,
                            &record.bytes,
                            self.workspace,
                        )
                    {
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
                        self.staged_synced = false;
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
                tail_index::forget(&self.lineage);
                if !self.staged_synced {
                    sync_dir(staged)?;
                }
                if let Some(selection) = self.pending_current.take() {
                    #[cfg(test)]
                    super::test_barrier::check(&self.lineage, "current-publish")?;
                    fs::rename(&selection, self.lineage.join("CURRENT"))?;
                    sync_dir(&self.lineage)?;
                } else {
                    durable_write(
                        &self.lineage.join("CURRENT"),
                        staged
                            .file_name()
                            .invariant("staged missing")?
                            .as_encoded_bytes(),
                    )?;
                }
                self.written = self.bytes.len() as u64;
                self.phase = self.collect(true)?;
            }
            RetainPhase::Collect(gc) => {
                if gc.step(limit)? == Step::Done(()) {
                    self.phase = RetainPhase::Done;
                }
            }
            RetainPhase::Done => {
                if self.publication.is_none()
                    && let Some(mut fast) = self.fast.take()
                    && !self.duplicate
                    && fast.append(&self.bytes, self.workspace)
                {
                    // The selection this skips would publish this tail as an
                    // original and change nothing but its listed tails.
                    fast.finish(&self.store.tail_cache);
                    self.publication = Some(super::shell_selection::Publication::Original);
                }
                if self.publication.is_none() {
                    if let Some(store) = &self.shell_store {
                        if self.selection.is_none() {
                            let incoming = tilo_relay_store::frames::incoming_identity(
                                &self.bytes,
                                self.workspace,
                            );
                            let binding =
                                store.shell_binding().invariant("shell binding missing")?;
                            let mut selection =
                                binding.selection(store, self.scratch.clone(), incoming)?;
                            if !self.checkpoint
                                && let Some(key) =
                                    binding.incoming_route(&self.bytes, self.workspace)
                            {
                                selection.want_route(key);
                            }
                            self.selection = Some(selection);
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

    /// Begin retaining `bytes` on `channel`: a delta at `seq`, or a
    /// checkpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when `bytes` is not a retained envelope of this
    /// channel and kind, or the lineage cannot be prepared.
    pub fn start_retain(
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
    /// Stage the checkpoint `bytes` on `channel` as a projected generation
    /// under `scratch`, without switching `CURRENT`.
    ///
    /// # Errors
    ///
    /// Returns an error when `bytes` is not a checkpoint of this channel or
    /// the lineage cannot be prepared.
    pub fn start_staged_checkpoint(
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
    /// Whether `author`'s lineage on `channel` has a generation: one stat,
    /// for a caller deciding whether a checkpoint may start it.
    pub(crate) fn lineage_has_generation(&self, author: &[u8; 32], channel: &[u8; 32]) -> bool {
        self.root
            .join(RETAINED_DIR)
            .join("v2")
            .join(format!("{}-{}", hex::encode(author), hex::encode(channel)))
            .join("CURRENT")
            .exists()
    }
    /// Stage the checkpoint `bytes` on `channel` as the first generation of
    /// a lineage that has none yet, and make the stage durable, all outside
    /// the workspace's order: everything [`RetainBuild`] writes before its
    /// commit for such a checkpoint, which depends on nothing retained. The
    /// owner's turn is left with the commit, [`Self::start_fresh_retain`]:
    /// one barrier instead of three rounds of them, for the identity record
    /// each newcomer of a room retains first.
    ///
    /// `None` when the lineage has a generation, the workspace has retained
    /// nothing yet, or the checkpoint is one [`RetainBuild`] would not stage
    /// as a generation of its own (it neither covers itself nor stands
    /// alone).
    ///
    /// Stages of other lineages, and the owner, change the retained root
    /// and this lineage meanwhile, so every barrier here is a real one: a
    /// remembered sync ([`sync_dir_once`]) could vouch for an entry made
    /// after it began. A stage that is never committed is discarded by the
    /// owner turn that falls back from it ([`CheckpointDiscard`]), or by its
    /// stager when no turn takes it ([`FreshCheckpoint::discard`]); what a
    /// crash leaves is collected with the lineage's next commit
    /// ([`GenerationGc`]).
    ///
    /// # Errors
    ///
    /// Returns an error when `bytes` is not a checkpoint of this channel or
    /// the stage cannot be written.
    pub(crate) fn stage_fresh_checkpoint(
        &self,
        channel: &[u8; 32],
        bytes: &[u8],
    ) -> Result<Option<FreshCheckpoint>, StoreError> {
        let (author, actual_channel, checkpoint) = identity(bytes, self.workspace_id)
            .ok_or_else(|| invalid("invalid retained envelope"))?;
        if actual_channel != *channel || !checkpoint {
            return Err(invalid("retained envelope channel or kind mismatch"));
        }
        let v2 = self.root.join(RETAINED_DIR).join("v2");
        let lineage = v2.join(format!("{}-{}", hex::encode(author), hex::encode(channel)));
        if !v2.is_dir()
            || current(&lineage)?.is_some()
            || (!covers(bytes, bytes, self.workspace_id)
                && standalone(&**self.payloads(), bytes, self.workspace_id).is_none())
        {
            return Ok(None);
        }
        for directory in self.root.ancestors().take(3) {
            sync_dir_once(directory)?;
        }
        match fs::create_dir(&lineage) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let name = hex::encode(rand::random::<[u8; 16]>());
        let staged = lineage.join(&name);
        fs::create_dir(&staged)?;
        let checkpoint = staged.join("checkpoint.delta");
        let selection = lineage.join(format!("{SELECTION_PREFIX}{name}"));
        // As `RetainBuild::inspected` stages one: the checkpoint, the
        // CURRENT that will name its stage and the lineage's own entry
        // order nothing between them, nor do the entries of the first two
        // after them. The lineage's entry is synced even when another stage
        // made it, which may not have synced it yet.
        barriers(&mut [
            &mut || write_synced(&checkpoint, bytes),
            &mut || write_synced(&selection, name.as_bytes()),
            &mut || sync_dir(&v2),
        ])?;
        barriers(&mut [&mut || sync_dir(&staged), &mut || sync_dir(&lineage)])?;
        Ok(Some(FreshCheckpoint {
            lineage,
            staged,
            selection,
            name,
        }))
    }
    /// Begin committing the stage of [`Self::stage_fresh_checkpoint`] in
    /// the workspace's order: the build renames the stage's CURRENT into
    /// place and syncs the lineage, then goes on as any checkpoint's build.
    /// The stage comes back, with `bytes`, when the lineage has a generation
    /// by now or the stage is not what was written: retain `bytes` with
    /// [`Self::start_retain`] then, and discard the stage.
    ///
    /// # Errors
    ///
    /// Returns an error when `bytes` is not a checkpoint of the stage's
    /// lineage or the lineage cannot be read.
    pub(crate) fn start_fresh_retain(
        &self,
        channel: &[u8; 32],
        fresh: FreshCheckpoint,
        bytes: Vec<u8>,
    ) -> Result<Result<RetainBuild, (FreshCheckpoint, Vec<u8>)>, StoreError> {
        let (author, actual_channel, checkpoint) = identity(&bytes, self.workspace_id)
            .ok_or_else(|| invalid("invalid retained envelope"))?;
        let lineage = self.root.join(RETAINED_DIR).join("v2").join(format!(
            "{}-{}",
            hex::encode(author),
            hex::encode(channel)
        ));
        if actual_channel != *channel || !checkpoint || lineage != fresh.lineage {
            return Err(invalid("retained envelope channel or kind mismatch"));
        }
        // Nothing but a commit of this lineage removes a stage of it, and a
        // commit leaves a CURRENT: an intact stage under no CURRENT is the
        // durable one its writer made.
        let intact = current(&lineage)?.is_none()
            && fs::read(fresh.staged.join("checkpoint.delta")).is_ok_and(|held| held == bytes)
            && fs::read(&fresh.selection).is_ok_and(|held| held == fresh.name.as_bytes());
        if !intact {
            return Ok(Err((fresh, bytes)));
        }
        // As any write but a delta the tail cache admits.
        self.tail_cache.invalidate();
        let scratch = self.root.join("store-work-v1").join(format!(
            "retain-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        Ok(Ok(RetainBuild {
            lineage,
            active: fresh.staged.clone(),
            fresh: false,
            unsynced_parent: None,
            pending_current: Some(fresh.selection),
            sorter: super::bounded::Sorter::unspilled(scratch.join("sort")),
            scratch,
            digest: *blake3::hash(&bytes).as_bytes(),
            key: checkpoint_key(&bytes, self.workspace_id),
            bytes,
            checkpoint: true,
            workspace: self.workspace_id,
            sorted: None,
            phase: RetainPhase::Commit,
            duplicate: false,
            opaque: false,
            last: 0,
            staged: Some(fresh.staged),
            staged_synced: true,
            staged_append: false,
            survivors: 0,
            written: 0,
            shell_store: self.shell_binding().map(|_| self.clone()),
            selected_read: self.selected_read()?,
            selection: None,
            publication: None,
            fast: None,
            fast_admitted: false,
            selected_obsolete: false,
            staged_only: false,
            continuity: None,
            store: self.clone(),
            stamp: None,
            indexed: false,
            collected: None,
        }))
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
        let fast = if checkpoint {
            None
        } else {
            self.checkout_tail(&bytes)?
        };
        if fast.is_none() {
            // Every other write changes retained history the cached cursors
            // were computed from; the next clean selection seeds them again.
            self.tail_cache.invalidate();
        }
        for directory in self.root.ancestors().take(3) {
            sync_dir_once(directory)?;
        }
        let v2 = self.root.join(RETAINED_DIR).join("v2");
        create_dir(&v2)?;
        let lineage = v2.join(format!("{}-{}", hex::encode(author), hex::encode(channel)));
        let unsynced_parent = create_lineage(v2, &lineage)?;
        let (active, fresh) = match current(&lineage)? {
            Some(active) => (active, false),
            // A checkpoint stages a generation of its own, so a new lineage
            // takes it as its first rather than committing an empty one
            // before it: a newcomer's first changes cost a selector write and
            // a collection less each.
            None if checkpoint => (lineage.join(hex::encode(rand::random::<[u8; 16]>())), true),
            None => (commit_generation(&lineage, &[], None)?, false),
        };
        let scratch_root = self.root.join("store-work-v1");
        let scratch = scratch.unwrap_or_else(|| {
            scratch_root.join(format!(
                "retain-{}",
                hex::encode(rand::random::<[u8; 16]>())
            ))
        });
        // Only a checkpoint's ordered pass spills metadata to scratch. A
        // delta's scan compares sizes and cached digests and its append names
        // the next ordinal, so it creates no directory to remove afterwards;
        // a shell selection creates the scratch it needs itself.
        let sorter = if checkpoint {
            fs::create_dir_all(&scratch_root)?;
            fs::create_dir(&scratch)?;
            super::bounded::Sorter::new(scratch.join("sort"))?
        } else {
            super::bounded::Sorter::unspilled(scratch.join("sort"))
        };
        let digest = *blake3::hash(&bytes).as_bytes();
        // A delta needs the generation's last ordinal and whether it already
        // holds these bytes; the tail index answers both without listing it.
        let stamp = if checkpoint {
            None
        } else {
            generation_stamp(&active)
        };
        let indexed = stamp
            .as_ref()
            .and_then(|stamp| tail_index::last(&lineage, stamp, &digest));
        let phase = if fresh {
            // Nothing to list: an empty ordered pass.
            RetainPhase::Sort
        } else if indexed.is_some() {
            RetainPhase::Indexed
        } else {
            RetainPhase::Scan(fs::read_dir(&active)?)
        };
        let key = checkpoint_key(&bytes, self.workspace_id);
        Ok(RetainBuild {
            lineage,
            active,
            fresh,
            unsynced_parent,
            pending_current: None,
            scratch,
            digest,
            bytes,
            checkpoint,
            workspace: self.workspace_id,
            sorter,
            sorted: None,
            phase,
            duplicate: false,
            opaque: false,
            last: indexed.unwrap_or(0),
            key,
            staged: None,
            staged_synced: false,
            staged_append: false,
            survivors: 0,
            written: 0,
            shell_store: self.shell_binding().map(|_| self.clone()),
            selected_read: if fast.is_some() {
                None
            } else {
                self.selected_read()?
            },
            selection: None,
            publication: None,
            fast,
            fast_admitted: false,
            selected_obsolete: false,
            staged_only: false,
            continuity: None,
            store: self.clone(),
            collected: stamp
                .filter(|_| indexed.is_none())
                .map(|_| std::collections::HashSet::new()),
            indexed: indexed.is_some(),
            stamp,
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

    /// Read-only channel projection with limits checked before file allocation.
    /// Uses the same committed selection and active lineage selectors as replay;
    /// never publishes a generation or confers collection authority.
    #[cfg(feature = "agents")]
    pub(crate) fn load_view_channel(
        &self,
        channel: &[u8; 32],
        max_bytes: usize,
    ) -> Result<Vec<Vec<u8>>, StoreError> {
        let mut budget = ViewReadBudget {
            root: self.root.clone(),
            bytes: max_bytes,
            entries: 8192,
        };
        let mut out = Vec::new();
        let selection = self.root.join(super::shell_selection::DIRECTORY);
        budget.selection(&selection)?;
        if let Some(selected) = current(&selection)? {
            budget.barrier(&selected.join("basis.manifest"))?;
            let mut manifest = fs::File::open(selected.join("basis.manifest"))?;
            while let Some(meta) = super::bounded::read_meta(&mut manifest)? {
                budget.entry()?;
                // Selected basis records can span channels; their ciphertext is
                // conservatively charged to the same read budget before routing.
                let bytes = budget.read(&self.root.join(meta.source))?;
                if view_channel_matches(&bytes, self.workspace_id, channel) {
                    out.push(bytes);
                }
            }
        }
        let v2 = self.root.join(RETAINED_DIR).join("v2");
        if !v2.exists() {
            return Ok(out);
        }
        let suffix = format!("-{}", hex::encode(channel));
        let mut lineages = Vec::new();
        for entry in fs::read_dir(v2)? {
            budget.entry()?;
            let entry = entry?;
            if entry.file_type()?.is_dir() && entry.file_name().to_string_lossy().ends_with(&suffix)
            {
                lineages.push(entry);
            }
        }
        lineages.sort_by_key(fs::DirEntry::file_name);
        for lineage in lineages {
            budget.selection(&lineage.path())?;
            let Some(active) = current(&lineage.path())? else {
                continue;
            };
            let mut paths = Vec::new();
            for entry in fs::read_dir(active)? {
                budget.entry()?;
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.ends_with(".delta")
                    && !name.starts_with('.')
                    && entry.file_type()?.is_file()
                {
                    paths.push(entry);
                }
            }
            paths.sort_by_key(|entry| (entry.file_name() != "checkpoint.delta", entry.file_name()));
            for entry in paths {
                let bytes = budget.read(&entry.path())?;
                if !view_channel_matches(&bytes, self.workspace_id, channel) {
                    return Err(invalid("retained view channel mismatch"));
                }
                out.push(bytes);
            }
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|bytes| seen.insert(*blake3::hash(bytes).as_bytes()));
        Ok(out)
    }

    /// Every retained frame a compatibility or admin read sees, the
    /// committed shell selection first.
    ///
    /// # Errors
    ///
    /// Returns an error when a retained file cannot be read.
    pub fn load_shared(&self) -> Result<Vec<Vec<u8>>, StoreError> {
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

#[cfg(feature = "agents")]
fn view_channel_matches(bytes: &[u8], workspace: u64, channel: &[u8; 32]) -> bool {
    payload(bytes, workspace).is_some_and(|(_, value)| {
        let (CoeditPayload::Checkpoint {
            channel: actual, ..
        }
        | CoeditPayload::RetainedDelta {
            channel: actual, ..
        }) = value;
        actual == *channel
    })
}

#[cfg(feature = "agents")]
struct ViewReadBudget {
    root: PathBuf,
    bytes: usize,
    entries: usize,
}

#[cfg(feature = "agents")]
impl ViewReadBudget {
    fn selection(&self, lineage: &Path) -> Result<(), StoreError> {
        let selector = lineage.join("CURRENT");
        if selector.exists() {
            self.barrier(&selector)?;
        }
        Ok(())
    }
    fn barrier(&self, path: &Path) -> Result<(), StoreError> {
        sync_file_once(path)?;
        for parent in path.ancestors().skip(1) {
            sync_dir_once(parent)?;
            if Some(parent) == self.root.parent() {
                break;
            }
        }
        Ok(())
    }

    fn entry(&mut self) -> Result<(), StoreError> {
        self.entries = self
            .entries
            .checked_sub(1)
            .ok_or_else(|| invalid("view read entry budget exceeded"))?;
        Ok(())
    }
    fn read(&mut self, path: &Path) -> Result<Vec<u8>, StoreError> {
        use std::io::Read as _;
        self.barrier(path)?;
        let file = fs::File::open(path)?;
        let size = usize::try_from(file.metadata()?.len())
            .map_err(|_| invalid("oversized view source"))?;
        if size > self.bytes {
            return Err(invalid("view read byte budget exceeded"));
        }
        let mut bytes = Vec::new();
        // Limit the read too, so an unexpectedly growing file cannot exceed it.
        file.take(self.bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)?;
        self.bytes = self
            .bytes
            .checked_sub(bytes.len())
            .ok_or_else(|| invalid("view read byte budget exceeded"))?;
        Ok(bytes)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::bounded::{Step, WorkLimit};
    use super::super::test_support::wrapper_suffix;
    use super::*;

    #[cfg(feature = "agents")]
    #[test]
    fn view_channel_reads_are_bounded_isolated_and_leave_retention_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(dir.path(), 7, false).unwrap();
        let checkpoint = message(true, 1);
        let delta = message(false, 2);
        store.retain_checkpoint(&[9; 32], &checkpoint).unwrap();
        store.retain_delta(&[9; 32], 1, &delta).unwrap();
        let retained = store.load_retained().unwrap();
        let size = checkpoint.len() + delta.len();
        assert_eq!(store.load_view_channel(&[9; 32], size).unwrap(), retained);
        assert!(store.load_view_channel(&[9; 32], size - 1).is_err());
        assert!(store.load_view_channel(&[8; 32], 0).unwrap().is_empty());
        assert_eq!(
            store.load_retained().unwrap(),
            retained,
            "bounded view reads must not rewrite committed retained history"
        );
    }

    #[cfg(feature = "agents")]
    #[test]
    fn retained_view_refuses_a_failed_durability_barrier() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(dir.path(), 7, false).unwrap();
        store
            .retain_checkpoint(&[9; 32], &message(true, 1))
            .unwrap();
        let v2 = store.root.join(RETAINED_DIR).join("v2");
        let lineage = fs::read_dir(v2).unwrap().next().unwrap().unwrap().path();
        let selector = lineage.join("CURRENT");
        let fault = super::super::test_barrier::fail(&selector, "sync-file");
        assert!(store.load_view_channel(&[9; 32], 4096).is_err());
        drop(fault);
        assert_eq!(store.load_view_channel(&[9; 32], 4096).unwrap().len(), 1);
    }

    #[test]
    fn a_retained_build_moves_cheaply() {
        // The owner job of every edit moves its build into the blocking pool
        // and back on each quantum; inline selection state made that 54 KB.
        use std::mem::size_of;
        let size = size_of::<RetainBuild>();
        assert!(
            size <= 2048,
            "a RetainBuild is {size} bytes inline: store {}, sorter {}, phase {}, \
             publication {}, fast {}, continuity {}, stamp {}",
            size_of::<WorkspaceStore>(),
            size_of::<super::super::bounded::Sorter>(),
            size_of::<RetainPhase>(),
            size_of::<Option<super::super::shell_selection::Publication>>(),
            size_of::<Option<Box<dyn super::super::shell_selection::ShellTail>>>(),
            size_of::<Option<super::super::continuity::Receipt>>(),
            size_of::<Option<tail_index::Stamp>>(),
        );
    }

    #[test]
    fn only_a_build_that_made_its_scratch_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(dir.path(), 7, false).unwrap();
        let mut checkpoint = store
            .start_retain(&[9; 32], None, message(true, 1))
            .unwrap();
        assert!(checkpoint.holds_scratch());
        assert!(checkpoint.scratch().is_dir());
        while checkpoint.step(WorkLimit::default()).unwrap() == Step::More {}
        assert!(checkpoint.holds_scratch());
        let mut delta = store
            .start_retain(&[9; 32], Some(1), message(false, 2))
            .unwrap();
        while delta.step(WorkLimit::default()).unwrap() == Step::More {}
        assert!(!delta.holds_scratch());
        assert!(
            !delta.scratch().exists(),
            "a build that holds no scratch must have made none"
        );
    }

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

    /// A retained tail whose bytes are hostile to Automerge is decoded only in
    /// the isolate: one that crashes the decoder costs a worker, not the relay,
    /// and neither it nor a known change with a bad checksum after a valid
    /// prefix is ever covered by a later checkpoint.
    #[test]
    fn hostile_or_corrupt_tail_is_decoded_in_the_isolate_and_never_covered() {
        use automerge::transaction::Transactable;
        // Checksum-valid document whose op actor index is past its actor
        // table: stock Automerge panics on it (tilo-automerge-isolate's
        // decoder cases).
        let decoder_crash = hex::decode("856f4a833d51cb7a00f0010110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1011f0242f07c77fbf3d3aac425927bb392f9d579080452645fdb50bf7e4097564b07010203021303230240034302560210010402061108130d15092102230e3403420d560f5708800109810102830103940103a50107020002017e0a0302007e00017f000207000309010003070102090005050000017f00000302007f0202017c00017b0a7d016b016c017400090c007b0b7e78067b02017b047d0104020308017c0102040703017f0703017f057f1602007f0203167f0002167e18147668656c6c6f010205007f0104007e010002007e0d7f07010400037f0162000801").unwrap();
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "first", 1).unwrap();
        document.commit();
        let first = document.get_last_local_change().unwrap();
        let known = first.raw_bytes().to_vec();
        let mut bad_checksum = known.clone();
        bad_checksum[4] ^= 0x01;
        let mut corrupt = known;
        corrupt.extend_from_slice(&bad_checksum);
        document.put(automerge::ROOT, "next", 2).unwrap();
        let checkpoint = frame(true, document.save(), 3);
        for (what, body) in [("decoder crash", decoder_crash), ("bad checksum", corrupt)] {
            let tmp = tempfile::tempdir().unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            let tail = frame(false, body, 1);
            store.retain_delta(&[9; 32], 1, &tail).unwrap();
            store.retain_checkpoint(&[9; 32], &checkpoint).unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            let replay = store.load_retained().unwrap();
            assert!(replay.contains(&tail), "{what}: tail was collected");
            assert!(replay.contains(&checkpoint), "{what}: checkpoint missing");
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
    fn a_standalone_checkpoint_replaces_its_authors_earlier_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let channel = collab_types::ChannelId::new();
        let device = collab_types::Ed25519PublicKey::from_bytes([3; 32]);
        let ack = |through: u8, timestamp: u64| {
            let message = neighbour_wire::NeighbourMsg::MlsGovernanceAck {
                room: "room".into(),
                ack: Box::new(collab_types::MlsGovernanceAckV1::unsigned(
                    channel,
                    device,
                    [through; 32],
                )),
                delivery: 0,
            };
            let (route, _) = neighbour_wire::retention(&message, false).unwrap();
            let bytes = crate::transport::encode(&Envelope::Delta {
                workspace_id: 7,
                author_device_public_key: [1; 32],
                authored_at_ns: timestamp,
                delta: CoeditPayload::Checkpoint {
                    channel: route,
                    body: postcard::to_stdvec(&message).unwrap(),
                }
                .encode()
                .unwrap(),
                signature: [0; 64],
            })
            .unwrap();
            (route, bytes)
        };
        let (route, first) = ack(1, 1);
        let (_, second) = ack(2, 2);
        let (_, third) = ack(3, 3);
        for checkpoint in [&first, &second, &third] {
            let before = crate::store::thread_fsyncs();
            store.retain_checkpoint(&route, checkpoint).unwrap();
            if checkpoint == &third {
                // Once the store's own directories have settled: the staged
                // generation's directory, its checkpoint and that
                // directory, CURRENT and the lineage, and the lineage once
                // the replaced generation is gone; Apple repeats the
                // directory barrier of each `durable_write`. Two of the
                // lineage's barriers are `sync_dir_once`: the one after the
                // staged generation's directory is made, and the one after
                // the replaced generation is gone. On a filesystem whose
                // change times are coarser than this test, the lineage holds
                // as many entries at each of them as it did at the same point
                // of the previous checkpoint, so its stamp can match the one
                // synced then and the barrier is skipped. Either skip is
                // safe: CURRENT's `durable_write` syncs the lineage before
                // the new generation is published, and collection is redone
                // after a crash. Each may or may not be counted.
                let barriers = if cfg!(target_vendor = "apple") { 8 } else { 6 };
                let issued = crate::store::thread_fsyncs() - before;
                assert!(
                    (barriers - 2..=barriers).contains(&issued),
                    "{issued} barriers, expected {barriers} or up to two fewer"
                );
            }
        }
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        assert_eq!(store.load_retained().unwrap(), vec![third]);
    }

    /// A standalone checkpoint of `author` (its acknowledgement of
    /// `channel`'s governance), with its retention route.
    fn standalone_checkpoint(
        channel: collab_types::ChannelId,
        author: u8,
        through: u8,
    ) -> ([u8; 32], Vec<u8>) {
        let message = neighbour_wire::NeighbourMsg::MlsGovernanceAck {
            room: "room".into(),
            ack: Box::new(collab_types::MlsGovernanceAckV1::unsigned(
                channel,
                collab_types::Ed25519PublicKey::from_bytes([author; 32]),
                [through; 32],
            )),
            delivery: 0,
        };
        let (route, _) = neighbour_wire::retention(&message, false).unwrap();
        let bytes = crate::transport::encode(&Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: [author; 32],
            authored_at_ns: u64::from(through),
            delta: CoeditPayload::Checkpoint {
                channel: route,
                body: postcard::to_stdvec(&message).unwrap(),
            }
            .encode()
            .unwrap(),
            signature: [0; 64],
        })
        .unwrap();
        (route, bytes)
    }

    fn drive(build: &mut RetainBuild) {
        while matches!(build.step(WorkLimit::default()).unwrap(), Step::More) {}
    }

    /// A newcomer's first checkpoint is staged before its owner turn; the
    /// turn only selects the stage, with one barrier where retaining it
    /// takes three rounds, and the checkpoint is retained like any other.
    #[test]
    fn a_staged_first_checkpoint_costs_its_turn_one_barrier() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        // Another member's record: the workspace has retained before.
        let channel = collab_types::ChannelId::new();
        let (earlier_route, earlier) = standalone_checkpoint(channel, 2, 1);
        store.retain_checkpoint(&earlier_route, &earlier).unwrap();
        let (route, first) = standalone_checkpoint(channel, 1, 1);
        let fresh = store
            .stage_fresh_checkpoint(&route, &first)
            .unwrap()
            .expect("a lineage without a generation is staged");
        let before = crate::store::thread_fsyncs();
        let Ok(mut build) = store
            .start_fresh_retain(&route, fresh, first.clone())
            .unwrap()
        else {
            panic!("an intact stage of a fresh lineage is committed");
        };
        drive(&mut build);
        // CURRENT renamed into place, then the lineage synced; nothing to
        // collect, since the stage was the lineage's only generation.
        assert_eq!(crate::store::thread_fsyncs() - before, 1);
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let mut retained = store.load_retained().unwrap();
        retained.sort();
        let mut expected = vec![earlier, first];
        expected.sort();
        assert_eq!(retained, expected);
    }

    /// A lineage committed between its stage and its turn is not replaced
    /// by the stage: the checkpoint is retained as usual, against what the
    /// lineage holds, and the stage is discarded.
    #[test]
    fn a_stage_its_lineage_overtook_is_retained_as_usual() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let channel = collab_types::ChannelId::new();
        let (route, first) = standalone_checkpoint(channel, 1, 1);
        let (_, second) = standalone_checkpoint(channel, 1, 2);
        store.retain_checkpoint(&route, &first).unwrap();
        // Nothing to stage once the lineage has a generation.
        assert!(
            store
                .stage_fresh_checkpoint(&route, &second)
                .unwrap()
                .is_none()
        );
        let lineage = store.root().join(RETAINED_DIR).join("v2").join(format!(
            "{}-{}",
            hex::encode([1; 32]),
            hex::encode(route)
        ));
        fs::remove_file(lineage.join("CURRENT")).unwrap();
        let fresh = store
            .stage_fresh_checkpoint(&route, &second)
            .unwrap()
            .expect("a lineage without a generation is staged");
        let (_, staged) = fresh.discard_paths();
        let active = commit_generation(&lineage, &[], Some(first.as_slice())).unwrap();
        let Err((_, second)) = store.start_fresh_retain(&route, fresh, second).unwrap() else {
            panic!("a stage under a CURRENT is not committed");
        };
        assert_eq!(current(&lineage).unwrap(), Some(active));
        let mut build = store.start_retain(&route, None, second.clone()).unwrap();
        drive(&mut build);
        if let Some(mut discard) = CheckpointDiscard::new(&lineage, staged.clone()).unwrap() {
            while discard.step(WorkLimit::default()).unwrap() == Step::More {}
        }
        assert!(!staged.exists());
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        assert_eq!(store.load_retained().unwrap(), vec![second]);
    }

    /// A stage no owner turn took (its job failed first) leaves nothing
    /// behind: its lineage may never commit, so nothing would collect it.
    #[test]
    fn a_stage_no_turn_took_is_discarded_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let channel = collab_types::ChannelId::new();
        let (earlier_route, earlier) = standalone_checkpoint(channel, 2, 1);
        store.retain_checkpoint(&earlier_route, &earlier).unwrap();
        let (route, first) = standalone_checkpoint(channel, 1, 1);
        assert!(!store.lineage_has_generation(&[1; 32], &route));
        let fresh = store
            .stage_fresh_checkpoint(&route, &first)
            .unwrap()
            .expect("a lineage without a generation is staged");
        let (lineage, staged) = fresh.discard_paths();
        fresh.discard();
        let left: Vec<_> = fs::read_dir(&lineage)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(left.is_empty(), "the discarded stage left {left:?}");
        assert!(!staged.exists());
        assert!(!store.lineage_has_generation(&[1; 32], &route));
        assert_eq!(store.load_retained().unwrap(), vec![earlier]);
    }

    /// A payload adapter that recognises nothing.
    struct Unclassified;

    impl crate::payloads::ShellPayloads for Unclassified {
        fn is_document(&self, _: &[u8]) -> bool {
            false
        }
        fn open_document(
            &self,
            _: &[u8],
        ) -> Option<(crate::payloads::Route, Box<collab_types::SignedOp>)> {
            None
        }
        fn document_body(
            &self,
            _: &[u8; 32],
            _: &crate::payloads::Route,
            _: collab_types::SignedOp,
        ) -> Result<Vec<u8>, String> {
            Err("unclassified".into())
        }
        fn host_identity(
            &self,
            _: &tilo_user_identity::DeviceCert,
        ) -> Result<([u8; 32], Vec<u8>), String> {
            Err("unclassified".into())
        }
        fn checkpoint(&self, _: &[u8; 32], _: &[u8]) -> Option<crate::payloads::Checkpoint> {
            None
        }
        fn audience(&self, _: &[u8]) -> Option<std::collections::BTreeSet<[u8; 32]>> {
            None
        }
        fn decoded_overhead(&self) -> usize {
            0
        }
    }

    /// Checkpoint replacement asks the adapter the store was opened with,
    /// not a process-wide one: through an adapter that classifies nothing,
    /// the governance acknowledgements the desktop adapter lets replace one
    /// another are all kept.
    #[test]
    fn checkpoint_replacement_asks_the_stores_own_adapter() {
        let tmp = tempfile::tempdir().unwrap();
        let channel = collab_types::ChannelId::new();
        let device = collab_types::Ed25519PublicKey::from_bytes([3; 32]);
        let ack = |through: u8| {
            let message = neighbour_wire::NeighbourMsg::MlsGovernanceAck {
                room: "room".into(),
                ack: Box::new(collab_types::MlsGovernanceAckV1::unsigned(
                    channel,
                    device,
                    [through; 32],
                )),
                delivery: 0,
            };
            let (route, _) = neighbour_wire::retention(&message, false).unwrap();
            let bytes = crate::transport::encode(&Envelope::Delta {
                workspace_id: 7,
                author_device_public_key: [1; 32],
                authored_at_ns: u64::from(through),
                delta: CoeditPayload::Checkpoint {
                    channel: route,
                    body: postcard::to_stdvec(&message).unwrap(),
                }
                .encode()
                .unwrap(),
                signature: [0; 64],
            })
            .unwrap();
            (route, bytes)
        };
        let (route, first) = ack(1);
        let (_, second) = ack(2);
        let unclassified = WorkspaceStore::open_with_payloads(
            tmp.path(),
            7,
            false,
            std::sync::Arc::new(Unclassified),
        )
        .unwrap();
        unclassified.retain_checkpoint(&route, &first).unwrap();
        unclassified.retain_checkpoint(&route, &second).unwrap();
        assert_eq!(
            unclassified.load_retained().unwrap(),
            vec![first, second],
            "nothing it cannot classify is replaced"
        );

        let (_, third) = ack(3);
        let desktop = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        desktop.retain_checkpoint(&route, &third).unwrap();
        assert_eq!(
            desktop.load_retained().unwrap(),
            vec![third],
            "the desktop adapter lets the newest acknowledgement replace the rest"
        );
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
        // failure can target only CURRENT publication, before its rename,
        // and leaves the CURRENT the stage prepared beside it.
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
        let fault = crate::store::test_barrier::fail(&lineage, "current-publish");
        assert!(build.step(WorkLimit::default()).is_err());
        drop(fault);
        drop(build);
        drop(store);
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        assert_eq!(
            store.load_retained().unwrap(),
            vec![old, tail, uncovered.clone()]
        );
        assert_eq!(current(&lineage).unwrap().unwrap(), active);
        let selections = || {
            fs::read_dir(&lineage)
                .unwrap()
                .filter(|entry| {
                    tilo_relay_store::durable::selection_generation(
                        &entry.as_ref().unwrap().file_name().to_string_lossy(),
                    )
                    .is_some()
                })
                .count()
        };
        assert_eq!(selections(), 1);
        store.retain_checkpoint(&[9; 32], &new).unwrap();
        assert_eq!(
            store.load_retained().unwrap(),
            vec![new.clone(), uncovered.clone()]
        );
        // The next commit's collection reclaims what the failed one left.
        assert_eq!(selections(), 0);
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
