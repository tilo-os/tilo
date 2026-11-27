//! The frames a store device holds for its workspaces.
//!
//! A workspace's frames are appended to its segments,
//! `workspaces/<workspace>/<segment>.pack`, each record framed with its
//! length and a check of its bytes. A write is acknowledged once the segment
//! holding it is synced, and the writes that wait at the same time share one
//! sync. A store that restarts reads back exactly what it acknowledged: a
//! record cut short by a crash ends its segment and is cut off there. The
//! index the store answers replay from is rebuilt at [`Store::open`] from the
//! records' headers, without decoding a frame; the segments are the truth.
//!
//! A retired frame is named by a record of its own after it, and a segment
//! whose bytes are mostly retired is written again with only what it still
//! holds. A store whose frames another build kept in another layout
//! ([`FRAMES_VERSION`]) is refused as it opens, by its version; one of version 7 on reads as
//! this build's and is relabelled, and one whose claims name no call is written again
//! ([`WRITTEN_AGAIN`]), the frames it kept one file each before segments among them.
//!
//! Beside its frames, a store keeps each workspace's governance: for each
//! route a channel's group frames take ([`neighbour_wire::retention`]), the
//! newest frame, and each change to the workspace's own state as a frame of
//! its own, one file each under `channels/<workspace>/`
//! ([`Store::keep_channel`], [`Store::keep_workspace_change`]). A device
//! that links later joins each channel's group from them and is replayed the
//! workspace's changes before the records they govern. A device's own state
//! beside them is kept whole under `state/` ([`Store::keep_state`]).
//!
//! The files are its backend's ([`Backend`]), which moves their bytes and
//! nothing else: a directory on the device's disk ([`FsBackend`]), or on a
//! page the database it keeps them in.
//!
//! Retention has three sources and no others: a checkpoint retiring the
//! frames it supersedes ([`Store::retain`]), the appetite the store was
//! opened with ([`Appetite`]), and the compaction an operator asks for
//! ([`Store::compact`]). A checkpoint supersedes its author's writes up to its
//! own sequence number and, when it is a sealed compaction, the other
//! authors' writes of the record it names in the clear. A sealed compaction is
//! a cache of those writes, never their replacement: they are retired only once
//! every device with the record's keys checked it against them
//! ([`Store::unchecked`], [`Store::retire_checked`]). Nothing else is retired
//! because it looks old, and compaction never retires a frame a device that
//! told the store what it holds ([`Store::holds`]) still lacks, nor a
//! lineage's newest checkpoint, which a device that holds none of it starts
//! from.

use crate::join::WorkspaceId;
pub use crate::retained::{Frame, Lineage, Refusal};
use collab_types::ContentRetention;
use neighbour_wire::NeighbourMsg;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tilo_lock::MutexRecover as _;

mod backend;
#[cfg(not(target_arch = "wasm32"))]
pub use backend::FsBackend;
pub use backend::{Appender, Backend, Durable, MemoryBackend, ReadAt};

/// How much history a store device keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appetite {
    /// Every frame it is given, until its authors retire their own.
    StoreAll,
    /// At most `max_bytes` of frames. The oldest frames are retired first;
    /// every lineage keeps its newest checkpoint, so a member never loses
    /// the state baseline a later frame builds on.
    Bounded {
        /// The most frame bytes the store keeps.
        max_bytes: u64,
    },
}

// The vocabulary a replay speaks — where a device stands, what one round may
// carry, what it answers — is the link's, which a page that keeps no store
// speaks too.
pub use crate::frames::{Budget, Cursor, Floor, Replay, advance, author_of, cursors, sealed};

/// What retaining a frame did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retained {
    /// The store now holds it.
    New,
    /// It held it already, byte for byte.
    Duplicate,
    /// Its author's newer checkpoint on its lineage covers it: the store
    /// holds that one, which retired it, and keeps nothing of this one.
    Covered,
    /// The store held another frame at its author's sequence number on its
    /// lineage, with other bytes: it now holds both, and serves both.
    Beside,
}

/// Errors from a store's files.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The store's directory could not be read or written.
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// A stored frame could not be encoded or decoded.
    #[error("frame: {0}")]
    Postcard(#[from] postcard::Error),
    /// A stored frame is not one a store keeps.
    #[error("frame: {0}")]
    Refused(#[from] Refusal),
    /// The store's frames were kept by another build, in a layout this one does not read: one
    /// after [`FRAMES_VERSION`].
    #[error(
        "the store's frames are version {0}; this build reads versions 1 to \
         {FRAMES_VERSION}"
    )]
    Version(u32),
    /// A stored frame claims the record's storage migration as a history move: bytes of a
    /// layout before the step's fields were written whole, which no build claims and no reader
    /// decides ([`Frame::of`] refuses such a write, and a frame kept before this build reads it
    /// as one).
    #[error(
        "the frame of {app}/{record} by {author} at {seq} names a storage migration as a \
         history move: its bytes are of a layout before the step wrote its fields whole"
    )]
    MigrationAsHistory {
        app: String,
        record: String,
        author: String,
        seq: u64,
    },
}

/// The frames retired from a store: what a compacting pass removed and what
/// bytes it freed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retired {
    /// Frames removed.
    pub frames: u64,
    /// Bytes they held.
    pub bytes: u64,
}

/// A sealed compaction a store keeps the writes of until every device with its record's keys
/// checked it against them ([`Store::unchecked`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UncheckedCheckpoint {
    /// The app whose record it is.
    pub app: String,
    /// The record.
    pub record: String,
    /// Who signed it.
    pub author: [u8; 32],
    /// Its sequence number.
    pub seq: u64,
}

/// A sealed compaction whose writes the store still keeps, with what it names in the clear.
#[derive(Clone)]
struct Unchecked {
    checkpoint: UncheckedCheckpoint,
    /// Its own author's line and record check: it is never one of the writes it retires.
    lineage: Lineage,
    check: [u8; 8],
    /// Each author with the last write it stands in for.
    covers: Vec<([u8; 32], u64)>,
}

/// A device's retained history, one directory of segments per workspace.
pub struct Store {
    disk: Disk,
    appetite: Appetite,
    held: std::sync::Mutex<Held>,
    /// This device's own writes of each workspace no other device keeping it has taken in yet,
    /// and those refused ([`crate::reach`]).
    reach: crate::reach::Ledgers,
    /// How far this device reserved the sequence numbers of its own lines, by workspace and line
    /// ([`Store::reserve_sequence`]): the numbers it hands out next, and the end of the block it
    /// has made durable.
    sequences: std::sync::Mutex<BTreeMap<(WorkspaceId, Lineage), Reserved>>,
}

/// The numbers of one line this store hands out: those it reserved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Reserved {
    /// Whether the line's reservation was read back from disk (or was made) here: a line this
    /// store has handed no number of yet starts past what it holds and what it is told.
    loaded: bool,
    /// The next number the line takes.
    next: u64,
    /// The end of the block whose numbers are on disk: a device that stops starts past it.
    end: u64,
}

/// How many sequence numbers of a line this device reserves at once: its block's end is written
/// before any number inside it is handed out, so a write costs one file write a block and a
/// device that stops mid-block skips what it never used when it starts again.
const SEQUENCE_BLOCK: u64 = 32;

/// Where a store's files are: a directory of its backend.
pub(crate) struct Disk {
    backend: Arc<dyn Backend>,
    /// The directory, relative to the backend's root.
    pub(crate) root: PathBuf,
}

impl std::ops::Deref for Disk {
    type Target = dyn Backend;

    fn deref(&self) -> &Self::Target {
        &*self.backend
    }
}

impl Disk {
    /// The directories inside `dir`, sorted, or none when it does not exist.
    fn directories(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        self.entries(dir, true)
    }

    /// The files inside `dir`, sorted, or none when it does not exist.
    fn files(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        self.entries(dir, false)
    }

    fn entries(&self, dir: &Path, directories: bool) -> io::Result<Vec<PathBuf>> {
        let mut found: Vec<PathBuf> = self
            .backend
            .list(dir)?
            .into_iter()
            .filter(|(_, is_dir)| *is_dir == directories)
            .map(|(path, _)| path)
            .collect();
        found.sort();
        Ok(found)
    }
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = self.held.lock_recover();
        f.debug_struct("Store")
            .field("backend", &self.disk.backend)
            .field("root", &self.disk.root)
            .field("appetite", &self.appetite)
            .field("bytes", &held.bytes)
            .field("workspaces", &held.workspaces.len())
            .finish_non_exhaustive()
    }
}

struct Held {
    bytes: u64,
    workspaces: BTreeMap<WorkspaceId, Workspace>,
    /// The segments open for appending, one at most per workspace.
    writers: usize,
    /// Counts appends, so the writer appended to longest ago is the one
    /// closed when too many are open.
    appends: u64,
    limits: Limits,
    /// The order the newest channel frame was kept in ([`Store::keep_channel`]).
    channel_order: u64,
}

/// How long segments grow and when one is written again.
#[derive(Clone, Copy)]
struct Limits {
    /// A segment this long takes no more appends; the next one starts.
    segment_bytes: u64,
    /// A segment shorter than this is not written again for what it frees.
    rewrite_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            segment_bytes: SEGMENT_BYTES,
            rewrite_bytes: REWRITE_BYTES,
        }
    }
}

#[derive(Default)]
struct Workspace {
    bytes: u64,
    lineages: BTreeMap<Lineage, Vec<Entry>>,
    /// What each device that asked for the workspace's history said it
    /// holds, by lineage ([`Store::holds`]).
    holders: BTreeMap<[u8; 32], BTreeMap<Lineage, u64>>,
    /// Every segment of the workspace, by number.
    segments: BTreeMap<u32, Segment>,
    /// The newest segment, open for appending.
    writer: Option<Arc<Writer>>,
    /// [`Held::appends`] at this workspace's last append.
    appended: u64,
    /// Frames appended and waiting for their sync, by the segment they are
    /// in, so a second copy that arrives meanwhile is a duplicate and the
    /// segment is not written again without them.
    staged: BTreeMap<(Lineage, u64, [u8; 8]), u32>,
    /// Records of retired frames still in their segments, by segment and
    /// offset: the retirement records naming them are kept until they go.
    retired: BTreeSet<(u32, u64)>,
    /// The newest channel governance frame of each route.
    channels: BTreeMap<[u8; 32], Channel>,
    /// The sealed compactions whose writes are kept until every device with their record's keys
    /// checked them.
    unchecked: Vec<Unchecked>,
}

/// A channel governance frame a store keeps, as its file holds it.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Channel {
    /// When it was kept, among the workspace's channel frames: they are
    /// served in that order, a group's genesis before what builds on it.
    order: u64,
    author: [u8; 32],
    /// The frame, as postcard [`NeighbourMsg`].
    #[serde(with = "serde_bytes")]
    bytes: Vec<u8>,
}

/// What the store knows of one segment.
#[derive(Clone, Copy, Default)]
struct Segment {
    /// How many times it was written again: its file's name, and what a
    /// record retiring a frame in it names, so one written before a rewrite
    /// names nothing after it.
    generation: u32,
    /// Its length in bytes.
    len: u64,
    /// The bytes of its records that hold nothing a replay reads.
    dead: u64,
    /// The bytes of retirement records a rewrite had to keep: dead, but not
    /// what another rewrite frees.
    kept: u64,
}

/// One retained frame as the index knows it.
#[derive(Clone, Copy)]
struct Entry {
    seq: u64,
    /// The frame's own bytes.
    bytes: u64,
    authored_at_ns: u64,
    retention: ContentRetention,
    /// Whether the store's first copy came over one of the device's links,
    /// rather than from the device itself or another carrier: what a reader
    /// of those links that fell behind is handed again
    /// ([`Store::taken_from_link`]). Known for the frames kept since the store
    /// opened, which is all a reader that subscribed since can miss.
    from_link: bool,
    /// The segment its record is in.
    segment: u32,
    /// Where its record starts there.
    at: u64,
    /// Its record's check: two frames at one sequence number with other
    /// bytes have other checks.
    check: [u8; 8],
}

impl Entry {
    /// The length of the record holding it.
    const fn record_len(&self) -> u64 {
        self.bytes.saturating_add(FRAME_OVERHEAD)
    }
}

/// How a writer's sync failed, if it did: each frame of a batch it covers answers that error
/// ([`Store::retain_all`]).
type SyncFailure = Option<(io::ErrorKind, String)>;

/// What staging a frame did: nothing more to do, or a record to sync.
enum Staging {
    Done(Retained),
    Appended(Staged),
}

/// A frame's record appended and not acknowledged yet.
struct Staged {
    writer: Arc<Writer>,
    /// Where its record starts.
    at: u64,
    /// Where its record ends: the segment synced this far holds it.
    end: u64,
    key: (Lineage, u64, [u8; 8]),
    /// The frame's own bytes.
    len: u64,
    outcome: Retained,
    from_link: bool,
}

/// The newest segment of a workspace, appended to under the store's lock
/// and synced outside it: whoever syncs covers every append before it.
struct Writer {
    segment: u32,
    file: Box<dyn Appender>,
    /// Bytes appended.
    written: AtomicU64,
    /// Bytes known synced.
    synced: std::sync::Mutex<u64>,
}

impl Writer {
    /// Wait until the segment is synced up to `end`.
    fn sync_to(&self, end: u64) -> io::Result<()> {
        let mut synced = self.synced.lock_recover();
        if *synced >= end {
            return Ok(());
        }
        let written = self.written.load(Ordering::Acquire);
        // A backend that commits later may hold `end` durable while appends
        // after it wait for their commit.
        let through = match self.file.sync(written) {
            Ok(()) => written,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock && end < written => {
                self.file.sync(end)?;
                end
            }
            Err(error) => return Err(error),
        };
        *synced = through;
        drop(synced);
        crate::work::commit();
        Ok(())
    }

    fn sync_all_written(&self) -> io::Result<()> {
        self.sync_to(self.written.load(Ordering::Acquire))
    }
}

/// A record holding a frame: its lineage, sequence number, time and class,
/// then the frame's bytes.
const FRAME_RECORD: u8 = 1;
/// A record retiring the frame record at a segment and offset.
const RETIRE_RECORD: u8 = 2;
/// Kind and body length.
const RECORD_HEADER: u64 = 1 + 4;
/// The check after a record's body.
const RECORD_CHECK: u64 = 8;
/// A frame record's header fields before the frame.
const FRAME_FIELDS: u64 = 32 + 32 + 8 + 8 + 1;
const FRAME_OVERHEAD: u64 = RECORD_HEADER + FRAME_FIELDS + RECORD_CHECK;
const RETIRE_BODY: u64 = 32 + 32 + 8 + 4 + 4 + 8;
const RETIRE_LEN: u64 = RECORD_HEADER + RETIRE_BODY + RECORD_CHECK;
/// A segment this long takes no more appends; the next one starts.
const SEGMENT_BYTES: u64 = 64 * 1024 * 1024;
/// A segment shorter than this is not written again for what it frees.
const REWRITE_BYTES: u64 = 64 * 1024;
/// The most segments the store keeps open for appending: a host keeps every
/// workspace it serves, and closes the least recently written beyond these.
const OPEN_WRITERS: usize = 256;
const PACK: &str = "pack";
/// A segment being written again, renamed over it once synced.
const NEXT: &str = "next";

/// A record read back from a segment.
enum Record<'a> {
    Frame {
        lineage: Lineage,
        seq: u64,
        authored_at_ns: u64,
        retention: ContentRetention,
        bytes: &'a [u8],
        check: [u8; 8],
    },
    Retire {
        lineage: Lineage,
        seq: u64,
        segment: u32,
        generation: u32,
        at: u64,
    },
}

fn check(kind: u8, body: &[u8]) -> [u8; 8] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[kind]);
    hasher.update(body);
    let mut out = [0; 8];
    out.copy_from_slice(hasher.finalize().as_bytes().get(..8).unwrap_or(&[0; 8]));
    out
}

fn record(kind: u8, body: &[u8]) -> Result<Vec<u8>, StoreError> {
    let len = u32::try_from(body.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "a frame too long for a segment",
        )
    })?;
    let mut out = Vec::with_capacity(body.len().saturating_add(13));
    out.push(kind);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(body);
    out.extend_from_slice(&check(kind, body));
    Ok(out)
}

/// The check that ends `record`, as [`record`] wrote it.
fn record_check(record: &[u8]) -> [u8; 8] {
    record
        .len()
        .checked_sub(8)
        .and_then(|at| array(record, at))
        .unwrap_or_default()
}

fn frame_record(frame: &Frame, bytes: &[u8]) -> Result<Vec<u8>, StoreError> {
    let mut body = Vec::with_capacity(bytes.len().saturating_add(81));
    body.extend_from_slice(&frame.lineage.record);
    body.extend_from_slice(&frame.lineage.author);
    body.extend_from_slice(&frame.seq.to_le_bytes());
    body.extend_from_slice(&frame.authored_at_ns.to_le_bytes());
    body.push(match frame.retention {
        ContentRetention::Checkpoint => 1,
        ContentRetention::Delta => 0,
    });
    body.extend_from_slice(bytes);
    record(FRAME_RECORD, &body)
}

fn retire_record(lineage: Lineage, entry: Entry, generation: u32) -> Result<Vec<u8>, StoreError> {
    let mut body = Vec::with_capacity(88);
    body.extend_from_slice(&lineage.record);
    body.extend_from_slice(&lineage.author);
    body.extend_from_slice(&entry.seq.to_le_bytes());
    body.extend_from_slice(&entry.segment.to_le_bytes());
    body.extend_from_slice(&generation.to_le_bytes());
    body.extend_from_slice(&entry.at.to_le_bytes());
    record(RETIRE_RECORD, &body)
}

fn array<const N: usize>(bytes: &[u8], at: usize) -> Option<[u8; N]> {
    bytes.get(at..at.checked_add(N)?)?.try_into().ok()
}

/// The record at the start of `bytes` and its length, or `None` where the
/// segment holds no whole record.
fn read_record(bytes: &[u8]) -> Option<(Record<'_>, usize)> {
    let kind = *bytes.first()?;
    let len = usize::try_from(u32::from_le_bytes(array(bytes, 1)?)).ok()?;
    let body = bytes.get(5..5_usize.checked_add(len)?)?;
    let end = 5_usize.checked_add(len)?.checked_add(8)?;
    let checked = check(kind, body);
    if array::<8>(bytes, end.checked_sub(8)?)? != checked {
        return None;
    }
    let lineage = Lineage {
        record: array(body, 0)?,
        author: array(body, 32)?,
    };
    let seq = u64::from_le_bytes(array(body, 64)?);
    let record = match kind {
        FRAME_RECORD => Record::Frame {
            lineage,
            seq,
            authored_at_ns: u64::from_le_bytes(array(body, 72)?),
            retention: match body.get(80)? {
                1 => ContentRetention::Checkpoint,
                0 => ContentRetention::Delta,
                _ => return None,
            },
            bytes: body.get(81..)?,
            check: checked,
        },
        RETIRE_RECORD => Record::Retire {
            lineage,
            seq,
            segment: u32::from_le_bytes(array(body, 72)?),
            generation: u32::from_le_bytes(array(body, 76)?),
            at: u64::from_le_bytes(array(body, 80)?),
        },
        _ => return None,
    };
    Some((record, end))
}

/// Refuse `message`, a frame read back, when a claim it carries names the
/// record's storage migration as a history move: bytes of a layout before the
/// step's fields were written whole, which a store kept by another build serves
/// as another claim. The record is named, so the device that holds it can set
/// it aside.
fn refuse_migration_as_history(message: &NeighbourMsg) -> Result<(), StoreError> {
    let NeighbourMsg::AppDocument {
        app_id,
        document_id,
        envelope,
        ..
    } = message
    else {
        return Ok(());
    };
    if envelope.migration_claimed_as_history().is_none() {
        return Ok(());
    }
    Err(StoreError::MigrationAsHistory {
        app: app_id.clone(),
        record: document_id.clone(),
        author: hex::encode(envelope.author.as_bytes()),
        seq: envelope.seq,
    })
}

/// Every whole record of a segment with its offset and length, and where the
/// whole records end.
fn records(bytes: &[u8]) -> (Vec<(u64, u64, Record<'_>)>, u64) {
    let mut found = Vec::new();
    let mut at = 0_usize;
    while let Some(rest) = bytes.get(at..)
        && !rest.is_empty()
    {
        let Some((record, len)) = read_record(rest) else {
            break;
        };
        found.push((at as u64, len as u64, record));
        at = at.saturating_add(len);
    }
    (found, at as u64)
}

/// Set `root`'s frames aside when they were kept before claims named a call ([`WRITTEN_AGAIN`]),
/// to be written again as this build writes them, and name where they are kept.
fn set_aside_earlier(disk: &Disk, kept: u32) -> Result<PathBuf, StoreError> {
    let root = &disk.root;
    let earlier = root.join(EARLIER_SEGMENTS);
    if WRITTEN_AGAIN.contains(&kept) {
        if disk.is_dir(&earlier) {
            // A packing a crash cut short: what it wrote goes, what was set aside stays.
            if disk.is_dir(&root.join(WORKSPACES)) {
                disk.remove_all(&root.join(WORKSPACES))?;
            }
        } else if disk.is_dir(&root.join(WORKSPACES)) {
            disk.create_dir_all(&earlier)?;
            disk.rename(&root.join(WORKSPACES), &earlier.join(WORKSPACES))?;
        }
    }
    Ok(earlier)
}

/// Say that two frames with other bytes share one author's sequence number, which the store
/// keeps and serves both of.
fn beside(workspace: WorkspaceId, frame: &Frame) {
    tracing::warn!(
        %workspace,
        author = %hex::encode(frame.lineage.author),
        seq = frame.seq,
        "two frames with other bytes at one author's sequence number; the store keeps and serves both"
    );
}

/// The route the store keeps `message`, a change to `workspace`'s own state
/// ([`Store::keep_workspace_change`]), under: named by the change's own
/// bytes, so none replaces the change before it. `None` when `message` is no
/// change to `workspace`'s own state.
fn workspace_change_route(
    workspace: WorkspaceId,
    message: &NeighbourMsg,
) -> Result<Option<[u8; 32]>, StoreError> {
    let NeighbourMsg::SyncResp {
        workspace: named,
        changes,
    } = message
    else {
        return Ok(None);
    };
    if changes.is_empty() || WorkspaceId::parse(named) != Some(workspace) {
        return Ok(None);
    }
    let bytes = postcard::to_allocvec(message)?;
    let mut seed = b"workspace-governance\0".to_vec();
    seed.extend_from_slice(workspace.as_uuid().as_bytes());
    seed.extend_from_slice(blake3::hash(&bytes).as_bytes());
    Ok(Some(*blake3::hash(&seed).as_bytes()))
}

impl Store {
    /// Open the store rooted at `root`, reading back the frames already
    /// there, and retire what `appetite` no longer allows.
    ///
    /// # Errors
    ///
    /// Returns an error when the root cannot be created or read. A file that
    /// is not a segment is skipped and logged, and so are the bytes after a
    /// segment's last whole record but in its newest segment, where a crash
    /// leaves them and they are cut off.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(root: impl Into<PathBuf>, appetite: Appetite) -> Result<Self, StoreError> {
        Self::open_on(Arc::new(FsBackend::new(root)), appetite)
    }

    /// [`Self::open`] the store `backend` keeps at its root.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    pub fn open_on(backend: Arc<dyn Backend>, appetite: Appetite) -> Result<Self, StoreError> {
        let disk = Disk {
            backend,
            root: PathBuf::new(),
        };
        Self::open_with(disk, appetite, Limits::default())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one walk over the store's directory, each workspace's segments in turn"
    )]
    fn open_with(disk: Disk, appetite: Appetite, limits: Limits) -> Result<Self, StoreError> {
        let root = disk.root.clone();
        let kept = frames_version(&disk)?;
        if !(*WRITTEN_AGAIN.start()..=FRAMES_VERSION).contains(&kept) {
            return Err(StoreError::Version(kept));
        }
        // Frames whose claims name no call are set aside, and written again as this build writes
        // them. A packing a crash cut short starts again from what was set aside.
        let earlier = set_aside_earlier(&disk, kept)?;
        disk.create_dir_all(&root.join(WORKSPACES))?;
        let store = Self {
            disk,
            appetite,
            held: std::sync::Mutex::new(Held {
                bytes: 0,
                workspaces: BTreeMap::new(),
                writers: 0,
                appends: 0,
                limits,
                channel_order: 0,
            }),
            reach: crate::reach::Ledgers::default(),
            sequences: std::sync::Mutex::new(BTreeMap::new()),
        };
        let holders = store.stored_holders()?;
        let channels = store.stored_channels()?;
        {
            let mut held = store.held.lock_recover();
            store.read_segments(&mut held)?;
            for (workspace, route, channel) in channels {
                held.channel_order = held.channel_order.max(channel.order);
                held.workspaces
                    .entry(workspace)
                    .or_default()
                    .channels
                    .insert(route, channel);
            }
            for (workspace, device, cursors) in holders {
                held.workspaces
                    .entry(workspace)
                    .or_default()
                    .holders
                    .insert(device, holding(&cursors));
            }
            // A checkpoint's retirements that a crash cut off before they
            // were synced are made again. A sealed compaction's writes stay
            // until every device with its record's keys checked it.
            store.retire_cut_off_checkpoints(&mut held);
            retire_to_appetite(&store.disk, appetite, &mut held);
            sync_writers(&held)?;
            drop(held);
        }
        if WRITTEN_AGAIN.contains(&kept) {
            store.pack_earlier(&earlier, kept)?;
        }
        if kept < FRAMES_VERSION || !store.disk.is_file(&root.join(FRAMES_VERSION_FILE)) {
            store.disk.replace(
                &root.join(FRAMES_VERSION_FILE),
                FRAMES_VERSION.to_string().as_bytes(),
            )?;
        }
        if store.disk.is_dir(&earlier) {
            store.disk.remove_all(&earlier)?;
        }
        Ok(store)
    }

    /// Make again the retirements of the checkpoints in `held` that a crash cut off before they
    /// were synced. A sealed compaction's writes stay until every device with its record's keys
    /// checked it.
    fn retire_cut_off_checkpoints(&self, held: &mut Held) {
        let checkpoints: Vec<(WorkspaceId, Lineage, Entry)> = held
            .workspaces
            .iter()
            .flat_map(|(workspace, stored)| {
                stored.lineages.iter().filter_map(|(lineage, entries)| {
                    let newest = newest_checkpoint(entries)?;
                    let entry = entries.iter().rev().find(|entry| {
                        entry.seq == newest && entry.retention == ContentRetention::Checkpoint
                    })?;
                    (entries.first()?.seq < newest).then_some((*workspace, *lineage, *entry))
                })
            })
            .collect();
        let mut reading = Reading::default();
        for (workspace, lineage, entry) in checkpoints {
            let message = held.workspaces.get(&workspace).and_then(|stored| {
                tilo_failure::ignored!(
                    "a checkpoint that does not read back retires its author's earlier frames by its sequence number",
                    reading.frame(&self.disk, workspace, stored, entry).ok()
                )
            });
            if let Some(unchecked) = message
                .as_ref()
                .and_then(|message| Unchecked::of(message, lineage, entry.check))
            {
                held.workspaces
                    .entry(workspace)
                    .or_default()
                    .unchecked
                    .push(unchecked);
                continue;
            }
            retire_before(&self.disk, workspace, lineage, entry.seq, held);
        }
    }

    /// The appetite this store was opened with.
    #[must_use]
    pub const fn appetite(&self) -> Appetite {
        self.appetite
    }

    /// The frame bytes the store holds now.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.held.lock_recover().bytes
    }

    /// The bytes the store holds of `workspace`.
    #[must_use]
    pub fn bytes_of(&self, workspace: WorkspaceId) -> u64 {
        self.held
            .lock_recover()
            .workspaces
            .get(&workspace)
            .map_or(0, |stored| stored.bytes)
    }

    /// Where the store stands on each author's line of `workspace`: the
    /// cursor at the latest frame it keeps there, with the earliest it keeps.
    /// The cursors are what a device whose store this is asks another's to
    /// continue from, and what it says it keeps.
    #[must_use]
    pub fn stands(&self, workspace: WorkspaceId) -> Vec<(Cursor, u64)> {
        let held = self.held.lock_recover();
        held.workspaces
            .get(&workspace)
            .map(|held| {
                held.lineages
                    .iter()
                    .filter_map(|(lineage, entries)| {
                        let first = entries.first()?.seq;
                        let last = entries.last()?.seq;
                        Some((
                            Cursor {
                                lineage: *lineage,
                                after_seq: last,
                            },
                            first,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Keep `frame` for `workspace`, unless the store holds that frame of
    /// its lineage already. A checkpoint retires the frames it supersedes:
    /// its author's writes up to its own sequence number, and the other
    /// authors' writes of the record its sealed compaction names in the
    /// clear. The appetite retires what it must. Returns once the frame is
    /// synced, with the frames written beside it.
    ///
    /// # Errors
    ///
    /// Returns an error when the frame cannot be written. The store does not
    /// serve it then.
    pub fn retain(&self, workspace: WorkspaceId, frame: &Frame) -> Result<Retained, StoreError> {
        self.retain_from(workspace, frame, false, true)
    }

    /// [`Self::retain`] each of `frames`, in order, with one sync for all of
    /// them: a batch of writes waits for the disk once, not once a frame.
    /// Returns once every frame is synced, with what keeping each did.
    ///
    /// # Errors
    ///
    /// Returns the first error a frame met. The store serves none of the
    /// frames the failed sync was to cover; those staged before a frame it
    /// could not write are synced and served.
    pub fn retain_all(
        &self,
        workspace: WorkspaceId,
        frames: &[Frame],
    ) -> Result<Vec<Retained>, StoreError> {
        let mut staged = Vec::with_capacity(frames.len());
        let mut failed = None;
        for frame in frames {
            match self.stage(workspace, frame, false, true) {
                Ok(staging) => staged.push((frame, staging)),
                Err(error) => {
                    failed = Some(error);
                    break;
                }
            }
        }
        // A writer synced to its last staged end covers every record staged before it.
        let mut ends: Vec<(Arc<Writer>, u64)> = Vec::new();
        for (_, staging) in &staged {
            if let Staging::Appended(appended) = staging {
                match ends
                    .iter_mut()
                    .find(|(writer, _)| Arc::ptr_eq(writer, &appended.writer))
                {
                    Some((_, end)) => *end = (*end).max(appended.end),
                    None => ends.push((Arc::clone(&appended.writer), appended.end)),
                }
            }
        }
        let synced: Vec<(Arc<Writer>, SyncFailure)> = ends
            .into_iter()
            .map(|(writer, end)| {
                let error = writer
                    .sync_to(end)
                    .err()
                    .map(|error| (error.kind(), error.to_string()));
                (writer, error)
            })
            .collect();
        let mut kept = Vec::with_capacity(staged.len());
        for (frame, staging) in staged {
            match staging {
                Staging::Done(outcome) => kept.push(Ok(outcome)),
                Staging::Appended(appended) => {
                    let sync = synced
                        .iter()
                        .find(|(writer, _)| Arc::ptr_eq(writer, &appended.writer))
                        .and_then(|(_, error)| error.as_ref())
                        .map_or(Ok(()), |(kind, error)| {
                            Err(io::Error::new(*kind, error.clone()))
                        });
                    kept.push(self.settle(workspace, frame, appended, sync));
                }
            }
        }
        if let Some(error) = failed {
            return Err(error);
        }
        kept.into_iter().collect()
    }

    /// [`Self::retain`] a frame one of the device's links brought it: the
    /// store remembers it came that way ([`Self::taken_from_link`]).
    ///
    /// # Errors
    ///
    /// As [`Self::retain`].
    pub fn retain_from_link(
        &self,
        workspace: WorkspaceId,
        frame: &Frame,
    ) -> Result<Retained, StoreError> {
        self.retain_from(workspace, frame, true, true)
    }

    /// Keep `frame` though a newer frame of its lineage exists, when a move
    /// found no checkpoint of that newer frame's author and record it could
    /// be read against: a frame with nothing covering it is kept rather than
    /// dropped. The store serves it as any frame it keeps.
    ///
    /// # Errors
    ///
    /// As [`Self::retain`].
    pub fn keep_uncovered(
        &self,
        workspace: WorkspaceId,
        frame: &Frame,
    ) -> Result<Retained, StoreError> {
        self.retain_from(workspace, frame, false, false)
    }

    /// Whether the store's first copy of `frame` came over one of the device's
    /// links since the store opened.
    #[must_use]
    pub fn taken_from_link(&self, workspace: WorkspaceId, frame: &Frame) -> bool {
        self.held
            .lock_recover()
            .workspaces
            .get(&workspace)
            .and_then(|held| held.lineages.get(&frame.lineage))
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.seq == frame.seq && entry.from_link)
            })
    }

    fn retain_from(
        &self,
        workspace: WorkspaceId,
        frame: &Frame,
        from_link: bool,
        cover: bool,
    ) -> Result<Retained, StoreError> {
        let staged = match self.stage(workspace, frame, from_link, cover)? {
            Staging::Done(outcome) => return Ok(outcome),
            Staging::Appended(staged) => staged,
        };
        let synced = staged.writer.sync_to(staged.end);
        self.settle(workspace, frame, staged, synced)
    }

    /// Append `frame`'s record under the store's lock, unless the store
    /// holds it or a checkpoint covers it.
    fn stage(
        &self,
        workspace: WorkspaceId,
        frame: &Frame,
        from_link: bool,
        cover: bool,
    ) -> Result<Staging, StoreError> {
        let bytes = postcard::to_allocvec(&frame.message)?;
        let record = frame_record(frame, &bytes)?;
        let check = record_check(&record);
        let key = (frame.lineage, frame.seq, check);
        let mut outcome = Retained::New;
        let (writer, at) = {
            let mut held = self.held.lock_recover();
            if let Some(stored) = held.workspaces.get(&workspace) {
                let entries = stored
                    .lineages
                    .get(&frame.lineage)
                    .map_or(&[][..], Vec::as_slice);
                if stored.staged.contains_key(&key)
                    || entries
                        .iter()
                        .any(|entry| entry.seq == frame.seq && entry.check == check)
                {
                    return Ok(Staging::Done(Retained::Duplicate));
                }
                // A write a sealed compaction stands in for is kept until every device with
                // its record's keys checked it: one that fetches it reads it from here.
                if cover
                    && newest_checkpoint(entries).is_some_and(|newest| newest > frame.seq)
                    && !stored
                        .unchecked
                        .iter()
                        .any(|unchecked| unchecked.lineage == frame.lineage)
                {
                    return Ok(Staging::Done(Retained::Covered));
                }
                if entries.iter().any(|entry| entry.seq == frame.seq)
                    || stored
                        .staged
                        .keys()
                        .any(|(lineage, seq, _)| *lineage == frame.lineage && *seq == frame.seq)
                {
                    beside(workspace, frame);
                    outcome = Retained::Beside;
                }
            }
            let appended = append(&self.disk, workspace, &mut held, &record)?;
            held.workspaces
                .entry(workspace)
                .or_default()
                .staged
                .insert(key, appended.0.segment);
            drop(held);
            appended
        };
        Ok(Staging::Appended(Staged {
            end: at.saturating_add(record.len() as u64),
            writer,
            at,
            key,
            len: bytes.len() as u64,
            outcome,
            from_link,
        }))
    }

    /// Acknowledge a staged frame once `synced` says its segment is synced,
    /// or let it go unserved when not.
    fn settle(
        &self,
        workspace: WorkspaceId,
        frame: &Frame,
        staged: Staged,
        synced: io::Result<()>,
    ) -> Result<Retained, StoreError> {
        let Staged {
            writer,
            at,
            key,
            len,
            outcome,
            from_link,
            ..
        } = staged;
        let mut held = self.held.lock_recover();
        let stored = held.workspaces.entry(workspace).or_default();
        stored.staged.remove(&key);
        let entry = Entry {
            seq: frame.seq,
            bytes: len,
            authored_at_ns: frame.authored_at_ns,
            retention: frame.retention,
            from_link,
            segment: writer.segment,
            at,
            check: key.2,
        };
        if let Err(error) = synced {
            // Not acknowledged, so not served; a restart may read it back,
            // signed as it is, and serves it then.
            stored.retired.insert((entry.segment, entry.at));
            if let Some(segment) = stored.segments.get_mut(&entry.segment) {
                segment.dead = segment.dead.saturating_add(entry.record_len());
            }
            return Err(error.into());
        }
        // Read before the frame joins the index: a sealed compaction is
        // numbered at the last write it folds, so that write shares the
        // checkpoint's sequence number and is only reached this way.
        let superseded = if frame.retention == ContentRetention::Checkpoint {
            superseded_entries(&held, workspace, frame)
        } else {
            Vec::new()
        };
        add(&mut held, workspace, frame.lineage, entry);
        let unchecked = (frame.retention == ContentRetention::Checkpoint)
            .then(|| Unchecked::of(&frame.message, frame.lineage, key.2))
            .flatten();
        if let Some(unchecked) = unchecked {
            // A sealed compaction names, in the clear, each author and the
            // highest write it folds, and its author signs that list. It is a
            // cache of those writes, never their replacement: they stay until
            // every device with the record's keys checked it against them
            // ([`Self::retire_checked`]).
            held.workspaces
                .entry(workspace)
                .or_default()
                .unchecked
                .push(unchecked);
        } else if frame.retention == ContentRetention::Checkpoint {
            // A checkpoint retires its own author's earlier frames: a
            // newcomer is sent the checkpoint already carrying their state,
            // not those writes as well.
            let retired = retire_covered(&self.disk, workspace, &superseded, &mut held);
            tracing::debug!(
                %retired.frames,
                %retired.bytes,
                "a checkpoint retired the frames it covers"
            );
        }
        retire_to_appetite(&self.disk, self.appetite, &mut held);
        drop(held);
        crate::work::frame_written(len);
        Ok(outcome)
    }

    /// The sealed compactions of `workspace` whose writes the store still keeps: each is retired
    /// with them once every device with its record's keys checked it ([`Self::retire_checked`]).
    #[must_use]
    pub fn unchecked(&self, workspace: WorkspaceId) -> Vec<UncheckedCheckpoint> {
        self.held
            .lock_recover()
            .workspaces
            .get(&workspace)
            .map_or_else(Vec::new, |stored| {
                stored
                    .unchecked
                    .iter()
                    .map(|unchecked| unchecked.checkpoint.clone())
                    .collect()
            })
    }

    /// Retire the writes `checkpoint`, a sealed compaction of `workspace` every device with its
    /// record's keys checked, stands in for: each author's writes up to the last one it names,
    /// whenever they arrived. The checkpoint itself stays.
    pub fn retire_checked(
        &self,
        workspace: WorkspaceId,
        checkpoint: &UncheckedCheckpoint,
    ) -> Retired {
        let mut held = self.held.lock_recover();
        let Some(stored) = held.workspaces.get_mut(&workspace) else {
            return Retired::default();
        };
        let Some(at) = stored
            .unchecked
            .iter()
            .position(|unchecked| &unchecked.checkpoint == checkpoint)
        else {
            return Retired::default();
        };
        let unchecked = stored.unchecked.remove(at);
        let own = (unchecked.lineage, unchecked.check);
        let covered: Vec<(Lineage, u64, [u8; 8])> = unchecked
            .covers
            .iter()
            .flat_map(|(author, through)| {
                let lineage = Lineage {
                    record: unchecked.lineage.record,
                    author: *author,
                };
                stored
                    .lineages
                    .get(&lineage)
                    .map_or(&[][..], Vec::as_slice)
                    .iter()
                    .filter(move |entry| entry.seq <= *through && (lineage, entry.check) != own)
                    .map(move |entry| (lineage, entry.seq, entry.check))
            })
            .collect();
        let retired = retire_covered(&self.disk, workspace, &covered, &mut held);
        tracing::debug!(
            %retired.frames,
            %retired.bytes,
            "a checked checkpoint retired the writes it stands in for"
        );
        if let Err(error) = sync_writers(&held) {
            tracing::warn!(%error, "the retirement of checked writes is synced with the next write");
        }
        drop(held);
        retired
    }

    /// Whether the store holds `frame` of `workspace` now, byte for byte.
    ///
    /// # Errors
    ///
    /// The frame does not encode.
    pub fn holds_frame(&self, workspace: WorkspaceId, frame: &Frame) -> Result<bool, StoreError> {
        let bytes = postcard::to_allocvec(&frame.message)?;
        let check = record_check(&frame_record(frame, &bytes)?);
        Ok(self
            .held
            .lock_recover()
            .workspaces
            .get(&workspace)
            .and_then(|stored| stored.lineages.get(&frame.lineage))
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.seq == frame.seq && entry.check == check)
            }))
    }

    /// The frames after `cursors` in `workspace`, at most `budget` of them,
    /// and the lineages the store cannot continue from them.
    ///
    /// # Errors
    ///
    /// Returns an error when a stored frame cannot be read back.
    pub fn replay(
        &self,
        workspace: WorkspaceId,
        cursors: &[Cursor],
        budget: Budget,
    ) -> Result<Replay, StoreError> {
        self.read(workspace, None, cursors, budget)
    }

    /// The frames of the record `channel` in `workspace` after `cursors`, at
    /// most `budget` of them, as [`Self::replay`] serves a workspace's: each of
    /// the record's lines in the order it was written, a checkpoint before the
    /// changes it bases. A device that opens a record holds none of it and
    /// names no cursors; one sent part of it already continues from what it was
    /// sent.
    ///
    /// # Errors
    ///
    /// Returns an error when a stored frame cannot be read back.
    pub fn replay_record(
        &self,
        workspace: WorkspaceId,
        channel: [u8; 32],
        cursors: &[Cursor],
        budget: Budget,
    ) -> Result<Replay, StoreError> {
        self.read(workspace, Some(channel), cursors, budget)
    }

    /// [`Self::replay`], of one record's lines alone when `only` names it.
    fn read(
        &self,
        workspace: WorkspaceId,
        only: Option<[u8; 32]>,
        cursors: &[Cursor],
        budget: Budget,
    ) -> Result<Replay, StoreError> {
        self.read_as(workspace, only, cursors, budget, |bytes| {
            Ok(NeighbourMsg::decode(bytes)?)
        })
    }

    /// [`Self::read`], each frame's bytes read by `decode`.
    fn read_as(
        &self,
        workspace: WorkspaceId,
        only: Option<[u8; 32]>,
        cursors: &[Cursor],
        budget: Budget,
        decode: impl Fn(&[u8]) -> Result<NeighbourMsg, StoreError>,
    ) -> Result<Replay, StoreError> {
        let held = self.held.lock_recover();
        let mut replay = Replay::default();
        let Some(stored) = held.workspaces.get(&workspace) else {
            return Ok(replay);
        };
        // Each author line's frames after its cursor, in the line's order.
        let mut pending: Vec<(Lineage, &[Entry])> = Vec::new();
        for (lineage, entries) in &stored.lineages {
            if only.is_some_and(|channel| lineage.record != channel) {
                continue;
            }
            let after = cursors
                .iter()
                .filter(|cursor| cursor.lineage == *lineage)
                .map(|cursor| cursor.after_seq)
                .max();
            let Some(first) = entries.first().map(|entry| entry.seq) else {
                continue;
            };
            if first > after.unwrap_or(0).saturating_add(1) {
                replay.floors.push(Floor {
                    lineage: *lineage,
                    earliest_seq: first,
                });
            }
            // A subscriber with no cursor holds none of the lineage, a
            // checkpoint at sequence number 0 included.
            let from = after.map_or(0, |after| {
                entries.partition_point(|entry| entry.seq <= after)
            });
            if let Some(rest) = entries.get(from..)
                && !rest.is_empty()
            {
                pending.push((*lineage, rest));
            }
        }
        // The lines are merged rather than sent one after another: a change
        // is taken in only once its record holds a baseline, so each record's
        // baselines go first, then every change in the order it was written.
        // A line sent whole before the line holding the baseline left that
        // line's changes held back for a baseline that came after them.
        let mut carried = 0_usize;
        let mut carried_bytes = 0_u64;
        let mut reading = Reading::default();
        loop {
            let next = pending
                .iter()
                .enumerate()
                .filter_map(|(at, (lineage, rest))| {
                    rest.first().map(|entry| {
                        let key = (
                            entry.retention != ContentRetention::Checkpoint,
                            entry.authored_at_ns,
                            *lineage,
                        );
                        (key, at)
                    })
                })
                .min();
            let Some((_, at)) = next else {
                break;
            };
            let Some((_, rest)) = pending.get_mut(at) else {
                break;
            };
            let Some((entry, after)) = rest.split_first() else {
                break;
            };
            if carried >= budget.max_frames
                || carried_bytes.saturating_add(entry.bytes) > budget.max_bytes
            {
                replay.more = true;
                break;
            }
            let message = decode(&reading.bytes(&self.disk, workspace, stored, *entry)?)?;
            refuse_migration_as_history(&message)?;
            replay.frames.push(message);
            carried = carried.saturating_add(1);
            carried_bytes = carried_bytes.saturating_add(entry.bytes);
            *rest = after;
        }
        drop(held);
        crate::work::frames_read(u64::try_from(carried).unwrap_or(u64::MAX), carried_bytes);
        Ok(replay)
    }

    /// Forget `workspace` and every frame the store retains of it: what a
    /// workspace deleted on its host leaves behind, since nothing names it
    /// again. The index entry goes under the lock; its directory, whose files
    /// are what replay reads, is removed after it, so a store serving the
    /// other workspaces does not wait for the unlink while it holds the lock.
    /// Removing a workspace's files is left to the caller to run off the
    /// thread that serves requests.
    ///
    /// # Errors
    ///
    /// Returns an error when the workspace's directory cannot be removed.
    pub fn forget(&self, workspace: WorkspaceId) -> Result<Retired, StoreError> {
        let (bytes, frames) = {
            let mut held = self.held.lock_recover();
            let Some(stored) = held.workspaces.remove(&workspace) else {
                return Ok(Retired::default());
            };
            if stored.writer.is_some() {
                held.writers = held.writers.saturating_sub(1);
            }
            for dir in [
                holders_dir(&self.disk.root, workspace),
                channels_dir(&self.disk.root, workspace),
            ] {
                match self.disk.remove_all(&dir) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            held.bytes = held.bytes.saturating_sub(stored.bytes);
            let frames = stored.lineages.values().map(Vec::len).sum::<usize>();
            drop(held);
            (stored.bytes, frames)
        };
        match self
            .disk
            .remove_all(&workspace_dir(&self.disk.root, workspace))
        {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.reach.forget(&self.disk, workspace)?;
        Ok(Retired {
            frames: u64::try_from(frames).unwrap_or(u64::MAX),
            bytes,
        })
    }

    /// `device`, which asked for `workspace`'s history from `cursors`, holds
    /// every frame up to them and none after: compaction keeps what it lacks
    /// ([`Self::compact`]). Kept across restarts.
    ///
    /// # Errors
    ///
    /// Returns an error when what the device holds cannot be written.
    pub fn holds(
        &self,
        workspace: WorkspaceId,
        device: [u8; 32],
        cursors: &[Cursor],
    ) -> Result<(), StoreError> {
        let holding = holding(cursors);
        let mut held = self.held.lock_recover();
        let stored = held.workspaces.entry(workspace).or_default();
        if stored.holders.get(&device) == Some(&holding) {
            return Ok(());
        }
        let path = holder_path(&self.disk.root, workspace, device);
        if let Some(parent) = path.parent() {
            self.disk.create_dir_all(parent)?;
        }
        let cursors: Vec<Cursor> = holding
            .iter()
            .map(|(lineage, after_seq)| Cursor {
                lineage: *lineage,
                after_seq: *after_seq,
            })
            .collect();
        self.disk
            .replace(&path, &postcard::to_allocvec(&cursors)?)?;
        stored.holders.insert(device, holding);
        drop(held);
        Ok(())
    }

    /// Retire the frames of `workspace` published before `before_ns`, for an
    /// operator whose store must shrink now rather than at its budget. A
    /// frame some device that told the store what it holds still lacks
    /// stays, and so does each lineage's newest checkpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when a frame cannot be removed.
    pub fn compact(&self, workspace: WorkspaceId, before_ns: u64) -> Result<Retired, StoreError> {
        let mut held = self.held.lock_recover();
        let mut retired = Retired::default();
        let Some(stored) = held.workspaces.get(&workspace) else {
            return Ok(retired);
        };
        let everyone_holds = |lineage: &Lineage, seq: u64| {
            stored
                .holders
                .values()
                .all(|holds| holds.get(lineage).is_some_and(|after| *after >= seq))
        };
        let old = stored
            .lineages
            .iter()
            .flat_map(|(lineage, entries)| {
                let checkpoint = newest_checkpoint(entries);
                entries
                    .iter()
                    .filter(|entry| {
                        entry.authored_at_ns < before_ns
                            && Some(entry.seq) != checkpoint
                            && everyone_holds(lineage, entry.seq)
                    })
                    .map(|entry| (*lineage, *entry))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        for (lineage, entry) in old {
            retire(
                &self.disk,
                workspace,
                lineage,
                entry,
                &mut held,
                &mut retired,
            )?;
        }
        sync_writers(&held)?;
        drop(held);
        Ok(retired)
    }

    /// Keep `message`, `author`'s channel governance for `workspace`, as the
    /// newest frame of its route, synced before this returns: a group's
    /// genesis is kept for good, its newest commit and the copy its joiners
    /// join from replace the ones before, and so does each device's newest
    /// access request. Returns whether the store holds anything new; a frame
    /// that is not channel governance is not kept.
    ///
    /// # Errors
    ///
    /// Returns an error when the frame cannot be written. The store keeps
    /// the frame before it then.
    pub fn keep_channel(
        &self,
        workspace: WorkspaceId,
        author: [u8; 32],
        message: &NeighbourMsg,
    ) -> Result<bool, StoreError> {
        if message.governance_workspace().is_none() {
            return Ok(false);
        }
        let Some((route, neighbour_wire::Retention::Checkpoint)) =
            neighbour_wire::retention(message, false)
        else {
            return Ok(false);
        };
        let bytes = postcard::to_allocvec(message)?;
        self.keep_at(workspace, route, author, bytes)
    }

    /// Keep `message`, `author`'s change to `workspace`'s own state, as a
    /// channel frame of a route of its own: each change keeps one, so none
    /// replaces the change before it, and a device that links later is
    /// replayed them in the order they were made, before the records they
    /// govern. Returns whether the store holds anything new; a message that
    /// is no change to `workspace`'s own state is not kept.
    ///
    /// # Errors
    ///
    /// Returns an error when the frame cannot be written. The store keeps
    /// the frames before it then.
    pub fn keep_workspace_change(
        &self,
        workspace: WorkspaceId,
        author: [u8; 32],
        message: &NeighbourMsg,
    ) -> Result<bool, StoreError> {
        let Some(route) = workspace_change_route(workspace, message)? else {
            return Ok(false);
        };
        let bytes = postcard::to_allocvec(message)?;
        self.keep_at(workspace, route, author, bytes)
    }

    /// Every change to `workspace`'s own state the store kept
    /// ([`Self::keep_workspace_change`]), each with the author it kept it
    /// for, in the order it kept them.
    #[must_use]
    pub fn kept_workspace_changes(&self, workspace: WorkspaceId) -> Vec<([u8; 32], NeighbourMsg)> {
        let mut kept: Vec<Channel> = self
            .held
            .lock_recover()
            .workspaces
            .get(&workspace)
            .map(|stored| stored.channels.values().cloned().collect())
            .unwrap_or_default();
        kept.sort_by_key(|channel| channel.order);
        kept.into_iter()
            .filter_map(|channel| match NeighbourMsg::decode(&channel.bytes) {
                Ok(message) => match workspace_change_route(workspace, &message) {
                    Ok(Some(_)) => Some((channel.author, message)),
                    Ok(None) => None,
                    Err(error) => {
                        tracing::warn!(%error, "a kept channel frame is no change to the workspace's own state; it is not served");
                        None
                    }
                },
                Err(error) => {
                    tracing::warn!(%error, "a kept channel frame does not decode; it is not served");
                    None
                }
            })
            .collect()
    }

    /// Drop the change to `workspace`'s own state the store kept of
    /// `message`, whoever it was kept for: false when it kept none.
    ///
    /// # Errors
    ///
    /// Returns an error when the frame's file cannot be removed.
    pub fn forget_workspace_change(
        &self,
        workspace: WorkspaceId,
        message: &NeighbourMsg,
    ) -> Result<bool, StoreError> {
        let Some(route) = workspace_change_route(workspace, message)? else {
            return Ok(false);
        };
        let removed = self
            .held
            .lock_recover()
            .workspaces
            .get_mut(&workspace)
            .and_then(|stored| stored.channels.remove(&route))
            .is_some();
        if !removed {
            return Ok(false);
        }
        let path = channels_dir(&self.disk.root, workspace).join(hex::encode(route));
        match self.disk.remove(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(error.into()),
        }
    }

    /// Keep `bytes`, `author`'s channel frame of `route`, replacing a frame
    /// of the same route that says the same: false when one does already.
    fn keep_at(
        &self,
        workspace: WorkspaceId,
        route: [u8; 32],
        author: [u8; 32],
        bytes: Vec<u8>,
    ) -> Result<bool, StoreError> {
        let mut held = self.held.lock_recover();
        if held
            .workspaces
            .get(&workspace)
            .and_then(|stored| stored.channels.get(&route))
            .is_some_and(|kept| kept.author == author && kept.bytes == bytes)
        {
            return Ok(false);
        }
        let channel = Channel {
            order: held.channel_order.saturating_add(1),
            author,
            bytes,
        };
        let path = channels_dir(&self.disk.root, workspace).join(hex::encode(route));
        self.disk
            .replace(&path, &postcard::to_allocvec(&channel)?)?;
        held.channel_order = channel.order;
        held.workspaces
            .entry(workspace)
            .or_default()
            .channels
            .insert(route, channel);
        drop(held);
        Ok(true)
    }

    /// Note `message`, a record write of this device's own that it sends to `workspace`'s
    /// other devices, as pending until one that keeps the workspace takes it in
    /// ([`crate::reach`]). Anything but an app's record write is not noted.
    pub fn note_own_write(&self, workspace: WorkspaceId, message: &NeighbourMsg) {
        self.reach.note(&self.disk, workspace, message);
    }

    /// Take another device's answer to this device's write `write` (its frame's hash) of
    /// `workspace`: kept, it reached that device; refused, it is listed refused.
    pub fn answered_own_write(
        &self,
        workspace: WorkspaceId,
        write: [u8; 32],
        kept: &crate::direct::Kept,
    ) {
        self.reach.answer(&self.disk, workspace, write, kept);
    }

    /// This device's writes of `app`'s `record` in `workspace` that no other device keeping
    /// it has taken in yet, and those the workspace refused.
    #[must_use]
    pub fn record_reach(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
    ) -> crate::reach::RecordReach {
        self.reach.record(&self.disk, workspace, app, record)
    }

    /// This device's writes of `workspace` no other device keeping it has taken in yet, in
    /// the order it wrote them: what a link that comes up sends again, so its answer comes.
    #[must_use]
    pub fn unreached(&self, workspace: WorkspaceId) -> Vec<NeighbourMsg> {
        self.reach.unreached(&self.disk, workspace)
    }

    /// Told each record of any workspace here whose reach moved: one of this device's writes
    /// of it was noted, taken in by another device, or refused.
    #[must_use]
    pub fn reach_moved(&self) -> tokio::sync::broadcast::Receiver<crate::reach::ReachMoved> {
        self.reach.moved()
    }

    /// `workspace`'s channel governance, each frame with its author, in the
    /// order the store kept them ([`Self::keep_channel`]).
    #[must_use]
    pub fn channels(&self, workspace: WorkspaceId) -> Vec<([u8; 32], NeighbourMsg)> {
        let mut kept: Vec<Channel> = self
            .held
            .lock_recover()
            .workspaces
            .get(&workspace)
            .map(|stored| stored.channels.values().cloned().collect())
            .unwrap_or_default();
        kept.sort_by_key(|channel| channel.order);
        kept.into_iter()
            .filter_map(|channel| match NeighbourMsg::decode(&channel.bytes) {
                Ok(message) => Some((channel.author, message)),
                Err(error) => {
                    tracing::warn!(%error, "a kept channel frame does not decode; it is not served");
                    None
                }
            })
            .collect()
    }

    /// The segments `workspace` keeps, and their bytes: what a diagnostic
    /// shows and a test of the layout reads.
    #[must_use]
    pub fn segments(&self, workspace: WorkspaceId) -> Vec<(u32, u64)> {
        self.held
            .lock_recover()
            .workspaces
            .get(&workspace)
            .map(|stored| {
                stored
                    .segments
                    .iter()
                    .map(|(number, segment)| (*number, segment.len))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The state kept under `name` ([`Self::keep_state`]), if any.
    ///
    /// # Errors
    ///
    /// `name` is not a relative path, or what it names cannot be read.
    pub fn state(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        match self.disk.read(&self.state_path(name)?) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Keep `bytes` under `name`, replacing what it held: state of the device's own the store
    /// holds whole beside its frames, never served as one. A name is a relative path, so a
    /// device groups its state as directories. It is written as the backend replaces a file,
    /// whole or not at all.
    ///
    /// # Errors
    ///
    /// `name` is not a relative path, or the state cannot be written.
    pub fn keep_state(&self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        Ok(self.disk.replace(&self.state_path(name)?, bytes)?)
    }

    /// Let the state kept under `name` go; none kept there is let go already.
    ///
    /// # Errors
    ///
    /// `name` is not a relative path, or the state cannot be removed.
    pub fn forget_state(&self, name: &str) -> Result<(), StoreError> {
        match self.disk.remove(&self.state_path(name)?) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }

    fn state_path(&self, name: &str) -> Result<PathBuf, StoreError> {
        let path = Path::new(name);
        let relative = path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)));
        if name.is_empty() || !relative {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name:?} is not the name of a store's state"),
            )
            .into());
        }
        Ok(self.disk.root.join(STATE).join(path))
    }

    /// What every device said it holds, as [`Store::open`] reads it. A file    /// that is not one is skipped, as a stray frame file is.
    fn stored_holders(&self) -> Result<Vec<HeldBy>, StoreError> {
        let mut found = Vec::new();
        for workspace in self.disk.directories(&self.disk.root.join(HOLDERS))? {
            let Some(id) = workspace
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(WorkspaceId::parse)
            else {
                continue;
            };
            for file in self.disk.files(&workspace)? {
                let device = file
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(<[u8; 32] as hex::FromHex>::from_hex);
                let cursors = postcard::from_bytes::<Vec<Cursor>>(&self.disk.read(&file)?);
                if let (Some(Ok(device)), Ok(cursors)) = (device, cursors) {
                    found.push((id, device, cursors));
                } else {
                    tracing::warn!(path = %file.display(), "not what a device holds; skipped");
                }
            }
        }
        Ok(found)
    }

    /// Every workspace's channel frames, as [`Store::open`] reads them. A file
    /// that is not one is skipped, as a stray segment is.
    fn stored_channels(&self) -> Result<Vec<(WorkspaceId, [u8; 32], Channel)>, StoreError> {
        let mut found = Vec::new();
        for workspace in self.disk.directories(&self.disk.root.join(CHANNELS))? {
            let Some(id) = workspace
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(WorkspaceId::parse)
            else {
                continue;
            };
            for file in self.disk.files(&workspace)? {
                let route = file
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(<[u8; 32] as hex::FromHex>::from_hex);
                let channel = postcard::from_bytes::<Channel>(&self.disk.read(&file)?);
                if let (Some(Ok(route)), Ok(channel)) = (route, channel) {
                    found.push((id, route, channel));
                } else {
                    tracing::warn!(path = %file.display(), "not a channel frame; skipped");
                }
            }
        }
        Ok(found)
    }

    /// Write the frames of version `kept` set aside at `earlier`, whose claims name no call, again
    /// as this build writes them, each signed as its author signed it
    /// ([`collab_types::before_call`]): those kept one file each in the order of their files, and
    /// those kept in segments in the order a replay serves them, what a frame retired there not
    /// moved. A frame that does not come up refuses the store by name, and the store keeps what it
    /// set aside for the next start: a store's history may be held nowhere else, so no write of
    /// it is left behind.
    fn pack_earlier(&self, earlier: &Path, kept: u32) -> Result<(), StoreError> {
        if !self.disk.is_dir(earlier) {
            return Ok(());
        }
        let refused = |what: String, error: String| {
            StoreError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{what}, kept at frames version {kept} before claims named a call, does not \
                     come up: {error}; what was set aside is kept at {}",
                    earlier.display()
                ),
            ))
        };
        let mut moved = 0_usize;
        let mut write = |workspace: WorkspaceId, frames: &[Frame]| -> Result<(), StoreError> {
            moved = moved.saturating_add(
                self.retain_all(workspace, frames)?
                    .iter()
                    .filter(|retained| **retained != Retained::Duplicate)
                    .count(),
            );
            Ok(())
        };
        if FRAME_FILES.contains(&kept) {
            for (workspace, files) in self.earlier_frame_files(earlier)? {
                let mut frames = Vec::with_capacity(files.len());
                for file in files {
                    let what = || format!("the frame {}", file.display());
                    let message = crate::state::frame_before_own_removal(&self.disk.read(&file)?)
                        .map_err(|error| refused(what(), error))?;
                    frames.push(
                        Frame::of(&message).map_err(|error| refused(what(), error.to_string()))?,
                    );
                }
                write(workspace, &frames)?;
            }
            tracing::info!(
                moved,
                "the store wrote the frames it kept one file each into segments as it writes them"
            );
            return Ok(());
        }
        let set_aside = Self {
            disk: Disk {
                backend: Arc::clone(&self.disk.backend),
                root: earlier.to_path_buf(),
            },
            appetite: self.appetite,
            held: std::sync::Mutex::new(Held {
                bytes: 0,
                workspaces: BTreeMap::new(),
                writers: 0,
                appends: 0,
                limits: self.held.lock_recover().limits,
                channel_order: 0,
            }),
            reach: crate::reach::Ledgers::default(),
            sequences: std::sync::Mutex::new(BTreeMap::new()),
        };
        let workspaces: Vec<WorkspaceId> = {
            let mut held = set_aside.held.lock_recover();
            set_aside.read_segments(&mut held)?;
            held.workspaces.keys().copied().collect()
        };
        let whole = Budget {
            max_frames: usize::MAX,
            max_bytes: u64::MAX,
        };
        for workspace in workspaces {
            let what = || format!("a frame of workspace {workspace}");
            let replay = set_aside.read_as(workspace, None, &[], whole, |bytes| {
                // Version 4 kept the member's own record a document write was, and comes up as
                // version 5 first.
                if BEFORE_OWN_REMOVAL_FRAMES.contains(&kept) {
                    crate::state::frame_before_own_removal(bytes)
                } else {
                    crate::state::frame_before_call(bytes)
                }
                .map_err(|error| refused(what(), error))
            })?;
            let frames = replay
                .frames
                .iter()
                .map(|message| {
                    Frame::of(message).map_err(|error| refused(what(), error.to_string()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            write(workspace, &frames)?;
        }
        tracing::info!(
            moved,
            "the store wrote the frames kept before claims named a call again as it writes them"
        );
        Ok(())
    }

    /// Each workspace's frames a store kept one file each before segments ([`FRAME_FILES`]), set
    /// aside at `earlier`: `workspaces/<workspace>/<record>/<author>/<seq>.<class>`, each author's
    /// in the order of their sequence numbers. A file whose name is hidden is a write a crash cut
    /// short before it took the frame's name, which the store never acknowledged.
    fn earlier_frame_files(
        &self,
        earlier: &Path,
    ) -> Result<Vec<(WorkspaceId, Vec<PathBuf>)>, StoreError> {
        let mut found = Vec::new();
        for dir in self.disk.directories(&earlier.join(WORKSPACES))? {
            let Some(workspace) = dir
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(WorkspaceId::parse)
            else {
                return Err(StoreError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{} is no workspace's frames; what was set aside is kept at {}",
                        dir.display(),
                        earlier.display()
                    ),
                )));
            };
            let mut files = Vec::new();
            for record in self.disk.directories(&dir)? {
                for author in self.disk.directories(&record)? {
                    for file in self.disk.files(&author)? {
                        if file
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with('.'))
                        {
                            tracing::warn!(path = %file.display(), "a frame's write a crash cut short; it was never acknowledged");
                            continue;
                        }
                        files.push(file);
                    }
                }
            }
            found.push((workspace, files));
        }
        Ok(found)
    }

    /// Index every workspace's segments, as [`Store::open`] reads them.
    fn read_segments(&self, held: &mut Held) -> Result<(), StoreError> {
        for dir in self.disk.directories(&self.disk.root.join(WORKSPACES))? {
            let Some(workspace) = dir
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(WorkspaceId::parse)
            else {
                continue;
            };
            let mut numbered = Vec::new();
            for file in self.disk.files(&dir)? {
                if file.extension().is_some_and(|extension| extension == NEXT) {
                    // A rewrite a crash cut short; the segment it was to
                    // replace is whole.
                    self.disk.remove(&file)?;
                    continue;
                }
                match segment_name(&file) {
                    Some((number, generation)) => numbered.push((number, generation, file)),
                    None => {
                        tracing::warn!(path = %file.display(), "not a segment of the store; it stays on disk and is not served");
                    }
                }
            }
            numbered.sort_by_key(|(number, generation, _)| (*number, *generation));
            // A rewrite a crash stopped after its rename leaves the
            // generation before it, which the newer one replaces whole.
            let mut segments: Vec<(u32, u32, PathBuf)> = Vec::new();
            for (number, generation, file) in numbered {
                if let Some(last) = segments.last_mut()
                    && last.0 == number
                {
                    self.disk.remove(&last.2)?;
                    *last = (number, generation, file);
                } else {
                    segments.push((number, generation, file));
                }
            }
            let newest = segments.last().map(|(number, _, _)| *number);
            for (number, generation, file) in segments {
                let bytes = self.disk.read(&file)?;
                let (found, whole) = records(&bytes);
                let stored = held.workspaces.entry(workspace).or_default();
                stored.segments.insert(
                    number,
                    Segment {
                        generation,
                        len: whole,
                        dead: 0,
                        kept: 0,
                    },
                );
                for (at, len, record) in found {
                    index_record(held, workspace, number, at, len, &record);
                }
                if whole < bytes.len() as u64 {
                    if Some(number) == newest {
                        tracing::warn!(path = %file.display(), whole, "a segment ends in a record a crash cut short; it is cut off");
                        self.disk.truncate(&file, whole)?;
                    } else {
                        tracing::warn!(path = %file.display(), whole, "a segment holds bytes that are not a record; what follows them is not served");
                    }
                }
            }
        }
        Ok(())
    }
}

/// Sync every segment open for appending.
fn sync_writers(held: &Held) -> io::Result<()> {
    for stored in held.workspaces.values() {
        if let Some(writer) = &stored.writer {
            writer.sync_all_written()?;
        }
    }
    Ok(())
}

/// Index `record`, read at `at` in `segment` of `workspace`.
fn index_record(
    held: &mut Held,
    workspace: WorkspaceId,
    segment: u32,
    at: u64,
    len: u64,
    record: &Record<'_>,
) {
    match *record {
        Record::Frame {
            lineage,
            seq,
            authored_at_ns,
            retention,
            bytes,
            check,
        } => {
            let entry = Entry {
                seq,
                bytes: bytes.len() as u64,
                authored_at_ns,
                retention,
                from_link: false,
                segment,
                at,
                check,
            };
            let stored = held.workspaces.entry(workspace).or_default();
            let duplicate = stored.lineages.get(&lineage).is_some_and(|entries| {
                entries
                    .iter()
                    .any(|held| held.seq == seq && held.check == check)
            });
            if duplicate {
                stored.retired.insert((segment, at));
                if let Some(segment) = stored.segments.get_mut(&segment) {
                    segment.dead = segment.dead.saturating_add(len);
                }
            } else {
                add(held, workspace, lineage, entry);
            }
        }
        Record::Retire {
            lineage,
            seq,
            segment: target,
            generation,
            at: target_at,
        } => {
            let Some(stored) = held.workspaces.get_mut(&workspace) else {
                return;
            };
            if let Some(own) = stored.segments.get_mut(&segment) {
                own.dead = own.dead.saturating_add(len);
            }
            if stored
                .segments
                .get(&target)
                .is_none_or(|target| target.generation != generation)
            {
                return;
            }
            let found = stored.lineages.get(&lineage).and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| {
                        entry.seq == seq && entry.segment == target && entry.at == target_at
                    })
                    .copied()
            });
            if let Some(entry) = found {
                unindex(held, workspace, lineage, entry);
            }
        }
    }
}

/// Append `record` to `workspace`'s newest segment, starting one when there
/// is none or it is full. Answers the segment's writer and where the record
/// starts; it is synced by [`Writer::sync_to`].
fn append(
    disk: &Disk,
    workspace: WorkspaceId,
    held: &mut Held,
    record: &[u8],
) -> Result<(Arc<Writer>, u64), StoreError> {
    held.appends = held.appends.saturating_add(1);
    let appends = held.appends;
    let full = held
        .workspaces
        .get(&workspace)
        .and_then(|stored| stored.writer.as_ref())
        .is_none_or(|writer| writer.written.load(Ordering::Acquire) >= held.limits.segment_bytes);
    if full {
        open_writer(disk, workspace, held)?;
    }
    let stored = held.workspaces.entry(workspace).or_default();
    stored.appended = appends;
    let Some(writer) = stored.writer.clone() else {
        return Err(io::Error::other("a workspace's segment did not open").into());
    };
    let at = writer.written.load(Ordering::Acquire);
    if let Err(error) = writer.file.append(record) {
        // What part of the record reached the file goes, so the next one
        // starts where this one did; failing that, the segment takes no
        // more and the next record starts the next one.
        if writer.file.truncate(at).is_err() {
            stored.writer = None;
            if let Some(segment) = stored.segments.get_mut(&writer.segment) {
                segment.len = u64::MAX;
            }
            held.writers = held.writers.saturating_sub(1);
        }
        return Err(error.into());
    }
    let end = at.saturating_add(record.len() as u64);
    writer.written.store(end, Ordering::Release);
    if let Some(segment) = stored.segments.get_mut(&writer.segment) {
        segment.len = end;
    }
    Ok((writer, at))
}

/// Open `workspace`'s newest segment for appending: the one it has when it
/// has room, or the next one, closing the writer appended to longest ago
/// when too many are open.
fn open_writer(disk: &Disk, workspace: WorkspaceId, held: &mut Held) -> Result<(), StoreError> {
    let root = &disk.root;
    let had = held
        .workspaces
        .get_mut(&workspace)
        .and_then(|stored| stored.writer.take());
    if let Some(writer) = had {
        writer.sync_all_written()?;
        held.writers = held.writers.saturating_sub(1);
    }
    while held.writers >= OPEN_WRITERS {
        let Some(idle) = held
            .workspaces
            .iter()
            .filter(|(_, stored)| stored.writer.is_some())
            .min_by_key(|(_, stored)| stored.appended)
            .map(|(id, _)| *id)
        else {
            break;
        };
        if let Some(writer) = held
            .workspaces
            .get_mut(&idle)
            .and_then(|stored| stored.writer.take())
        {
            writer.sync_all_written()?;
        }
        held.writers = held.writers.saturating_sub(1);
    }
    let dir = workspace_dir(root, workspace);
    if !disk.is_dir(&dir) {
        disk.create_dir_all(&dir)?;
        disk.sync_dir(&root.join(WORKSPACES))?;
    }
    let segment_bytes = held.limits.segment_bytes;
    let stored = held.workspaces.entry(workspace).or_default();
    let newest = stored.segments.iter().next_back().map(|(n, s)| (*n, *s));
    let (number, generation, len) = match newest {
        Some((number, segment)) if segment.len < segment_bytes => {
            (number, segment.generation, segment.len)
        }
        Some((number, _)) => (number.saturating_add(1), 0, 0),
        None => (0, 0, 0),
    };
    let path = segment_path(root, workspace, number, generation);
    let created = !disk.is_file(&path);
    let file = disk.open_append(&path)?;
    if created {
        disk.sync_dir(&dir)?;
    }
    stored.segments.entry(number).or_insert(Segment {
        generation,
        len,
        dead: 0,
        kept: 0,
    });
    stored.writer = Some(Arc::new(Writer {
        segment: number,
        file,
        written: AtomicU64::new(len),
        synced: std::sync::Mutex::new(len),
    }));
    held.writers = held.writers.saturating_add(1);
    Ok(())
}

/// Reads frames back, keeping the segment it read last open.
#[derive(Default)]
struct Reading {
    open: Option<(WorkspaceId, u32, Box<dyn ReadAt>)>,
}

impl Reading {
    fn frame(
        &mut self,
        disk: &Disk,
        workspace: WorkspaceId,
        stored: &Workspace,
        entry: Entry,
    ) -> Result<NeighbourMsg, StoreError> {
        Ok(NeighbourMsg::decode(
            &self.bytes(disk, workspace, stored, entry)?,
        )?)
    }

    /// The bytes of the frame `entry` names, as they were written.
    fn bytes(
        &mut self,
        disk: &Disk,
        workspace: WorkspaceId,
        stored: &Workspace,
        entry: Entry,
    ) -> Result<Vec<u8>, StoreError> {
        let reuse = matches!(&self.open, Some((id, segment, _)) if *id == workspace && *segment == entry.segment);
        if !reuse {
            let generation = stored
                .segments
                .get(&entry.segment)
                .map_or(0, |segment| segment.generation);
            let file = disk.open_read(&segment_path(
                &disk.root,
                workspace,
                entry.segment,
                generation,
            ))?;
            self.open = Some((workspace, entry.segment, file));
        }
        let Some((_, _, file)) = &mut self.open else {
            return Err(io::Error::other("a segment did not open").into());
        };
        let mut bytes = vec![0; usize::try_from(entry.bytes).unwrap_or(usize::MAX)];
        file.read_exact_at(
            entry
                .at
                .saturating_add(RECORD_HEADER)
                .saturating_add(FRAME_FIELDS),
            &mut bytes,
        )?;
        Ok(bytes)
    }
}

/// The floors of `replay` that a device with `cursors` cannot bridge: the
/// store has no frame the cursor leads into, so another device of the
/// workspace has to answer for the gap.
#[must_use]
pub fn gaps(cursors: &[Cursor], replay: &Replay) -> Vec<Floor> {
    replay
        .floors
        .iter()
        .filter(|floor| {
            let after = cursors
                .iter()
                .filter(|cursor| cursor.lineage == floor.lineage)
                .map(|cursor| cursor.after_seq)
                .max()
                .unwrap_or(0);
            floor.earliest_seq > after.saturating_add(1)
        })
        .copied()
        .collect()
}

const WORKSPACES: &str = "workspaces";

/// Where the end of the block of sequence numbers reserved for `lineage` of `workspace` is kept:
/// state of the device's own, beside its frames ([`Store::reserve_sequence`]).
fn sequence_state_name(workspace: WorkspaceId, lineage: Lineage) -> String {
    format!(
        "sequences/{}/{}-{}",
        workspace,
        hex::encode(lineage.record),
        hex::encode(lineage.author)
    )
}

/// The layout of the frames a store keeps: segments of records, each frame one
/// [`NeighbourMsg`] whose claims carry the caller's signed call, which the claim's signature
/// covers, and whose identity hello may name a placed device's certificate or a placement grant
/// ([`tilo_user_identity::DeviceCapabilities::records`]). Version 13 is version 12's layout,
/// whose messages may be the apps a workspace runs ([`NeighbourMsg::WorkspaceApps`]), which a
/// build of version 12 refuses; version 12 is version 11's layout,
/// whose claims name the types they store by the crates those types lived in before they moved
/// below the policy, which changes no byte a frame holds; version 11 is version 10's layout,
/// whose identity hello may name an agent's grant that names its agent by its id
/// ([`tilo_user_identity::AgentScope::Agent`]), which a build of version 10 refuses; version 10
/// is version 9's layout, whose
/// messages may be the word that pushed content is kept or let go of
/// ([`NeighbourMsg::DocumentContentKept`] and [`NeighbourMsg::DocumentContentReleased`]), which a
/// build of version 9 refuses; version 9 is version 8's, whose messages may be the content asked
/// for and answered with many at a time ([`NeighbourMsg::DocumentContentWants`] and
/// [`NeighbourMsg::DocumentContentPieces`]); version 8 is version 7's, whose identity hello a
/// build of version 7 refuses. A store of version 7 to 12 is relabelled as it opens, its frames
/// left as they are; one of version 1 to 6 has its frames written again ([`WRITTEN_AGAIN`]). A
/// store of a later version is refused as it opens, by its version, and left as it is.
pub(crate) const FRAMES_VERSION: u32 = 13;
/// The versions whose frames' claims name no call: 5, and 6, which differs from it only in the
/// definitions a certificate's capabilities are pinned by. A store of either is written again as
/// it opens, every frame kept with the signature its author made ([`Store::pack_earlier`]).
const BEFORE_CALL_FRAMES: std::ops::RangeInclusive<u32> = 5..=6;
/// The versions whose document writes named the member's own record they were, and whose claims
/// name no call either: one postcard [`NeighbourMsg`] per file up to version 3 ([`FRAME_FILES`]),
/// in segments at version 4. Version 1, which the released build keeps, ended a document write
/// with that record; version 2 ends it with the reader set it is sealed for after it, and version
/// 3 is the frames a document write may name its own content key in. Each frame comes up as
/// version 5 kept it ([`crate::state::frame_before_own_removal`]) and is written again as
/// [`BEFORE_CALL_FRAMES`]'s are.
const BEFORE_OWN_REMOVAL_FRAMES: std::ops::RangeInclusive<u32> = 1..=4;
/// The versions that kept each frame in a file of its own, before segments.
const FRAME_FILES: std::ops::RangeInclusive<u32> = 1..=3;
/// The versions whose frames a store writes again as it opens: [`BEFORE_OWN_REMOVAL_FRAMES`] and
/// [`BEFORE_CALL_FRAMES`].
const WRITTEN_AGAIN: std::ops::RangeInclusive<u32> =
    *BEFORE_OWN_REMOVAL_FRAMES.start()..=*BEFORE_CALL_FRAMES.end();
/// Where a store opening frames of [`WRITTEN_AGAIN`] sets them aside while it writes them again.
const EARLIER_SEGMENTS: &str = "earlier";

/// Where a store records the version of the frames it keeps: no file is version 1, kept before
/// a store recorded one.
const FRAMES_VERSION_FILE: &str = "frames-version";

/// The version of the frames kept at `root` ([`FRAMES_VERSION`]): the one recorded there, version
/// 1 when frames were kept before a store recorded one (set aside to be written again, too, when a
/// crash cut that short), today's for a store that keeps none yet.
fn frames_version(disk: &Disk) -> Result<u32, StoreError> {
    match disk
        .read(&disk.root.join(FRAMES_VERSION_FILE))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    {
        Ok(text) => text.trim().parse().map_err(|_| {
            StoreError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{FRAMES_VERSION_FILE} holds {text:?}, not a version"),
            ))
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if disk.directories(&disk.root.join(WORKSPACES))?.is_empty()
                && !disk.is_dir(&disk.root.join(EARLIER_SEGMENTS).join(WORKSPACES))
            {
                Ok(FRAMES_VERSION)
            } else {
                Ok(1)
            }
        }
        Err(error) => Err(error.into()),
    }
}
/// Where the store keeps what each device said it holds, per workspace.
const HOLDERS: &str = "holders";
/// Where the store keeps each workspace's channel governance.
const CHANNELS: &str = "channels";
/// Where the store keeps the state a device keeps beside its frames ([`Store::keep_state`]).
const STATE: &str = "state";

fn channels_dir(root: &Path, workspace: WorkspaceId) -> PathBuf {
    root.join(CHANNELS).join(workspace.to_string())
}

/// A workspace, a device of it, and where that device said it stands.
type HeldBy = (WorkspaceId, [u8; 32], Vec<Cursor>);

fn workspace_dir(root: &Path, workspace: WorkspaceId) -> PathBuf {
    root.join(WORKSPACES).join(workspace.to_string())
}

fn segment_path(root: &Path, workspace: WorkspaceId, number: u32, generation: u32) -> PathBuf {
    workspace_dir(root, workspace).join(format!("{number:08x}.{generation:08x}.{PACK}"))
}

/// The number and generation a segment's file is named by.
fn segment_name(path: &Path) -> Option<(u32, u32)> {
    if path.extension()? != PACK {
        return None;
    }
    let (number, generation) = path.file_stem()?.to_str()?.split_once('.')?;
    if number.len() != 8 || generation.len() != 8 {
        return None;
    }
    Some((
        u32::from_str_radix(number, 16).ok()?,
        u32::from_str_radix(generation, 16).ok()?,
    ))
}

fn holders_dir(root: &Path, workspace: WorkspaceId) -> PathBuf {
    root.join(HOLDERS).join(workspace.to_string())
}

fn holder_path(root: &Path, workspace: WorkspaceId, device: [u8; 32]) -> PathBuf {
    holders_dir(root, workspace).join(hex::encode(device))
}

/// `cursors` by lineage: the furthest each reaches.
fn holding(cursors: &[Cursor]) -> BTreeMap<Lineage, u64> {
    let mut holding = BTreeMap::new();
    for cursor in cursors {
        let after = holding.entry(cursor.lineage).or_insert(0);
        *after = cursor.after_seq.max(*after);
    }
    holding
}

/// The newest checkpoint among a lineage's `entries`.
fn newest_checkpoint(entries: &[Entry]) -> Option<u64> {
    entries
        .iter()
        .filter(|entry| entry.retention == ContentRetention::Checkpoint)
        .map(|entry| entry.seq)
        .max()
}

impl Unchecked {
    /// The sealed compaction `message`, a checkpoint frame, is, kept in the store under `lineage`
    /// with record check `check`; `None` for any other checkpoint.
    fn of(message: &NeighbourMsg, lineage: Lineage, check: [u8; 8]) -> Option<Self> {
        let NeighbourMsg::AppDocument {
            app_id,
            document_id,
            envelope,
            ..
        } = message
        else {
            return None;
        };
        let covers = covers_of(message);
        if covers.is_empty() {
            return None;
        }
        Some(Self {
            checkpoint: UncheckedCheckpoint {
                app: app_id.clone(),
                record: document_id.clone(),
                author: *envelope.author.as_bytes(),
                seq: envelope.seq,
            },
            lineage,
            check,
            covers,
        })
    }
}

/// What `frame`'s checkpoint signs it supersedes: each author with the
/// highest sequence number it covers, read from the checkpoint's cleartext
/// payload without opening it. Empty for a frame that is no sealed
/// compaction, which supersedes only its own author's earlier frames.
fn superseded_by(frame: &Frame) -> Vec<([u8; 32], u64)> {
    covers_of(&frame.message)
}

/// [`superseded_by`], of the frame's message.
fn covers_of(message: &NeighbourMsg) -> Vec<([u8; 32], u64)> {
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        return Vec::new();
    };
    collab_types::SealedCompaction::parse(envelope).map_or_else(
        |_| Vec::new(),
        |compaction| {
            compaction
                .covers
                .iter()
                .map(|(author, seq)| (*author.as_bytes(), *seq))
                .collect()
        },
    )
}

/// The frames `frame`, a checkpoint, supersedes as the index holds them
/// before it joins it: the writes each author it names reached, or its own
/// earlier frames when it names none.
fn superseded_entries(
    held: &Held,
    workspace: WorkspaceId,
    frame: &Frame,
) -> Vec<(Lineage, u64, [u8; 8])> {
    fn on_line(
        stored: &Workspace,
        lineage: Lineage,
        covered: impl Fn(u64) -> bool,
    ) -> Vec<(Lineage, u64, [u8; 8])> {
        stored
            .lineages
            .get(&lineage)
            .map_or_else(Vec::new, |entries| {
                entries
                    .iter()
                    .filter(|entry| covered(entry.seq))
                    .map(|entry| (lineage, entry.seq, entry.check))
                    .collect()
            })
    }
    let Some(stored) = held.workspaces.get(&workspace) else {
        return Vec::new();
    };
    let covers = superseded_by(frame);
    if covers.is_empty() {
        return on_line(stored, frame.lineage, |seq| seq < frame.seq);
    }
    covers
        .into_iter()
        .flat_map(|(author, through)| {
            let lineage = Lineage {
                record: frame.lineage.record,
                author,
            };
            on_line(stored, lineage, |seq| seq <= through)
        })
        .collect()
}

fn add(held: &mut Held, workspace: WorkspaceId, lineage: Lineage, entry: Entry) {
    let stored = held.workspaces.entry(workspace).or_default();
    let entries = stored.lineages.entry(lineage).or_default();
    let at = entries.partition_point(|held| held.seq <= entry.seq);
    entries.insert(at, entry);
    stored.bytes = stored.bytes.saturating_add(entry.bytes);
    held.bytes = held.bytes.saturating_add(entry.bytes);
}

/// Drop `entry` from the index, its record now dead in its segment.
fn unindex(held: &mut Held, workspace: WorkspaceId, lineage: Lineage, entry: Entry) {
    let Some(stored) = held.workspaces.get_mut(&workspace) else {
        return;
    };
    if let Some(entries) = stored.lineages.get_mut(&lineage) {
        entries.retain(|held| (held.seq, held.check) != (entry.seq, entry.check));
        if entries.is_empty() {
            stored.lineages.remove(&lineage);
        }
    }
    stored.retired.insert((entry.segment, entry.at));
    if let Some(segment) = stored.segments.get_mut(&entry.segment) {
        segment.dead = segment.dead.saturating_add(entry.record_len());
    }
    stored.bytes = stored.bytes.saturating_sub(entry.bytes);
    held.bytes = held.bytes.saturating_sub(entry.bytes);
}

/// Remove every frame of `lineage` before `seq` and report what went.
fn retire_before(
    disk: &Disk,
    workspace: WorkspaceId,
    lineage: Lineage,
    seq: u64,
    held: &mut Held,
) -> Retired {
    let mut retired = Retired::default();
    let entries = |held: &Held| {
        held.workspaces
            .get(&workspace)
            .and_then(|stored| stored.lineages.get(&lineage))
            .cloned()
            .unwrap_or_default()
    };
    let covered: Vec<(u64, [u8; 8])> = entries(held)
        .iter()
        .filter(|entry| entry.seq < seq)
        .map(|entry| (entry.seq, entry.check))
        .collect();
    for (seq, check) in covered {
        // Read again for each: writing a segment again moves the records
        // still in it.
        let Some(entry) = entries(held)
            .into_iter()
            .find(|entry| (entry.seq, entry.check) == (seq, check))
        else {
            continue;
        };
        if let Err(error) = retire(disk, workspace, lineage, entry, held, &mut retired) {
            tracing::warn!(%error, "a covered frame could not be retired");
        }
    }
    retired
}

/// Retire each frame in `covered`, read again before each: writing a segment
/// again moves the records still in it.
fn retire_covered(
    disk: &Disk,
    workspace: WorkspaceId,
    covered: &[(Lineage, u64, [u8; 8])],
    held: &mut Held,
) -> Retired {
    let mut retired = Retired::default();
    for (lineage, seq, check) in covered {
        let Some(entry) = held
            .workspaces
            .get(&workspace)
            .and_then(|stored| stored.lineages.get(lineage))
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.seq == *seq && entry.check == *check)
                    .copied()
            })
        else {
            continue;
        };
        if let Err(error) = retire(disk, workspace, *lineage, entry, held, &mut retired) {
            tracing::warn!(%error, "a covered frame could not be retired");
        }
    }
    retired
}

/// Retire `entry`: a record naming it is appended, synced with the next
/// write, and the segment holding it is written again once mostly retired.
fn retire(
    disk: &Disk,
    workspace: WorkspaceId,
    lineage: Lineage,
    entry: Entry,
    held: &mut Held,
    retired: &mut Retired,
) -> Result<(), StoreError> {
    let generation = held
        .workspaces
        .get(&workspace)
        .and_then(|stored| stored.segments.get(&entry.segment))
        .map_or(0, |segment| segment.generation);
    let record = retire_record(lineage, entry, generation)?;
    let (writer, _) = append(disk, workspace, held, &record)?;
    unindex(held, workspace, lineage, entry);
    if let Some(stored) = held.workspaces.get_mut(&workspace)
        && let Some(segment) = stored.segments.get_mut(&writer.segment)
    {
        segment.dead = segment.dead.saturating_add(RETIRE_LEN);
    }
    retired.frames = retired.frames.saturating_add(1);
    retired.bytes = retired.bytes.saturating_add(entry.bytes);
    if let Err(error) = rewrite(disk, workspace, held, entry.segment) {
        tracing::warn!(%error, segment = entry.segment, "a mostly retired segment was not written again; it is read as it is");
    }
    Ok(())
}

/// Whether the index serves the frame `seq` of `lineage` from `at` in
/// segment `number`.
fn serves(stored: &Workspace, lineage: Lineage, seq: u64, number: u32, at: u64) -> bool {
    stored.lineages.get(&lineage).is_some_and(|entries| {
        entries
            .iter()
            .any(|entry| entry.seq == seq && entry.segment == number && entry.at == at)
    })
}

/// Write `number` of `workspace` again with only the records it still needs,
/// when it is not the newest segment and most of it is retired: the frames
/// the index serves from it, and the records retiring frames that are still
/// in other segments. A segment left with none goes.
fn rewrite(disk: &Disk, workspace: WorkspaceId, held: &mut Held, number: u32) -> io::Result<()> {
    let root = &disk.root;
    let rewrite_bytes = held.limits.rewrite_bytes;
    let Some(stored) = held.workspaces.get_mut(&workspace) else {
        return Ok(());
    };
    let Some(segment) = stored.segments.get(&number).copied() else {
        return Ok(());
    };
    let newest = stored.segments.keys().next_back().copied();
    let freed = segment.dead.saturating_sub(segment.kept);
    if newest == Some(number)
        || stored.staged.values().any(|staged| *staged == number)
        || segment.len < rewrite_bytes
        || freed.saturating_mul(2) <= segment.len
    {
        return Ok(());
    }
    let path = segment_path(root, workspace, number, segment.generation);
    let bytes = disk.read(&path)?;
    let (found, _) = records(&bytes);
    let mut kept = Vec::new();
    let mut moved = BTreeMap::new();
    let mut kept_retirements = 0_u64;
    for (at, len, record) in &found {
        let keep = match record {
            Record::Frame { lineage, seq, .. } => serves(stored, *lineage, *seq, number, *at),
            Record::Retire {
                segment,
                generation,
                at,
                ..
            } => {
                *segment != number
                    && stored.retired.contains(&(*segment, *at))
                    && stored
                        .segments
                        .get(segment)
                        .is_some_and(|target| target.generation == *generation)
            }
        };
        if !keep {
            continue;
        }
        let start = usize::try_from(*at).unwrap_or(usize::MAX);
        let end = start.saturating_add(usize::try_from(*len).unwrap_or(usize::MAX));
        let Some(bytes) = bytes.get(start..end) else {
            continue;
        };
        if matches!(record, Record::Frame { .. }) {
            moved.insert(*at, kept.len() as u64);
        } else {
            kept_retirements = kept_retirements.saturating_add(*len);
        }
        kept.extend_from_slice(bytes);
    }
    let dir = workspace_dir(root, workspace);
    if kept.is_empty() {
        disk.remove(&path)?;
        stored.segments.remove(&number);
    } else {
        let generation = segment.generation.saturating_add(1);
        let again = segment_path(root, workspace, number, generation);
        let next = again.with_extension(NEXT);
        disk.create(&next, &kept)?;
        disk.rename(&next, &again)?;
        disk.remove(&path)?;
        stored.segments.insert(
            number,
            Segment {
                generation,
                len: kept.len() as u64,
                dead: kept_retirements,
                kept: kept_retirements,
            },
        );
    }
    disk.sync_dir(&dir)?;
    for entries in stored.lineages.values_mut() {
        for entry in entries.iter_mut().filter(|entry| entry.segment == number) {
            if let Some(at) = moved.get(&entry.at) {
                entry.at = *at;
            }
        }
    }
    stored.retired.retain(|(segment, _)| *segment != number);
    tracing::debug!(
        segment = number,
        before = segment.len,
        after = kept.len(),
        "a mostly retired segment was written again"
    );
    Ok(())
}

/// Retire frames until the store fits `appetite`: oldest first, and a
/// lineage's newest checkpoint last among its own frames.
fn retire_to_appetite(disk: &Disk, appetite: Appetite, held: &mut Held) {
    let Appetite::Bounded { max_bytes } = appetite else {
        return;
    };
    if held.bytes <= max_bytes {
        return;
    }
    let mut candidates = Vec::new();
    for (workspace, stored) in &held.workspaces {
        for (lineage, entries) in &stored.lineages {
            // The newest checkpoint of a lineage outlives everything the
            // store could otherwise drop: it is the state a later frame is
            // read against.
            let checkpoint = newest_checkpoint(entries);
            for entry in entries {
                if Some(entry.seq) == checkpoint {
                    continue;
                }
                candidates.push((entry.authored_at_ns, *workspace, *lineage, entry.seq));
            }
        }
    }
    candidates.sort_unstable();
    let mut retired = Retired::default();
    for (_, workspace, lineage, seq) in candidates {
        if held.bytes <= max_bytes {
            break;
        }
        let Some(entry) = held
            .workspaces
            .get(&workspace)
            .and_then(|stored| stored.lineages.get(&lineage))
            .and_then(|entries| entries.iter().find(|entry| entry.seq == seq))
            .copied()
        else {
            continue;
        };
        if let Err(error) = retire(disk, workspace, lineage, entry, held, &mut retired) {
            tracing::warn!(%error, "a frame the appetite retires could not be retired");
            break;
        }
    }
    if retired.frames > 0 {
        tracing::info!(
            frames = retired.frames,
            bytes = retired.bytes,
            max_bytes,
            "the store's appetite retired its oldest frames"
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;
    use collab_types::{
        ArtifactId, AttestationId, Ed25519PublicKey, EncryptionMetadata, OpKind, SealedCompaction,
        SignedOp, sign_op,
    };
    use ed25519_dalek::{Signer as _, SigningKey};
    use neighbour_wire::peer_handle;
    use std::fs;
    use std::time::Duration;
    use web_time::{SystemTime, UNIX_EPOCH};

    /// A directory of this device's disk, as a store lists it.
    fn on_disk(root: &Path) -> Disk {
        Disk {
            backend: Arc::new(FsBackend::new(root)),
            root: PathBuf::new(),
        }
    }

    /// The directories inside `dir`, sorted, or none when it does not exist.
    fn directories(dir: &Path) -> io::Result<Vec<PathBuf>> {
        on_disk(dir).directories(Path::new(""))
    }

    /// The files inside `dir`, sorted, or none when it does not exist.
    fn files(dir: &Path) -> io::Result<Vec<PathBuf>> {
        Ok(on_disk(dir)
            .files(Path::new(""))?
            .into_iter()
            .map(|file| dir.join(file))
            .collect())
    }

    fn workspace() -> WorkspaceId {
        WorkspaceId::from_u128(7)
    }

    const APP: &str = "test.app";
    const CLUSTER: &str = "workspace";
    const RECORD: &str = "tab:1";

    fn device(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn at(ns: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_nanos(ns)
    }

    /// A link's claims as version 6 wrote them: one run per shared action, and no call for the
    /// run or for the claim the op's signature covers.
    #[derive(serde::Serialize, serde::Deserialize)]
    struct BeforeCallRun {
        changes: Vec<[u8; 32]>,
        action: collab_types::ClaimedAction,
        units: Vec<collab_types::UnitPath>,
        invoker: Option<String>,
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct BeforeCallClaim {
        change: [u8; 32],
        action: collab_types::ClaimedAction,
        units: Vec<collab_types::UnitPath>,
        invoker: Option<String>,
    }

    /// What version 6's build signed: the op's fields, its claims naming no call.
    #[derive(serde::Serialize)]
    struct BeforeCallSigning {
        #[serde(with = "serde_bytes")]
        payload: Vec<u8>,
        artifact: ArtifactId,
        author: Ed25519PublicKey,
        attestation_id: AttestationId,
        kind: OpKind,
        timestamp: SystemTime,
        seq: u64,
        encryption: Option<EncryptionMetadata>,
        claims: Vec<BeforeCallClaim>,
        surface: Option<[u8; 32]>,
        governance: Vec<[u8; 4]>,
    }

    /// Version 6's signed op, as it kept it in a frame.
    #[derive(serde::Serialize, serde::Deserialize)]
    struct BeforeCallEnvelope {
        #[serde(with = "serde_bytes")]
        payload: Vec<u8>,
        artifact: ArtifactId,
        author: Ed25519PublicKey,
        attestation_id: AttestationId,
        kind: OpKind,
        timestamp: SystemTime,
        seq: u64,
        encryption: Option<EncryptionMetadata>,
        claims: Vec<BeforeCallRun>,
        surface: Option<[u8; 32]>,
        governance: Vec<[u8; 4]>,
        signature: collab_types::Signature,
    }

    /// Version 6's frame: one `AppDocument` as it wrote it, tagged as today tags it.
    #[derive(serde::Serialize, serde::Deserialize)]
    struct BeforeCallFrame {
        peer: neighbour_wire::PeerId,
        app_id: String,
        cluster_id: String,
        document_id: String,
        envelope: BeforeCallEnvelope,
        readers: Option<String>,
    }

    /// `key`'s version 6 frame, and the bytes its claim signature covers: built from version 6's
    /// layout, not from this build's types.
    fn before_call_frame(key: &SigningKey) -> (Vec<u8>, Vec<u8>) {
        let acting = tilo_member_types::Acting {
            by: tilo_member_types::Actor::member(tilo_member_types::MemberId::person("ana")),
            app: None,
            agent_session: None,
            cause: None,
            foreign_device: false,
            handed: false,
            acted_for: Vec::new(),
        };
        let command = serde_json::to_string(&acting).unwrap();
        let change = [9_u8; 32];
        let action = collab_types::ClaimedAction::whole_resource_edit(String::new());
        let units = vec![Vec::new()];
        let payload = vec![7_u8; 64];
        let artifact = ArtifactId::new();
        let author = Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
        let attestation_id = AttestationId::new();
        let timestamp = at(9);
        let encryption = Some(EncryptionMetadata::content_key(1, ContentRetention::Delta));
        let signed = postcard::to_allocvec(&BeforeCallSigning {
            payload: payload.clone(),
            artifact,
            author,
            attestation_id,
            kind: OpKind::Change,
            timestamp,
            seq: 9,
            encryption,
            claims: vec![BeforeCallClaim {
                change,
                action: action.clone(),
                units: units.clone(),
                invoker: Some(command.clone()),
            }],
            surface: None,
            governance: Vec::new(),
        })
        .unwrap();
        let signature = key.sign(&signed);
        let mut bytes = vec![NeighbourMsg::APP_DOCUMENT_TAG];
        bytes.extend(
            postcard::to_allocvec(&BeforeCallFrame {
                peer: peer_handle(&key.verifying_key().to_bytes()),
                app_id: APP.into(),
                cluster_id: CLUSTER.into(),
                document_id: RECORD.into(),
                envelope: BeforeCallEnvelope {
                    payload,
                    artifact,
                    author,
                    attestation_id,
                    kind: OpKind::Change,
                    timestamp,
                    seq: 9,
                    encryption,
                    claims: vec![BeforeCallRun {
                        changes: vec![change],
                        action,
                        units,
                        invoker: Some(command),
                    }],
                    surface: None,
                    governance: Vec::new(),
                    signature: collab_types::Signature::from_bytes(signature.to_bytes()),
                },
                readers: None,
            })
            .unwrap(),
        );
        (bytes, signed)
    }

    /// `bytes`, a frame as version 5 kept it naming no reader set ([`before_call_frame`]), as a
    /// store of `version` before 5 kept it: a document write named the member's own record it was
    /// after its envelope, and from version 2 on the reader set `readers` after that.
    fn before_own_removal(bytes: &[u8], version: u32, readers: Option<&str>) -> Vec<u8> {
        let mut kept = bytes.to_vec();
        assert_eq!(
            kept.pop(),
            Some(0),
            "a write naming no reader set ends with none"
        );
        kept.extend(postcard::to_allocvec(&Some(("notes.page".to_owned(), [7_u8; 32]))).unwrap());
        if version > 1 {
            kept.extend(postcard::to_allocvec(&readers).unwrap());
        }
        kept
    }

    /// Where a store kept `frame` of [`workspace`] in a file of its own, before segments.
    fn frame_file(root: &Path, frame: &Frame) -> PathBuf {
        workspace_dir(root, workspace())
            .join(hex::encode(frame.lineage.record))
            .join(hex::encode(frame.lineage.author))
            .join(format!("{:016x}.d", frame.seq))
    }

    /// A claim as a build wrote it before a migration step wrote its fields whole: the variant
    /// indices are `ClaimedAction`'s and `HistoryOp`'s own, and the step names the version it
    /// brings the record to and nothing after it.
    #[derive(serde::Serialize)]
    #[expect(
        dead_code,
        reason = "the variants before the step hold the indices it is written at"
    )]
    enum BeforeWholeAction {
        Verb,
        Edit,
        Group,
        History(BeforeWholeMigrate),
        Framework,
    }

    #[derive(serde::Serialize)]
    #[expect(
        dead_code,
        reason = "the variants before the migration hold the index it is written at"
    )]
    enum BeforeWholeMigrate {
        Undo,
        Redo,
        Restore,
        Checkpoint,
        Migrate { to: u32 },
    }

    #[derive(serde::Serialize)]
    struct BeforeWholeRun {
        changes: Vec<[u8; 32]>,
        action: BeforeWholeAction,
        units: Vec<collab_types::UnitPath>,
        invoker: Option<String>,
        call: Option<tilo_actions::remote::SignedInvocation>,
    }

    /// A claim as that build signed it: one change each, as an op's signing bytes write its claims,
    /// never as the runs its frame carries them in.
    #[derive(serde::Serialize)]
    struct BeforeWholeClaim {
        change: [u8; 32],
        action: BeforeWholeAction,
        units: Vec<collab_types::UnitPath>,
        invoker: Option<String>,
        call: Option<tilo_actions::remote::SignedInvocation>,
    }

    /// What the build that wrote the step signed: the op's fields, its claims written one change
    /// each as today's signing bytes write them, the step's `rewrite` left out.
    #[derive(serde::Serialize)]
    struct BeforeWholeSigning {
        #[serde(with = "serde_bytes")]
        payload: Vec<u8>,
        artifact: ArtifactId,
        author: Ed25519PublicKey,
        attestation_id: AttestationId,
        kind: OpKind,
        timestamp: SystemTime,
        seq: u64,
        encryption: Option<EncryptionMetadata>,
        claims: Vec<BeforeWholeClaim>,
        surface: Option<[u8; 32]>,
        governance: Vec<[u8; 4]>,
    }

    /// The signed op the build that wrote the step kept in its frame.
    #[derive(serde::Serialize)]
    struct BeforeWholeEnvelope {
        #[serde(with = "serde_bytes")]
        payload: Vec<u8>,
        artifact: ArtifactId,
        author: Ed25519PublicKey,
        attestation_id: AttestationId,
        kind: OpKind,
        timestamp: SystemTime,
        seq: u64,
        encryption: Option<EncryptionMetadata>,
        claims: Vec<BeforeWholeRun>,
        surface: Option<[u8; 32]>,
        governance: Vec<[u8; 4]>,
        signature: collab_types::Signature,
    }

    /// That build's frame: one `AppDocument` as it wrote it, tagged as today tags it.
    #[derive(serde::Serialize)]
    struct BeforeWholeFrame {
        peer: neighbour_wire::PeerId,
        app_id: String,
        cluster_id: String,
        document_id: String,
        envelope: BeforeWholeEnvelope,
        readers: Option<String>,
    }

    /// `key`'s frame as a build wrote it before the migration step wrote its `rewrite` whole:
    /// the step's `to` is written, its bool is not, and the units it writes follow. Built from
    /// that layout's types, not from this build's: what it signs is what this build writes for
    /// the claim it reads those bytes as.
    fn before_whole_migration_frame(key: &SigningKey) -> Vec<u8> {
        let change = [9_u8; 32];
        let to = 3_u32;
        let payload = vec![7_u8; 64];
        let artifact = ArtifactId::new();
        let author = Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
        let attestation_id = AttestationId::new();
        let timestamp = at(9);
        let encryption = Some(EncryptionMetadata::content_key(1, ContentRetention::Delta));
        let claim = BeforeWholeClaim {
            change,
            action: BeforeWholeAction::History(BeforeWholeMigrate::Migrate { to }),
            units: vec![Vec::new()],
            invoker: None,
            call: None,
        };
        let signed = postcard::to_allocvec(&BeforeWholeSigning {
            payload: payload.clone(),
            artifact,
            author,
            attestation_id,
            kind: OpKind::Change,
            timestamp,
            seq: 9,
            encryption,
            claims: vec![claim],
            surface: None,
            governance: Vec::new(),
        })
        .unwrap();
        let signature = key.sign(&signed);
        let mut bytes = vec![NeighbourMsg::APP_DOCUMENT_TAG];
        bytes.extend(
            postcard::to_allocvec(&BeforeWholeFrame {
                peer: peer_handle(&key.verifying_key().to_bytes()),
                app_id: APP.into(),
                cluster_id: CLUSTER.into(),
                document_id: RECORD.into(),
                envelope: BeforeWholeEnvelope {
                    payload,
                    artifact,
                    author,
                    attestation_id,
                    kind: OpKind::Change,
                    timestamp,
                    seq: 9,
                    encryption,
                    claims: vec![BeforeWholeRun {
                        changes: vec![change],
                        action: BeforeWholeAction::History(BeforeWholeMigrate::Migrate { to }),
                        units: vec![Vec::new()],
                        invoker: None,
                        call: None,
                    }],
                    surface: None,
                    governance: Vec::new(),
                    signature: collab_types::Signature::from_bytes(signature.to_bytes()),
                },
                readers: None,
            })
            .unwrap(),
        );
        bytes
    }

    /// The files of a store the released build kept, as its own store wrote them: three signed,
    /// sealed document writes of [`RECORD`] in [`workspace`], each one file, the first a
    /// checkpoint naming no own record and the others naming one, and no frames-version file.
    /// One block per file: its path in the store, then its bytes in hex.
    const RELEASED_STORE: &str = include_str!("store/fixtures/released-store.txt");

    /// A sealed document frame by `key` on `record`, with whatever ciphertext a
    /// store never opens.
    fn frame_of(key: &SigningKey, record: &str, seq: u64, retention: ContentRetention) -> Frame {
        let mut op = SignedOp::new_unsigned(
            vec![seq as u8; 64],
            ArtifactId::new(),
            Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
            AttestationId::new(),
            OpKind::Change,
            at(seq),
            seq,
        );
        op.encryption = Some(EncryptionMetadata::content_key(1, retention));
        sign_op(&mut op, key).unwrap();
        let message = NeighbourMsg::AppDocument {
            peer: peer_handle(&key.verifying_key().to_bytes()),
            app_id: APP.into(),
            cluster_id: CLUSTER.into(),
            document_id: record.into(),
            envelope: Box::new(op),
            readers: None,
        };
        Frame::of(&message).unwrap()
    }

    /// A sealed document frame by `key` on [`RECORD`].
    fn frame(key: &SigningKey, seq: u64, retention: ContentRetention) -> Frame {
        frame_of(key, RECORD, seq, retention)
    }

    /// A sealed compaction by `key` at `seq` that names `superseded`, with a
    /// ciphertext a store never opens.
    fn compaction(key: &SigningKey, seq: u64, superseded: &[Frame]) -> Frame {
        let ops: Vec<SignedOp> = superseded
            .iter()
            .map(|frame| match &frame.message {
                NeighbourMsg::AppDocument { envelope, .. } => (**envelope).clone(),
                other => panic!("a frame is a document write, not {other:?}"),
            })
            .collect();
        let payload = SealedCompaction::new(&ops, vec![seq as u8; 32])
            .unwrap()
            .encode()
            .unwrap();
        let mut op = SignedOp::new_unsigned(
            payload,
            ArtifactId::new(),
            Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
            AttestationId::new(),
            OpKind::Compaction,
            at(seq),
            seq,
        );
        op.encryption = Some(EncryptionMetadata::content_key(
            1,
            ContentRetention::Checkpoint,
        ));
        sign_op(&mut op, key).unwrap();
        let message = NeighbourMsg::AppDocument {
            peer: peer_handle(&key.verifying_key().to_bytes()),
            app_id: APP.into(),
            cluster_id: CLUSTER.into(),
            document_id: RECORD.into(),
            envelope: Box::new(op),
            readers: None,
        };
        Frame::of(&message).unwrap()
    }

    fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Appetite::StoreAll).unwrap();
        (dir, store)
    }

    fn cursor(lineage: Lineage, after_seq: u64) -> Cursor {
        Cursor { lineage, after_seq }
    }

    tilo_test_env::scenario! {
        fn replay_serves_a_records_frames_in_order_within_its_floor_and_budget;

        phase replay_continues_after_a_cursor;
        {
            let (_dir, store) = open();
            let key = device(2);
            let frames: Vec<Frame> = (1..=3)
                .map(|seq| frame(&key, seq, ContentRetention::Delta))
                .collect();
            for frame in &frames {
                assert_eq!(store.retain(workspace(), frame).unwrap(), Retained::New);
            }
            let lineage = frames[0].lineage;
            assert_eq!(
                store.stands(workspace()),
                vec![(cursor(lineage, 3), 1)],
                "one lineage holds the three frames"
            );

            let all = store.replay(workspace(), &[], Budget::default()).unwrap();
            assert_eq!(all.frames.len(), 3);
            assert!(all.floors.is_empty(), "nothing was retired");
            assert!(!all.more);

            let later = store
                .replay(workspace(), &[cursor(lineage, 2)], Budget::default())
                .unwrap();
            assert_eq!(later.frames, vec![frames[2].message.clone()]);

            assert_eq!(
                store.retain(workspace(), &frames[2]).unwrap(),
                Retained::Duplicate,
                "the same frame is kept once"
            );
            assert!(
                store
                    .replay(WorkspaceId::from_u128(8), &[], Budget::default())
                    .unwrap()
                    .frames
                    .is_empty()
            );
        }

        phase replay_record_serves_the_one_records_frames;
        // A device that opens a record is sent that record and no other, whatever else the
        // workspace keeps, and one whose round was cut short continues from what it was sent.
        {
            let (_dir, store) = open();
            let (founder, editor) = (device(2), device(3));
            let baseline = frame_of(&founder, RECORD, 1, ContentRetention::Checkpoint);
            let edit = frame_of(&editor, RECORD, 2, ContentRetention::Delta);
            let elsewhere = frame_of(&editor, "tab:other", 1, ContentRetention::Delta);
            store.retain(workspace(), &elsewhere).unwrap();
            store.retain(workspace(), &edit).unwrap();
            store.retain(workspace(), &baseline).unwrap();

            let channel = neighbour_wire::app_document_group(APP, RECORD);
            let replay = store
                .replay_record(workspace(), channel, &[], Budget::default())
                .unwrap();
            assert_eq!(
                replay.frames,
                vec![baseline.message.clone(), edit.message.clone()],
                "the record's own baseline before its changes, and nothing of another record"
            );
            assert!(
                store
                    .replay_record(
                        workspace(),
                        neighbour_wire::app_document_group(APP, "tab:unkept"),
                        &[],
                        Budget::default(),
                    )
                    .unwrap()
                    .frames
                    .is_empty(),
                "a record the store keeps nothing of is empty"
            );

            let cut = store
                .replay_record(
                    workspace(),
                    channel,
                    &[],
                    Budget {
                        max_frames: 1,
                        ..Budget::default()
                    },
                )
                .unwrap();
            assert!(cut.more, "the record holds more than the round carried");
            assert_eq!(cut.frames, vec![baseline.message]);
            let mut cursors = Vec::new();
            advance(&mut cursors, &cut.frames);
            let rest = store
                .replay_record(workspace(), channel, &cursors, Budget::default())
                .unwrap();
            assert_eq!(rest.frames, vec![edit.message]);
            assert!(!rest.more);
        }

        phase replay_sends_a_records_baseline_before_its_changes;
        // A member that catches up takes a change in only once its record holds a
        // baseline: the record's baseline is sent before any author's changes,
        // whichever author's line sorts first. A line sent whole before the
        // baseline's left the late member without that author's changes.
        {
            let (_dir, store) = open();
            let (mut founder, mut editor) = (device(2), device(3));
            if frame(&editor, 2, ContentRetention::Delta).lineage
                > frame(&founder, 1, ContentRetention::Checkpoint).lineage
            {
                // The editor's line sorts first, as a line sent whole once did.
                std::mem::swap(&mut founder, &mut editor);
            }
            let baseline = frame(&founder, 1, ContentRetention::Checkpoint);
            let edit = frame(&editor, 2, ContentRetention::Delta);
            assert!(edit.lineage < baseline.lineage);
            store.retain(workspace(), &edit).unwrap();
            store.retain(workspace(), &baseline).unwrap();

            let replay = store.replay(workspace(), &[], Budget::default()).unwrap();
            assert_eq!(replay.frames, vec![baseline.message, edit.message]);
        }

        phase replay_reports_a_floor_it_cannot_continue_from;
        {
            let (_dir, store) = open();
            let key = device(2);
            for seq in 1..=5 {
                store
                    .retain(workspace(), &frame(&key, seq, ContentRetention::Delta))
                    .unwrap();
            }
            let lineage = store
                .replay(workspace(), &[], Budget::default())
                .unwrap()
                .frames[0]
                .clone();
            let lineage = Frame::of(&lineage).unwrap().lineage;
            let retired = store.compact(workspace(), 4).unwrap();
            assert_eq!(retired.frames, 3, "the three oldest frames went");

            let early = store
                .replay(workspace(), &[cursor(lineage, 1)], Budget::default())
                .unwrap();
            assert_eq!(
                early.floors,
                vec![Floor {
                    lineage,
                    earliest_seq: 4
                }]
            );
            assert_eq!(gaps(&[cursor(lineage, 1)], &early).len(), 1);
            assert!(
                gaps(&[cursor(lineage, 3)], &early).is_empty(),
                "a cursor leading straight into the earliest frame has no gap"
            );
        }

        phase replay_stops_at_its_budget_and_says_more;
        {
            let (_dir, store) = open();
            let key = device(2);
            let frames: Vec<Frame> = (1..=5)
                .map(|seq| frame(&key, seq, ContentRetention::Delta))
                .collect();
            for frame in &frames {
                store.retain(workspace(), frame).unwrap();
            }
            let budget = Budget {
                max_frames: 2,
                max_bytes: 8 * 1024 * 1024,
            };
            let mut cursors = Vec::new();
            let mut rounds = 0;
            loop {
                let replay = store.replay(workspace(), &cursors, budget).unwrap();
                advance(&mut cursors, &replay.frames);
                rounds += 1;
                if !replay.more {
                    break;
                }
            }
            assert_eq!(rounds, 3, "five frames in rounds of two");
            assert_eq!(cursors, vec![cursor(frames[0].lineage, 5)]);
        }
    }

    tilo_test_env::scenario! {
        fn the_store_retires_only_what_a_checkpoint_a_bound_or_a_time_covers;

        phase a_checkpoint_that_names_no_author_retires_only_its_own_line;
        {
            let (_dir, store) = open();
            let author = device(2);
            let other = device(3);
            for seq in 1..=2 {
                store
                    .retain(workspace(), &frame(&author, seq, ContentRetention::Delta))
                    .unwrap();
            }
            let theirs = frame(&other, 1, ContentRetention::Delta);
            store.retain(workspace(), &theirs).unwrap();
            let checkpoint = frame(&author, 3, ContentRetention::Checkpoint);
            store.retain(workspace(), &checkpoint).unwrap();
            store
                .retain(workspace(), &frame(&author, 4, ContentRetention::Delta))
                .unwrap();

            let stands = store.stands(workspace());
            assert_eq!(
                stands,
                vec![
                    (cursor(checkpoint.lineage, 4), 3),
                    (cursor(theirs.lineage, 1), 1)
                ],
                "the checkpoint retired the author's earlier frames and nothing else"
            );
            let replay = store.replay(workspace(), &[], Budget::default()).unwrap();
            let seqs: Vec<u64> = replay
                .frames
                .iter()
                .map(|message| match message {
                    NeighbourMsg::AppDocument { envelope, .. } => envelope.seq,
                    other => panic!("a document frame, not {other:?}"),
                })
                .collect();
            assert_eq!(
                seqs,
                vec![3, 1, 4],
                "the baseline first, then the changes in the order they were written"
            );
        }

        phase a_sealed_compaction_retires_the_frames_it_names_of_every_author_once_checked;
        {
            let (dir, store) = open();
            let author = device(2);
            let other = device(3);
            let unnamed = device(4);
            let own = [
                frame(&author, 1, ContentRetention::Delta),
                frame(&author, 2, ContentRetention::Delta),
            ];
            let theirs = [
                frame(&other, 1, ContentRetention::Delta),
                frame(&other, 2, ContentRetention::Delta),
                frame(&other, 3, ContentRetention::Delta),
            ];
            let untouched = frame(&unnamed, 1, ContentRetention::Delta);
            for frame in own.iter().chain(&theirs).chain([&untouched]) {
                store.retain(workspace(), frame).unwrap();
            }
            // A sealed compaction is numbered at the last write it folds, so
            // it shares that write's sequence number on its author's line.
            let compaction = compaction(
                &author,
                2,
                &[own[0].clone(), own[1].clone(), theirs[0].clone(), theirs[1].clone()],
            );
            store.retain(workspace(), &compaction).unwrap();

            // The compaction is a cache of the writes it names: they stay, through a restart too,
            // until every device with the record's keys checked it against them.
            let checkpoint = UncheckedCheckpoint {
                app: APP.into(),
                record: RECORD.into(),
                author: author.verifying_key().to_bytes(),
                seq: 2,
            };
            assert_eq!(store.unchecked(workspace()), vec![checkpoint.clone()]);
            let mut expected = vec![
                (cursor(compaction.lineage, 2), 1),
                (cursor(theirs[2].lineage, 3), 1),
                (cursor(untouched.lineage, 1), 1),
            ];
            expected.sort();
            let mut stands = store.stands(workspace());
            stands.sort();
            assert_eq!(stands, expected, "no write the compaction names is retired unchecked");
            drop(store);
            let store = Store::open(dir.path(), Appetite::StoreAll).unwrap();
            assert_eq!(
                store.unchecked(workspace()),
                vec![checkpoint.clone()],
                "a restart keeps the writes of a compaction no one checked yet"
            );
            let mut stands = store.stands(workspace());
            stands.sort();
            assert_eq!(stands, expected, "a restart retires none of them either");

            let retired = store.retire_checked(workspace(), &checkpoint);
            assert_eq!(retired.frames, 4, "the four writes it names go once it is checked");
            assert!(store.unchecked(workspace()).is_empty());

            let stands = store.stands(workspace());
            assert_eq!(stands.len(), 3, "one line per author still standing");
            assert!(
                stands.contains(&(cursor(compaction.lineage, 2), 2)),
                "the compaction stands in place of the last write it folds"
            );
            assert!(
                stands.contains(&(cursor(theirs[2].lineage, 3), 3)),
                "a named author's write after the floor stays"
            );
            assert!(
                stands.contains(&(cursor(untouched.lineage, 1), 1)),
                "an author the compaction does not name keeps its writes"
            );
        }

        phase a_bounded_appetite_retires_the_oldest_and_keeps_a_checkpoint;
        {
            let (dir, store) = open();
            let key = device(2);
            for seq in 1..=2 {
                store
                    .retain(workspace(), &frame(&key, seq, ContentRetention::Delta))
                    .unwrap();
            }
            let checkpoint = frame(&key, 3, ContentRetention::Checkpoint);
            store.retain(workspace(), &checkpoint).unwrap();
            let size = store.bytes();
            let kept = postcard::to_allocvec(&checkpoint.message).unwrap().len() as u64;
            assert!(size >= kept);

            // Reopening under an appetite of exactly the checkpoint keeps that
            // and retires everything else, since a checkpoint is the state a
            // later frame is read against.
            let reopened = Store::open(dir.path(), Appetite::Bounded { max_bytes: kept }).unwrap();
            assert_eq!(reopened.bytes(), kept);
            assert_eq!(
                reopened.stands(workspace()),
                vec![(cursor(checkpoint.lineage, 3), 3)]
            );
            let replay = reopened.replay(workspace(), &[], Budget::default()).unwrap();
            assert_eq!(replay.frames, vec![checkpoint.message]);
        }

        phase compaction_retires_the_frames_a_time_covers;
        {
            let (_dir, store) = open();
            let key = device(2);
            for seq in 1..=3 {
                store
                    .retain(workspace(), &frame(&key, seq, ContentRetention::Delta))
                    .unwrap();
            }
            let retired = store.compact(workspace(), 3).unwrap();
            assert_eq!(retired.frames, 2);
            assert!(retired.bytes > 0);
            assert_eq!(store.stands(workspace()).len(), 1);
            assert_eq!(
                store.compact(WorkspaceId::from_u128(8), u64::MAX).unwrap(),
                Retired::default()
            );
        }

        phase compaction_keeps_what_a_holder_still_lacks;
        // Two holders said what they hold; compacting retires only what both
        // hold, also after the store restarts, and never the newest
        // checkpoint a newcomer starts from. Once the one behind catches up,
        // the rest goes.
        {
            let (dir, store) = open();
            let key = device(2);
            store
                .retain(workspace(), &frame(&key, 1, ContentRetention::Checkpoint))
                .unwrap();
            let mut lineage = None;
            for seq in 2..=5 {
                let frame = frame(&key, seq, ContentRetention::Delta);
                lineage = Some(frame.lineage);
                store.retain(workspace(), &frame).unwrap();
            }
            let lineage = lineage.unwrap();
            store
                .holds(workspace(), [10; 32], &[cursor(lineage, 5)])
                .unwrap();
            store
                .holds(workspace(), [11; 32], &[cursor(lineage, 3)])
                .unwrap();
            drop(store);
            let store = Store::open(dir.path(), Appetite::StoreAll).unwrap();
            let retired = store.compact(workspace(), u64::MAX).unwrap();
            assert_eq!(retired.frames, 2, "the two deltas both holders hold went");
            assert_eq!(store.stands(workspace()), vec![(cursor(lineage, 5), 1)]);
            let behind = store
                .replay(workspace(), &[cursor(lineage, 3)], Budget::default())
                .unwrap();
            assert_eq!(behind.frames.len(), 2, "the holder behind still gets 4 and 5");
            assert!(behind.floors.is_empty());

            store
                .holds(workspace(), [11; 32], &[cursor(lineage, 5)])
                .unwrap();
            assert_eq!(store.compact(workspace(), u64::MAX).unwrap().frames, 2);
            assert_eq!(
                store.stands(workspace()),
                vec![(cursor(lineage, 1), 1)],
                "the newest checkpoint stays"
            );
        }

        phase forgetting_a_workspace_removes_its_frames_and_files;
        // Forgetting a workspace ends everything the store keeps of it, in one
        // pass: its frames, its bytes and its files, and no reopen reads it
        // again. The store's other workspaces are untouched.
        {
            let (dir, store) = open();
            let key = device(2);
            let other = WorkspaceId::from_u128(9);
            for seq in 1..=3 {
                store
                    .retain(workspace(), &frame(&key, seq, ContentRetention::Delta))
                    .unwrap();
                store
                    .retain(other, &frame(&key, seq, ContentRetention::Delta))
                    .unwrap();
            }
            let size = store.bytes();
            let forgotten = store.bytes_of(workspace());
            let retired = store.forget(workspace()).unwrap();
            assert_eq!(retired.frames, 3, "every frame it held went");
            assert_eq!(retired.bytes, forgotten);
            assert_eq!(
                store.bytes(),
                size - forgotten,
                "the other workspace's bytes stay"
            );
            assert!(
                !workspace_dir(dir.path(), workspace()).exists(),
                "its directory went whole"
            );
            assert!(
                store
                    .replay(workspace(), &[], Budget::default())
                    .unwrap()
                    .frames
                    .is_empty()
            );
            assert_eq!(
                store
                    .replay(other, &[], Budget::default())
                    .unwrap()
                    .frames
                    .len(),
                3,
                "the other workspace still replays"
            );
            assert_eq!(store.forget(workspace()).unwrap(), Retired::default());
            drop(store);

            let reopened = Store::open(dir.path(), Appetite::StoreAll).unwrap();
            assert!(
                reopened
                    .replay(workspace(), &[], Budget::default())
                    .unwrap()
                    .frames
                    .is_empty(),
                "a reopened store does not read a forgotten workspace back"
            );
        }
    }

    tilo_test_env::scenario! {
        fn the_store_keeps_and_serves_only_the_signed_frames_it_acknowledged;

        phase reopening_reads_back_what_the_store_acknowledged;
        {
            let (dir, store) = open();
            let key = device(2);
            let frames: Vec<Frame> = (1..=3)
                .map(|seq| frame(&key, seq, ContentRetention::Delta))
                .collect();
            for frame in &frames {
                store.retain(workspace(), frame).unwrap();
            }
            let bytes = store.bytes();
            drop(store);

            let reopened = Store::open(dir.path(), Appetite::StoreAll).unwrap();
            assert_eq!(reopened.bytes(), bytes);
            let replay = reopened
                .replay(workspace(), &[], Budget::default())
                .unwrap();
            assert_eq!(replay.frames.len(), 3);
        }

        phase a_file_that_is_not_a_frame_is_not_served;
        {
            let (dir, store) = open();
            let key = device(2);
            let frame = frame(&key, 1, ContentRetention::Delta);
            store.retain(workspace(), &frame).unwrap();
            let stray = workspace_dir(dir.path(), workspace()).join("stray");
            fs::write(&stray, b"not a frame").unwrap();
            drop(store);

            let reopened = Store::open(dir.path(), Appetite::StoreAll).unwrap();
            let replay = reopened
                .replay(workspace(), &[], Budget::default())
                .unwrap();
            assert_eq!(replay.frames.len(), 1, "the stray file is skipped");
            assert!(stray.exists(), "and left on disk for an operator");
        }

        phase only_signed_sealed_document_frames_are_kept;
        {
            let key = device(2);
            assert_eq!(
                Frame::of(&NeighbourMsg::Heartbeat { peer: 0, ts: 0 }),
                Err(Refusal::NotADocument)
            );

            let mut unsigned = SignedOp::new_unsigned(
                vec![1; 8],
                ArtifactId::new(),
                Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
                AttestationId::new(),
                OpKind::Change,
                at(1),
                1,
            );
            unsigned.encryption = Some(EncryptionMetadata::content_key(1, ContentRetention::Delta));
            let message = NeighbourMsg::AppDocument {
                peer: 0,
                app_id: APP.into(),
                cluster_id: CLUSTER.into(),
                document_id: RECORD.into(),
                envelope: Box::new(unsigned),
                readers: None,
            };
            assert_eq!(Frame::of(&message), Err(Refusal::Unsigned));

            let mut cleartext = SignedOp::new_unsigned(
                vec![1; 8],
                ArtifactId::new(),
                Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
                AttestationId::new(),
                OpKind::Change,
                at(1),
                1,
            );
            sign_op(&mut cleartext, &key).unwrap();
            let message = NeighbourMsg::AppDocument {
                peer: 0,
                app_id: APP.into(),
                cluster_id: CLUSTER.into(),
                document_id: RECORD.into(),
                envelope: Box::new(cleartext),
                readers: None,
            };
            assert_eq!(Frame::of(&message), Err(Refusal::NotSealed));

            let mut mls = SignedOp::new_unsigned(
                vec![1; 8],
                ArtifactId::new(),
                Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
                AttestationId::new(),
                OpKind::Change,
                at(1),
                1,
            );
            mls.encryption = Some(EncryptionMetadata::mls_application(1));
            sign_op(&mut mls, &key).unwrap();
            let message = NeighbourMsg::AppDocument {
                peer: 0,
                app_id: APP.into(),
                cluster_id: CLUSTER.into(),
                document_id: RECORD.into(),
                envelope: Box::new(mls),
                readers: None,
            };
            assert_eq!(Frame::of(&message), Err(Refusal::NotSealed));
        }
    }

    /// A store with small segments, so a test fills and retires several.
    fn open_small(root: &Path) -> Store {
        Store::open_with(
            on_disk(root),
            Appetite::StoreAll,
            Limits {
                segment_bytes: 4 * 1024,
                rewrite_bytes: 1024,
            },
        )
        .unwrap()
    }

    fn seqs(replay: &Replay) -> Vec<u64> {
        replay
            .frames
            .iter()
            .map(|message| match message {
                NeighbourMsg::AppDocument { envelope, .. } => envelope.seq,
                other => panic!("a document frame, not {other:?}"),
            })
            .collect()
    }

    fn pack_files(root: &Path) -> Vec<PathBuf> {
        files(&workspace_dir(root, workspace()))
            .unwrap()
            .into_iter()
            .filter(|file| segment_name(file).is_some())
            .collect()
    }

    tilo_test_env::scenario! {
        fn segments_hold_what_the_store_acknowledged_and_free_what_it_retired;

        phase writes_fill_one_segment_after_another_and_read_back_after_a_restart;
        {
            let dir = tempfile::tempdir().unwrap();
            let store = open_small(dir.path());
            let key = device(2);
            for seq in 1..=60 {
                store
                    .retain(workspace(), &frame(&key, seq, ContentRetention::Delta))
                    .unwrap();
            }
            let segments = store.segments(workspace());
            assert!(segments.len() > 2, "the writes filled several segments: {segments:?}");
            assert_eq!(pack_files(dir.path()).len(), segments.len(), "one file per segment");
            let bytes = store.bytes();
            drop(store);

            let reopened = open_small(dir.path());
            assert_eq!(reopened.bytes(), bytes);
            let replay = reopened.replay(workspace(), &[], Budget { max_frames: 100, ..Budget::default() }).unwrap();
            assert_eq!(seqs(&replay), (1..=60).collect::<Vec<_>>());
        }

        phase a_mostly_retired_segment_is_written_again_with_what_it_still_serves;
        {
            let dir = tempfile::tempdir().unwrap();
            let store = open_small(dir.path());
            let author = device(2);
            let other = device(3);
            // The other author's frames sit between the first author's, in
            // the same segments; the first author's checkpoint retires its own.
            for seq in 1..=40 {
                store
                    .retain(workspace(), &frame(&author, seq, ContentRetention::Delta))
                    .unwrap();
                if seq % 8 == 0 {
                    store
                        .retain(workspace(), &frame(&other, seq / 8, ContentRetention::Delta))
                        .unwrap();
                }
            }
            let on_disk = |root: &Path| -> u64 {
                pack_files(root)
                    .iter()
                    .map(|file| fs::metadata(file).unwrap().len())
                    .sum()
            };
            let before = on_disk(dir.path());
            store
                .retain(workspace(), &frame(&author, 41, ContentRetention::Checkpoint))
                .unwrap();
            let after = on_disk(dir.path());
            assert!(after * 2 < before, "the retired frames' bytes are freed: {before} then {after}");
            let replay = store.replay(workspace(), &[], Budget::default()).unwrap();
            assert_eq!(seqs(&replay), vec![41, 1, 2, 3, 4, 5]);
            drop(store);

            let reopened = open_small(dir.path());
            let again = reopened.replay(workspace(), &[], Budget::default()).unwrap();
            assert_eq!(again.frames, replay.frames, "a restart reads back what it served");
        }

        phase a_record_a_crash_cut_short_is_cut_off;
        {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(dir.path(), Appetite::StoreAll).unwrap();
            let key = device(2);
            for seq in 1..=3 {
                store
                    .retain(workspace(), &frame(&key, seq, ContentRetention::Delta))
                    .unwrap();
            }
            drop(store);
            let segment = pack_files(dir.path()).pop().unwrap();
            let whole = fs::metadata(&segment).unwrap().len();
            let mut torn = fs::read(&segment).unwrap();
            let next = frame_record(&frame(&key, 4, ContentRetention::Delta), &[7; 300]).unwrap();
            torn.extend_from_slice(&next[..next.len() / 2]);
            fs::write(&segment, &torn).unwrap();

            let reopened = Store::open(dir.path(), Appetite::StoreAll).unwrap();
            assert_eq!(fs::metadata(&segment).unwrap().len(), whole, "the cut record is cut off");
            assert_eq!(
                seqs(&reopened.replay(workspace(), &[], Budget::default()).unwrap()),
                vec![1, 2, 3]
            );
            reopened
                .retain(workspace(), &frame(&key, 4, ContentRetention::Delta))
                .unwrap();
            drop(reopened);
            let again = Store::open(dir.path(), Appetite::StoreAll).unwrap();
            assert_eq!(
                seqs(&again.replay(workspace(), &[], Budget::default()).unwrap()),
                vec![1, 2, 3, 4],
                "the next write lands where the cut one began"
            );
        }

        phase a_store_of_version_7_is_relabelled_and_keeps_its_segments;
        {
            // Version 7's frames read as this build's: the store records the newer version and
            // serves them from the segments they are in, without packing them again.
            let dir = tempfile::tempdir().unwrap();
            let key = device(5);
            let frames: Vec<Frame> = (1..=3)
                .map(|seq| frame(&key, seq, ContentRetention::Delta))
                .collect();
            let mut segment = Vec::new();
            for frame in &frames {
                segment.extend(
                    frame_record(frame, &postcard::to_allocvec(&frame.message).unwrap()).unwrap(),
                );
            }
            let path = segment_path(dir.path(), workspace(), 0, 0);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &segment).unwrap();
            fs::write(dir.path().join(FRAMES_VERSION_FILE), b"7").unwrap();

            let store = Store::open(dir.path(), Appetite::StoreAll).unwrap();
            assert_eq!(
                store.replay(workspace(), &[], Budget::default()).unwrap().frames,
                frames.iter().map(|frame| frame.message.clone()).collect::<Vec<_>>(),
                "every frame of version 7 is served"
            );
            assert_eq!(
                fs::read_to_string(dir.path().join(FRAMES_VERSION_FILE)).unwrap(),
                FRAMES_VERSION.to_string()
            );
            assert_eq!(
                fs::read(&path).unwrap(),
                segment,
                "the segment is kept as version 7 wrote it"
            );
        }

        phase a_store_of_version_8_is_relabelled_and_keeps_its_segments;
        {
            // Version 8's frames read as this build's: the messages appended since take
            // discriminants past every frame version 8 wrote and leave its fields as they were,
            // so the store records the newer version and serves them from the segments they are
            // in, without packing them again.
            let dir = tempfile::tempdir().unwrap();
            let key = device(5);
            let frames: Vec<Frame> = (1..=3)
                .map(|seq| frame(&key, seq, ContentRetention::Delta))
                .collect();
            let mut segment = Vec::new();
            for frame in &frames {
                segment.extend(
                    frame_record(frame, &postcard::to_allocvec(&frame.message).unwrap()).unwrap(),
                );
            }
            let path = segment_path(dir.path(), workspace(), 0, 0);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &segment).unwrap();
            fs::write(dir.path().join(FRAMES_VERSION_FILE), b"8").unwrap();

            let store = Store::open(dir.path(), Appetite::StoreAll).unwrap();
            assert_eq!(
                store.replay(workspace(), &[], Budget::default()).unwrap().frames,
                frames.iter().map(|frame| frame.message.clone()).collect::<Vec<_>>(),
                "every frame of version 8 is served"
            );
            assert_eq!(
                fs::read_to_string(dir.path().join(FRAMES_VERSION_FILE)).unwrap(),
                FRAMES_VERSION.to_string()
            );
            assert_eq!(
                fs::read(&path).unwrap(),
                segment,
                "the segment is kept as version 8 wrote it"
            );
        }

        phase a_store_of_version_10_is_relabelled_and_keeps_its_segments;
        {
            // Version 10's frames read as this build's: a certificate an identity hello names
            // reads as it was, and only an agent's grant naming its agent by its id, which no
            // frame of version 10 holds, is new.
            let dir = tempfile::tempdir().unwrap();
            let key = device(5);
            let frames: Vec<Frame> = (1..=3)
                .map(|seq| frame(&key, seq, ContentRetention::Delta))
                .collect();
            let mut segment = Vec::new();
            for frame in &frames {
                segment.extend(
                    frame_record(frame, &postcard::to_allocvec(&frame.message).unwrap()).unwrap(),
                );
            }
            let path = segment_path(dir.path(), workspace(), 0, 0);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &segment).unwrap();
            fs::write(dir.path().join(FRAMES_VERSION_FILE), b"10").unwrap();

            let store = Store::open(dir.path(), Appetite::StoreAll).unwrap();
            assert_eq!(
                store.replay(workspace(), &[], Budget::default()).unwrap().frames,
                frames.iter().map(|frame| frame.message.clone()).collect::<Vec<_>>(),
                "every frame of version 10 is served"
            );
            assert_eq!(
                fs::read_to_string(dir.path().join(FRAMES_VERSION_FILE)).unwrap(),
                FRAMES_VERSION.to_string()
            );
            assert_eq!(fs::read(&path).unwrap(), segment, "the segment is kept as it was");
        }

        phase a_store_of_version_11_is_relabelled_and_keeps_its_segments;
        {
            // Version 11's frames are this build's bytes: the types their claims name moved
            // below the policy, which changes none of them, so a store of version 11 records the
            // newer version and serves its frames from the segments they are in.
            let dir = tempfile::tempdir().unwrap();
            let key = device(5);
            let frames: Vec<Frame> = (1..=3)
                .map(|seq| frame(&key, seq, ContentRetention::Delta))
                .collect();
            let mut segment = Vec::new();
            for frame in &frames {
                segment.extend(
                    frame_record(frame, &postcard::to_allocvec(&frame.message).unwrap()).unwrap(),
                );
            }
            let path = segment_path(dir.path(), workspace(), 0, 0);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &segment).unwrap();
            fs::write(dir.path().join(FRAMES_VERSION_FILE), b"11").unwrap();

            let store = Store::open(dir.path(), Appetite::StoreAll).unwrap();
            assert_eq!(
                store.replay(workspace(), &[], Budget::default()).unwrap().frames,
                frames.iter().map(|frame| frame.message.clone()).collect::<Vec<_>>(),
                "every frame of version 11 is served"
            );
            assert_eq!(
                fs::read_to_string(dir.path().join(FRAMES_VERSION_FILE)).unwrap(),
                FRAMES_VERSION.to_string()
            );
            assert_eq!(fs::read(&path).unwrap(), segment, "the segment is kept as it was");
        }

        phase frames_kept_before_claims_named_a_call_are_kept_as_their_authors_signed_them;
        {
            // Versions 5 and 6 wrote each claim without a call, and their authors signed exactly
            // those claim bytes. The store writes such a frame again as this build writes it,
            // once, and the write still verifies as its author signed it: a device that is the
            // only one keeping a workspace loses none of its writes.
            for version in [5_u32, 6] {
                let dir = tempfile::tempdir().unwrap();
                let key = device(4);
                let (bytes, _) = before_call_frame(&key);
                let header = frame(&key, 9, ContentRetention::Delta);
                let path = segment_path(dir.path(), workspace(), 0, 0);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, frame_record(&header, &bytes).unwrap()).unwrap();
                fs::write(dir.path().join(FRAMES_VERSION_FILE), version.to_string()).unwrap();

                let store = Store::open(dir.path(), Appetite::StoreAll).unwrap();
                let replay = store.replay(workspace(), &[], Budget::default()).unwrap();
                assert_eq!(replay.frames.len(), 1, "version {version}: the frame is served");
                let NeighbourMsg::AppDocument { envelope, document_id, .. } = &replay.frames[0]
                else {
                    panic!("version {version}: the frame is a document write");
                };
                assert_eq!(document_id, RECORD);
                assert_eq!(envelope.claims.len(), 1);
                assert!(envelope.claims[0].call.is_none());
                collab_types::verify_op(envelope)
                    .expect("the write verifies as its author signed it");
                assert_eq!(
                    fs::read_to_string(dir.path().join(FRAMES_VERSION_FILE)).unwrap(),
                    FRAMES_VERSION.to_string()
                );
                assert!(!dir.path().join(EARLIER_SEGMENTS).exists(), "what was set aside is gone");
                drop(store);
                let again = Store::open(dir.path(), Appetite::StoreAll).unwrap();
                assert_eq!(
                    again.replay(workspace(), &[], Budget::default()).unwrap().frames,
                    replay.frames,
                    "version {version}: it is written again once"
                );
            }
        }

        phase frames_kept_before_own_records_went_are_kept_as_their_authors_signed_them;
        {
            // Up to version 4 a document write named the member's own record it was: version 1,
            // which the released build keeps without a version file, ended the write with it,
            // and versions 2 to 4 put the reader set after it. Up to version 3 each frame is a
            // file of its own, and version 4 keeps them in segments. Each comes up as version 5
            // kept it and is written again as this build writes it, once, still verifying as its
            // author signed it.
            for version in 1_u32..=4 {
                let dir = tempfile::tempdir().unwrap();
                let key = device(4);
                let (bytes, _) = before_call_frame(&key);
                let readers = (version > 1).then_some("readers:ab");
                let kept = before_own_removal(&bytes, version, readers);
                let header = frame(&key, 9, ContentRetention::Delta);
                if FRAME_FILES.contains(&version) {
                    let path = frame_file(dir.path(), &header);
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    fs::write(&path, &kept).unwrap();
                } else {
                    let path = segment_path(dir.path(), workspace(), 0, 0);
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    fs::write(&path, frame_record(&header, &kept).unwrap()).unwrap();
                }
                if version > 1 {
                    fs::write(dir.path().join(FRAMES_VERSION_FILE), version.to_string()).unwrap();
                }

                let store = Store::open(dir.path(), Appetite::StoreAll).unwrap();
                let replay = store.replay(workspace(), &[], Budget::default()).unwrap();
                assert_eq!(replay.frames.len(), 1, "version {version}: the frame is served");
                let NeighbourMsg::AppDocument { envelope, document_id, readers: named, .. } =
                    &replay.frames[0]
                else {
                    panic!("version {version}: the frame is a document write");
                };
                assert_eq!(document_id, RECORD);
                assert_eq!(named.as_deref(), readers, "version {version}: its reader set");
                assert_eq!(envelope.claims.len(), 1);
                collab_types::verify_op(envelope)
                    .expect("the write verifies as its author signed it");
                assert_eq!(
                    fs::read_to_string(dir.path().join(FRAMES_VERSION_FILE)).unwrap(),
                    FRAMES_VERSION.to_string()
                );
                assert!(!dir.path().join(EARLIER_SEGMENTS).exists(), "what was set aside is gone");
                assert!(
                    directories(&workspace_dir(dir.path(), workspace())).unwrap().is_empty(),
                    "version {version}: no frame is kept in a file of its own"
                );
                assert_eq!(pack_files(dir.path()).len(), 1, "version {version}: one segment");
                drop(store);
                let again = Store::open(dir.path(), Appetite::StoreAll).unwrap();
                assert_eq!(
                    again.replay(workspace(), &[], Budget::default()).unwrap().frames,
                    replay.frames,
                    "version {version}: it is written again once"
                );
            }
        }

        phase a_store_the_released_build_kept_opens_with_every_frame;
        {
            // The released build kept each frame in a file of its own and recorded no version.
            // Its store's files open with every frame, each as its author signed it.
            let dir = tempfile::tempdir().unwrap();
            for file in RELEASED_STORE.split("\n\n") {
                let (path, bytes) = file.split_once('\n').unwrap();
                let path = dir.path().join(path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                let bytes: String = bytes.split_whitespace().collect();
                fs::write(&path, hex::decode(bytes).unwrap()).unwrap();
            }
            assert!(!dir.path().join(FRAMES_VERSION_FILE).exists());

            let store = Store::open(dir.path(), Appetite::StoreAll).unwrap();
            let replay = store.replay(workspace(), &[], Budget::default()).unwrap();
            let mut authored = Vec::new();
            for message in &replay.frames {
                let NeighbourMsg::AppDocument { app_id, document_id, envelope, readers, .. } =
                    message
                else {
                    panic!("a document write, not {message:?}");
                };
                assert_eq!((app_id.as_str(), document_id.as_str()), (APP, RECORD));
                assert_eq!(*readers, None, "the released build named no reader set");
                assert_eq!(envelope.claims.len(), 1);
                assert!(envelope.claims[0].invoker.is_some(), "the claim names who made it");
                collab_types::verify_op(envelope)
                    .expect("the write verifies as its author signed it");
                authored.push((*envelope.author.as_bytes(), envelope.seq));
            }
            authored.sort_unstable();
            let author = |seed| device(seed).verifying_key().to_bytes();
            let mut kept = vec![(author(2), 1), (author(2), 2), (author(3), 1)];
            kept.sort_unstable();
            assert_eq!(authored, kept, "every frame the released build kept is served");
            assert_eq!(
                fs::read_to_string(dir.path().join(FRAMES_VERSION_FILE)).unwrap(),
                FRAMES_VERSION.to_string()
            );
            assert!(!dir.path().join(EARLIER_SEGMENTS).exists(), "what was set aside is gone");
            assert!(directories(&workspace_dir(dir.path(), workspace())).unwrap().is_empty());
            drop(store);
            let again = Store::open(dir.path(), Appetite::StoreAll).unwrap();
            assert_eq!(again.replay(workspace(), &[], Budget::default()).unwrap().frames, replay.frames);
        }

        phase a_frame_that_does_not_come_up_refuses_the_store_by_name_and_is_kept;
        {
            // A store's history may be held nowhere else: a frame it cannot bring up refuses the
            // store, by the file it is in, rather than going, and every frame stays set aside
            // for a build that brings it up.
            let dir = tempfile::tempdir().unwrap();
            let key = device(4);
            let (bytes, _) = before_call_frame(&key);
            let kept = before_own_removal(&bytes, 1, None);
            let header = frame(&key, 9, ContentRetention::Delta);
            let good = frame_file(dir.path(), &header);
            let bad = good.with_file_name(format!("{:016x}.d", 10));
            fs::create_dir_all(good.parent().unwrap()).unwrap();
            fs::write(&good, &kept).unwrap();
            fs::write(&bad, b"not a frame").unwrap();
            let set_aside = |path: &Path| {
                dir.path()
                    .join(EARLIER_SEGMENTS)
                    .join(path.strip_prefix(dir.path()).unwrap())
            };

            for attempt in 0..2 {
                let Err(error) = Store::open(dir.path(), Appetite::StoreAll) else {
                    panic!("attempt {attempt}: a store with a frame that does not come up opens");
                };
                let error = error.to_string();
                let named = Path::new(EARLIER_SEGMENTS).join(bad.strip_prefix(dir.path()).unwrap());
                assert!(
                    error.contains(&named.display().to_string()),
                    "attempt {attempt}: the refusal names the frame: {error}"
                );
                assert!(!dir.path().join(FRAMES_VERSION_FILE).exists(), "no version is recorded");
                assert_eq!(fs::read(set_aside(&good)).unwrap(), kept, "every frame is kept");
                assert_eq!(fs::read(set_aside(&bad)).unwrap(), b"not a frame");
                assert!(pack_files(dir.path()).is_empty(), "nothing is served in its place");
            }
        }

        phase a_frame_a_migration_step_was_written_without_its_bool_is_refused_by_its_record;
        {
            // A frame a build in the window between the step and its `rewrite` being written
            // whole wrote decodes: a postcard reader takes the units prefix after the step as
            // the absent bool, and the record's write reads as a migration claimed by its app,
            // which its author's signature covers as written. It is refused by the record it
            // names, never served as the claim it reads as.
            let dir = tempfile::tempdir().unwrap();
            let key = device(4);
            let bytes = before_whole_migration_frame(&key);
            let decoded = NeighbourMsg::decode(&bytes).expect("the frame decodes");
            let NeighbourMsg::AppDocument { envelope, .. } = &decoded else {
                panic!("a document write");
            };
            assert_eq!(
                envelope.claims[0].action,
                collab_types::claim::ClaimedAction::History(tilo_actions::HistoryOp::Migrate {
                    to: 3,
                    rewrite: true,
                }),
                "the absent bool reads as the units prefix after it"
            );
            assert!(envelope.claims[0].units.is_empty(), "and its units are gone");
            collab_types::verify_op(envelope)
                .expect("the write verifies as its author signed the bytes it reads as");

            let header = frame(&key, 9, ContentRetention::Delta);
            let path = segment_path(dir.path(), workspace(), 0, 0);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, frame_record(&header, &bytes).unwrap()).unwrap();
            fs::write(dir.path().join(FRAMES_VERSION_FILE), FRAMES_VERSION.to_string()).unwrap();

            let store = Store::open(dir.path(), Appetite::StoreAll).unwrap();
            let Err(error) = store.replay(workspace(), &[], Budget::default()) else {
                panic!("a frame written without the step's bool is served");
            };
            assert_eq!(
                error.to_string(),
                format!(
                    "the frame of {APP}/{RECORD} by {} at 9 names a storage migration as a \
                     history move: its bytes are of a layout before the step wrote its fields \
                     whole",
                    hex::encode(key.verifying_key().to_bytes())
                )
            );
        }

        phase a_store_newer_than_this_build_reads_is_refused_and_left_whole;
        {
            // Every earlier version comes up. A store of a version no build wrote, or of a later
            // one, is refused by its version as it opens: not one frame of it is read, rewritten
            // or set aside.
            for version in [0_u32, FRAMES_VERSION + 1] {
                let dir = tempfile::tempdir().unwrap();
                let key = device(4);
                let store = Store::open(dir.path(), Appetite::StoreAll).unwrap();
                store
                    .retain(workspace(), &frame(&key, 1, ContentRetention::Delta))
                    .unwrap();
                drop(store);
                fs::write(dir.path().join(FRAMES_VERSION_FILE), version.to_string()).unwrap();
                let packs = pack_files(dir.path());
                let bytes: Vec<Vec<u8>> = packs.iter().map(|pack| fs::read(pack).unwrap()).collect();

                let refused = Store::open(dir.path(), Appetite::StoreAll);
                assert!(
                    matches!(refused, Err(StoreError::Version(kept)) if kept == version),
                    "a store of version {version} is refused by its version"
                );
                assert_eq!(
                    refused.err().unwrap().to_string(),
                    format!(
                        "the store's frames are version {version}; this build reads versions 1 \
                         to {FRAMES_VERSION}"
                    )
                );
                assert_eq!(
                    fs::read_to_string(dir.path().join(FRAMES_VERSION_FILE)).unwrap(),
                    version.to_string(),
                    "the store keeps the version it was written at"
                );
                assert_eq!(pack_files(dir.path()), packs, "no segment is added or taken away");
                for (pack, bytes) in packs.iter().zip(&bytes) {
                    assert_eq!(&fs::read(pack).unwrap(), bytes, "every segment is left whole");
                }
            }
        }
    }

    /// A host keeping a workspace of 10k resources: every resource written,
    /// then 100k more writes from writers at once, then a restart. The
    /// writes share their syncs, and a restart reads only the records'
    /// headers. Frames are signed once per resource and written again under
    /// later sequence numbers: the store never opens a frame it keeps, so
    /// what it costs is its own. What is held to a thousand writes a second
    /// is the processor time the writes take: the store does its work on
    /// the writers' threads, and the wall-clock rate on a shared node says
    /// how busy the node is, so it is printed beside it.
    #[test]
    fn a_store_takes_a_thousand_writes_a_second_and_reopens_ten_thousand_resources_quickly() {
        const RESOURCES: usize = 10_000;
        const WRITES: usize = 100_000;
        const WRITERS: usize = 16;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path(), Appetite::StoreAll).unwrap());
        let key = device(2);
        let signed: Vec<Frame> = (0..RESOURCES)
            .map(|resource| {
                let mut op = SignedOp::new_unsigned(
                    vec![resource as u8; 96],
                    ArtifactId::new(),
                    Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
                    AttestationId::new(),
                    OpKind::Change,
                    at(1),
                    1,
                );
                op.encryption = Some(EncryptionMetadata::content_key(1, ContentRetention::Delta));
                sign_op(&mut op, &key).unwrap();
                Frame::of(&NeighbourMsg::AppDocument {
                    peer: 0,
                    app_id: APP.into(),
                    cluster_id: CLUSTER.into(),
                    document_id: format!("resource:{resource}"),
                    envelope: Box::new(op),
                    readers: None,
                })
                .unwrap()
            })
            .collect();
        for frame in &signed {
            store.retain(workspace(), frame).unwrap();
        }
        let started = std::time::Instant::now();
        let spent = std::sync::Mutex::new(Duration::ZERO);
        std::thread::scope(|scope| {
            for writer in 0..WRITERS {
                let (store, signed, spent) = (&store, &signed, &spent);
                scope.spawn(move || {
                    let cpu = ui::rt::thread_cpu_time().expect("the thread's processor time");
                    for write in (writer..WRITES).step_by(WRITERS) {
                        let resource = write % RESOURCES;
                        let mut frame = signed[resource].clone();
                        frame.seq = (write / RESOURCES) as u64 + 2;
                        frame.authored_at_ns = frame.seq;
                        assert_eq!(store.retain(workspace(), &frame).unwrap(), Retained::New);
                    }
                    *spent.lock().unwrap() += ui::rt::thread_cpu_time()
                        .expect("the thread's processor time")
                        .saturating_sub(cpu);
                });
            }
        });
        let cpu = spent.into_inner().unwrap();
        let elapsed = started.elapsed();
        let total = f64::from(u32::try_from(WRITES).unwrap());
        let per_second = total / elapsed.as_secs_f64();
        let per_cpu_second = total / cpu.as_secs_f64();
        let bytes = store.bytes();
        drop(store);

        let started = std::time::Instant::now();
        let reopened = Store::open(dir.path(), Appetite::StoreAll).unwrap();
        let open = started.elapsed();
        eprintln!(
            "store at 10k resources: {per_cpu_second:.0} writes per processor second ({WRITES} in {cpu:?} of processor time), {per_second:.0} writes/s on the clock ({elapsed:?}), reopened in {open:?}"
        );
        assert_eq!(reopened.stands(workspace()).len(), RESOURCES);
        assert_eq!(reopened.bytes(), bytes);
        assert!(
            reopened.segments(workspace()).len() < 16,
            "the writes are packed in few files"
        );
        assert!(
            per_cpu_second >= 1_000.0,
            "{per_cpu_second:.0} writes per processor second ({cpu:?} for {WRITES})"
        );
        assert!(open < Duration::from_secs(5), "reopening took {open:?}");
        let last = reopened
            .replay(
                workspace(),
                &[cursor(signed[0].lineage, 10)],
                Budget {
                    max_frames: 1,
                    ..Budget::default()
                },
            )
            .unwrap();
        assert_eq!(last.frames.len(), 1);
    }

    /// The workspace's own published changes are kept, each beside the
    /// others in the order they were made: a device that links later is
    /// replayed them before the records they govern. A change already here
    /// is not kept twice, one that changes nothing is not kept, and neither
    /// is another workspace's.
    #[test]
    fn a_workspace_keeps_the_changes_it_publishes_in_order() {
        let (dir, store) = open();
        let publish = |workspace: WorkspaceId, change: &[u8]| NeighbourMsg::SyncResp {
            changes: vec![change.to_vec()],
            workspace: workspace.to_string(),
        };
        let first = publish(workspace(), b"first");
        let second = publish(workspace(), b"second");
        assert!(
            store
                .keep_workspace_change(workspace(), [1; 32], &first)
                .unwrap()
        );
        assert!(
            store
                .keep_workspace_change(workspace(), [1; 32], &second)
                .unwrap()
        );
        assert!(
            !store
                .keep_workspace_change(workspace(), [1; 32], &first)
                .unwrap(),
            "a change already here is not kept twice"
        );
        assert!(
            !store
                .keep_workspace_change(
                    workspace(),
                    [1; 32],
                    &NeighbourMsg::SyncResp {
                        changes: Vec::new(),
                        workspace: workspace().to_string(),
                    },
                )
                .unwrap(),
            "a message that changes nothing is not kept"
        );
        let other = WorkspaceId::from_u128(8);
        assert!(
            !store
                .keep_workspace_change(
                    workspace(),
                    [1; 32],
                    &publish(other, b"another workspace's"),
                )
                .unwrap(),
            "another workspace's change is not kept here"
        );
        let kept: Vec<NeighbourMsg> = store
            .channels(workspace())
            .into_iter()
            .map(|(_, message)| message)
            .collect();
        assert_eq!(
            kept,
            vec![first, second],
            "the changes are served in the order they were made"
        );
        drop(store);
        let reopened = Store::open(dir.path(), Appetite::StoreAll).unwrap();
        assert_eq!(
            reopened.channels(workspace()).len(),
            2,
            "a restart serves them still"
        );
    }

    /// Two frames with other bytes at one author's sequence number are both
    /// kept and both served, across a restart: neither is taken for the
    /// other. The same bytes again are one frame.
    #[test]
    fn two_frames_at_one_sequence_number_are_both_kept() {
        let (dir, store) = open();
        let key = device(2);
        let one = frame(&key, 4, ContentRetention::Delta);
        let other = frame(&key, 4, ContentRetention::Delta);
        assert_ne!(one.message, other.message);
        assert_eq!(store.retain(workspace(), &one).unwrap(), Retained::New);
        assert_eq!(store.retain(workspace(), &other).unwrap(), Retained::Beside);
        assert_eq!(
            store.retain(workspace(), &other).unwrap(),
            Retained::Duplicate
        );
        assert!(store.holds_frame(workspace(), &one).unwrap());
        assert!(store.holds_frame(workspace(), &other).unwrap());
        let served = |store: &Store| {
            let mut served = store
                .replay(workspace(), &[], Budget::default())
                .unwrap()
                .frames;
            served.sort_by_key(|message| postcard::to_allocvec(message).unwrap());
            served
        };
        let mut both = vec![one.message.clone(), other.message];
        both.sort_by_key(|message| postcard::to_allocvec(message).unwrap());
        assert_eq!(served(&store), both);
        drop(store);
        let reopened = Store::open(dir.path(), Appetite::StoreAll).unwrap();
        assert_eq!(served(&reopened), both, "a restart keeps both");
        assert_eq!(
            reopened.retain(workspace(), &one).unwrap(),
            Retained::Duplicate
        );
    }

    /// A checkpoint at sequence number 0 is served to a device that holds
    /// none of its lineage, and not to one that holds it. A frame its
    /// author's newer checkpoint covers is not kept, as a restart would not
    /// keep it either.
    #[test]
    fn a_checkpoint_at_zero_is_served_and_a_covered_frame_is_not_kept() {
        let (dir, store) = open();
        let key = device(3);
        let base = frame(&key, 0, ContentRetention::Checkpoint);
        assert_eq!(store.retain(workspace(), &base).unwrap(), Retained::New);
        let fresh = store.replay(workspace(), &[], Budget::default()).unwrap();
        assert_eq!(fresh.frames, vec![base.message.clone()]);
        assert!(fresh.floors.is_empty());
        let holding = store
            .replay(workspace(), &[cursor(base.lineage, 0)], Budget::default())
            .unwrap();
        assert!(
            holding.frames.is_empty(),
            "a device holding it is not sent it again"
        );

        let newer = frame(&key, 5, ContentRetention::Checkpoint);
        assert_eq!(store.retain(workspace(), &newer).unwrap(), Retained::New);
        assert!(
            !store.holds_frame(workspace(), &base).unwrap(),
            "the newer checkpoint retired it"
        );
        let older = frame(&key, 3, ContentRetention::Delta);
        assert_eq!(
            store.retain(workspace(), &older).unwrap(),
            Retained::Covered
        );
        let before = store.stands(workspace());
        drop(store);
        let reopened = Store::open(dir.path(), Appetite::StoreAll).unwrap();
        assert_eq!(
            reopened.stands(workspace()),
            before,
            "a restart retires nothing more"
        );
        assert_eq!(
            reopened
                .replay(workspace(), &[], Budget::default())
                .unwrap()
                .frames,
            vec![newer.message]
        );
    }

    /// A batch of writes kept together is served in the order it was made, once each, and is
    /// there when the store opens again.
    #[test]
    fn a_batch_kept_with_one_sync_is_served_in_order_after_a_restart() {
        let (dir, store) = open();
        let key = device(2);
        let frames: Vec<Frame> = (1..=200)
            .map(|seq| frame(&key, seq, ContentRetention::Delta))
            .collect();
        assert_eq!(
            store.retain(workspace(), &frames[0]).unwrap(),
            Retained::New
        );
        let kept = store.retain_all(workspace(), &frames).unwrap();
        assert_eq!(
            kept[0],
            Retained::Duplicate,
            "a frame kept before is kept once"
        );
        assert!(kept[1..].iter().all(|kept| *kept == Retained::New));
        assert!(
            store.retain_all(workspace(), &[]).unwrap().is_empty(),
            "an empty batch keeps nothing"
        );
        drop(store);
        let reopened = Store::open(dir.path(), Appetite::StoreAll).unwrap();
        let replay = reopened
            .replay(workspace(), &[], Budget::default())
            .unwrap();
        assert_eq!(seqs(&replay), (1..=200).collect::<Vec<u64>>());
    }

    /// A device's own state is kept whole beside its frames and read back
    /// after a restart; a name that would reach outside the store's state is
    /// refused.
    #[test]
    fn state_kept_beside_the_frames_is_read_back_after_a_restart() {
        let (dir, store) = open();
        assert_eq!(store.state("peer/record").unwrap(), None);
        store.keep_state("peer/record", b"first").unwrap();
        store.keep_state("peer/record", b"second").unwrap();
        store.keep_state("peer/parts/index", b"").unwrap();
        store.keep_state("peer/runs/gone", b"run").unwrap();
        store.forget_state("peer/runs/gone").unwrap();
        store.forget_state("peer/runs/never").unwrap();
        for refused in ["", "/etc/passwd", "../outside", "peer/../../outside"] {
            assert!(store.keep_state(refused, b"x").is_err(), "{refused:?}");
        }
        drop(store);
        let reopened = Store::open(dir.path(), Appetite::StoreAll).unwrap();
        assert_eq!(
            reopened.state("peer/record").unwrap(),
            Some(b"second".to_vec())
        );
        assert_eq!(
            reopened.state("peer/parts/index").unwrap(),
            Some(Vec::new())
        );
        assert_eq!(reopened.state("peer/runs/gone").unwrap(), None);
        assert!(
            reopened
                .replay(workspace(), &[], Budget::default())
                .unwrap()
                .frames
                .is_empty(),
            "state is never served as a frame"
        );
    }

    /// The store's logic over files held in memory: what it retained and
    /// retired is what a store opened again over the same files serves.
    #[test]
    fn a_store_over_memory_serves_after_reopening_what_it_kept() {
        let files = MemoryBackend::new();
        let store = Store::open_on(Arc::new(files.clone()), Appetite::StoreAll).unwrap();
        let key = device(4);
        for seq in 1..=3 {
            store
                .retain(workspace(), &frame(&key, seq, ContentRetention::Delta))
                .unwrap();
        }
        let checkpoint = frame(&key, 4, ContentRetention::Checkpoint);
        store.retain(workspace(), &checkpoint).unwrap();
        let later = frame(&key, 5, ContentRetention::Delta);
        store.retain(workspace(), &later).unwrap();
        drop(store);
        let reopened = Store::open_on(Arc::new(files), Appetite::StoreAll).unwrap();
        assert_eq!(
            reopened
                .replay(workspace(), &[], Budget::default())
                .unwrap()
                .frames,
            vec![checkpoint.message, later.message],
            "the checkpoint retired what it covers, and the restart kept that"
        );
    }
}
