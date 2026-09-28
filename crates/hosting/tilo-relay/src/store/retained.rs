//! Shared retained coverage. Callers serialize writes with the workspace lock.
//!
//! A lineage is an authenticated envelope author plus channel, never a timestamp.
//! CURRENT selects durable shared replay coverage. Opaque records append in
//! receipt order; only proven raw Automerge ancestry, or a checkpoint's own
//! declaration of what it holds ([`Coverage`]), permits collection. A
//! declaration reaches only its publisher's lineage, so one member can never
//! retire another's history. A replacement generation is collected only after
//! its CURRENT selector is synced. An opaque checkpoint cannot prove that a
//! queue entry is covered merely because its filename or timestamp is older.

use super::{RETAINED_DIR, StoreError, WorkspaceStore};
use crate::store::bounded::Invariant;
use crate::{
    coedit::{CoeditPayload, Coverage},
    transport::Envelope,
};
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
pub(super) fn payload(bytes: &[u8], workspace: u64) -> Option<([u8; 32], CoeditPayload)> {
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
    let (author, CoeditPayload::Checkpoint { channel, body, .. }) = payload(bytes, workspace)?
    else {
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
    let Some((author, CoeditPayload::Checkpoint { channel, body, .. })) =
        payload(checkpoint, workspace)
    else {
        return false;
    };
    let Some((old_author, old_payload)) = payload(prior, workspace) else {
        return false;
    };
    let (old_channel, old_body) = match old_payload {
        CoeditPayload::Checkpoint { channel, body, .. }
        | CoeditPayload::RetainedDelta { channel, body, .. } => (channel, body),
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

/// What a checkpoint's publisher states it holds of its own lineage. The host
/// never infers it; it only acts on what the checkpoint names.
pub(super) enum Declared {
    /// A whole-state baseline whose newest version wins: it holds every
    /// earlier record of the lineage.
    Supersedes,
    /// A merge of the document holding exactly the records with these entry
    /// ids, as the publisher read them from the bytes it saved.
    Entries(std::collections::HashSet<[u8; 32]>),
}

/// The coverage a strictly decoded checkpoint declares; `None` for anything
/// else, and for a checkpoint that names nothing.
pub(super) fn declared(checkpoint: &[u8], workspace: u64) -> Option<Declared> {
    let (_, CoeditPayload::Checkpoint { coverage, .. }) = payload(checkpoint, workspace)? else {
        return None;
    };
    match coverage {
        Coverage::Supersedes => Some(Declared::Supersedes),
        Coverage::Entries(entries) if entries.is_empty() => None,
        Coverage::Entries(entries) => Some(Declared::Entries(entries.into_iter().collect())),
    }
}

/// Whether `prior`, an earlier record of the same lineage, leaves replay when
/// `checkpoint` is committed: its causal proof covers it, or the checkpoint's
/// declaration does. Named entries retire only from the front of the lineage
/// (`front_open`), so no surviving record ever waits on a dependency that
/// only a retired one carried. Unknown or malformed records never retire.
fn retires(
    checkpoint: &[u8],
    declared: Option<&Declared>,
    front_open: bool,
    prior: &[u8],
    workspace: u64,
) -> bool {
    if covers(checkpoint, prior, workspace) {
        return true;
    }
    declared.is_some_and(|declared| {
        (front_open || matches!(declared, Declared::Supersedes))
            && declared_covers(declared, prior, workspace)
    })
}

/// A lineage now holding `entries` records of `bytes` in all: past its
/// [`super::RetentionBounds`] the publisher is asked for a checkpoint that
/// declares what it holds. Nothing is dropped or refused — a retained record
/// may carry a change a member still needs.
fn note_lineage_size(store: &WorkspaceStore, author: [u8; 32], entries: u64, bytes: u64) {
    let bounds = store.retention_bounds();
    if entries > bounds.soft_entries || bytes > bounds.soft_bytes {
        store.want_checkpoint(author);
    }
}

/// Whether `declared` names `prior`, a strictly decoded record of the same
/// lineage.
fn declared_covers(declared: &Declared, prior: &[u8], workspace: u64) -> bool {
    let Some((_, prior)) = payload(prior, workspace) else {
        return false;
    };
    match (declared, prior) {
        (Declared::Supersedes, _) => true,
        (
            Declared::Entries(entries),
            CoeditPayload::Checkpoint { entry, .. } | CoeditPayload::RetainedDelta { entry, .. },
        ) => entries.contains(&entry),
    }
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

pub(super) fn create_dir(path: &Path) -> Result<(), StoreError> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {}
        Err(error) => return Err(error.into()),
    }
    // Also sync existing directories: a previous attempt may have returned
    // after creation but before the parent directory reached stable storage.
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

pub(super) fn durable_write(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    atomic_file::write(path, bytes)?;
    // atomic_file's parent fsync is best-effort. Destructive retirement needs
    // an explicit, successful directory barrier before it can proceed.
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
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
        fs::File::open(self.lineage.join("CURRENT"))?.sync_all()?;
        sync_dir(&self.lineage)?;
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
                sync_dir(&self.lineage)?;
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
    checkpoint: bool,
    workspace: u64,
    sorter: super::bounded::Sorter,
    sorted: Option<PathBuf>,
    phase: RetainPhase,
    duplicate: bool,
    opaque: bool,
    /// What an incoming checkpoint declares it holds of this lineage.
    declared: Option<Declared>,
    /// No record has survived this replacement yet: named entries may still
    /// retire (see [`retires`]).
    front_open: bool,
    /// Whether the declaration names the lineage's front record, once seen.
    /// Only then does it replace anything; otherwise the checkpoint is kept
    /// after the records it does not hold, in receipt order.
    declared_front: Option<bool>,
    /// The publisher this lineage belongs to.
    author: [u8; 32],
    /// Records and bytes of the active generation, as inspected.
    lineage_entries: u64,
    lineage_bytes: u64,
    /// Bytes of the records a replacement keeps.
    survivor_bytes: u64,
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
                for _ in 0..limit.record_entries() {
                    self.sorter.stats.last_step_work =
                        self.sorter.stats.last_step_work.saturating_add(1);
                    let Some(entry) = entries.next() else {
                        self.phase = RetainPhase::Sort;
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
                        if self.duplicate && self.continuity.is_none() {
                            self.phase = if self.staged_only {
                                RetainPhase::Done
                            } else {
                                RetainPhase::Collect(Box::new(GenerationGc::new(
                                    self.lineage.clone(),
                                )?))
                            };
                        } else if self.continuity.is_none()
                            && self.checkpoint
                            && (self.declared_front == Some(true)
                                || (!self.opaque
                                    && covers(&self.bytes, &self.bytes, self.workspace)))
                        {
                            let staged = self.lineage.join(hex::encode(rand::random::<[u8; 16]>()));
                            create_dir(&staged)?;
                            durable_write(&staged.join("checkpoint.delta"), &self.bytes)?;
                            self.staged = Some(staged);
                            #[cfg(test)]
                            super::test_barrier::check(
                                &self.lineage,
                                "checkpoint-stage-after-allocation",
                            )?;
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
                            super::test_barrier::check(
                                &self.lineage,
                                "checkpoint-stage-after-allocation",
                            )?;
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
                                super::continuity::account_validation(
                                    &mut self.sorter.stats,
                                    self.bytes.len(),
                                );
                                super::continuity::write(&self.store, &path, &self.bytes, receipt)?;
                            }
                            durable_write(&path, &self.bytes)?;
                            self.written = self.bytes.len() as u64;
                            note_lineage_size(
                                &self.store,
                                self.author,
                                self.lineage_entries.saturating_add(1),
                                self.lineage_bytes.saturating_add(self.bytes.len() as u64),
                            );
                            self.phase = RetainPhase::Done;
                        }
                        return Ok(Step::More);
                    };
                    self.lineage_entries = self.lineage_entries.saturating_add(1);
                    self.lineage_bytes = self.lineage_bytes.saturating_add(meta.len);
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
                    if self.declared_front.is_none()
                        && let Some(declared) = &self.declared
                    {
                        self.declared_front =
                            Some(declared_covers(declared, &record.bytes, self.workspace));
                    }
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
                            note_lineage_size(
                                &self.store,
                                self.author,
                                self.lineage_entries.saturating_add(1),
                                self.lineage_bytes.saturating_add(self.bytes.len() as u64),
                            );
                        } else {
                            note_lineage_size(
                                &self.store,
                                self.author,
                                self.survivors.saturating_add(1),
                                self.survivor_bytes.saturating_add(self.bytes.len() as u64),
                            );
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
                    } else if !retires(
                        &self.bytes,
                        self.declared.as_ref(),
                        self.front_open,
                        &record.bytes,
                        self.workspace,
                    ) {
                        self.front_open = false;
                        let path = self
                            .staged
                            .as_ref()
                            .invariant("staged missing")?
                            .join(format!("{:016x}.delta", self.survivors));
                        increment(&mut self.survivors)?;
                        self.survivor_bytes = self
                            .survivor_bytes
                            .saturating_add(record.bytes.len() as u64);
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
            sync_dir(directory)?;
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
        let declared = if checkpoint {
            declared(&bytes, self.workspace_id)
        } else {
            None
        };
        Ok(RetainBuild {
            lineage,
            active,
            scratch,
            bytes,
            checkpoint,
            workspace: self.workspace_id,
            sorter,
            sorted: None,
            phase,
            duplicate: false,
            opaque: false,
            declared,
            front_open: true,
            declared_front: None,
            author,
            lineage_entries: 0,
            lineage_bytes: 0,
            survivor_bytes: 0,
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

    /// [`Self::load_shared`] for one channel: the committed shell selection's
    /// records on it, then every publisher lineage of it in name order, each
    /// checkpoint first. Read-only; grants no proof or collection authority.
    pub(super) fn load_shared_channel(
        &self,
        channel: &[u8; 32],
    ) -> Result<Vec<Vec<u8>>, StoreError> {
        let on_channel = |bytes: &[u8]| {
            identity(bytes, self.workspace_id).is_some_and(|(_, actual, _)| actual == *channel)
        };
        let mut out = Vec::new();
        if let Some(selected) = current(&self.root.join(super::shell_selection::DIRECTORY))? {
            let mut manifest = fs::File::open(selected.join("basis.manifest"))?;
            while let Some(meta) = super::bounded::read_meta(&mut manifest)? {
                let bytes = fs::read(self.root.join(meta.source))?;
                if on_channel(&bytes) {
                    out.push(bytes);
                }
            }
        }
        let v2 = self.root.join(RETAINED_DIR).join("v2");
        if !v2.exists() {
            return Ok(out);
        }
        let suffix = format!("-{}", hex::encode(channel));
        let mut lineages = fs::read_dir(v2)?.collect::<Result<Vec<_>, _>>()?;
        lineages.retain(|lineage| lineage.file_name().to_string_lossy().ends_with(&suffix));
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
        frame(checkpoint, vec![value], 1)
    }

    /// A retained frame declaring no coverage: only the causal proof may
    /// retire what it holds.
    fn frame(checkpoint: bool, body: Vec<u8>, timestamp: u64) -> Vec<u8> {
        covering_frame(checkpoint, body, timestamp, Coverage::Entries(Vec::new()))
    }

    fn entry_of(body: &[u8]) -> [u8; 32] {
        *blake3::hash(body).as_bytes()
    }

    fn covering_frame(
        checkpoint: bool,
        body: Vec<u8>,
        timestamp: u64,
        coverage: Coverage,
    ) -> Vec<u8> {
        let entry = entry_of(&body);
        let payload = if checkpoint {
            CoeditPayload::Checkpoint {
                channel: [9; 32],
                entry,
                coverage,
                body,
            }
        } else {
            CoeditPayload::RetainedDelta {
                channel: [9; 32],
                entry,
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
        let failed = atomic_file::test_support::with_failure(
            atomic_file::test_support::Checkpoint::BeforePublish,
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

    fn authored(author: u8, bytes: &[u8]) -> Vec<u8> {
        let mut envelope = crate::transport::decode(&bytes[4..]).unwrap();
        let Envelope::Delta {
            author_device_public_key,
            ..
        } = &mut envelope
        else {
            panic!("delta")
        };
        *author_device_public_key = [author; 32];
        crate::transport::encode(&envelope).unwrap()
    }

    /// A retained tail is trimmed only by what a checkpoint names, and only
    /// from its front: a checkpoint naming nothing discards nothing, and one
    /// whose names skip the front record retires none of them.
    #[test]
    fn declared_entries_retire_only_the_named_front_of_their_lineage() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let base = frame(true, b"base".to_vec(), 1);
        store.retain_checkpoint(&[9; 32], &base).unwrap();
        let tails: Vec<Vec<u8>> = (1..=5u64)
            .map(|n| frame(false, format!("tail-{n}").into_bytes(), n))
            .collect();
        for (n, tail) in (1..=5u64).zip(&tails) {
            store.retain_delta(&[9; 32], n, tail).unwrap();
        }
        let silent = frame(true, b"names nothing".to_vec(), 6);
        store.retain_checkpoint(&[9; 32], &silent).unwrap();
        assert_eq!(
            store.load_retained().unwrap().len(),
            7,
            "a checkpoint naming nothing discards nothing"
        );

        let named: Vec<[u8; 32]> = [b"base".to_vec()]
            .into_iter()
            .chain((1..=3).map(|n| format!("tail-{n}").into_bytes()))
            .map(|body| entry_of(&body))
            .collect();
        let merged = covering_frame(true, b"merged".to_vec(), 7, Coverage::Entries(named));
        store.retain_checkpoint(&[9; 32], &merged).unwrap();
        assert_eq!(
            store.load_retained().unwrap(),
            vec![
                merged.clone(),
                tails[3].clone(),
                tails[4].clone(),
                silent.clone()
            ],
            "the named front is retired; what it does not name stays in order"
        );

        let holed = covering_frame(
            true,
            b"holed".to_vec(),
            8,
            Coverage::Entries(vec![entry_of(b"tail-5")]),
        );
        store.retain_checkpoint(&[9; 32], &holed).unwrap();
        let replay = WorkspaceStore::open(tmp.path(), 7, false)
            .unwrap()
            .load_retained()
            .unwrap();
        assert_eq!(
            replay.len(),
            5,
            "names behind a surviving record retire nothing"
        );
        for kept in [&merged, &tails[3], &tails[4], &silent, &holed] {
            assert!(replay.contains(kept));
        }
    }

    /// A publisher's history for a channel past its bound asks that publisher
    /// (and nobody else) for a checkpoint; nothing is dropped. The covering
    /// checkpoint it answers with brings the history back under the bound,
    /// after which it is not asked again.
    #[test]
    fn a_lineage_past_its_bound_asks_its_publisher_for_a_checkpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        store.set_retention_bounds(super::super::RetentionBounds {
            soft_entries: 3,
            soft_bytes: u64::MAX,
        });
        store
            .retain_checkpoint(&[9; 32], &frame(true, b"base".to_vec(), 1))
            .unwrap();
        let mut names = vec![entry_of(b"base")];
        for n in 1..=2u64 {
            let body = format!("tail-{n}").into_bytes();
            names.push(entry_of(&body));
            store
                .retain_delta(&[9; 32], n, &frame(false, body, n))
                .unwrap();
        }
        assert!(
            store.take_checkpoint_wants().is_empty(),
            "at the bound nobody is asked"
        );
        let body = b"tail-3".to_vec();
        names.push(entry_of(&body));
        store
            .retain_delta(&[9; 32], 3, &frame(false, body, 3))
            .unwrap();
        // Another publisher's own history stays under its bound.
        store
            .retain_delta(
                &[9; 32],
                4,
                &authored(2, &frame(false, b"other".to_vec(), 4)),
            )
            .unwrap();
        assert_eq!(
            store.take_checkpoint_wants(),
            vec![[1; 32]],
            "past the bound only its publisher is asked"
        );
        assert_eq!(
            store.load_retained().unwrap().len(),
            5,
            "nothing is dropped"
        );

        let merged = covering_frame(true, b"merged".to_vec(), 5, Coverage::Entries(names));
        store.retain_checkpoint(&[9; 32], &merged).unwrap();
        assert_eq!(
            store.load_retained_channel(&[9; 32]).unwrap().len(),
            2,
            "the covering checkpoint retires what it holds"
        );
        assert!(
            store.take_checkpoint_wants().is_empty(),
            "back under the bound, nobody is asked"
        );
    }

    /// A whole-state baseline retires its publisher's earlier records, and
    /// no declaration reaches another publisher's lineage.
    #[test]
    fn supersedes_retires_only_its_own_lineage() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let other_base = authored(2, &frame(true, b"other base".to_vec(), 1));
        let other_tail = authored(2, &frame(false, b"other tail".to_vec(), 2));
        store.retain_checkpoint(&[9; 32], &other_base).unwrap();
        store.retain_delta(&[9; 32], 2, &other_tail).unwrap();
        store
            .retain_checkpoint(&[9; 32], &frame(true, b"own base".to_vec(), 3))
            .unwrap();
        store
            .retain_delta(&[9; 32], 4, &frame(false, b"own tail".to_vec(), 4))
            .unwrap();
        let everything = [b"other base".to_vec(), b"other tail".to_vec()]
            .iter()
            .map(|body| entry_of(body))
            .collect();
        let baseline = covering_frame(true, b"baseline".to_vec(), 5, Coverage::Supersedes);
        store.retain_checkpoint(&[9; 32], &baseline).unwrap();
        let naming_others = covering_frame(
            true,
            b"names others".to_vec(),
            6,
            Coverage::Entries(everything),
        );
        store.retain_checkpoint(&[9; 32], &naming_others).unwrap();
        let replay = store.load_retained().unwrap();
        assert_eq!(replay.len(), 4);
        for kept in [&other_base, &other_tail, &baseline, &naming_others] {
            assert!(replay.contains(kept));
        }
    }
}
