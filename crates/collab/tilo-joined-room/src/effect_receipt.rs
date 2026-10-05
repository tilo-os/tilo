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
    if receipt.invocation.0 != *invocation
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
        let mut peer = candidate.peer.clone();
        peer.apply_workspace_effect_receipt(&effect, key, now)?;
        if let Some(existing) = candidate.record.effect_receipts.get(&id) {
            if existing.receipt.binding != binding {
                return Err("effect invocation is already bound to another receipt".into());
            }
            return Ok(existing.receipt.clone());
        }
        if candidate.record.effect_receipts.len() >= 128 {
            return Err("native effect receipt capacity exhausted".into());
        }
        let generation = candidate
            .generation()
            .checked_add(1)
            .ok_or("effect generation exhausted")?;
        peer.durable_mutation_messages(&candidate.peer, key, now, generation)?;
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
