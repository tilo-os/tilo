//! Real Store candidate/barrier tests. These do not manufacture or qualify live provider witnesses.
use super::*;
use crate::*;
pub(super) fn enrolled() -> (tempfile::TempDir, Store, [u8; 32]) {
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut store = Store::open(dir.path(), "private-runtime").unwrap();
    let request = crate::tests::approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&request, 11).unwrap();
    (dir, store, device.verifying_key().to_bytes())
}
pub(super) fn intent(store: &Store, caller: &VerifiedAgentCaller) -> AgentIntentV2 {
    let room = store.state.room.clone();
    AgentIntentV2 {
        scope: AgentInvocationScopeV2 {
            machine: store.state.machine_id.clone(),
            room: room.clone(),
            host: room.clone(),
            caller: caller.certificate().clone(),
        },
        route: AgentProviderRouteV2 {
            app_id: "so.tilo.agent".into(),
            app_peer: qos_relay::room_workspace_id(&room).unwrap().to_string(),
            cluster_id: room.clone(),
            record_id: String::new(),
            resource: format!("workspace:{room}:agents"),
            resource_kind: "workspace.agents".into(),
            action: "spawn".into(),
        },
        descriptor_revision: "a".repeat(64),
        request: String::from_utf8(
            qos_agent_control_protocol::canonical_request(&AgentRequest::Spawn {
                version: 1,
                name: "Private worker".into(),
                task: "Do not publish this task".into(),
                grant_set_id: uuid::Uuid::from_u128(99).to_string(),
            })
            .unwrap(),
        )
        .unwrap(),
        grants: vec![GrantAction {
            app_id: "so.tilo.word-processor".into(),
            record_id: "tab:0000000000000001".into(),
            resource: "document".into(),
            action: "insert".into(),
        }],
    }
}
#[test]
fn agent_owner_v2_explicit_selection_and_real_admission_replay() {
    let (dir, mut store, device) = enrolled();
    let caller = store.agent_caller(device).unwrap();
    let input = intent(&store, &caller);
    let invocation = uuid::Uuid::from_u128(1);
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    assert!(
        store
            .admit_agent_candidate(&caller, invocation, &input)
            .is_err()
    );
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    let peer = store.state.bound.as_ref().unwrap().peer.clone();
    let outbox = serde_json::to_vec(&store.state.outbox).unwrap();
    let AgentAdmissionResult::Fresh(a) = store
        .admit_agent_candidate(&caller, invocation, &input)
        .unwrap()
    else {
        panic!("fresh admission missing")
    };
    let a_generation = a.receipt.generation;
    let a_binding = a.receipt.binding.clone();
    let AgentAdmissionResult::Fresh(b) = store
        .admit_agent_candidate(&caller, uuid::Uuid::from_u128(2), &input)
        .unwrap()
    else {
        panic!("second admission missing")
    };
    let AgentAdmissionResult::Existing(replay) = store
        .admit_agent_candidate(&caller, invocation, &input)
        .unwrap()
    else {
        panic!("replay must not be fresh")
    };
    assert_eq!(replay.receipt.generation, b.receipt.generation);
    assert_eq!(replay.receipt.binding, a_binding);
    assert_eq!(
        replay.receipt.aggregate_hash,
        *blake3::hash(&serde_json::to_vec(&store.state).unwrap()).as_bytes()
    );
    assert!(!replay.qualified_completed);
    assert_eq!(store.state.bound.as_ref().unwrap().peer, peer);
    assert_eq!(serde_json::to_vec(&store.state.outbox).unwrap(), outbox);
    assert_eq!(
        store.runtime_ledger().unwrap().facts[&format!("{invocation}:admitted")].generation,
        a_generation
    );
    drop(store);
    let mut store = Store::open(dir.path(), "private-runtime").unwrap();
    let caller = store.agent_caller(device).unwrap();
    let AgentAdmissionResult::Existing(replay) = store
        .admit_agent_candidate(&caller, invocation, &input)
        .unwrap()
    else {
        panic!("reopen replay issued new admission")
    };
    assert_eq!(replay.agent_id, a.ids.agent_id);
    assert_eq!(replay.task_id, a.ids.task_id);
    assert_eq!(replay.receipt.generation, b.receipt.generation);
    let mut conflict = input.clone();
    conflict.descriptor_revision = "b".repeat(64);
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    assert!(
        store
            .admit_agent_candidate(&caller, invocation, &conflict)
            .is_err()
    );
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
}
#[test]
fn agent_owner_v2_failed_selection_keeps_writer_and_latch() {
    use atomic_file::test_support::Checkpoint;
    let (dir, mut store, device) = enrolled();
    let caller = store.agent_caller(device).unwrap();
    let result = crate::storage::test_support::with_save_failure(
        dir.path(),
        Checkpoint::BeforePublish,
        || store.activate_private_agent_runtime_v2(&caller),
    );
    assert!(result.is_err());
    assert!(
        store.agent_caller(device).is_err(),
        "uncertain owner must remain latched"
    );
    drop(store);
    let store = Store::open(dir.path(), "private-runtime").unwrap();
    assert_eq!(store.state.version, 4);
    assert!(store.state.private_agent_ledger.is_none());
}

#[test]
fn agent_owner_v2_capacity_is_explicit_and_keeps_retained_status() {
    let (dir, mut store, device) = enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    let input = intent(&store, &caller);
    // Every live spawn up to the explicit limit is admitted with the same
    // (unchanged) descriptor/offer intent: admitting agents changes neither.
    let limit = MAX_LIVE_PRIVATE_AGENTS as u128;
    assert!(limit >= 16, "a workspace holds at least 16 live agents");
    for n in 1..=limit {
        assert!(matches!(
            store
                .admit_agent_candidate(&caller, uuid::Uuid::from_u128(n), &input)
                .unwrap(),
            AgentAdmissionResult::Fresh(_)
        ));
    }
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    assert!(matches!(
        store.admit_agent_candidate(&caller, uuid::Uuid::from_u128(limit + 1), &input),
        Err(AgentCommitError::Capacity {
            limit: MAX_LIVE_PRIVATE_AGENTS
        })
    ));
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    let id = AgentControlId::new(uuid::Uuid::from_u128(1)).unwrap();
    assert!(store.observe_agent_runtime(&caller, &id).is_ok());
    assert_eq!(
        store.retained_agent_intent(&caller, &id).unwrap(),
        Some(input.clone())
    );
    assert!(
        store
            .observe_agent_intent(&caller, &id, &input)
            .unwrap()
            .is_some()
    );
    let mut wrong = input;
    wrong.descriptor_revision = "b".repeat(64);
    assert!(store.observe_agent_intent(&caller, &id, &wrong).is_err());
}
#[test]
fn agent_owner_v2_uncertain_admission_reopens_as_existing_never_fresh() {
    use atomic_file::test_support::Checkpoint;
    let (dir, mut store, device) = enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    let input = intent(&store, &caller);
    let invocation = uuid::Uuid::from_u128(1);
    let generation = store.state.generation + 1;
    let result = crate::storage::test_support::with_save_failure(
        dir.path(),
        Checkpoint::BeforeCommit,
        || store.admit_agent_candidate(&caller, invocation, &input),
    );
    assert!(matches!(result, Err(AgentCommitError::Uncertain { .. })));
    assert!(store.agent_caller(device).is_err());
    assert!(
        store
            .admit_agent_candidate(&caller, invocation, &input)
            .is_err()
    );
    drop(store);
    let mut store = Store::open(dir.path(), "private-runtime").unwrap();
    let caller = store.agent_caller(device).unwrap();
    let AgentAdmissionResult::Existing(observed) = store
        .admit_agent_candidate(&caller, invocation, &input)
        .unwrap()
    else {
        panic!("uncertain admission was issued fresh")
    };
    assert_eq!(observed.receipt.generation, generation);
    assert_eq!(store.runtime_ledger().unwrap().facts.len(), 1);
    assert!(!observed.qualified_completed);
}
#[test]
fn agent_owner_v2_corrupt_selected_input_is_never_rewritten() {
    for failure in ["floor", "duplicate", "unknown", "scope"] {
        let (dir, mut store, device) = enrolled();
        let caller = store.agent_caller(device).unwrap();
        store.activate_private_agent_runtime_v2(&caller).unwrap();
        let input = intent(&store, &caller);
        store
            .admit_agent_candidate(&caller, uuid::Uuid::from_u128(1), &input)
            .unwrap();
        let mut state = store.state.clone();
        if failure == "scope" {
            let Some(PrivateAgentState::V2(ledger)) = &mut state.private_agent_ledger else {
                unreachable!()
            };
            let receipt = ledger.facts.values_mut().next().unwrap();
            let AgentRuntimeFact::Admitted { intent, .. } = &mut receipt.change.fact else {
                unreachable!()
            };
            intent.scope.machine = "another-owner".into();
            receipt.change.payload_digest =
                *blake3::hash(&receipt.change.business_bytes().unwrap()).as_bytes();
            receipt.binding = OwnerCommitRequest {
                room: state.room.clone(),
                operation: CommitOperation::AgentRuntime {
                    change: Box::new(receipt.change.clone()),
                },
            }
            .binding()
            .unwrap();
        }
        let mut value = serde_json::to_value(state).unwrap();
        match failure {
            "floor" => value["private_agent_ledger"]["writer_floor"] = 1.into(),
            "unknown" => value["private_agent_ledger"]["surprise"] = true.into(),
            _ => (),
        }
        let mut bytes = serde_json::to_vec(&value).unwrap();
        if failure == "duplicate" {
            bytes = String::from_utf8(bytes)
                .unwrap()
                .replace(
                    "\"writer_floor\":2",
                    "\"writer_floor\":2,\"writer_floor\":2",
                )
                .into_bytes();
        }
        drop(store);
        std::fs::write(dir.path().join("state.json"), &bytes).unwrap();
        assert!(
            Store::open(dir.path(), "private-runtime").is_err(),
            "accepted {failure}"
        );
        assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), bytes);
    }
}

// Retained-history data fixture, not a live process or recipient witness.
pub(super) fn retained_completed_state(store: &Store, caller: &VerifiedAgentCaller) -> State {
    let literal = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../collab/qos-peer-state/src/owner_commit/fixtures/agent-v2-completed.json"
    ));
    let bytes = literal.replace("00000000-0000-0000-0000-000000000001", &store.state.room);
    let mut ledger: PrivateAgentLedgerV2 = serde_json::from_str(&bytes).unwrap();
    let offset = store.state.generation;
    for receipt in ledger.facts.values_mut() {
        receipt.generation += offset;
        receipt.change.expected_generation += offset;
        match &mut receipt.change.fact {
            AgentRuntimeFact::Admitted { intent, .. } => {
                intent.scope.machine = store.state.machine_id.clone();
                intent.scope.caller = caller.certificate().clone();
                intent.route.app_peer = qos_relay::room_workspace_id(&store.state.room)
                    .unwrap()
                    .to_string();
            }
            AgentRuntimeFact::LaunchIntended { binding, .. } => binding.generation += offset,
            AgentRuntimeFact::ProcessBound { process, .. } => process.binding.generation += offset,
            AgentRuntimeFact::ProcessExited { exit, .. } => exit.binding.generation += offset,
            _ => (),
        }
        rebind_runtime_fixture(receipt, &store.state.room);
    }
    ledger.validate(&store.state.room, offset + 8).unwrap();
    let mut state = store.state.clone();
    state.generation = offset + 8;
    state.private_agent_ledger = Some(PrivateAgentState::V2(ledger));
    state
}
fn rebind_runtime_fixture(receipt: &mut AgentRuntimeFactReceipt, room: &str) {
    receipt.change.payload_digest =
        *blake3::hash(&receipt.change.business_bytes().unwrap()).as_bytes();
    receipt.binding = OwnerCommitRequest {
        room: room.into(),
        operation: CommitOperation::AgentRuntime {
            change: Box::new(receipt.change.clone()),
        },
    }
    .binding()
    .unwrap();
}
#[test]
fn agent_owner_v2_review_rebound_invalid_history_preserves_input_on_open() {
    let (dir, mut store, device) = enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    let valid = retained_completed_state(&store, &caller);
    drop(store);
    let path = dir.path().join("state.json");
    let valid_bytes = serde_json::to_vec(&valid).unwrap();
    std::fs::write(&path, &valid_bytes).unwrap();
    let restored = Store::open(dir.path(), "private-runtime").unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        valid_bytes,
        "valid fixture must reopen without rewriting"
    );
    drop(restored);
    for defect in [
        "facets",
        "context_unknown",
        "group_unknown",
        "first_two",
        "over_capacity",
    ] {
        let mut bad = valid.clone();
        let Some(PrivateAgentState::V2(ledger)) = &mut bad.private_agent_ledger else {
            unreachable!()
        };
        for receipt in ledger.facts.values_mut() {
            match &mut receipt.change.fact {
                AgentRuntimeFact::DispatchIntended { envelope, .. }
                    if !defect.starts_with("first") && defect != "over_capacity" =>
                {
                    let value: serde_json::Value = serde_json::from_str(envelope).unwrap();
                    let mut context = value["task"]["action_context"].clone();
                    let old = serde_json::to_string(&context).unwrap();
                    match defect {
                        "facets" => context["facets"] = serde_json::json!(0),
                        "context_unknown" => context["surprise"] = serde_json::json!(null),
                        "group_unknown" => context["group"]["surprise"] = serde_json::json!(null),
                        _ => unreachable!(),
                    }
                    *envelope = envelope.replacen(
                        &format!("\"action_context\":{old}"),
                        &format!(
                            "\"action_context\":{}",
                            serde_json::to_string(&context).unwrap()
                        ),
                        1,
                    );
                }
                AgentRuntimeFact::Completed { acceptance, .. }
                    if defect == "first_two" || defect == "over_capacity" =>
                {
                    acceptance.acceptance_sequence = if defect == "first_two" { 2 } else { 1025 }
                }
                AgentRuntimeFact::TaskFinished {
                    acceptance_sequence,
                    ..
                } if defect == "first_two" || defect == "over_capacity" => {
                    *acceptance_sequence = if defect == "first_two" { 2 } else { 1025 }
                }
                _ => continue,
            }
            rebind_runtime_fixture(receipt, &bad.room);
        }
        let bytes = serde_json::to_vec(&bad).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            Store::open(dir.path(), "private-runtime").is_err(),
            "accepted {defect}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "rewrote invalid {defect}"
        );
    }
}

#[test]
fn agent_prelaunch_recovery_restart_and_close_preserve_only_admitted_spawn() {
    let (dir, mut store, device) = enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    let input = intent(&store, &caller);
    let invocation = uuid::Uuid::from_u128(907);
    let AgentAdmissionResult::Fresh(admitted) = store
        .admit_agent_candidate(&caller, invocation, &input)
        .unwrap()
    else {
        panic!("fresh");
    };
    let original = serde_json::to_vec(&store.state).unwrap();
    let ids = admitted.ids.clone();
    drop(store);
    for _ in 0..2 {
        let owner = crate::agent_runtime_adapter::RuntimeOwner::open(dir.path(), "private-runtime")
            .unwrap();
        owner.retain_restart_uncertainty().unwrap();
        owner
            .uncertain(invocation, "gateway failed before launch")
            .unwrap();
        owner
            .uncertain(invocation, "queued or connecting runtime closed")
            .unwrap();
        let store = owner.store().unwrap();
        assert_eq!(
            blake3::hash(&serde_json::to_vec(&store.state).unwrap()),
            blake3::hash(&original),
            "an admission with no launch intent must remain durable pending work"
        );
        let current = store.agent_caller(device).unwrap();
        let observed = store.observe_agent_runtime(&current, &admitted.id).unwrap();
        assert_eq!(observed.phase, AgentRuntimePhase::Admitted);
        assert_eq!(observed.agent_id, ids.agent_id);
        assert!(
            owner.attempt(invocation).is_err(),
            "startup labels minted live custody"
        );
    }
}
