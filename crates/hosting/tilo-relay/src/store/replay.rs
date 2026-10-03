//! Bounded storage cursors. These confer no transport/session cleanup authority:
//! the ordered owner in persistence must supply that authority in Task 2c2.
//! No Drop implementation recursively deletes files or touches pending sources.
use super::bounded::{
    self, DirectoryCursor, Meta, Sorter, WorkStats, increment, invalid, read_meta, write_meta,
};
pub(crate) use super::bounded::{Step, WorkLimit};
use super::{StoreError, WorkspaceStore, retained};
use crate::store::bounded::Invariant;
use crate::transport::{self, Envelope};
use std::{
    collections::VecDeque,
    ffi::OsString,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

pub(crate) const DEFAULT_BATCH_FRAMES: usize = 8;
pub(crate) const DEFAULT_BATCH_BYTES: usize = 1024 * 1024;
pub(crate) const DEFAULT_OUTPUT_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const DEFAULT_REPLAY_TICKETS: usize = 32;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReplayId(pub [u8; 16]);
#[derive(Clone, Copy)]
pub(crate) struct ReplayLimits {
    pub max_frames: usize,
    pub max_bytes: usize,
}
impl Default for ReplayLimits {
    fn default() -> Self {
        Self {
            max_frames: DEFAULT_BATCH_FRAMES,
            max_bytes: DEFAULT_BATCH_BYTES,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReplaySummary {
    pub total_frames: u64,
}
/// A caller shares this budget across all owners; the batch keeps its permit
/// through writes, including `transport::encode`'s payload + output overlap.
#[derive(Clone)]
pub(crate) struct ReplayBudget {
    inner: Arc<Budget>,
}
struct Budget {
    capacity: usize,
    used: AtomicUsize,
}
impl ReplayBudget {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Budget {
                capacity,
                used: AtomicUsize::new(0),
            }),
        }
    }
    pub(crate) fn used(&self) -> usize {
        self.inner.used.load(Ordering::Relaxed)
    }
    fn acquire(&self, bytes: usize) -> Result<ReplayAllocation, StoreError> {
        self.inner
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= self.inner.capacity)
            })
            .map_err(|_| invalid("replay output budget exhausted"))?;
        Ok(ReplayAllocation {
            inner: self.inner.clone(),
            bytes,
        })
    }
}
pub(crate) struct ReplayAllocation {
    inner: Arc<Budget>,
    bytes: usize,
}
impl Drop for ReplayAllocation {
    fn drop(&mut self) {
        self.inner.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
pub(crate) struct ReplayBatch {
    pub number: u64,
    frames: VecDeque<Envelope>,
    pub eof: bool,
    pub encoded_bytes: usize,
    pub charged_bytes: usize,
    _allocation: ReplayAllocation,
}

impl ReplayBatch {
    pub(crate) fn front(&self) -> Option<&Envelope> {
        self.frames.front()
    }
    pub(crate) fn pop_written(&mut self) {
        self.frames.pop_front();
    }
    pub(crate) fn len(&self) -> usize {
        self.frames.len()
    }
}

/// The transport payload after a frame's four-byte length prefix. A frame
/// from `frame_bytes` always has one; anything shorter decodes as empty.
fn frame_payload(bytes: &[u8]) -> &[u8] {
    bytes.get(4..).unwrap_or_default()
}

/// Only reads complete legal transport frames. Invalid data remains untouched.
/// Prefix and file length are checked before any payload-sized allocation.
pub(super) fn frame_bytes(path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if !(4..=(transport::MAX_FRAME_BYTES as u64).saturating_add(4)).contains(&len) {
        return Ok(None);
    }
    let mut prefix = [0; 4];
    file.read_exact(&mut prefix)?;
    if u64::from(u32::from_le_bytes(prefix)) != len.saturating_sub(4) {
        return Ok(None);
    }
    let mut bytes = vec![0; usize::try_from(len).map_err(|_| invalid("oversized frame"))?];
    // The range check above guarantees at least the four prefix bytes.
    let (head, body) = bytes.split_at_mut(4);
    head.copy_from_slice(&prefix);
    file.read_exact(body)?;
    let mut extra = [0];
    if file.read(&mut extra)? != 0 {
        return Err(invalid("frame changed during read"));
    }
    Ok(Some(bytes))
}
fn heap_bytes(envelope: &Envelope) -> usize {
    match envelope {
        Envelope::Delta { delta, .. } => delta.capacity(),
        Envelope::Presence { payload, .. } => payload.capacity(),
        Envelope::NotAuthorised { reason, .. } => reason.capacity(),
        Envelope::PeerRelayHello { hosted_workspaces } => {
            hosted_workspaces.capacity().saturating_mul(8)
        }
        Envelope::SyncRequest { have_heads, .. } => have_heads.capacity().saturating_mul(32),
        Envelope::SyncResponse { delta_since, .. } => delta_since.capacity(),
        _ => 0,
    }
}
fn frame_charge(
    envelope: &Envelope,
    raw_len: usize,
    encoded: &[u8],
    encoder_capacity: usize,
) -> Result<u64, StoreError> {
    let output_len = encoded
        .len()
        .checked_add(4)
        .ok_or_else(|| invalid("frame allocation overflow"))?;
    let encode_overlap = encoder_capacity
        .checked_add(output_len)
        .ok_or_else(|| invalid("frame allocation overflow"))?;
    heap_bytes(envelope)
        .checked_add(std::mem::size_of::<Envelope>())
        .and_then(|n| n.checked_add(raw_len.max(encode_overlap)))
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(|| invalid("frame allocation charge overflow"))
}
/// Charge for re-encoding `envelope` with `transport::encode`, measured on
/// that encoder's own output `framed` of allocator capacity `capacity`. The
/// encoder serializes into one growable Vec (length prefix included), so its
/// peak is that final buffer plus the one it outgrew, which held fewer than
/// `framed.len()` bytes.
fn v1_frame_charge(
    envelope: &Envelope,
    raw_len: usize,
    framed: &[u8],
    capacity: usize,
) -> Result<u64, StoreError> {
    frame_charge(envelope, raw_len, frame_payload(framed), capacity)
}
/// Sort rank of a retained lineage (`<author>-<channel>`): a device's
/// certificate channel replays first, because a shell drops what a device
/// published until it holds that device's certificate, and channels are
/// otherwise ordered by hash.
pub(crate) fn lineage_rank(lineage: &str) -> &'static str {
    let identity = lineage.split_once('-').is_some_and(|(author, channel)| {
        let Ok(author) = hex::decode(author) else {
            return false;
        };
        let mut route = b"identity-hello\0".to_vec();
        route.extend_from_slice(&author);
        blake3::hash(&route).to_hex().as_str() == channel
    });
    if identity { "0" } else { "1" }
}

/// [`lineage_rank`] of the retained lineage a store-relative source path
/// lies under; `"1"` for anything else.
fn source_rank(source: &Path) -> &'static str {
    let mut parts = source
        .components()
        .map(|part| part.as_os_str().to_string_lossy());
    match (parts.next(), parts.next(), parts.next()) {
        (Some(root), Some(version), Some(lineage)) if root == "retained" && version == "v2" => {
            lineage_rank(&lineage)
        }
        _ => "1",
    }
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}
fn open_dir(path: &Path) -> Result<Option<fs::ReadDir>, StoreError> {
    match fs::read_dir(path) {
        Ok(dir) => Ok(Some(dir)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn capture_continuity(
    store: &WorkspaceStore,
    source: &Path,
    target: &Path,
    pin: u64,
    manifest: &mut File,
    stats: &mut WorkStats,
) -> Result<(), StoreError> {
    if let Some(receipt) = super::continuity::copy_measured(store, source, target, stats, 0)? {
        stats.peak_open_files = stats.peak_open_files.max(8);
        write_meta(
            manifest,
            &Meta {
                key: vec![
                    receipt.group().into(),
                    format!("{:016x}", u64::MAX.saturating_sub(receipt.sequence)).into(),
                    hex::encode(receipt.id).into(),
                    format!("{pin:016x}").into(),
                ],
                source: target
                    .strip_prefix(store.root())
                    .map_err(|_| invalid("continuity pin outside workspace"))?
                    .into(),
                pin,
                len: 0,
                pending: false,
            },
        )?;
    }
    Ok(())
}

pub(crate) struct ReplayBuild {
    root: PathBuf,
    workspace: PathBuf,
    device: [u8; 32],
    phase: u8,
    outer: Option<fs::ReadDir>,
    child: Option<(OsString, PathBuf, fs::ReadDir)>,
    sorter: Option<Sorter>,
    next_pin: u64,
    continuity: Option<File>,
    selection: Option<super::shell_selection::Selection>,
    selected: Option<File>,
    shell_selected: bool,
    skip: Option<ResumeSkip>,
}
/// Retained frames a resuming device already holds, by
/// [`super::resume::frame_digest`]; a replay for it leaves them out.
pub(crate) type ResumeSkip = std::sync::Arc<std::collections::HashSet<[u8; 32]>>;
impl ReplayBuild {
    /// Leave out the retained frames `skip` names (a resuming device holds
    /// them). Its queue is replayed whole.
    pub(crate) fn skipping(mut self, skip: Option<ResumeSkip>) -> Self {
        self.skip = skip;
        self
    }
    #[cfg(test)]
    pub(crate) fn stats(&self) -> Option<&WorkStats> {
        self.sorter.as_ref().map(|s| &s.stats)
    }
    pub(crate) fn into_discard(self) -> Result<WorkDiscard, StoreError> {
        WorkDiscard::new(self.root)
    }
}
struct Candidate {
    continuity: Option<super::continuity::Receipt>,
    meta: Meta,
    coverage: bool,
    charge: u64,
    index: File,
    /// Held by the resuming device: indexed for coverage, not replayed.
    held: bool,
}
pub(crate) struct ReplaySnapshot {
    root: PathBuf,
    workspace: PathBuf,
    workspace_id: u64,
    pending_root: PathBuf,
    sorter: Option<Sorter>,
    ordered: Option<File>,
    continuity_sorter: Option<Sorter>,
    continuity_input: Option<File>,
    continuity_ordered: Option<File>,
    continuity_previous: Option<super::continuity::Receipt>,
    candidate: Option<Candidate>,
    output: File,
    cleanup: File,
    ready: bool,
    count: u64,
    read_offset: u64,
    cleanup_offset: u64,
    batch_number: u64,
    stats: WorkStats,
    digest: fn(&[u8]) -> [u8; 32],
    skip: Option<ResumeSkip>,
}
impl ReplaySnapshot {
    pub(crate) fn stats(&self) -> &WorkStats {
        &self.stats
    }
    pub(crate) fn into_discard(self) -> Result<WorkDiscard, StoreError> {
        WorkDiscard::new(self.root)
    }
    fn pin(&self, pin: u64) -> PathBuf {
        self.root.join("pins").join(format!("{pin:016x}.frame"))
    }
    #[expect(
        clippy::too_many_lines,
        reason = "one frame's measured decode, coverage and resume classification; splitting it would scatter its accounting"
    )]
    fn classify(&mut self, store: &WorkspaceStore, mut meta: Meta) -> Result<(), StoreError> {
        let continuity =
            super::continuity::load_measured(store, &self.pin(meta.pin), &mut self.stats)?;
        if let Some(receipt) = &continuity {
            let winner = super::continuity::read_receipt(&self.winner_path(&receipt.group()))?
                .ok_or_else(|| invalid("invalid continuity winner"))?;
            if winner.group() != receipt.group() {
                return Err(invalid("continuity winner group mismatch"));
            }
            if &winner != receipt {
                if meta.pending {
                    write_meta(&mut self.cleanup, &meta)?;
                }
                return Ok(());
            }
        }
        let Some(bytes) = frame_bytes(&self.pin(meta.pin))? else {
            return Ok(());
        };
        increment(&mut self.stats.preparation_reads)?;
        self.stats.peak_frame_bytes = self.stats.peak_frame_bytes.max(bytes.capacity());
        let Ok(envelope) = transport::decode(frame_payload(&bytes)) else {
            return Ok(());
        };
        let decoded = heap_bytes(&envelope).saturating_add(std::mem::size_of::<Envelope>());
        self.stats.peak_frame_bytes = self
            .stats
            .peak_frame_bytes
            .max(bytes.capacity().saturating_add(decoded));

        // Canonical complete frames alone can prove retained duplicate coverage.
        let retained = if let Envelope::Delta {
            workspace_id,
            delta,
            ..
        } = &envelope
        {
            if *workspace_id == self.workspace_id {
                let payload = super::retained::complete_coedit_payload(delta);
                if let Some(payload) = &payload {
                    let body = match payload {
                        crate::coedit::CoeditPayload::Checkpoint { body, .. }
                        | crate::coedit::CoeditPayload::RetainedDelta { body, .. } => body,
                    };
                    self.stats.peak_frame_bytes = self.stats.peak_frame_bytes.max(
                        bytes
                            .capacity()
                            .saturating_add(decoded)
                            .saturating_add(body.capacity()),
                    );
                }
                payload.is_some()
            } else {
                false
            }
        } else {
            false
        };
        let encoded =
            transport::encode(&envelope).map_err(|_| invalid("replay re-encoding failed"))?;
        let v1_charge = v1_frame_charge(&envelope, bytes.len(), &encoded, encoded.capacity())?;
        self.stats.peak_frame_bytes = self.stats.peak_frame_bytes.max(
            bytes
                .capacity()
                .saturating_add(decoded)
                .saturating_add(encoded.capacity()),
        );
        // Exact outer re-encoding rejects every unconsumed Envelope suffix;
        // `retained` independently requires complete Coedit consumption.
        let coverage = !meta.pending && retained && encoded == bytes;
        let held = !meta.pending
            && self
                .skip
                .as_ref()
                .is_some_and(|skip| skip.contains(blake3::hash(&encoded).as_bytes()));
        drop(encoded);
        let v2 = tilo_delivery_protocol::transport_v2::state_payload(&envelope)
            .map_err(|_| invalid("v2 replay re-encoding failed"))?;
        let charge = v1_charge.max(frame_charge(&envelope, bytes.len(), &v2, v2.capacity())?);
        self.stats.peak_frame_bytes = self.stats.peak_frame_bytes.max(
            bytes
                .capacity()
                .saturating_add(decoded)
                .saturating_add(v2.capacity()),
        );
        drop(v2);
        let hash = (self.digest)(&bytes);
        // Pin length is immutable. Store measured output reservation in the
        // ordering key only in the final manifest (ordering is already done).
        meta.len = bytes.len() as u64;
        // Marked copies have their own receipt identity. An earlier opaque,
        // byte-identical record cannot substitute for the chosen marked copy.
        let index_name = continuity.as_ref().map_or_else(
            || format!("{}.idx", hex::encode(hash)),
            |receipt| format!("c{}.idx", hex::encode(receipt.id)),
        );
        let index_path = self.root.join("index").join(index_name);
        let index = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(index_path)?;
        self.candidate = Some(Candidate {
            continuity,
            meta,
            coverage,
            charge,
            index,
            held,
        });
        Ok(())
    }
    fn winner_path(&self, group: &str) -> PathBuf {
        self.root.join("index").join(format!(
            "{}.winner",
            blake3::hash(group.as_bytes()).to_hex()
        ))
    }
    fn elect_continuity(&mut self, store: &WorkspaceStore, meta: &Meta) -> Result<(), StoreError> {
        let receipt =
            super::continuity::load_measured(store, &self.pin(meta.pin), &mut self.stats)?
                .ok_or_else(|| invalid("captured continuity association disappeared"))?;
        let group = receipt.group();
        let same_group = self
            .continuity_previous
            .as_ref()
            .is_some_and(|previous| previous.group() == group);
        if let Some(previous) = &self.continuity_previous
            && same_group
            && previous.sequence == receipt.sequence
            && previous != &receipt
        {
            return Err(invalid("conflicting continuity receipt sequence"));
        }
        if !same_group {
            let encoded = postcard::to_stdvec(&receipt)
                .map_err(|_| invalid("continuity winner encoding failed"))?;
            let mut winner = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(self.winner_path(&group))?;
            winner.write_all(&encoded)?;
        }
        self.continuity_previous = Some(receipt);
        Ok(())
    }
    fn accept_candidate(&mut self, duplicate: bool) -> Result<(), StoreError> {
        let candidate = self.candidate.take().invariant("candidate missing")?;
        let Candidate {
            continuity,
            mut meta,
            coverage,
            charge,
            mut index,
            held,
        } = candidate;
        if meta.pending {
            write_meta(&mut self.cleanup, &meta)?;
        }
        if !duplicate {
            if !meta.pending || continuity.is_some() {
                index.write_all(&meta.pin.to_le_bytes())?;
                index.write_all(&meta.len.to_le_bytes())?;
                index.write_all(&[u8::from(coverage || continuity.is_some())])?;
            }
            if !held {
                meta.key = vec![charge.to_string().into()];
                write_meta(&mut self.output, &meta)?;
                increment(&mut self.count)?;
            }
        }
        Ok(())
    }
}
impl WorkspaceStore {
    pub(crate) fn replay_capture_has_basis(&self) -> Result<bool, StoreError> {
        Ok(self.shell_authority().is_none()
            || retained::current(&self.root.join(super::shell_selection::DIRECTORY))?.is_some())
    }

    pub(crate) fn start_replay_capture(
        &self,
        id: ReplayId,
        device: [u8; 32],
    ) -> Result<ReplayBuild, StoreError> {
        self.start_replay_capture_at(id, device, false)
    }
    pub(crate) fn start_pending_replay_capture(
        &self,
        id: ReplayId,
        device: [u8; 32],
    ) -> Result<ReplayBuild, StoreError> {
        self.start_replay_capture_at(id, device, true)
    }
    fn start_replay_capture_at(
        &self,
        id: ReplayId,
        device: [u8; 32],
        pending_only: bool,
    ) -> Result<ReplayBuild, StoreError> {
        let root = self.root.join("store-work-v1");
        fs::create_dir_all(&root)?;
        let root = root.join(format!("replay-{}", hex::encode(id.0)));
        fs::create_dir(&root)?;
        fs::create_dir(root.join("pins"))?;
        fs::create_dir(root.join("index"))?;
        let sorter = Sorter::new(root.join("sort"))?;
        let selection = if pending_only {
            None
        } else {
            self.shell_authority()
                .map(|_| super::shell_selection::Selection::new(self, root.clone(), None))
                .transpose()?
        };
        Ok(ReplayBuild {
            selection,
            selected: if !pending_only && self.shell_authority().is_none() {
                retained::current(&self.root.join(super::shell_selection::DIRECTORY))?
                    .map(|p| File::open(p.join("basis.manifest")))
                    .transpose()?
            } else {
                None
            },
            shell_selected: !pending_only && self.shell_authority().is_some(),
            root: root.clone(),
            workspace: self.root.clone(),
            device,
            phase: if pending_only { 3 } else { 2 },
            outer: None,
            child: None,
            sorter: Some(sorter),
            next_pin: 0,
            continuity: Some(File::create(root.join("continuity.manifest"))?),
            skip: None,
        })
    }
    /// Invoke under one owner operation until Done; later mutations must not
    /// interleave before every selected source has its immutable hardlink.
    #[expect(
        clippy::too_many_lines,
        reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
    )]
    pub(crate) fn capture_replay_step(
        &self,
        build: &mut ReplayBuild,
        limit: WorkLimit,
    ) -> Result<Step<ReplaySnapshot>, StoreError> {
        let limit = limit.checked()?;
        if build.workspace != self.root {
            return Err(invalid("replay store mismatch"));
        }
        if let Some(selection) = &mut build.selection {
            let unresolved = match selection.step(limit)? {
                Step::More => return Ok(Step::More),
                Step::Done(super::shell_selection::SelectionOutcome::Published(_)) => false,
                Step::Done(super::shell_selection::SelectionOutcome::BatchPublished { .. }) => {
                    return Err(invalid("replay capture returned batch selection"));
                }
                Step::Done(
                    super::shell_selection::SelectionOutcome::UnresolvedTailDependencies,
                ) => true,
            };
            let current = retained::current(&self.root.join(super::shell_selection::DIRECTORY))?;
            if unresolved && current.is_none() {
                return Err(invalid(
                    "unresolved shell tail floor or dependencies prevent coherent selection",
                ));
            }
            // Selection validated the immutable CURRENT generation before it
            // considered newer retained inputs. Those inputs remain untouched;
            // subscription replay can still use CURRENT plus pending frames.
            let path = current.ok_or_else(|| invalid("missing selected shell basis"))?;
            let selected = File::open(path.join("basis.manifest"))?;
            build.selection = None;
            build.selected = Some(selected);
            return Ok(Step::More);
        }
        if build.phase > 4 {
            return Err(invalid("capture already consumed"));
        }
        let sorter = build
            .sorter
            .as_mut()
            .ok_or_else(|| invalid("capture already consumed"))?;
        sorter.stats.last_step_work = 0;
        if sorter.spilling() {
            sorter.spill_step(limit)?;
            return Ok(Step::More);
        }
        for _ in 0..limit.record_entries() {
            sorter.stats.last_step_work = sorter.stats.last_step_work.saturating_add(1);
            if build.phase == 4 {
                build.phase = 5;
                drop(build.continuity.take());
                let output = File::create(build.root.join("replay.manifest"))?;
                let cleanup = File::create(build.root.join("cleanup.manifest"))?;
                return Ok(Step::Done(ReplaySnapshot {
                    root: build.root.clone(),
                    workspace: self.root.clone(),
                    workspace_id: self.workspace_id,
                    pending_root: PathBuf::from("pending").join(hex::encode(build.device)),
                    sorter: build.sorter.take(),
                    ordered: None,
                    continuity_sorter: None,
                    continuity_input: Some(File::open(build.root.join("continuity.manifest"))?),
                    continuity_ordered: None,
                    continuity_previous: None,
                    candidate: None,
                    output,
                    cleanup,
                    ready: false,
                    count: 0,
                    read_offset: 0,
                    cleanup_offset: 0,
                    batch_number: 0,
                    stats: WorkStats::default(),
                    digest,
                    skip: build.skip.take(),
                }));
            }
            if build.phase == 2 && build.selected.is_some() {
                if let Some(meta) = read_meta(
                    build
                        .selected
                        .as_mut()
                        .ok_or_else(|| invalid("missing selected manifest"))?,
                )? {
                    let pin = build.next_pin;
                    increment(&mut build.next_pin)?;
                    let source = self.root.join(&meta.source);
                    let target = build.root.join("pins").join(format!("{pin:016x}.frame"));
                    fs::hard_link(&source, &target)?;
                    capture_continuity(
                        self,
                        &source,
                        &target,
                        pin,
                        build.continuity.as_mut().invariant("continuity missing")?,
                        &mut sorter.stats,
                    )?;
                    sorter.push(Meta {
                        key: vec![
                            source_rank(Path::new(&meta.source)).into(),
                            "0".into(),
                            format!("{pin:016x}").into(),
                        ],
                        source: meta.source,
                        pin,
                        len: fs::metadata(target)?.len(),
                        pending: false,
                    })?;
                    if sorter.spilling() {
                        return Ok(Step::More);
                    }
                } else {
                    if build.shell_selected {
                        build.phase = 3;
                    }
                    build.outer = None;
                    build.selected = None;
                }
                continue;
            }
            if build.outer.is_none() {
                let dir = match build.phase {
                    2 => self.root.join("retained/v2"),
                    _ => self.pending_dir_for(&build.device),
                };
                build.outer = open_dir(&dir)?;
                if build.outer.is_none() {
                    build.phase = build.phase.saturating_add(1);
                    continue;
                }
            }
            let selected = if let Some((channel, _, child)) = &mut build.child {
                if let Some(entry) = child.next() {
                    let entry = entry?;
                    let name = entry.file_name();
                    let valid = name.to_string_lossy().ends_with(".delta")
                        && !name.to_string_lossy().starts_with('.');
                    if entry.file_type()?.is_file() && valid {
                        let order = if name == "checkpoint.delta" { "0" } else { "1" };
                        let rank = lineage_rank(&channel.to_string_lossy());
                        Some((
                            entry.path(),
                            vec![rank.into(), channel.clone(), order.into(), name],
                        ))
                    } else {
                        None
                    }
                } else {
                    build.child = None;
                    None
                }
            } else {
                if let Some(entry) = build.outer.as_mut().invariant("outer missing")?.next() {
                    let entry = entry?;
                    let name = entry.file_name();
                    let kind = entry.file_type()?;
                    match build.phase {
                        2 if kind.is_dir() => {
                            if let Some(active) = retained::current(&entry.path())? {
                                build.child = Some((name, active.clone(), fs::read_dir(active)?));
                            }
                            None
                        }
                        3 if kind.is_file() && name.to_string_lossy().ends_with(".delta") => {
                            Some((entry.path(), vec!["2".into(), name]))
                        }
                        _ => None,
                    }
                } else {
                    build.outer = None;
                    build.phase = build.phase.saturating_add(1);
                    None
                }
            };
            if let Some((source, key)) = selected {
                let pin = build.next_pin;
                increment(&mut build.next_pin)?;
                let target = build.root.join("pins").join(format!("{pin:016x}.frame"));
                fs::hard_link(&source, &target)?;
                capture_continuity(
                    self,
                    &source,
                    &target,
                    pin,
                    build.continuity.as_mut().invariant("continuity missing")?,
                    &mut sorter.stats,
                )?;
                let len = fs::metadata(&target)?.len();
                sorter.push(Meta {
                    key,
                    source: source
                        .strip_prefix(&self.root)
                        .map_err(|_| invalid("invalid capture path"))?
                        .into(),
                    pin,
                    len,
                    pending: build.phase == 3,
                })?;
                sorter.stats.peak_open_files = sorter.stats.peak_open_files.max(3);
                if sorter.spilling() {
                    return Ok(Step::More);
                }
            }
        }
        Ok(Step::More)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
    )]
    pub(crate) fn prepare_replay_step(
        &self,
        snapshot: &mut ReplaySnapshot,
        limit: WorkLimit,
    ) -> Result<Step<ReplaySummary>, StoreError> {
        #[cfg(test)]
        super::test_barrier::pause(&self.root, "replay-manifest-prepare");
        let limit = limit.checked()?;
        if snapshot.workspace != self.root {
            return Err(invalid("replay store mismatch"));
        }
        if snapshot.ready {
            return Ok(Step::Done(ReplaySummary {
                total_frames: snapshot.count,
            }));
        }
        snapshot.stats.last_step_work = 0;
        if let Some(sorter) = &mut snapshot.sorter {
            let result = sorter.step(limit)?;
            snapshot.stats = sorter.stats.clone();
            snapshot.stats.peak_open_files = snapshot.stats.peak_open_files.saturating_add(2); // output + cleanup manifests
            if let Step::Done(path) = result {
                snapshot.ordered = Some(File::open(path)?);
                snapshot.sorter = None;
                snapshot.continuity_sorter =
                    Some(Sorter::new(snapshot.root.join("continuity-sort"))?);
            }
            return Ok(Step::More);
        }
        if let Some(sorter) = &mut snapshot.continuity_sorter {
            if let Some(input) = &mut snapshot.continuity_input {
                if sorter.spilling() {
                    sorter.spill_step(limit)?;
                } else {
                    for _ in 0..limit.record_entries() {
                        snapshot.stats.last_step_work =
                            snapshot.stats.last_step_work.saturating_add(1);
                        let Some(meta) = read_meta(input)? else {
                            snapshot.continuity_input = None;
                            break;
                        };
                        sorter.push(meta)?;
                        if sorter.spilling() {
                            break;
                        }
                    }
                }
                snapshot.stats.peak_metadata_entries = snapshot
                    .stats
                    .peak_metadata_entries
                    .max(sorter.stats.peak_metadata_entries);
                snapshot.stats.peak_metadata_bytes = snapshot
                    .stats
                    .peak_metadata_bytes
                    .max(sorter.stats.peak_metadata_bytes);
                snapshot.stats.peak_open_files = snapshot
                    .stats
                    .peak_open_files
                    .max(sorter.stats.peak_open_files.saturating_add(4));
                snapshot.stats.last_step_work = snapshot
                    .stats
                    .last_step_work
                    .max(sorter.stats.last_step_work);
                return Ok(Step::More);
            }
            let result = sorter.step(limit)?;
            snapshot.stats.last_step_work = sorter.stats.last_step_work;
            snapshot.stats.peak_metadata_entries = snapshot
                .stats
                .peak_metadata_entries
                .max(sorter.stats.peak_metadata_entries);
            snapshot.stats.peak_metadata_bytes = snapshot
                .stats
                .peak_metadata_bytes
                .max(sorter.stats.peak_metadata_bytes);
            snapshot.stats.peak_open_files = snapshot
                .stats
                .peak_open_files
                .max(sorter.stats.peak_open_files.saturating_add(3));
            if let Step::Done(path) = result {
                snapshot.continuity_ordered = Some(File::open(path)?);
                snapshot.continuity_sorter = None;
            }
            return Ok(Step::More);
        }
        if snapshot.continuity_ordered.is_some() {
            for _ in 0..limit.record_entries() {
                snapshot.stats.last_step_work = snapshot.stats.last_step_work.saturating_add(1);
                let Some(meta) = read_meta(
                    snapshot
                        .continuity_ordered
                        .as_mut()
                        .invariant("continuity ordered missing")?,
                )?
                else {
                    snapshot.continuity_ordered = None;
                    snapshot.continuity_previous = None;
                    return Ok(Step::More);
                };
                snapshot.elect_continuity(self, &meta)?;
            }
            return Ok(Step::More);
        }
        snapshot.stats.peak_open_files = snapshot.stats.peak_open_files.max(6);
        for _ in 0..limit.record_entries() {
            snapshot.stats.last_step_work = snapshot.stats.last_step_work.saturating_add(1);
            if let Some(candidate) = &mut snapshot.candidate {
                let mut entry = [0; 17];
                if candidate.index.read(&mut entry[..1])? == 0 {
                    snapshot.accept_candidate(false)?;
                    continue;
                }
                candidate.index.read_exact(&mut entry[1..])?;
                let pin = u64::from_le_bytes(entry[..8].try_into().invariant("entry missing")?);
                let len = u64::from_le_bytes(entry[8..16].try_into().invariant("entry missing")?);
                let compare =
                    len == candidate.meta.len && (!candidate.meta.pending || entry[16] == 1);
                let candidate_pin = candidate.meta.pin;
                let receipt = candidate.continuity.clone();
                let same_receipt = if let Some(receipt) = &receipt {
                    super::continuity::load_measured(self, &snapshot.pin(pin), &mut snapshot.stats)?
                        .as_ref()
                        == Some(receipt)
                } else {
                    true
                };
                if compare
                    && same_receipt
                    && equal_files(&snapshot.pin(candidate_pin), &snapshot.pin(pin))?
                {
                    snapshot.accept_candidate(true)?;
                }
            } else if let Some(meta) =
                read_meta(snapshot.ordered.as_mut().invariant("ordered missing")?)?
            {
                snapshot.classify(self, meta)?;
            } else {
                snapshot.ready = true;
                return Ok(Step::Done(ReplaySummary {
                    total_frames: snapshot.count,
                }));
            }
        }
        Ok(Step::More)
    }
    pub(crate) fn read_replay_batch(
        &self,
        snapshot: &mut ReplaySnapshot,
        limits: ReplayLimits,
        budget: &ReplayBudget,
    ) -> Result<ReplayBatch, StoreError> {
        if snapshot.workspace != self.root || !snapshot.ready {
            return Err(invalid("replay not ready for this store"));
        }
        if limits.max_frames == 0
            || limits.max_frames > bounded::RUN_ENTRIES
            || limits.max_bytes == 0
        {
            return Err(invalid("invalid replay batch limits"));
        }
        let mut input = File::open(snapshot.root.join("replay.manifest"))?;
        input.seek(SeekFrom::Start(snapshot.read_offset))?;
        let mut selected = Vec::with_capacity(limits.max_frames);
        let mut encoded = 0usize;
        let mut charge = 0usize;
        let mut end = snapshot.read_offset;
        let mut eof = false;
        loop {
            let Some(meta) = read_meta(&mut input)? else {
                eof = true;
                break;
            };
            let len = usize::try_from(meta.len).map_err(|_| invalid("frame length overflow"))?;
            if !selected.is_empty()
                && (selected.len() == limits.max_frames
                    || encoded
                        .checked_add(len)
                        .is_none_or(|n| n > limits.max_bytes))
            {
                break;
            }
            let allocation = meta
                .key
                .first()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<usize>().ok())
                .ok_or_else(|| invalid("invalid allocation manifest"))?;
            charge = charge
                .checked_add(allocation)
                .ok_or_else(|| invalid("allocation overflow"))?;
            encoded = encoded
                .checked_add(len)
                .ok_or_else(|| invalid("batch length overflow"))?;
            selected.push(meta);
            end = input.stream_position()?;
        }
        snapshot.stats.metadata(
            selected.len().saturating_add(1),
            selected
                .iter()
                .map(Meta::charge)
                .sum::<usize>()
                .saturating_add(bounded::MAX_METADATA.saturating_mul(2)),
            4,
        );
        // Deque backing storage is exact-capacity; empty batches charge nothing.
        let allocation = budget.acquire(charge)?;
        let mut frames = VecDeque::with_capacity(selected.len());
        let mut decoded_live = frames
            .capacity()
            .saturating_mul(std::mem::size_of::<Envelope>());
        for meta in selected {
            let bytes = frame_bytes(&snapshot.pin(meta.pin))?
                .ok_or_else(|| invalid("pinned frame invalidated"))?;
            let envelope = transport::decode(frame_payload(&bytes))
                .map_err(|_| invalid("pinned frame decode failed"))?;
            increment(&mut snapshot.stats.hydration_reads)?;
            decoded_live = decoded_live.saturating_add(heap_bytes(&envelope));
            snapshot.stats.peak_frame_bytes = snapshot
                .stats
                .peak_frame_bytes
                .max(bytes.capacity().saturating_add(decoded_live));
            frames.push_back(envelope);
        }
        snapshot.read_offset = end;
        increment(&mut snapshot.batch_number)?;
        Ok(ReplayBatch {
            number: snapshot.batch_number,
            frames,
            eof,
            encoded_bytes: encoded,
            charged_bytes: charge,
            _allocation: allocation,
        })
    }
    /// Caller must authorize the exact session after whole-replay success before
    /// EVERY quantum. Passing entries=1 permits an authorization check per unlink.
    pub(crate) fn cleanup_replay_step(
        &self,
        snapshot: &mut ReplaySnapshot,
        limit: WorkLimit,
    ) -> Result<Step<()>, StoreError> {
        let limit = limit.checked()?;
        if snapshot.workspace != self.root || !snapshot.ready {
            return Err(invalid("replay cleanup not prepared"));
        }
        let mut input = File::open(snapshot.root.join("cleanup.manifest"))?;
        input.seek(SeekFrom::Start(snapshot.cleanup_offset))?;
        for _ in 0..limit.record_entries() {
            let Some(meta) = read_meta(&mut input)? else {
                return Ok(Step::Done(()));
            };
            if !meta.pending || meta.source.parent() != Some(snapshot.pending_root.as_path()) {
                return Err(invalid("invalid pending cleanup identity"));
            }
            let source = self.root.join(&meta.source);
            let pin = snapshot.pin(meta.pin);
            if !super::continuity::cleanup_pending(self, &source, &pin, &mut snapshot.stats)?
                && same_inode(&source, &pin)?
                && equal_files(&source, &pin)?
            {
                fs::remove_file(&source)?;
            }
            snapshot.cleanup_offset = input.stream_position()?;
        }
        Ok(Step::More)
    }
}
pub(super) fn equal_files(a: &Path, b: &Path) -> Result<bool, StoreError> {
    let mut a = File::open(a)?;
    let mut b = File::open(b)?;
    if a.metadata()?.len() != b.metadata()?.len() {
        return Ok(false);
    }
    let mut left = [0; 16 * 1024];
    let mut right = [0; 16 * 1024];
    loop {
        let n = a.read(&mut left)?;
        if n == 0 {
            return Ok(true);
        }
        let (Some(read), Some(expected)) = (left.get(..n), right.get_mut(..n)) else {
            return Ok(false);
        };
        b.read_exact(expected)?;
        if read != expected {
            return Ok(false);
        }
    }
}
pub(super) fn same_inode(a: &Path, b: &Path) -> Result<bool, StoreError> {
    let a = match fs::symlink_metadata(a) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let b = fs::symlink_metadata(b)?;
    if !a.is_file() || !b.is_file() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }
    #[cfg(not(unix))]
    {
        Ok(false)
    } // Unavailable exact identity always preserves.
}

/// Snapshot/staging-only reclamation. Never execute abandoned cleanup manifests.
pub(crate) struct WorkDiscard {
    root: PathBuf,
    outer: Option<DirectoryCursor>,
    child: Option<(PathBuf, DirectoryCursor)>,
    done: bool,
}
impl WorkDiscard {
    pub(crate) fn new(root: PathBuf) -> Result<Self, StoreError> {
        if !owned_work_name(root.file_name().unwrap_or_default())
            || root.parent().and_then(Path::file_name)
                != Some(std::ffi::OsStr::new("store-work-v1"))
        {
            return Err(invalid("invalid work disposal root"));
        }
        Ok(Self {
            outer: match DirectoryCursor::open(&root) {
                Ok(cursor) => Some(cursor),
                Err(StoreError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            },
            root,
            child: None,
            done: false,
        })
    }
    pub(crate) fn step(&mut self, limit: WorkLimit) -> Result<Step<()>, StoreError> {
        let limit = limit.checked()?;
        if self.done || self.outer.is_none() {
            return Ok(Step::Done(()));
        }
        for _ in 0..limit.record_entries() {
            if let Some((path, entries)) = &mut self.child {
                if let Some(entry) = entries.peek()? {
                    if existing_kind(entry)?.is_some_and(|kind| kind.is_file())
                        && owned_work_file(path.file_name().unwrap_or_default(), &entry.file_name())
                    {
                        remove_file_if_present(&entry.path())?;
                    }
                    entries.advance();
                } else {
                    remove_empty(path)?;
                    self.child = None;
                }
            } else {
                let outer = self.outer.as_mut().invariant("outer missing")?;
                if let Some(entry) = outer.peek()? {
                    let name = entry.file_name();
                    if let Some(kind) = existing_kind(entry)? {
                        if kind.is_dir()
                            && [
                                "sort",
                                "continuity-sort",
                                "shell-sort",
                                "extras-sort",
                                "extras-sort-fallback",
                                "pins",
                                "index",
                            ]
                            .iter()
                            .any(|s| name == *s)
                        {
                            match DirectoryCursor::open(&entry.path()) {
                                Ok(cursor) => self.child = Some((entry.path(), cursor)),
                                Err(StoreError::Io(error))
                                    if error.kind() == io::ErrorKind::NotFound => {}
                                Err(error) => return Err(error),
                            }
                        } else if kind.is_file()
                            && [
                                "replay.manifest",
                                "cleanup.manifest",
                                "continuity.manifest",
                                "room.manifest",
                                "extras.manifest",
                                "retire.manifest",
                            ]
                            .iter()
                            .any(|v| name == *v)
                        {
                            remove_file_if_present(&entry.path())?;
                        }
                    }
                    outer.advance();
                } else {
                    remove_empty(&self.root)?;
                    self.done = true;
                    return Ok(Step::Done(()));
                }
            }
        }
        Ok(Step::More)
    }
}
// These helpers are only for disposal of already selected reserved work. An
// absent entry is already reclaimed; every other error retains its cursor.
fn existing_kind(entry: &fs::DirEntry) -> Result<Option<fs::FileType>, StoreError> {
    match entry.file_type() {
        Ok(kind) => Ok(Some(kind)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn remove_file_if_present(path: &Path) -> Result<(), StoreError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn remove_empty(path: &Path) -> Result<(), StoreError> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::NotFound
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}
fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn owned_work_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .and_then(|s| {
            s.strip_prefix("replay-")
                .or_else(|| s.strip_prefix("retain-"))
        })
        .is_some_and(|s| is_hex(s, 32))
}
fn owned_work_file(dir: &std::ffi::OsStr, name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if dir == "pins" {
        name.strip_suffix(".frame")
            .or_else(|| name.strip_suffix(".frame.continuity-v1"))
            .or_else(|| name.strip_suffix(".source-association"))
            .is_some_and(|s| is_hex(s, 16))
    } else if dir == "index" {
        name.strip_suffix(".idx")
            .is_some_and(|s| is_hex(s, 64) || s.strip_prefix('c').is_some_and(|s| is_hex(s, 32)))
            || name.strip_suffix(".winner").is_some_and(|s| is_hex(s, 64))
    } else if dir == "sort"
        || dir == "continuity-sort"
        || dir == "shell-sort"
        || dir == "extras-sort"
        || dir == "extras-sort-fallback"
    {
        name.strip_suffix(".run")
            .and_then(|s| s.split_once('-'))
            .is_some_and(|(a, b)| is_hex(a, 16) && is_hex(b, 16))
    } else {
        false
    }
}
/// Startup-only cursor. Finish before admitting any new work: it treats all
/// recognized old snapshots as abandoned, never as live cleanup authority.
pub(crate) struct WorkReclaimer {
    entries: Option<DirectoryCursor>,
    current: Option<WorkDiscard>,
}
impl WorkspaceStore {
    pub(crate) fn start_work_reclamation(&self) -> Result<WorkReclaimer, StoreError> {
        Ok(WorkReclaimer {
            entries: match DirectoryCursor::open(&self.root.join("store-work-v1")) {
                Ok(cursor) => Some(cursor),
                Err(StoreError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            },
            current: None,
        })
    }
}
impl WorkReclaimer {
    pub(crate) fn step(&mut self, limit: WorkLimit) -> Result<Step<()>, StoreError> {
        let limit = limit.checked()?;
        if let Some(current) = &mut self.current {
            if let Step::Done(()) = current.step(limit)? {
                self.current = None;
            }
            return Ok(Step::More);
        }
        let Some(entries) = &mut self.entries else {
            return Ok(Step::Done(()));
        };
        for _ in 0..limit.record_entries() {
            let Some(entry) = entries.peek()? else {
                return Ok(Step::Done(()));
            };
            if existing_kind(entry)?.is_some_and(|kind| kind.is_dir())
                && owned_work_name(&entry.file_name())
            {
                self.current = Some(WorkDiscard::new(entry.path())?);
                entries.advance();
                return Ok(Step::More);
            }
            entries.advance();
        }
        Ok(Step::More)
    }
}

#[cfg(test)]
#[path = "replay_tests.rs"]
mod tests;

/// One selected publication frame, sharing the normal process output budget.
/// The owner holds its ordered completion across this cursor; queued deliveries
/// own the batch/permit, so subsequent GC cannot invalidate their bytes.
pub(crate) struct PublicationCursor {
    workspace: PathBuf,
    input: File,
    recovery: bool,
}
impl PublicationCursor {
    pub(crate) fn open(generation: &Path) -> Result<Self, StoreError> {
        Self::open_manifest(generation, false)
    }
    /// The serialized workspace owner must retain its queue position through
    /// the cursor's completion; it excludes generation GC while paths are read.
    /// Delivered batches then own their bytes/permits independently of GC.
    pub(crate) fn open_recovery(generation: &Path) -> Result<Self, StoreError> {
        Self::open_manifest(generation, true)
    }
    fn open_manifest(generation: &Path, recovery: bool) -> Result<Self, StoreError> {
        #[cfg(test)]
        super::test_barrier::check(
            generation.parent().and_then(Path::parent).unwrap(),
            "shell-publication-open",
        )?;
        Ok(Self {
            workspace: generation
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| invalid("invalid publication generation"))?
                .into(),
            input: File::open(generation.join(if recovery {
                "basis.manifest"
            } else {
                "live.manifest"
            }))?,
            recovery,
        })
    }
    pub(crate) fn step(
        &mut self,
        budget: &ReplayBudget,
    ) -> Result<Step<Option<ReplayBatch>>, StoreError> {
        let offset = self.input.stream_position()?;
        #[cfg(test)]
        if self.recovery && offset > 0 {
            super::test_barrier::check(&self.workspace, "shell-recovery-read")?;
        }
        let Some(meta) = read_meta(&mut self.input)? else {
            return Ok(Step::Done(None));
        };
        if !self.recovery && meta.key.first().is_some_and(|v| v == "extra") {
            return Ok(Step::More);
        }
        // As with replay preparation, inspect one bounded legal frame to measure
        // both transport encoders. No hydrated output escapes without a permit.
        let bytes = frame_bytes(&self.workspace.join(meta.source))?
            .ok_or_else(|| invalid("invalid activation frame"))?;
        let envelope = transport::decode(frame_payload(&bytes))
            .map_err(|_| invalid("invalid activation envelope"))?;
        if self.recovery {
            let workspace_id = self
                .workspace
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.parse::<u64>().ok())
                .ok_or_else(|| invalid("invalid recovery workspace"))?;
            if !matches!(&envelope, Envelope::Delta { workspace_id: actual, .. } if *actual == workspace_id)
            {
                return Err(invalid("recovery frame outside workspace"));
            }
        }
        let v1 = transport::encode(&envelope).map_err(|_| invalid("activation encoding"))?;
        let mut charge = v1_frame_charge(&envelope, bytes.len(), &v1, v1.capacity())?;
        drop(v1);
        let v2 = tilo_delivery_protocol::transport_v2::state_payload(&envelope)
            .map_err(|_| invalid("activation v2 encoding"))?;
        charge = charge.max(frame_charge(&envelope, bytes.len(), &v2, v2.capacity())?);
        drop(v2);
        let charge = usize::try_from(charge).map_err(|_| invalid("activation charge"))?;
        let allocation = match budget.acquire(charge) {
            Ok(p) => p,
            Err(error) => {
                self.input.seek(SeekFrom::Start(offset))?;
                return Err(error);
            }
        };
        let encoded_bytes = bytes.len();
        drop(bytes);
        Ok(Step::Done(Some(ReplayBatch {
            number: 0,
            frames: VecDeque::from([envelope]),
            eof: false,
            encoded_bytes,
            charged_bytes: charge,
            _allocation: allocation,
        })))
    }
}
