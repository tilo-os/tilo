//! Rooms this device governs itself, having created them: the same admitted
//! state and durable record as a room joined through a host, without the
//! signed join and without a host's custody of its writes.
use crate::{NativeCommittedView, NativeWorkspaceOwner, record::StoredRoom};
use ed25519_dalek::SigningKey;
use std::time::SystemTime;
use tilo_desktop_model::RoomId;
use tilo_peer_state::{SeedState, StatePeer};
use tilo_user_identity::{DeviceCert, RevocationList};

impl NativeWorkspaceOwner {
    /// Create `room`, shown in local workspace `workspace`, governed by
    /// `device` (whose key `key` is) and starting from `seed`: its
    /// governance, and the records it starts with.
    ///
    /// # Errors
    /// The owner is fenced, the room is already held, the seed is refused or
    /// the record cannot be published.
    pub fn create_governed(
        &self,
        room: RoomId,
        workspace: u64,
        device: &DeviceCert,
        key: &SigningKey,
        seed: SeedState,
        now: SystemTime,
    ) -> Result<(), String> {
        if key.verifying_key().to_bytes() != device.device_public_key {
            return Err("the governing key is not the device's".into());
        }
        let _turn = self.0.mutation.lock();
        if self.contains_room(room) {
            return Err("the room is already held".into());
        }
        let mut peer = StatePeer::new(
            &room.to_string(),
            device.clone(),
            vec![device.clone()],
            RevocationList::new(),
            now,
        )?;
        peer.select_admission_v5()
            .map_err(|error| error.to_string())?;
        peer.install_seed(seed, key, now)?;
        let record = StoredRoom {
            version: 2,
            room,
            workspace,
            governor: device.clone(),
            device: device.clone(),
            enrollment: None,
            peer: peer.durable()?,
            private_direct_conversations: crate::PrivateDirectConversationStore::default(),
            generation: 1,
            continuity: 0,
            publication: Some(crate::publication::StoredPublication::default()),
            workspace_memberships: std::collections::BTreeMap::new(),
            effect_receipts: std::collections::BTreeMap::new(),
            effect_history_root: None,
            successions: Vec::new(),
        };
        self.publish_room(room, NativeCommittedView { peer, record }, None)
    }
}

#[cfg(test)]
mod tests;
