//! A shortcut saved or removed by a member's own device, as a signed room
//! write. The room document is where shortcuts live; every receiver admits
//! the write by the rule that proves who saved it.
use crate::{NativeWorkspaceOwner, RoomId};
use ed25519_dalek::SigningKey;
use tilo_owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use web_time::SystemTime;

impl NativeWorkspaceOwner {
    /// Commit this device's own shortcut change, signed by `key`, its key:
    /// the rule admission holds every writer to
    /// ([`tilo_peer_state::room::proven_saver`]), then the room change every
    /// receiver admits. A change the room already reflects commits nothing
    /// and answers the current generation.
    ///
    /// # Errors
    /// Refuses another family, a caller or key that is not this device's, a
    /// caller the room does not admit to change it, a saver without proof,
    /// or failed publication.
    pub fn commit_shortcut_change(
        &self,
        request: &OwnerCommitRequest,
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
        if candidate.device() != caller
            || key.verifying_key().to_bytes() != caller.device_public_key
        {
            return Err("a shortcut change is signed by this device's own key".into());
        }
        let governs = candidate.device() == candidate.governor();
        let before = candidate.peer.room_heads().unwrap_or_default();
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
        let written = if governs {
            peer.publication_mut().durable_mutation_messages(
                &candidate.peer,
                key,
                now,
                next_generation,
            )?;
            Vec::new()
        } else {
            peer.room_changes_since(&before)
        };
        candidate.peer = peer;
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = next_generation;
        let receipt = OwnerCommitReceipt {
            binding,
            generation: next_generation,
            aggregate_hash: *blake3::hash(&candidate.record.peer).as_bytes(),
        };
        candidate.record.shortcut_change = Some(receipt.clone());
        if governs {
            crate::publication::replace_governance_checkpoint(
                &mut candidate,
                receipt.binding.clone(),
                now,
            )?;
        } else if !written.is_empty() {
            crate::publication::add_member_changes(
                &mut candidate,
                written,
                receipt.binding.clone(),
                now,
            )?;
        }
        self.publish_room(room, candidate, None)?;
        Ok(receipt)
    }
}

#[cfg(test)]
#[path = "shortcuts_tests.rs"]
mod tests;
