//! Launch-only failed-append provenance. No durable data can construct a retry.
use super::*;
use crate::agent_runtime_adapter::LiveAttempt;
use crate::agent_runtime_adapter::launch_retry::QuiescentLaunchRetry;
use std::sync::{Arc, Weak};

/// Only an actual reserved launch commit returning Err may construct this value.
pub(crate) struct FailedLaunchAppend {
    origin: Option<Weak<LiveAttempt>>,
    admission: AgentRuntimeFactReceipt,
    policy_time: Option<qos_action_broker::peer_actions::PolicyTime>,
    attempted: CommitBinding,
    error: anyhow::Error,
}
impl std::fmt::Debug for FailedLaunchAppend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FailedLaunchAppend").finish_non_exhaustive()
    }
}
impl FailedLaunchAppend {
    pub(crate) fn belongs_to(&self, attempt: &Arc<LiveAttempt>) -> bool {
        self.origin
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|original| Arc::ptr_eq(&original, attempt))
    }
    pub(crate) fn error(&self) -> &anyhow::Error {
        &self.error
    }
    pub(super) fn into_error(self) -> AgentCommitError {
        storage_error(self.error)
    }
    pub(crate) fn validate(
        &self,
        custody: &AgentAttemptCustody,
        store: &Store,
    ) -> Result<(), AgentCommitError> {
        if self.admission != custody.admission || !custody.stages[0].load(Ordering::Acquire) {
            return Err(rejected("failed launch differs from original admission"));
        }
        require_policy_time(store, self.policy_time.as_ref())?;
        custody.validate_time(store)?;
        let CommitTarget::AgentRuntime { mutation } = &self.attempted.target else {
            return Err(rejected("failed launch target differs"));
        };
        if self.attempted.room != self.admission.binding.room
            || store.admission_fact(&custody.id())? != &self.admission
            || mutation != &format!("{}:launch_intended", custody.id().invocation())
        {
            return Err(rejected("failed launch invocation differs"));
        }
        Ok(())
    }
}

pub(crate) enum RuntimeLaunchClaim {
    Fresh {
        permit: AgentLaunchPermit,
        receipt: OwnerCommitReceipt,
    },
    Existing(AgentRuntimeObservation),
    Refused {
        error: AgentCommitError,
        retry: Option<QuiescentLaunchRetry>,
    },
    Failed(FailedLaunchAppend),
    PostCommit(AgentCommitError),
}
pub(super) enum LaunchCandidateClaim {
    Committed(FactCommit, ManagedBinding),
    Refused {
        error: AgentCommitError,
        retry: Option<QuiescentLaunchRetry>,
    },
    Failed(FailedLaunchAppend),
    PostCommit(AgentCommitError),
}
#[cfg(test)]
pub(crate) enum LaunchCandidateTestClaim {
    Committed(bool, ManagedBinding),
    Refused {
        error: AgentCommitError,
        retry: Option<QuiescentLaunchRetry>,
    },
    Failed(FailedLaunchAppend),
    PostCommit(AgentCommitError),
}
impl Store {
    pub(crate) fn claim_agent_launch_for_runtime(
        &mut self,
        caller: &VerifiedAgentCaller,
        attempt: &Arc<LiveAttempt>,
        runtime: &VerifiedManagedRuntime,
        retry: Option<QuiescentLaunchRetry>,
    ) -> RuntimeLaunchClaim {
        let custody = attempt.launch_custody();
        let preflight = (|| {
            custody.validate_time(self)?;
            let current = self
                .refresh_agent_caller(caller)
                .map_err(AgentCommitError::Rejected)?;
            if current.certificate() != self.original_agent_caller(&custody.id())?.certificate() {
                return Err(rejected("launch caller differs"));
            }
            if let Some(retry) = retry.as_ref() {
                retry.validate(custody, self)?;
            }
            let observation = self.observe_agent_runtime(&current, &custody.id())?;
            if observation.phase != AgentRuntimePhase::Admitted
                || (retry.is_none() && custody.stages[0].load(Ordering::Acquire))
            {
                return Ok(Err(observation));
            }
            let facts = runtime.revalidate().map_err(AgentCommitError::Rejected)?;
            Ok(Ok((current, runtime_to_data(&facts))))
        })();
        let (current, facts) = match preflight {
            Err(error) => return RuntimeLaunchClaim::Refused { error, retry },
            Ok(Err(observation)) => return RuntimeLaunchClaim::Existing(observation),
            Ok(Ok(value)) => value,
        };
        match self.claim_launch_candidate_with_origin(
            custody,
            facts,
            retry,
            Some(Arc::downgrade(attempt)),
        ) {
            LaunchCandidateClaim::Committed(FactCommit::Fresh(receipt), binding) => {
                RuntimeLaunchClaim::Fresh {
                    permit: AgentLaunchPermit {
                        policy_time: custody.policy_time.clone(),
                        binding,
                        runtime: runtime.clone(),
                        receipt: receipt.clone(),
                    },
                    receipt,
                }
            }
            LaunchCandidateClaim::Committed(FactCommit::Existing(_), _) => {
                match self.observe_agent_runtime(&current, &custody.id()) {
                    Ok(observation) => RuntimeLaunchClaim::Existing(observation),
                    Err(error) => RuntimeLaunchClaim::PostCommit(error),
                }
            }
            LaunchCandidateClaim::Refused { error, retry } => {
                RuntimeLaunchClaim::Refused { error, retry }
            }
            LaunchCandidateClaim::Failed(failed) => RuntimeLaunchClaim::Failed(failed),
            LaunchCandidateClaim::PostCommit(error) => RuntimeLaunchClaim::PostCommit(error),
        }
    }
    pub(super) fn claim_launch_candidate_for_runtime(
        &mut self,
        custody: &AgentAttemptCustody,
        runtime: ManagedRuntimeFactsV2,
        retry: Option<QuiescentLaunchRetry>,
    ) -> LaunchCandidateClaim {
        self.claim_launch_candidate_with_origin(custody, runtime, retry, None)
    }
    #[cfg(test)]
    pub(crate) fn claim_launch_candidate_unqualified_test(
        &mut self,
        attempt: &Arc<LiveAttempt>,
        runtime: ManagedRuntimeFactsV2,
        retry: Option<QuiescentLaunchRetry>,
    ) -> LaunchCandidateTestClaim {
        // Actual Store/linear resource core only; no VerifiedManagedRuntime or permit.
        match self.claim_launch_candidate_with_origin(
            attempt.launch_custody(),
            runtime,
            retry,
            Some(Arc::downgrade(attempt)),
        ) {
            LaunchCandidateClaim::Committed(FactCommit::Fresh(_), binding) => {
                LaunchCandidateTestClaim::Committed(true, binding)
            }
            LaunchCandidateClaim::Committed(FactCommit::Existing(_), binding) => {
                LaunchCandidateTestClaim::Committed(false, binding)
            }
            LaunchCandidateClaim::Refused { error, retry } => {
                LaunchCandidateTestClaim::Refused { error, retry }
            }
            LaunchCandidateClaim::Failed(failed) => LaunchCandidateTestClaim::Failed(failed),
            LaunchCandidateClaim::PostCommit(error) => LaunchCandidateTestClaim::PostCommit(error),
        }
    }
    fn claim_launch_candidate_with_origin(
        &mut self,
        custody: &AgentAttemptCustody,
        runtime: ManagedRuntimeFactsV2,
        retry: Option<QuiescentLaunchRetry>,
        origin: Option<Weak<LiveAttempt>>,
    ) -> LaunchCandidateClaim {
        // All launch-specific fallible preparation runs while the original Store
        // transaction is held, before the permanent initial reservation.
        let prepared = (|| {
            custody.validate_time(self)?;
            let id = custody.id();
            let caller = self.current_agent_effect(&id)?;
            if let Some(retry) = retry.as_ref() {
                retry.validate(custody, self)?;
            }
            if let Some(receipt) = self
                .runtime_ledger()?
                .facts
                .get(&format!("{}:launch_intended", id.invocation()))
            {
                let AgentRuntimeFact::LaunchIntended { binding, .. } = &receipt.change.fact else {
                    unreachable!()
                };
                return Ok((
                    PreparedEffectFact::Existing(receipt.binding.clone()),
                    binding_to_live(binding),
                ));
            }
            if self.sole_admitted_spawn(&id)? != Some(&custody.admission) {
                return Err(rejected(
                    "launch requires the exact sole original Admitted Spawn fact",
                ));
            }
            let AgentRuntimeFact::Admitted {
                ids,
                parent_invocation: None,
                ..
            } = &custody.admission.change.fact
            else {
                return Err(rejected("only Spawn launches a process"));
            };
            let binding = ManagedBindingFactsV2 {
                room: self.state.room.clone(),
                app: "so.tilo.agent".into(),
                agent: ids.agent_id.clone(),
                task: ids.task_id.clone(),
                run: ids.run_id.clone(),
                launch: ids.launch_id.clone(),
                generation: self.next_generation().map_err(storage_error)?,
            };
            let live = binding_to_live(&binding);
            let candidate = self.prepare_effect_fact(
                &id,
                caller.governance_revision(),
                AgentRuntimeFact::LaunchIntended { binding, runtime },
            )?;
            Ok((candidate, live))
        })();
        let (candidate, live) = match prepared {
            Ok(value) => value,
            Err(error) => return LaunchCandidateClaim::Refused { error, retry },
        };
        let (state, binding) = match candidate {
            PreparedEffectFact::Existing(binding) => {
                return match self.runtime_receipt(binding) {
                    Ok(receipt) => {
                        LaunchCandidateClaim::Committed(FactCommit::Existing(receipt), live)
                    }
                    Err(error) => LaunchCandidateClaim::PostCommit(error),
                };
            }
            PreparedEffectFact::Fresh { state, binding } => (state, binding),
        };
        if retry.is_none() {
            match custody.reserve(self, 0) {
                Ok(true) => {}
                Ok(false) => {
                    return LaunchCandidateClaim::Refused {
                        error: rejected("launch stage already consumed; status only"),
                        retry,
                    };
                }
                Err(error) => return LaunchCandidateClaim::Refused { error, retry },
            }
        }
        // Consumed retry never resets the initial atomic. No further fallible
        // launch preparation occurs between this point and the actual commit.
        drop(retry);
        if let Err(error) = self.commit(state) {
            return LaunchCandidateClaim::Failed(FailedLaunchAppend {
                origin,
                admission: custody.admission.clone(),
                policy_time: custody.policy_time.clone(),
                attempted: binding,
                error,
            });
        }
        // Commit Ok is a permanent phase boundary, even if receipt materialization fails.
        match self.launch_post_commit_receipt(binding) {
            Ok(receipt) => LaunchCandidateClaim::Committed(FactCommit::Fresh(receipt), live),
            Err(error) => LaunchCandidateClaim::PostCommit(error),
        }
    }
    fn launch_post_commit_receipt(
        &self,
        binding: CommitBinding,
    ) -> Result<OwnerCommitReceipt, AgentCommitError> {
        #[cfg(test)]
        if RECEIPT_FAILURE.with(|flag| flag.replace(false)) {
            return Err(rejected(
                "injected post-commit receipt materialization failure",
            ));
        }
        self.runtime_receipt(binding)
    }
}

#[cfg(test)]
thread_local! { static RECEIPT_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(test)]
fn with_receipt_failure<T>(operation: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            RECEIPT_FAILURE.with(|flag| flag.set(self.0));
        }
    }
    let _restore = Restore(RECEIPT_FAILURE.with(|flag| flag.replace(true)));
    operation()
}
#[cfg(test)]
mod tests;
