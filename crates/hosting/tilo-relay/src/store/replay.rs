//! Bounded storage cursors. These confer no transport/session cleanup authority:
//! the ordered owner in persistence must supply that authority in Task 2c2.
//! No Drop implementation recursively deletes files or touches pending sources.
use super::bounded::{
    self, DirectoryCursor, Meta, Sorter, WorkStats, increment, invalid, read_meta, write_meta,
};
pub(crate) use super::bounded::{Step, WorkLimit};
use super::{StoreError, WorkspaceStore, retained};
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
/// through writes, including transport::encode's payload + output overlap.
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

/// Only reads complete legal transport frames. Invalid data remains untouched.
/// Prefix and file length are checked before any payload-sized allocation.
pub(super) fn frame_bytes(path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if !(4..=transport::MAX_FRAME_BYTES as u64 + 4).contains(&len) {
        return Ok(None);
    }
    let mut prefix = [0; 4];
    file.read_exact(&mut prefix)?;
    if u64::from(u32::from_le_bytes(prefix)) != len - 4 {
        return Ok(None);
    }
    let mut bytes = vec![0; len as usize];
    bytes[..4].copy_from_slice(&prefix);
    file.read_exact(&mut bytes[4..])?;
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
        Envelope::PeerRelayHello { hosted_workspaces } => hosted_workspaces.capacity() * 8,
        Envelope::SyncRequest { have_heads, .. } => have_heads.capacity() * 32,
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

pub(crate) struct ReplayBuild {
    root: PathBuf,
    workspace: PathBuf,
    device: [u8; 32],
    phase: u8,
    outer: Option<fs::ReadDir>,
    child: Option<(OsString, PathBuf, fs::ReadDir)>,
    sorter: Option<Sorter>,
    next_pin: u64,
    selection: Option<super::shell_selection::Selection>,
    selected: Option<File>,
    shell_selected: bool,
}
impl ReplayBuild {
    #[cfg(test)]
    pub(crate) fn stats(&self) -> Option<&WorkStats> {
        self.sorter.as_ref().map(|s| &s.stats)
    }
    pub(crate) fn into_discard(self) -> Result<WorkDiscard, StoreError> {
        WorkDiscard::new(self.root)
    }
}
struct Candidate {
    meta: Meta,
    coverage: bool,
    charge: u64,
    index: File,
}
pub(crate) struct ReplaySnapshot {
    root: PathBuf,
    workspace: PathBuf,
    workspace_id: u64,
    pending_root: PathBuf,
    sorter: Option<Sorter>,
    ordered: Option<File>,
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
    fn classify(&mut self, mut meta: Meta) -> Result<(), StoreError> {
        let Some(bytes) = frame_bytes(&self.pin(meta.pin))? else {
            return Ok(());
        };
        increment(&mut self.stats.preparation_reads)?;
        self.stats.peak_frame_bytes = self.stats.peak_frame_bytes.max(bytes.capacity());
        let Ok(envelope) = transport::decode(&bytes[4..]) else {
            return Ok(());
        };
        let decoded = heap_bytes(&envelope) + std::mem::size_of::<Envelope>();
        self.stats.peak_frame_bytes = self.stats.peak_frame_bytes.max(bytes.capacity() + decoded);

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
                    self.stats.peak_frame_bytes = self
                        .stats
                        .peak_frame_bytes
                        .max(bytes.capacity() + decoded + body.capacity());
                }
                payload.is_some()
            } else {
                false
            }
        } else {
            false
        };
        // Measure the exact allocator capacities used by transport::encode:
        // postcard's growable payload can have almost twice its encoded length.
        // The final framed Vec coexists with that payload during encoding.
        let encoded =
            postcard::to_allocvec(&envelope).map_err(|_| invalid("replay re-encoding failed"))?;
        let v1_charge = frame_charge(&envelope, bytes.len(), &encoded, encoded.capacity())?;
        self.stats.peak_frame_bytes = self
            .stats
            .peak_frame_bytes
            .max(bytes.capacity() + decoded + encoded.capacity());
        // Exact outer re-encoding rejects every unconsumed Envelope suffix;
        // `retained` independently requires complete Coedit consumption.
        let coverage = !meta.pending && retained && encoded == bytes[4..];
        drop(encoded);
        let v2 = crate::transport_v2::state_payload(&envelope)
            .map_err(|_| invalid("v2 replay re-encoding failed"))?;
        let charge = v1_charge.max(frame_charge(&envelope, bytes.len(), &v2, v2.capacity())?);
        self.stats.peak_frame_bytes = self
            .stats
            .peak_frame_bytes
            .max(bytes.capacity() + decoded + v2.capacity());
        drop(v2);
        let hash = (self.digest)(&bytes);
        // Pin length is immutable. Store measured output reservation in the
        // ordering key only in the final manifest (ordering is already done).
        meta.len = bytes.len() as u64;
        let index_path = self
            .root
            .join("index")
            .join(format!("{}.idx", hex::encode(hash)));
        let index = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(index_path)?;
        self.candidate = Some(Candidate {
            meta,
            coverage,
            charge,
            index,
        });
        Ok(())
    }
    fn accept_candidate(&mut self, duplicate: bool) -> Result<(), StoreError> {
        let candidate = self.candidate.take().unwrap();
        let Candidate {
            mut meta,
            coverage,
            charge,
            mut index,
        } = candidate;
        if meta.pending {
            write_meta(&mut self.cleanup, &meta)?;
        }
        if !duplicate {
            if !meta.pending {
                index.write_all(&meta.pin.to_le_bytes())?;
                index.write_all(&meta.len.to_le_bytes())?;
                index.write_all(&[u8::from(coverage)])?;
            }
            meta.key = vec![charge.to_string().into()];
            write_meta(&mut self.output, &meta)?;
            increment(&mut self.count)?;
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
            root,
            workspace: self.root.clone(),
            device,
            phase: if pending_only { 3 } else { 0 },
            outer: None,
            child: None,
            sorter: Some(sorter),
            next_pin: 0,
        })
    }
    /// Invoke under one owner operation until Done; later mutations must not
    /// interleave before every selected source has its immutable hardlink.
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
            sorter.stats.last_step_work += 1;
            if build.phase == 4 {
                build.phase = 5;
                let output = File::create(build.root.join("replay.manifest"))?;
                let cleanup = File::create(build.root.join("cleanup.manifest"))?;
                return Ok(Step::Done(ReplaySnapshot {
                    root: build.root.clone(),
                    workspace: self.root.clone(),
                    workspace_id: self.workspace_id,
                    pending_root: PathBuf::from("pending").join(hex::encode(build.device)),
                    sorter: build.sorter.take(),
                    ordered: None,
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
                    sorter.push(Meta {
                        key: vec!["1".into(), "0".into(), format!("{pin:016x}").into()],
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
                    0 => self.root.join("retained/checkpoints"),
                    1 => self.root.join("retained/tails"),
                    2 => self.root.join("retained/v2"),
                    _ => self.pending_dir_for(&build.device),
                };
                build.outer = open_dir(&dir)?;
                if build.outer.is_none() {
                    build.phase += 1;
                    continue;
                }
            }
            let selected = if let Some((channel, _, child)) = &mut build.child {
                match child.next() {
                    Some(entry) => {
                        let entry = entry?;
                        let name = entry.file_name();
                        let valid = build.phase == 1
                            || (name.to_string_lossy().ends_with(".delta")
                                && !name.to_string_lossy().starts_with('.'));
                        if entry.file_type()?.is_file() && valid {
                            let tier = if build.phase == 1 { "0" } else { "1" };
                            let order = if build.phase == 2 && name == "checkpoint.delta" {
                                "0"
                            } else {
                                "1"
                            };
                            Some((
                                entry.path(),
                                vec![tier.into(), channel.clone(), order.into(), name],
                            ))
                        } else {
                            None
                        }
                    }
                    None => {
                        build.child = None;
                        None
                    }
                }
            } else {
                match build.outer.as_mut().unwrap().next() {
                    Some(entry) => {
                        let entry = entry?;
                        let name = entry.file_name();
                        let kind = entry.file_type()?;
                        match build.phase {
                            0 if kind.is_file() => {
                                Some((entry.path(), vec!["0".into(), name, "0".into()]))
                            }
                            1 if kind.is_dir() => {
                                build.child =
                                    Some((name, entry.path(), fs::read_dir(entry.path())?));
                                None
                            }
                            2 if kind.is_dir() => {
                                if let Some(active) = retained::current(&entry.path())? {
                                    build.child =
                                        Some((name, active.clone(), fs::read_dir(active)?));
                                }
                                None
                            }
                            3 if kind.is_file() && name.to_string_lossy().ends_with(".delta") => {
                                Some((entry.path(), vec!["2".into(), name]))
                            }
                            _ => None,
                        }
                    }
                    None => {
                        build.outer = None;
                        build.phase += 1;
                        None
                    }
                }
            };
            if let Some((source, key)) = selected {
                let pin = build.next_pin;
                increment(&mut build.next_pin)?;
                let target = build.root.join("pins").join(format!("{pin:016x}.frame"));
                fs::hard_link(&source, &target)?;
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
            snapshot.stats.peak_open_files += 2; // output + cleanup manifests
            if let Step::Done(path) = result {
                snapshot.ordered = Some(File::open(path)?);
                snapshot.sorter = None;
            }
            return Ok(Step::More);
        }
        snapshot.stats.peak_open_files = snapshot.stats.peak_open_files.max(6);
        for _ in 0..limit.record_entries() {
            snapshot.stats.last_step_work += 1;
            if let Some(candidate) = &mut snapshot.candidate {
                let mut entry = [0; 17];
                if candidate.index.read(&mut entry[..1])? == 0 {
                    snapshot.accept_candidate(false)?;
                    continue;
                }
                candidate.index.read_exact(&mut entry[1..])?;
                let pin = u64::from_le_bytes(entry[..8].try_into().unwrap());
                let len = u64::from_le_bytes(entry[8..16].try_into().unwrap());
                let compare =
                    len == candidate.meta.len && (!candidate.meta.pending || entry[16] == 1);
                let candidate_pin = candidate.meta.pin;
                if compare && equal_files(&snapshot.pin(candidate_pin), &snapshot.pin(pin))? {
                    snapshot.accept_candidate(true)?;
                }
            } else if let Some(meta) = read_meta(snapshot.ordered.as_mut().unwrap())? {
                snapshot.classify(meta)?;
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
            selected.len() + 1,
            selected.iter().map(Meta::charge).sum::<usize>() + bounded::MAX_METADATA * 2,
            4,
        );
        // Deque backing storage is exact-capacity; empty batches charge nothing.
        let allocation = budget.acquire(charge)?;
        let mut frames = VecDeque::with_capacity(selected.len());
        let mut decoded_live = frames.capacity() * std::mem::size_of::<Envelope>();
        for meta in selected {
            let bytes = frame_bytes(&snapshot.pin(meta.pin))?
                .ok_or_else(|| invalid("pinned frame invalidated"))?;
            let envelope = transport::decode(&bytes[4..])
                .map_err(|_| invalid("pinned frame decode failed"))?;
            increment(&mut snapshot.stats.hydration_reads)?;
            decoded_live += heap_bytes(&envelope);
            snapshot.stats.peak_frame_bytes = snapshot
                .stats
                .peak_frame_bytes
                .max(bytes.capacity() + decoded_live);
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
            if same_inode(&source, &pin)? && equal_files(&source, &pin)? {
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
        b.read_exact(&mut right[..n])?;
        if left[..n] != right[..n] {
            return Ok(false);
        }
    }
}
fn same_inode(a: &Path, b: &Path) -> Result<bool, StoreError> {
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
                let outer = self.outer.as_mut().unwrap();
                if let Some(entry) = outer.peek()? {
                    let name = entry.file_name();
                    if let Some(kind) = existing_kind(entry)? {
                        if kind.is_dir()
                            && [
                                "sort",
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
        name.strip_suffix(".frame").is_some_and(|s| is_hex(s, 16))
    } else if dir == "index" {
        name.strip_suffix(".idx").is_some_and(|s| is_hex(s, 64))
    } else if dir == "sort"
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

/// One selected activation frame, sharing the normal process output budget.
/// The owner holds its ordered completion across this cursor; queued deliveries
/// own the batch/permit, so subsequent GC cannot invalidate their bytes.
pub(crate) struct PublicationCursor {
    workspace: PathBuf,
    input: File,
}
impl PublicationCursor {
    pub(crate) fn open(generation: &Path) -> Result<Self, StoreError> {
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
            input: File::open(generation.join("live.manifest"))?,
        })
    }
    pub(crate) fn step(
        &mut self,
        budget: &ReplayBudget,
    ) -> Result<Step<Option<ReplayBatch>>, StoreError> {
        let offset = self.input.stream_position()?;
        let Some(meta) = read_meta(&mut self.input)? else {
            return Ok(Step::Done(None));
        };
        if meta.key.first().is_some_and(|v| v == "extra") {
            return Ok(Step::More);
        }
        // As with replay preparation, inspect one bounded legal frame to measure
        // both transport encoders. No hydrated output escapes without a permit.
        let bytes = frame_bytes(&self.workspace.join(meta.source))?
            .ok_or_else(|| invalid("invalid activation frame"))?;
        let envelope =
            transport::decode(&bytes[4..]).map_err(|_| invalid("invalid activation envelope"))?;
        let v1 = postcard::to_allocvec(&envelope).map_err(|_| invalid("activation encoding"))?;
        let mut charge = frame_charge(&envelope, bytes.len(), &v1, v1.capacity())?;
        drop(v1);
        let v2 = crate::transport_v2::state_payload(&envelope)
            .map_err(|_| invalid("activation v2 encoding"))?;
        charge = charge.max(frame_charge(&envelope, bytes.len(), &v2, v2.capacity())?);
        drop(v2);
        let allocation = match budget.acquire(charge as usize) {
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
            charged_bytes: charge as usize,
            _allocation: allocation,
        })))
    }
}
