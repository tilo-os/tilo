//! Founding a workspace: a room this device hosts itself, with no host of
//! anyone else's to enroll with. The room starts from the canonical seed
//! (its identity, its governance and its name) and is published like any
//! other change this device makes, so whoever holds a copy of it — a store
//! device that keeps only its ciphertext, another member's device — has it
//! from the moment it exists.
use crate::owner::record::StoredRoom;
use crate::owner::{NativeCommittedView, NativeWorkspaceOwner, RoomId, publication};
use crate::workspace_membership::valid_room_name;
use crate::{StatePeer, workspace_seed};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_user_identity::{DeviceCert, RevocationList};
use web_time::SystemTime;

impl NativeWorkspaceOwner {
    /// Found a workspace named `name` on `device`, the device whose signing
    /// key is `key`, and hold it here: the room is new, its host is this
    /// device, and its governance admits any other device the owner
    /// invites. Nothing of it is its founder's private state: it is the
    /// room's, published like every other change this device makes.
    ///
    /// # Errors
    /// The name is not 1–80 characters, `key` is not `device`'s, `device`
    /// may not own workspaces, or the record cannot be published.
    pub fn create_workspace(
        &self,
        name: &str,
        device: &DeviceCert,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<RoomId, String> {
        let name = valid_room_name(name)?;
        if device.device_public_key != key.verifying_key().to_bytes() {
            return Err("a workspace is founded with its own device's key".into());
        }
        if !device.capabilities.can_own_workspaces {
            return Err("this device may not own workspaces".into());
        }
        let _turn = self.0.mutation.lock();
        let room = uuid::Uuid::new_v4().hyphenated().to_string();
        let id = RoomId::parse(&room).ok_or("a founded workspace has a canonical room")?;
        if self.0.authority.read().rooms.contains_key(&id) {
            return Err("this device holds that room already".into());
        }
        let workspace = crate::join::room_workspace_id(&room).map_err(|error| error.to_string())?;
        let roster = vec![device.clone()];
        let mut peer = StatePeer::new(
            &room,
            device.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )?;
        peer.select_admission_v5()
            .map_err(|error| format!("the workspace exceeds exact admission bounds: {error}"))?;
        let mut seed = workspace_seed(&room, device, &roster, name)?;
        // A device that takes part in a room's channels opens its records
        // as sealed channels: a record seeded in the clear would be refused
        // by every member that joins after the channel opened, and a store
        // device keeps only what is sealed. The workspace starts with its
        // governance; its records arrive sealed, on the channels its
        // members open.
        seed.documents.clear();
        peer.install_seed(seed, key, now)?;
        peer.governance_mut()
            .reconcile_administrator_grants(key, now)?;
        peer.media_mut().reconcile_media_governance(key, now)?;
        peer.media().validate_media_governance_for_publication()?;
        // What the room holds of this device's signing, sent before it is
        // referred to: a member that joins from this device's copy later
        // has it from the room itself.
        let messages = peer
            .publication_mut()
            .durable_bootstrap_messages(key, now, 1)?;
        let mut view = NativeCommittedView {
            record: StoredRoom {
                version: 2,
                room: id,
                workspace,
                room_host: device.clone(),
                device: device.clone(),
                // No other device admitted this one into its own room, so
                // there is neither an enrollment nor a vouch to hold.
                enrollment: None,
                vouched: None,
                peer: peer.durable()?,
                private_direct_conversations: crate::owner::PrivateDirectConversationStore::default(
                ),
                generation: 1,
                continuity: 0,
                publication: Some(publication::StoredPublication::default()),
                workspace_memberships: std::collections::BTreeMap::new(),
                effect_receipts: std::collections::BTreeMap::new(),
                shortcut_change: None,
                effect_history_root: None,
                join_ledger: crate::join_ledger::JoinLedger::default(),
                join_admissions: std::collections::BTreeMap::new(),
            },
            peer,
        };
        for message in messages {
            // The room's own part of the seed is its governance, which
            // every joiner is answered with; what else was published — the
            // sealed records and their keys — waits here like any other
            // outgoing publication.
            if matches!(message, NeighbourMsg::AppDocument { .. }) {
                let bytes = publication::prepare(&publication::request(&message)?)?;
                publication::append_local(&mut view, bytes)?;
            }
        }
        self.publish_room(id, view, None)?;
        Ok(id)
    }
}
