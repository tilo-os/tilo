//! Replays: what a device is sent of its workspace when it joins, and of
//! its own queue when it comes back.
//!
//! A replay sends the workspace's retained frames in one order: the
//! certificates' lineages, the governance's, the device's own queue, the
//! shared desktop's lineages, then every other document's
//! ([`lineage_rank`]); within a lineage its checkpoint, then its frames by
//! name. The store keeps that order as frames commit ([`ReplayOrder`]):
//! each write names the lineage it changed, and the next capture lists that
//! lineage again, and only it. A newcomer's capture takes the order as it
//! stands, in its owner's turn, and its first batch goes out after reading
//! the frames it carries: a join costs what it sends, not what the
//! workspace retained.
//!
//! These cursors confer no transport/session cleanup authority: the ordered
//! owner in persistence supplies it. No Drop implementation recursively
//! deletes files or touches pending sources.
use super::bounded::{
    self, DirectoryCursor, Meta, Sorter, WorkStats, increment, invalid, read_meta, write_meta,
};
pub(crate) use super::bounded::{Step, WorkLimit};
use super::{StoreError, WorkspaceStore, retained};
use crate::store::bounded::Invariant;
use crate::transport::{self, Envelope};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
pub(super) use tilo_server_store::frames::{equal_files, frame_bytes};

pub(crate) const DEFAULT_BATCH_FRAMES: usize = 64;
pub(crate) const DEFAULT_BATCH_BYTES: usize = 1024 * 1024;
pub(crate) const DEFAULT_OUTPUT_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const DEFAULT_REPLAY_TICKETS: usize = 32;
/// Names one replay's work directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayId(pub [u8; 16]);

// Lineages a capture listed from their generation rather than took from the
// kept order, on this thread: what a test reads to see the order kept.
#[cfg(test)]
thread_local! {
    pub(crate) static LINEAGES_LISTED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// A replay reads the retained frames its capture took straight from their
/// generations, while it is sent. Only collection removes a retained frame,
/// so collection waits while any capture of the store is leased: a lineage
/// collected then is remembered, and collected once no capture is leased
/// ([`WorkspaceStore::deferred_collections`]). A replay gives its lease up
/// once it has read its last retained frame.
///
/// Leases are taken in the owner's turn, and collection is decided there,
/// so no collection that a lease should stop can have begun before it.
#[derive(Default)]
pub(crate) struct CaptureLeases {
    held: AtomicUsize,
    deferred: std::sync::Mutex<BTreeSet<PathBuf>>,
    /// The store's owner, woken when the last lease ends so what waited is
    /// collected without waiting for its next job.
    wake: std::sync::Mutex<Option<Arc<tokio::sync::Notify>>>,
    /// The order a replay sends the retained lineages in.
    order: std::sync::Mutex<ReplayOrder>,
    /// How each retained channel's record syncs
    /// ([`super::document_sync`]).
    pub(super) records: super::document_sync::DocumentClasses,
    /// Which retained channels' records are sealed ([`super::sealed`]).
    pub(super) sealed: super::sealed::SealedChannels,
}
impl CaptureLeases {
    /// The leases of the store at `root`, shared by every handle on it.
    pub(crate) fn for_root(root: &Path) -> Arc<Self> {
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
    fn deferred(&self) -> std::sync::MutexGuard<'_, BTreeSet<PathBuf>> {
        self.deferred.lock_recover()
    }
}
/// One capture's hold on its store's collection, until its replay has read
/// its retained frames or is given up.
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

/// One retained lineage as a replay sends it: the generation `CURRENT`
/// selected when it was last listed, and the frames that generation held
/// then, in replay order. Frames are named by ordinal
/// (`{ordinal:016x}.delta`) as retention writes them, so they are kept as
/// spans of ordinals: a lineage costs a few words however long it is.
pub(crate) struct LineageFrames {
    rank: &'static str,
    /// The document its frames carry, as the lineage's name says.
    document: Option<[u8; 32]>,
    generation: PathBuf,
    checkpoint: bool,
    /// Ordinals held, as sorted inclusive spans.
    spans: Vec<(u64, u64)>,
    /// Frames not named by an ordinal, sorted.
    others: Vec<OsString>,
}

/// The ordinal of a frame retention named `{ordinal:016x}.delta`.
fn ordinal(name: &str) -> Option<u64> {
    let hex = name.strip_suffix(".delta")?;
    if hex.len() != 16 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    u64::from_str_radix(hex, 16).ok()
}

impl LineageFrames {
    /// What `generation`, lineage `name`'s current one, holds, and whether
    /// that is settled: no frame of it waits for its owner's group to put
    /// it in place ([`super::retained::group`]).
    fn list(
        rank: &'static str,
        name: &OsStr,
        generation: PathBuf,
    ) -> Result<(Self, bool), StoreError> {
        #[cfg(test)]
        LINEAGES_LISTED.with(|listed| listed.set(listed.get().saturating_add(1)));
        let mut checkpoint = false;
        let mut settled = true;
        let mut ordinals = Vec::new();
        let mut others = Vec::new();
        for entry in fs::read_dir(&generation)? {
            let entry = entry?;
            let file = entry.file_name();
            let lossy = file.to_string_lossy();
            if lossy.ends_with(".group") {
                settled = false;
            }
            if !lossy.ends_with(".delta") || lossy.starts_with('.') || !entry.file_type()?.is_file()
            {
                continue;
            }
            if file == "checkpoint.delta" {
                checkpoint = true;
            } else if let Some(ordinal) = file.to_str().and_then(ordinal) {
                ordinals.push(ordinal);
            } else {
                others.push(file);
            }
        }
        ordinals.sort_unstable();
        others.sort();
        let mut spans: Vec<(u64, u64)> = Vec::new();
        for ordinal in ordinals {
            match spans.last_mut() {
                Some((_, last)) if last.checked_add(1) == Some(ordinal) => *last = ordinal,
                _ => spans.push((ordinal, ordinal)),
            }
        }
        let frames = Self {
            rank,
            document: super::document_sync::lineage_channel(name),
            generation,
            checkpoint,
            spans,
            others,
        };
        Ok((frames, settled))
    }

    /// The next frame after `cursor`, by name.
    fn next(&self, cursor: &mut FrameCursor) -> Option<OsString> {
        if !std::mem::replace(&mut cursor.checkpoint_taken, true) && self.checkpoint {
            return Some("checkpoint.delta".into());
        }
        let numbered = self.spans.get(cursor.span).map(|(first, last)| {
            let ordinal = first.saturating_add(cursor.taken);
            (
                OsString::from(format!("{ordinal:016x}.delta")),
                ordinal == *last,
            )
        });
        let other = self.others.get(cursor.other);
        match (numbered, other) {
            (Some((numbered, _)), Some(other)) if *other < numbered => {
                cursor.other = cursor.other.saturating_add(1);
                Some(other.clone())
            }
            (Some((numbered, span_done)), _) => {
                if span_done {
                    cursor.span = cursor.span.saturating_add(1);
                    cursor.taken = 0;
                } else {
                    cursor.taken = cursor.taken.saturating_add(1);
                }
                Some(numbered)
            }
            (None, Some(other)) => {
                cursor.other = cursor.other.saturating_add(1);
                Some(other.clone())
            }
            (None, None) => None,
        }
    }
}

/// Where a replay is in one lineage's frames.
#[derive(Default)]
struct FrameCursor {
    checkpoint_taken: bool,
    span: usize,
    taken: u64,
    other: usize,
}

/// The order a replay sends the retained lineages in, kept as frames
/// commit rather than worked out for each newcomer: a workspace's captures
/// listed, pinned, sorted and compared every frame it retained, in every
/// join.
///
/// It is exact, not a cache to validate. Retention marks a lineage stale
/// before it writes it ([`WorkspaceStore::forget_lineage_listing`]),
/// whoever drives it, a replacement of a document's history marks its
/// lineages,
/// and a lineage that appears is marked when the store first reads it
/// ([`super::document_sync`]). The next capture lists what is stale again.
/// After every owner job but one that says it changed retained history
/// only through retention, the owner marks every lineage
/// ([`WorkspaceStore::forget_listed`]): the next capture lists them all.
/// What changes off the owner (a stage, a collection, a discard) is never
/// a lineage's current generation.
///
/// It lives in this process: the retained lineages are what lasts, and
/// the first capture after the store opens lists them once.
#[derive(Default)]
pub(crate) struct ReplayOrder {
    lineages: BTreeMap<(&'static str, OsString), Arc<LineageFrames>>,
    /// Each lineage's rank in `lineages`, by name.
    ranks: HashMap<OsString, &'static str>,
    /// Lineages to list again before the order is read.
    stale: BTreeSet<OsString>,
    /// Lineages this capture could not list as settled, or could not read at all: the next
    /// capture lists them again, this one never. Their in-place frames are listed now; what a
    /// commit has staged and not put in place reaches the newcomer through the queue it is sent
    /// ([`crate::store::replay`]'s pending cut).
    deferred: BTreeSet<OsString>,
    /// Whether `lineages` holds every lineage but the stale ones.
    complete: bool,
}

impl WorkspaceStore {
    /// Wake `owner` when the last capture lease ends with collections
    /// waiting for it.
    pub(crate) fn wake_on_capture_release(&self, owner: Arc<tokio::sync::Notify>) {
        *self.capture_leases.wake.lock_recover() = Some(owner);
    }

    fn replay_order(&self) -> std::sync::MutexGuard<'_, ReplayOrder> {
        self.capture_leases.order.lock_recover()
    }

    /// Mark the lineage directories `touched` stale in the replay order, or
    /// every lineage when `None` ([`ReplayOrder`]).
    pub(crate) fn forget_listed(&self, touched: Option<&[OsString]>) {
        let mut order = self.replay_order();
        match touched {
            None => order.complete = false,
            Some(touched) => {
                // Counted: lineages a write marked.
                crate::timing::window("capture.lineage_forgotten", crate::timing::start());
                order.stale.extend(touched.iter().cloned());
            }
        }
    }

    /// Mark every lineage on `channel` stale, of any author, and forget
    /// whether its document is sealed: its history is rewritten.
    pub(crate) fn forget_channel_listing(&self, channel: &[u8; 32]) {
        self.capture_leases.sealed.forget(channel);
        let suffix = format!("-{}", hex::encode(channel));
        let mut order = self.replay_order();
        let touched: Vec<OsString> = order
            .ranks
            .keys()
            .filter(|name| name.to_string_lossy().ends_with(&suffix))
            .cloned()
            .collect();
        order.stale.extend(touched);
    }

    /// Mark the lineage directory `lineage` stale, about to change:
    /// retention does so for every lineage it writes, whoever drives it.
    pub(crate) fn forget_lineage_listing(&self, lineage: &Path) {
        if let Some(name) = lineage.file_name() {
            self.forget_listed(Some(&[name.to_owned()]));
        }
    }

    /// The lineages the replay order holds, sorted; none while it must be
    /// listed whole.
    #[cfg(test)]
    pub(crate) fn listed_lineage_names(&self) -> Vec<OsString> {
        let order = self.replay_order();
        if !order.complete {
            return Vec::new();
        }
        let mut names: Vec<_> = order.ranks.keys().cloned().collect();
        drop(order);
        names.sort();
        names
    }

    /// One quantum of bringing the replay order up to date: list each stale
    /// lineage again, every lineage when the order is not complete.
    fn update_replay_order(&self, limit: WorkLimit) -> Result<Step<()>, StoreError> {
        let v2 = self.root.join("retained/v2");

        let more = {
            let mut order = self.replay_order();
            if !order.complete {
                let mut names = BTreeSet::new();
                if let Some(entries) = open_dir(&v2)? {
                    for entry in entries {
                        let entry = entry?;
                        if entry.file_type()?.is_dir() {
                            names.insert(entry.file_name());
                        }
                    }
                }
                order.lineages.clear();
                order.ranks.clear();
                order.stale = names;
                order.deferred.clear();
                order.complete = true;
            }
            for _ in 0..limit.entry_bound() {
                let Some(name) = order.stale.pop_first() else {
                    break;
                };
                if let Some(rank) = order.ranks.remove(&name) {
                    order.lineages.remove(&(rank, name.clone()));
                }
                let listed = self.list_lineage(&v2, &name);
                match listed {
                    Ok(Some((frames, settled))) => {
                        if !settled {
                            // A frame its owner's group has not put in place yet. Only the owner
                            // settles it, and the owner is here until this capture ends: listing
                            // it again in this capture would never finish, and every newcomer's
                            // replay would wait behind it. Its frames in place are listed now,
                            // what is staged arrives through the queue the newcomer is sent, and
                            // the next capture lists it again.
                            order.deferred.insert(name.clone());
                        }
                        order.ranks.insert(name.clone(), frames.rank);
                        order.lineages.insert((frames.rank, name), Arc::new(frames));
                    }
                    Ok(None) => {
                        // A first checkpoint still staged, or a lineage gone: read again by the
                        // next capture.
                        order.deferred.insert(name);
                    }
                    Err(error) => {
                        order.stale.insert(name);
                        return Err(error);
                    }
                }
            }
            let more = !order.stale.is_empty();
            if !more {
                // What this capture could not settle or read waits for the next one.
                order.stale = std::mem::take(&mut order.deferred);
            }
            more
        };
        Ok(if more { Step::More } else { Step::Done(()) })
    }

    /// Lineage `name` of `v2` as its current generation holds it, and
    /// whether that is settled: no frame of it waits to be put in place.
    /// `None` for a lineage whose first checkpoint is staged but not
    /// committed yet, or that is gone.
    fn list_lineage(
        &self,
        v2: &Path,
        name: &OsStr,
    ) -> Result<Option<(LineageFrames, bool)>, StoreError> {
        let lineage = v2.join(name);
        if !lineage.is_dir() {
            return Ok(None);
        }
        let Some(generation) = retained::current(&lineage)? else {
            return Ok(None);
        };
        LineageFrames::list(self.capture_rank(name), name, generation).map(Some)
    }

    /// The lineages a replay sends `device`, in order: every lineage of the
    /// order but those a capture leaves out ([`ReplayBuild`]).
    fn replayed_lineages(
        &self,
        every_document: bool,
        channels: Option<&BTreeSet<[u8; 32]>>,
    ) -> Vec<Arc<LineageFrames>> {
        let order = self.replay_order();
        let classes = self.classes();
        order
            .lineages
            .values()
            .filter(|lineage| {
                let Some(document) = lineage.document else {
                    return true;
                };
                // A workspace keeps far more lineages than a newcomer is
                // replayed (each author's part of every document, on-demand
                // ones included). A confined device is replayed the
                // lineages of its own document; another document's
                // is none of its.
                (every_document || !classes.leaves_out(&document))
                    && channels.is_none_or(|channels| channels.contains(&document))
            })
            .cloned()
            .collect()
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
    /// Pending entries the replay's cleanup unlinks once it is written: none
    /// needs the owner's cleanup turn ([`WorkspaceStore::cleanup_replay_step`]).
    pub cleanup_entries: u64,
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
impl ReplayAllocation {
    /// Hold `other`'s bytes as this allocation's.
    fn absorb(&mut self, mut other: Self) {
        self.bytes = self.bytes.saturating_add(std::mem::take(&mut other.bytes));
    }
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
        Envelope::PeerServerHello { hosted_workspaces } => {
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
/// Sort rank of a retained lineage (`<author>-<channel>`) by its name: a
/// device's certificate channel replays first, because a shell drops what a
/// device published until it holds that device's certificate; the workspace's
/// shared desktop record after the governance, because it is what a joining
/// page shows first and its desktop waits for it rather than for the whole
/// replay; and channels are otherwise ordered by hash. A capture also ranks
/// every lineage that carries no record (the workspace's own document, its
/// channels' governance) right after the certificates
/// ([`WorkspaceStore::capture_rank`]): a record's frames wait for the
/// governance that admits their author and names the group whose keys open
/// them, and a page asks to join a group only once it has heard of it. A
/// device's own queue replays between the governance and the desktop.
pub(crate) fn lineage_rank(lineage: &str) -> &'static str {
    let Some((author, channel)) = lineage.split_once('-') else {
        return "3";
    };
    if let Ok(author) = tilo_failure::ignored!(
        "only a sort rank: a lineage not named for an author ranks last",
        hex::decode(author)
    ) {
        let mut route = b"identity-hello\0".to_vec();
        route.extend_from_slice(&author);
        if blake3::hash(&route).to_hex().as_str() == channel {
            return "0";
        }
    }
    if channel == desktop_channel() {
        "2"
    } else {
        "3"
    }
}

/// The rank a device's own queue replays at: after the governance, before
/// the shared desktop.
const QUEUE_RANK: &str = "2";

/// The retained channel of the workspace's shared desktop record, as hex.
fn desktop_channel() -> &'static str {
    static CHANNEL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CHANNEL.get_or_init(|| {
        use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
        hex::encode(neighbour_wire::retention::app_document_channel(
            DESKTOP_APP,
            DESKTOP_DOCUMENT,
        ))
    })
}

impl WorkspaceStore {
    /// The sort rank a capture gives the retained lineage `name`: its
    /// [`lineage_rank`], and the governance's place right after the
    /// certificates for a lineage that carries no record.
    fn capture_rank(&self, name: &OsStr) -> &'static str {
        match lineage_rank(&name.to_string_lossy()) {
            "3" if !self.carries_document(name) => "1",
            rank => rank,
        }
    }
}

fn open_dir(path: &Path) -> Result<Option<fs::ReadDir>, StoreError> {
    match fs::read_dir(path) {
        Ok(dir) => Ok(Some(dir)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// A capture in progress: the replay order brought up to date and taken,
/// then the device's queue (or a final cut's held commits) listed.
pub struct ReplayBuild {
    root: PathBuf,
    workspace: PathBuf,
    device: [u8; 32],
    /// What a catch-up held while it ran, which its final cut takes before
    /// the device's own queue.
    held: Option<PathBuf>,
    /// The held queue was listed; the device's own comes next.
    held_listed: bool,
    phase: u8,
    outer: Option<fs::ReadDir>,
    sorter: Option<Sorter>,
    next_pin: u64,
    skip: Option<ResumeSkip>,
    /// Replay every record, on-demand ones and earlier parts of chains
    /// included: a host attached to the workspace keeps all of it.
    every_document: bool,
    /// Replay only these record channels' lineages: a browser environment's
    /// device reaches its own record and nothing else of the workspace.
    channels: Option<BTreeSet<[u8; 32]>>,
    /// The retained lineages taken, with the lease that keeps their frames
    /// in place until the replay has read them ([`CaptureLeases`]).
    lineages: Vec<Arc<LineageFrames>>,
    lease: Option<CaptureLease>,
    /// A final cut's held commits, listed to pin when the capture is
    /// prepared (`capture.manifest`): nothing changes them once the cut is
    /// taken. The device's own queue is pinned at once. Buffered: a capture
    /// lists an entry per commit in the owner's turn.
    listed: Option<io::BufWriter<File>>,
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
    /// Replay on-demand records and earlier parts of chains too.
    pub(crate) fn with_every_document(mut self, every_document: bool) -> Self {
        self.every_document = every_document;
        self
    }
    /// Replay only the lineages of these record channels: everything else of
    /// the workspace stays out of this device's replay.
    pub(crate) fn with_channels(mut self, channels: Option<BTreeSet<[u8; 32]>>) -> Self {
        self.channels = channels;
        self
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
/// A final cut's held commits, listed and pinned when its replay is
/// prepared.
struct Link {
    /// `capture.manifest`: one entry per listed commit, its pin assigned.
    listed: File,
}
/// Where a replay is: the retained lineages before the device's queue, the
/// queue, the lineages after it, then done.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Early,
    Queue,
    Late,
    Done,
}
/// A frame read for a batch that did not fit it: the next batch's first.
struct Carried {
    envelope: Envelope,
    len: usize,
    charge: usize,
}
/// A captured replay: the retained lineages it sends and the device's
/// pinned queue, read batch by batch.
pub struct ReplaySnapshot {
    /// A final cut's held commits, not pinned yet.
    link: Option<Link>,
    root: PathBuf,
    workspace: PathBuf,
    workspace_id: WorkspaceId,
    pending_root: PathBuf,
    /// A final cut's held queue, which it cleans up like the device's own.
    held_root: Option<PathBuf>,
    sorter: Option<Sorter>,
    ordered: Option<File>,
    lineages: Vec<Arc<LineageFrames>>,
    next_lineage: usize,
    /// The lineage being read, where in it, and the frames of it read so
    /// far, by digest: a frame a generation holds twice is sent once.
    current: Option<(Arc<LineageFrames>, FrameCursor, HashSet<[u8; 32]>)>,
    /// Retained frames before the queue that prove a queued twin is
    /// covered, by digest.
    covered: HashSet<[u8; 32]>,
    stage: Stage,
    lease: Option<CaptureLease>,
    carried: Option<Carried>,
    cleanup: File,
    ready: bool,
    count: u64,
    pending_count: u64,
    /// Entries in `cleanup.manifest`.
    cleanup_count: u64,
    cleanup_offset: u64,
    batch_number: u64,
    stats: WorkStats,
    skip: Option<ResumeSkip>,
}
/// Where the next frame of a replay comes from.
enum Source {
    Retained(PathBuf),
    Queued(Meta),
}
impl ReplaySnapshot {
    pub(crate) fn stats(&self) -> &WorkStats {
        &self.stats
    }
    /// What the replay sent, once it has sent everything.
    pub(crate) fn summary(&self) -> Option<ReplaySummary> {
        (self.stage == Stage::Done && self.carried.is_none()).then_some(ReplaySummary {
            total_frames: self.count,
            pending_frames: self.pending_count,
            cleanup_entries: self.cleanup_count,
        })
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
    /// Where the next frame comes from, in replay order.
    fn next_source(&mut self) -> Result<Option<Source>, StoreError> {
        loop {
            match self.stage {
                Stage::Early | Stage::Late => {
                    if let Some((lineage, cursor, _)) = &mut self.current {
                        if let Some(name) = lineage.next(cursor) {
                            return Ok(Some(Source::Retained(lineage.generation.join(name))));
                        }
                        self.current = None;
                    }
                    match self.lineages.get(self.next_lineage) {
                        Some(lineage)
                            if self.stage == Stage::Early && lineage.rank >= QUEUE_RANK =>
                        {
                            self.stage = Stage::Queue;
                        }
                        Some(lineage) => {
                            self.current =
                                Some((lineage.clone(), FrameCursor::default(), HashSet::new()));
                            self.next_lineage = self.next_lineage.saturating_add(1);
                        }
                        None if self.stage == Stage::Early => self.stage = Stage::Queue,
                        None => {
                            // Every retained frame is read: collection may go on.
                            self.stage = Stage::Done;
                            self.lease = None;
                            self.lineages = Vec::new();
                            self.covered = HashSet::new();
                        }
                    }
                }
                Stage::Queue => {
                    if let Some(meta) = self.ordered.as_mut().map(read_meta).transpose()?.flatten()
                    {
                        return Ok(Some(Source::Queued(meta)));
                    }
                    self.stage = Stage::Late;
                }
                Stage::Done => return Ok(None),
            }
        }
    }
    /// The next frame this replay sends, decoded, with its length and the
    /// output it is charged; frames it leaves out are passed over.
    fn next_frame(&mut self) -> Result<Option<Carried>, StoreError> {
        if let Some(carried) = self.carried.take() {
            return Ok(Some(carried));
        }
        while let Some(source) = self.next_source()? {
            if let Some(frame) = self.read_frame(&source)? {
                return Ok(Some(frame));
            }
        }
        Ok(None)
    }
    /// Read the frame at `source`: `None` when the replay leaves it out (a
    /// twin, a frame the resuming device holds, one that does not decode).
    fn read_frame(&mut self, source: &Source) -> Result<Option<Carried>, StoreError> {
        let (path, queued) = match source {
            Source::Retained(path) => (path.clone(), None),
            Source::Queued(meta) => (self.pin(meta.pin), Some(meta)),
        };
        let Some(bytes) = frame_bytes(&path)? else {
            return Ok(None);
        };
        increment(&mut self.stats.hydration_reads)?;
        self.stats.peak_frame_bytes = self.stats.peak_frame_bytes.max(bytes.capacity());
        let envelope = match transport::decode(frame_payload(&bytes)) {
            Ok(envelope) => envelope,
            Err(error) => {
                // A retained frame that does not decode is not replayed; it
                // is reported, never left out unseen.
                if queued.is_none() {
                    tracing::warn!(
                        workspace_id = %self.workspace_id,
                        path = %path.display(),
                        %error,
                        "a retained frame does not decode; it is left out of the replay"
                    );
                }
                return Ok(None);
            }
        };
        let decoded = heap_bytes(&envelope).saturating_add(std::mem::size_of::<Envelope>());
        self.stats.peak_frame_bytes = self
            .stats
            .peak_frame_bytes
            .max(bytes.capacity().saturating_add(decoded));
        // Canonical complete frames alone can prove retained duplicate coverage.
        let complete = matches!(
            &envelope,
            Envelope::Delta { workspace_id, delta, .. }
                if *workspace_id == self.workspace_id
                    && super::retained::complete_coedit_payload(delta).is_some()
        );
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
        // `complete` independently requires complete Coedit consumption.
        let coverage = queued.is_none() && complete && encoded == bytes;
        let held = queued.is_none()
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
        let digest = *blake3::hash(&bytes).as_bytes();
        match queued {
            None => {
                let (_, _, read) = self.current.as_mut().invariant("lineage missing")?;
                if !read.insert(digest) {
                    return Ok(None);
                }
                if coverage && self.stage == Stage::Early {
                    self.covered.insert(digest);
                }
                if held {
                    return Ok(None);
                }
            }
            Some(meta) => {
                self.clean_up(meta)?;
                if self.covered.contains(&digest) {
                    return Ok(None);
                }
                increment(&mut self.pending_count)?;
            }
        }
        increment(&mut self.count)?;
        Ok(Some(Carried {
            envelope,
            len: bytes.len(),
            charge: usize::try_from(charge).map_err(|_| invalid("frame charge overflow"))?,
        }))
    }
    /// Have the replay's cleanup unlink the pending entry `meta` once the
    /// replay is written. A final cut's held commits are not unlinked one by
    /// one: they are its session's alone, no later cut reads them, and they
    /// go with the catch-up's held directory once it is written.
    fn clean_up(&mut self, meta: &Meta) -> Result<(), StoreError> {
        if self.held_root.is_some() && meta.source.parent() == self.held_root.as_deref() {
            return Ok(());
        }
        write_meta(&mut self.cleanup, meta)?;
        increment(&mut self.cleanup_count)
    }
}
impl WorkspaceStore {
    /// Begin capturing a replay for `device`.
    ///
    /// # Errors
    ///
    /// Returns an error when the capture's work directory cannot be made.
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
    /// Move the work directory of replay `id`, written and cleaned up, aside
    /// under a fresh name, so its catch-up's final cut can take the name over
    /// at once; the returned discard removes it wherever it is stepped.
    /// Discarded in place, in the owner's turn, it unlinked every pin of the
    /// first replay in front of every queued job.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be renamed or listed.
    pub(crate) fn retire_replay_work(&self, id: ReplayId) -> Result<WorkDiscard, StoreError> {
        let work = self.root.join(WORK_DIR);
        let current = work.join(format!("replay-{}", hex::encode(id.0)));
        let retired = work.join(format!(
            "replay-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        match fs::rename(&current, &retired) {
            Ok(()) => WorkDiscard::new(retired),
            Err(error) if error.kind() == io::ErrorKind::NotFound => WorkDiscard::new(current),
            Err(error) => Err(error.into()),
        }
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
        let sorter = Sorter::new(root.join("sort"))?;
        Ok(ReplayBuild {
            root: root.clone(),
            workspace: self.root.clone(),
            device,
            listed: if pending_only {
                Some(io::BufWriter::new(File::create(
                    root.join("capture.manifest"),
                )?))
            } else {
                None
            },
            held,
            held_listed: false,
            phase: if pending_only { 3 } else { 2 },
            outer: None,
            sorter: Some(sorter),
            next_pin: 0,
            skip: None,
            every_document: false,
            channels: None,
            lineages: Vec::new(),
            lease: (!pending_only).then(|| self.lease_capture()),
        })
    }
    /// Invoke under one owner operation until Done; later mutations must not
    /// interleave before the replay order is taken and every queued entry
    /// has its immutable hardlink, or, for a final cut's held commit, is
    /// listed.
    ///
    /// # Errors
    ///
    /// Returns an error when retained history or the queue cannot be read
    /// or pinned.
    pub fn capture_replay_step(
        &self,
        build: &mut ReplayBuild,
        limit: WorkLimit,
    ) -> Result<Step<ReplaySnapshot>, StoreError> {
        let limit = limit.checked()?;
        if build.workspace != self.root {
            return Err(invalid("replay store mismatch"));
        }
        if build.phase > 3 {
            return Err(invalid("capture already consumed"));
        }
        if build.phase == 2 {
            self.classify_lineages()?;
            let (stale, deferred) = {
                let order = self.replay_order();
                (order.stale.len(), order.deferred.len())
            };
            let more = self.update_replay_order(limit)? == Step::More;
            tracing::debug!(device = %hex::encode(&build.device[..2]), stale, deferred, more, "CAPTURE order");
            if more {
                return Ok(Step::More);
            }
            build.lineages = self.replayed_lineages(build.every_document, build.channels.as_ref());
            if build.lineages.is_empty() {
                build.lease = None;
            }
            build.phase = 3;
            return Ok(Step::More);
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
        for _ in 0..limit.entry_bound() {
            sorter.stats.last_step_work = sorter.stats.last_step_work.saturating_add(1);
            if build.outer.is_none() {
                let held_queue = build.held.as_ref().filter(|_| !build.held_listed);
                let dir =
                    held_queue.map_or_else(|| self.pending_dir_for(&build.device), Clone::clone);
                build.outer = open_dir(&dir)?;
                if build.outer.is_none() {
                    if held_queue.is_some() {
                        build.held_listed = true;
                        continue;
                    }
                    return self.captured(build).map(Step::Done);
                }
            }
            let Some(entry) = build.outer.as_mut().invariant("outer missing")?.next() else {
                build.outer = None;
                if build.held.is_some() && !build.held_listed {
                    build.held_listed = true;
                    continue;
                }
                return self.captured(build).map(Step::Done);
            };
            let entry = entry?;
            let name = entry.file_name();
            if !entry.file_type()?.is_file() || !name.to_string_lossy().ends_with(".delta") {
                continue;
            }
            let held = build.held.is_some() && !build.held_listed;
            // Counted: what a final cut takes from the commits its catch-up
            // held, and from the device's queue.
            crate::timing::window(
                if held {
                    "capture.held_entry"
                } else {
                    "capture.queued_entry"
                },
                crate::timing::start(),
            );
            let pin = build.next_pin;
            increment(&mut build.next_pin)?;
            let source = entry.path();
            let meta = Meta {
                key: vec![name],
                source: source
                    .strip_prefix(&self.root)
                    .map_err(|_| invalid("invalid capture path"))?
                    .into(),
                pin,
                len: 0,
                pending: true,
            };
            // A final cut's held commits are pinned when the capture is
            // prepared, off the owner.
            if held && let Some(listed) = build.listed.as_mut() {
                write_meta(listed, &meta)?;
                continue;
            }
            let target = build.root.join("pins").join(format!("{pin:016x}.frame"));
            fs::hard_link(&source, &target)?;
            let len = fs::metadata(&target)?.len();
            sorter.push(Meta { len, ..meta })?;
            sorter.stats.peak_open_files = sorter.stats.peak_open_files.max(3);
            if sorter.spilling() {
                return Ok(Step::More);
            }
        }
        Ok(Step::More)
    }
    /// The snapshot a finished capture hands over.
    fn captured(&self, build: &mut ReplayBuild) -> Result<ReplaySnapshot, StoreError> {
        build.phase = 4;
        let link = match build.listed.take() {
            Some(listed) => {
                // Written out before it is read back.
                drop(
                    listed
                        .into_inner()
                        .map_err(io::IntoInnerError::into_error)?,
                );
                Some(Link {
                    listed: File::open(build.root.join("capture.manifest"))?,
                })
            }
            None => None,
        };
        let cleanup = File::options()
            .create_new(true)
            .append(true)
            .open(build.root.join("cleanup.manifest"))?;
        Ok(ReplaySnapshot {
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
            lineages: std::mem::take(&mut build.lineages),
            next_lineage: 0,
            current: None,
            covered: HashSet::new(),
            stage: Stage::Early,
            lease: build.lease.take(),
            carried: None,
            cleanup,
            ready: false,
            count: 0,
            pending_count: 0,
            cleanup_count: 0,
            cleanup_offset: 0,
            batch_number: 0,
            stats: WorkStats::default(),
            skip: build.skip.take(),
        })
    }
    /// One quantum of pinning a final cut's held commits, as the capture
    /// pinned the device's queue: a hard link and its place in the order.
    fn pin_listed_step(
        &self,
        snapshot: &mut ReplaySnapshot,
        limit: WorkLimit,
    ) -> Result<Step<()>, StoreError> {
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
        for _ in 0..limit.entry_bound() {
            sorter.stats.last_step_work = sorter.stats.last_step_work.saturating_add(1);
            let Some(meta) = read_meta(&mut link.listed)? else {
                snapshot.link = None;
                break;
            };
            let source = self.root.join(&meta.source);
            let target = snapshot
                .root
                .join("pins")
                .join(format!("{:016x}.frame", meta.pin));
            fs::hard_link(&source, &target)?;
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
    /// One quantum of ordering a captured snapshot's queue. The retained
    /// frames need none: they are read in the order the store keeps.
    ///
    /// # Errors
    ///
    /// Returns an error when the queue cannot be pinned or sorted.
    pub fn prepare_replay_step(
        &self,
        snapshot: &mut ReplaySnapshot,
        limit: WorkLimit,
    ) -> Result<Step<()>, StoreError> {
        #[cfg(test)]
        super::test_barrier::pause(&self.root, "replay-manifest-prepare");
        let limit = limit.checked()?;
        if snapshot.workspace != self.root {
            return Err(invalid("replay store mismatch"));
        }
        if snapshot.ready {
            return Ok(Step::Done(()));
        }
        snapshot.stats.last_step_work = 0;
        if snapshot.link.is_some() {
            return self.pin_listed_step(snapshot, limit);
        }
        let sorter = snapshot.sorter.as_mut().invariant("queue order missing")?;
        let result = sorter.step(limit)?;
        snapshot.stats = sorter.stats.clone();
        if let Step::Done(path) = result {
            snapshot.ordered = Some(File::open(path)?);
            snapshot.sorter = None;
            snapshot.ready = true;
            return Ok(Step::Done(()));
        }
        Ok(Step::More)
    }
    /// The next batch of a prepared snapshot's frames.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot is not ready for this store, a
    /// frame cannot be read, or the budget cannot hold the batch's first
    /// frame (which the next call starts with).
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
        let mut allocation = budget.acquire(0)?;
        let mut frames = VecDeque::new();
        let mut encoded = 0usize;
        let mut charge = 0usize;
        let mut decoded_live = 0usize;
        let mut eof = false;
        // A frame is read only for the batch it goes in, or the next one's
        // first when it does not fit.
        while frames.len() < limits.max_frames {
            let Some(frame) = snapshot.next_frame()? else {
                eof = true;
                break;
            };
            if !frames.is_empty()
                && encoded
                    .checked_add(frame.len)
                    .is_none_or(|n| n > limits.max_bytes)
            {
                snapshot.carried = Some(frame);
                break;
            }
            let held = match budget.acquire(frame.charge) {
                Ok(held) => held,
                Err(error) => {
                    let first = frames.is_empty();
                    snapshot.carried = Some(frame);
                    if first {
                        return Err(error);
                    }
                    break;
                }
            };
            allocation.absorb(held);
            charge = charge
                .checked_add(frame.charge)
                .ok_or_else(|| invalid("allocation overflow"))?;
            encoded = encoded
                .checked_add(frame.len)
                .ok_or_else(|| invalid("batch length overflow"))?;
            decoded_live = decoded_live
                .saturating_add(heap_bytes(&frame.envelope))
                .saturating_add(std::mem::size_of::<Envelope>());
            snapshot.stats.peak_frame_bytes = snapshot.stats.peak_frame_bytes.max(decoded_live);
            frames.push_back(frame.envelope);
        }
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
        if snapshot.workspace != self.root || snapshot.summary().is_none() {
            return Err(invalid("replay cleanup before the replay was read"));
        }
        let mut input = File::open(snapshot.root.join("cleanup.manifest"))?;
        input.seek(SeekFrom::Start(snapshot.cleanup_offset))?;
        for _ in 0..limit.entry_bound() {
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
            if same_inode(&source, &pin)? && equal_files(&source, &pin)? {
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
        for _ in 0..limit.entry_bound() {
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
                                "shell-sort",
                                "extras-sort",
                                "extras-sort-fallback",
                                "pins",
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
                                "workspace.manifest",
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
        name.strip_suffix(".frame").is_some_and(|s| is_hex(s, 16))
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
        for _ in 0..limit.entry_bound() {
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
