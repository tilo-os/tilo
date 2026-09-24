//! Private executor v2 under the existing Store owner. No method here launches a process.
use crate::Store;
use qos_action_broker::agent_control::VerifiedAgentAdmission;
use qos_agent_control_protocol::{AgentError, AgentErrorCode, AgentRequest};
use qos_peer_state::owner_commit::*;
mod effects;
mod recovery;
#[cfg(test)]
pub(crate) use effects::enrolled_fixture::EnrolledFixtureTask;
pub use effects::{AgentAttemptCustody, AgentDispatchPermit, AgentLaunchPermit, EffectClaim};
pub(crate) use effects::{FailedLaunchAppend, RuntimeLaunchClaim};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentControlId(uuid::Uuid);
impl AgentControlId {
    pub fn new(invocation: uuid::Uuid) -> Result<Self, AgentCommitError> {
        if invocation.is_nil() {
            return Err(rejected("nil agent invocation"));
        }
        Ok(Self(invocation))
    }
    pub fn invocation(&self) -> uuid::Uuid {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentRuntimePhase {
    Admitted,
    Launching,
    Bound,
    Dispatching,
    Completed,
    Failed,
    Unknown,
}
#[derive(Clone, Debug)]
pub struct AgentRuntimeObservation {
    pub invocation: String,
    pub agent_id: String,
    pub task_id: String,
    pub run_id: String,
    pub phase: AgentRuntimePhase,
    pub qualified_completed: bool,
    pub receipt: OwnerCommitReceipt,
    pub error: Option<AgentError>,
}
#[derive(Debug)]
pub struct AdmittedAgent {
    pub id: AgentControlId,
    pub ids: AgentExecutionIdsV2,
    pub receipt: OwnerCommitReceipt,
    custody: AgentAttemptCustody,
}
impl AdmittedAgent {
    pub fn into_custody(self) -> AgentAttemptCustody {
        self.custody
    }
}
#[derive(Debug)]
pub enum AgentAdmissionResult {
    Fresh(AdmittedAgent),
    Existing(AgentRuntimeObservation),
}

impl Store {
    /// Explicit host writer selection; not called by open, read, or ordinary admission.
    pub fn activate_private_agent_runtime_v2(
        &mut self,
        caller: &VerifiedAgentCaller,
    ) -> Result<(), AgentCommitError> {
        let fresh = self
            .refresh_agent_caller(caller)
            .map_err(AgentCommitError::Rejected)?;
        let peer = self.peer().map_err(storage_error)?;
        fresh
            .revalidate(
                &peer,
                self.agent_scope(&peer)
                    .map_err(AgentCommitError::Rejected)?,
                self.state.generation,
                self.policy_now().map_err(storage_error)?,
            )
            .map_err(AgentCommitError::Rejected)?;
        if matches!(
            self.state.private_agent_ledger,
            Some(PrivateAgentState::V2(_))
        ) {
            return Ok(());
        }
        let legacy = match (
            &self.state.private_agent_ledger,
            self.state.private_schema_version().map_err(storage_error)?,
        ) {
            (None, crate::STATE_VERSION) => PrivateAgentLedger::default(),
            (Some(PrivateAgentState::V1(v)), crate::AGENT_STATE_VERSION) => v.clone(),
            _ => return Err(rejected("invalid private v2 migration source")),
        };
        // Do not choose one winner for preexisting same-caller identities in two journals.
        for record in legacy.records.values() {
            if self
                .collaboration_has_invocation(record.caller.device_public_key, &record.invocation)
            {
                return Err(rejected("existing agent/collaboration invocation conflict"));
            }
        }
        let candidate = PrivateAgentLedgerV2 {
            version: 2,
            writer_floor: 2,
            legacy_v1: legacy,
            facts: Default::default(),
        };
        candidate
            .validate(&self.state.room, self.state.generation)
            .map_err(AgentCommitError::Rejected)?;
        let mut state = self.state.clone();
        state.select_private_schema().map_err(storage_error)?;
        state.private_agent_ledger = Some(PrivateAgentState::V2(candidate));
        self.commit(state).map_err(storage_error)
    }

    /// Only native verified provider ingress can reach public admission.
    /// Plain deserialized intent is never a provider witness.
    ///
    /// ```compile_fail
    /// fn plain(store: &mut qos_cloud_host::Store,
    ///          caller: &qos_peer_state::owner_commit::VerifiedAgentCaller,
    ///          intent: &qos_peer_state::owner_commit::AgentIntentV2) {
    ///     let _ = store.admit_agent_runtime(caller, intent);
    /// }
    /// ```
    pub fn admit_agent_runtime(
        &mut self,
        caller: &VerifiedAgentCaller,
        admission: &VerifiedAgentAdmission,
    ) -> Result<AgentAdmissionResult, AgentCommitError> {
        self.ensure_reconciled().map_err(storage_error)?;
        admission.revalidate().map_err(AgentCommitError::Rejected)?;
        let current = self
            .refresh_agent_caller(caller)
            .map_err(AgentCommitError::Rejected)?;
        if current.governance_revision() != admission.governance_revision() {
            return Err(rejected("native provider grant policy changed"));
        }
        // A Message witness includes the actual live parent. Stored labels cannot supply it.
        match admission
            .intent()
            .validate(&self.state.room)
            .map_err(AgentCommitError::Rejected)?
        {
            AgentRequest::Spawn { .. } if admission.parent_process().is_none() => (),
            AgentRequest::Message { agent_id, .. } => {
                let live = admission
                    .parent_process()
                    .ok_or_else(|| rejected("live message parent unavailable"))?;
                let facts = live.facts().map_err(AgentCommitError::Rejected)?;
                let parent = self
                    .runtime_ledger()?
                    .facts
                    .values()
                    .find_map(|r| {
                        if let AgentRuntimeFact::Admitted {
                            ids,
                            parent_invocation: None,
                            intent,
                            ..
                        } = &r.change.fact
                        {
                            (ids.agent_id == agent_id
                                && intent.scope.caller == *current.certificate())
                            .then_some(&r.change.invocation)
                        } else {
                            None
                        }
                    })
                    .ok_or_else(|| rejected("retained message parent missing"))?;
                let retained = self
                    .runtime_ledger()?
                    .facts
                    .get(&format!("{parent}:process_bound"))
                    .ok_or_else(|| rejected("retained parent process missing"))?;
                let AgentRuntimeFact::ProcessBound { process, .. } = &retained.change.fact else {
                    return Err(rejected("invalid retained process kind"));
                };
                let b = &facts.binding;
                let r = &facts.runtime;
                if process.binding.room != b.room
                    || process.binding.app != b.app
                    || process.binding.agent != b.agent
                    || process.binding.task != b.task
                    || process.binding.run != b.run
                    || process.binding.launch != b.launch
                    || process.binding.generation != b.generation
                    || process.runtime.runtime_epoch != r.runtime_epoch
                    || process.runtime.package_id != r.package_id
                    || process.runtime.package_hash != r.package_hash
                    || process.runtime.executable_hash != r.executable_hash
                    || process.runtime.runtime_hash != r.runtime_hash
                    || process.channel_id != facts.channel_id
                    || process.process.pid != facts.process.pid
                    || process.process.start != facts.process.start
                    || process.supervisor.pid != facts.supervisor.pid
                    || process.supervisor.start != facts.supervisor.start
                {
                    return Err(rejected(
                        "live message custody differs from retained parent",
                    ));
                }
            }
            _ => return Err(rejected("provider proof has invalid parent custody")),
        }
        self.admit_agent_candidate(&current, admission.invocation(), admission.intent())
    }

    // Pure owner admission core. Private to this module: no external provider bypass,
    // no verified constructor, and no execution/dispatch permit is returned.
    fn admit_agent_candidate(
        &mut self,
        caller: &VerifiedAgentCaller,
        invocation: uuid::Uuid,
        intent: &AgentIntentV2,
    ) -> Result<AgentAdmissionResult, AgentCommitError> {
        self.ensure_reconciled().map_err(storage_error)?;
        let id = AgentControlId::new(invocation)?;
        let current = self
            .refresh_agent_caller(caller)
            .map_err(AgentCommitError::Rejected)?;
        let peer = self.peer().map_err(storage_error)?;
        current
            .revalidate(
                &peer,
                self.agent_scope(&peer)
                    .map_err(AgentCommitError::Rejected)?,
                self.state.generation,
                self.policy_now().map_err(storage_error)?,
            )
            .map_err(AgentCommitError::Rejected)?;
        let request = intent
            .validate(&self.state.room)
            .map_err(AgentCommitError::Rejected)?;
        if intent.scope.machine != self.state.machine_id
            || intent.scope.host != self.state.room
            || intent.scope.caller != *current.certificate()
            || intent.route.app_peer
                != qos_relay::room_workspace_id(&self.state.room)
                    .map_err(|e| rejected(&e.to_string()))?
                    .to_string()
        {
            return Err(rejected(
                "private admission differs from current Store caller/provider scope",
            ));
        }
        let key = invocation.to_string();
        if self.collaboration_has_invocation(current.device(), &key) {
            return Err(rejected(
                "same-caller collaboration invocation already reserved",
            ));
        }
        let ledger = self.runtime_ledger()?;
        ledger
            .validate(&self.state.room, self.state.generation)
            .map_err(AgentCommitError::Rejected)?;
        if let Some(prior) = ledger.facts.get(&format!("{key}:admitted")) {
            let AgentRuntimeFact::Admitted { intent: old, .. } = &prior.change.fact else {
                return Err(rejected("invalid retained admission"));
            };
            if old != intent {
                return Err(rejected("agent invocation intent conflict"));
            }
            return self
                .observe_agent_runtime(&current, &id)
                .map(AgentAdmissionResult::Existing);
        }
        if ledger.legacy_v1.records.contains_key(&key)
            || ledger
                .legacy_v1
                .records
                .values()
                .any(|r| r.ids.invocation == key)
        {
            return Err(rejected("legacy agent invocation remains reserved"));
        }
        let admissions = ledger
            .facts
            .values()
            .filter(|r| matches!(r.change.fact, AgentRuntimeFact::Admitted { .. }))
            .count();
        if admissions + ledger.legacy_v1.records.len() >= MAX_PRIVATE_AGENT_ADMISSIONS {
            return Err(AgentCommitError::Capacity {
                limit: MAX_PRIVATE_AGENT_ADMISSIONS,
            });
        }
        if matches!(request, AgentRequest::Spawn { .. }) {
            let legacy_seats = ledger
                .legacy_v1
                .records
                .values()
                .filter(|r| !r.phase.terminal())
                .count();
            let seats = ledger
                .facts
                .values()
                .filter(|r| {
                    matches!(
                        r.change.fact,
                        AgentRuntimeFact::Admitted {
                            parent_invocation: None,
                            ..
                        }
                    ) && !ledger
                        .facts
                        .contains_key(&format!("{}:failed", r.change.invocation))
                        && !ledger
                            .facts
                            .contains_key(&format!("{}:process_exited", r.change.invocation))
                })
                .count();
            if legacy_seats + seats >= 4 {
                return Err(AgentCommitError::Capacity { limit: 4 });
            }
        }
        for grant in &intent.grants {
            let record = if grant.record_id == "@qos:package-primary" {
                ""
            } else {
                &grant.record_id
            };
            if !matches!(
                grant.app_id.as_str(),
                "dev.qos.word-processor" | "dev.qos.chat"
            ) || peer.document(&grant.app_id, record).is_none()
            {
                return Err(rejected(
                    "agent grant has no current exact supported canonical record",
                ));
            }
        }
        let spawn = matches!(request, AgentRequest::Spawn { .. });
        let fresh_id = || uuid::Uuid::new_v4().to_string();
        let (agent_id, launch_id, parent_invocation) = match request {
            AgentRequest::Spawn { .. } => (fresh_id(), fresh_id(), None),
            AgentRequest::Message { agent_id, .. } => {
                let parent = ledger
                    .facts
                    .values()
                    .find_map(|r| {
                        if let AgentRuntimeFact::Admitted {
                            ids,
                            intent: parent_intent,
                            parent_invocation: None,
                            ..
                        } = &r.change.fact
                        {
                            (ids.agent_id == agent_id
                                && parent_intent.scope.caller == intent.scope.caller)
                                .then_some((ids.launch_id.clone(), r.change.invocation.clone()))
                        } else {
                            None
                        }
                    })
                    .ok_or_else(|| rejected("message has no retained same-caller spawn"))?;
                (agent_id, parent.0, Some(parent.1))
            }
            _ => return Err(rejected("private read cannot be admitted as an effect")),
        };
        let ids = AgentExecutionIdsV2 {
            agent_id,
            task_id: fresh_id(),
            run_id: fresh_id(),
            launch_id,
            message_id: fresh_id(),
        };
        let mut change = AgentRuntimeChange {
            mutation: format!("{key}:admitted"),
            invocation: key,
            expected_generation: self.state.generation,
            expected_governance: current.governance_revision().into(),
            payload_digest: [0; 32],
            fact: AgentRuntimeFact::Admitted {
                ids: ids.clone(),
                intent: intent.clone(),
                parent_invocation,
                reservation: AgentReservationV2::new(spawn),
                timestamp_ms: self.policy_now_ns().map_err(storage_error)? / 1_000_000,
            },
        };
        change.payload_digest = *blake3::hash(
            &change
                .business_bytes()
                .map_err(AgentCommitError::Rejected)?,
        )
        .as_bytes();
        let binding = OwnerCommitRequest {
            room: self.state.room.clone(),
            operation: CommitOperation::AgentRuntime {
                change: Box::new(change.clone()),
            },
        }
        .binding()
        .map_err(AgentCommitError::Rejected)?;
        let next = self.next_generation().map_err(storage_error)?;
        let mut candidate = ledger.clone();
        candidate.facts.insert(
            change.mutation.clone(),
            AgentRuntimeFactReceipt {
                binding: binding.clone(),
                generation: next,
                change,
            },
        );
        candidate
            .validate(&self.state.room, next)
            .map_err(AgentCommitError::Rejected)?;
        let mut state = self.state.clone();
        state.private_agent_ledger = Some(PrivateAgentState::V2(candidate));
        self.commit(state).map_err(storage_error)?;
        let custody = AgentAttemptCustody::fresh(
            self.runtime_ledger()?.facts[&format!("{}:admitted", id.invocation())].clone(),
            self.policy_time.clone(),
        );
        Ok(AgentAdmissionResult::Fresh(AdmittedAgent {
            id,
            ids,
            receipt: self.runtime_receipt(binding)?,
            custody,
        }))
    }

    fn runtime_ledger(&self) -> Result<&PrivateAgentLedgerV2, AgentCommitError> {
        self.ensure_reconciled().map_err(storage_error)?;
        match &self.state.private_agent_ledger {
            Some(PrivateAgentState::V2(ledger))
                if self.state.private_schema_version().map_err(storage_error)?
                    == crate::AGENT_STATE_VERSION =>
            {
                Ok(ledger)
            }
            _ => Err(rejected("explicit private v2 writer selection required")),
        }
    }
    fn runtime_receipt(
        &self,
        binding: CommitBinding,
    ) -> Result<OwnerCommitReceipt, AgentCommitError> {
        self.ensure_reconciled().map_err(storage_error)?;
        let bytes = serde_json::to_vec(&self.state).map_err(|e| AgentCommitError::Uncertain {
            generation: Some(self.state.generation),
            reason: e.to_string(),
        })?;
        Ok(OwnerCommitReceipt {
            binding,
            generation: self.state.generation,
            aggregate_hash: *blake3::hash(&bytes).as_bytes(),
        })
    }
    /// Scoped retained data for status/retry comparison, available without a live provider.
    /// Returning an intent never grants admission, launch, or dispatch permission.
    pub fn retained_agent_intent(
        &self,
        caller: &VerifiedAgentCaller,
        id: &AgentControlId,
    ) -> Result<Option<AgentIntentV2>, AgentCommitError> {
        let current = self
            .refresh_agent_caller(caller)
            .map_err(AgentCommitError::Rejected)?;
        let ledger = self.runtime_ledger()?;
        let key = id.invocation().to_string();
        if let Some(receipt) = ledger.facts.get(&format!("{key}:admitted")) {
            let AgentRuntimeFact::Admitted { intent, .. } = &receipt.change.fact else {
                return Err(rejected("invalid retained admission kind"));
            };
            if intent.scope.caller != *current.certificate() {
                return Err(rejected("private invocation belongs to another caller"));
            }
            return Ok(Some(intent.clone()));
        }
        if ledger.legacy_v1.records.contains_key(&key) {
            return Err(rejected("legacy invocation is retained status-only"));
        }
        Ok(None)
    }
    pub fn observe_agent_intent(
        &self,
        caller: &VerifiedAgentCaller,
        id: &AgentControlId,
        intent: &AgentIntentV2,
    ) -> Result<Option<AgentRuntimeObservation>, AgentCommitError> {
        let Some(retained) = self.retained_agent_intent(caller, id)? else {
            return Ok(None);
        };
        if retained != *intent {
            return Err(rejected("agent invocation intent conflict"));
        }
        self.observe_agent_runtime(caller, id).map(Some)
    }

    pub fn observe_agent_runtime(
        &self,
        caller: &VerifiedAgentCaller,
        id: &AgentControlId,
    ) -> Result<AgentRuntimeObservation, AgentCommitError> {
        let current = self
            .refresh_agent_caller(caller)
            .map_err(AgentCommitError::Rejected)?;
        let ledger = self.runtime_ledger()?;
        let invocation = id.0.to_string();
        if let Some(legacy) = ledger
            .legacy_v1
            .record_for(&current, &invocation)
            .map_err(AgentCommitError::Rejected)?
        {
            let admission = ledger
                .legacy_v1
                .mutations
                .get(&legacy.ids.mutation)
                .ok_or_else(|| rejected("legacy admission receipt missing"))?;
            let phase = match legacy.phase {
                AgentControlPhase::Completed => AgentRuntimePhase::Completed,
                AgentControlPhase::Failed => AgentRuntimePhase::Failed,
                _ => AgentRuntimePhase::Unknown,
            };
            return Ok(AgentRuntimeObservation {
                invocation,
                agent_id: legacy.ids.agent.clone(),
                task_id: legacy.ids.task.clone(),
                run_id: legacy.ids.run.clone(),
                phase,
                qualified_completed: false,
                receipt: self.runtime_receipt(admission.binding.clone())?,
                error: legacy.error.clone().or_else(|| {
                    (phase == AgentRuntimePhase::Unknown).then(|| AgentError {
                        code: AgentErrorCode::OutcomeUncertain,
                        message: "Imported admission has no live executor custody".into(),
                    })
                }),
            });
        }
        let admission = ledger
            .facts
            .get(&format!("{invocation}:admitted"))
            .ok_or_else(|| rejected("agent invocation not found"))?;
        let AgentRuntimeFact::Admitted { ids, intent, .. } = &admission.change.fact else {
            return Err(rejected("invalid admission kind"));
        };
        if intent.scope.caller != *current.certificate() {
            return Err(rejected("private operation belongs to another caller"));
        }
        let mut phase = AgentRuntimePhase::Admitted;
        let mut error = None;
        let mut binding = admission.binding.clone();
        let mut facts = ledger
            .facts
            .values()
            .filter(|r| r.change.invocation == invocation)
            .collect::<Vec<_>>();
        facts.sort_by_key(|r| r.generation);
        for fact in facts {
            if matches!(
                fact.change.fact,
                AgentRuntimeFact::LaunchIntended { .. }
                    | AgentRuntimeFact::ProcessBound { .. }
                    | AgentRuntimeFact::DispatchIntended { .. }
                    | AgentRuntimeFact::Completed { .. }
                    | AgentRuntimeFact::Failed { .. }
                    | AgentRuntimeFact::Unknown { .. }
            ) {
                binding = fact.binding.clone();
            }
            match &fact.change.fact {
                AgentRuntimeFact::LaunchIntended { .. } => phase = AgentRuntimePhase::Launching,
                AgentRuntimeFact::ProcessBound { .. } => phase = AgentRuntimePhase::Bound,
                AgentRuntimeFact::DispatchIntended { .. } => phase = AgentRuntimePhase::Dispatching,
                AgentRuntimeFact::Completed { .. } => {
                    phase = AgentRuntimePhase::Completed;
                    error = None;
                }
                AgentRuntimeFact::Failed { error: e, .. } => {
                    phase = AgentRuntimePhase::Failed;
                    error = Some(e.clone());
                }
                AgentRuntimeFact::Unknown { error: e, .. } => {
                    phase = AgentRuntimePhase::Unknown;
                    error = Some(e.clone());
                }
                _ => (),
            }
        }
        Ok(AgentRuntimeObservation {
            invocation,
            agent_id: ids.agent_id.clone(),
            task_id: ids.task_id.clone(),
            run_id: ids.run_id.clone(),
            phase,
            qualified_completed: phase == AgentRuntimePhase::Completed,
            receipt: self.runtime_receipt(binding)?,
            error,
        })
    }
    fn collaboration_has_invocation(&self, device: [u8; 32], invocation: &str) -> bool {
        let scope = format!("{}:{invocation}", hex::encode(device));
        self.state.canonical_intents.contains_key(&scope)
            || self.state.receipts.contains_key(&scope)
    }
    /// Reciprocal check preserves the existing collaboration device:UUID namespace.
    pub(crate) fn private_agent_invocation_reserved(
        &self,
        device: [u8; 32],
        invocation: uuid::Uuid,
    ) -> Result<bool, String> {
        self.ensure_reconciled().map_err(|e| e.to_string())?;
        let Some(selected) = &self.state.private_agent_ledger else {
            return Ok(false);
        };
        let key = invocation.to_string();
        if selected.legacy().records.values().any(|r| {
            (r.invocation == key || r.ids.invocation == key) && r.caller.device_public_key == device
        }) {
            return Ok(true);
        }
        if let PrivateAgentState::V2(ledger) = selected {
            if let Some(receipt) = ledger.facts.get(&format!("{key}:admitted")) {
                let AgentRuntimeFact::Admitted { intent, .. } = &receipt.change.fact else {
                    return Err("invalid private admission kind".into());
                };
                return Ok(intent.scope.caller.device_public_key == device);
            }
        }
        Ok(false)
    }
}
fn rejected(reason: &str) -> AgentCommitError {
    AgentCommitError::Rejected(reason.into())
}
fn storage_error(error: anyhow::Error) -> AgentCommitError {
    AgentCommitError::Uncertain {
        generation: error
            .downcast_ref::<crate::StoreCommitFailure>()
            .map(|e| e.generation),
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests;

/// Actual local Store admission fixture; it never creates provider/process evidence.
#[cfg(test)]
pub(crate) fn operator_receipt_fixture() -> (tempfile::TempDir, uuid::Uuid, [u8; 32], String, String)
{
    let (dir, mut store, device) = tests::enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    let invocation = uuid::Uuid::from_u128(0x1357);
    let intent = tests::intent(&store, &caller);
    let AgentAdmissionResult::Fresh(admitted) = store
        .admit_agent_candidate(&caller, invocation, &intent)
        .unwrap()
    else {
        panic!("actual fresh owner admission required")
    };
    let digest = admitted.receipt.binding.request_digest;
    let agent = admitted.ids.agent_id.clone();
    let task = admitted.ids.task_id.clone();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    (dir, invocation, digest, agent, task)
}

#[cfg(test)]
pub(crate) use effects::LaunchCandidateTestClaim;

#[cfg(test)]
pub(crate) fn launch_stage_test_enrolled() -> (tempfile::TempDir, Store, [u8; 32]) {
    tests::enrolled()
}
#[cfg(test)]
pub(crate) fn launch_stage_test_intent(
    store: &Store,
    caller: &VerifiedAgentCaller,
) -> AgentIntentV2 {
    tests::intent(store, caller)
}

#[cfg(test)]
pub(crate) fn launch_stage_test_admit(
    store: &mut Store,
    caller: &VerifiedAgentCaller,
    id: uuid::Uuid,
    intent: &AgentIntentV2,
) -> Result<AgentAdmissionResult, AgentCommitError> {
    store.admit_agent_candidate(caller, id, intent)
}
