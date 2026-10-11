//! The durable owner of this profile's own room. The runtime governs the
//! room itself with its certified device key, keeps it local to this
//! machine, and commits each app document write to one file in the profile
//! before the broker acknowledges it.
//!
//! The room starts with governance only: apps start empty. On an app's first
//! write to a record the room lacks, the governing device creates the record,
//! under the app's route, but only in a resource kind the app's installed
//! manifest declares; in dev mode, an app that is not installed is trusted
//! with the kind its own build names.
//!
//! Apps seal their writes; this device stewards the room's channels, so the
//! owner opens each sealed write only after the sealed envelope passed every
//! check, and admits the plaintext it verified.

#[path = "owner_documents.rs"]
mod documents;

use super::workspace::ProfileWorkspace;
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};
use tilo_action_broker::channel_encryption::ChannelKeyProvider;
use tilo_desktop_model::RoomId;
use tilo_peer_state::{
    SeedState, StatePeer,
    owner_commit::{
        CommitFuture, CommitOperation, OwnerCommitReceipt, OwnerCommitRequest, OwnerCommitService,
    },
};
use tilo_user_identity::{DeviceCert, RevocationList};

/// The owner's file in the profile.
pub(super) const ROOM_OWNER: &str = "room-owner.bin";
const STORED_VERSION: u32 = 1;

/// The resource kinds an installed app declares its records hold; `None`
/// for an app this profile has not installed.
pub(super) type DeclaredKinds = Box<dyn Fn(&str) -> Option<Vec<String>> + Send + Sync>;

/// In which kinds the owner creates a missing record for an app.
pub(super) struct RecordCreation {
    pub(super) declared_kinds: DeclaredKinds,
    /// Started with `--dev`: an app that is not installed is a build under
    /// development, and may create records in the kinds it names.
    pub(super) dev: bool,
}

impl RecordCreation {
    fn allows(&self, app: &str, kind: &str) -> bool {
        match (self.declared_kinds)(app) {
            Some(declared) => declared.iter().any(|declared| declared == kind),
            None => self.dev,
        }
    }
}

pub(super) struct RoomOwner {
    room: String,
    path: PathBuf,
    key: SigningKey,
    /// The certificate the room is governed by, pinned when it was created.
    governor: DeviceCert,
    creation: RecordCreation,
    workspace: Arc<ProfileWorkspace>,
    /// The channel keys this device holds as the room's steward.
    keys: ChannelKeyProvider,
    committed: Mutex<Committed>,
}

struct Committed {
    peer: StatePeer,
    durable: Vec<u8>,
    generation: u64,
}

#[derive(Serialize, Deserialize)]
struct StoredRoom {
    version: u32,
    governor: DeviceCert,
    generation: u64,
    peer: Vec<u8>,
}

impl RoomOwner {
    /// The owner of `room` kept in `profile`, created on first start as a room
    /// governed by `device` alone, with no records.
    ///
    /// # Errors
    ///
    /// Returns an error when the stored room cannot be read or belongs to
    /// another device key, or a new room cannot be created and saved.
    pub(super) fn open(
        profile: &Path,
        room: &str,
        (key, device): (SigningKey, DeviceCert),
        creation: RecordCreation,
        workspace: Arc<ProfileWorkspace>,
        keys: ChannelKeyProvider,
    ) -> io::Result<Self> {
        let path = profile.join(ROOM_OWNER);
        let now = SystemTime::now();
        let (governor, committed) = match std::fs::read(&path) {
            Ok(bytes) => restore(&bytes, room, &key, now)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let committed = create(room, &device, &key, now).map_err(io::Error::other)?;
                save(&path, &device, &committed)?;
                (device, committed)
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            room: room.to_owned(),
            path,
            key,
            governor,
            creation,
            workspace,
            keys,
            committed: Mutex::new(committed),
        })
    }

    /// `app`'s `record` as this owner committed it.
    #[cfg(test)]
    pub(super) fn document(&self, app: &str, record: &str) -> Option<Vec<u8>> {
        self.committed.lock().ok()?.peer.document(app, record)
    }

    /// Whether `app`'s `record` is encrypted: its writes arrived sealed.
    #[cfg(test)]
    pub(super) fn encrypted(&self, app: &str, record: &str) -> bool {
        let Ok(committed) = self.committed.lock() else {
            return false;
        };
        let now = SystemTime::now();
        tilo_peer_state::BoundedStatePeer::restore(
            &committed.durable,
            &self.room,
            &self.governor,
            std::slice::from_ref(&self.governor),
            &RevocationList::new(),
            now,
            &tilo_peer_state::PeerLimits::default(),
        )
        .is_ok_and(|peer| {
            matches!(
                peer.read()
                    .document_read(self.governor.device_public_key, app, record, now),
                Err(tilo_peer_state::PeerDocumentReadError::Encrypted)
            )
        })
    }

    fn commit_now(
        &self,
        request: OwnerCommitRequest,
        record_kind: Option<&str>,
    ) -> Result<OwnerCommitReceipt, String> {
        let binding = request.binding()?;
        if request.room != self.room {
            return Err("the runtime owns only its own room".into());
        }
        let mut committed = self
            .committed
            .lock()
            .map_err(|_| "runtime room owner lock poisoned")?;
        match request.operation {
            CommitOperation::Document {
                app,
                record,
                envelope,
            } => self.commit_document(&mut committed, &app, &record, *envelope, record_kind)?,
            CommitOperation::EffectReceipt {
                invocation,
                redacted,
            } => {
                let receipt: tilo_desktop_model::WorkspaceEffectReceipt =
                    serde_json::from_slice(&redacted).map_err(|e| e.to_string())?;
                if receipt.invocation.0 != invocation {
                    return Err("effect receipt invocation mismatch".into());
                }
                self.workspace.keep_effect_receipt(&self.room, receipt)?;
            }
            CommitOperation::DesktopCommand { .. }
            | CommitOperation::RetiredAgentRuntime
            | CommitOperation::DocumentImport { .. }
            | CommitOperation::WorkspaceTaskCommand { .. }
            | CommitOperation::WorkspaceMembershipCommand { .. } => {
                return Err("the runtime room owner commits documents and effect receipts".into());
            }
        }
        Ok(OwnerCommitReceipt {
            binding,
            generation: committed.generation,
            aggregate_hash: *blake3::hash(&committed.durable).as_bytes(),
        })
    }

    fn commit_document(
        &self,
        committed: &mut Committed,
        app: &str,
        record: &str,
        envelope: collab_types::SignedOp,
        record_kind: Option<&str>,
    ) -> Result<(), String> {
        let now = SystemTime::now();
        if envelope.author.as_bytes() != &self.governor.device_public_key {
            return Err("runtime document signer differs from this device".into());
        }
        if committed
            .peer
            .lookup_exact_local_document(app, record, &envelope, now)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Ok(());
        }
        let mut candidate = committed.peer.clone();
        if candidate.document(app, record).is_none() {
            self.create_record(&mut candidate, app, record, record_kind, now)?;
        }
        if envelope.encryption.is_some() {
            let channels = self
                .keys
                .get()
                .ok_or("the room's channel keys are not available")?;
            let room = RoomId::parse(&self.room).ok_or("invalid runtime room")?;
            candidate.admit_new_opened_local_document(
                app,
                record,
                envelope,
                &mut |sealed| channels.open_new_write(room, sealed),
                now,
            )?;
        } else {
            candidate.admit_new_local_document(app, record, envelope, now)?;
        }
        let next = Committed {
            durable: candidate.durable()?,
            peer: candidate,
            generation: committed
                .generation
                .checked_add(1)
                .ok_or("runtime room generation exhausted")?,
        };
        save(&self.path, &self.governor, &next).map_err(|e| e.to_string())?;
        *committed = next;
        Ok(())
    }

    /// Create `app`'s missing `record`, empty, in the kind the app named.
    fn create_record(
        &self,
        candidate: &mut StatePeer,
        app: &str,
        record: &str,
        record_kind: Option<&str>,
        now: SystemTime,
    ) -> Result<(), String> {
        let kind = record_kind.ok_or_else(|| format!("{app} named no kind for a new record"))?;
        if !self.creation.allows(app, kind) {
            return Err(format!("{app} does not declare the resource kind `{kind}`"));
        }
        candidate.publish_document(
            app,
            record,
            automerge::AutoCommit::new().save(),
            &self.key,
            now,
        )?;
        Ok(())
    }
}

impl OwnerCommitService for RoomOwner {
    fn requires_room(&self, room: &str) -> bool {
        room == self.room
    }

    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        self.commit_with_record_kind(request, None)
    }

    fn commit_with_record_kind(
        &self,
        request: OwnerCommitRequest,
        record_kind: Option<String>,
    ) -> CommitFuture {
        Box::pin(std::future::ready(
            self.commit_now(request, record_kind.as_deref()),
        ))
    }

    fn health(&self) -> Result<(), String> {
        Ok(())
    }
}

/// The owner of a profile adopted from the desktop session: that host
/// governs the room, so every commit here is refused.
pub(super) struct AdoptedRoom {
    pub(super) room: String,
}

impl OwnerCommitService for AdoptedRoom {
    fn requires_room(&self, room: &str) -> bool {
        room == self.room
    }

    fn commit(&self, _: OwnerCommitRequest) -> CommitFuture {
        Box::pin(std::future::ready(Err(ADOPTED.into())))
    }

    fn health(&self) -> Result<(), String> {
        Err(ADOPTED.into())
    }
}

const ADOPTED: &str = "the desktop session's workspace is read only in the runtime";

/// A room governed by `device` alone: its governance, and no records.
fn create(
    room: &str,
    device: &DeviceCert,
    key: &SigningKey,
    now: SystemTime,
) -> Result<Committed, String> {
    let roster = vec![device.clone()];
    let mut peer = StatePeer::new(
        room,
        device.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )?;
    peer.select_admission_v5().map_err(|e| e.to_string())?;
    let seed = SeedState {
        room_changes: tilo_peer_state::seed_room(room, device, &roster)?,
        documents: Vec::new(),
    };
    peer.install_seed(seed, key, now)?;
    Ok(Committed {
        durable: peer.durable()?,
        peer,
        generation: 1,
    })
}

fn restore(
    bytes: &[u8],
    room: &str,
    key: &SigningKey,
    now: SystemTime,
) -> io::Result<(DeviceCert, Committed)> {
    let stored: StoredRoom = postcard::from_bytes(bytes).map_err(invalid)?;
    if stored.version != STORED_VERSION || stored.generation == 0 {
        return Err(invalid("unsupported runtime room owner file"));
    }
    if stored.governor.device_public_key != key.verifying_key().to_bytes() {
        return Err(invalid(
            "the runtime's room is governed by another device key",
        ));
    }
    let peer = StatePeer::restore(
        &stored.peer,
        room,
        stored.governor.clone(),
        vec![stored.governor.clone()],
        RevocationList::new(),
        now,
    )
    .map_err(invalid)?;
    if !peer.admission_v5_selected() {
        return Err(invalid("the runtime's room requires durable admission v5"));
    }
    Ok((
        stored.governor,
        Committed {
            peer,
            durable: stored.peer,
            generation: stored.generation,
        },
    ))
}

fn save(path: &Path, governor: &DeviceCert, committed: &Committed) -> io::Result<()> {
    let bytes = postcard::to_allocvec(&StoredRoom {
        version: STORED_VERSION,
        governor: governor.clone(),
        generation: committed.generation,
        peer: committed.durable.clone(),
    })
    .map_err(io::Error::other)?;
    tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600)
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
#[path = "owner_tests.rs"]
mod tests;
