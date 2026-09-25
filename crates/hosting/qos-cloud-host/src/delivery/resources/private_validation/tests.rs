use super::*;
use crate::delivery::resources::{AggregateBytes, CodecBudget, DecodeLimits, tests::precharged};
use automerge::resource_limits::NativeAllowance;
use qos_peer_state::owner_commit::*;
use qos_peer_state::{PeerLimits, PeerRegistry};
fn limits() -> DecodeLimits {
    DecodeLimits {
        maximum_bytes: 64 * 1024 * 1024,
        allocation_bytes: 8 * 1024 * 1024,
        collection_entries: 1024 * 1024,
    }
}
fn selected_v2() -> crate::State {
    let (_dir, mut store, device) = crate::agent_owner_tests::enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    // Existing exact retained-history literal; this is inert fixture data, not a
    // live provider/process admission. Rebind it to the actual enrolled Store.
    let literal = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../collab/qos-peer-state/src/owner_commit/fixtures/agent-v2-completed.json"
    ));
    let bytes = literal.replace("00000000-0000-0000-0000-000000000001", &store.state.room);
    let mut ledger: PrivateAgentLedgerV2 = serde_json::from_str(&bytes).unwrap();
    let offset = store.state.generation;
    for r in ledger.facts.values_mut() {
        r.generation += offset;
        r.change.expected_generation += offset;
        match &mut r.change.fact {
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
            _ => {}
        }
        rebind(r, &store.state.room);
    }
    ledger.validate(&store.state.room, offset + 8).unwrap();
    let mut state = store.state.clone();
    state.generation = offset + 8;
    state.private_agent_ledger = Some(PrivateAgentState::V2(ledger));
    state
}
fn rebind(r: &mut AgentRuntimeFactReceipt, room: &str) {
    r.change.payload_digest = *blake3::hash(&r.change.business_bytes().unwrap()).as_bytes();
    r.binding = OwnerCommitRequest {
        room: room.into(),
        operation: CommitOperation::AgentRuntime {
            change: Box::new(r.change.clone()),
        },
    }
    .binding()
    .unwrap();
}
fn validate(state: &crate::State) -> Result<(), PrivateStateFailure> {
    let bytes = serde_json::to_vec(state).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let work = turn.work().unwrap();
    let budget = CodecBudget::peer(&work);
    let input = AggregateBytes::copy_from(&bytes, bytes.len(), &budget).unwrap();
    let held = HeldCloudState::decode(&input, limits(), &budget).unwrap();
    let before = current.retained_bytes();
    let outcome = precharged(&current, || held.validate_private(&work));
    assert_eq!(current.retained_bytes(), before);
    assert_eq!(serde_json::to_vec(held.state()).unwrap(), bytes);
    outcome
}
#[test]
fn cloud_private_validation_actual_v1_v2_and_literal_outer_schemas() {
    assert_eq!(validate(&selected_v2()), Ok(()));
    for bytes in [
        include_bytes!("../../fixtures/v4.json").as_slice(),
        include_bytes!("../../fixtures/v5.json").as_slice(),
    ] {
        let state: crate::State = serde_json::from_slice(bytes).unwrap();
        assert_eq!(validate(&state), Ok(()));
    }
    let (_dir, mut store, device) = crate::agent_owner_tests::enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_ledger(&caller).unwrap();
    let (caller, request) = crate::agent_owner_tests::prepare(&store, device, "paid-private");
    store.commit_agent_request(&request, &caller).unwrap();
    assert_eq!(validate(&store.state), Ok(()));
}

fn written(state: &crate::State) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.json");
    std::fs::write(&path, serde_json::to_vec(state).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    root
}
#[test]
fn cloud_private_validation_retained_owner_mismatch_matches_original_open_without_rewrite() {
    for field in 0..4 {
        let mut state = selected_v2();
        let PrivateAgentState::V2(ledger) = state.private_agent_ledger.as_mut().unwrap() else {
            unreachable!()
        };
        for receipt in ledger.facts.values_mut() {
            if let AgentRuntimeFact::Admitted { intent, .. } = &mut receipt.change.fact {
                match field {
                    0 => intent.scope.machine = "another-machine".into(),
                    1 => intent.scope.host = uuid::Uuid::from_u128(918).to_string(),
                    2 => {
                        let root = ed25519_dalek::SigningKey::from_bytes(&[99; 32]);
                        intent.scope.caller.user_public_key = root.verifying_key().to_bytes();
                        qos_user_identity::sign_device_cert(&mut intent.scope.caller, &root)
                            .unwrap();
                    }
                    3 => {
                        intent.route.app_peer = if intent.route.app_peer == "999" {
                            "998"
                        } else {
                            "999"
                        }
                        .into()
                    }
                    _ => unreachable!(),
                }
                rebind(receipt, &state.room);
            }
        }
        // This is an owner mismatch, not a broken signature/digest shortcut.
        ledger.validate(&state.room, state.generation).unwrap();
        let root = written(&state);
        let original = std::fs::read(root.path().join("state.json")).unwrap();
        assert!(crate::Store::open(root.path(), &state.machine_id).is_err());
        assert_eq!(validate(&state), Err(PrivateStateFailure::Denied));
        assert_eq!(
            std::fs::read(root.path().join("state.json")).unwrap(),
            original
        );
    }
}
#[test]
fn cloud_private_validation_private_phase_precedes_bound_and_capacity_is_retryable() {
    let mut state = selected_v2();
    state.bound = None;
    let bytes = serde_json::to_vec(&state).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let work = turn.work().unwrap();
    let budget = CodecBudget::peer(&work);
    let input = AggregateBytes::copy_from(&bytes, bytes.len(), &budget).unwrap();
    let held = HeldCloudState::decode(&input, limits(), &budget).unwrap();
    let pressure = work.reserve_output(current.available_bytes()).unwrap();
    assert_eq!(
        precharged(&current, || held.validate_private(&work)),
        Err(PrivateStateFailure::Capacity)
    );
    drop(pressure);
    assert_eq!(
        precharged(&current, || held.validate_private(&work)),
        Err(PrivateStateFailure::Denied)
    );
    assert_eq!(serde_json::to_vec(held.state()).unwrap(), bytes);
    let valid = selected_v2();
    let bytes = serde_json::to_vec(&valid).unwrap();
    let input = AggregateBytes::copy_from(&bytes, bytes.len(), &budget).unwrap();
    let held = HeldCloudState::decode(&input, limits(), &budget).unwrap();
    let pressure = work.reserve_output(current.available_bytes()).unwrap();
    assert_eq!(
        precharged(&current, || held.validate_private(&work)),
        Err(PrivateStateFailure::Capacity)
    );
    drop(pressure);
    assert_eq!(
        precharged(&current, || held.validate_private(&work)),
        Ok(())
    );
}
#[test]
fn cloud_private_validation_preserves_v1_bound_semantics_and_requires_even_empty_v2_bound() {
    let mut v1: crate::State =
        serde_json::from_slice(include_bytes!("../../fixtures/v5.json")).unwrap();
    v1.bound = None;
    assert_eq!(validate(&v1), Ok(()));
    let mut v2 = selected_v2();
    v2.bound = None;
    let PrivateAgentState::V2(ledger) = v2.private_agent_ledger.as_mut().unwrap() else {
        unreachable!()
    };
    ledger.facts.clear();
    assert_eq!(validate(&v2), Err(PrivateStateFailure::Denied));
    let root = written(&v2);
    let before = std::fs::read(root.path().join("state.json")).unwrap();
    assert!(crate::Store::open(root.path(), &v2.machine_id).is_err());
    assert_eq!(
        std::fs::read(root.path().join("state.json")).unwrap(),
        before
    );
}
#[test]
fn cloud_private_validation_actual_outer_six_keeps_selected_private_version() {
    for state in [
        selected_v2(),
        serde_json::from_slice(include_bytes!("../../fixtures/v4.json")).unwrap(),
    ] {
        // Only enrolled Store can activate delivery; literal4 is an inert
        // schema fixture, so use a fresh enrolled outer4 in that branch.
        let state = if state.version == 4 {
            crate::agent_owner_tests::enrolled().1.state.clone()
        } else {
            state
        };
        let root = written(&state);
        let mut store = crate::Store::open(root.path(), &state.machine_id).unwrap();
        store
            .activate_delivery_storage(qos_relay::clock::now_ns())
            .unwrap();
        assert_eq!(store.state.version, 6);
        assert_eq!(validate(&store.state), Ok(()));
    }
}

#[test]
fn cloud_private_validation_semantic_failure_drops_diagnostic_before_credit() {
    let mut state = selected_v2();
    let PrivateAgentState::V2(ledger) = state.private_agent_ledger.as_mut().unwrap() else {
        unreachable!()
    };
    for r in ledger.facts.values_mut() {
        if let AgentRuntimeFact::DispatchIntended { envelope, .. } = &mut r.change.fact {
            let mut value: serde_json::Value = serde_json::from_str(envelope).unwrap();
            value[format!("unknown-{}", "界".repeat(400))] = serde_json::Value::Bool(true);
            *envelope = serde_json::to_string(&value).unwrap();
            rebind(r, &state.room);
        }
    }
    assert!(
        ledger
            .validate(&state.room, state.generation)
            .unwrap_err()
            .contains("unknown field")
    );
    assert_eq!(validate(&state), Err(PrivateStateFailure::Denied));
    let root = written(&state);
    let before = std::fs::read(root.path().join("state.json")).unwrap();
    assert!(crate::Store::open(root.path(), &state.machine_id).is_err());
    assert_eq!(
        std::fs::read(root.path().join("state.json")).unwrap(),
        before
    );
}
