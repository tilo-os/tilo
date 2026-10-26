//! Inert terminal preparation in the existing private invocation journal.
use super::{
    Durable, DurableInvocation, Injected, InvocationJournal, InvocationKey, InvocationOwner,
    InvocationUpdate,
};
use serde::{Deserialize, Serialize};
use tilo_owner_commit::{CommitOperation, OwnerCommitRequest};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparedEffect {
    version: u8,
    #[serde(deserialize_with = "terminal_update")]
    pub(crate) update: InvocationUpdate,
    room: String,
    invocation: uuid::Uuid,
    redacted: Vec<u8>,
}

fn terminal_update<'de, D: serde::Deserializer<'de>>(d: D) -> Result<InvocationUpdate, D::Error> {
    use serde::de::Error;
    let raw = super::guarded::RawUpdate::deserialize(d)?;
    raw.check_duplicates(true).map_err(D::Error::custom)?;
    let update: InvocationUpdate =
        serde_json::from_value(raw.value.clone()).map_err(D::Error::custom)?;
    if !update.is_terminal()
        || serde_json::to_value(&update).map_err(D::Error::custom)? != raw.value
    {
        return Err(D::Error::custom("noncanonical prepared terminal update"));
    }
    Ok(update)
}

impl PreparedEffect {
    pub(super) fn new(
        update: InvocationUpdate,
        request: &OwnerCommitRequest,
    ) -> Result<Self, String> {
        let CommitOperation::EffectReceipt {
            invocation,
            redacted,
        } = &request.operation
        else {
            return Err("prepared effect requires an effect receipt".into());
        };
        Ok(Self {
            version: 1,
            update,
            room: request.room.clone(),
            invocation: *invocation,
            redacted: redacted.clone(),
        })
    }

    pub(crate) fn request(&self) -> OwnerCommitRequest {
        OwnerCommitRequest {
            room: self.room.clone(),
            operation: CommitOperation::EffectReceipt {
                invocation: self.invocation,
                redacted: self.redacted.clone(),
            },
        }
    }

    pub(super) fn validate(&self, record: &DurableInvocation) -> Result<(), String> {
        if self.version != 1
            || self.room != record.key.room
            || self.invocation != record.key.invocation.0
            || self.invocation.is_nil()
            || self.update.invocation() != record.key.invocation
            || !self.update.is_terminal()
            || self.redacted.len() > 16 * 1024
            || serde_json::to_vec(&self.update)
                .map_err(|_| "prepared update encoding")?
                .len()
                > 256 * 1024
            || record.guarded.is_some()
            || record.intent.get("operation_kind")
                != Some(
                    &serde_json::to_value(tilo_actions::OperationKind::Effect)
                        .map_err(|_| "prepared operation encoding")?,
                )
        {
            return Err("prepared effect identity, kind or bounds invalid".into());
        }
        self.request().binding()?;
        let receipt: tilo_desktop_model::WorkspaceEffectReceipt =
            serde_json::from_slice(&self.redacted).map_err(|_| "prepared receipt invalid")?;
        let owner_actor = match &record.key.owner {
            InvocationOwner::Local { actor } => Some(actor.clone()),
            InvocationOwner::Certified { root, .. }
            | InvocationOwner::AuthenticatedLocal {
                root, agent: None, ..
            } => Some(super::super::principal_id_for_root(*root)),
            InvocationOwner::NativeTask { agent, .. }
            | InvocationOwner::AuthenticatedLocal {
                agent: Some(agent), ..
            } => Some(format!("agent:{agent}")),
            // Its exact current task/source admission is checked by the observing caller.
            InvocationOwner::Enrolled { .. } => None,
        };
        let context: tilo_actions::ActionContext =
            serde_json::from_value(record.intent.get("context").cloned().unwrap_or_default())
                .map_err(|_| "prepared context invalid")?;
        let status = tilo_actions::EffectReceiptStatus::from_terminal(&self.update)
            .ok_or("prepared effect is not terminal")?;
        let expected = tilo_desktop_model::WorkspaceEffectReceipt::redacted(
            record.key.invocation,
            &receipt.actor,
            context,
            status,
            tilo_clock::unix_now_i64(),
        );
        // completed_at belongs to the original preparation, never recovery time.
        if receipt.invocation != expected.invocation
            || owner_actor
                .as_deref()
                .is_some_and(|actor| actor != receipt.actor)
            || record
                .intent
                .get("actor")
                .and_then(serde_json::Value::as_str)
                != Some(receipt.actor.as_str())
            || receipt.context != expected.context
            || receipt.status != expected.status
            || receipt.actor.is_empty()
            || receipt.actor.chars().count() > 160
            || receipt.actor.chars().any(char::is_control)
            || receipt.completed_at < 0
            || serde_json::to_vec(&receipt).map_err(|_| "prepared receipt encoding")?
                != self.redacted
            || (record.update.is_terminal() && record.update != self.update)
        {
            return Err("prepared effect payload differs from admitted terminal".into());
        }
        Ok(())
    }
}

impl InvocationJournal {
    /// [`Self::stage_prepared_effect`], waiting for the write here.
    #[cfg(test)]
    pub(crate) fn prepare_effect(
        &mut self,
        key: &InvocationKey,
        update: InvocationUpdate,
        request: &OwnerCommitRequest,
    ) -> Result<PreparedEffect, String> {
        let at = self.position(key).ok_or("unknown prepared invocation")?;
        let started = self.snapshot.records.get(at).cloned();
        let (prepared, durable) = self.stage_prepared_effect(key, update, request)?;
        let started = started.ok_or("unknown prepared invocation")?;
        self.settle(at, started, durable)?;
        Ok(prepared)
    }

    /// [`Self::stage_finished_effect`], waiting for the write here.
    #[cfg(test)]
    pub(crate) fn finish_effect(
        &mut self,
        key: &InvocationKey,
        prepared: &PreparedEffect,
    ) -> Result<DurableInvocation, String> {
        let at = self.position(key).ok_or("unknown prepared invocation")?;
        let started = self.snapshot.records.get(at).cloned();
        let (record, durable) = self.stage_finished_effect(key, prepared)?;
        let started = started.ok_or("unknown prepared invocation")?;
        self.settle(at, started, durable)?;
        Ok(record)
    }

    /// The prepared effect, durable once the returned write resolves.
    pub(crate) fn stage_prepared_effect(
        &mut self,
        key: &InvocationKey,
        update: InvocationUpdate,
        request: &OwnerCommitRequest,
    ) -> Result<(PreparedEffect, Durable), String> {
        self.ready()?;
        let prepared = PreparedEffect::new(update, request)?;
        let (at, record) = self
            .record_mut(key)
            .map_err(|_| "unknown prepared invocation")?;
        prepared.validate(record)?;
        if let Some(previous) = &record.prepared_effect {
            if previous != &prepared {
                return Err("prepared terminal receipt changed".into());
            }
            let previous = previous.clone();
            return Ok((previous, self.in_flight(&key.invocation)));
        }
        if record.update.is_terminal() || record.uncertain {
            return Err("cannot prepare a retired invocation".into());
        }
        record.prepared_effect = Some(prepared.clone());
        self.snapshot.version = self.snapshot.version.max(5);
        let durable = self.commit(at, Injected::default())?;
        Ok((prepared, durable))
    }

    /// The prepared effect's terminal, durable once the returned write
    /// resolves; the record as that write makes it.
    pub(crate) fn stage_finished_effect(
        &mut self,
        key: &InvocationKey,
        prepared: &PreparedEffect,
    ) -> Result<(DurableInvocation, Durable), String> {
        self.ready()?;
        let (at, record) = self
            .record_mut(key)
            .map_err(|_| "unknown prepared invocation")?;
        if record.prepared_effect.as_ref() != Some(prepared) {
            return Err("prepared effect reconciliation changed".into());
        }
        prepared.validate(record)?;
        if record.update.is_terminal() && !record.uncertain {
            let record = record.clone();
            return Ok((record, self.in_flight(&key.invocation)));
        }
        record.update = prepared.update.clone();
        record.uncertain = false;
        let durable = self.commit(at, Injected::default())?;
        let record = self
            .snapshot
            .records
            .get(at)
            .cloned()
            .ok_or("prepared invocation disappeared")?;
        Ok((record, durable))
    }
}
