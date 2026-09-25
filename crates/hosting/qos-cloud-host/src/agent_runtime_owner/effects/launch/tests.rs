//! Real Store barrier core. Runtime labels here are inert, not process authority.
use super::*;
use crate::agent_runtime_owner::tests::{enrolled, intent};

#[test]
fn agent_launch_stage_failure_requires_actual_commit_error() {
    let (dir, mut store, device) = enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    let input = intent(&store, &caller);
    let AgentAdmissionResult::Fresh(admission) = store
        .admit_agent_candidate(&caller, uuid::Uuid::from_u128(913), &input)
        .unwrap()
    else {
        panic!("fresh real admission required");
    };
    let custody = admission.into_custody();
    let runtime = ManagedRuntimeFactsV2 {
        runtime_epoch: uuid::Uuid::from_u128(91).to_string(),
        package_id: "so.tilo.agent".into(),
        package_hash: "a".repeat(64),
        executable_hash: "b".repeat(64),
        runtime_hash: "c".repeat(64),
    };
    let result = crate::storage::test_support::with_save_failure(
        dir.path(),
        atomic_file::test_support::Checkpoint::PublishSubmitted,
        || store.claim_launch_candidate_for_runtime(&custody, runtime, None),
    );
    let LaunchCandidateClaim::Failed(failed) = result else {
        panic!("actual commit Err must retain typed failed-call custody");
    };
    assert!(
        failed
            .error()
            .downcast_ref::<crate::StoreCommitFailure>()
            .is_some()
    );
    assert!(custody.stages[0].load(Ordering::Acquire));
    assert_eq!(
        store.failed_commit.get().unwrap().outcome,
        crate::StoreSaveOutcome::PublicationUnknown
    );
}

#[test]
fn agent_launch_stage_plain_consumed_custody_stays_refused_after_source_reconciliation() {
    use crate::agent_runtime_adapter::RuntimeOwner;
    use crate::https_owner::{CloudStoreSource, Current};
    use qos_action_broker::peer_actions::OwnerPolicyClock;
    let (dir, mut store, device) = enrolled();
    let clock = OwnerPolicyClock::new().unwrap();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    store
        .activate_delivery_storage(clock.now_ns().unwrap())
        .unwrap();
    let source = CloudStoreSource::bind_with_clock(&store, clock.clone()).unwrap();
    drop(store);
    let Current::Current(time) = source.with_current(|turn| turn.policy_time().unwrap()) else {
        panic!("actual Source required");
    };
    let owner =
        RuntimeOwner::open_with_source(dir.path(), "private-runtime", clock, time.clone(), source)
            .unwrap();
    let mut store = owner.store_for(&time).unwrap();
    let caller = store.agent_caller(device).unwrap();
    let input = intent(&store, &caller);
    let AgentAdmissionResult::Fresh(admitted) = store
        .admit_agent_candidate(&caller, uuid::Uuid::from_u128(914), &input)
        .unwrap()
    else {
        panic!("fresh admission required");
    };
    let custody = admitted.into_custody();
    let original = custody.admission.clone();
    let runtime = || ManagedRuntimeFactsV2 {
        runtime_epoch: uuid::Uuid::from_u128(91).to_string(),
        package_id: "so.tilo.agent".into(),
        package_hash: "a".repeat(64),
        executable_hash: "b".repeat(64),
        runtime_hash: "c".repeat(64),
    };
    let result = crate::storage::test_support::with_save_failure(
        dir.path(),
        atomic_file::test_support::Checkpoint::PublishSubmitted,
        || store.claim_launch_candidate(&custody, runtime()),
    );
    assert!(matches!(result, Err(AgentCommitError::Uncertain { .. })));
    assert!(custody.stages[0].load(Ordering::Acquire));
    assert!(store.sole_admitted_spawn(&custody.id()).is_err());
    drop(store);
    let mut reconciled = owner.store_for(&time).unwrap();
    assert_eq!(
        reconciled.sole_admitted_spawn(&custody.id()).unwrap(),
        Some(&original)
    );
    assert!(
        !matches!(
            reconciled.claim_launch_candidate(&custody, runtime()),
            Ok((FactCommit::Fresh(_), _))
        ),
        "plain custody cannot replace joined failed-call continuation"
    );
    assert!(custody.stages[0].load(Ordering::Acquire));
}

#[test]
fn agent_launch_stage_preflight_rejection_does_not_consume_initial_stage() {
    let (dir, mut store, device) = enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    let input = intent(&store, &caller);
    let AgentAdmissionResult::Fresh(admitted) = store
        .admit_agent_candidate(&caller, uuid::Uuid::from_u128(915), &input)
        .unwrap()
    else {
        panic!("fresh real admission required");
    };
    let custody = admitted.into_custody();
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    let invalid_runtime = ManagedRuntimeFactsV2 {
        runtime_epoch: uuid::Uuid::from_u128(91).to_string(),
        package_id: "so.tilo.agent".into(),
        package_hash: "invalid".into(),
        executable_hash: "b".repeat(64),
        runtime_hash: "c".repeat(64),
    };
    assert!(
        store
            .claim_launch_candidate(&custody, invalid_runtime)
            .is_err()
    );
    assert!(
        !custody.stages[0].load(Ordering::Acquire),
        "candidate validation must precede the permanent stage reservation"
    );
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    assert!(store.failed_commit.get().is_none());
}

#[test]
fn agent_launch_retry_post_commit_receipt_failure_has_no_failed_append() {
    let (dir, mut store, device) = enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    let input = intent(&store, &caller);
    let AgentAdmissionResult::Fresh(admitted) = store
        .admit_agent_candidate(&caller, uuid::Uuid::from_u128(964), &input)
        .unwrap()
    else {
        panic!("actual fresh admission required")
    };
    let custody = admitted.into_custody();
    let before = store.state.generation;
    let runtime = ManagedRuntimeFactsV2 {
        runtime_epoch: uuid::Uuid::from_u128(91).to_string(),
        package_id: "so.tilo.agent".into(),
        package_hash: "a".repeat(64),
        executable_hash: "b".repeat(64),
        runtime_hash: "c".repeat(64),
    };
    let result = with_receipt_failure(|| {
        store.claim_launch_candidate_for_runtime(&custody, runtime.clone(), None)
    });
    assert!(
        matches!(result, LaunchCandidateClaim::PostCommit(_)),
        "a committed barrier cannot grant failed-append custody"
    );
    assert_eq!(store.state.generation, before + 1);
    assert!(store.failed_commit.get().is_none());
    assert!(custody.stages[0].load(Ordering::Acquire));
    drop(store);
    let mut reopened = Store::open(dir.path(), "private-runtime").unwrap();
    assert!(
        reopened
            .sole_admitted_spawn(&custody.id())
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        reopened.claim_launch_candidate_for_runtime(&custody, runtime, None),
        LaunchCandidateClaim::Committed(FactCommit::Existing(_), _)
    ));
    assert_eq!(reopened.state.generation, before + 1);
}
