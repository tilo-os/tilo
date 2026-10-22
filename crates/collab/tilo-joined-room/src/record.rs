use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tilo_desktop_model::{RoomId, WorkspaceId};
use tilo_peer_state::join::TransportHint;
use tilo_user_identity::DeviceCert;

/// Native-only state-file envelope. Its fields cannot be authored by UI callers.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NativeAuthorityStorage {
    pub(crate) revision: u64,
    pub(crate) rooms: BTreeMap<RoomId, StoredRoom>,
    pub(crate) pending: BTreeMap<RoomId, StoredPendingEnrollment>,
}
impl NativeAuthorityStorage {
    #[must_use]
    pub fn requires(&self, room: RoomId) -> bool {
        self.rooms.contains_key(&room) || self.pending.contains_key(&room)
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rooms.is_empty() && self.pending.is_empty()
    }
    /// Rooms that are enrolled or have an enrollment in flight.
    pub fn rooms(&self) -> impl Iterator<Item = RoomId> + '_ {
        self.rooms.keys().chain(self.pending.keys()).copied()
    }
    /// Whether this device governs `room` itself, having created it.
    #[must_use]
    pub fn governed_here(&self, room: RoomId) -> bool {
        self.rooms
            .get(&room)
            .is_some_and(|stored| stored.enrollment.is_none())
    }
    /// Whether this device holds `room`: its replica is installed, not
    /// merely an enrollment into it in flight.
    #[must_use]
    pub fn holds(&self, room: RoomId) -> bool {
        self.rooms.contains_key(&room)
    }
    /// Every room whose replica is installed here, with the workspace id the
    /// room's record names (the room's transport shard).
    pub fn held(&self) -> impl Iterator<Item = (RoomId, WorkspaceId)> + '_ {
        self.rooms
            .iter()
            .map(|(room, stored)| (*room, WorkspaceId(stored.workspace)))
    }
    /// Every room an enrollment is in flight into, with the local workspace
    /// it installs into, where it names one.
    pub fn joins(&self) -> impl Iterator<Item = (RoomId, Option<WorkspaceId>)> + '_ {
        self.pending
            .iter()
            .map(|(room, pending)| (*room, pending.target_workspace()))
    }
    /// Whether every pending enrollment names the local slot it binds.
    #[must_use]
    pub fn every_pending_has_slot(&self) -> bool {
        self.pending
            .values()
            .all(|pending| pending.target.is_some())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredRoom {
    pub version: u32,
    pub room: RoomId,
    pub workspace: u64,
    pub governor: DeviceCert,
    pub device: DeviceCert,
    /// How this device joined the room a host governs; `None` for a room
    /// this device governs itself, which it joined by creating it.
    #[serde(flatten)]
    pub enrollment: Option<StoredEnrollment>,
    /// `StatePeer::durable()`: the largest field by far.
    #[serde(with = "base64_bytes")]
    pub peer: Vec<u8>,
    /// Two-person private text; never included in public room or task state.
    #[serde(default, rename = "privateDirectConversations")]
    pub private_direct_conversations: crate::PrivateDirectConversationStore,
    pub generation: u64,
    pub continuity: u64,
    /// Outgoing publication state; required on every committed room record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<crate::publication::StoredPublication>,
    /// Original person membership command outcomes, private to the owning host.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workspace_memberships:
        BTreeMap<String, crate::workspace_membership::StoredMembershipReceipt>,
    /// Exact broker terminal bindings, committed with the signed room checkpoint.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub effect_receipts: BTreeMap<String, crate::effect_receipt::StoredEffectReceipt>,
    /// Content-addressed cold UUID index, also anchored in signed public room history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_history_root: Option<[u8; 32]>,
    /// Root-signed statements naming who governs in place of a revoked
    /// governor: [`Self::governor`] is the governor the invitation named,
    /// these say who governs the room now.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub successions: Vec<tilo_user_identity::GovernorSuccession>,
    /// What this device, governing the room, keeps between the steps of
    /// its joins: the same ledger a relay keeps for the rooms it governs.
    /// Empty for a room joined through a host.
    #[serde(
        default,
        rename = "joinLedger",
        skip_serializing_if = "tilo_peer_state::join_ledger::JoinLedger::is_empty"
    )]
    pub join_ledger: tilo_peer_state::join_ledger::JoinLedger,
}

impl StoredRoom {
    /// Who governs the room now, and the revocations of the governors it
    /// passed on the way: the invitation's governor unless a succession
    /// names another ([`tilo_peer_state::succeeded_authority`]).
    pub(crate) fn current_governor(
        &self,
        now: std::time::SystemTime,
    ) -> (DeviceCert, tilo_user_identity::RevocationList) {
        let mut revoked = tilo_user_identity::RevocationList::new();
        let Some((governor, revocations)) =
            tilo_peer_state::succeeded_authority(&self.governor, &self.successions, now)
        else {
            return (self.governor.clone(), revoked);
        };
        for revocation in revocations {
            // Each verified with its succession.
            let _ = revoked.insert(revocation);
        }
        (governor, revoked)
    }
}
/// The signed join a device enrolled in a host-governed room with, and the
/// host it pinned then.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredEnrollment {
    #[serde(with = "base64_bytes")]
    pub invitation: Vec<u8>,
    #[serde(with = "base64_bytes")]
    pub enrollment_intent: Vec<u8>,
    pub enrolled_at_ns: u64,
    pub transport_hint: TransportHint,
    pub host: Option<DeviceCert>,
    pub locator: Option<TransportHint>,
    pub hint: Option<TransportHint>,
    pub floor: Option<DescriptorFloor>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct DescriptorFloor {
    pub generation: u64,
    pub digest: [u8; 32],
}

/// Only a successful state-file publication creates an enrollment handle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEnrollmentId {
    room: RoomId,
    generation: u64,
    digest: [u8; 32],
}
impl PendingEnrollmentId {
    pub(crate) fn new(room: RoomId, generation: u64, digest: [u8; 32]) -> Self {
        Self {
            room,
            generation,
            digest,
        }
    }
    #[must_use]
    pub fn room(&self) -> RoomId {
        self.room
    }
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingEnrollmentStatus {
    Active,
    Cancelled,
}
/// The local workspace an enrollment installs into: the workspace that is
/// the enrolled room, made for the join before the enrollment began. A
/// workspace never changes its room, so installing binds nothing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnrollmentTarget {
    pub workspace: WorkspaceId,
    /// The room the workspace is: the enrollment's own. Stored under the
    /// name the binding of a placeholder slot used, so a file read by a
    /// build from before keeps its layout; nothing reads it back.
    #[serde(rename = "prior_room")]
    pub room: RoomId,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredPendingEnrollment {
    /// `None` where the host has no local slots to bind.
    pub(crate) target: Option<EnrollmentTarget>,
    pub(crate) id: PendingEnrollmentId,
    pub(crate) intent: Vec<u8>,
    pub(crate) status: PendingEnrollmentStatus,
}
impl StoredPendingEnrollment {
    #[must_use]
    pub fn id(&self) -> &PendingEnrollmentId {
        &self.id
    }
    #[must_use]
    pub fn intent_bytes(&self) -> &[u8] {
        &self.intent
    }
    #[must_use]
    pub fn status(&self) -> PendingEnrollmentStatus {
        self.status
    }
    /// The local workspace the enrollment installs into, where the host has
    /// workspaces.
    #[must_use]
    pub fn target_workspace(&self) -> Option<WorkspaceId> {
        self.target.as_ref().map(|target| target.workspace)
    }
}

/// Opaque byte fields of a room record: base64 text in a human-readable
/// format (the owner file, the desktop state file), the unchanged `Vec<u8>`
/// encoding in a binary one.
pub(crate) mod base64_bytes {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

    pub(crate) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.serialize_str(&STANDARD.encode(bytes))
        } else {
            bytes.serialize(serializer)
        }
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        if deserializer.is_human_readable() {
            STANDARD
                .decode(String::deserialize(deserializer)?)
                .map_err(de::Error::custom)
        } else {
            Vec::deserialize(deserializer)
        }
    }
}
