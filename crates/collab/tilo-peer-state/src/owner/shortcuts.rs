//! A shortcut saved or removed by a member's own device, as a signed workspace
//! write. The workspace document is where shortcuts live; every receiver admits
//! the write by the rule that proves who saved it.
use crate::owner::{NativeWorkspaceOwner, WorkspaceId};
use crate::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use collab_types::signer::DeviceSigner;
use web_time::SystemTime;

impl NativeWorkspaceOwner {
    /// Commit this device's own shortcut change, signed by `signer`, its custody, outside the
    /// owner's turn:
    /// the rule admission holds every writer to
    /// ([`crate::workspace::proven_saver`]), then the workspace change every
    /// receiver admits. A change the workspace already reflects commits nothing
    /// and answers the current generation.
    pub fn commit_shortcut_change(
        &self,
        request: &OwnerCommitRequest,
        signer: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<OwnerCommitReceipt, String> {
        let binding = request.binding()?;
        let workspace =
            WorkspaceId::parse(&request.workspace).ok_or("invalid native shortcut workspace")?;
        let CommitOperation::ShortcutChange { caller, change, .. } = &request.operation else {
            return Err("unsupported native shortcut owner commit family".into());
        };
        let change: tilo_automation::ShortcutChange =
            serde_json::from_slice(change).map_err(|error| error.to_string())?;
        crate::owner::write::signed_turn(signer, |key| {
            let _turn = self.0.mutation.lock();
            let candidate = self.view(workspace)?;
            if candidate.device() != caller || key.device_public_key() != caller.device_public_key {
                return Err("a shortcut change is signed by this device's own key".into());
            }
            let governs = candidate.hosted_here();
            let previous = candidate.peer.clone();
            let (receipt, record) = candidate.change(|live, record| {
                let before = live.workspace_heads().unwrap_or_default();
                let applied = live
                    .workspace_mut()
                    .apply_shortcut_change(caller, &change, key, now)?;
                key.settled()?;
                if !applied {
                    return Ok(None);
                }
                let next_generation = record
                    .generation
                    .checked_add(1)
                    .ok_or("native shortcut generation exhausted")?;
                let written = if governs {
                    live.publication_mut().durable_mutation_messages(
                        &previous,
                        key,
                        now,
                        next_generation,
                    )?;
                    key.settled()?;
                    Vec::new()
                } else {
                    live.workspace_changes_since(&before)
                };
                let receipt = OwnerCommitReceipt {
                    binding: binding.clone(),
                    generation: next_generation,
                    aggregate_hash: *blake3::hash(&live.durable()?).as_bytes(),
                };
                record.generation = next_generation;
                record.shortcut_change = Some(receipt.clone());
                if governs {
                    crate::owner::publication::replace_governance_checkpoint(
                        live,
                        record,
                        receipt.binding.clone(),
                        now,
                    )?;
                } else if !written.is_empty() {
                    crate::owner::publication::add_member_changes(
                        live,
                        record,
                        written,
                        receipt.binding.clone(),
                        now,
                    )?;
                }
                Ok(Some(receipt))
            })?;
            let Some(receipt) = receipt else {
                return Ok(OwnerCommitReceipt {
                    binding: binding.clone(),
                    generation: candidate.generation(),
                    aggregate_hash: *blake3::hash(&candidate.record.peer).as_bytes(),
                });
            };
            self.publish_record(workspace, &candidate, record, None)?;
            Ok(receipt)
        })
    }
}

#[cfg(test)]
#[path = "shortcuts_tests.rs"]
mod tests;
