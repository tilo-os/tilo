//! A device's own terminal audit: each member's receipts are their own
//! slice of the workspace, written by their devices and checked by every
//! receiver ([`crate::workspace::effect_slices_kept`]). A device keeps
//! its newest receipts' commit receipts hot and archives the rest to a cold
//! history it anchors in its slice.
use crate::StatePeer;
use crate::owner::record::StoredWorkspace;
use crate::owner::{NativeWorkspaceOwner, WorkspaceId};
use crate::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use web_time::SystemTime;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredEffectReceipt {
    pub(crate) receipt: OwnerCommitReceipt,
}

pub(crate) fn validate(view: &crate::owner::NativeCommittedView) -> Result<(), String> {
    if view.record.effect_receipts.len() > 128 {
        return Err("native effect receipt capacity exceeded on restore".into());
    }
    let document = view
        .peer()
        .admitted_workspace()
        .ok_or("effect workspace is absent")?;
    let effects = tilo_desktop_model::projection::workspace_effect_receipts_of(document);
    for (id, stored) in &view.record.effect_receipts {
        // A receipt this device wrote is the truth about an effect it served;
        // the workspace's signed half of it arrives when its change is merged,
        // which is not before this view is restored. Until then the receipt
        // binds the invocation without a public half to check it against: it
        // keeps the workspace, and it is what answers a retry of that
        // invocation.
        let Some(effect) = effects
            .iter()
            .find(|effect| effect.invocation.0.to_string() == *id)
        else {
            continue;
        };
        let request = OwnerCommitRequest {
            workspace: view.record.workspace.to_string(),
            operation: CommitOperation::EffectReceipt {
                invocation: effect.invocation.0,
                redacted: serde_json::to_vec(effect).map_err(|error| error.to_string())?,
            },
        };
        decode(&request)?;
        if request.binding()? != stored.receipt.binding
            || stored.receipt.generation == 0
            || stored.receipt.generation > view.generation()
        {
            return Err("stored effect receipt binding changed".into());
        }
    }
    Ok(())
}

fn validate_fact(
    record: &StoredWorkspace,
    fact: &crate::owner::effect_history::Fact,
    effects: &EffectIndex,
) -> Result<(), String> {
    let request = crate::owner::effect_history::request(&record.workspace.to_string(), fact);
    let effect = decode(&request)?;
    if request.binding()? != fact.receipt.binding
        || fact.receipt.generation == 0
        || fact.receipt.generation > record.generation
        || !effects
            .get(&effect.invocation.0)
            .is_some_and(|receipts| receipts.contains(&effect))
    {
        return Err("cold effect fact differs from signed public history".into());
    }
    Ok(())
}

type EffectIndex =
    std::collections::BTreeMap<uuid::Uuid, Vec<tilo_desktop_model::WorkspaceEffectReceipt>>;

fn index_effects(effects: Vec<tilo_desktop_model::WorkspaceEffectReceipt>) -> EffectIndex {
    let mut index = EffectIndex::new();
    for effect in effects {
        index.entry(effect.invocation.0).or_default().push(effect);
    }
    index
}

fn public_effect_index(peer: &StatePeer) -> Result<EffectIndex, String> {
    Ok(index_effects(
        tilo_desktop_model::projection::workspace_effect_receipts_of(
            peer.admitted_workspace()
                .ok_or("effect workspace is absent")?,
        ),
    ))
}

pub(crate) fn validate_history(
    view: &crate::owner::NativeCommittedView,
    store: &dyn crate::owner::Store,
) -> Result<(), String> {
    let signed_root = view
        .peer()
        .workspace()
        .workspace_effect_history_root(view.device())?;
    let Some(root) = view.record.effect_history_root else {
        if signed_root.is_some() {
            return Err("this device's effect history index is missing from its record".into());
        }
        return Ok(());
    };
    if signed_root != Some(root) {
        return Err("native effect index differs from signed governance root".into());
    }
    let effects = tilo_desktop_model::projection::workspace_effect_receipts_of(
        view.peer()
            .admitted_workspace()
            .ok_or("effect workspace is absent")?,
    );
    let count = effects.len();
    let effects = index_effects(effects);
    crate::owner::effect_history::validate(store, root, count, |fact| {
        validate_fact(&view.record, fact, &effects)
    })
}

fn archive_one(
    peer: &mut StatePeer,
    view: &mut StoredWorkspace,
    store: &dyn crate::owner::Store,
    key: &SigningKey,
    now: SystemTime,
) -> Result<(), String> {
    let (id, stored) = view
        .effect_receipts
        .iter()
        .min_by_key(|(_, value)| value.receipt.generation)
        .ok_or("effect hot cache is absent")?;
    let id = id.clone();
    let effects = public_effect_index(peer)?;
    let effect = effects
        .values()
        .flatten()
        .find(|effect| effect.invocation.0.to_string() == id)
        .ok_or("effect hot cache has no signed public receipt")?;
    let fact = crate::owner::effect_history::Fact {
        invocation: effect.invocation.0,
        redacted: serde_json::to_vec(effect).map_err(|e| e.to_string())?,
        receipt: stored.receipt.clone(),
    };
    validate_fact(view, &fact, &effects)?;
    let root = crate::owner::effect_history::insert(store, view.effect_history_root, fact)?;
    let device = view.device.clone();
    peer.workspace_mut()
        .set_workspace_effect_history_root(&device, root, key, now)?;
    view.effect_history_root = Some(root);
    // Older readers must refuse an archived owner record, not ignore the
    // index and mint a different original receipt for an already-known UUID.
    view.version = 3;
    view.effect_receipts.remove(&id);
    Ok(())
}

fn decode(
    request: &OwnerCommitRequest,
) -> Result<tilo_desktop_model::WorkspaceEffectReceipt, String> {
    request.binding()?;
    let CommitOperation::EffectReceipt {
        invocation,
        redacted,
    } = &request.operation
    else {
        return Err("unsupported native effect commit family".into());
    };
    crate::PeerWorkspace::validate_workspace_effect_receipt_encoding(*invocation, redacted)
}

#[cfg(test)]
#[path = "effect_receipt_tests.rs"]
mod tests;

impl NativeWorkspaceOwner {
    /// Commit the broker's exact redacted terminal receipt, of an invocation
    /// this device served, before acknowledging it: written into this
    /// device's person's slice of the workspace, signed by `key`, the device's
    /// own, and published as any workspace change of its own. Original
    /// arguments and results never enter the workspace.
    pub fn commit_effect_receipt(
        &self,
        request: &OwnerCommitRequest,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<OwnerCommitReceipt, String> {
        let effect = decode(request)?;
        let binding = request.binding()?;
        let workspace = WorkspaceId::parse(&request.workspace).ok_or("invalid effect workspace")?;
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let device = candidate.device().clone();
        if key.verifying_key().to_bytes() != device.device_public_key {
            return Err("an effect receipt is signed by this device's own key".into());
        }
        let id = effect.invocation.0.to_string();
        // A signed root without this device's exact retained history is not
        // an empty index. No authority is inferred from an unknown UUID.
        if candidate
            .peer()
            .workspace()
            .workspace_effect_history_root(&device)?
            != candidate.record.effect_history_root
        {
            return Err("native effect index is unavailable for the signed root".into());
        }
        if let Some(existing) = candidate.record.effect_receipts.get(&id) {
            if existing.receipt.binding != binding {
                return Err("effect invocation is already bound to another receipt".into());
            }
            return Ok(existing.receipt.clone());
        }
        let effects = public_effect_index(candidate.peer())?;
        if let Some(fact) = crate::owner::effect_history::lookup(
            self.0.store.as_ref(),
            candidate.record.effect_history_root,
            effect.invocation.0,
        )? {
            validate_fact(&candidate.record, &fact, &effects)?;
            if fact.receipt.binding != binding {
                return Err("cold effect invocation already has another receipt".into());
            }
            return Ok(fact.receipt);
        }
        let governs = candidate.hosted_here();
        let previous = candidate.peer.clone();
        let (receipt, record) = candidate.change(|live, record| {
            let before = live.workspace_heads().unwrap_or_default();
            // A publication that did not reach disk leaves the replica holding
            // this receipt's fact already; the retry writes the record it left
            // behind instead of refusing a receipt the workspace has.
            let is_new = live
                .workspace_mut()
                .apply_own_effect_receipt(&device, &effect, key, now)?;
            if record.effect_receipts.len() >= 128 {
                archive_one(live, record, self.0.store.as_ref(), key, now)?;
            }
            let generation = record
                .generation
                .checked_add(1)
                .ok_or("effect generation exhausted")?;
            let written = if !is_new {
                Vec::new()
            } else if governs {
                // The signed delta includes the root update as well as the new receipt.
                live.publication_mut()
                    .durable_mutation_messages(&previous, key, now, generation)?;
                Vec::new()
            } else {
                live.workspace_changes_since(&before)
            };
            record.generation = generation;
            let receipt = OwnerCommitReceipt {
                binding: binding.clone(),
                generation,
                aggregate_hash: *blake3::hash(&live.durable()?).as_bytes(),
            };
            record.effect_receipts.insert(
                id.clone(),
                StoredEffectReceipt {
                    receipt: receipt.clone(),
                },
            );
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
            Ok(receipt)
        })?;
        self.publish_record(workspace, &candidate, record, None)?;
        Ok(receipt)
    }
}
