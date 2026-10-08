//! Durable signed profile policy, never grants retained only in desktop caches.
use crate::{NativeWorkspaceOwner, RoomId};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::time::SystemTime;
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use tilo_user_identity::DeviceCert;

const MAX_COMMANDS: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredPermissionProfileReceipt {
    pub(crate) receipt: OwnerCommitReceipt,
}

#[cfg(test)]
#[path = "workspace_permission_profile_tests.rs"]
mod tests;

impl NativeWorkspaceOwner {
    /// Save or assign a signed room profile under current original Person authority.
    /// Exact UUID retries retain the original receipt; changed reuse never changes policy.
    /// # Errors
    /// Refuses retired caller/governor custody, restricted policy, changed replay or publication failure.
    pub fn commit_workspace_permission_profile_command(
        &self,
        request: OwnerCommitRequest,
        governor: &DeviceCert,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<OwnerCommitReceipt, String> {
        let binding = request.binding()?;
        let room = RoomId::parse(&request.room).ok_or("invalid permission profile room")?;
        let CommitOperation::WorkspacePermissionProfileCommand {
            invocation,
            caller,
            command,
        } = request.operation
        else {
            return Err("unsupported permission profile owner command".into());
        };
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        if candidate.governor() != governor
            || candidate.device() != governor
            || key.verifying_key().to_bytes() != governor.device_public_key
        {
            return Err("permission profile lacks current governor signing custody".into());
        }
        let id = serde_json::to_string(&(caller.device_public_key, invocation))
            .map_err(|_| "permission profile receipt identity encoding failed")?;
        let mut peer = candidate.peer.clone();
        // Revalidate authority even for a replay. This disposable candidate cannot publish another effect.
        peer.apply_workspace_permission_profile_command(&caller, &command, key, now)?;
        if let Some(existing) = candidate.record.workspace_permission_profiles.get(&id) {
            if existing.receipt.binding != binding {
                return Err("permission profile UUID already bound to another command".into());
            }
            return Ok(existing.receipt.clone());
        }
        if candidate.record.workspace_permission_profiles.len() >= MAX_COMMANDS {
            return Err("permission profile receipt capacity exhausted".into());
        }
        let next_generation = candidate
            .generation()
            .checked_add(1)
            .ok_or("permission profile generation exhausted")?;
        let messages =
            peer.durable_mutation_messages(&candidate.peer, key, now, next_generation)?;
        for message in &messages {
            if matches!(message, tilo_peer_state::NeighbourMsg::AppDocument { .. }) {
                let outgoing = crate::publication::prepare(&crate::publication::request(message)?)?;
                crate::publication::reserve(&candidate, &outgoing)?;
                crate::publication::append_local(&mut candidate, outgoing)?;
            }
        }
        candidate.peer = peer;
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = next_generation;
        let receipt = OwnerCommitReceipt {
            binding,
            generation: next_generation,
            aggregate_hash: *blake3::hash(&candidate.record.peer).as_bytes(),
        };
        candidate.record.workspace_permission_profiles.insert(
            id,
            StoredPermissionProfileReceipt {
                receipt: receipt.clone(),
            },
        );
        crate::publication::replace_governance_checkpoint(
            &mut candidate,
            receipt.binding.clone(),
            now,
        )?;
        self.publish_room(room, candidate, None)?;
        Ok(receipt)
    }
}
