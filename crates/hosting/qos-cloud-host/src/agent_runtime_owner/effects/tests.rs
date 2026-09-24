use super::*;
use crate::agent_runtime_owner::tests::{enrolled, intent};
fn admitted() -> (tempfile::TempDir, Store, AgentAttemptCustody) {
    let (dir, mut store, device) = enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    let input = intent(&store, &caller);
    let AgentAdmissionResult::Fresh(value) = store
        .admit_agent_candidate(&caller, uuid::Uuid::from_u128(81), &input)
        .unwrap()
    else {
        unreachable!()
    };
    (dir, store, value.into_custody())
}
#[test]
fn agent_owner_effect_uncertainty_crosses_real_barrier_and_replays_current_receipt() {
    let (dir, mut store, custody) = admitted();
    let fact = AgentRuntimeFact::Unknown {
        error: AgentError {
            code: AgentErrorCode::OutcomeUncertain,
            message: "Lost live runtime".into(),
        },
        timestamp_ms: 10,
    };
    let FactCommit::Fresh(first) = store
        .append_effect_fact(&custody.id(), "a".repeat(64).as_str(), fact.clone())
        .unwrap()
    else {
        panic!("fresh fact missing")
    };
    let generation = first.generation;
    drop(store);
    let mut store = Store::open(dir.path(), "private-runtime").unwrap();
    let FactCommit::Existing(replay) = store
        .append_effect_fact(&custody.id(), "a".repeat(64).as_str(), fact)
        .unwrap()
    else {
        panic!("replay was fresh")
    };
    assert_eq!(replay.generation, generation);
    assert_eq!(replay.binding, first.binding);
    assert_eq!(
        replay.aggregate_hash,
        *blake3::hash(&serde_json::to_vec(&store.state).unwrap()).as_bytes()
    );
}

fn runtime_data() -> ManagedRuntimeFactsV2 {
    ManagedRuntimeFactsV2 {
        runtime_epoch: uuid::Uuid::from_u128(91).to_string(),
        package_id: "so.tilo.agent".into(),
        package_hash: "a".repeat(64),
        executable_hash: "b".repeat(64),
        runtime_hash: "c".repeat(64),
    }
}
#[test]
fn agent_owner_effect_live_admission_custody_survives_normal_store_reopen() {
    let (dir, store, custody) = admitted();
    let id = custody.id();
    drop(store);
    let mut store = Store::open(dir.path(), "private-runtime").unwrap();
    // This is the private retained-data/barrier core, not a fabricated live runtime witness.
    let (FactCommit::Fresh(first), binding) = store
        .claim_launch_candidate(&custody, runtime_data())
        .unwrap()
    else {
        panic!("same live custody must cross Store transactions")
    };
    assert_eq!(
        first.binding.target,
        CommitTarget::AgentRuntime {
            mutation: format!("{}:launch_intended", id.invocation())
        }
    );
    assert_eq!(binding.generation, first.generation);
    drop(store);
    let mut store = Store::open(dir.path(), "private-runtime").unwrap();
    let (FactCommit::Existing(replay), _) = store
        .claim_launch_candidate(&custody, runtime_data())
        .unwrap()
    else {
        panic!("same custody replay minted a new claim")
    };
    assert_eq!(replay.binding, first.binding);
    assert_eq!(store.runtime_ledger().unwrap().facts.len(), 2);
    drop(custody);
    let device = ed25519_dalek::SigningKey::from_bytes(&[8; 32])
        .verifying_key()
        .to_bytes();
    let caller = store.agent_caller(device).unwrap();
    assert_eq!(
        store.observe_agent_runtime(&caller, &id).unwrap().phase,
        AgentRuntimePhase::Launching
    );
    // A persisted intent/read observation has no constructor for AgentAttemptCustody.
}
#[test]
fn agent_owner_effect_uncertain_intent_consumes_live_stage_and_never_reclaims() {
    use atomic_file::test_support::Checkpoint;
    for checkpoint in [Checkpoint::BeforePublish, Checkpoint::BeforeCommit] {
        let (dir, mut store, custody) = admitted();
        let result =
            crate::storage::test_support::with_save_failure(dir.path(), checkpoint, || {
                store.claim_launch_candidate(&custody, runtime_data())
            });
        assert!(matches!(result, Err(AgentCommitError::Uncertain { .. })));
        assert!(custody.stages[0].load(Ordering::Acquire));
        drop(store);
        let mut store = Store::open(dir.path(), "private-runtime").unwrap();
        assert!(!matches!(
            store.claim_launch_candidate(&custody, runtime_data()),
            Ok((FactCommit::Fresh(_), _))
        ));
    }
}
#[test]
fn agent_owner_effect_conflicting_terminal_and_missing_acceptance_never_commit() {
    let (dir, mut store, custody) = admitted();
    let id = custody.id();
    let error = AgentError {
        code: AgentErrorCode::OutcomeUncertain,
        message: "Original uncertainty".into(),
    };
    store.mark_agent_unknown(&id, error).unwrap();
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    assert!(
        store
            .fail_agent_before_effect(
                &id,
                AgentError {
                    code: AgentErrorCode::PermissionDenied,
                    message: "changed".into()
                }
            )
            .is_err()
    );
    assert!(
        store
            .claim_launch_candidate(&custody, runtime_data())
            .is_err()
    );
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
}

// Untrusted historical data only. Public process/acceptance APIs never accept these values.
fn remaining_history(store: &Store, custody: &AgentAttemptCustody) -> Vec<AgentRuntimeFact> {
    let AgentRuntimeFact::Admitted { ids, intent, .. } = &custody.admission.change.fact else {
        unreachable!()
    };
    let literal = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../collab/qos-peer-state/src/owner_commit/fixtures/agent-v2-completed.json"
    ));
    let mut json = literal.to_string();
    for (n, value) in [
        (1, store.state.room.as_str()),
        (2, custody.admission.change.invocation.as_str()),
        (3, ids.agent_id.as_str()),
        (4, ids.task_id.as_str()),
        (5, ids.run_id.as_str()),
        (6, ids.launch_id.as_str()),
        (7, ids.message_id.as_str()),
    ] {
        json = json.replace(&uuid::Uuid::from_u128(n).to_string(), value);
    }
    let AgentRequest::Spawn { task, .. } = intent.validate(&store.state.room).unwrap() else {
        unreachable!()
    };
    json = json.replace("Retained private task", &task);
    let ledger: PrivateAgentLedgerV2 = serde_json::from_str(&json).unwrap();
    let mut receipts = ledger.facts.into_values().collect::<Vec<_>>();
    receipts.sort_by_key(|r| r.generation);
    let launch_generation = store.state.generation + 1;
    receipts
        .into_iter()
        .filter_map(|r| {
            let mut fact = r.change.fact;
            match &mut fact {
                AgentRuntimeFact::Admitted { .. } => return None,
                AgentRuntimeFact::LaunchIntended { binding, .. } => {
                    binding.generation = launch_generation
                }
                AgentRuntimeFact::ProcessBound { process, .. } => {
                    process.binding.generation = launch_generation
                }
                AgentRuntimeFact::ProcessExited { exit, .. } => {
                    exit.binding.generation = launch_generation
                }
                _ => (),
            }
            Some(fact)
        })
        .collect()
}
#[test]
fn agent_owner_effect_unknown_process_allows_factual_callback_but_not_grants() {
    let (dir, mut store, custody) = admitted();
    let history = remaining_history(&store, &custody);
    let governance = custody.admission.change.expected_governance.clone();
    let AgentRuntimeFact::Admitted { intent, .. } = &custody.admission.change.fact else {
        unreachable!()
    };
    let caller = intent.scope.caller.clone();
    let mut process = None;
    for fact in &history {
        if matches!(fact, AgentRuntimeFact::Completed { .. }) {
            break;
        }
        if let AgentRuntimeFact::ProcessBound { process: facts, .. } = fact {
            process = Some(facts.clone());
        }
        store
            .append_effect_fact(&custody.id(), &governance, fact.clone())
            .unwrap();
    }
    let process = process.unwrap();
    store
        .mark_agent_unknown(
            &custody.id(),
            AgentError {
                code: AgentErrorCode::OutcomeUncertain,
                message: "Reply lost while exact channel remains live".into(),
            },
        )
        .unwrap();
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    // Policy/data regression only: production entry requires VerifiedManagedProcess.
    assert_eq!(
        store.agent_process_scope_data(&process, &caller).unwrap(),
        custody.id()
    );
    assert!(store.agent_process_grants_data(&process, &caller).is_err());
    let mut wrong_channel = process.clone();
    wrong_channel.channel_id = uuid::Uuid::new_v4().to_string();
    assert!(
        store
            .agent_process_scope_data(&wrong_channel, &caller)
            .is_err()
    );
    let wrong_caller = store.state.bound.as_ref().unwrap().authority.clone();
    assert!(
        store
            .agent_process_scope_data(&process, &wrong_caller)
            .is_err()
    );
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    // Existing schema/barrier core accepts the exact retained late acceptance,
    // without another launch or dispatch fact. This is not a live RPC proof.
    let completed = history
        .into_iter()
        .find(|f| matches!(f, AgentRuntimeFact::Completed { .. }))
        .unwrap();
    store
        .append_effect_fact(&custody.id(), &governance, completed)
        .unwrap();
    assert_eq!(
        store
            .runtime_ledger()
            .unwrap()
            .facts
            .values()
            .filter(|r| matches!(r.change.fact, AgentRuntimeFact::DispatchIntended { .. }))
            .count(),
        1
    );
    assert!(
        store
            .observe_agent_runtime(
                &store.agent_caller(caller.device_public_key).unwrap(),
                &custody.id()
            )
            .unwrap()
            .qualified_completed
    );
}
#[test]
fn agent_owner_effect_full_history_finishes_at_admission_capacity_without_evicting() {
    let (dir, mut store, custody) = admitted();
    let device = ed25519_dalek::SigningKey::from_bytes(&[8; 32])
        .verifying_key()
        .to_bytes();
    for n in 82..=84 {
        let caller = store.agent_caller(device).unwrap();
        let input = intent(&store, &caller);
        assert!(matches!(
            store
                .admit_agent_candidate(&caller, uuid::Uuid::from_u128(n), &input)
                .unwrap(),
            AgentAdmissionResult::Fresh(_)
        ));
    }
    let history = remaining_history(&store, &custody);
    let governance = custody.admission.change.expected_governance.clone();
    let completed = history
        .iter()
        .find(|f| matches!(f, AgentRuntimeFact::Completed { .. }))
        .unwrap()
        .clone();
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    assert!(
        store
            .append_effect_fact(&custody.id(), &governance, completed.clone())
            .is_err(),
        "completion cannot precede process/dispatch evidence"
    );
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    for fact in history {
        if matches!(fact, AgentRuntimeFact::ProcessExited { .. }) {
            let caller = store.agent_caller(device).unwrap();
            let input = intent(&store, &caller);
            assert!(matches!(
                store.admit_agent_candidate(&caller, uuid::Uuid::from_u128(85), &input),
                Err(AgentCommitError::Capacity { limit: 4 })
            ));
        }
        assert!(matches!(
            store
                .append_effect_fact(&custody.id(), &governance, fact)
                .unwrap(),
            FactCommit::Fresh(_)
        ));
    }
    let caller = store.agent_caller(device).unwrap();
    let original = store.observe_agent_runtime(&caller, &custody.id()).unwrap();
    assert!(original.qualified_completed);
    let input = intent(&store, &caller);
    assert!(matches!(
        store
            .admit_agent_candidate(&caller, uuid::Uuid::from_u128(85), &input)
            .unwrap(),
        AgentAdmissionResult::Fresh(_)
    ));
    let retained_facts = store.runtime_ledger().unwrap().facts.len();
    let FactCommit::Existing(replayed) = store
        .append_effect_fact(&custody.id(), &governance, completed)
        .unwrap()
    else {
        panic!("terminal retry appended")
    };
    assert!(replayed.generation > original.receipt.generation);
    assert_eq!(store.runtime_ledger().unwrap().facts.len(), retained_facts);
    drop(store);
    let store = Store::open(dir.path(), "private-runtime").unwrap();
    assert!(
        store
            .observe_agent_runtime(&store.agent_caller(device).unwrap(), &custody.id())
            .unwrap()
            .qualified_completed
    );
    assert_eq!(store.runtime_ledger().unwrap().facts.len(), retained_facts);
}

#[test]
fn cloud_policy_clock_effect_custody_cannot_escape_into_unbound_store() {
    let (dir, mut store, device) = enrolled();
    let clock = qos_action_broker::peer_actions::OwnerPolicyClock::new().unwrap();
    let source = clock.new_origin();
    store.bind_policy_time(source.capture().unwrap()).unwrap();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    let input = intent(&store, &caller);
    let AgentAdmissionResult::Fresh(value) = store
        .admit_agent_candidate(&caller, uuid::Uuid::from_u128(9181), &input)
        .unwrap()
    else {
        panic!("fresh admission");
    };
    let custody = value.into_custody();
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    drop(store);
    source.invalidate();
    let mut reopened = Store::open(dir.path(), "private-runtime").unwrap();
    assert!(
        reopened
            .claim_launch_candidate(&custody, runtime_data())
            .is_err(),
        "required effect custody escaped into an ordinary reopened Store"
    );
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    assert!(!custody.stages[0].load(Ordering::Acquire));
}

#[test]
fn agent_prelaunch_recovery_candidate_refuses_any_later_fact() {
    let (_dir, mut store, custody) = admitted();
    store
        .mark_agent_unknown(
            &custody.id(),
            AgentError {
                code: AgentErrorCode::OutcomeUncertain,
                message: "prior uncertain effect".into(),
            },
        )
        .unwrap();
    let before = serde_json::to_vec(&store.state).unwrap();
    assert!(
        store
            .claim_launch_candidate(&custody, runtime_data())
            .is_err(),
        "a retained Unknown must never become a new launch"
    );
    assert_eq!(serde_json::to_vec(&store.state).unwrap(), before);
}

#[test]
fn agent_prelaunch_recovery_launch_intent_restart_stays_uncertain() {
    let (dir, mut store, custody) = admitted();
    let id = custody.id();
    store
        .claim_launch_candidate(&custody, runtime_data())
        .unwrap();
    assert!(store.sole_admitted_spawn(&id).unwrap().is_none());
    drop(store);
    let owner =
        crate::agent_runtime_adapter::RuntimeOwner::open(dir.path(), "private-runtime").unwrap();
    owner.retain_restart_uncertainty().unwrap();
    let store = owner.store().unwrap();
    assert!(
        store
            .runtime_ledger()
            .unwrap()
            .facts
            .contains_key(&format!("{}:unknown", id.invocation()))
    );
    assert!(store.pending_spawn_ids(None, 8).unwrap().is_empty());
    assert!(owner.attempt(id.invocation()).is_err());
}
