//! The frames a store device holds for its workspaces.
//!
//! Every frame is one immutable file under
//! `workspaces/<workspace>/<record>/<author>/<seq>.<class>`, written with a
//! crash-safe replace, so a store that restarts reads back exactly what it
//! acknowledged and nothing half-written. The index the store answers
//! replay from is rebuilt by reading those files at [`Store::open`]; the
//! files are the truth.
//!
//! Retention has three sources and no others: a frame's author retiring its
//! own earlier frames by publishing a checkpoint ([`Store::retain`]), the
//! appetite the store was opened with ([`Appetite`]), and the compaction an
//! operator asks for ([`Store::compact`]). Nothing is retired because it
//! looks old, and compaction never retires a frame a device that told the
//! store what it holds ([`Store::holds`]) still lacks, nor a lineage's newest
//! checkpoint, which a device that holds none of it starts from.

use crate::join::WorkspaceId;
pub use crate::retained::{Frame, Lineage, Refusal};
use collab_types::ContentRetention;
use neighbour_wire::NeighbourMsg;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tilo_lock::MutexRecover as _;

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

/// Where a subscriber stands on one lineage: it holds every frame up to
/// `after_seq` and asks for the ones after it.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct Cursor {
    /// The lineage it resumes.
    pub lineage: Lineage,
    /// The last sequence number it holds; `0` for a device that holds none.
    pub after_seq: u64,
}

/// Where a store has nothing: the earliest frame it still holds on a
/// lineage. A subscriber whose cursor is further back has a gap there and
/// asks another device of the workspace for what the store retired.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Floor {
    /// The lineage the store is short on.
    pub lineage: Lineage,
    /// The earliest frame it holds there.
    pub earliest_seq: u64,
}

/// What one replay round carries.
#[derive(Clone, Debug, Default)]
pub struct Replay {
    /// The frames after the subscriber's cursors, in storage order.
    pub frames: Vec<NeighbourMsg>,
    /// Lineages the store cannot continue from the cursors it was given.
    pub floors: Vec<Floor>,
    /// Whether the round stopped at its budget with frames left; ask again
    /// from the cursors these frames advance.
    pub more: bool,
}

/// What retaining a frame did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retained {
    /// The store now holds it.
    New,
    /// It held it already.
    Duplicate,
}

/// How much one replay round may carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// The most frames in one round.
    pub max_frames: usize,
    /// The most frame bytes in one round.
    pub max_bytes: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_frames: 256,
            max_bytes: 8 * 1024 * 1024,
        }
    }
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
    /// The store's frames were kept by a later build, in a layout this one does not read.
    #[error("the store's frames are version {0}; this build reads up to {FRAMES_VERSION}")]
    Newer(u32),
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

/// A device's retained history, one directory per workspace.
pub struct Store {
    root: PathBuf,
    appetite: Appetite,
    held: std::sync::Mutex<Held>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = self.held.lock_recover();
        f.debug_struct("Store")
            .field("root", &self.root)
            .field("appetite", &self.appetite)
            .field("bytes", &held.bytes)
            .field("workspaces", &held.workspaces.len())
            .finish()
    }
}

struct Held {
    bytes: u64,
    workspaces: BTreeMap<WorkspaceId, Workspace>,
}

#[derive(Default)]
struct Workspace {
    bytes: u64,
    lineages: BTreeMap<Lineage, Vec<Entry>>,
    /// What each device that asked for the workspace's history said it
    /// holds, by lineage ([`Store::holds`]).
    holders: BTreeMap<[u8; 32], BTreeMap<Lineage, u64>>,
}

/// One retained frame as the index knows it.
#[derive(Clone, Copy)]
struct Entry {
    seq: u64,
    bytes: u64,
    authored_at_ns: u64,
    retention: ContentRetention,
    /// Whether the store's first copy came over one of the device's links,
    /// rather than from the device itself or another carrier: what a reader
    /// of those links that fell behind is handed again
    /// ([`Store::taken_from_link`]). Known for the frames kept since the store
    /// opened, which is all a reader that subscribed since can miss.
    from_link: bool,
}

impl Store {
    /// Open the store rooted at `root`, reading back the frames already
    /// there, and retire what `appetite` no longer allows.
    ///
    /// # Errors
    ///
    /// Returns an error when the root cannot be created or read. A file that
    /// is not a frame a store keeps is skipped and logged: a crash leaves
    /// nothing half-written, so such a file came from outside.
    pub fn open(root: impl Into<PathBuf>, appetite: Appetite) -> Result<Self, StoreError> {
        let root = root.into();
        let kept = frames_version(&root)?;
        if kept > FRAMES_VERSION {
            return Err(StoreError::Newer(kept));
        }
        fs::create_dir_all(root.join(WORKSPACES))?;
        let store = Self {
            root,
            appetite,
            held: std::sync::Mutex::new(Held {
                bytes: 0,
                workspaces: BTreeMap::new(),
            }),
        };
        let (frames, holders) = (store.stored(kept)?, store.stored_holders()?);
        if kept < FRAMES_VERSION || !store.root.join(FRAMES_VERSION_FILE).is_file() {
            tilo_files::native::atomic::write(
                &store.root.join(FRAMES_VERSION_FILE),
                FRAMES_VERSION.to_string().as_bytes(),
            )?;
        }
        {
            let mut held = store.held.lock_recover();
            for (workspace, frame, bytes) in frames {
                add(&mut held, workspace, &frame, bytes, false);
            }
            for (workspace, device, cursors) in holders {
                held.workspaces
                    .entry(workspace)
                    .or_default()
                    .holders
                    .insert(device, holding(&cursors));
            }
            retire_to_appetite(&store.root, appetite, &mut held);
            drop(held);
        }
        Ok(store)
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

    /// Each lineage of `workspace` with the earliest and latest sequence
    /// number the store holds there: what a member asks about and what a
    /// diagnostic shows.
    #[must_use]
    pub fn lineages(&self, workspace: WorkspaceId) -> Vec<(Lineage, u64, u64)> {
        let held = self.held.lock_recover();
        held.workspaces
            .get(&workspace)
            .map(|held| {
                held.lineages
                    .iter()
                    .filter_map(|(lineage, entries)| {
                        let first = entries.first()?.seq;
                        let last = entries.last()?.seq;
                        Some((*lineage, first, last))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Keep `frame` for `workspace`, unless the store holds that frame of
    /// its lineage already. A checkpoint the author published retires the
    /// author's earlier frames of the same lineage, and the appetite retires
    /// what it must.
    ///
    /// # Errors
    ///
    /// Returns an error when the frame cannot be written. The store is
    /// unchanged then.
    pub fn retain(&self, workspace: WorkspaceId, frame: &Frame) -> Result<Retained, StoreError> {
        self.retain_from(workspace, frame, false)
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
        self.retain_from(workspace, frame, true)
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
            .and_then(|entries| entries.iter().find(|entry| entry.seq == frame.seq))
            .is_some_and(|entry| entry.from_link)
    }

    fn retain_from(
        &self,
        workspace: WorkspaceId,
        frame: &Frame,
        from_link: bool,
    ) -> Result<Retained, StoreError> {
        let bytes = postcard::to_allocvec(&frame.message)?;
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let mut held = self.held.lock_recover();
        if held
            .workspaces
            .get(&workspace)
            .and_then(|held| held.lineages.get(&frame.lineage))
            .is_some_and(|entries| entries.iter().any(|entry| entry.seq == frame.seq))
        {
            return Ok(Retained::Duplicate);
        }
        let path = frame_path(
            &self.root,
            workspace,
            frame.lineage,
            frame.seq,
            frame.retention,
        );
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        tilo_files::native::atomic::write(&path, &bytes)?;
        add(&mut held, workspace, frame, len, from_link);
        if frame.retention == ContentRetention::Checkpoint {
            let retired = retire_before(&self.root, workspace, frame.lineage, frame.seq, &mut held);
            tracing::debug!(
                %retired.frames,
                %retired.bytes,
                "a checkpoint retired the frames it covers"
            );
        }
        retire_to_appetite(&self.root, self.appetite, &mut held);
        drop(held);
        Ok(Retained::New)
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
        let held = self.held.lock_recover();
        let mut replay = Replay::default();
        let Some(stored) = held.workspaces.get(&workspace) else {
            return Ok(replay);
        };
        // Each author line's frames after its cursor, in the line's order.
        let mut pending: Vec<(Lineage, &[Entry])> = Vec::new();
        for (lineage, entries) in &stored.lineages {
            let after = cursors
                .iter()
                .filter(|cursor| cursor.lineage == *lineage)
                .map(|cursor| cursor.after_seq)
                .max()
                .unwrap_or(0);
            let Some(first) = entries.first().map(|entry| entry.seq) else {
                continue;
            };
            if first > after.saturating_add(1) {
                replay.floors.push(Floor {
                    lineage: *lineage,
                    earliest_seq: first,
                });
            }
            let from = entries.partition_point(|entry| entry.seq <= after);
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
            let Some((lineage, rest)) = pending.get_mut(at) else {
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
            replay
                .frames
                .push(self.frame_at(workspace, *lineage, *entry)?);
            carried = carried.saturating_add(1);
            carried_bytes = carried_bytes.saturating_add(entry.bytes);
            *rest = after;
        }
        drop(held);
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
            match fs::remove_dir_all(holders_dir(&self.root, workspace)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            held.bytes = held.bytes.saturating_sub(stored.bytes);
            let frames = stored.lineages.values().map(Vec::len).sum::<usize>();
            drop(held);
            (stored.bytes, frames)
        };
        match fs::remove_dir_all(workspace_dir(&self.root, workspace)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
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
        let path = holder_path(&self.root, workspace, device);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let cursors: Vec<Cursor> = holding
            .iter()
            .map(|(lineage, after_seq)| Cursor {
                lineage: *lineage,
                after_seq: *after_seq,
            })
            .collect();
        tilo_files::native::atomic::write(&path, &postcard::to_allocvec(&cursors)?)?;
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
                &self.root,
                workspace,
                lineage,
                entry,
                &mut held,
                &mut retired,
            )?;
        }
        drop(held);
        Ok(retired)
    }

    /// What every device said it holds, as [`Store::open`] reads it. A file
    /// that is not one is skipped, as a stray frame file is.
    fn stored_holders(&self) -> Result<Vec<HeldBy>, StoreError> {
        let mut found = Vec::new();
        for workspace in directories(&self.root.join(HOLDERS))? {
            let Some(id) = workspace
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(WorkspaceId::parse)
            else {
                continue;
            };
            for file in files(&workspace)? {
                let device = file
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(<[u8; 32] as hex::FromHex>::from_hex);
                let cursors = postcard::from_bytes::<Vec<Cursor>>(&fs::read(&file)?);
                if let (Some(Ok(device)), Ok(cursors)) = (device, cursors) {
                    found.push((id, device, cursors));
                } else {
                    tracing::warn!(path = %file.display(), "not what a device holds; skipped");
                }
            }
        }
        Ok(found)
    }

    /// Every stored frame, as [`Store::open`] reads it, each written again as this build writes
    /// it when the store kept it as version `kept` of its frames, before [`FRAMES_VERSION`].
    fn stored(&self, kept: u32) -> Result<Vec<(WorkspaceId, Frame, u64)>, StoreError> {
        let mut found = Vec::new();
        for workspace in directories(&self.root.join(WORKSPACES))? {
            let Some(workspace) = workspace
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(WorkspaceId::parse)
            else {
                continue;
            };
            for record in directories(&workspace_dir(&self.root, workspace))? {
                for author in directories(&record)? {
                    for file in files(&author)? {
                        let bytes = fs::read(&file)?;
                        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                        let message = match NeighbourMsg::decode(&bytes) {
                            Ok(message) => message,
                            Err(error) => {
                                tracing::warn!(path = %file.display(), %error, "a stored frame does not decode; it stays on disk and is not served");
                                continue;
                            }
                        };
                        let len = if kept < FRAMES_VERSION {
                            written_as_today(&file, &bytes, &message).unwrap_or(len)
                        } else {
                            len
                        };
                        match Frame::of(&message) {
                            Ok(frame) => found.push((workspace, frame, len)),
                            Err(error) => {
                                tracing::warn!(path = %file.display(), %error, "a stored file is not a frame a store keeps; it stays on disk and is not served");
                            }
                        }
                    }
                }
            }
        }
        Ok(found)
    }

    fn frame_at(
        &self,
        workspace: WorkspaceId,
        lineage: Lineage,
        entry: Entry,
    ) -> Result<NeighbourMsg, StoreError> {
        let path = frame_path(&self.root, workspace, lineage, entry.seq, entry.retention);
        let bytes = fs::read(&path)?;
        Ok(NeighbourMsg::decode(&bytes)?)
    }
}

/// The frame `message`, read from `bytes` at `file`, written again as this build writes it when
/// it was kept in an earlier layout ([`NeighbourMsg::decode`]). Answers the length kept, or `None`
/// when the file was not written again (it is served as it reads either way).
fn written_as_today(file: &Path, bytes: &[u8], message: &NeighbourMsg) -> Option<u64> {
    let today = postcard::to_allocvec(message).ok()?;
    if today == bytes {
        return None;
    }
    match tilo_files::native::atomic::write(file, &today) {
        Ok(()) => u64::try_from(today.len()).ok(),
        Err(error) => {
            tracing::warn!(path = %file.display(), %error, "a frame kept in an earlier layout was not written again; it is served as it reads");
            None
        }
    }
}

/// Where `peer`'s replica stands, one cursor per author line of its sealed
/// history: what it asks a store to continue from.
#[must_use]
pub fn cursors(peer: &crate::StatePeer) -> Vec<Cursor> {
    peer.replay_floors()
        .into_iter()
        .map(|(app, record, author, seq)| Cursor {
            lineage: Lineage::of(&app, &record, author),
            after_seq: seq,
        })
        .collect()
}

/// Where `cursors` stand after `frames`: one cursor per lineage they belong
/// to, at the last sequence number it carried.
pub fn advance(cursors: &mut Vec<Cursor>, frames: &[NeighbourMsg]) {
    for frame in frames {
        let Ok(frame) = Frame::of(frame) else {
            continue;
        };
        match cursors
            .iter_mut()
            .find(|cursor| cursor.lineage == frame.lineage)
        {
            Some(cursor) => cursor.after_seq = cursor.after_seq.max(frame.seq),
            None => cursors.push(Cursor {
                lineage: frame.lineage,
                after_seq: frame.seq,
            }),
        }
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

/// Whether `message` is a sealed document frame: the only history a store
/// keeps, since it never holds a workspace's keys.
#[must_use]
pub fn sealed(message: &NeighbourMsg) -> bool {
    match message {
        NeighbourMsg::AppDocument { envelope, .. } => envelope.encryption.is_some(),
        _ => false,
    }
}

/// The device `message` names as its own, when it names one: a frame a
/// workspace's host forwards says whose it is by its own signature, whatever
/// link carried it.
#[must_use]
pub fn author_of(message: &NeighbourMsg) -> Option<[u8; 32]> {
    match message {
        NeighbourMsg::AppDocument { envelope, .. } => Some(*envelope.author.as_bytes()),
        NeighbourMsg::IdentityHello { cert, .. } => Some(cert.device_public_key),
        _ => None,
    }
}

const WORKSPACES: &str = "workspaces";

/// The layout of the frames a store keeps, one postcard [`NeighbourMsg`] per file. Version 1 kept
/// a document write without the reader set it is sealed for, which version 2 ends it with
/// ([`NeighbourMsg::AppDocument`]); a store opening frames of version 1 writes each again as
/// version 2 ([`written_as_today`]) before it records the version it keeps.
pub(crate) const FRAMES_VERSION: u32 = 2;
/// Where a store records the version of the frames it keeps: no file is version 1, kept before
/// a store recorded one.
const FRAMES_VERSION_FILE: &str = "frames-version";

/// The version of the frames kept at `root` ([`FRAMES_VERSION`]): the one recorded there, version
/// 1 when frames were kept before a store recorded one, today's for a store that keeps none yet.
fn frames_version(root: &Path) -> Result<u32, StoreError> {
    match fs::read_to_string(root.join(FRAMES_VERSION_FILE)) {
        Ok(text) => text.trim().parse().map_err(|_| {
            StoreError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{FRAMES_VERSION_FILE} holds {text:?}, not a version"),
            ))
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if directories(&root.join(WORKSPACES))?.is_empty() {
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

/// A workspace, a device of it, and where that device said it stands.
type HeldBy = (WorkspaceId, [u8; 32], Vec<Cursor>);

fn workspace_dir(root: &Path, workspace: WorkspaceId) -> PathBuf {
    root.join(WORKSPACES).join(workspace.to_string())
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

fn frame_path(
    root: &Path,
    workspace: WorkspaceId,
    lineage: Lineage,
    seq: u64,
    retention: ContentRetention,
) -> PathBuf {
    let class = match retention {
        ContentRetention::Checkpoint => "c",
        ContentRetention::Delta => "d",
    };
    workspace_dir(root, workspace)
        .join(hex::encode(lineage.record))
        .join(hex::encode(lineage.author))
        .join(format!("{seq:016x}.{class}"))
}

/// The directories inside `dir`, sorted, or none when it does not exist.
fn directories(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(found),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

/// The files inside `dir`, sorted, or none when it does not exist.
fn files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(found),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let path = entry?.path();
        if path.is_file() {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

fn add(held: &mut Held, workspace: WorkspaceId, frame: &Frame, bytes: u64, from_link: bool) {
    let entry = Entry {
        seq: frame.seq,
        bytes,
        authored_at_ns: frame.authored_at_ns,
        retention: frame.retention,
        from_link,
    };
    let stored = held.workspaces.entry(workspace).or_default();
    let entries = stored.lineages.entry(frame.lineage).or_default();
    let at = entries.partition_point(|held| held.seq < entry.seq);
    entries.insert(at, entry);
    stored.bytes = stored.bytes.saturating_add(bytes);
    held.bytes = held.bytes.saturating_add(bytes);
}

/// Remove every frame of `lineage` before `seq` and report what went.
fn retire_before(
    root: &Path,
    workspace: WorkspaceId,
    lineage: Lineage,
    seq: u64,
    held: &mut Held,
) -> Retired {
    let mut retired = Retired::default();
    let covered = held
        .workspaces
        .get(&workspace)
        .and_then(|stored| stored.lineages.get(&lineage))
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry.seq < seq)
                .copied()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for entry in covered {
        if let Err(error) = retire(root, workspace, lineage, entry, held, &mut retired) {
            tracing::warn!(%error, "a covered frame could not be removed");
        }
    }
    retired
}

fn retire(
    root: &Path,
    workspace: WorkspaceId,
    lineage: Lineage,
    entry: Entry,
    held: &mut Held,
    retired: &mut Retired,
) -> io::Result<()> {
    let path = frame_path(root, workspace, lineage, entry.seq, entry.retention);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let Some(stored) = held.workspaces.get_mut(&workspace) else {
        return Ok(());
    };
    if let Some(entries) = stored.lineages.get_mut(&lineage) {
        entries.retain(|held| held.seq != entry.seq);
        if entries.is_empty() {
            stored.lineages.remove(&lineage);
        }
    }
    stored.bytes = stored.bytes.saturating_sub(entry.bytes);
    held.bytes = held.bytes.saturating_sub(entry.bytes);
    retired.frames = retired.frames.saturating_add(1);
    retired.bytes = retired.bytes.saturating_add(entry.bytes);
    Ok(())
}

/// Retire frames until the store fits `appetite`: oldest first, and a
/// lineage's newest checkpoint last among its own frames.
fn retire_to_appetite(root: &Path, appetite: Appetite, held: &mut Held) {
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
        if let Err(error) = retire(root, workspace, lineage, entry, held, &mut retired) {
            tracing::warn!(%error, "a frame the appetite retires could not be removed");
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
        ArtifactId, AttestationId, Ed25519PublicKey, EncryptionMetadata, OpKind, SignedOp, sign_op,
    };
    use ed25519_dalek::SigningKey;
    use neighbour_wire::peer_handle;
    use std::time::Duration;
    use web_time::{SystemTime, UNIX_EPOCH};

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

    /// A sealed document frame by `key`, with whatever ciphertext a store
    /// never opens.
    fn frame(key: &SigningKey, seq: u64, retention: ContentRetention) -> Frame {
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
            document_id: RECORD.into(),
            envelope: Box::new(op),
            own: None,
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
                store.lineages(workspace()),
                vec![(lineage, 1, 3)],
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

        phase a_checkpoint_retires_only_its_own_lineage;
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

            let lineages = store.lineages(workspace());
            assert_eq!(
                lineages,
                vec![(checkpoint.lineage, 3, 4), (theirs.lineage, 1, 1)],
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
            let path = frame_path(
                dir.path(),
                workspace(),
                checkpoint.lineage,
                checkpoint.seq,
                checkpoint.retention,
            );
            let kept = fs::read(&path).unwrap().len() as u64;
            assert!(size >= kept);

            // Reopening under an appetite of exactly the checkpoint keeps that
            // and retires everything else, since a checkpoint is the state a
            // later frame is read against.
            let reopened = Store::open(dir.path(), Appetite::Bounded { max_bytes: kept }).unwrap();
            assert_eq!(reopened.bytes(), kept);
            assert_eq!(
                reopened.lineages(workspace()),
                vec![(checkpoint.lineage, 3, 3)]
            );
            assert!(fs::read(&path).is_ok());
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
            assert_eq!(store.lineages(workspace()).len(), 1);
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
            assert_eq!(store.lineages(workspace()), vec![(lineage, 1, 5)]);
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
                store.lineages(workspace()),
                vec![(lineage, 1, 1)],
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
            let stray = frame_path(
                dir.path(),
                workspace(),
                frame.lineage,
                2,
                ContentRetention::Delta,
            );
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
                own: None,
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
                own: None,
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
                own: None,
                readers: None,
            };
            assert_eq!(Frame::of(&message), Err(Refusal::NotSealed));
        }
    }
}
