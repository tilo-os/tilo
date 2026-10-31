//! The frames a store device holds for its workspaces.
//!
//! Every frame is one immutable file under
//! `workspaces/<workspace>/<channel>/<author>/<seq>.<class>`, written with a
//! crash-safe replace, so a store that restarts reads back exactly what it
//! acknowledged and nothing half-written. The index the store answers
//! replay from is rebuilt by reading those files at [`Store::open`]; the
//! files are the truth.
//!
//! Retention has three sources and no others: a frame's author retiring its
//! own earlier frames by publishing a checkpoint ([`Store::retain`]), the
//! appetite the store was opened with ([`Appetite`]), and the compaction an
//! operator asks for ([`Store::compact`]). Nothing is retired because it
//! looks old.

use crate::frame::{Frame, Lineage, Refusal};
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
    workspaces: BTreeMap<u64, Workspace>,
}

#[derive(Default)]
struct Workspace {
    bytes: u64,
    lineages: BTreeMap<Lineage, Vec<Entry>>,
}

/// One retained frame as the index knows it.
#[derive(Clone, Copy)]
struct Entry {
    seq: u64,
    bytes: u64,
    authored_at_ns: u64,
    retention: ContentRetention,
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
        fs::create_dir_all(root.join(WORKSPACES))?;
        let store = Self {
            root,
            appetite,
            held: std::sync::Mutex::new(Held {
                bytes: 0,
                workspaces: BTreeMap::new(),
            }),
        };
        {
            let mut held = store.held.lock_recover();
            for (workspace, frame, bytes) in store.stored()? {
                add(&mut held, workspace, &frame, bytes);
            }
            retire_to_appetite(&store.root, appetite, &mut held);
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

    /// Each lineage of `workspace` with the earliest and latest sequence
    /// number the store holds there: what a member asks about and what a
    /// diagnostic shows.
    #[must_use]
    pub fn lineages(&self, workspace: u64) -> Vec<(Lineage, u64, u64)> {
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
    pub fn retain(&self, workspace: u64, frame: &Frame) -> Result<Retained, StoreError> {
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
        add(&mut held, workspace, frame, len);
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
        workspace: u64,
        cursors: &[Cursor],
        budget: Budget,
    ) -> Result<Replay, StoreError> {
        let held = self.held.lock_recover();
        let mut replay = Replay::default();
        let Some(stored) = held.workspaces.get(&workspace) else {
            return Ok(replay);
        };
        let mut carried = 0_usize;
        let mut carried_bytes = 0_u64;
        'lineages: for (lineage, entries) in &stored.lineages {
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
            for entry in entries {
                if entry.seq <= after {
                    continue;
                }
                if carried >= budget.max_frames
                    || carried_bytes.saturating_add(entry.bytes) > budget.max_bytes
                {
                    replay.more = true;
                    break 'lineages;
                }
                replay
                    .frames
                    .push(self.frame_at(workspace, *lineage, *entry)?);
                carried = carried.saturating_add(1);
                carried_bytes = carried_bytes.saturating_add(entry.bytes);
            }
        }
        drop(held);
        Ok(replay)
    }

    /// Retire the frames of `workspace` published before `before_ns`, for an
    /// operator whose store must shrink now rather than at its budget.
    ///
    /// # Errors
    ///
    /// Returns an error when a frame cannot be removed.
    pub fn compact(&self, workspace: u64, before_ns: u64) -> Result<Retired, StoreError> {
        let mut held = self.held.lock_recover();
        let mut retired = Retired::default();
        let Some(stored) = held.workspaces.get(&workspace) else {
            return Ok(retired);
        };
        let old = stored
            .lineages
            .iter()
            .flat_map(|(lineage, entries)| {
                entries
                    .iter()
                    .filter(|entry| entry.authored_at_ns < before_ns)
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

    /// Every stored frame, as [`Store::open`] reads it.
    fn stored(&self) -> Result<Vec<(u64, Frame, u64)>, StoreError> {
        let mut found = Vec::new();
        for workspace in directories(&self.root.join(WORKSPACES))? {
            let Some(workspace) = workspace
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| u64::from_str_radix(name, 16).ok())
            else {
                continue;
            };
            for channel in directories(&workspace_dir(&self.root, workspace))? {
                for author in directories(&channel)? {
                    for file in files(&author)? {
                        let bytes = fs::read(&file)?;
                        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                        let message: NeighbourMsg = match postcard::from_bytes(&bytes) {
                            Ok(message) => message,
                            Err(error) => {
                                tracing::warn!(path = %file.display(), %error, "a stored frame does not decode; it stays on disk and is not served");
                                continue;
                            }
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
        workspace: u64,
        lineage: Lineage,
        entry: Entry,
    ) -> Result<NeighbourMsg, StoreError> {
        let path = frame_path(&self.root, workspace, lineage, entry.seq, entry.retention);
        let bytes = fs::read(&path)?;
        Ok(postcard::from_bytes(&bytes)?)
    }
}

const WORKSPACES: &str = "workspaces";

fn workspace_dir(root: &Path, workspace: u64) -> PathBuf {
    root.join(WORKSPACES).join(format!("{workspace:016x}"))
}

fn frame_path(
    root: &Path,
    workspace: u64,
    lineage: Lineage,
    seq: u64,
    retention: ContentRetention,
) -> PathBuf {
    let class = match retention {
        ContentRetention::Checkpoint => "c",
        ContentRetention::Delta => "d",
    };
    workspace_dir(root, workspace)
        .join(hex::encode(lineage.channel))
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

fn add(held: &mut Held, workspace: u64, frame: &Frame, bytes: u64) {
    let entry = Entry {
        seq: frame.seq,
        bytes,
        authored_at_ns: frame.authored_at_ns,
        retention: frame.retention,
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
    workspace: u64,
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
    workspace: u64,
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
            let checkpoint = entries
                .iter()
                .filter(|entry| entry.retention == ContentRetention::Checkpoint)
                .map(|entry| entry.seq)
                .max();
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
    use crate::client;
    use collab_types::{
        ArtifactId, AttestationId, ChannelId, Ed25519PublicKey, EncryptionMetadata, OpKind,
        SignedOp, sign_op,
    };
    use ed25519_dalek::SigningKey;
    use neighbour_wire::peer_handle;
    use std::time::Duration;
    use web_time::{SystemTime, UNIX_EPOCH};

    const WORKSPACE: u64 = 7;
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
    fn frame(key: &SigningKey, channel: ChannelId, seq: u64, retention: ContentRetention) -> Frame {
        let mut op = SignedOp::new_unsigned(
            vec![seq as u8; 64],
            ArtifactId::new(),
            Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
            AttestationId::new(),
            OpKind::Change,
            at(seq),
            seq,
        );
        op.encryption = Some(EncryptionMetadata::content_key(1, channel, retention));
        sign_op(&mut op, key).unwrap();
        let message = NeighbourMsg::AppDocument {
            peer: peer_handle(&key.verifying_key().to_bytes()),
            app_id: APP.into(),
            cluster_id: CLUSTER.into(),
            record_id: RECORD.into(),
            envelope: Box::new(op),
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

    #[test]
    fn replay_continues_after_a_cursor() {
        let (_dir, store) = open();
        let key = device(2);
        let channel = ChannelId::new();
        let frames: Vec<Frame> = (1..=3)
            .map(|seq| frame(&key, channel, seq, ContentRetention::Delta))
            .collect();
        for frame in &frames {
            assert_eq!(store.retain(WORKSPACE, frame).unwrap(), Retained::New);
        }
        let lineage = frames[0].lineage;
        assert_eq!(
            store.lineages(WORKSPACE),
            vec![(lineage, 1, 3)],
            "one lineage holds the three frames"
        );

        let all = store.replay(WORKSPACE, &[], Budget::default()).unwrap();
        assert_eq!(all.frames.len(), 3);
        assert!(all.floors.is_empty(), "nothing was retired");
        assert!(!all.more);

        let later = store
            .replay(WORKSPACE, &[cursor(lineage, 2)], Budget::default())
            .unwrap();
        assert_eq!(later.frames, vec![frames[2].message.clone()]);

        assert_eq!(
            store.retain(WORKSPACE, &frames[2]).unwrap(),
            Retained::Duplicate,
            "the same frame is kept once"
        );
        assert!(
            store
                .replay(WORKSPACE + 1, &[], Budget::default())
                .unwrap()
                .frames
                .is_empty()
        );
    }

    #[test]
    fn a_checkpoint_retires_only_its_own_lineage() {
        let (_dir, store) = open();
        let channel = ChannelId::new();
        let author = device(2);
        let other = device(3);
        for seq in 1..=2 {
            store
                .retain(
                    WORKSPACE,
                    &frame(&author, channel, seq, ContentRetention::Delta),
                )
                .unwrap();
        }
        let theirs = frame(&other, channel, 1, ContentRetention::Delta);
        store.retain(WORKSPACE, &theirs).unwrap();
        let checkpoint = frame(&author, channel, 3, ContentRetention::Checkpoint);
        store.retain(WORKSPACE, &checkpoint).unwrap();
        store
            .retain(
                WORKSPACE,
                &frame(&author, channel, 4, ContentRetention::Delta),
            )
            .unwrap();

        let lineages = store.lineages(WORKSPACE);
        assert_eq!(
            lineages,
            vec![(checkpoint.lineage, 3, 4), (theirs.lineage, 1, 1)],
            "the checkpoint retired the author's earlier frames and nothing else"
        );
        let replay = store.replay(WORKSPACE, &[], Budget::default()).unwrap();
        let seqs: Vec<u64> = replay
            .frames
            .iter()
            .map(|message| match message {
                NeighbourMsg::AppDocument { envelope, .. } => envelope.seq,
                other => panic!("a document frame, not {other:?}"),
            })
            .collect();
        assert_eq!(seqs, vec![3, 4, 1], "lineage by lineage, in sequence order");
    }

    #[test]
    fn replay_reports_a_floor_it_cannot_continue_from() {
        let (_dir, store) = open();
        let key = device(2);
        let channel = ChannelId::new();
        for seq in 1..=5 {
            store
                .retain(
                    WORKSPACE,
                    &frame(&key, channel, seq, ContentRetention::Delta),
                )
                .unwrap();
        }
        let lineage = store
            .replay(WORKSPACE, &[], Budget::default())
            .unwrap()
            .frames[0]
            .clone();
        let lineage = Frame::of(&lineage).unwrap().lineage;
        let retired = store.compact(WORKSPACE, 4).unwrap();
        assert_eq!(retired.frames, 3, "the three oldest frames went");

        let early = store
            .replay(WORKSPACE, &[cursor(lineage, 1)], Budget::default())
            .unwrap();
        assert_eq!(
            early.floors,
            vec![Floor {
                lineage,
                earliest_seq: 4
            }]
        );
        assert_eq!(client::gaps(&[cursor(lineage, 1)], &early).len(), 1);
        assert!(
            client::gaps(&[cursor(lineage, 3)], &early).is_empty(),
            "a cursor leading straight into the earliest frame has no gap"
        );
    }

    #[test]
    fn replay_stops_at_its_budget_and_says_more() {
        let (_dir, store) = open();
        let key = device(2);
        let channel = ChannelId::new();
        let frames: Vec<Frame> = (1..=5)
            .map(|seq| frame(&key, channel, seq, ContentRetention::Delta))
            .collect();
        for frame in &frames {
            store.retain(WORKSPACE, frame).unwrap();
        }
        let budget = Budget {
            max_frames: 2,
            max_bytes: 8 * 1024 * 1024,
        };
        let mut cursors = Vec::new();
        let mut rounds = 0;
        loop {
            let replay = store.replay(WORKSPACE, &cursors, budget).unwrap();
            client::advance(&mut cursors, &replay);
            rounds += 1;
            if !replay.more {
                break;
            }
        }
        assert_eq!(rounds, 3, "five frames in rounds of two");
        assert_eq!(cursors, vec![cursor(frames[0].lineage, 5)]);
    }

    #[test]
    fn a_bounded_appetite_retires_the_oldest_and_keeps_a_checkpoint() {
        let (dir, store) = open();
        let key = device(2);
        let channel = ChannelId::new();
        for seq in 1..=2 {
            store
                .retain(
                    WORKSPACE,
                    &frame(&key, channel, seq, ContentRetention::Delta),
                )
                .unwrap();
        }
        let checkpoint = frame(&key, channel, 3, ContentRetention::Checkpoint);
        store.retain(WORKSPACE, &checkpoint).unwrap();
        let size = store.bytes();
        let path = frame_path(
            dir.path(),
            WORKSPACE,
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
            reopened.lineages(WORKSPACE),
            vec![(checkpoint.lineage, 3, 3)]
        );
        assert!(fs::read(&path).is_ok());
    }

    #[test]
    fn compaction_retires_the_frames_a_time_covers() {
        let (_dir, store) = open();
        let key = device(2);
        let channel = ChannelId::new();
        for seq in 1..=3 {
            store
                .retain(
                    WORKSPACE,
                    &frame(&key, channel, seq, ContentRetention::Delta),
                )
                .unwrap();
        }
        let retired = store.compact(WORKSPACE, 3).unwrap();
        assert_eq!(retired.frames, 2);
        assert!(retired.bytes > 0);
        assert_eq!(store.lineages(WORKSPACE).len(), 1);
        assert_eq!(
            store.compact(WORKSPACE + 1, u64::MAX).unwrap(),
            Retired::default()
        );
    }

    #[test]
    fn reopening_reads_back_what_the_store_acknowledged() {
        let (dir, store) = open();
        let key = device(2);
        let channel = ChannelId::new();
        let frames: Vec<Frame> = (1..=3)
            .map(|seq| frame(&key, channel, seq, ContentRetention::Delta))
            .collect();
        for frame in &frames {
            store.retain(WORKSPACE, frame).unwrap();
        }
        let bytes = store.bytes();
        drop(store);

        let reopened = Store::open(dir.path(), Appetite::StoreAll).unwrap();
        assert_eq!(reopened.bytes(), bytes);
        let replay = reopened.replay(WORKSPACE, &[], Budget::default()).unwrap();
        assert_eq!(replay.frames.len(), 3);
    }

    #[test]
    fn a_file_that_is_not_a_frame_is_not_served() {
        let (dir, store) = open();
        let key = device(2);
        let channel = ChannelId::new();
        let frame = frame(&key, channel, 1, ContentRetention::Delta);
        store.retain(WORKSPACE, &frame).unwrap();
        let stray = frame_path(
            dir.path(),
            WORKSPACE,
            frame.lineage,
            2,
            ContentRetention::Delta,
        );
        fs::write(&stray, b"not a frame").unwrap();
        drop(store);

        let reopened = Store::open(dir.path(), Appetite::StoreAll).unwrap();
        let replay = reopened.replay(WORKSPACE, &[], Budget::default()).unwrap();
        assert_eq!(replay.frames.len(), 1, "the stray file is skipped");
        assert!(stray.exists(), "and left on disk for an operator");
    }

    #[test]
    fn only_signed_sealed_document_frames_are_kept() {
        let key = device(2);
        let channel = ChannelId::new();
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
        unsigned.encryption = Some(EncryptionMetadata::content_key(
            1,
            channel,
            ContentRetention::Delta,
        ));
        let message = NeighbourMsg::AppDocument {
            peer: 0,
            app_id: APP.into(),
            cluster_id: CLUSTER.into(),
            record_id: RECORD.into(),
            envelope: Box::new(unsigned),
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
            record_id: RECORD.into(),
            envelope: Box::new(cleartext),
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
        mls.encryption = Some(EncryptionMetadata::mls_application(1, channel));
        sign_op(&mut mls, &key).unwrap();
        let message = NeighbourMsg::AppDocument {
            peer: 0,
            app_id: APP.into(),
            cluster_id: CLUSTER.into(),
            record_id: RECORD.into(),
            envelope: Box::new(mls),
        };
        assert_eq!(Frame::of(&message), Err(Refusal::NotSealed));
    }
}
