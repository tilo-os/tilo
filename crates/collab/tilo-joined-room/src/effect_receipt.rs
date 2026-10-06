//! Governor-owned terminal audit, committed with its signed room checkpoint.
use crate::{NativeWorkspaceOwner, RoomId};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::time::SystemTime;
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use tilo_user_identity::DeviceCert;

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
    let signed_root = view.peer().workspace_effect_history_root()?;
    let Some(root) = view.record.effect_history_root else {
        if signed_root.is_some() && view.device() == view.governor() {
            return Err("native governor history index is missing from its owner record".into());
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
    view.peer
        .set_workspace_effect_history_root(root, key, now)?;
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
    let receipt: tilo_desktop_model::WorkspaceEffectReceipt =
        serde_json::from_slice(redacted).map_err(|_| "malformed redacted effect receipt")?;
    if invocation.is_nil()
        || receipt.invocation.0 != *invocation
        || receipt.actor.trim().is_empty()
        || receipt.actor.chars().count() > 160
        || receipt.actor.chars().any(char::is_control)
        || receipt.completed_at < 0
        || !bounded_context(&receipt.context)
        || matches!(&receipt.status, tilo_actions::EffectReceiptStatus::Failed { code }
            if code.is_empty() || code.len() > 96 || !code.bytes().all(|byte|
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')))
        || receipt.context.facets.len() != 2
        || !receipt
            .context
            .facets
            .iter()
            .any(|facet| facet.key == "tilo.receipt" && facet.value == "effect")
        || !receipt
            .context
            .facets
            .iter()
            .any(|facet| facet.key == "tilo.invocation" && facet.value == invocation.to_string())
        || serde_json::to_vec(&receipt).map_err(|error| error.to_string())? != *redacted
    {
        return Err("effect receipt is not the exact bounded broker terminal encoding".into());
    }
    Ok(receipt)
}

fn bounded_context(context: &tilo_actions::ActionContext) -> bool {
    fn text(value: &str, max: usize) -> bool {
        value.trim() == value
            && value.chars().count() <= max
            && !value.chars().any(char::is_control)
    }
    text(&context.action_label, 200)
        && context
            .app
            .as_ref()
            .is_none_or(|app| text(&app.id, 160) && text(&app.label, 160))
        && context.resource.as_ref().is_none_or(|resource| {
            text(&resource.id, 512) && text(&resource.kind, 160) && text(&resource.label, 200)
        })
        && context
            .purpose
            .as_ref()
            .is_none_or(|purpose| !purpose.is_empty() && text(purpose, 512))
        && context.group.as_ref().is_none_or(|group| {
            text(&group.id, 160) && text(&group.label, 200) && text(&group.kind, 160)
        })
}

#[cfg(test)]
#[path = "effect_receipt_tests.rs"]
mod tests;

impl NativeWorkspaceOwner {
    /// Commit the broker's exact redacted terminal receipt before acknowledging it.
    /// Original arguments/results never enter the room; governor signing custody
    /// is supplied out of band, not granted by the receipt.
    ///
    /// # Errors
    /// Refuses invalid receipts, absent governor custody, changed retries, quota
    /// exhaustion or failed durable publication.
    pub fn commit_effect_receipt(
        &self,
        request: &OwnerCommitRequest,
        governor: &DeviceCert,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<OwnerCommitReceipt, String> {
        let effect = decode(request)?;
        let binding = request.binding()?;
        let room = RoomId::parse(&request.room).ok_or("invalid effect room")?;
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        if candidate.governor() != governor
            || candidate.device() != governor
            || key.verifying_key().to_bytes() != governor.device_public_key
        {
            return Err("effect receipt lacks exact current governor signing custody".into());
        }
        let id = effect.invocation.0.to_string();
        let mut checked = candidate.peer.clone();
        let is_new = checked.apply_workspace_effect_receipt(&effect, key, now)?;
        // A received signed root without its exact retained owner history is not
        // an empty index. No creator authority is inferred from an unknown UUID.
        if candidate.peer.workspace_effect_history_root()? != candidate.record.effect_history_root {
            return Err("native effect index is unavailable for the signed governance root".into());
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
            return Err("signed effect UUID lacks its immutable owner receipt history".into());
        }
        if candidate.record.effect_receipts.len() >= 128 {
            archive_one(&mut candidate, self.0.store.as_ref(), key, now)?;
        }
        let mut peer = candidate.peer.clone();
        peer.apply_workspace_effect_receipt(&effect, key, now)?;
        let generation = candidate
            .generation()
            .checked_add(1)
            .ok_or("effect generation exhausted")?;
        // The signed delta includes the root update as well as the new receipt.
        self.with_current(room, |previous| {
            peer.durable_mutation_messages(&previous.peer, key, now, generation)?;
            Ok(())
        })?;
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
        crate::publication::replace_governance_checkpoint(
            &mut candidate,
            receipt.binding.clone(),
            now,
        )?;
        self.publish_room(room, candidate, None)?;
        Ok(receipt)
    }
}
