//! The record kept as an index and, per workspace, a snapshot with the journal
//! of what each publication since changed ([`super::journal`]).
//!
//! A store that keeps the record this way writes what a publication changed and
//! not the record: a note added to a notebook holding thousands appends that
//! note's change, and the record is whole again when it is read back. The
//! snapshot and the index are the same small compact JSON a host's files hold;
//! each journal entry is the compact binary delta of one publication.
//!
//! The index names each workspace's snapshot at the revision that snapshot was
//! written at, and a snapshot or journal the index does not name is not the
//! record's. A publication's own commit is its journal entry: one an
//! interrupted write cut short fails its digest and ends the journal there, so
//! a record never reads a publication that was not acknowledged.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tilo_desktop_model::WorkspaceId;

use super::journal;
use super::stored::{StoredPendingEnrollment, StoredWorkspace, WORKSPACE_VERSION};
use super::{Change, NativeAuthorityStorage};

/// The index's place among the parts.
const INDEX_NAME: &str = "index";
/// The most a journal holds past the larger of its snapshot and this floor
/// before the next publication writes a snapshot again.
const JOURNAL_FLOOR: usize = 256 * 1024;

/// Where a journaled record keeps its parts, by name: the index, and each
/// workspace's snapshot and journal.
pub trait JournalParts: collab_types::signer::MaybeSendSync {
    /// The part `name` holds, if any.
    fn read_part(&self, name: &str) -> Result<Option<Vec<u8>>, String>;
    /// Keep `bytes` as the part `name`, replacing what it held.
    fn write_part(&self, name: &str, bytes: &[u8]) -> Result<(), String>;
    /// Append `bytes` to the part `name`, which need not exist yet.
    fn append_part(&self, name: &str, bytes: &[u8]) -> Result<(), String>;
}

/// The index of the record kept apart from its workspaces.
#[derive(Serialize, Deserialize)]
struct Index {
    version: u32,
    /// Raised by every publication the index was written with.
    revision: u64,
    /// The revision each held workspace's snapshot was written at.
    held: BTreeMap<WorkspaceId, u64>,
    pending: BTreeMap<WorkspaceId, StoredPendingEnrollment>,
}

const INDEX_VERSION: u32 = 1;

/// A workspace's snapshot journal, as this store last appended to it.
struct Kept {
    /// The revision of the snapshot it continues.
    revision: u64,
    /// What its next entry is a delta of.
    written: journal::Written,
    snapshot: usize,
    appended: usize,
}

/// The record `parts` keeps.
pub struct Journaled {
    index: Index,
    /// The snapshot and journal of each workspace this store wrote or read:
    /// a workspace read back writes a snapshot first.
    kept: BTreeMap<WorkspaceId, Kept>,
    /// Every part this store wrote, so one the index no longer names is let go.
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
        if index.version != INDEX_VERSION {
            return Err(format!(
                "the workspaces' index is version {}, and this build reads version {INDEX_VERSION}",
                index.version
            ));
        }
        let mut workspaces = BTreeMap::new();
        let mut kept = BTreeMap::new();
        let mut written = BTreeSet::from([INDEX_NAME.to_owned()]);
        let mut revision = index.revision;
        for (workspace, snapshot_revision) in &index.held {
            let name = snapshot_name(*workspace, *snapshot_revision);
            let snapshot = parts.read_part(&name)?.ok_or_else(|| {
                format!("the record names {workspace}'s snapshot and it is missing")
            })?;
            written.insert(name);
            let mut stored: StoredWorkspace = serde_json::from_slice(&snapshot)
                .map_err(|error| format!("{workspace}'s snapshot does not read: {error}"))?;
            let mut journal_kept = Kept {
                revision: *snapshot_revision,
                written: journal::first_of(&stored)?,
                snapshot: snapshot.len(),
                appended: 0,
            };
            let log = journal_name(*workspace, *snapshot_revision);
            let mut whole = true;
            // A snapshot of an earlier version comes up, and a journal beside it
            // of that version is refused: this build reads only the journal it
            // appends, and the next publication writes a snapshot rather than
            // append to one of another layout.
            if stored.version < WORKSPACE_VERSION {
                if parts.read_part(&log)?.is_some() {
                    return Err(format!(
                        "{workspace}'s journal is a version {} one, and this build reads a \
                         version {WORKSPACE_VERSION} one",
                        stored.version
                    ));
                }
                stored.version = WORKSPACE_VERSION;
                whole = false;
            } else if let Some(entries) = parts.read_part(&log)? {
                written.insert(log);
                whole = journal::read_whole(&entries);
                let (replayed, last) = journal::replay(stored, &entries)
                    .map_err(|error| format!("{workspace}'s journal does not read: {error}"))?;
                stored = replayed;
                revision = revision.max(last.unwrap_or(0));
                journal_kept.written = journal::first_of(&stored)?;
                journal_kept.appended = entries.len();
            }
            if stored.workspace != *workspace {
                return Err(format!("{workspace}'s record holds another workspace"));
            }
            workspaces.insert(*workspace, stored);
            // A journal a write cut short is let go with a snapshot first.
            if whole {
                kept.insert(*workspace, journal_kept);
            }
        }
        let record = NativeAuthorityStorage {
            revision,
            workspaces,
            pending: index.pending.clone(),
        };
        Ok(Some((
            Self {
                index,
                kept,
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
            return Ok(());
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
        self.index = Index {
            version: INDEX_VERSION,
            revision: next.revision,
            held,
            pending: next.pending.clone(),
        };
        self.write_index(parts)?;
        self.prune(parts);
        Ok(())
    }

    /// Append what `next` changed of `workspace` to its journal: `false`,
    /// with nothing written, when the journal is due a new snapshot.
    fn append(
        &mut self,
        parts: &dyn JournalParts,
        next: &NativeAuthorityStorage,
        workspace: WorkspaceId,
    ) -> Result<bool, String> {
        let Some(stored) = next.stored(workspace) else {
            return Ok(false);
        };
        let held_at = self.index.held.get(&workspace).copied();
        // An enrollment in flight is what the index names beside the
        // workspaces: a publication that changed one writes the index.
        if serde_json::to_vec(&next.pending).ok() != serde_json::to_vec(&self.index.pending).ok() {
            return Ok(false);
        }
        let Some(kept) = self.kept.get_mut(&workspace) else {
            return Ok(false);
        };
        if held_at != Some(kept.revision) {
            return Ok(false);
        }
        let (entry, written) = journal::next(&kept.written, stored, next.revision)?;
        let appended = kept.appended.saturating_add(entry.len());
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

    /// Write `workspace`'s snapshot at `revision` and an empty journal after it.
    fn write_snapshot(
        &mut self,
        parts: &dyn JournalParts,
        workspace: WorkspaceId,
        revision: u64,
        stored: &StoredWorkspace,
    ) -> Result<(), String> {
        let snapshot = serde_json::to_vec(stored).map_err(|error| error.to_string())?;
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
                written: journal::first_of(stored)?,
                snapshot: snapshot.len(),
                appended: 0,
            },
        );
        Ok(())
    }

    fn write_index(&mut self, parts: &dyn JournalParts) -> Result<(), String> {
        let bytes = serde_json::to_vec(&self.index).map_err(|error| error.to_string())?;
        parts.write_part(INDEX_NAME, &bytes)?;
        self.written.insert(INDEX_NAME.to_owned());
        Ok(())
    }

    /// Let go every part the record no longer names: one a workspace was
    /// published over, one a publication stopped before its index wrote, and
    /// the parts of a workspace this device released.
    fn prune(&mut self, parts: &dyn JournalParts) {
        let mut named = BTreeSet::from([INDEX_NAME.to_owned()]);
        for (workspace, revision) in &self.index.held {
            named.insert(snapshot_name(*workspace, *revision));
            named.insert(journal_name(*workspace, *revision));
        }
        let gone: Vec<String> = self
            .written
            .iter()
            .filter(|name| !named.contains(*name))
            .cloned()
            .collect();
        for name in gone {
            // A store need not be able to forget a name: an emptied part
            // names nothing.
            if let Err(error) = parts.write_part(&name, &[]) {
                tracing::warn!(%error, part = %name, "a record part no longer named was kept");
                continue;
            }
            self.written.remove(&name);
        }
    }
}

fn snapshot_name(workspace: WorkspaceId, revision: u64) -> String {
    format!("w{workspace}.{revision}")
}

fn journal_name(workspace: WorkspaceId, revision: u64) -> String {
    format!("l{workspace}.{revision}")
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
            frames: Vec::new(),
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
