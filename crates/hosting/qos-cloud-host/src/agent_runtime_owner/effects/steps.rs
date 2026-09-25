//! Durable parent custody of one accepted enrolled task's managed child steps.
//!
//! Every method first rechecks the original live dispatch custody under this
//! Store guard (the same witness check as task actions). `bind` and `commit`
//! return only after the private fact is committed. An identical repeat returns
//! the retained record; any different child, intent, result or continuation
//! for the same step is a conflict. Retained labels never grant authority.
use super::*;
use qos_action_broker::enrolled_dispatch::{
    ChildCommit, ChildTicket, ParentFailure, ParentReceipt, ParentRecord, ParentStep,
};
use ui::host_worker::managed_sender::ParentCommitted;

fn parent_failure(error: AgentCommitError) -> ParentFailure {
    match error {
        AgentCommitError::Uncertain { reason, .. } => ParentFailure::Uncertain(reason),
        other => ParentFailure::Unavailable(other.to_string()),
    }
}
fn bound_key(invocation: uuid::Uuid, step: u32) -> String {
    format!("{invocation}:task_step_bound:{step}")
}
fn committed_key(invocation: uuid::Uuid, step: u32) -> String {
    format!("{invocation}:task_step_committed:{step}")
}
fn decode_hex<const N: usize>(value: &str) -> Result<[u8; N], ParentFailure> {
    hex::decode(value)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| ParentFailure::Unavailable("invalid retained step bytes".into()))
}

/// The exact retained last committed step of one task, for TaskFinished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommittedStep {
    pub(crate) step: u32,
    pub(crate) result: [u8; 32],
    pub(crate) continuation: Vec<u8>,
}

impl Store {
    /// Original live custody for step operations. Lookup and commit remain
    /// available after the task became uncertain or finished so a bound child
    /// can always be recovered and released; binding a new step requires the
    /// task to be currently accepted for actions.
    fn step_custody(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
        step: Option<ParentStep>,
        new_step: bool,
    ) -> Result<(VerifiedAgentCaller, AgentControlId), ParentFailure> {
        let id = AgentControlId::new(dispatch.invocation()).map_err(parent_failure)?;
        if step.is_some_and(|step| step.parent() != *dispatch.invocation().as_bytes()) {
            return Err(ParentFailure::Conflict);
        }
        let caller = if new_step {
            // Same witness as task actions: live grants, not uncertain.
            self.validate_dispatch_witness(dispatch)
                .map_err(parent_failure)?
                .0
        } else {
            // Exact original process, caller and envelope; still valid for an
            // uncertain or finished task so bound children stay recoverable.
            let live = dispatch.revalidate().map_err(ParentFailure::Unavailable)?;
            self.validate_enrolled(&id, dispatch.caller(), &live, dispatch.canonical_envelope())
                .map_err(parent_failure)?
                .0
        };
        Ok((caller, id))
    }
    fn step_record(
        &self,
        id: &AgentControlId,
        index: u32,
    ) -> Result<Option<ParentRecord>, ParentFailure> {
        let ledger = self.runtime_ledger().map_err(parent_failure)?;
        let Some(bound) = ledger.facts.get(&bound_key(id.invocation(), index)) else {
            return Ok(None);
        };
        let AgentRuntimeFact::TaskStepBound { child, intent, .. } = &bound.change.fact else {
            return Err(ParentFailure::Unavailable(
                "invalid retained step kind".into(),
            ));
        };
        let child = ChildTicket::from_bytes(&decode_hex::<48>(child)?)
            .ok_or_else(|| ParentFailure::Unavailable("invalid retained child".into()))?;
        let committed = match ledger.facts.get(&committed_key(id.invocation(), index)) {
            None => None,
            Some(receipt) => {
                let AgentRuntimeFact::TaskStepCommitted {
                    result,
                    continuation,
                    ..
                } = &receipt.change.fact
                else {
                    return Err(ParentFailure::Unavailable(
                        "invalid retained commit kind".into(),
                    ));
                };
                Some(ParentCommitted {
                    result: decode_hex::<32>(result)?,
                    continuation: hex::decode(continuation).map_err(|_| {
                        ParentFailure::Unavailable("invalid retained continuation".into())
                    })?,
                })
            }
        };
        Ok(Some(ParentRecord {
            step: ParentStep::new(*id.invocation().as_bytes(), index)
                .map_err(|_| ParentFailure::Conflict)?,
            child,
            intent: decode_hex::<32>(intent)?,
            committed,
        }))
    }
    /// Steps bound for this task, and whether the last one is uncommitted.
    pub(crate) fn agent_task_steps(
        &self,
        id: &AgentControlId,
    ) -> Result<(u32, Option<CommittedStep>, bool), AgentCommitError> {
        let ledger = self.runtime_ledger()?;
        let mut index = 0;
        let mut last = None;
        let mut open = false;
        while let Some(bound) = ledger.facts.get(&bound_key(id.invocation(), index)) {
            let _ = bound;
            match ledger.facts.get(&committed_key(id.invocation(), index)) {
                Some(receipt) => {
                    let AgentRuntimeFact::TaskStepCommitted {
                        result,
                        continuation,
                        ..
                    } = &receipt.change.fact
                    else {
                        return Err(rejected("invalid retained commit kind"));
                    };
                    last = Some(CommittedStep {
                        step: index,
                        result: hex::decode(result)
                            .ok()
                            .and_then(|b| b.try_into().ok())
                            .ok_or_else(|| rejected("invalid retained step result"))?,
                        continuation: hex::decode(continuation)
                            .map_err(|_| rejected("invalid retained continuation"))?,
                    });
                    open = false;
                }
                None => open = true,
            }
            index += 1;
        }
        Ok((index, last, open))
    }
    pub(crate) fn agent_task_step(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
        step: ParentStep,
    ) -> Result<Option<ParentRecord>, ParentFailure> {
        let (_, id) = self.step_custody(dispatch, Some(step), false)?;
        self.step_record(&id, step.index())
    }
    pub(crate) fn agent_task_step_child(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
        child: ChildTicket,
    ) -> Result<Option<ParentRecord>, ParentFailure> {
        let (_, id) = self.step_custody(dispatch, None, false)?;
        match self.find_step_child(child)? {
            None => Ok(None),
            // A child owned by another task is never this task's step.
            Some(record) if record.step.parent() != *id.invocation().as_bytes() => {
                Err(ParentFailure::Conflict)
            }
            Some(record) => Ok(Some(record)),
        }
    }
    fn find_step_child(&self, child: ChildTicket) -> Result<Option<ParentRecord>, ParentFailure> {
        let wanted = hex::encode(child.to_bytes());
        let ledger = self.runtime_ledger().map_err(parent_failure)?;
        let found = ledger
            .facts
            .values()
            .find_map(|receipt| match &receipt.change.fact {
                AgentRuntimeFact::TaskStepBound { child, step, .. } if *child == wanted => {
                    Some((receipt.change.invocation.clone(), *step))
                }
                _ => None,
            });
        let Some((invocation, index)) = found else {
            return Ok(None);
        };
        let id = control_id(&invocation).map_err(parent_failure)?;
        self.step_record(&id, index)
    }
    pub(crate) fn bind_agent_task_step(
        &mut self,
        dispatch: &VerifiedEnrolledDispatch,
        step: ParentStep,
        child: ChildTicket,
        intent: [u8; 32],
    ) -> Result<ParentRecord, ParentFailure> {
        let (caller, id) = self.step_custody(dispatch, Some(step), true)?;
        if let Some(record) = self.step_record(&id, step.index())? {
            return if record.child == child && record.intent == intent {
                Ok(record)
            } else {
                Err(ParentFailure::Conflict)
            };
        }
        // A new step needs the task currently accepted for actions.
        let ledger = self.runtime_ledger().map_err(parent_failure)?;
        if !self
            .agent_runtime_phase(&caller, &id)
            .map_err(parent_failure)?
            .1
            || ledger
                .facts
                .contains_key(&format!("{}:task_finished", id.invocation()))
            || ledger
                .facts
                .contains_key(&format!("{}:task_unknown", id.invocation()))
        {
            return Err(ParentFailure::Unavailable(
                "agent task is not currently accepted for steps".into(),
            ));
        }
        // Scoped-grant invocation rate (security review MUST-9): one token
        // per new step; v1 tuple grants are unbudgeted.
        self.admit_agent_step_rate(dispatch)
            .map_err(|e| ParentFailure::Unavailable(e.to_string()))?;
        // Content-impact baseline (MUST-8): the covered Writer text before
        // this step's child can execute.
        self.note_agent_step_start(&id, step.index());
        self.append_effect_fact(
            &id,
            caller.governance_revision(),
            AgentRuntimeFact::TaskStepBound {
                dispatch_fact: format!("{}:dispatch_intended", id.invocation()),
                step: step.index(),
                child: hex::encode(child.to_bytes()),
                intent: hex::encode(intent),
            },
        )
        .map_err(parent_failure)?;
        let record = self
            .step_record(&id, step.index())?
            .ok_or_else(|| ParentFailure::Uncertain("committed step binding missing".into()))?;
        if record.child != child || record.intent != intent {
            return Err(ParentFailure::Conflict);
        }
        Ok(record)
    }
    pub(crate) fn commit_agent_task_step(
        &mut self,
        dispatch: &VerifiedEnrolledDispatch,
        commit: ChildCommit<'_>,
    ) -> Result<ParentReceipt, ParentFailure> {
        let (caller, id) = self.step_custody(dispatch, Some(commit.step), false)?;
        let step = commit.step.index();
        let receipt = self.commit_step(&id, caller.governance_revision(), commit)?;
        // Measured content removal of the committed step (MUST-8). Over
        // budget stops the task: its next step is refused.
        self.enforce_agent_step_removal(&id, step);
        Ok(receipt)
    }
    fn commit_step(
        &mut self,
        id: &AgentControlId,
        governance: &str,
        commit: ChildCommit<'_>,
    ) -> Result<ParentReceipt, ParentFailure> {
        if commit.step.parent() != *id.invocation().as_bytes() {
            return Err(ParentFailure::Conflict);
        }
        let record = self
            .step_record(id, commit.step.index())?
            .ok_or(ParentFailure::Conflict)?;
        if record.child != commit.child || record.intent != commit.intent {
            return Err(ParentFailure::Conflict);
        }
        let receipt = ParentReceipt {
            step: commit.step,
            child: commit.child,
            result: commit.result,
        };
        if record.committed.is_some() {
            // Exact repeat only: the retained JSON and continuation must match.
            let ledger = self.runtime_ledger().map_err(parent_failure)?;
            let retained = ledger
                .facts
                .get(&committed_key(id.invocation(), commit.step.index()))
                .ok_or_else(|| ParentFailure::Uncertain("retained step commit missing".into()))?;
            let AgentRuntimeFact::TaskStepCommitted {
                result,
                result_json,
                continuation,
                ..
            } = &retained.change.fact
            else {
                return Err(ParentFailure::Conflict);
            };
            return if *result == hex::encode(commit.result)
                && result_json == commit.result_json
                && *continuation == hex::encode(commit.continuation)
            {
                Ok(receipt)
            } else {
                Err(ParentFailure::Conflict)
            };
        }
        self.append_effect_fact(
            id,
            governance,
            AgentRuntimeFact::TaskStepCommitted {
                bound_fact: bound_key(id.invocation(), commit.step.index()),
                step: commit.step.index(),
                result: hex::encode(commit.result),
                result_json: commit.result_json.into(),
                continuation: hex::encode(commit.continuation),
            },
        )
        .map_err(parent_failure)?;
        Ok(receipt)
    }

    /// Private stable seed for the Cloud sender's scope incarnation and
    /// instance: this Store's host secret, machine and room.
    pub(crate) fn sender_seed(&self) -> Vec<u8> {
        let mut seed = self.state.host_secret.to_vec();
        seed.extend_from_slice(self.state.machine_id.as_bytes());
        seed.push(0);
        seed.extend_from_slice(self.state.room.as_bytes());
        seed
    }
    /// The exact committed child result JSON of one task step.
    pub(crate) fn committed_step_result(
        &self,
        invocation: uuid::Uuid,
        index: u32,
    ) -> Result<Option<String>, AgentCommitError> {
        Ok(self
            .runtime_ledger()?
            .facts
            .get(&committed_key(invocation, index))
            .and_then(|receipt| match &receipt.change.fact {
                AgentRuntimeFact::TaskStepCommitted { result_json, .. } => {
                    Some(result_json.clone())
                }
                _ => None,
            }))
    }
    /// True when any task holds a bound step whose result is uncommitted.
    pub(crate) fn any_open_task_step(&self) -> Result<bool, AgentCommitError> {
        let Some(PrivateAgentState::V2(ledger)) = &self.state.private_agent_ledger else {
            return Ok(false);
        };
        Ok(ledger
            .facts
            .values()
            .any(|receipt| match &receipt.change.fact {
                AgentRuntimeFact::TaskStepBound { step, .. } => !ledger.facts.contains_key(
                    &format!("{}:task_step_committed:{step}", receipt.change.invocation),
                ),
                _ => false,
            }))
    }
    /// Retained journal ownership of one task's children, from its admission,
    /// dispatch envelope and process binding.
    pub(crate) fn enrolled_child_owner(
        &self,
        invocation: uuid::Uuid,
    ) -> Result<qos_action_broker::collab_broker_capnp::EnrolledChildOwner, AgentCommitError> {
        let id = AgentControlId::new(invocation)?;
        let AgentRuntimeFact::Admitted { intent, .. } = &self.admission_fact(&id)?.change.fact
        else {
            return Err(rejected("invalid retained admission"));
        };
        let ledger = self.runtime_ledger()?;
        let dispatch = ledger
            .facts
            .get(&format!("{invocation}:dispatch_intended"))
            .ok_or_else(|| rejected("task has no retained dispatch"))?;
        let AgentRuntimeFact::DispatchIntended { envelope, .. } = &dispatch.change.fact else {
            return Err(rejected("invalid retained dispatch"));
        };
        let envelope: runtime_collab::EnrolledTaskEnvelope =
            serde_json::from_str(envelope).map_err(|e| rejected(&e.to_string()))?;
        Ok(qos_action_broker::collab_broker_capnp::EnrolledChildOwner {
            root: intent.scope.caller.user_public_key,
            device: intent.scope.caller.device_public_key,
            task: envelope.task_id,
            run: envelope.run_id,
            channel: envelope.channel_lease,
            room: self.state.room.clone(),
        })
    }
    // Host recovery parent: the Cloud's own installed sender reconciles its
    // retained children after restart, when no live enrolled dispatch exists.
    // It can only read and settle steps that are already durably bound; it
    // never binds a new step or grants the task any authority.
    pub(crate) fn recovery_task_step(
        &self,
        step: ParentStep,
    ) -> Result<Option<ParentRecord>, ParentFailure> {
        let id =
            AgentControlId::new(uuid::Uuid::from_bytes(step.parent())).map_err(parent_failure)?;
        self.step_record(&id, step.index())
    }
    pub(crate) fn recovery_task_step_child(
        &self,
        child: ChildTicket,
    ) -> Result<Option<ParentRecord>, ParentFailure> {
        self.find_step_child(child)
    }
    pub(crate) fn recovery_commit_task_step(
        &mut self,
        commit: ChildCommit<'_>,
    ) -> Result<ParentReceipt, ParentFailure> {
        let id = AgentControlId::new(uuid::Uuid::from_bytes(commit.step.parent()))
            .map_err(parent_failure)?;
        let governance = self
            .admission_fact(&id)
            .map_err(parent_failure)?
            .change
            .expected_governance
            .clone();
        self.commit_step(&id, &governance, commit)
    }
}
