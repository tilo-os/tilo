#![cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the journal is the host's own file record")
)]
//! The record kept as an index and, per workspace, a snapshot with the journal
//! of what each publication since changed ([`super::journal`]). A host's files
//! ([`super::file_store`]) and a page's store keep the record this way alike.
//!
//! A store that keeps the record this way writes what a publication changed and
//! not the record: a note added to a notebook holding thousands appends that
//! note's change, and the record is whole again when it is read back. The
//! snapshot and the index are small compact JSON; each journal entry is the
//! compact binary delta of one publication.
//!
//! A snapshot keeps the workspace's replica apart, as content-defined chunks
//! each kept once under its digest, and the list of them: a snapshot written
//! again writes the chunks that changed since the last one, its list and its
//! fields, never the history it already kept.
//!
//! The index names each workspace's snapshot at the revision that snapshot was
//! written at, and a snapshot, list or journal the index does not name is not
//! the record's. A publication's own commit is its journal entry: one an
//! interrupted write cut short fails its digest and ends the journal there, so
//! a record never reads a publication that was not acknowledged.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tilo_desktop_model::WorkspaceId;

use super::journal;
use super::stored::{DurablePeer, StoredPendingEnrollment, StoredWorkspace, WORKSPACE_VERSION};
use super::{Change, NativeAuthorityStorage};

/// The index's place among the parts.
pub(crate) const INDEX_NAME: &str = "index";
/// The most a journal holds past the larger of its snapshot and this floor
/// before the next publication writes a snapshot again.
const JOURNAL_FLOOR: usize = 256 * 1024;
/// The layout an index is: from this version on, a snapshot may keep its
/// replica as chunks beside it, which an earlier build would not read.
const INDEX_VERSION: u32 = 3;
/// The oldest index this build reads: versions 1 and 2 name snapshots that
/// hold their replica within them.
const OLDEST_INDEX: u32 = 1;
/// The length of a chunk's digest in a snapshot's list.
const DIGEST: usize = 32;

/// Where a journaled record keeps its parts, by name: the index, and each
/// workspace's snapshot, its chunks and its journal.
pub trait JournalParts: collab_types::signer::MaybeSendSync {
    /// The part `name` holds, if any.
    fn read_part(&self, name: &str) -> Result<Option<Vec<u8>>, String>;
    /// Keep `bytes` as the part `name`, replacing what it held.
    fn write_part(&self, name: &str, bytes: &[u8]) -> Result<(), String>;
    /// Append `bytes` to the part `name`, which need not exist yet.
    fn append_part(&self, name: &str, bytes: &[u8]) -> Result<(), String>;
    /// `bytes`, a snapshot of the workspace entry's version `stored`, brought up to this build's in
    /// the part `name`: a store of files writes the brought-up bytes there and keeps `bytes` beside
    /// it as `<name>.v<stored>`. The record names `name` only through the index, so a crash leaves
    /// the snapshot and journal the earlier build wrote. A store that keeps no layout older than
    /// its build reads returns them as they are.
    fn bring_up_part(&self, _name: &str, bytes: Vec<u8>, _stored: u32) -> Result<Vec<u8>, String> {
        Ok(bytes)
    }
    /// Let the part `name` go. A store that cannot forget a name empties it:
    /// an empty part names nothing.
    fn remove_part(&self, name: &str) -> Result<(), String> {
        self.write_part(name, &[])
    }
}

/// The index of the record kept apart from its workspaces.
#[derive(Serialize, Deserialize)]
struct Index {
    version: u32,
    /// Raised by every publication the index was written with.
    revision: u64,
    /// The revision each held workspace's snapshot was written at.
    held: BTreeMap<WorkspaceId, u64>,
    #[serde(default)]
    pending: BTreeMap<WorkspaceId, StoredPendingEnrollment>,
}

/// A workspace's snapshot journal, as this store last appended to it.
struct Kept {
    /// The revision of the snapshot it continues.
    revision: u64,
    /// What its next entry is a delta of.
    written: journal::Written,
    /// What the snapshot holds, its replica's chunks included.
    snapshot: usize,
    appended: usize,
}

/// A workspace's snapshot of an earlier version, read but not yet brought up: the store writes the
/// pair it brings the workspace up with at a snapshot of this build's, so the earlier build's
/// snapshot and journal stay the record's until the index names the new pair.
struct Earlier {
    workspace: WorkspaceId,
    snapshot: Vec<u8>,
    version: u32,
    ownership: Option<serde_json::Value>,
    entries: Option<Vec<u8>>,
    list: Option<Vec<u8>>,
}

/// What one workspace's parts hold: the record as this build reads it, or the pair an earlier
/// build wrote, still to be brought up.
enum Read {
    Current(Box<StoredWorkspace>),
    Earlier(Earlier),
}

/// The record `parts` keeps.
pub struct Journaled {
    index: Index,
    /// The snapshot and journal of each workspace this store wrote or read:
    /// a workspace read back writes a snapshot first.
    kept: BTreeMap<WorkspaceId, Kept>,
    /// The chunks each snapshot lists, by its workspace and revision: those
    /// the index names, and one written since that it does not name yet.
    chunks: BTreeMap<(WorkspaceId, u64), Vec<[u8; DIGEST]>>,
    /// Every part this store wrote or read, so one the index no longer names
    /// is let go, and a chunk kept already is not written again.
    written: BTreeSet<String>,
}

impl Journaled {
    /// The record `parts` keeps, and whether there was one: `None` when no
    /// index is kept, which is a store that holds nothing of it yet.
    ///
    /// # Errors
    /// The index is kept and cannot be read, or names a part that is not
    /// there or does not read as the record. Nothing is guessed at: the caller
    /// starts from no record rather than from part of one. A record brought up
    /// from an earlier version that cannot be written fails here, the pair the
    /// earlier build wrote untouched.
    pub fn open(
        parts: &dyn JournalParts,
    ) -> Result<Option<(Self, NativeAuthorityStorage)>, String> {
        let Some(index) = read_index(parts)? else {
            return Ok(None);
        };
        let mut journaled = Self {
            index,
            kept: BTreeMap::new(),
            chunks: BTreeMap::new(),
            written: BTreeSet::from([INDEX_NAME.to_owned()]),
        };
        let held = journaled.index.held.clone();
        let mut revision = journaled.index.revision;
        let mut workspaces = BTreeMap::new();
        let mut earlier = Vec::new();
        for (&workspace, &snapshot_revision) in &held {
            let (read, newest) = journaled.read_workspace(parts, workspace, snapshot_revision)?;
            revision = revision.max(newest.unwrap_or(0));
            match read {
                Read::Current(stored) => {
                    workspaces.insert(workspace, *stored);
                }
                Read::Earlier(read) => earlier.push(read),
            }
        }
        let mut record = NativeAuthorityStorage {
            revision,
            workspaces,
            pending: journaled.index.pending.clone(),
        };
        journaled.write_brought_up(parts, &mut record, earlier)?;
        Ok(Some((journaled, record)))
    }

    /// Read one workspace's parts the index names: the record as its snapshot and journal leave it,
    /// or the snapshot and journal of an earlier version, still to be brought up, with the revision
    /// of the journal's last entry.
    fn read_workspace(
        &mut self,
        parts: &dyn JournalParts,
        workspace: WorkspaceId,
        snapshot_revision: u64,
    ) -> Result<(Read, Option<u64>), String> {
        let name = snapshot_name(workspace, snapshot_revision);
        let snapshot = parts
            .read_part(&name)?
            .ok_or_else(|| format!("the record names {workspace}'s snapshot and it is missing"))?;
        self.written.insert(name.clone());
        // The layout the snapshot names: one this build writes reads as it is; an earlier one is
        // read as that build wrote it, and brought up into a snapshot of this build's at the end,
        // the journal beside it folded in, so the pair on disk is one build's or the other.
        let version = stored_version(&name, &snapshot)?;
        if version < WORKSPACE_VERSION {
            let read = self.read_earlier(parts, workspace, snapshot_revision, snapshot, version)?;
            return Ok((Read::Earlier(read), None));
        }
        // A snapshot this build reads as it is; one a later build wrote is refused here.
        let snapshot = parts.bring_up_part(&name, snapshot, version)?;
        let (stored, newest) = self.read_current(parts, workspace, snapshot_revision, &snapshot)?;
        Ok((Read::Current(Box::new(stored)), newest))
    }

    /// The snapshot and journal the earlier build wrote, kept as they are until the pair this build
    /// writes is named. A journal is a delta of the snapshot beside it, and one beside the snapshot
    /// of the build before this one is a delta of the ownership that snapshot held, which its step
    /// drops, so that is read too.
    fn read_earlier(
        &mut self,
        parts: &dyn JournalParts,
        workspace: WorkspaceId,
        snapshot_revision: u64,
        snapshot: Vec<u8>,
        version: u32,
    ) -> Result<Earlier, String> {
        let ownership = journal::ownership_before(&snapshot)
            .map_err(|error| format!("{workspace}'s snapshot does not read: {error}"))?;
        let log = journal_name(workspace, snapshot_revision);
        let entries = match parts.read_part(&log)? {
            Some(entries) => {
                self.written.insert(log);
                Some(entries)
            }
            None => None,
        };
        let name = list_name(workspace, snapshot_revision);
        let list = match parts.read_part(&name)? {
            Some(list) => {
                self.written.insert(name);
                Some(list)
            }
            None => None,
        };
        Ok(Earlier {
            workspace,
            snapshot,
            version,
            ownership,
            entries,
            list,
        })
    }

    /// The record a snapshot of this build's version and the journal beside it leave, with the
    /// revision of the journal's last entry. What a publication after this open appends to is kept.
    fn read_current(
        &mut self,
        parts: &dyn JournalParts,
        workspace: WorkspaceId,
        snapshot_revision: u64,
        snapshot: &[u8],
    ) -> Result<(StoredWorkspace, Option<u64>), String> {
        let mut size = snapshot.len();
        let mut stored: StoredWorkspace = serde_json::from_slice(snapshot)
            .map_err(|error| format!("{workspace}'s snapshot does not read: {error}"))?;
        stored.version = WORKSPACE_VERSION;
        // A snapshot an earlier build wrote holds its replica; one this
        // build wrote lists its chunks beside it.
        let list = list_name(workspace, snapshot_revision);
        if let Some(list_bytes) = parts.read_part(&list)? {
            let digests = read_list(&list_bytes)?;
            self.written.insert(list);
            let mut peer = Vec::new();
            for digest in &digests {
                let chunk_name = chunk_name(workspace, digest);
                let chunk = parts.read_part(&chunk_name)?.ok_or_else(|| {
                    format!("{workspace}'s snapshot lists a chunk that is missing")
                })?;
                if blake3::hash(&chunk).as_bytes() != digest {
                    return Err(format!("a chunk of {workspace}'s snapshot does not read"));
                }
                peer.extend_from_slice(&chunk);
                self.written.insert(chunk_name);
            }
            size = size.saturating_add(peer.len());
            stored.peer = DurablePeer::read(&peer);
            self.chunks.insert((workspace, snapshot_revision), digests);
        } else if stored.peer.layout().len() == 0 {
            return Err(format!(
                "the record names {workspace}'s chunks and their list is missing"
            ));
        }
        let mut whole = true;
        let mut appended = 0;
        let mut journal_revision = None;
        let log = journal_name(workspace, snapshot_revision);
        if let Some(entries) = parts.read_part(&log)? {
            self.written.insert(log);
            whole = journal::read_whole(&entries);
            appended = entries.len();
            let (replayed, newest) = journal::replay(stored, &entries, WORKSPACE_VERSION, None)
                .map_err(|error| format!("{workspace}'s journal does not read: {error}"))?;
            stored = replayed;
            journal_revision = newest;
        }
        if stored.workspace != workspace {
            return Err(format!("{workspace}'s record holds another workspace"));
        }
        // The next publication goes on with the journal as it was read, so a write after a start
        // costs what it changed; a journal a write cut short is let go with a snapshot first, as
        // nothing after its torn entry would read.
        if whole {
            self.kept.insert(
                workspace,
                Kept {
                    revision: snapshot_revision,
                    written: journal::first_of(&stored)?,
                    snapshot: size,
                    appended,
                },
            );
        }
        Ok((stored, journal_revision))
    }

    /// Write each workspace whose snapshot was brought up as one snapshot of this build's version
    /// with an empty journal beside it, then the index naming them: the index is the commit point
    /// of a change, so a crash at any point leaves either the pair the earlier build wrote, its
    /// journal with the writes no link took yet, or the pair this build wrote. Each file is
    /// replaced whole or not at all ([`JournalParts::write_part`]).
    fn write_brought_up(
        &mut self,
        parts: &dyn JournalParts,
        record: &mut NativeAuthorityStorage,
        earlier: Vec<Earlier>,
    ) -> Result<(), String> {
        if earlier.is_empty() {
            return Ok(());
        }
        let revision = record
            .revision
            .checked_add(1)
            .ok_or("the record's revision is exhausted")?;
        let mut held = self.index.held.clone();
        for read in earlier {
            let stored = self.fold(parts, read, revision)?;
            self.write_snapshot(parts, stored.workspace, revision, &stored)?;
            held.insert(stored.workspace, revision);
            record.workspaces.insert(stored.workspace, stored);
        }
        self.name_pairs(parts, record, held, revision)
    }

    /// The record the journal the earlier build appended leaves: its snapshot brought up at
    /// `revision`, with what that build wrote kept beside it, and the journal's entries applied.
    fn fold(
        &mut self,
        parts: &dyn JournalParts,
        read: Earlier,
        revision: u64,
    ) -> Result<StoredWorkspace, String> {
        let workspace = read.workspace;
        let name = snapshot_name(workspace, revision);
        // The brought-up bytes go to the snapshot this build names, the bytes the earlier build
        // wrote kept beside it: neither is the record's until the index below is.
        let snapshot = parts.bring_up_part(&name, read.snapshot, read.version)?;
        self.written.insert(name);
        let mut stored: StoredWorkspace = serde_json::from_slice(&snapshot)
            .map_err(|error| format!("{workspace}'s snapshot does not read: {error}"))?;
        stored.version = WORKSPACE_VERSION;
        if let Some(list) = read.list {
            let digests = read_list(&list)?;
            let mut peer = Vec::new();
            for digest in &digests {
                let chunk_name = chunk_name(workspace, digest);
                let chunk = parts.read_part(&chunk_name)?.ok_or_else(|| {
                    format!("{workspace}'s snapshot lists a chunk that is missing")
                })?;
                if blake3::hash(&chunk).as_bytes() != digest {
                    return Err(format!("a chunk of {workspace}'s snapshot does not read"));
                }
                peer.extend_from_slice(&chunk);
                self.written.insert(chunk_name);
            }
            stored.peer = DurablePeer::read(&peer);
        } else if stored.peer.layout().len() == 0 {
            return Err(format!(
                "the record names {workspace}'s chunks and their list is missing"
            ));
        }
        if let Some(entries) = read.entries {
            let (replayed, _) =
                journal::replay(stored, &entries, read.version, read.ownership.as_ref())
                    .map_err(|error| format!("{workspace}'s journal does not read: {error}"))?;
            stored = replayed;
        }
        if stored.workspace != workspace {
            return Err(format!("{workspace}'s record holds another workspace"));
        }
        Ok(stored)
    }

    /// Name the pairs just written in the index, the commit point of the record, and let go the
    /// parts it no longer names.
    fn name_pairs(
        &mut self,
        parts: &dyn JournalParts,
        record: &mut NativeAuthorityStorage,
        held: BTreeMap<WorkspaceId, u64>,
        revision: u64,
    ) -> Result<(), String> {
        let index = Index {
            version: INDEX_VERSION,
            revision,
            held,
            pending: record.pending.clone(),
        };
        let bytes = serde_json::to_vec(&index).map_err(|error| error.to_string())?;
        parts.write_part(INDEX_NAME, &bytes)?;
        self.written.insert(INDEX_NAME.to_owned());
        self.index = index;
        record.revision = revision;
        let held = &self.index.held;
        self.chunks
            .retain(|(workspace, revision), _| held.get(workspace) == Some(revision));
        self.prune(parts);
        Ok(())
    }

    /// The record `parts` keeps from `record`, which this store takes as
    /// already published and writes whole: the record a store held before it
    /// kept its parts apart.
    #[must_use]
    pub fn start(record: &NativeAuthorityStorage) -> Self {
        Self {
            index: Index {
                version: INDEX_VERSION,
                revision: record.revision,
                held: BTreeMap::new(),
                pending: record.pending.clone(),
            },
            kept: BTreeMap::new(),
            chunks: BTreeMap::new(),
            written: BTreeSet::new(),
        }
    }

    /// Go on from the replicas `owner` restored from the record [`Self::open`]
    /// read, as they encode themselves: the next write of each appends what
    /// it changed. One whose replica encodes other bytes writes a snapshot
    /// first.
    pub fn resume(&mut self, owner: &super::DeviceWorkspaces) {
        self.kept
            .retain(|workspace, kept| owner.resume_journal(*workspace, &mut kept.written));
    }

    /// The revision of the record as the index last named it.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.index.revision
    }

    /// Every part the record names: a part a store keeps beside these that is
    /// not among them was left by a publication that stopped before its index.
    pub(crate) fn named(&self) -> BTreeSet<String> {
        let mut named = BTreeSet::from([INDEX_NAME.to_owned()]);
        for (workspace, revision) in &self.index.held {
            named.insert(snapshot_name(*workspace, *revision));
            named.insert(journal_name(*workspace, *revision));
            if let Some(digests) = self.chunks.get(&(*workspace, *revision)) {
                named.insert(list_name(*workspace, *revision));
                for digest in digests {
                    named.insert(chunk_name(*workspace, digest));
                }
            }
        }
        named
    }

    /// Make `next` durable as `change` says it changed: the changed
    /// workspace's journal entry appended, or a snapshot where its journal is
    /// due a new one, and the index last.
    ///
    /// # Errors
    /// A part could not be written. Nothing the index names was published then.
    pub fn publish(
        &mut self,
        parts: &dyn JournalParts,
        next: &NativeAuthorityStorage,
        change: &Change,
    ) -> Result<(), String> {
        self.publish_indexed(parts, next, change).map(drop)
    }

    /// [`Self::publish`], and whether it wrote the index: `false` when it
    /// appended to a journal alone.
    pub(crate) fn publish_indexed(
        &mut self,
        parts: &dyn JournalParts,
        next: &NativeAuthorityStorage,
        change: &Change,
    ) -> Result<bool, String> {
        let changed = match change {
            Change::Workspace { workspace, .. } => Some(*workspace),
            Change::Pending | Change::Released { .. } => None,
        };
        if let Some(workspace) = changed
            && next.holds(workspace)
            && self.append(parts, next, workspace)?
        {
            // The journal's last entry carries the revision: the index names
            // the snapshot only, and is written when a snapshot is.
            self.index.revision = next.revision;
            return Ok(false);
        }
        // A snapshot is written for the workspace that changed, and for every
        // workspace the index has none for yet.
        let mut held = self.index.held.clone();
        held.retain(|workspace, _| next.holds(*workspace));
        self.kept.retain(|workspace, _| next.holds(*workspace));
        let mut writing: Vec<WorkspaceId> = changed
            .filter(|workspace| next.holds(*workspace))
            .into_iter()
            .collect();
        // A workspace this store has no snapshot for yet is written at this
        // revision; one it has is left as it was (its journal carries what it
        // changed).
        for workspace in next.workspaces.keys() {
            if !held.contains_key(workspace) && !writing.contains(workspace) {
                writing.push(*workspace);
            }
        }
        for workspace in writing {
            let stored = next
                .stored(workspace)
                .ok_or("the published workspace is not in the record")?;
            self.write_snapshot(parts, workspace, next.revision, stored)?;
            held.insert(workspace, next.revision);
        }
        // The index names each workspace's snapshot at the revision it was
        // written at, and is the commit point: once it is written, the record
        // holds the change, and the snapshot it named before is not the
        // record's.
        let index = Index {
            version: INDEX_VERSION,
            revision: next.revision,
            held,
            pending: next.pending.clone(),
        };
        let bytes = serde_json::to_vec(&index).map_err(|error| error.to_string())?;
        parts.write_part(INDEX_NAME, &bytes)?;
        self.written.insert(INDEX_NAME.to_owned());
        self.index = index;
        let held = &self.index.held;
        self.chunks
            .retain(|(workspace, revision), _| held.get(workspace) == Some(revision));
        self.prune(parts);
        Ok(true)
    }

    /// Append what `next` changed of `workspace` to its journal: `false`,
    /// with nothing written, when the publication changes what the index
    /// says, or the journal is due a new snapshot.
    fn append(
        &mut self,
        parts: &dyn JournalParts,
        next: &NativeAuthorityStorage,
        workspace: WorkspaceId,
    ) -> Result<bool, String> {
        let Some(stored) = next.stored(workspace) else {
            return Ok(false);
        };
        // The workspaces held and the enrollments in flight are what the index
        // names: a publication that changed either writes the index.
        if self.index.held.len() != next.workspaces.len()
            || next
                .workspaces
                .keys()
                .any(|held_here| !self.index.held.contains_key(held_here))
            || serde_json::to_vec(&next.pending).ok()
                != serde_json::to_vec(&self.index.pending).ok()
        {
            return Ok(false);
        }
        let held_at = self.index.held.get(&workspace).copied();
        let Some(kept) = self.kept.get_mut(&workspace) else {
            return Ok(false);
        };
        if held_at != Some(kept.revision) {
            return Ok(false);
        }
        let (entry, written) = journal::next(&kept.written, stored, next.revision)?;
        let appended = kept.appended.saturating_add(entry.len());
        // A journal past its snapshot is due a new one.
        if appended > kept.snapshot.max(JOURNAL_FLOOR) {
            return Ok(false);
        }
        let name = journal_name(workspace, kept.revision);
        parts.append_part(&name, &entry)?;
        self.written.insert(name);
        kept.written = written;
        kept.appended = appended;
        Ok(true)
    }

    /// Write `workspace`'s snapshot at `revision`: the chunks of its replica
    /// not kept yet, their list, an empty journal and its fields.
    fn write_snapshot(
        &mut self,
        parts: &dyn JournalParts,
        workspace: WorkspaceId,
        revision: u64,
        stored: &StoredWorkspace,
    ) -> Result<(), String> {
        let peer = stored.peer.to_vec();
        let mut digests = Vec::new();
        for range in journal::snapshot_chunks(&peer) {
            let chunk = peer.get(range).unwrap_or_default();
            let digest = *blake3::hash(chunk).as_bytes();
            let name = chunk_name(workspace, &digest);
            if self.written.insert(name.clone())
                && let Err(error) = parts.write_part(&name, chunk)
            {
                self.written.remove(&name);
                return Err(error);
            }
            digests.push(digest);
        }
        let list = list_name(workspace, revision);
        parts.write_part(&list, digests.as_flattened())?;
        self.written.insert(list);
        let fields = StoredWorkspace {
            peer: DurablePeer::default(),
            ..stored.clone()
        };
        let snapshot = serde_json::to_vec(&fields).map_err(|error| error.to_string())?;
        let name = snapshot_name(workspace, revision);
        let log = journal_name(workspace, revision);
        // The journal, empty, first: the index names the snapshot only after
        // both are there, and a journal the index does not name is let go.
        parts.write_part(&log, &[])?;
        self.written.insert(log);
        parts.write_part(&name, &snapshot)?;
        self.written.insert(name);
        self.kept.insert(
            workspace,
            Kept {
                revision,
                written: journal::first(stored, &snapshot)?,
                snapshot: snapshot.len().saturating_add(peer.len()),
                appended: 0,
            },
        );
        self.chunks.insert((workspace, revision), digests);
        Ok(())
    }

    /// Let go every part the record no longer names: one a workspace was
    /// published over, one a publication stopped before its index wrote, a
    /// chunk no snapshot lists any more, and the parts of a workspace this
    /// device released.
    fn prune(&mut self, parts: &dyn JournalParts) {
        let named = self.named();
        let gone: Vec<String> = self
            .written
            .iter()
            .filter(|name| !named.contains(*name))
            .cloned()
            .collect();
        for name in gone {
            if let Err(error) = parts.remove_part(&name) {
                tracing::warn!(%error, part = %name, "a record part no longer named was kept");
                continue;
            }
            self.written.remove(&name);
        }
    }
}

/// The version a snapshot's bytes name.
#[cfg(not(target_arch = "wasm32"))]
fn stored_version(name: &str, bytes: &[u8]) -> Result<u32, String> {
    super::stored::format::stored_version(bytes).map_err(|error| format!("{name}: {error}"))
}

/// The version a snapshot's bytes name, which a page reads as they are: its
/// store kept no layout older than its build reads.
#[cfg(target_arch = "wasm32")]
fn stored_version(name: &str, bytes: &[u8]) -> Result<u32, String> {
    #[derive(Deserialize)]
    struct Versioned {
        version: u32,
    }
    serde_json::from_slice::<Versioned>(bytes)
        .map(|entry| entry.version)
        .map_err(|error| format!("{name}: {error}"))
}

/// The index `parts` keeps, or `None` when no record is kept yet.
fn read_index(parts: &dyn JournalParts) -> Result<Option<Index>, String> {
    let Some(bytes) = parts.read_part(INDEX_NAME)? else {
        return Ok(None);
    };
    let index: Index = serde_json::from_slice(&bytes)
        .map_err(|error| format!("the workspaces' index does not read: {error}"))?;
    if !(OLDEST_INDEX..=INDEX_VERSION).contains(&index.version) {
        return Err(format!(
            "the workspaces' index is version {}, and this build reads versions \
             {OLDEST_INDEX} to {INDEX_VERSION}",
            index.version
        ));
    }
    Ok(Some(index))
}

/// The digests a snapshot's list of chunks holds, in order.
fn read_list(bytes: &[u8]) -> Result<Vec<[u8; DIGEST]>, String> {
    let (digests, rest) = bytes.as_chunks::<DIGEST>();
    if !rest.is_empty() {
        return Err("a snapshot's list of chunks does not read".into());
    }
    Ok(digests.to_vec())
}

pub(crate) fn snapshot_name(workspace: WorkspaceId, revision: u64) -> String {
    format!("w{workspace}.{revision}")
}

pub(crate) fn journal_name(workspace: WorkspaceId, revision: u64) -> String {
    format!("l{workspace}.{revision}")
}

pub(crate) fn list_name(workspace: WorkspaceId, revision: u64) -> String {
    format!("k{workspace}.{revision}")
}

pub(crate) fn chunk_name(workspace: WorkspaceId, digest: &[u8; DIGEST]) -> String {
    format!(
        "c{workspace}.{}",
        blake3::Hash::from_bytes(*digest).to_hex()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owner::stored::StoredWorkspace;
    use crate::owner::{Change, NativeAuthorityStorage};
    use ed25519_dalek::SigningKey;
    use std::sync::Mutex;

    /// Parts kept in memory, and what was appended to them.
    #[derive(Default)]
    struct Memory {
        parts: Mutex<BTreeMap<String, Vec<u8>>>,
        appended: Mutex<usize>,
        replaced: Mutex<Vec<String>>,
        /// The bytes every part written whole held.
        rewritten: Mutex<usize>,
        /// The part a write fails on, as a publication that stopped before it.
        fail: Mutex<Option<String>>,
    }

    impl JournalParts for Memory {
        fn read_part(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(self
                .parts
                .lock()
                .map_err(|error| error.to_string())?
                .get(name)
                .cloned())
        }

        fn write_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
            {
                let mut fail = self.fail.lock().map_err(|error| error.to_string())?;
                if fail.as_deref() == Some(name) {
                    *fail = None;
                    return Err(format!("{name} is not written"));
                }
            }
            self.replaced
                .lock()
                .map_err(|error| error.to_string())?
                .push(name.to_owned());
            {
                let mut rewritten = self.rewritten.lock().map_err(|error| error.to_string())?;
                *rewritten = rewritten.saturating_add(bytes.len());
            }
            self.parts
                .lock()
                .map_err(|error| error.to_string())?
                .insert(name.to_owned(), bytes.to_vec());
            Ok(())
        }

        fn append_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
            {
                let mut appended = self.appended.lock().map_err(|error| error.to_string())?;
                *appended = appended.saturating_add(bytes.len());
            }
            self.parts
                .lock()
                .map_err(|error| error.to_string())?
                .entry(name.to_owned())
                .or_default()
                .extend_from_slice(bytes);
            Ok(())
        }
    }

    fn certificate() -> Result<tilo_user_identity::DeviceCert, String> {
        let root = SigningKey::from_bytes(&[1; 32]);
        let key = SigningKey::from_bytes(&[2; 32]);
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "A's desktop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root)
            .map_err(|error| error.to_string())?;
        Ok(cert)
    }

    fn workspace_resource(
        workspace: WorkspaceId,
        peer: &[u8],
        revision: u64,
    ) -> Result<NativeAuthorityStorage, String> {
        let cert = certificate()?;
        let stored = StoredWorkspace {
            version: crate::owner::stored::WORKSPACE_VERSION,
            workspace,
            workspace_host: cert.clone(),
            device: cert,
            enrollment: None,
            vouched: None,
            host_hint: None,
            peer: crate::owner::stored::DurablePeer::read(peer),
            generation: 0,
            continuity: 0,
            frames: crate::owner::stored::Frames::default(),
        };
        let mut record = NativeAuthorityStorage {
            revision,
            ..NativeAuthorityStorage::default()
        };
        record.workspaces.insert(workspace, stored);
        Ok(record)
    }

    /// The peer bytes of a record's one workspace.
    fn peer_of(record: &NativeAuthorityStorage, workspace: WorkspaceId) -> Result<Vec<u8>, String> {
        Ok(record
            .stored(workspace)
            .ok_or("the record holds no such workspace")?
            .peer
            .to_vec())
    }

    /// `condition`, or the message as the test's failure.
    fn check(
        condition: bool,
        message: impl Into<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if condition {
            Ok(())
        } else {
            Err(message.into().into())
        }
    }

    #[test]
    fn a_publication_appends_what_it_changed_and_a_reopen_reads_the_record()
    -> Result<(), Box<dyn std::error::Error>> {
        let workspace = WorkspaceId::from_u128(7);
        let parts = Memory::default();
        let first = workspace_resource(workspace, b"page-one", 1)?;
        let mut journaled = Journaled::start(&first);
        journaled.publish(
            &parts,
            &first,
            &Change::Workspace {
                workspace,
                shown: false,
            },
        )?;
        check(
            parts
                .replaced
                .lock()
                .map_err(|error| error.to_string())?
                .iter()
                .any(|name| name == INDEX_NAME),
            "the first publication writes the index",
        )?;
        check(
            *parts.appended.lock().map_err(|error| error.to_string())? == 0,
            "nothing to append yet",
        )?;

        let second = workspace_resource(workspace, b"page-onepage-two", 2)?;
        *parts.appended.lock().map_err(|error| error.to_string())? = 0;
        let replaced_before = parts
            .replaced
            .lock()
            .map_err(|error| error.to_string())?
            .len();
        journaled.publish(
            &parts,
            &second,
            &Change::Workspace {
                workspace,
                shown: false,
            },
        )?;
        let appended = *parts.appended.lock().map_err(|error| error.to_string())?;
        check(appended > 0, "the second publication appends its change")?;
        check(
            appended < 256,
            format!("only what changed crosses: {appended} bytes"),
        )?;
        check(
            parts
                .replaced
                .lock()
                .map_err(|error| error.to_string())?
                .len()
                == replaced_before,
            "nothing is rewritten, only appended",
        )?;

        // The page restarts and reads what it kept.
        let (mut reopened_journal, reopened) =
            Journaled::open(&parts)?.ok_or("the record is not kept")?;
        check(
            peer_of(&reopened, workspace)? == peer_of(&second, workspace)?,
            "the record is what was kept",
        )?;
        check(reopened.revision() == 2, "the revision was kept")?;

        let third = workspace_resource(workspace, b"page-onepage-twopage-three", 3)?;
        *parts.appended.lock().map_err(|error| error.to_string())? = 0;
        reopened_journal.publish(
            &parts,
            &third,
            &Change::Workspace {
                workspace,
                shown: false,
            },
        )?;
        check(
            *parts.appended.lock().map_err(|error| error.to_string())? < 256,
            "the journal continues from what was kept",
        )?;
        let read = Journaled::open(&parts)?.ok_or("the record is not kept")?.1;
        check(
            peer_of(&read, workspace)? == peer_of(&third, workspace)?,
            "the appended record reads back",
        )?;
        check(read.revision() == 3, "the appended revision reads back")?;
        Ok(())
    }

    /// A snapshot written again keeps the chunks of the replica it kept
    /// already: after one small change it writes that change's chunk, its list
    /// and its fields, not the state.
    #[test]
    fn a_snapshot_written_again_after_one_small_change_writes_that_change()
    -> Result<(), Box<dyn std::error::Error>> {
        const STATE: usize = 8 * 1024 * 1024;
        let workspace = WorkspaceId::from_u128(11);
        let parts = Memory::default();
        // Bytes that repeat nowhere, as sealed history is.
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let state: Vec<u8> = (0..STATE)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed.to_le_bytes()[0]
            })
            .collect();
        let first = workspace_resource(workspace, &state, 1)?;
        let mut journaled = Journaled::start(&first);
        let change = Change::Workspace {
            workspace,
            shown: false,
        };
        journaled.publish(&parts, &first, &change)?;

        // A crash cut the journal short: the next publication of the
        // workspace writes its snapshot again.
        parts
            .parts
            .lock()
            .map_err(|error| error.to_string())?
            .insert(journal_name(workspace, 1), vec![200, 0, 0, 0, 1, 2, 3]);
        let (mut reopened, read) = Journaled::open(&parts)?.ok_or("the record is not kept")?;
        check(
            peer_of(&read, workspace)? == state,
            "the snapshot's chunks read back as the state",
        )?;
        let mut changed = state;
        changed
            .get_mut(STATE / 2..STATE / 2 + 16)
            .ok_or("the state is shorter than its middle")?
            .copy_from_slice(b"one small change");
        let second = workspace_resource(workspace, &changed, 2)?;
        *parts.rewritten.lock().map_err(|error| error.to_string())? = 0;
        reopened.publish(&parts, &second, &change)?;
        check(
            parts
                .replaced
                .lock()
                .map_err(|error| error.to_string())?
                .contains(&snapshot_name(workspace, 2)),
            "the snapshot is written again",
        )?;
        let rewritten = *parts.rewritten.lock().map_err(|error| error.to_string())?;
        check(
            rewritten < STATE / 16,
            format!("a snapshot after one small change wrote {rewritten} bytes of {STATE}"),
        )?;
        let read = Journaled::open(&parts)?.ok_or("the record is not kept")?.1;
        check(
            peer_of(&read, workspace)? == changed,
            "the snapshot written again reads back with the change",
        )?;
        check(
            parts
                .parts
                .lock()
                .map_err(|error| error.to_string())?
                .iter()
                .filter(|(name, bytes)| name.starts_with('c') && !bytes.is_empty())
                .map(|(_, bytes)| bytes.len())
                .sum::<usize>()
                == STATE,
            "the chunk the change replaced is let go",
        )?;
        Ok(())
    }

    #[test]
    fn a_released_workspace_leaves_no_part_behind() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = WorkspaceId::from_u128(9);
        let parts = Memory::default();
        let record = workspace_resource(workspace, b"page", 1)?;
        let mut journaled = Journaled::start(&record);
        journaled.publish(
            &parts,
            &record,
            &Change::Workspace {
                workspace,
                shown: false,
            },
        )?;
        let empty = NativeAuthorityStorage::default();
        journaled.publish(&parts, &empty, &Change::Released { workspace })?;
        check(
            Journaled::open(&parts)?
                .ok_or("the record is not kept")?
                .1
                .is_empty(),
            "the record holds no workspace after a release",
        )?;
        check(
            parts
                .parts
                .lock()
                .map_err(|error| error.to_string())?
                .iter()
                .all(|(name, bytes)| name == INDEX_NAME || bytes.is_empty()),
            "every part of the released workspace is let go",
        )?;
        Ok(())
    }

    /// A record a version 10 build kept: its snapshot, holding the ownership a build before it
    /// kept, and an entry that build appended beside it. This build opens it with the entry's
    /// change, the ownership gone, and its next publication writes a snapshot of its own layout.
    #[test]
    fn a_record_the_build_before_kept_opens_with_its_journal()
    -> Result<(), Box<dyn std::error::Error>> {
        use serde::Serialize;
        /// One run of an entry's bytes, as a version 10 build framed it.
        #[derive(Serialize)]
        enum Run {
            Copy { from: u64, len: u64 },
            Bytes(#[serde(with = "serde_bytes")] Vec<u8>),
        }
        /// A stretch of ownership's writes the snapshot held.
        #[derive(Serialize)]
        enum KeptWrites {
            Kept { from: u32, len: u32 },
        }
        /// An entry in that build's layout. Its answers and the writes its server had not taken
        /// are units here: an entry that carries one is what the last case refuses.
        #[derive(Serialize)]
        struct Appended {
            revision: u64,
            fields: Vec<Run>,
            answered: Vec<()>,
            unanswered: Vec<()>,
            writes: [Vec<KeptWrites>; 3],
            frames: Vec<()>,
            head: Vec<Run>,
            documents: Vec<()>,
            tail: Vec<Run>,
        }
        let framed = |entry: &Appended| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
            let payload = postcard::to_allocvec(entry)?;
            let mut framed = u32::try_from(payload.len())?.to_le_bytes().to_vec();
            framed.extend_from_slice(blake3::hash(&payload).as_bytes());
            framed.extend_from_slice(&payload);
            Ok(framed)
        };
        let golden: Vec<u8> = include_str!("../../golden/owner-workspace-document/v10.hex")
            .split_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16))
            .collect::<Result<_, _>>()?;
        let workspace = WorkspaceId::parse("40267648-f762-4c8d-89ad-c5ab27aa13b8")
            .ok_or("the golden entry's workspace")?;
        let parts = Memory::default();
        // The same record without a journal, for the replica the journal's entry leaves as it was.
        let alone_parts = Memory::default();
        let mut held = serde_json::Map::new();
        held.insert(workspace.to_string(), 1.into());
        let index = serde_json::to_vec(&serde_json::json!({
            "version": INDEX_VERSION,
            "revision": 1,
            "held": held
        }))?;
        for parts in [&parts, &alone_parts] {
            parts.write_part(INDEX_NAME, &index)?;
            parts.write_part(&snapshot_name(workspace, 1), &golden)?;
        }
        let alone = Journaled::open(&alone_parts)?
            .ok_or("the record is not kept")?
            .1;
        let held = alone.stored(workspace).ok_or("the snapshot's workspace")?;
        check(held.generation == 1, "the golden entry is of generation 1")?;
        let layout = held.peer.layout();
        let length =
            |parts: &[std::sync::Arc<[u8]>]| -> usize { parts.iter().map(|part| part.len()).sum() };
        let wide = |len: usize| u64::try_from(len).unwrap_or(u64::MAX);
        let kept = |len: usize| {
            if len == 0 {
                Vec::new()
            } else {
                vec![Run::Copy {
                    from: 0,
                    len: wide(len),
                }]
            }
        };

        // That build's entry is a delta of the fields it split from the snapshot: the snapshot's
        // own, without its replica, the ownership last.
        let mut split: serde_json::Value = serde_json::from_slice(&golden)?;
        split["peer"] = serde_json::to_value(crate::owner::stored::DurablePeer::default())?;
        let split = serde_json::to_vec(&split)?;
        let at = split
            .windows(b"\"generation\":1,".len())
            .position(|window| window == b"\"generation\":1,")
            .ok_or("the split names its generation")?
            + b"\"generation\":".len();
        let entry = |answered: Vec<()>| Appended {
            revision: 2,
            fields: vec![
                Run::Copy {
                    from: 0,
                    len: wide(at),
                },
                Run::Bytes(b"7".to_vec()),
                Run::Copy {
                    from: wide(at + 1),
                    len: wide(split.len() - at - 1),
                },
            ],
            answered,
            unanswered: Vec::new(),
            writes: [
                vec![KeptWrites::Kept { from: 0, len: 0 }],
                Vec::new(),
                Vec::new(),
            ],
            frames: Vec::new(),
            head: kept(length(&layout.head)),
            documents: Vec::new(),
            tail: kept(length(&layout.tail)),
        };
        parts.write_part(&journal_name(workspace, 1), &framed(&entry(Vec::new()))?)?;
        // A store that stopped after writing the pair it brings the workspace up with, before the
        // index named it, opens again from the pair the earlier build wrote, the entry read.
        parts
            .fail
            .lock()
            .map_err(|error| error.to_string())?
            .replace(INDEX_NAME.to_owned());
        check(
            Journaled::open(&parts).is_err(),
            "the open stops where the index is written",
        )?;
        {
            let kept = parts.parts.lock().map_err(|error| error.to_string())?;
            check(
                kept.get(&snapshot_name(workspace, 2))
                    .is_some_and(|snapshot| !snapshot.is_empty())
                    && kept
                        .get(&journal_name(workspace, 2))
                        .is_some_and(Vec::is_empty),
                "the brought-up pair is written before the index",
            )?;
            let index: Index =
                serde_json::from_slice(kept.get(INDEX_NAME).ok_or("the index is kept")?)?;
            check(
                index.held.get(&workspace) == Some(&1),
                "the index still names the pair the earlier build wrote",
            )?;
        }
        let (mut journaled, read) = Journaled::open(&parts)?.ok_or("the record is not kept")?;
        let stored = read.stored(workspace).ok_or("the journal's workspace")?;
        check(stored.generation == 7, "the entry's change is read")?;
        check(
            stored.version == crate::owner::stored::WORKSPACE_VERSION,
            "the record reads as this build's",
        )?;
        check(
            peer_of(&read, workspace)? == peer_of(&alone, workspace)?,
            "the replica is the snapshot's",
        )?;

        journaled.publish(
            &parts,
            &read,
            &Change::Workspace {
                workspace,
                shown: false,
            },
        )?;
        let again = Journaled::open(&parts)?.ok_or("the record is not kept")?.1;
        check(
            again.stored(workspace).map(|stored| stored.generation) == Some(7),
            "the next publication keeps what the entry changed",
        )?;
        let (_, kept_now) = parts
            .parts
            .lock()
            .map_err(|error| error.to_string())?
            .iter()
            .filter(|(name, _)| name.starts_with('w'))
            .map(|(name, bytes)| (name.clone(), bytes.clone()))
            .next_back()
            .ok_or("a snapshot is kept")?;
        check(
            crate::owner::stored::format::stored_version(&kept_now)?
                == crate::owner::stored::WORKSPACE_VERSION
                && !String::from_utf8_lossy(&kept_now).contains("\"publication\""),
            "the next publication writes a snapshot of this build's layout, without ownership",
        )?;

        // An entry of that build that carries an answer is refused, never read past.
        let parts_with_answer = Memory::default();
        parts_with_answer.write_part(INDEX_NAME, &index)?;
        parts_with_answer.write_part(&snapshot_name(workspace, 1), &golden)?;
        parts_with_answer.write_part(&journal_name(workspace, 1), &framed(&entry(vec![()]))?)?;
        check(
            Journaled::open(&parts_with_answer).is_err(),
            "an entry carrying ownership's answer is refused",
        )?;
        Ok(())
    }
}
