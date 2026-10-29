//! A device's own terminal audit: each member's receipts are their own
//! slice of the room, written by their devices and checked by every
//! receiver ([`tilo_peer_state::room::effect_slices_kept`]). A device keeps
//! its newest receipts' commit receipts hot and archives the rest to a cold
//! history it anchors in its slice.
use crate::{NativeWorkspaceOwner, RoomId};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use web_time::SystemTime;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredEffectReceipt {
    pub(crate) receipt: OwnerCommitReceipt,
}

pub(crate) fn validate(view: &crate::NativeCommittedView) -> Result<(), String> {
    if view.record.effect_receipts.len() > 128 {
        return Err("native effect receipt capacity exceeded on restore".into());
    }
    let document = view.peer().admitted_room().ok_or("effect room is absent")?;
    let effects = tilo_desktop_model::projection::workspace_effect_receipts_of(document);
    for (id, stored) in &view.record.effect_receipts {
        let effect = effects
            .iter()
            .find(|effect| effect.invocation.0.to_string() == *id)
            .ok_or("stored effect binding has no admitted public receipt")?;
        let request = OwnerCommitRequest {
            room: view.record.room.to_string(),
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
    view: &crate::NativeCommittedView,
    fact: &crate::effect_history::Fact,
    effects: &EffectIndex,
) -> Result<(), String> {
    let request = crate::effect_history::request(&view.record.room.to_string(), fact);
    let effect = decode(&request)?;
    if request.binding()? != fact.receipt.binding
        || fact.receipt.generation == 0
        || fact.receipt.generation > view.generation()
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

fn public_effect_index(view: &crate::NativeCommittedView) -> Result<EffectIndex, String> {
    Ok(index_effects(
        tilo_desktop_model::projection::workspace_effect_receipts_of(
            view.peer().admitted_room().ok_or("effect room is absent")?,
        ),
    ))
}

pub(crate) fn validate_history(
    view: &crate::NativeCommittedView,
    store: &dyn crate::Store,
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
        view.peer().admitted_room().ok_or("effect room is absent")?,
    );
    let count = effects.len();
    let effects = index_effects(effects);
    crate::effect_history::validate(store, root, count, |fact| {
        validate_fact(view, fact, &effects)
    })
}

fn archive_one(
    view: &mut crate::NativeCommittedView,
    store: &dyn crate::Store,
    key: &SigningKey,
    now: SystemTime,
) -> Result<(), String> {
    let (id, stored) = view
        .record
        .effect_receipts
        .iter()
        .min_by_key(|(_, value)| value.receipt.generation)
        .ok_or("effect hot cache is absent")?;
    let id = id.clone();
    let effects = public_effect_index(view)?;
    let effect = effects
        .values()
        .flatten()
        .find(|effect| effect.invocation.0.to_string() == id)
        .ok_or("effect hot cache has no signed public receipt")?;
    let fact = crate::effect_history::Fact {
        invocation: effect.invocation.0,
        redacted: serde_json::to_vec(effect).map_err(|e| e.to_string())?,
        receipt: stored.receipt.clone(),
    };
    validate_fact(view, &fact, &effects)?;
    let root = crate::effect_history::insert(store, view.record.effect_history_root, fact)?;
    let device = view.device().clone();
    view.peer
        .workspace_mut()
        .set_workspace_effect_history_root(&device, root, key, now)?;
    view.record.effect_history_root = Some(root);
    // Older readers must refuse an archived owner record, not ignore the
    // index and mint a different original receipt for an already-known UUID.
    view.record.version = 3;
    view.record.effect_receipts.remove(&id);
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
    tilo_peer_state::PeerWorkspace::validate_workspace_effect_receipt_encoding(
        *invocation,
        redacted,
    )
}

#[cfg(test)]
#[path = "effect_receipt_tests.rs"]
mod tests;

impl NativeWorkspaceOwner {
    /// Commit the broker's exact redacted terminal receipt, of an invocation
    /// this device served, before acknowledging it: written into this
    /// device's person's slice of the room, signed by `key`, the device's
    /// own, and published as any room change of its own. Original
    /// arguments and results never enter the room.
    ///
    /// # Errors
    /// Refuses invalid receipts, a key that is not this device's, another's
    /// receipt, changed retries or failed durable publication.
    pub fn commit_effect_receipt(
        &self,
        request: &OwnerCommitRequest,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<OwnerCommitReceipt, String> {
        let effect = decode(request)?;
        let binding = request.binding()?;
        let room = RoomId::parse(&request.room).ok_or("invalid effect room")?;
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        let device = candidate.device().clone();
        if key.verifying_key().to_bytes() != device.device_public_key {
            return Err("an effect receipt is signed by this device's own key".into());
        }
        let id = effect.invocation.0.to_string();
        let mut checked = candidate.peer.clone();
        let is_new = checked
            .workspace_mut()
            .apply_own_effect_receipt(&device, &effect, key, now)?;
        // A signed root without this device's exact retained history is not
        // an empty index. No authority is inferred from an unknown UUID.
        if candidate
            .peer
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
        if let Some(fact) = crate::effect_history::lookup(
            self.0.store.as_ref(),
            candidate.record.effect_history_root,
            effect.invocation.0,
        )? {
            validate_fact(&candidate, &fact, &public_effect_index(&candidate)?)?;
            if fact.receipt.binding != binding {
                return Err("cold effect invocation already has another receipt".into());
            }
            return Ok(fact.receipt);
        }
        if !is_new {
            return Err("signed effect UUID lacks its immutable receipt history".into());
        }
        let governs = candidate.device() == candidate.governor();
        let before = candidate.peer.room_heads().unwrap_or_default();
        if candidate.record.effect_receipts.len() >= 128 {
            archive_one(&mut candidate, self.0.store.as_ref(), key, now)?;
        }
        let mut peer = candidate.peer.clone();
        peer.workspace_mut()
            .apply_own_effect_receipt(&device, &effect, key, now)?;
        let generation = candidate
            .generation()
            .checked_add(1)
            .ok_or("effect generation exhausted")?;
        let written = if governs {
            // The signed delta includes the root update as well as the new receipt.
            self.with_current(room, |previous| {
                peer.publication_mut().durable_mutation_messages(
                    &previous.peer,
                    key,
                    now,
                    generation,
                )?;
                Ok(())
            })?;
            Vec::new()
        } else {
            peer.room_changes_since(&before)
        };
        candidate.peer = peer;
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = generation;
        let receipt = OwnerCommitReceipt {
            binding,
            generation,
            aggregate_hash: *blake3::hash(&candidate.record.peer).as_bytes(),
        };
        candidate.record.effect_receipts.insert(
            id,
            StoredEffectReceipt {
                receipt: receipt.clone(),
            },
        );
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
