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
    /// starts from no record rather than from part of one.
    pub fn open(
        parts: &dyn JournalParts,
    ) -> Result<Option<(Self, NativeAuthorityStorage)>, String> {
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
        let mut workspaces = BTreeMap::new();
        let mut kept = BTreeMap::new();
        let mut chunks = BTreeMap::new();
        let mut written = BTreeSet::from([INDEX_NAME.to_owned()]);
        let mut revision = index.revision;
        for (&workspace, &snapshot_revision) in &index.held {
            let name = snapshot_name(workspace, snapshot_revision);
            let snapshot = parts.read_part(&name)?.ok_or_else(|| {
                format!("the record names {workspace}'s snapshot and it is missing")
            })?;
            written.insert(name.clone());
            let mut size = snapshot.len();
            // The layout the snapshot names: one this build writes reads as it
            // is, and an earlier one is brought up, the next publication writing
            // a snapshot of this build's version. A journal is a delta of the
            // snapshot beside it, as that version's bytes: one beside an
            // earlier snapshot is refused.
            let (snapshot_version, snapshot) = brought_up(&name, snapshot)?;
            let mut stored: StoredWorkspace = serde_json::from_slice(&snapshot)
                .map_err(|error| format!("{workspace}'s snapshot does not read: {error}"))?;
            stored.version = WORKSPACE_VERSION;
            // A snapshot an earlier build wrote holds its replica; one this
            // build wrote lists its chunks beside it.
            let list = list_name(workspace, snapshot_revision);
            if let Some(list_bytes) = parts.read_part(&list)? {
                let digests = read_list(&list_bytes)?;
                written.insert(list);
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
                    written.insert(chunk_name);
                }
                size = size.saturating_add(peer.len());
                stored.peer = DurablePeer::read(&peer);
                chunks.insert((workspace, snapshot_revision), digests);
            } else if stored.peer.layout().len() == 0 {
                return Err(format!(
                    "the record names {workspace}'s chunks and their list is missing"
                ));
            }
            let mut whole = snapshot_version == WORKSPACE_VERSION;
            let mut appended = 0;
            let log = journal_name(workspace, snapshot_revision);
            if let Some(entries) = parts.read_part(&log)? {
                written.insert(log);
                whole = whole && journal::read_whole(&entries);
                appended = entries.len();
                let (replayed, newest) = journal::replay(stored, &entries, snapshot_version)
                    .map_err(|error| format!("{workspace}'s journal does not read: {error}"))?;
                stored = replayed;
                revision = revision.max(newest.unwrap_or(0));
            }
            if stored.workspace != workspace {
                return Err(format!("{workspace}'s record holds another workspace"));
            }
            // The next publication goes on with the journal as it was read, so
            // a write after a start costs what it changed; a journal a write cut
            // short is let go with a snapshot first, as nothing after its torn
            // entry would read.
            if whole {
                kept.insert(
                    workspace,
                    Kept {
                        revision: snapshot_revision,
                        written: journal::first_of(&stored)?,
                        snapshot: size,
                        appended,
                    },
                );
            }
            workspaces.insert(workspace, stored);
        }
        let mut record = NativeAuthorityStorage {
            revision,
            workspaces,
            pending: index.pending.clone(),
        };
        drop_recovery_projections(&mut record);
        Ok(Some((
            Self {
                index,
                kept,
                chunks,
                written,
            },
            record,
        )))
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
    pub fn resume(&mut self, owner: &super::NativeWorkspaceOwner) {
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

/// The version a snapshot's bytes name, and the bytes as this build's layout.
#[cfg(not(target_arch = "wasm32"))]
fn brought_up(name: &str, bytes: Vec<u8>) -> Result<(u32, Vec<u8>), String> {
    use super::stored::format;
    let version = format::stored_version(&bytes)?;
    let bytes = format::FORMAT
        .migrate(bytes, version)
        .map_err(|error| format!("{name}: {error}"))?;
    Ok((version, bytes))
}

/// The version a snapshot's bytes name, and the bytes, which a page reads as
/// they are: its store kept no layout older than its build reads.
#[cfg(target_arch = "wasm32")]
fn brought_up(name: &str, bytes: Vec<u8>) -> Result<(u32, Vec<u8>), String> {
    #[derive(Deserialize)]
    struct Versioned {
        version: u32,
    }
    let version = serde_json::from_slice::<Versioned>(&bytes)
        .map_err(|error| format!("{name}: {error}"))?
        .version;
    Ok((version, bytes))
}

/// A recovery projection is a re-signed copy of frames this build re-sends
/// as their signed originals. A record that holds one still loads; the copy
/// is dropped here, and the record keeps its originals held until the
/// workspace retires them.
fn drop_recovery_projections(record: &mut NativeAuthorityStorage) {
    record.workspaces.values_mut().for_each(|workspace| {
        if let Some(publication) = workspace.publication.as_mut() {
            publication.projection.clear();
        }
    });
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
            publication: None,
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
}
