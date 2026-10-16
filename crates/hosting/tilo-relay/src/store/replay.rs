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
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tilo_lock::MutexRecover as _;
pub(super) use tilo_relay_store::frames::{equal_files, frame_bytes};

pub(crate) const DEFAULT_BATCH_FRAMES: usize = 64;
pub(crate) const DEFAULT_BATCH_BYTES: usize = 1024 * 1024;
pub(crate) const DEFAULT_OUTPUT_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const DEFAULT_REPLAY_TICKETS: usize = 32;
/// Names one replay's work directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayId(pub [u8; 16]);

// Captures that listed a lineage from what an earlier one learned, on this
// thread: what a test reads to see the listing remembered.
#[cfg(test)]
thread_local! {
    pub(crate) static LISTING_HITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// A capture lists the retained frames it replays in its owner turn and
/// pins them (one hard link each) when it is prepared, off the owner: the
/// pins were the bulk of a newcomer's turn, and the frames to pin grow with
/// the room. In between, the listed frames must stay where they were
/// listed. Only collection removes a retained frame, so collection waits
/// while any capture of the store is leased: a lineage collected then is
/// remembered, and collected once no capture is leased
/// ([`WorkspaceStore::deferred_collections`]).
///
/// Leases are taken in the owner's turn, and collection is decided there,
/// so no collection that a lease should stop can have begun before it.
#[derive(Default)]
pub(crate) struct CaptureLeases {
    held: AtomicUsize,
    deferred: std::sync::Mutex<std::collections::BTreeSet<PathBuf>>,
    /// The store's owner, woken when the last lease ends so what waited is
    /// collected without waiting for its next job.
    wake: std::sync::Mutex<Option<Arc<tokio::sync::Notify>>>,
    /// This host governs the store's room, and so stewards its channels:
    /// captures leave out what only the steward reads
    /// ([`WorkspaceStore::replay_without_steward_only`]).
    steward_here: std::sync::atomic::AtomicBool,
    /// Whether each retained lineage a capture looked at holds what only a
    /// channel's steward reads. A lineage's route names its kind of message
    /// for good, so the answer never changes: each is read once.
    steward_only: std::sync::Mutex<std::collections::HashMap<std::ffi::OsString, bool>>,
    /// What each lineage's current generation held when last listed.
    listed: std::sync::Mutex<ListedLineages>,
}
impl CaptureLeases {
    /// The leases of the store at `root`, shared by every handle on it.
    pub(crate) fn for_root(root: &Path) -> Arc<Self> {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock, Weak};
        static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<CaptureLeases>>>> = OnceLock::new();
        let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let mut registry = REGISTRY.get_or_init(Mutex::default).lock_recover();
        registry.retain(|_, leases| leases.strong_count() > 0);
        if let Some(leases) = registry.get(&root).and_then(Weak::upgrade) {
            return leases;
        }
        let leases = Arc::new(Self::default());
        registry.insert(root, Arc::downgrade(&leases));
        leases
    }
    fn deferred(&self) -> std::sync::MutexGuard<'_, std::collections::BTreeSet<PathBuf>> {
        self.deferred.lock_recover()
    }
}
/// One capture's hold on its store's collection, until its frames are
/// pinned or it is given up.
pub(crate) struct CaptureLease(Arc<CaptureLeases>);
impl Drop for CaptureLease {
    fn drop(&mut self) {
        if self.0.held.fetch_sub(1, Ordering::SeqCst) == 1
            && !self.0.deferred().is_empty()
            && let Some(wake) = self.0.wake.lock_recover().as_ref()
        {
            wake.notify_one();
        }
    }
}
impl WorkspaceStore {
    /// Wake `owner` when the last capture lease ends with collections
    /// waiting for it.
    pub(crate) fn wake_on_capture_release(&self, owner: Arc<tokio::sync::Notify>) {
        *self.capture_leases.wake.lock_recover() = Some(owner);
    }
    /// From now on, leave the lineages of what only a channel's steward
    /// reads (access requests, acknowledgements:
    /// [`crate::payloads::ShellPayloads::steward_only`]) out of every
    /// capture: this host governs the room, so it stewards the room's
    /// channels itself, and no member replaying needs them. They stay
    /// retained.
    pub(crate) fn replay_without_steward_only(&self) {
        self.capture_leases
            .steward_here
            .store(true, Ordering::SeqCst);
    }

    /// Whether a capture leaves the retained lineage `name` at `lineage`
    /// out. A lineage is classified by its checkpoint once, then from
    /// memory: no capture reads it again.
    fn leaves_out(&self, name: &std::ffi::OsStr, lineage: &Path) -> Result<bool, StoreError> {
        /// A larger checkpoint is no access request or acknowledgement.
        const MAX_CLASSIFIED_BYTES: u64 = 1024 * 1024;
        if !self.capture_leases.steward_here.load(Ordering::SeqCst) {
            return Ok(false);
        }
        let known = self
            .capture_leases
            .steward_only
            .lock_recover()
            .get(name)
            .copied();
        if let Some(known) = known {
            return Ok(known);
        }
        // A lineage with no generation yet is not classified: its first
        // frame is still being retained.
        let Some(active) = retained::current(lineage)? else {
            return Ok(false);
        };
        let checkpoint = active.join("checkpoint.delta");
        let steward_only = match fs::symlink_metadata(&checkpoint) {
            Ok(meta) if meta.is_file() && meta.len() <= MAX_CLASSIFIED_BYTES => {
                let bytes = fs::read(&checkpoint)?;
                matches!(
                    retained::payload(&bytes, self.workspace_id),
                    Some((_, crate::coedit::CoeditPayload::Checkpoint { body, .. }))
                        if self.payloads.steward_only(&body)
                )
            }
            // What only a steward reads is a checkpoint that stands alone:
            // a lineage of deltas never holds it.
            Ok(_) => false,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        self.capture_leases
            .steward_only
            .lock_recover()
            .insert(name.to_owned(), steward_only);
        Ok(steward_only)
    }

    fn listed_lineages(&self) -> std::sync::MutexGuard<'_, ListedLineages> {
        self.capture_leases.listed.lock_recover()
    }

    /// Forget what captures listed of the lineage directories `touched`, or
    /// of every lineage when `None` ([`ListedLineages`]).
    pub(crate) fn forget_listed(&self, touched: Option<&[OsString]>) {
        let mut listed = self.listed_lineages();
        match touched {
            None => *listed = ListedLineages::default(),
            Some(touched) => {
                for name in touched {
                    if let Some(gone) = listed.lineages.remove(name) {
                        // Counted: remembered listings a write ended.
                        crate::timing::window("capture.lineage_forgotten", crate::timing::start());
                        listed.frames = listed.frames.saturating_sub(gone.frames.len());
                    }
                }
            }
        }
    }

    /// Forget what captures listed of every lineage on `channel`, of any
    /// author.
    pub(crate) fn forget_channel_listing(&self, channel: &[u8; 32]) {
        let suffix = format!("-{}", hex::encode(channel));
        let mut listed = self.listed_lineages();
        let mut gone = 0usize;
        listed.lineages.retain(|name, known| {
            let keep = !name.to_string_lossy().ends_with(&suffix);
            if !keep {
                gone = gone.saturating_add(known.frames.len());
            }
            keep
        });
        listed.frames = listed.frames.saturating_sub(gone);
    }

    /// Forget what captures listed of the lineage directory `lineage`, about
    /// to change: retention does so for every lineage it writes, whoever
    /// drives it.
    pub(crate) fn forget_lineage_listing(&self, lineage: &Path) {
        if let Some(name) = lineage.file_name() {
            self.forget_listed(Some(&[name.to_owned()]));
        }
    }

    /// The lineages whose listing captures remember, sorted.
    #[cfg(test)]
    pub(crate) fn listed_lineage_names(&self) -> Vec<OsString> {
        let mut names: Vec<_> = self.listed_lineages().lineages.keys().cloned().collect();
        names.sort();
        names
    }

    /// What a capture last listed of lineage `name`, while no owner job
    /// changed it since.
    fn known_listing(&self, name: &OsStr) -> Option<(PathBuf, Vec<OsString>)> {
        if self.shell_binding().is_some() {
            return None;
        }
        let known = self
            .listed_lineages()
            .lineages
            .get(name)
            .map(|known| (known.generation.clone(), known.frames.clone()))?;
        #[cfg(test)]
        LISTING_HITS.with(|hits| hits.set(hits.get().saturating_add(1)));
        Some(known)
    }

    /// Remember that lineage `name`'s current `generation` holds `frames`.
    fn learn_listing(&self, name: OsString, generation: PathBuf, frames: Vec<OsString>) {
        if self.shell_binding().is_some() {
            return;
        }
        let mut listed = self.listed_lineages();
        if listed.frames.saturating_add(frames.len()) > MAX_LISTED_FRAMES {
            return;
        }
        listed.frames = listed.frames.saturating_add(frames.len());
        if let Some(replaced) = listed.lineages.insert(name, Listed { generation, frames }) {
            listed.frames = listed.frames.saturating_sub(replaced.frames.len());
        }
    }

    fn lease_capture(&self) -> CaptureLease {
        self.capture_leases.held.fetch_add(1, Ordering::SeqCst);
        CaptureLease(Arc::clone(&self.capture_leases))
    }
    /// The collection of `lineage`'s replaced generations, or `None` while
    /// a capture is leased: the lineage is collected later then.
    /// `after_commit` as [`retained::GenerationGc::after_commit`].
    ///
    /// # Errors
    ///
    /// Returns an error when the lineage cannot be listed.
    pub(crate) fn collect_or_defer(
        &self,
        lineage: PathBuf,
        after_commit: bool,
    ) -> Result<Option<retained::GenerationGc>, StoreError> {
        if self.capture_leases.held.load(Ordering::SeqCst) != 0 {
            self.capture_leases.deferred().insert(lineage);
            return Ok(None);
        }
        if after_commit {
            retained::GenerationGc::after_commit(lineage).map(Some)
        } else {
            retained::GenerationGc::new(lineage).map(Some)
        }
    }
    /// The lineages whose collection waited for a capture, once none is
    /// leased; none while one is.
    pub(crate) fn deferred_collections(&self) -> Vec<PathBuf> {
        if self.capture_leases.held.load(Ordering::SeqCst) != 0 {
            return Vec::new();
        }
        std::mem::take(&mut *self.capture_leases.deferred())
            .into_iter()
            .collect()
    }
}
/// The most one replay batch carries.
#[derive(Clone, Copy)]
pub struct ReplayLimits {
    /// Frames per batch.
    pub max_frames: usize,
    /// Frame bytes per batch.
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
pub struct ReplaySummary {
    pub total_frames: u64,
    /// Of `total_frames`, those replayed from the device's pending queue
    /// rather than retained history.
    pub pending_frames: u64,
}
/// A caller shares this budget across all owners; the batch keeps its permit
/// through writes, including `transport::encode`'s payload + output overlap.
#[derive(Clone)]
pub struct ReplayBudget {
    inner: Arc<Budget>,
}
struct Budget {
    capacity: usize,
    used: AtomicUsize,
}
impl ReplayBudget {
    /// A budget of `capacity` bytes of replayed frames held at once.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Budget {
                capacity,
                used: AtomicUsize::new(0),
            }),
        }
    }
    /// Bytes currently held against the budget.
    pub fn used(&self) -> usize {
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
pub struct ReplayBatch {
    pub number: u64,
    frames: VecDeque<Envelope>,
    pub eof: bool,
    pub encoded_bytes: usize,
    pub charged_bytes: usize,
    _allocation: ReplayAllocation,
}

impl ReplayBatch {
    /// The next frame to write.
    pub fn front(&self) -> Option<&Envelope> {
        self.frames.front()
    }
    /// Drop the frame [`Self::front`] returned, now written.
    pub fn pop_written(&mut self) {
        self.frames.pop_front();
    }
    pub(crate) fn len(&self) -> usize {
        self.frames.len()
    }
    /// The frames still to write, in order.
    pub(crate) fn frames(&self) -> impl Iterator<Item = &Envelope> {
        self.frames.iter()
    }
}

/// The transport payload after a frame's four-byte length prefix. A frame
/// from `frame_bytes` always has one; anything shorter decodes as empty.
fn frame_payload(bytes: &[u8]) -> &[u8] {
    bytes.get(4..).unwrap_or_default()
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

pub struct ReplayBuild {
    root: PathBuf,
    workspace: PathBuf,
    device: [u8; 32],
    /// What a catch-up held while it ran, which its final cut takes before
    /// the device's own queue (where commits with continuity receipts wait).
    held: Option<PathBuf>,
    /// The held queue was listed; the device's own comes next.
    held_listed: bool,
    phase: u8,
    outer: Option<fs::ReadDir>,
    child: Option<Lineage>,
    sorter: Option<Sorter>,
    next_pin: u64,
    continuity: Option<File>,
    selection: Option<Box<dyn super::shell_selection::ShellSelection>>,
    selected: Option<File>,
    shell_selected: bool,
    skip: Option<ResumeSkip>,
    /// The frames listed to pin when the capture is prepared
    /// (`capture.manifest`): the retained frames, with the lease that keeps
    /// them in place until then ([`CaptureLeases`]), or a final cut's held
    /// commits, which nothing changes once the cut is taken. The device's
    /// own queue is pinned at once. Buffered: a capture lists a frame per
    /// entry in the owner's turn, and two writes each were most of it.
    listed: Option<(io::BufWriter<File>, Option<CaptureLease>)>,
}
/// The order a capture lists retained frame `name` of lineage `channel` in:
/// by the lineage's rank, the lineage, its checkpoint first, then by name.
fn lineage_key(channel: &OsStr, name: OsString) -> Vec<OsString> {
    let order = if name == "checkpoint.delta" { "0" } else { "1" };
    let rank = lineage_rank(&channel.to_string_lossy());
    vec![rank.into(), channel.to_owned(), order.into(), name]
}

/// The retained lineage a capture is listing: from its generation, or from
/// what an earlier capture learned of it ([`ListedLineages`]).
enum Lineage {
    Reading {
        channel: OsString,
        generation: PathBuf,
        entries: fs::ReadDir,
        /// The frames listed so far, to remember once the generation is
        /// listed whole; `None` past what one lineage remembers.
        learned: Option<Vec<OsString>>,
    },
    Known {
        channel: OsString,
        generation: PathBuf,
        frames: std::vec::IntoIter<OsString>,
    },
}

/// Frames one lineage's listing is remembered for: a capture lists larger
/// lineages from their generation every time.
const MAX_LISTED_LINEAGE: usize = 256;
/// Frames remembered over every lineage of a store. Past it, a capture
/// remembers no more lineages until an owner job changes one.
const MAX_LISTED_FRAMES: usize = 65_536;

/// What each retained lineage's current generation held when a capture
/// listed it, until an owner job changes the lineage. In a room every
/// newcomer's capture listed every lineage again, a generation's `CURRENT`
/// and its entries each time, in the owner's turn; now it lists only the
/// lineages that changed since the last capture.
///
/// It is exact, not a cache to validate. Retention forgets a lineage before
/// it writes it ([`WorkspaceStore::forget_lineage_listing`]), whoever
/// drives it. A current generation changes only in an owner job, and after
/// every normal job but one that says it changed retained history only
/// through retention, the owner forgets every lineage
/// ([`WorkspaceStore::forget_listed`]). What changes off the owner (a
/// stage, a collection, a discard) is never a lineage's current
/// generation. A store with a shell binding remembers nothing: its
/// selection retires retained records itself.
#[derive(Default)]
pub(crate) struct ListedLineages {
    lineages: std::collections::HashMap<OsString, Listed>,
    frames: usize,
}

struct Listed {
    generation: PathBuf,
    frames: Vec<OsString>,
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
    /// Discard this build's work directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be listed.
    pub fn into_discard(self) -> Result<WorkDiscard, StoreError> {
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
/// The retained frames a capture listed and its preparation pins first.
struct Link {
    /// `capture.manifest`: one entry per listed frame, its pin assigned.
    listed: File,
    /// `continuity.manifest`, which a pinned frame's receipt joins.
    continuity: File,
    _lease: Option<CaptureLease>,
}
/// A captured replay: pinned retained frames, ready to prepare and read.
pub struct ReplaySnapshot {
    /// Retained frames listed in the owner's turn, not pinned yet.
    link: Option<Link>,
    root: PathBuf,
    workspace: PathBuf,
    workspace_id: u64,
    pending_root: PathBuf,
    /// A final cut's held queue, which it cleans up like the device's own.
    held_root: Option<PathBuf>,
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
    pending_count: u64,
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
    /// Discard this snapshot's work directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be listed.
    pub fn into_discard(self) -> Result<WorkDiscard, StoreError> {
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
                if meta.pending {
                    increment(&mut self.pending_count)?;
                }
            }
        }
        Ok(())
    }
}
impl WorkspaceStore {
    pub(crate) fn replay_capture_has_basis(&self) -> Result<bool, StoreError> {
        Ok(self.shell_binding().is_none()
            || retained::current(&self.root.join(super::shell_selection::DIRECTORY))?.is_some())
    }

    /// Begin capturing a replay for `device`.
    ///
    /// # Errors
    ///
    /// Returns an error when the capture's work directory cannot be made
    /// or the shell selection it starts with cannot be opened.
    pub fn start_replay_capture(
        &self,
        id: ReplayId,
        device: [u8; 32],
    ) -> Result<ReplayBuild, StoreError> {
        self.start_replay_capture_at(id, device, None)
    }
    /// The final cut of a catch-up: the queue under `held`, which must be in
    /// this store's work directory.
    pub(crate) fn start_pending_replay_capture(
        &self,
        id: ReplayId,
        device: [u8; 32],
        held: &Path,
    ) -> Result<ReplayBuild, StoreError> {
        if held.parent() != Some(self.root.join(WORK_DIR).as_path()) {
            return Err(invalid("held catch-up outside the store's work"));
        }
        self.start_replay_capture_at(id, device, Some(held.to_owned()))
    }
    fn start_replay_capture_at(
        &self,
        id: ReplayId,
        device: [u8; 32],
        held: Option<PathBuf>,
    ) -> Result<ReplayBuild, StoreError> {
        crate::timing::windowed("owner.replay.capture_start", || {
            self.start_replay_capture_in(id, device, held)
        })
    }
    fn start_replay_capture_in(
        &self,
        id: ReplayId,
        device: [u8; 32],
        held: Option<PathBuf>,
    ) -> Result<ReplayBuild, StoreError> {
        let pending_only = held.is_some();
        let root = self.root.join(WORK_DIR);
        fs::create_dir_all(&root)?;
        let root = root.join(format!("replay-{}", hex::encode(id.0)));
        fs::create_dir(&root)?;
        fs::create_dir(root.join("pins"))?;
        fs::create_dir(root.join("index"))?;
        let sorter = Sorter::new(root.join("sort"))?;
        let selection = if pending_only {
            None
        } else {
            self.shell_selection(root.clone(), None)?
        };
        Ok(ReplayBuild {
            selection,
            selected: if !pending_only && self.shell_binding().is_none() {
                retained::current(&self.root.join(super::shell_selection::DIRECTORY))?
                    .map(|p| File::open(p.join("basis.manifest")))
                    .transpose()?
            } else {
                None
            },
            shell_selected: !pending_only && self.shell_binding().is_some(),
            root: root.clone(),
            workspace: self.root.clone(),
            device,
            held,
            held_listed: false,
            phase: if pending_only { 3 } else { 2 },
            outer: None,
            child: None,
            sorter: Some(sorter),
            next_pin: 0,
            continuity: Some(File::create(root.join("continuity.manifest"))?),
            skip: None,
            listed: Some((
                io::BufWriter::new(File::create(root.join("capture.manifest"))?),
                (!pending_only).then(|| self.lease_capture()),
            )),
        })
    }
    /// Invoke under one owner operation until Done; later mutations must not
    /// interleave before every selected source has its immutable hardlink,
    /// or, for a retained frame, is listed under the capture's lease (it is
    /// pinned when the snapshot is prepared: [`CaptureLeases`]).
    #[expect(
        clippy::too_many_lines,
        reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
    )]
    ///
    /// # Errors
    ///
    /// Returns an error when retained history cannot be read or pinned.
    pub fn capture_replay_step(
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
                // Retained frames still to pin keep the continuity manifest
                // open for theirs; it is read once they are pinned.
                let link = match build.listed.take() {
                    Some((listed, lease)) => {
                        // Written out before it is read back.
                        drop(
                            listed
                                .into_inner()
                                .map_err(io::IntoInnerError::into_error)?,
                        );
                        Some(Link {
                            listed: File::open(build.root.join("capture.manifest"))?,
                            continuity: build.continuity.take().invariant("continuity missing")?,
                            _lease: lease,
                        })
                    }
                    None => None,
                };
                let continuity_input = if link.is_none() {
                    drop(build.continuity.take());
                    Some(File::open(build.root.join("continuity.manifest"))?)
                } else {
                    None
                };
                let output = File::create(build.root.join("replay.manifest"))?;
                let cleanup = File::create(build.root.join("cleanup.manifest"))?;
                return Ok(Step::Done(ReplaySnapshot {
                    link,
                    root: build.root.clone(),
                    workspace: self.root.clone(),
                    workspace_id: self.workspace_id,
                    pending_root: PathBuf::from("pending").join(hex::encode(build.device)),
                    held_root: build
                        .held
                        .as_ref()
                        .map(|held| {
                            held.strip_prefix(&self.root)
                                .map(Path::to_path_buf)
                                .map_err(|_| invalid("invalid held catch-up path"))
                        })
                        .transpose()?,
                    sorter: build.sorter.take(),
                    ordered: None,
                    continuity_sorter: None,
                    continuity_input,
                    continuity_ordered: None,
                    continuity_previous: None,
                    candidate: None,
                    output,
                    cleanup,
                    ready: false,
                    count: 0,
                    pending_count: 0,
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
                let dir = match (build.phase, &build.held) {
                    (2, _) => self.root.join("retained/v2"),
                    (_, Some(held)) if !build.held_listed => held.clone(),
                    _ => self.pending_dir_for(&build.device),
                };
                build.outer = open_dir(&dir)?;
                if build.outer.is_none() {
                    listed(
                        &mut build.phase,
                        build.held.is_some(),
                        &mut build.held_listed,
                    );
                    continue;
                }
            }
            let selected = if let Some(lineage) = &mut build.child {
                match lineage {
                    Lineage::Reading {
                        channel,
                        generation,
                        entries,
                        learned,
                    } => {
                        if let Some(entry) = entries.next() {
                            let entry = entry?;
                            let name = entry.file_name();
                            let valid = name.to_string_lossy().ends_with(".delta")
                                && !name.to_string_lossy().starts_with('.');
                            if entry.file_type()?.is_file() && valid {
                                if let Some(frames) = learned {
                                    if frames.len() < MAX_LISTED_LINEAGE {
                                        frames.push(name.clone());
                                    } else {
                                        *learned = None;
                                    }
                                }
                                Some((entry.path(), lineage_key(channel, name)))
                            } else {
                                None
                            }
                        } else {
                            if let Some(frames) = learned.take() {
                                self.learn_listing(
                                    std::mem::take(channel),
                                    std::mem::take(generation),
                                    frames,
                                );
                            }
                            build.child = None;
                            None
                        }
                    }
                    Lineage::Known {
                        channel,
                        generation,
                        frames,
                    } => {
                        if let Some(name) = frames.next() {
                            Some((generation.join(&name), lineage_key(channel, name)))
                        } else {
                            build.child = None;
                            None
                        }
                    }
                }
            } else if let Some(entry) = build.outer.as_mut().invariant("outer missing")?.next() {
                let entry = entry?;
                let name = entry.file_name();
                let kind = entry.file_type()?;
                match build.phase {
                    2 if kind.is_dir() => {
                        let lineage = entry.path();
                        if !self.leaves_out(&name, &lineage)? {
                            // Counted: lineages listed from memory and
                            // from their generation.
                            build.child =
                                if let Some((generation, frames)) = self.known_listing(&name) {
                                    crate::timing::window(
                                        "capture.lineage_known",
                                        crate::timing::start(),
                                    );
                                    Some(Lineage::Known {
                                        channel: name,
                                        generation,
                                        frames: frames.into_iter(),
                                    })
                                } else if let Some(active) = retained::current(&lineage)? {
                                    crate::timing::window(
                                        "capture.lineage_read",
                                        crate::timing::start(),
                                    );
                                    Some(Lineage::Reading {
                                        channel: name,
                                        entries: fs::read_dir(&active)?,
                                        generation: active,
                                        learned: Some(Vec::new()),
                                    })
                                } else {
                                    // A lineage whose first checkpoint is staged
                                    // but not committed yet.
                                    None
                                };
                        }
                        None
                    }
                    3 if kind.is_file() && name.to_string_lossy().ends_with(".delta") => {
                        // Counted: what a final cut takes from the commits
                        // its catch-up held, and from the device's queue.
                        crate::timing::window(
                            if build.held.is_some() && !build.held_listed {
                                "capture.held_entry"
                            } else {
                                "capture.queued_entry"
                            },
                            crate::timing::start(),
                        );
                        Some((entry.path(), vec!["2".into(), name]))
                    }
                    _ => None,
                }
            } else {
                build.outer = None;
                listed(
                    &mut build.phase,
                    build.held.is_some(),
                    &mut build.held_listed,
                );
                None
            };
            if let Some((source, key)) = selected {
                let pin = build.next_pin;
                increment(&mut build.next_pin)?;
                // Retained frames, and a final cut's held commits, are pinned
                // when the capture is prepared, off the owner.
                let held = build.phase == 3 && build.held.is_some() && !build.held_listed;
                if (build.phase == 2 || held)
                    && let Some((listed, _)) = build.listed.as_mut()
                {
                    write_meta(
                        listed,
                        &Meta {
                            key,
                            source: source
                                .strip_prefix(&self.root)
                                .map_err(|_| invalid("invalid capture path"))?
                                .into(),
                            pin,
                            len: 0,
                            pending: held,
                        },
                    )?;
                    continue;
                }
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
    /// One quantum of pinning the retained frames a capture listed, as the
    /// capture pinned the rest: a hard link, its continuity receipt, and
    /// its place in the order. Once every listed frame is pinned the lease
    /// ends, and retention may collect what it replaced meanwhile.
    fn pin_listed_step(
        &self,
        snapshot: &mut ReplaySnapshot,
        limit: WorkLimit,
    ) -> Result<Step<ReplaySummary>, StoreError> {
        let link = snapshot.link.as_mut().invariant("listed frames missing")?;
        let sorter = snapshot
            .sorter
            .as_mut()
            .invariant("listed frames without their order")?;
        sorter.stats.last_step_work = 0;
        if sorter.spilling() {
            sorter.spill_step(limit)?;
            snapshot.stats.last_step_work = sorter.stats.last_step_work;
            return Ok(Step::More);
        }
        for _ in 0..limit.record_entries() {
            sorter.stats.last_step_work = sorter.stats.last_step_work.saturating_add(1);
            let Some(meta) = read_meta(&mut link.listed)? else {
                snapshot.link = None;
                snapshot.continuity_input =
                    Some(File::open(snapshot.root.join("continuity.manifest"))?);
                break;
            };
            let source = self.root.join(&meta.source);
            let target = snapshot
                .root
                .join("pins")
                .join(format!("{:016x}.frame", meta.pin));
            fs::hard_link(&source, &target)?;
            capture_continuity(
                self,
                &source,
                &target,
                meta.pin,
                &mut link.continuity,
                &mut sorter.stats,
            )?;
            let len = fs::metadata(&target)?.len();
            sorter.push(Meta { len, ..meta })?;
            sorter.stats.peak_open_files = sorter.stats.peak_open_files.max(4);
            if sorter.spilling() {
                break;
            }
        }
        snapshot.stats.last_step_work = sorter.stats.last_step_work;
        Ok(Step::More)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
    )]
    /// One quantum of ordering a captured snapshot's frames.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot cannot be read or sorted.
    pub fn prepare_replay_step(
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
                pending_frames: snapshot.pending_count,
            }));
        }
        snapshot.stats.last_step_work = 0;
        if snapshot.link.is_some() {
            return self.pin_listed_step(snapshot, limit);
        }
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
                    pending_frames: snapshot.pending_count,
                }));
            }
        }
        Ok(Step::More)
    }
    /// The next batch of a prepared snapshot's frames.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot is not ready for this store or a
    /// frame cannot be read.
    pub fn read_replay_batch(
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
            let parent = meta.source.parent();
            if !meta.pending
                || (parent != Some(snapshot.pending_root.as_path())
                    && parent != snapshot.held_root.as_deref())
            {
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
pub struct WorkDiscard {
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
    /// One quantum of removal.
    ///
    /// # Errors
    ///
    /// Returns an error when an entry cannot be listed or removed.
    pub fn step(&mut self, limit: WorkLimit) -> Result<Step<()>, StoreError> {
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
                                "capture.manifest",
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
/// A capture listed the directory of its `phase`: on to the next. A final
/// cut with a `held` queue lists it before the device's own.
const fn listed(phase: &mut u8, held: bool, held_listed: &mut bool) {
    if *phase == 3 && held && !*held_listed {
        *held_listed = true;
    } else {
        *phase = phase.saturating_add(1);
    }
}

fn owned_work_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .and_then(|s| {
            s.strip_prefix("replay-")
                .or_else(|| s.strip_prefix("retain-"))
                .or_else(|| s.strip_prefix(HELD_PREFIX))
        })
        .is_some_and(|s| is_hex(s, 32))
}

/// Where a store keeps work no restart needs; reclaimed on open.
pub(crate) const WORK_DIR: &str = "store-work-v1";
const HELD_PREFIX: &str = "held-";

/// The work directory name of a catch-up's held commits.
pub(crate) fn held_work_name(id: [u8; 16]) -> String {
    format!("{HELD_PREFIX}{}", hex::encode(id))
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
pub struct WorkReclaimer {
    entries: Option<DirectoryCursor>,
    current: Option<WorkDiscard>,
}
impl WorkspaceStore {
    /// Reclaim every abandoned work directory, before admitting new work.
    ///
    /// # Errors
    ///
    /// Returns an error when the work directory cannot be listed.
    pub fn start_work_reclamation(&self) -> Result<WorkReclaimer, StoreError> {
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
    /// One quantum of reclamation.
    ///
    /// # Errors
    ///
    /// Returns an error when an entry cannot be listed or removed.
    pub fn step(&mut self, limit: WorkLimit) -> Result<Step<()>, StoreError> {
        let limit = limit.checked()?;
        if let Some(current) = &mut self.current {
            if current.step(limit)? == Step::Done(()) {
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
pub struct PublicationCursor {
    workspace: PathBuf,
    input: File,
    recovery: bool,
}
impl PublicationCursor {
    /// The live publication of the selected `generation`.
    ///
    /// # Errors
    ///
    /// Returns an error when its manifest cannot be opened.
    pub fn open(generation: &Path) -> Result<Self, StoreError> {
        Self::open_manifest(generation, false)
    }
    /// The serialized workspace owner must retain its queue position through
    /// the cursor's completion; it excludes generation GC while paths are read.
    /// Delivered batches then own their bytes/permits independently of GC.
    ///
    /// # Errors
    ///
    /// Returns an error when its manifest cannot be opened.
    pub fn open_recovery(generation: &Path) -> Result<Self, StoreError> {
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
    /// The next frame of the publication, within `budget`.
    ///
    /// # Errors
    ///
    /// Returns an error when a frame cannot be read or exceeds `budget`.
    pub fn step(&mut self, budget: &ReplayBudget) -> Result<Step<Option<ReplayBatch>>, StoreError> {
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
