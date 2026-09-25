use super::super::state::{RoomId, WorkspaceId};
use qos_peer_state::join::TransportHint;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tilo_user_identity::DeviceCert;

/// Native-only state-file envelope. Its fields cannot be authored by UI callers.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NativeAuthorityStorage {
    pub(super) revision: u64,
    pub(super) rooms: BTreeMap<RoomId, StoredRoom>,
    pub(super) pending: BTreeMap<RoomId, StoredPendingEnrollment>,
}
impl NativeAuthorityStorage {
    pub(crate) fn requires(&self, room: RoomId) -> bool {
        self.rooms.contains_key(&room) || self.pending.contains_key(&room)
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.rooms.is_empty() && self.pending.is_empty()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredRoom {
    pub version: u32,
    pub room: RoomId,
    pub workspace: u64,
    pub governor: DeviceCert,
    pub device: DeviceCert,
    pub invitation: Vec<u8>,
    pub enrollment_intent: Vec<u8>,
    pub enrolled_at_ns: u64,
    pub transport_hint: TransportHint,
    pub host: Option<DeviceCert>,
    pub locator: Option<TransportHint>,
    pub hint: Option<TransportHint>,
    pub peer: Vec<u8>,
    pub generation: u64,
    pub continuity: u64,
    pub floor: Option<DescriptorFloor>,
    /// Absent only on the explicitly migrated native format-2 room record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<super::publication::StoredPublication>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct DescriptorFloor {
    pub generation: u64,
    pub digest: [u8; 32],
}

/// Only a successful state-file publication creates an enrollment handle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PendingEnrollmentId {
    room: RoomId,
    generation: u64,
    digest: [u8; 32],
}
impl PendingEnrollmentId {
    pub(super) fn new(room: RoomId, generation: u64, digest: [u8; 32]) -> Self {
        Self {
            room,
            generation,
            digest,
        }
    }
    pub(crate) fn room(&self) -> RoomId {
        self.room
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn digest(&self) -> [u8; 32] {
        self.digest
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum PendingEnrollmentStatus {
    Active,
    Cancelled,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct EnrollmentTarget {
    pub workspace: WorkspaceId,
    pub prior_room: RoomId,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredPendingEnrollment {
    pub(super) target: EnrollmentTarget,
    pub(super) id: PendingEnrollmentId,
    pub(super) intent: Vec<u8>,
    pub(super) status: PendingEnrollmentStatus,
}
impl StoredPendingEnrollment {
    pub(crate) fn id(&self) -> &PendingEnrollmentId {
        &self.id
    }
    pub(crate) fn intent_bytes(&self) -> &[u8] {
        &self.intent
    }
    pub(crate) fn status(&self) -> PendingEnrollmentStatus {
        self.status
    }
}
