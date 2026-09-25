use super::*;
use crate::agent_runtime_owner::tests::{enrolled, intent};
use crate::https_owner::{CloudStoreSource, Current};
use qos_action_broker::peer_actions::OwnerPolicyClock;

#[test]
fn agent_prelaunch_recovery_requires_installed_source_and_real_runtime() {
    let (dir, mut store, device) = enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    let input = intent(&store, &caller);
    let invocation = uuid::Uuid::from_u128(909);
    store
        .admit_agent_candidate(&caller, invocation, &input)
        .unwrap();
    let clock = OwnerPolicyClock::new().unwrap();
    store
        .activate_delivery_storage(clock.now_ns().unwrap())
        .unwrap();
    let before = blake3::hash(&serde_json::to_vec(&store.state).unwrap());
    let source = CloudStoreSource::bind_with_clock(&store, clock.clone()).unwrap();
    drop(store);
    let Current::Current(time) = source.with_current(|turn| turn.policy_time().unwrap()) else {
        panic!("source");
    };
    let legacy = RuntimeOwner::open(dir.path(), "private-runtime").unwrap();
    assert!(
        legacy
            .recover_before_launch(invocation)
            .unwrap_err()
            .contains("installed")
    );
    assert!(legacy.attempt(invocation).is_err());
    let owner =
        RuntimeOwner::open_with_source(dir.path(), "private-runtime", clock, time, source.clone())
            .unwrap();
    assert_eq!(owner.pending_spawns(None, 8).unwrap(), vec![invocation]);
    assert!(
        owner
            .recover_before_launch(invocation)
            .unwrap_err()
            .contains("runtime unavailable")
    );
    assert!(owner.attempt(invocation).is_err());
    assert_eq!(
        blake3::hash(&serde_json::to_vec(&owner.store().unwrap().state).unwrap()),
        before
    );
    source.retire();
    assert!(owner.pending_spawns(None, 8).is_err());
    assert!(owner.recover_before_launch(invocation).is_err());
}

#[test]
fn agent_prelaunch_recovery_scan_is_bounded_and_preserves_originals() {
    let (dir, mut store, device) = enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    let input = intent(&store, &caller);
    let ids: Vec<_> = (1..=4).map(uuid::Uuid::from_u128).collect();
    for id in &ids {
        store.admit_agent_candidate(&caller, *id, &input).unwrap();
    }
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    assert_eq!(store.pending_spawn_ids(None, 2).unwrap(), ids[..2]);
    assert_eq!(store.pending_spawn_ids(Some(ids[1]), 2).unwrap(), ids[2..]);
    assert!(store.pending_spawn_ids(Some(ids[3]), 2).unwrap().is_empty());
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    store
        .mark_agent_unknown(
            &AgentControlId::new(ids[0]).unwrap(),
            AgentError {
                code: AgentErrorCode::OutcomeUncertain,
                message: "explicit retained unknown".into(),
            },
        )
        .unwrap();
    assert_eq!(store.pending_spawn_ids(None, 8).unwrap(), ids[1..]);
}

#[test]
fn agent_prelaunch_recovery_uncertain_admission_requires_actual_reopen() {
    let (dir, mut store, device) = enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    let input = intent(&store, &caller);
    let invocation = uuid::Uuid::from_u128(910);
    let result = crate::storage::test_support::with_save_failure(
        dir.path(),
        atomic_file::test_support::Checkpoint::BeforeCommit,
        || store.admit_agent_candidate(&caller, invocation, &input),
    );
    assert!(matches!(result, Err(AgentCommitError::Uncertain { .. })));
    assert!(store.pending_spawn_ids(None, 8).is_err());
    drop(store);
    let store = Store::open(dir.path(), "private-runtime").unwrap();
    assert_eq!(store.pending_spawn_ids(None, 8).unwrap(), vec![invocation]);
    let id = AgentControlId::new(invocation).unwrap();
    assert!(store.sole_admitted_spawn(&id).unwrap().is_some());
}

#[test]
fn agent_prelaunch_recovery_complete_fact_set_is_not_recoverable() {
    let (_dir, mut store, device) = enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    // Literal validated retained history only; this is never a live process fixture.
    let state = crate::agent_runtime_owner::tests::retained_completed_state(&store, &caller);
    let Some(PrivateAgentState::V2(ledger)) = &state.private_agent_ledger else {
        panic!("v2");
    };
    for receipt in ledger.facts.values() {
        let id = &receipt.change.invocation;
        let expected = ledger
            .facts
            .values()
            .filter(|r| &r.change.invocation == id)
            .count();
        if expected > 1 {
            assert!(sole(ledger, id).is_none());
        }
    }
    store.state = state;
    assert!(
        store
            .pending_spawn_ids(None, MAX_PRIVATE_AGENT_ADMISSIONS)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn agent_prelaunch_recovery_classifier_refuses_message_admission_data() {
    let (_dir, mut store, device) = enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    let input = intent(&store, &caller);
    let invocation = uuid::Uuid::from_u128(911);
    store
        .admit_agent_candidate(&caller, invocation, &input)
        .unwrap();
    let mut ledger = store.runtime_ledger().unwrap().clone();
    let receipt = ledger.facts.values_mut().next().unwrap();
    let AgentRuntimeFact::Admitted {
        intent,
        ids,
        parent_invocation,
        ..
    } = &mut receipt.change.fact
    else {
        panic!("admitted");
    };
    // Negative classifier input, not a valid signed history or positive custody fixture.
    *parent_invocation = Some(uuid::Uuid::from_u128(912).to_string());
    intent.route.action = "message".into();
    intent.request = String::from_utf8(
        qos_agent_control_protocol::canonical_request(&AgentRequest::Message {
            version: 1,
            agent_id: ids.agent_id.clone(),
            text: "retained message".into(),
        })
        .unwrap(),
    )
    .unwrap();
    assert!(sole(&ledger, &invocation.to_string()).is_none());
}
