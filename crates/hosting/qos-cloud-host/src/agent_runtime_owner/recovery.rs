//! Closed pre-launch recovery. Retained data alone never grants runtime custody.
use super::*;
use crate::agent_runtime_adapter::RuntimeOwner;
use qos_action_broker::peer_actions::PolicyTime;

fn sole<'a>(ledger: &'a PrivateAgentLedgerV2, id: &str) -> Option<&'a AgentRuntimeFactReceipt> {
    let mut facts = ledger.facts.values().filter(|r| r.change.invocation == id);
    let receipt = facts.next()?;
    if facts.next().is_some() {
        return None;
    }
    match &receipt.change.fact {
        AgentRuntimeFact::Admitted {
            intent,
            parent_invocation: None,
            ..
        } if matches!(
            intent.validate(&receipt.binding.room).ok(),
            Some(AgentRequest::Spawn { .. })
        ) =>
        {
            Some(receipt)
        }
        _ => None,
    }
}
impl Store {
    /// Structural classification only, also usable before runtime installation.
    pub(crate) fn sole_admitted_spawn(
        &self,
        id: &AgentControlId,
    ) -> Result<Option<&AgentRuntimeFactReceipt>, AgentCommitError> {
        let ledger = self.runtime_ledger()?;
        ledger
            .validate(&self.state.room, self.state.generation)
            .map_err(AgentCommitError::Rejected)?;
        Ok(sole(ledger, &id.invocation().to_string()))
    }
    /// Bounded, deterministic scan. The serial runtime owns the cursor and work capacity.
    pub(crate) fn pending_spawn_ids(
        &self,
        after: Option<uuid::Uuid>,
        limit: usize,
    ) -> Result<Vec<uuid::Uuid>, AgentCommitError> {
        let ledger = self.runtime_ledger()?;
        ledger
            .validate(&self.state.room, self.state.generation)
            .map_err(AgentCommitError::Rejected)?;
        Ok(ledger
            .facts
            .values()
            .filter_map(|receipt| {
                if !matches!(receipt.change.fact, AgentRuntimeFact::Admitted { .. }) {
                    return None;
                }
                let id: uuid::Uuid = receipt.change.invocation.parse().ok()?;
                (after.is_none_or(|a| id > a) && sole(ledger, &receipt.change.invocation).is_some())
                    .then_some(id)
            })
            .take(limit.min(MAX_PRIVATE_AGENT_ADMISSIONS))
            .collect())
    }
    /// The only reconstruction door: actual installed owner/source, current runtime,
    /// exact retained caller/grants and the entire fact set are checked under Store.
    pub(crate) fn recover_admitted_spawn(
        &self,
        id: &AgentControlId,
        owner: &RuntimeOwner,
        time: &PolicyTime,
    ) -> Result<Option<AdmittedAgent>, AgentCommitError> {
        let Some(admission) = self.sole_admitted_spawn(id)? else {
            return Ok(None);
        };
        owner
            .validate_spawn_current(self, id, time)
            .map_err(AgentCommitError::Rejected)?;
        let AgentRuntimeFact::Admitted { ids, .. } = &admission.change.fact else {
            unreachable!()
        };
        Ok(Some(AdmittedAgent {
            id: id.clone(),
            ids: ids.clone(),
            receipt: self.runtime_receipt(admission.binding.clone())?,
            custody: AgentAttemptCustody::fresh(admission.clone(), Some(time.clone())),
        }))
    }
    pub(crate) fn current_spawn_intent(
        &self,
        id: &AgentControlId,
    ) -> Result<&AgentIntentV2, AgentCommitError> {
        self.current_agent_effect(id)?;
        let admission = self.admission_fact(id)?;
        let AgentRuntimeFact::Admitted {
            intent,
            parent_invocation: None,
            ..
        } = &admission.change.fact
        else {
            return Err(rejected("recovery is limited to Spawn"));
        };
        if !matches!(
            intent
                .validate(&self.state.room)
                .map_err(AgentCommitError::Rejected)?,
            AgentRequest::Spawn { .. }
        ) {
            return Err(rejected("recovery is limited to Spawn"));
        }
        Ok(intent)
    }
}

#[cfg(test)]
mod tests;
