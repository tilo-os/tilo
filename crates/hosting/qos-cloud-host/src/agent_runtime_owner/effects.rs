//! Store effect facts and live, single-use runtime custody. Stored labels grant no authority.
use super::*;
use launcher::managed::{
    ChannelProcessFacts, ManagedBinding, ManagedRuntimeFacts, VerifiedManagedExit,
    VerifiedManagedProcess, VerifiedManagedRuntime,
};
use qos_action_broker::enrolled_dispatch::{
    TaskRunUpdate, VerifiedEnrolledDispatch, VerifiedRecipientAcceptance, VerifiedTaskUpdate,
};
use std::sync::atomic::{AtomicBool, Ordering};

/// Minted after admission, or by the closed sole-Admitted recovery door. Never on open.
/// Atomic stage reservations hold no lock across Store, launcher or broker callbacks.
pub struct AgentAttemptCustody {
    policy_time: Option<qos_action_broker::peer_actions::PolicyTime>,
    admission: AgentRuntimeFactReceipt,
    stages: [AtomicBool; 2],
}
impl AgentAttemptCustody {
    pub(super) fn fresh(
        admission: AgentRuntimeFactReceipt,
        policy_time: Option<qos_action_broker::peer_actions::PolicyTime>,
    ) -> Self {
        Self {
            policy_time,
            admission,
            stages: [AtomicBool::new(false), AtomicBool::new(false)],
        }
    }
    pub fn id(&self) -> AgentControlId {
        AgentControlId(
            uuid::Uuid::parse_str(&self.admission.change.invocation)
                .expect("validated admission UUID"),
        )
    }
    fn validate_time(&self, store: &Store) -> Result<(), AgentCommitError> {
        require_policy_time(store, self.policy_time.as_ref())
    }
    fn reserve(&self, store: &Store, stage: usize) -> Result<bool, AgentCommitError> {
        self.validate_time(store)?;
        let retained = store
            .runtime_ledger()?
            .facts
            .get(&self.admission.change.mutation)
            .ok_or_else(|| rejected("live admission custody lost retained fact"))?;
        if retained != &self.admission {
            return Err(rejected("live admission custody differs from Store"));
        }
        Ok(self.stages[stage]
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok())
    }
}
impl std::fmt::Debug for AgentAttemptCustody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentAttemptCustody")
            .field("admission", &self.admission)
            .field("has_policy_time", &self.policy_time.is_some())
            .finish_non_exhaustive()
    }
}
fn require_policy_time(
    store: &Store,
    retained: Option<&qos_action_broker::peer_actions::PolicyTime>,
) -> Result<(), AgentCommitError> {
    match (retained, store.policy_time.as_ref()) {
        (None, None) => Ok(()), // Explicit legacy admission and legacy owner only.
        (Some(original), Some(current)) if original.same_origin(current) => {
            original.now_ns().map_err(|e| rejected(&e.to_string()))?;
            current.now_ns().map_err(|e| rejected(&e.to_string()))?;
            Ok(())
        }
        _ => Err(rejected("effect custody lost original owner policy time")),
    }
}
pub enum EffectClaim<P> {
    Fresh {
        permit: P,
        receipt: OwnerCommitReceipt,
    },
    Existing(AgentRuntimeObservation),
}
/// No Clone/Serde/data constructor. Only a fresh launch-intent barrier issues this token.
pub struct AgentLaunchPermit {
    policy_time: Option<qos_action_broker::peer_actions::PolicyTime>,
    binding: ManagedBinding,
    runtime: VerifiedManagedRuntime,
    receipt: OwnerCommitReceipt,
}
impl AgentLaunchPermit {
    pub fn binding(&self) -> &ManagedBinding {
        &self.binding
    }
    pub fn consume(
        self,
        store: &Store,
        binding: &ManagedBinding,
    ) -> Result<OwnerCommitReceipt, AgentCommitError> {
        require_policy_time(store, self.policy_time.as_ref())?;
        if binding != &self.binding {
            return Err(rejected("launch permit binding differs"));
        }
        store.revalidate_agent_launch(binding, &self.runtime)?;
        store.require_effect_receipt(&self.receipt)?;
        store.runtime_receipt(self.receipt.binding)
    }
}
/// The actual dispatch witness stays live until the adapter consumes the fresh permit.
pub struct AgentDispatchPermit {
    policy_time: Option<qos_action_broker::peer_actions::PolicyTime>,
    dispatch: VerifiedEnrolledDispatch,
    receipt: OwnerCommitReceipt,
}
impl AgentDispatchPermit {
    pub fn consume(self, store: &Store) -> Result<OwnerCommitReceipt, AgentCommitError> {
        require_policy_time(store, self.policy_time.as_ref())?;
        let (caller, _) = store.validate_dispatch_witness(&self.dispatch)?;
        let id = AgentControlId::new(self.dispatch.invocation())?;
        if store.observe_agent_runtime(&caller, &id)?.phase != AgentRuntimePhase::Dispatching {
            return Err(rejected(
                "dispatch permit no longer has a current intended effect",
            ));
        }
        store.require_effect_receipt(&self.receipt)?;
        store.runtime_receipt(self.receipt.binding)
    }
}
enum PreparedEffectFact {
    Existing(CommitBinding),
    Fresh {
        state: crate::State,
        binding: CommitBinding,
    },
}

enum FactCommit {
    Fresh(OwnerCommitReceipt),
    Existing(OwnerCommitReceipt),
}
impl FactCommit {
    fn receipt(self) -> OwnerCommitReceipt {
        match self {
            Self::Fresh(r) | Self::Existing(r) => r,
        }
    }
}
impl Store {
    pub(super) fn admission_fact(
        &self,
        id: &AgentControlId,
    ) -> Result<&AgentRuntimeFactReceipt, AgentCommitError> {
        self.runtime_ledger()?
            .facts
            .get(&format!("{}:admitted", id.invocation()))
            .ok_or_else(|| rejected("retained v2 admission missing"))
    }
    fn original_agent_caller(
        &self,
        id: &AgentControlId,
    ) -> Result<VerifiedAgentCaller, AgentCommitError> {
        let admission = self.admission_fact(id)?;
        let AgentRuntimeFact::Admitted { intent, .. } = &admission.change.fact else {
            return Err(rejected("invalid retained admission"));
        };
        let caller = self
            .agent_caller(intent.scope.caller.device_public_key)
            .map_err(AgentCommitError::Rejected)?;
        if caller.certificate() != &intent.scope.caller {
            return Err(rejected("original agent caller changed"));
        }
        Ok(caller)
    }
    pub(super) fn current_agent_effect(
        &self,
        id: &AgentControlId,
    ) -> Result<VerifiedAgentCaller, AgentCommitError> {
        self.current_agent_effect_pinned(id, GovernancePin::Own)
    }
    /// Governance revision recorded by the latest admitted control (the
    /// Spawn `id` itself or a later Message to it) of one agent process.
    fn latest_control_governance(&self, id: &AgentControlId) -> Result<String, AgentCommitError> {
        let spawn = id.invocation().to_string();
        self.runtime_ledger()?
            .facts
            .values()
            .filter(|receipt| match &receipt.change.fact {
                AgentRuntimeFact::Admitted {
                    parent_invocation, ..
                } => {
                    receipt.change.invocation == spawn
                        || parent_invocation.as_deref() == Some(spawn.as_str())
                }
                _ => false,
            })
            // A refused or failed control never re-pins the process (R2).
            .filter(|receipt| {
                receipt.change.invocation == spawn
                    || !self.runtime_ledger().is_ok_and(|ledger| {
                        ledger
                            .facts
                            .contains_key(&format!("{}:failed", receipt.change.invocation))
                    })
            })
            .max_by_key(|receipt| receipt.generation)
            .map(|receipt| receipt.change.expected_governance.clone())
            .ok_or_else(|| rejected("retained v2 admission missing"))
    }
    fn current_agent_effect_pinned(
        &self,
        id: &AgentControlId,
        pin: GovernancePin,
    ) -> Result<VerifiedAgentCaller, AgentCommitError> {
        let caller = self.original_agent_caller(id)?;
        let admission = self.admission_fact(id)?;
        let AgentRuntimeFact::Admitted {
            intent,
            parent_invocation,
            ..
        } = &admission.change.fact
        else {
            unreachable!()
        };
        // A task stops at its next step after any policy change: each control
        // (Spawn task or Message task) is pinned to the governance revision
        // it was admitted under. The owner's later Message re-pins the agent
        // to the then-current governance (security review T12/T13, MUST-10).
        let pinned = match pin {
            GovernancePin::Own => Some(admission.change.expected_governance.clone()),
            GovernancePin::LatestControl => Some(self.latest_control_governance(id)?),
            GovernancePin::NewControl => None,
        };
        if pinned.is_some_and(|pinned| caller.governance_revision() != pinned) {
            return Err(rejected("agent governance changed since admission"));
        }
        if self.collaboration_has_invocation(caller.device(), &id.invocation().to_string()) {
            return Err(rejected("same-caller cross-journal conflict"));
        }
        let peers = self.peer_shared().map_err(storage_error)?;
        // Every record of every retained grant or scope is a current document
        // of a manifest-delegable app (no hard-coded app list).
        grant_records_current(&peers.1, &intent.grants)
            .map_err(|_| rejected("retained agent grant has no current supported record"))?;
        if let Some(parent) = parent_invocation {
            let parent_id = control_id(parent)?;
            let p = self.admission_fact(&parent_id)?;
            let AgentRuntimeFact::Admitted {
                intent: original,
                parent_invocation: None,
                ..
            } = &p.change.fact
            else {
                return Err(rejected("invalid message parent"));
            };
            if original.scope != intent.scope || original.grants != intent.grants {
                return Err(rejected("message changed original caller/grants"));
            }
            // The Message's own pin (checked above) covers governance; its
            // parent must otherwise still be a current effect.
            self.current_agent_effect_pinned(&parent_id, GovernancePin::NewControl)?;
            if self
                .runtime_ledger()?
                .facts
                .contains_key(&format!("{parent}:process_exited"))
            {
                return Err(rejected("message parent exited"));
            }
        }
        Ok(caller)
    }
    fn require_effect_receipt(&self, receipt: &OwnerCommitReceipt) -> Result<(), AgentCommitError> {
        let CommitTarget::AgentRuntime { mutation } = &receipt.binding.target else {
            return Err(rejected("wrong effect receipt target"));
        };
        let retained = self
            .runtime_ledger()?
            .facts
            .get(mutation)
            .ok_or_else(|| rejected("effect receipt missing"))?;
        if retained.binding != receipt.binding || retained.generation != receipt.generation {
            return Err(rejected("effect receipt lost original commit"));
        }
        Ok(())
    }
    fn append_effect_fact(
        &mut self,
        id: &AgentControlId,
        governance: &str,
        fact: AgentRuntimeFact,
    ) -> Result<FactCommit, AgentCommitError> {
        match self.prepare_effect_fact(id, governance, fact)? {
            PreparedEffectFact::Existing(binding) => {
                self.runtime_receipt(binding).map(FactCommit::Existing)
            }
            PreparedEffectFact::Fresh { state, binding } => {
                self.commit(state).map_err(storage_error)?;
                self.runtime_receipt(binding).map(FactCommit::Fresh)
            }
        }
    }
    fn prepare_effect_fact(
        &self,
        id: &AgentControlId,
        governance: &str,
        mut fact: AgentRuntimeFact,
    ) -> Result<PreparedEffectFact, AgentCommitError> {
        self.ensure_reconciled().map_err(storage_error)?;
        if matches!(fact, AgentRuntimeFact::Admitted { .. }) {
            return Err(rejected("effect writer cannot admit control"));
        }
        let key = fact.mutation_key(&id.invocation().to_string());
        let ledger = self.runtime_ledger()?;
        if let Some(old) = ledger.facts.get(&key) {
            if let (Some(new_time), Some(old_time)) =
                (timestamp_mut(&mut fact), timestamp(&old.change.fact))
            {
                *new_time = old_time;
            }
            if old.change.fact != fact {
                return Err(rejected("immutable effect fact conflict"));
            }
            return Ok(PreparedEffectFact::Existing(old.binding.clone()));
        }
        let mut change = AgentRuntimeChange {
            mutation: key.clone(),
            invocation: id.invocation().to_string(),
            expected_generation: self.state.generation,
            expected_governance: governance.into(),
            payload_digest: [0; 32],
            fact,
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
            key,
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
        Ok(PreparedEffectFact::Fresh { state, binding })
    }
    // Conservative initial-only data core for Store tests; no runtime/process witness.
    fn claim_launch_candidate(
        &mut self,
        custody: &AgentAttemptCustody,
        runtime: ManagedRuntimeFactsV2,
    ) -> Result<(FactCommit, ManagedBinding), AgentCommitError> {
        match self.claim_launch_candidate_for_runtime(custody, runtime, None) {
            launch::LaunchCandidateClaim::Committed(receipt, binding) => Ok((receipt, binding)),
            launch::LaunchCandidateClaim::Refused { error, .. }
            | launch::LaunchCandidateClaim::PostCommit(error) => Err(error),
            launch::LaunchCandidateClaim::Failed(failed) => Err(failed.into_error()),
        }
    }
    /// A retained ID cannot replace the live fresh-admission handoff.
    /// ```compile_fail
    /// fn restored(s: &mut qos_cloud_host::Store,
    /// caller: &qos_peer_state::owner_commit::VerifiedAgentCaller,
    /// id: &qos_cloud_host::agent_runtime_owner::AgentControlId,
    /// runtime: &launcher::managed::VerifiedManagedRuntime) {
    ///     let _ = s.claim_agent_launch(caller, id, runtime);
    /// }
    /// ```
    pub fn claim_agent_launch(
        &mut self,
        caller: &VerifiedAgentCaller,
        custody: &AgentAttemptCustody,
        runtime: &VerifiedManagedRuntime,
    ) -> Result<EffectClaim<AgentLaunchPermit>, AgentCommitError> {
        custody.validate_time(self)?;
        let id = custody.id();
        let current = self
            .refresh_agent_caller(caller)
            .map_err(AgentCommitError::Rejected)?;
        if current.certificate() != self.original_agent_caller(&id)?.certificate() {
            return Err(rejected("launch caller differs"));
        }
        let observation = self.observe_agent_runtime(&current, &id)?;
        if observation.phase != AgentRuntimePhase::Admitted
            || custody.stages[0].load(Ordering::Acquire)
        {
            return Ok(EffectClaim::Existing(observation));
        }
        let facts = runtime.revalidate().map_err(AgentCommitError::Rejected)?;
        let (result, binding) = self.claim_launch_candidate(custody, runtime_to_data(&facts))?;
        match result {
            FactCommit::Fresh(receipt) => Ok(EffectClaim::Fresh {
                permit: AgentLaunchPermit {
                    policy_time: custody.policy_time.clone(),
                    binding,
                    runtime: runtime.clone(),
                    receipt: receipt.clone(),
                },
                receipt,
            }),
            FactCommit::Existing(_) => self
                .observe_agent_runtime(&current, &id)
                .map(EffectClaim::Existing),
        }
    }
    pub fn revalidate_agent_launch(
        &self,
        binding: &ManagedBinding,
        runtime: &VerifiedManagedRuntime,
    ) -> Result<(), AgentCommitError> {
        let r = runtime_to_data(&runtime.revalidate().map_err(AgentCommitError::Rejected)?);
        let receipt = self.runtime_ledger()?.facts.values().find(|v| matches!(&v.change.fact, AgentRuntimeFact::LaunchIntended { binding: b, .. } if b.launch == binding.launch)).ok_or_else(|| rejected("launch intent missing"))?;
        let AgentRuntimeFact::LaunchIntended {
            binding: b,
            runtime: expected,
        } = &receipt.change.fact
        else {
            unreachable!()
        };
        if binding_to_live(b) != *binding || *expected != r {
            return Err(rejected("launch/runtime binding differs"));
        }
        let id = control_id(&receipt.change.invocation)?;
        let caller = self.current_agent_effect(&id)?;
        let phase = self.observe_agent_runtime(&caller, &id)?.phase;
        if matches!(
            phase,
            AgentRuntimePhase::Unknown | AgentRuntimePhase::Failed
        ) || self
            .runtime_ledger()?
            .facts
            .contains_key(&format!("{}:process_exited", id.invocation()))
        {
            return Err(rejected("launch is no longer current"));
        }
        Ok(())
    }
    /// A deserialized process record cannot bind a live process.
    /// ```compile_fail
    /// fn data(s: &mut qos_cloud_host::Store, id: &qos_cloud_host::agent_runtime_owner::AgentControlId,
    /// p: &qos_peer_state::owner_commit::ChannelProcessFactsV2) { let _ = s.bind_agent_process(id, p); }
    /// ```
    pub fn bind_agent_process(
        &mut self,
        id: &AgentControlId,
        process: &VerifiedManagedProcess,
    ) -> Result<OwnerCommitReceipt, AgentCommitError> {
        let live = process.facts().map_err(AgentCommitError::Rejected)?;
        let caller = self.current_agent_effect(id)?;
        let launch = format!("{}:launch_intended", id.invocation());
        let retained = self
            .runtime_ledger()?
            .facts
            .get(&launch)
            .ok_or_else(|| rejected("process has no retained launch"))?;
        let AgentRuntimeFact::LaunchIntended { binding, runtime } = &retained.change.fact else {
            unreachable!()
        };
        let facts = process_to_data(&live);
        if facts.binding != *binding || facts.runtime != *runtime {
            return Err(rejected("actual process differs from retained launch"));
        }
        self.append_effect_fact(
            id,
            caller.governance_revision(),
            AgentRuntimeFact::ProcessBound {
                launch_fact: launch,
                process: facts,
            },
        )
        .map(FactCommit::receipt)
    }
    fn validate_enrolled(
        &self,
        id: &AgentControlId,
        caller: &qos_user_identity::DeviceCert,
        live: &ChannelProcessFacts,
        canonical: &str,
    ) -> Result<(VerifiedAgentCaller, String), AgentCommitError> {
        let current = self.current_agent_effect(id)?;
        if current.certificate() != caller {
            return Err(rejected("enrolled callback changed original caller"));
        }
        let AgentRuntimeFact::Admitted {
            parent_invocation, ..
        } = &self.admission_fact(id)?.change.fact
        else {
            unreachable!()
        };
        let parent = parent_invocation
            .clone()
            .unwrap_or_else(|| id.invocation().to_string());
        let process_fact = format!("{parent}:process_bound");
        let retained = self
            .runtime_ledger()?
            .facts
            .get(&process_fact)
            .ok_or_else(|| rejected("enrolled process fact missing"))?;
        let AgentRuntimeFact::ProcessBound { process, .. } = &retained.change.fact else {
            unreachable!()
        };
        if *process != process_to_data(live) {
            return Err(rejected("enrolled callback lost exact retained process"));
        }
        if let Some(dispatch) = self
            .runtime_ledger()?
            .facts
            .get(&format!("{}:dispatch_intended", id.invocation()))
        {
            let AgentRuntimeFact::DispatchIntended { envelope, .. } = &dispatch.change.fact else {
                unreachable!()
            };
            if envelope != canonical {
                return Err(rejected("enrolled callback changed retained envelope"));
            }
        }
        Ok((current, process_fact))
    }
    fn validate_dispatch_witness(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
    ) -> Result<(VerifiedAgentCaller, String), AgentCommitError> {
        let began = std::time::Instant::now();
        let live = dispatch.revalidate().map_err(AgentCommitError::Rejected)?;
        let live_us = qos_action_broker::agent_timing::us(began);
        let stage = std::time::Instant::now();
        // Factual callback authorization is deliberately wider than new dispatch/tool use.
        self.agent_process_grants_data(&process_to_data(&live), dispatch.caller())?;
        let grants_us = qos_action_broker::agent_timing::us(stage);
        let stage = std::time::Instant::now();
        let _timing = TimingOnDrop(move || {
            qos_action_broker::agent_timing::slow(
                "dispatch_witness",
                Some(dispatch.invocation()),
                began,
                &[
                    ("live_us", live_us),
                    ("grants_us", grants_us),
                    ("enrolled_us", qos_action_broker::agent_timing::us(stage)),
                ],
            )
        });
        self.validate_enrolled(
            &AgentControlId::new(dispatch.invocation())?,
            dispatch.caller(),
            &live,
            dispatch.canonical_envelope(),
        )
    }
    pub fn claim_agent_dispatch(
        &mut self,
        custody: &AgentAttemptCustody,
        dispatch: &VerifiedEnrolledDispatch,
    ) -> Result<EffectClaim<AgentDispatchPermit>, AgentCommitError> {
        custody.validate_time(self)?;
        let id = custody.id();
        if id.invocation() != dispatch.invocation() {
            return Err(rejected("dispatch differs from live admission custody"));
        }
        let (caller, process_fact) = self.validate_dispatch_witness(dispatch)?;
        let observation = self.observe_agent_runtime(&caller, &id)?;
        if !matches!(
            observation.phase,
            AgentRuntimePhase::Admitted | AgentRuntimePhase::Bound
        ) || !custody.reserve(self, 1)?
        {
            return Ok(EffectClaim::Existing(observation));
        }
        let result = self.append_effect_fact(
            &id,
            caller.governance_revision(),
            AgentRuntimeFact::DispatchIntended {
                process_fact,
                envelope: dispatch.canonical_envelope().into(),
            },
        )?;
        match result {
            FactCommit::Fresh(receipt) => Ok(EffectClaim::Fresh {
                permit: AgentDispatchPermit {
                    policy_time: custody.policy_time.clone(),
                    dispatch: dispatch.clone(),
                    receipt: receipt.clone(),
                },
                receipt,
            }),
            FactCommit::Existing(_) => self
                .observe_agent_runtime(&caller, &id)
                .map(EffectClaim::Existing),
        }
    }
    /// Only the actual recipient RPC witness can qualify control completion.
    /// ```compile_fail
    /// fn data(s: &mut qos_cloud_host::Store, a: &qos_peer_state::owner_commit::RecipientAcceptanceV2) { let _ = s.complete_agent_control(a); }
    /// ```
    pub fn complete_agent_control(
        &mut self,
        accepted: &VerifiedRecipientAcceptance,
    ) -> Result<OwnerCommitReceipt, AgentCommitError> {
        let id = AgentControlId::new(accepted.invocation())?;
        let live = accepted.revalidate().map_err(AgentCommitError::Rejected)?;
        let (caller, _) =
            self.validate_enrolled(&id, accepted.caller(), &live, accepted.canonical_envelope())?;
        let a = accepted.acceptance();
        let acceptance = RecipientAcceptanceV2 {
            version: a.version,
            message_id: a.message_id.clone(),
            task_id: a.task_id.clone(),
            run_id: a.run_id.clone(),
            launch_id: a.launch_id.clone(),
            runtime_epoch: a.runtime_epoch.clone(),
            recipient_agent: a.recipient_agent.clone(),
            channel_lease: a.channel_lease.clone(),
            acceptance_sequence: a.acceptance_sequence,
        };
        self.append_effect_fact(
            &id,
            caller.governance_revision(),
            AgentRuntimeFact::Completed {
                dispatch_fact: format!("{}:dispatch_intended", id.invocation()),
                acceptance,
                timestamp_ms: self.policy_now_ns().map_err(storage_error)? / 1_000_000,
            },
        )
        .map(FactCommit::receipt)
    }
    pub fn record_agent_task_update(
        &mut self,
        update: &VerifiedTaskUpdate,
    ) -> Result<OwnerCommitReceipt, AgentCommitError> {
        let id = AgentControlId::new(update.invocation())?;
        let live = update.revalidate().map_err(AgentCommitError::Rejected)?;
        let (caller, _) =
            self.validate_enrolled(&id, update.caller(), &live, update.canonical_envelope())?;
        // A task cannot succeed while a bound child step is uncommitted: the
        // parent never observed that child's exact result or continuation.
        let (_, _, open_step) = self.agent_task_steps(&id)?;
        if open_step && matches!(update.update(), TaskRunUpdate::Succeeded { .. }) {
            return Err(rejected(
                "task success refused while a bound child step is uncommitted",
            ));
        }
        let (outcome, text) = match update.update() {
            TaskRunUpdate::Succeeded { message } => (TaskOutcomeV2::Succeeded, message),
            TaskRunUpdate::Failed { message } => (TaskOutcomeV2::Failed, message),
            TaskRunUpdate::Cancelled { message } => (TaskOutcomeV2::Cancelled, message),
            _ => return Err(rejected("nonterminal progress is not a terminal task fact")),
        };
        self.append_effect_fact(
            &id,
            caller.governance_revision(),
            AgentRuntimeFact::TaskFinished {
                completed_fact: format!("{}:completed", id.invocation()),
                acceptance_sequence: update.acceptance().acceptance_sequence,
                update_sequence: update.sequence(),
                outcome,
                text: text.clone(),
                timestamp_ms: self.policy_now_ns().map_err(storage_error)? / 1_000_000,
            },
        )
        .map(FactCommit::receipt)
    }
    /// `live` is the current facts of an actual live process custody value
    /// (a kernel-verified managed process, or a test-support fixture).
    pub(crate) fn agent_process_grants(
        &self,
        live: Result<ChannelProcessFacts, String>,
        caller: &qos_user_identity::DeviceCert,
    ) -> Result<(AgentControlId, Vec<GrantAction>), AgentCommitError> {
        let facts = process_to_data(&live.map_err(AgentCommitError::Rejected)?);
        self.agent_process_grants_data(&facts, caller)
    }
    pub(crate) fn authorize_agent_process(
        &self,
        live: Result<ChannelProcessFacts, String>,
        caller: &qos_user_identity::DeviceCert,
    ) -> Result<AgentControlId, AgentCommitError> {
        // A live exact channel may still deliver the original acceptance after Unknown.
        // This grants no catalogue, action, launch or fresh dispatch authority.
        let facts = process_to_data(&live.map_err(AgentCommitError::Rejected)?);
        self.agent_process_scope_data(&facts, caller)
    }
    fn agent_process_scope_data(
        &self,
        facts: &ChannelProcessFactsV2,
        caller: &qos_user_identity::DeviceCert,
    ) -> Result<AgentControlId, AgentCommitError> {
        self.agent_process_scope_pinned(facts, caller, GovernancePin::LatestControl)
    }
    fn agent_process_scope_pinned(
        &self,
        facts: &ChannelProcessFactsV2,
        caller: &qos_user_identity::DeviceCert,
        pin: GovernancePin,
    ) -> Result<AgentControlId, AgentCommitError> {
        let retained = self.runtime_ledger()?.facts.values().find(|r| matches!(&r.change.fact, AgentRuntimeFact::ProcessBound { process, .. } if process == facts))
            .ok_or_else(|| rejected("live agent process has no exact retained binding"))?;
        let id = control_id(&retained.change.invocation)?;
        let current = self.current_agent_effect_pinned(&id, pin)?;
        let (phase, _) = self.agent_runtime_phase(&current, &id)?;
        if current.certificate() != caller
            || phase == AgentRuntimePhase::Failed
            || self
                .runtime_ledger()?
                .facts
                .contains_key(&format!("{}:process_exited", id.invocation()))
        {
            return Err(rejected(
                "agent process caller/custody is no longer current",
            ));
        }
        let AgentRuntimeFact::Admitted {
            parent_invocation: None,
            ..
        } = &self.admission_fact(&id)?.change.fact
        else {
            return Err(rejected("process does not name a Spawn"));
        };
        Ok(id)
    }
    /// The live process's retained Spawn grants for authorizing a *new*
    /// owner Message to it: every process/caller/custody check, but the
    /// governance pin is the new Message's own (current) revision, which its
    /// admission records and every later step of it rechecks.
    pub(crate) fn agent_process_grants_for_new_control(
        &self,
        live: Result<ChannelProcessFacts, String>,
        caller: &qos_user_identity::DeviceCert,
    ) -> Result<(AgentControlId, Vec<GrantAction>), AgentCommitError> {
        let facts = process_to_data(&live.map_err(AgentCommitError::Rejected)?);
        self.agent_process_grants_pinned(&facts, caller, GovernancePin::NewControl)
    }
    fn agent_process_grants_data(
        &self,
        facts: &ChannelProcessFactsV2,
        caller: &qos_user_identity::DeviceCert,
    ) -> Result<(AgentControlId, Vec<GrantAction>), AgentCommitError> {
        self.agent_process_grants_pinned(facts, caller, GovernancePin::LatestControl)
    }
    fn agent_process_grants_pinned(
        &self,
        facts: &ChannelProcessFactsV2,
        caller: &qos_user_identity::DeviceCert,
        pin: GovernancePin,
    ) -> Result<(AgentControlId, Vec<GrantAction>), AgentCommitError> {
        let id = self.agent_process_scope_pinned(facts, caller, pin)?;
        let current = self.original_agent_caller(&id)?;
        if self.agent_runtime_phase(&current, &id)?.0 == AgentRuntimePhase::Unknown {
            return Err(rejected("uncertain agent has no current grant authority"));
        }
        let AgentRuntimeFact::Admitted { intent, .. } = &self.admission_fact(&id)?.change.fact
        else {
            unreachable!()
        };
        Ok((id, intent.grants.clone()))
    }
    pub(crate) fn authorize_agent_task_action(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
        app: &str,
        record: &str,
        resource: &str,
        action: &str,
    ) -> Result<(), AgentCommitError> {
        let result = self.authorize_agent_task_scope(dispatch, app, record, resource, action);
        let refused = result.as_ref().err().map(ToString::to_string);
        scope_audit::persist(
            &self.root,
            dispatch.invocation(),
            app,
            action,
            refused.as_deref(),
            self.policy_now_ns().map_or(0, |ns| ns / 1_000_000),
        );
        scope_audit::record(dispatch.invocation(), app, action, refused);
        result
    }
    /// The host Store's half of the shared per-step authorization: task state
    /// (accepted, not finished, not uncertain, not exited or revoked), the
    /// canonical route record, retained scope coverage (v1 tuples as exact
    /// degenerate scopes, v2 scopes with their frozen schema pin), and the
    /// step and wall-clock budgets. Governance pinning and the owner check run
    /// in `validate_dispatch_witness`. The broker's half (`scoped::authorize_published`)
    /// adds the live descriptor: class, confirmation, agent ∧ owner policy.
    fn authorize_agent_task_scope(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
        app: &str,
        record: &str,
        resource: &str,
        action: &str,
    ) -> Result<(), AgentCommitError> {
        let began = std::time::Instant::now();
        let (caller, _) = self.validate_dispatch_witness(dispatch)?;
        let witness_us = qos_action_broker::agent_timing::us(began);
        let stage = std::time::Instant::now();
        let id = AgentControlId::new(dispatch.invocation())?;
        let ledger = self.runtime_ledger()?;
        let fact = |kind: &str| {
            ledger
                .facts
                .contains_key(&format!("{}:{kind}", id.invocation()))
        };
        if !self.agent_runtime_phase(&caller, &id)?.1
            || fact("task_finished")
            || fact("task_unknown")
            || fact("process_exited")
        {
            return Err(rejected("agent task is not currently accepted for actions"));
        }
        let observe_us = qos_action_broker::agent_timing::us(stage);
        let stage = std::time::Instant::now();
        let _timing = TimingOnDrop(move || {
            qos_action_broker::agent_timing::slow(
                "authorize_scope",
                Some(dispatch.invocation()),
                began,
                &[
                    ("witness_us", witness_us),
                    ("observe_us", observe_us),
                    ("scope_us", qos_action_broker::agent_timing::us(stage)),
                ],
            )
        });
        let AgentRuntimeFact::Admitted { intent, .. } = &self.admission_fact(&id)?.change.fact
        else {
            unreachable!()
        };
        let canonical = qos_agent_control_protocol::canonical_record(record)
            .map_err(|_| rejected("reserved or invalid route record"))?;
        let registry = qos_action_broker::agent_control::scoped::delegation_registry();
        // Frozen scopes, plus the records this task opened itself with a
        // covered open action (from this Store's own desktop receipts).
        let scopes = qos_agent_control_protocol::scopes_with_opened_records(
            &qos_agent_control_protocol::scopes_from_grants(&intent.grants)
                .map_err(|_| rejected("invalid retained agent grants"))?,
            &self.task_opened_records(&dispatch.envelope().task_id),
            registry,
            &qos_action_broker::agent_control::scoped::schema_digest,
        );
        let classes = registry.classes_of_action(app, action);
        let live_digest = qos_action_broker::agent_control::scoped::schema_digest(app);
        let covering: Vec<_> = scopes
            .iter()
            .filter(|scope| {
                let target = |class| qos_agent_control_protocol::ScopeTarget {
                    app_id: app,
                    record_id: &canonical,
                    resource,
                    action,
                    class,
                    // The broker checks the contract's verb precisely.
                    required: qos_agent_control_protocol::GrantVerb::Read,
                };
                let covered = if scope.is_degenerate() {
                    scope.covers(&target(None))
                } else {
                    classes
                        .iter()
                        .any(|class| scope.covers(&target(Some(*class))))
                };
                covered
                    && scope.package.as_ref().is_none_or(|pin| {
                        live_digest.as_deref() == Some(pin.schema_digest.as_str())
                            && registry.package_for_app(app) == Some(pin.package_id.as_str())
                    })
            })
            .collect();
        if covering.is_empty() {
            return Err(rejected(
                "agent action is outside its immutable task grants",
            ));
        }
        // v2 budgets (v1 tuples keep today's unbudgeted behaviour, T20).
        if let Some(budgets) = covering
            .iter()
            .filter(|scope| !scope.is_degenerate())
            .map(|scope| scope.budgets())
            .reduce(qos_agent_control_protocol::ScopeBudgets::tightest)
        {
            let steps = ledger
                .facts
                .values()
                .filter(|r| {
                    r.change.invocation == id.invocation().to_string()
                        && matches!(r.change.fact, AgentRuntimeFact::TaskStepBound { .. })
                })
                .count();
            let accepted_ms = ledger
                .facts
                .get(&format!("{}:completed", id.invocation()))
                .and_then(|r| timestamp(&r.change.fact))
                .ok_or_else(|| rejected("agent task acceptance time missing"))?;
            let now_ms = self.policy_now_ns().map_err(storage_error)? / 1_000_000;
            qos_agent_control_protocol::check_step_budget(
                &budgets,
                u32::try_from(steps).unwrap_or(u32::MAX),
                now_ms.saturating_sub(accepted_ms),
            )
            .map_err(|refusal| rejected(&refusal.to_string()))?;
        }
        Ok(())
    }
    /// Invocation-rate budget of one task (MUST-9): one token per new step,
    /// burst and sustained from the task's v2 scopes. Called once when a new
    /// step binds; v1 tuples are unbudgeted. Process-local and never a grant.
    pub(crate) fn admit_agent_step_rate(
        &self,
        dispatch: &VerifiedEnrolledDispatch,
    ) -> Result<(), AgentCommitError> {
        let id = AgentControlId::new(dispatch.invocation())?;
        let AgentRuntimeFact::Admitted { intent, .. } = &self.admission_fact(&id)?.change.fact
        else {
            unreachable!()
        };
        let Some(budgets) = qos_agent_control_protocol::scopes_from_grants(&intent.grants)
            .map_err(|_| rejected("invalid retained agent grants"))?
            .iter()
            .filter(|scope| !scope.is_degenerate())
            .map(|scope| scope.budgets())
            .reduce(qos_agent_control_protocol::ScopeBudgets::tightest)
        else {
            return Ok(());
        };
        let now_ms = self.policy_now_ns().map_err(storage_error)? / 1_000_000;
        scope_audit::admit_rate(dispatch.invocation(), &budgets, now_ms)
            .map_err(|refusal| rejected(&refusal.to_string()))
    }
    /// The v2 budgets of a task's covering scopes (`None` for v1 tuples).
    fn agent_task_budgets(
        &self,
        id: &AgentControlId,
    ) -> Option<(qos_agent_control_protocol::ScopeBudgets, Vec<GrantAction>)> {
        let AgentRuntimeFact::Admitted { intent, .. } = &self.admission_fact(id).ok()?.change.fact
        else {
            return None;
        };
        let budgets = qos_agent_control_protocol::scopes_from_grants(&intent.grants)
            .ok()?
            .iter()
            .filter(|scope| !scope.is_degenerate())
            .map(|scope| scope.budgets())
            .reduce(qos_agent_control_protocol::ScopeBudgets::tightest)?;
        Some((budgets, intent.grants.clone()))
    }
    /// Visible text bytes of every Writer record a task's scopes name, from
    /// the committed Store documents.
    fn covered_writer_text_bytes(&self, grants: &[GrantAction]) -> Option<u64> {
        fn bytes(blocks: &[qdoc::snapshot::BlockView]) -> u64 {
            blocks
                .iter()
                .map(|b| b.text.len() as u64 + bytes(&b.children))
                .sum()
        }
        let peer = self.peer().ok()?;
        let scopes = qos_agent_control_protocol::scopes_from_grants(grants).ok()?;
        let mut total = 0;
        for scope in scopes
            .iter()
            .filter(|s| s.app_id == qos_agent_control_protocol::WRITER_APP_ID)
        {
            for record in scope.records() {
                let record = qos_agent_control_protocol::route_record(record).ok()?;
                let Some(document) = peer.document(&scope.app_id, record) else {
                    continue;
                };
                let doc = ui::automerge::AutoCommit::load(&document).ok()?;
                let Ok(containers) = qdoc::model::open(&doc, &ui::automerge::ROOT) else {
                    continue;
                };
                total += bytes(&qdoc::model::materialize_region(
                    &doc,
                    &containers,
                    qdoc::model::Region::Body,
                ));
            }
        }
        Some(total)
    }
    /// Remember the covered Writer text size before a new step executes.
    pub(crate) fn note_agent_step_start(&self, id: &AgentControlId, step: u32) {
        if let Some((_, grants)) = self.agent_task_budgets(id)
            && let Some(bytes) = self.covered_writer_text_bytes(&grants)
        {
            scope_audit::note_step_start(id.invocation(), step, bytes);
        }
    }
    /// Measure what the committed step removed and stop the task when the
    /// per-step or per-task content-removal budget is exceeded (MUST-8). A
    /// human editing the same records during the step window counts against
    /// the agent: the check errs toward stopping.
    pub(crate) fn enforce_agent_step_removal(&mut self, id: &AgentControlId, step: u32) {
        let Some((budgets, grants)) = self.agent_task_budgets(id) else {
            return;
        };
        let Some(after) = self.covered_writer_text_bytes(&grants) else {
            return;
        };
        let Some(verdict) = scope_audit::finish_step(id.invocation(), step, after, &budgets) else {
            return;
        };
        if let Err(refusal) = verdict {
            scope_audit::persist(
                &self.root,
                id.invocation(),
                qos_agent_control_protocol::WRITER_APP_ID,
                "content-removal",
                Some(&refusal.to_string()),
                self.policy_now_ns().map_or(0, |ns| ns / 1_000_000),
            );
            scope_audit::record(
                id.invocation(),
                qos_agent_control_protocol::WRITER_APP_ID,
                "content-removal",
                Some(refusal.to_string()),
            );
            let _ = self.mark_agent_task_unknown(
                id,
                AgentError {
                    code: AgentErrorCode::PermissionDenied,
                    message: format!("Stopped: {refusal}"),
                },
            );
        }
    }
    /// The owner's durable audit of one of its agent tasks (MUST-18): every
    /// step decision with its refusal reason. Only the task's original
    /// owner caller may read it.
    #[allow(
        dead_code,
        reason = "owner read route in the agents panel lane (integration diff)"
    )]
    pub(crate) fn agent_task_audit(
        &self,
        caller: &qos_user_identity::DeviceCert,
        id: &AgentControlId,
    ) -> Result<Vec<serde_json::Value>, AgentCommitError> {
        let owner = self.original_agent_caller(id)?;
        if owner.certificate().user_public_key != caller.user_public_key {
            return Err(rejected("agent audit belongs to another owner"));
        }
        Ok(scope_audit::read_persisted(&self.root, id.invocation()))
    }
    /// Owner revocation of one running task (MUST-10): commits a terminal
    /// task fact, so the next step's Store check refuses. The adapter then
    /// closes the enrolled connection; the one child already dispatched may
    /// still complete.
    #[allow(
        dead_code,
        reason = "RuntimeOwner::revoke_agent (lane S integration diff) calls it"
    )]
    pub(crate) fn revoke_agent_task(
        &mut self,
        caller: &qos_user_identity::DeviceCert,
        id: &AgentControlId,
    ) -> Result<OwnerCommitReceipt, AgentCommitError> {
        let owner = self.current_agent_effect(id)?;
        if owner.certificate().user_public_key != caller.user_public_key {
            return Err(rejected("only the task's owner may revoke it"));
        }
        self.mark_agent_task_unknown(
            id,
            AgentError {
                code: AgentErrorCode::PermissionDenied,
                message: "Revoked by the workspace owner".into(),
            },
        )
    }
    pub(crate) fn control_for_launch(
        &self,
        binding: &ManagedBinding,
    ) -> Result<AgentControlId, AgentCommitError> {
        let retained = self.runtime_ledger()?.facts.values().find(|r| matches!(&r.change.fact, AgentRuntimeFact::LaunchIntended { binding: candidate, .. } if binding_to_live(candidate) == *binding))
            .ok_or_else(|| rejected("exact launch intent missing"))?;
        control_id(&retained.change.invocation)
    }
    pub(crate) fn validate_agent_progress(
        &self,
        update: &VerifiedTaskUpdate,
    ) -> Result<(), AgentCommitError> {
        let live = update.revalidate().map_err(AgentCommitError::Rejected)?;
        let id = AgentControlId::new(update.invocation())?;
        self.validate_enrolled(&id, update.caller(), &live, update.canonical_envelope())?;
        let completed = self
            .runtime_ledger()?
            .facts
            .get(&format!("{}:completed", id.invocation()))
            .ok_or_else(|| rejected("progress before committed acceptance"))?;
        let AgentRuntimeFact::Completed { acceptance, .. } = &completed.change.fact else {
            unreachable!()
        };
        if acceptance.acceptance_sequence != update.acceptance().acceptance_sequence
            || update.sequence() == 0
            || self
                .runtime_ledger()?
                .facts
                .contains_key(&format!("{}:task_finished", id.invocation()))
        {
            return Err(rejected(
                "progress after terminal task or changed acceptance",
            ));
        }
        Ok(())
    }
    pub(crate) fn fail_agent_before_effect(
        &mut self,
        id: &AgentControlId,
        error: AgentError,
    ) -> Result<OwnerCommitReceipt, AgentCommitError> {
        let governance = self.admission_fact(id)?.change.expected_governance.clone();
        self.append_effect_fact(
            id,
            &governance,
            AgentRuntimeFact::Failed {
                error,
                timestamp_ms: self.policy_now_ns().map_err(storage_error)? / 1_000_000,
            },
        )
        .map(FactCommit::receipt)
    }
    pub(crate) fn mark_agent_unknown(
        &mut self,
        id: &AgentControlId,
        error: AgentError,
    ) -> Result<OwnerCommitReceipt, AgentCommitError> {
        let governance = self.admission_fact(id)?.change.expected_governance.clone();
        self.append_effect_fact(
            id,
            &governance,
            AgentRuntimeFact::Unknown {
                error,
                timestamp_ms: self.policy_now_ns().map_err(storage_error)? / 1_000_000,
            },
        )
        .map(FactCommit::receipt)
    }
    pub(crate) fn mark_agent_task_unknown(
        &mut self,
        id: &AgentControlId,
        error: AgentError,
    ) -> Result<OwnerCommitReceipt, AgentCommitError> {
        let governance = self.admission_fact(id)?.change.expected_governance.clone();
        self.append_effect_fact(
            id,
            &governance,
            AgentRuntimeFact::TaskUnknown {
                error,
                timestamp_ms: self.policy_now_ns().map_err(storage_error)? / 1_000_000,
            },
        )
        .map(FactCommit::receipt)
    }
    pub(crate) fn record_agent_process_exit(
        &mut self,
        id: &AgentControlId,
        exit: &VerifiedManagedExit,
    ) -> Result<OwnerCommitReceipt, AgentCommitError> {
        use std::os::unix::process::ExitStatusExt;
        let governance = self.admission_fact(id)?.change.expected_governance.clone();
        let launch_fact = format!("{}:launch_intended", id.invocation());
        let status = exit.status();
        self.append_effect_fact(
            id,
            &governance,
            AgentRuntimeFact::ProcessExited {
                launch_fact,
                exit: SupervisorExitV2 {
                    runtime: runtime_to_data(exit.runtime_facts()),
                    binding: binding_to_data(exit.binding()),
                    supervisor: ProcessIdentityV2 {
                        pid: exit.supervisor().pid,
                        start: exit.supervisor().start,
                    },
                    exit_code: status.code(),
                    signal: status.signal(),
                },
                timestamp_ms: self.policy_now_ns().map_err(storage_error)? / 1_000_000,
            },
        )
        .map(FactCommit::receipt)
    }
}
fn control_id(value: &str) -> Result<AgentControlId, AgentCommitError> {
    AgentControlId::new(uuid::Uuid::parse_str(value).map_err(|_| rejected("invalid control UUID"))?)
}
fn timestamp(fact: &AgentRuntimeFact) -> Option<u64> {
    let mut clone = fact.clone();
    timestamp_mut(&mut clone).copied()
}
fn timestamp_mut(fact: &mut AgentRuntimeFact) -> Option<&mut u64> {
    match fact {
        AgentRuntimeFact::Admitted { timestamp_ms, .. }
        | AgentRuntimeFact::Completed { timestamp_ms, .. }
        | AgentRuntimeFact::Failed { timestamp_ms, .. }
        | AgentRuntimeFact::Unknown { timestamp_ms, .. }
        | AgentRuntimeFact::TaskUnknown { timestamp_ms, .. }
        | AgentRuntimeFact::TaskFinished { timestamp_ms, .. }
        | AgentRuntimeFact::ProcessExited { timestamp_ms, .. } => Some(timestamp_ms),
        _ => None,
    }
}
fn binding_to_live(b: &ManagedBindingFactsV2) -> ManagedBinding {
    ManagedBinding {
        room: b.room.clone(),
        app: b.app.clone(),
        agent: b.agent.clone(),
        task: b.task.clone(),
        run: b.run.clone(),
        launch: b.launch.clone(),
        generation: b.generation,
    }
}
fn binding_to_data(b: &ManagedBinding) -> ManagedBindingFactsV2 {
    ManagedBindingFactsV2 {
        room: b.room.clone(),
        app: b.app.clone(),
        agent: b.agent.clone(),
        task: b.task.clone(),
        run: b.run.clone(),
        launch: b.launch.clone(),
        generation: b.generation,
    }
}
fn runtime_to_data(r: &ManagedRuntimeFacts) -> ManagedRuntimeFactsV2 {
    ManagedRuntimeFactsV2 {
        runtime_epoch: r.runtime_epoch.clone(),
        package_id: r.package_id.clone(),
        package_hash: r.package_hash.clone(),
        executable_hash: r.executable_hash.clone(),
        runtime_hash: r.runtime_hash.clone(),
    }
}
fn process_to_data(p: &ChannelProcessFacts) -> ChannelProcessFactsV2 {
    ChannelProcessFactsV2 {
        binding: binding_to_data(&p.binding),
        runtime: runtime_to_data(&p.runtime),
        process: ProcessIdentityV2 {
            pid: p.process.pid,
            start: p.process.start,
        },
        supervisor: ProcessIdentityV2 {
            pid: p.supervisor.pid,
            start: p.supervisor.start,
        },
        channel_id: p.channel_id.clone(),
    }
}
mod scope_audit;
mod steps;
#[cfg(test)]
mod tests;
pub(crate) use steps::CommittedStep;
#[cfg(test)]
pub(crate) mod enrolled_fixture;

mod launch;
pub(crate) use launch::{FailedLaunchAppend, RuntimeLaunchClaim};

#[cfg(test)]
pub(crate) use launch::LaunchCandidateTestClaim;

/// Runs its closure when dropped (content-free timing of early-return paths).
struct TimingOnDrop<F: FnMut()>(F);
impl<F: FnMut()> Drop for TimingOnDrop<F> {
    fn drop(&mut self) {
        (self.0)()
    }
}

/// Which admission's governance revision an effect check pins.
#[derive(Clone, Copy)]
enum GovernancePin {
    /// The checked control's own admission (every task step).
    Own,
    /// The latest admitted control of the checked Spawn's process.
    LatestControl,
    /// None here: a new control being authorized under current governance,
    /// or a Message's parent whose own pin the Message already checked.
    NewControl,
}
