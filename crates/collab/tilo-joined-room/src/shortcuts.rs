//! A shortcut saved or removed at the room's governor, as a signed room
//! write. The room document is where shortcuts live; a device's tree only
//! mirrors what the governor committed.
use crate::{NativeWorkspaceOwner, RoomId};
use ed25519_dalek::SigningKey;
use std::time::SystemTime;
use tilo_owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use tilo_user_identity::DeviceCert;

impl NativeWorkspaceOwner {
    /// Commit the authenticated caller's shortcut change with the governing
    /// device's out-of-band signing custody: the same rule admission holds
    /// every writer to ([`tilo_peer_state::room::proven_saver`]), then the
    /// governance publication and checkpoint every receiver admits. A change
    /// the room already reflects commits nothing and answers the current
    /// generation.
    ///
    /// # Errors
    /// Refuses another family, changed custody, a caller the room does not
    /// admit to change it, a saver without proof, or failed publication.
    pub fn commit_shortcut_change(
        &self,
        request: &OwnerCommitRequest,
        governor: &DeviceCert,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<OwnerCommitReceipt, String> {
        let binding = request.binding()?;
        let room = RoomId::parse(&request.room).ok_or("invalid native shortcut room")?;
        let CommitOperation::ShortcutChange { caller, change, .. } = &request.operation else {
            return Err("unsupported native shortcut owner commit family".into());
        };
        let change: tilo_automation::ShortcutChange =
            serde_json::from_slice(change).map_err(|error| error.to_string())?;
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        if candidate.governor() != governor
            || candidate.device() != governor
            || key.verifying_key().to_bytes() != governor.device_public_key
        {
            return Err("shortcut change lacks exact current governor signing custody".into());
        }
        let mut peer = candidate.peer.clone();
        if !peer
            .workspace_mut()
            .apply_shortcut_change(caller, &change, key, now)?
        {
            return Ok(OwnerCommitReceipt {
                binding,
                generation: candidate.generation(),
                aggregate_hash: *blake3::hash(&candidate.record.peer).as_bytes(),
            });
        }
        let next_generation = candidate
            .generation()
            .checked_add(1)
            .ok_or("native shortcut generation exhausted")?;
        peer.publication_mut().durable_mutation_messages(
            &candidate.peer,
            key,
            now,
            next_generation,
        )?;
        candidate.peer = peer;
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = next_generation;
        let receipt = OwnerCommitReceipt {
            binding,
            generation: next_generation,
            aggregate_hash: *blake3::hash(&candidate.record.peer).as_bytes(),
        };
        candidate.record.shortcut_change = Some(receipt.clone());
        crate::publication::replace_governance_checkpoint(
            &mut candidate,
            receipt.binding.clone(),
            now,
        )?;
        self.publish_room(room, candidate, None)?;
        Ok(receipt)
    }
}

#[cfg(test)]
#[path = "shortcuts_tests.rs"]
mod tests;
