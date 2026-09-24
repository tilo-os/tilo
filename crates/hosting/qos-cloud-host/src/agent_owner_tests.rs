//! Real Store regressions for private, durable agent admission. No process is simulated.
use super::*;
use atomic_file::test_support::{Checkpoint, with_failure};
use qos_agent_control_protocol::{AgentRequest, GrantAction, canonical_request};
use qos_peer_state::owner_commit::*;

pub(super) fn enrolled() -> (tempfile::TempDir, Store, [u8; 32]) {
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut store = Store::open(dir.path(), "agent-admission").unwrap();
    let request = crate::tests::approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&request, 11).unwrap();
    (dir, store, device.verifying_key().to_bytes())
}
pub(super) fn prepare(
    store: &Store,
    device: [u8; 32],
    invocation: &str,
) -> (VerifiedAgentCaller, OwnerCommitRequest) {
    let caller = store.agent_caller(device).unwrap();
    let request = canonical_request(&AgentRequest::Spawn {
        version: 1,
        name: "Private worker".into(),
        task: "secret task never published".into(),
        grant_set_id: "offer-1".into(),
    })
    .unwrap();
    let change = AgentChange::new(
        &caller,
        AgentIds {
            mutation: format!("prepare:{invocation}"),
            invocation: invocation.into(),
            agent: format!("agent:{invocation}"),
            task: format!("task:{invocation}"),
            run: format!("run:{invocation}"),
        },
        AgentMutation::Prepare {
            invocation: invocation.into(),
            request,
            grants: vec![GrantAction {
                app_id: "dev.qos.word-processor".into(),
                record_id: "tab:0000000000000001".into(),
                resource: "document".into(),
                action: "insert".into(),
            }],
        },
    )
    .unwrap();
    (
        caller.clone(),
        OwnerCommitRequest {
            room: caller.room().into(),
            operation: CommitOperation::Agent {
                change: Box::new(change),
            },
        },
    )
}
#[test]
fn agent_owner_v4_stays_literal_until_explicit_private_selection() {
    let (dir, mut store, device) = enrolled();
    let bytes = std::fs::read(dir.path().join("state.json")).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["version"],
        4
    );
    let (caller, request) = prepare(&store, device, "first");
    assert!(store.commit_agent_request(&request, &caller).is_err());
    assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), bytes);
    drop(store);
    let mut store = Store::open(dir.path(), "agent-admission").unwrap();
    assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), bytes);
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_ledger(&caller).unwrap();
    let selected = std::fs::read(dir.path().join("state.json")).unwrap();
    let selected: serde_json::Value = serde_json::from_slice(&selected).unwrap();
    assert_eq!(selected["version"], 5);
    assert_eq!(selected["private_agent_ledger"]["version"], 1);
    let mut old: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let mut current = selected;
    old.as_object_mut().unwrap().remove("version");
    old.as_object_mut().unwrap().remove("generation");
    current.as_object_mut().unwrap().remove("version");
    current.as_object_mut().unwrap().remove("generation");
    current
        .as_object_mut()
        .unwrap()
        .remove("private_agent_ledger");
    assert_eq!(
        current, old,
        "selection must preserve every existing Store field"
    );
}

#[test]
fn agent_owner_inner_v2_reader_preserves_legacy_and_selected_floor() {
    let (dir, mut store, device) = enrolled();
    store
        .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
        .unwrap();
    let (caller, request) = prepare(&store, device, "retained-v1");
    store.commit_agent_request(&request, &caller).unwrap();
    let original = std::fs::read(dir.path().join("state.json")).unwrap();
    let mut selected: serde_json::Value = serde_json::from_slice(&original).unwrap();
    let legacy = selected["private_agent_ledger"].take();
    selected["private_agent_ledger"] = serde_json::json!({
        "version":2,"writer_floor":2,"legacy_v1":legacy,"facts":{}
    });
    let selected_bytes = serde_json::to_vec(&selected).unwrap();
    drop(store);
    std::fs::write(dir.path().join("state.json"), &selected_bytes).unwrap();
    let mut reopened = Store::open(dir.path(), "agent-admission").unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        selected_bytes
    );
    let current = reopened.state.clone();
    reopened.commit(current).unwrap();
    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("state.json")).unwrap()).unwrap();
    assert_eq!(after["version"], 5);
    assert_eq!(
        after["private_agent_ledger"],
        selected["private_agent_ledger"]
    );
    let caller = reopened.agent_caller(device).unwrap();
    assert!(
        reopened.commit_agent_request(&request, &caller).is_err(),
        "v2 must not mutate frozen v1 records"
    );
}
#[test]
fn agent_owner_private_admission_reopens_same_ids_without_publication_or_replay() {
    let (dir, mut store, device) = enrolled();
    let proof = store.agent_caller(device).unwrap();
    store.activate_private_agent_ledger(&proof).unwrap();
    let peer = store.state.bound.as_ref().unwrap().peer.clone();
    let outbox = serde_json::to_vec(&store.state.outbox).unwrap();
    let (caller, request) = prepare(&store, device, "one");
    let receipt = store.commit_agent_request(&request, &caller).unwrap();
    receipt.validate_for(&request).unwrap();
    assert_eq!(store.state.bound.as_ref().unwrap().peer, peer);
    assert_eq!(serde_json::to_vec(&store.state.outbox).unwrap(), outbox);
    drop(store);
    let mut store = Store::open(dir.path(), "agent-admission").unwrap();
    let generation = store.state.generation;
    let fresh = store.agent_caller(device).unwrap();
    let replay = store.commit_agent_request(&request, &fresh).unwrap();
    assert_eq!(replay.binding, receipt.binding);
    assert_eq!(replay.generation, receipt.generation);
    assert_eq!(store.state.generation, generation);
    let record = store.agent_snapshot(&fresh).unwrap().ledger.records["one"].clone();
    assert_eq!(record.ids.agent, "agent:one");
    assert_eq!(record.ids.task, "task:one");
    assert_eq!(record.ids.run, "run:one");
    assert_eq!(record.phase, AgentControlPhase::Admitted);
    let mut changed = request.clone();
    if let CommitOperation::Agent { change } = &mut changed.operation {
        if let AgentMutation::Prepare { request, .. } = &mut change.mutation {
            request.push(b' ');
        }
    }
    assert!(store.commit_agent_request(&changed, &fresh).is_err());
    assert_eq!(store.state.generation, generation);
}
#[test]
fn agent_owner_private_barrier_failure_latches_and_reopens_original_admission() {
    let (dir, mut store, device) = enrolled();
    store
        .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
        .unwrap();
    let (caller, request) = prepare(&store, device, "lost-reply");
    let generation = store.state.generation + 1;
    assert!(matches!(
        with_failure(Checkpoint::BeforeCommit, || store
            .commit_agent_request(&request, &caller)),
        Err(AgentCommitError::Uncertain { .. })
    ));
    assert!(store.agent_caller(device).is_err());
    assert!(store.commit_agent_request(&request, &caller).is_err());
    drop(store);
    let mut store = Store::open(dir.path(), "agent-admission").unwrap();
    let fresh = store.agent_caller(device).unwrap();
    let receipt = store.commit_agent_request(&request, &fresh).unwrap();
    assert_eq!(receipt.generation, generation);
    assert_eq!(store.state.generation, generation);
    assert_eq!(
        store.agent_snapshot(&fresh).unwrap().ledger.records.len(),
        1
    );
}
#[test]
fn agent_owner_private_format_corruption_refuses_rewrite() {
    for mode in ["version", "unknown", "duplicate", "missing"] {
        let (dir, mut store, device) = enrolled();
        store
            .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
            .unwrap();
        drop(store);
        let path = dir.path().join("state.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        match mode {
            "version" => value["private_agent_ledger"]["version"] = 2.into(),
            "unknown" => value["private_agent_ledger"]["unrecognized"] = true.into(),
            "missing" => {
                value
                    .as_object_mut()
                    .unwrap()
                    .remove("private_agent_ledger");
            }
            _ => (),
        }
        let mut bytes = serde_json::to_vec(&value).unwrap();
        if mode == "duplicate" {
            bytes = String::from_utf8(bytes)
                .unwrap()
                .replacen(
                    "\"private_agent_ledger\":{",
                    "\"private_agent_ledger\":{\"version\":1,",
                    1,
                )
                .into_bytes();
        }
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            Store::open(dir.path(), "agent-admission").is_err(),
            "{mode}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn agent_owner_controller_concurrent_prepare_and_reopen_return_original_business_ids() {
    use qos_agent_control_service::AgentAdmission;
    use std::sync::Arc;
    let (dir, mut store, device) = enrolled();
    store
        .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
        .unwrap();
    drop(store);
    let authority = Arc::new(
        crate::agent_owner::CloudAgentAuthority::open(dir.path(), "agent-admission").unwrap(),
    );
    let caller = authority.caller(device).unwrap();
    let catalogue = authority.grant_catalogue(vec![GrantAction {
        app_id: "dev.qos.word-processor".into(),
        record_id: "tab:0000000000000001".into(),
        resource: "document".into(),
        action: "insert".into(),
    }]);
    let admission = Arc::new(AgentAdmission::new(
        authority.clone(),
        authority.clone(),
        Arc::new(catalogue),
    ));
    let offer = admission.offers(&caller).unwrap().remove(0);
    let input = qos_agent_control_protocol::SpawnInput {
        name: "Worker".into(),
        task: "private concurrent command".into(),
        grant_set_id: offer.id,
    };
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut workers = vec![];
    for _ in 0..2 {
        let admission = admission.clone();
        let caller = caller.clone();
        let input = input.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            admission.prepare(&caller, "one-command", input).unwrap()
        }));
    }
    barrier.wait();
    let a = workers.remove(0).join().unwrap();
    let b = workers.remove(0).join().unwrap();
    assert_eq!(a.ids, b.ids);
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    drop(admission);
    let admission = AgentAdmission::new(
        authority.clone(),
        authority.clone(),
        Arc::new(authority.grant_catalogue(vec![])),
    );
    let again = admission
        .prepare(&caller, "one-command", input.clone())
        .unwrap();
    assert_eq!(
        again.ids, a.ids,
        "known ID reattaches even with no retained grant offer"
    );
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    let mut changed = input;
    changed.task = "different business intent".into();
    assert!(admission.prepare(&caller, "one-command", changed).is_err());
    let status = admission.status(&caller, "one-command").unwrap();
    assert_eq!(status.invocation_id, "one-command");
    assert_eq!(status.agent_id.as_deref(), Some(a.ids.agent.as_str()));
    assert!(admission.status(&caller, "never-admitted").is_err());
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
}

#[test]
fn agent_owner_unknown_and_terminal_status_never_relaunch_or_regress() {
    let (dir, mut store, device) = enrolled();
    store
        .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
        .unwrap();
    let (caller, request) = prepare(&store, device, "uncertain");
    store.commit_agent_request(&request, &caller).unwrap();
    let CommitOperation::Agent { change } = request.operation else {
        unreachable!()
    };
    let mut ids = change.ids;
    for (i, phase) in [
        AgentControlPhase::Launching,
        AgentControlPhase::Running,
        AgentControlPhase::Unknown,
        AgentControlPhase::Completed,
    ]
    .into_iter()
    .enumerate()
    {
        ids.mutation = format!("outcome:{i}");
        let caller = store.agent_caller(device).unwrap();
        let change = AgentChange::new(
            &caller,
            ids.clone(),
            AgentMutation::Transition {
                phase,
                error: if phase == AgentControlPhase::Unknown {
                    Some(qos_agent_control_protocol::AgentError {
                        code: qos_agent_control_protocol::AgentErrorCode::OutcomeUncertain,
                        message: "supervisor custody lost".into(),
                    })
                } else {
                    None
                },
            },
        )
        .unwrap();
        store
            .commit_agent_request(
                &OwnerCommitRequest {
                    room: caller.room().into(),
                    operation: CommitOperation::Agent {
                        change: Box::new(change),
                    },
                },
                &caller,
            )
            .unwrap();
        drop(store);
        store = Store::open(dir.path(), "agent-admission").unwrap();
        let fresh = store.agent_caller(device).unwrap();
        assert_eq!(
            store.agent_snapshot(&fresh).unwrap().ledger.records["uncertain"].phase,
            phase
        );
    }
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    let caller = store.agent_caller(device).unwrap();
    ids.mutation = "late-running".into();
    let change = AgentChange::new(
        &caller,
        ids,
        AgentMutation::Transition {
            phase: AgentControlPhase::Running,
            error: None,
        },
    )
    .unwrap();
    assert!(
        store
            .commit_agent_request(
                &OwnerCommitRequest {
                    room: caller.room().into(),
                    operation: CommitOperation::Agent {
                        change: Box::new(change)
                    }
                },
                &caller
            )
            .is_err()
    );
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
}

#[test]
fn agent_owner_private_phase_and_nested_shape_require_exact_evidence() {
    for mode in ["phase", "nested", "binding"] {
        let (dir, mut store, device) = enrolled();
        store
            .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
            .unwrap();
        let (caller, request) = prepare(&store, device, "original");
        store.commit_agent_request(&request, &caller).unwrap();
        drop(store);
        let path = dir.path().join("state.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        if mode == "phase" {
            value["private_agent_ledger"]["records"]["original"]["phase"] = "Completed".into();
        } else if mode == "nested" {
            value["private_agent_ledger"]["records"]["original"]["caller"]["unrecognized_authority"] =
                true.into();
        } else {
            value["private_agent_ledger"]["mutations"]["prepare:original"]["binding"]["unrecognized"] =
                true.into();
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            Store::open(dir.path(), "agent-admission").is_err(),
            "accepted altered {mode}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn agent_owner_unselected_v4_rejects_explicit_private_field() {
    let (dir, store, _) = enrolled();
    drop(store);
    let path = dir.path().join("state.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["private_agent_ledger"] = serde_json::Value::Null;
    let bytes = serde_json::to_vec(&value).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert!(Store::open(dir.path(), "agent-admission").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn agent_owner_other_device_cannot_alias_retained_business_identity() {
    let (dir, mut store, device) = enrolled();
    store
        .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
        .unwrap();
    let (caller, request) = prepare(&store, device, "reserved");
    store.commit_agent_request(&request, &caller).unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let second = SigningKey::from_bytes(&[9; 32]);
    let enrollment = crate::tests::approved(store.inspect(20).unwrap(), &root, &second);
    store.apply(&enrollment, 21).unwrap();
    let (other, alias) = prepare(&store, second.verifying_key().to_bytes(), "reserved");
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    assert!(store.commit_agent_request(&alias, &other).is_err());
    assert!(
        store
            .agent_snapshot(&other)
            .unwrap()
            .ledger
            .record_for(&other, "reserved")
            .is_err()
    );
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    let original = store.agent_caller(device).unwrap();
    assert_eq!(
        store
            .agent_snapshot(&original)
            .unwrap()
            .ledger
            .record_for(&original, "reserved")
            .unwrap()
            .unwrap()
            .ids
            .agent,
        "agent:reserved"
    );
}

#[test]
fn agent_owner_unbound_message_admission_is_unavailable() {
    let (_dir, mut store, device) = enrolled();
    store
        .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
        .unwrap();
    let (caller, mut request) = prepare(&store, device, "message");
    let CommitOperation::Agent { change } = &mut request.operation else {
        unreachable!()
    };
    let grants = match &change.mutation {
        AgentMutation::Prepare { grants, .. } => grants.clone(),
        _ => unreachable!(),
    };
    let message = canonical_request(&AgentRequest::Message {
        version: 1,
        agent_id: "unadmitted-agent".into(),
        text: "private follow-up".into(),
    })
    .unwrap();
    assert!(
        AgentChange::new(
            &caller,
            change.ids.clone(),
            AgentMutation::Prepare {
                invocation: "message".into(),
                request: message,
                grants
            }
        )
        .is_err()
    );
}
#[test]
fn agent_owner_capacity_retains_all_prior_admissions_and_replay() {
    let (dir, mut store, device) = enrolled();
    store
        .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
        .unwrap();
    let mut saved = vec![];
    for i in 0..4 {
        let (caller, request) = prepare(&store, device, &format!("active-{i}"));
        store.commit_agent_request(&request, &caller).unwrap();
        saved.push(request);
    }
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    let (caller, request) = prepare(&store, device, "fifth");
    assert!(matches!(
        store.commit_agent_request(&request, &caller),
        Err(AgentCommitError::Capacity { .. })
    ));
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    for request in saved {
        let fresh = store.agent_caller(device).unwrap();
        store.commit_agent_request(&request, &fresh).unwrap();
    }
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
}

#[test]
fn agent_owner_replay_receipt_certifies_current_aggregate_and_retains_original_generation() {
    let (dir, mut store, device) = enrolled();
    store
        .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
        .unwrap();
    let (caller, a) = prepare(&store, device, "a");
    let admitted = store.commit_agent_request(&a, &caller).unwrap();
    let (caller, b) = prepare(&store, device, "b");
    let later = store.commit_agent_request(&b, &caller).unwrap();
    assert!(later.generation > admitted.generation);
    for _ in 0..2 {
        let before = std::fs::read(dir.path().join("state.json")).unwrap();
        let caller = store.agent_caller(device).unwrap();
        let replay = store.commit_agent_request(&a, &caller).unwrap();
        replay.validate_for(&a).unwrap();
        assert_eq!(replay.binding, admitted.binding);
        assert_eq!(replay.generation, later.generation);
        assert_eq!(replay.aggregate_hash, later.aggregate_hash);
        assert_eq!(
            replay.aggregate_hash,
            *blake3::hash(&serde_json::to_vec(&store.state).unwrap()).as_bytes()
        );
        let ledger = store.agent_snapshot(&caller).unwrap().ledger;
        assert_eq!(ledger.records["a"].admitted_generation, admitted.generation);
        assert_eq!(
            ledger.mutations["prepare:a"].generation,
            admitted.generation
        );
        assert_eq!(
            std::fs::read(dir.path().join("state.json")).unwrap(),
            before
        );
        drop(store);
        store = Store::open(dir.path(), "agent-admission").unwrap();
    }
}

fn diagnostic_transition(
    store: &Store,
    device: [u8; 32],
    ids: &AgentIds,
    phase: AgentControlPhase,
) -> (VerifiedAgentCaller, OwnerCommitRequest) {
    let caller = store.agent_caller(device).unwrap();
    let mut ids = ids.clone();
    ids.mutation = "diagnostic".into();
    let change = AgentChange::new(
        &caller,
        ids,
        AgentMutation::Transition {
            phase,
            error: Some(qos_agent_control_protocol::AgentError {
                code: qos_agent_control_protocol::AgentErrorCode::OutcomeUncertain,
                message: "Retained execution outcome is unknown 🦀".into(),
            }),
        },
    )
    .unwrap();
    (
        caller.clone(),
        OwnerCommitRequest {
            room: caller.room().into(),
            operation: CommitOperation::Agent {
                change: Box::new(change),
            },
        },
    )
}

fn invalid_diagnostics() -> [qos_agent_control_protocol::AgentError; 2] {
    use qos_agent_control_protocol::{AgentError, AgentErrorCode};
    [
        AgentError {
            code: AgentErrorCode::OutcomeUncertain,
            message: String::new(),
        },
        AgentError {
            code: AgentErrorCode::UnknownInvocation,
            message: "An admitted operation cannot be absent".into(),
        },
    ]
}

#[test]
fn agent_owner_invalid_diagnostics_cannot_commit() {
    for phase in [AgentControlPhase::Failed, AgentControlPhase::Unknown] {
        for invalid in invalid_diagnostics() {
            let (dir, mut store, device) = enrolled();
            store
                .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
                .unwrap();
            let (caller, a) = prepare(&store, device, "diagnostic-owner");
            store.commit_agent_request(&a, &caller).unwrap();
            let CommitOperation::Agent { change } = &a.operation else {
                unreachable!()
            };
            let (caller, mut transition) =
                diagnostic_transition(&store, device, &change.ids, phase);
            let CommitOperation::Agent { change } = &mut transition.operation else {
                unreachable!()
            };
            if let AgentMutation::Transition { error, .. } = &mut change.mutation {
                *error = Some(invalid);
            }
            change.payload_digest = *blake3::hash(&change.business_bytes().unwrap()).as_bytes();
            let before = std::fs::read(dir.path().join("state.json")).unwrap();
            assert!(
                store.commit_agent_request(&transition, &caller).is_err(),
                "accepted invalid {phase:?} diagnostic"
            );
            assert_eq!(
                std::fs::read(dir.path().join("state.json")).unwrap(),
                before
            );
        }
    }
}

#[test]
fn agent_owner_invalid_retained_diagnostics_preserve_input() {
    for phase in [AgentControlPhase::Failed, AgentControlPhase::Unknown] {
        for invalid in invalid_diagnostics() {
            let (dir, mut store, device) = enrolled();
            store
                .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
                .unwrap();
            let (caller, a) = prepare(&store, device, "diagnostic-owner");
            store.commit_agent_request(&a, &caller).unwrap();
            let CommitOperation::Agent { change } = &a.operation else {
                unreachable!()
            };
            let (caller, transition) = diagnostic_transition(&store, device, &change.ids, phase);
            store.commit_agent_request(&transition, &caller).unwrap();
            let mut corrupt = store.state.clone();
            let PrivateAgentState::V1(ledger) = corrupt.private_agent_ledger.as_mut().unwrap()
            else {
                panic!("fixture must remain v1")
            };
            ledger.records.get_mut("diagnostic-owner").unwrap().error = Some(invalid.clone());
            let retained = ledger.mutations.get_mut("diagnostic").unwrap();
            if let AgentMutation::Transition { error, .. } = &mut retained.change.mutation {
                *error = Some(invalid);
            }
            let business = retained.change.business_bytes().unwrap();
            retained.change.payload_digest = *blake3::hash(&business).as_bytes();
            // Produce the exact legacy binding independently of validation, so
            // rejection must cover the diagnostic, not a stale checksum.
            let encoded =
                postcard::to_allocvec(&(&corrupt.room, &retained.binding.target, &business))
                    .unwrap();
            retained.binding.request_digest =
                *blake3::Hasher::new_derive_key("qOS owner commit request v1")
                    .update(&encoded)
                    .finalize()
                    .as_bytes();
            retained.binding.basis =
                *blake3::Hasher::new_derive_key("qOS owner committed basis v1")
                    .update(&business)
                    .finalize()
                    .as_bytes();
            let bytes = serde_json::to_vec(&corrupt).unwrap();
            drop(store);
            let path = dir.path().join("state.json");
            std::fs::write(&path, &bytes).unwrap();
            assert!(
                Store::open(dir.path(), "agent-admission").is_err(),
                "accepted stored invalid {phase:?} diagnostic"
            );
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
    }
}

#[test]
fn agent_owner_actual_status_diagnostics_roundtrip_frozen_response_contract() {
    use qos_agent_control_protocol::{
        AgentBinding, AgentResponse, AgentResponseKind, ExpectedResponse, decode_response,
    };
    use qos_agent_control_service::AgentAdmission;
    use std::sync::Arc;
    for phase in [AgentControlPhase::Failed, AgentControlPhase::Unknown] {
        let (dir, mut store, device) = enrolled();
        store
            .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
            .unwrap();
        let (caller, a) = prepare(&store, device, "diagnostic-owner");
        store.commit_agent_request(&a, &caller).unwrap();
        let CommitOperation::Agent { change } = &a.operation else {
            unreachable!()
        };
        let (caller, transition) = diagnostic_transition(&store, device, &change.ids, phase);
        store.commit_agent_request(&transition, &caller).unwrap();
        let binding = AgentBinding {
            machine: "agent-admission".into(),
            room: store.state.room.clone(),
            device: hex::encode(device),
            host: hex::encode(store.key().verifying_key().to_bytes()),
        };
        drop(store);
        let authority = Arc::new(
            crate::agent_owner::CloudAgentAuthority::open(dir.path(), "agent-admission").unwrap(),
        );
        let admission = AgentAdmission::new(
            authority.clone(),
            authority.clone(),
            Arc::new(authority.grant_catalogue(vec![])),
        );
        let caller = authority.caller(device).unwrap();
        let operation = admission.status(&caller, "diagnostic-owner").unwrap();
        let expected = ExpectedResponse {
            invocation_id: "diagnostic-owner".into(),
            binding: binding.clone(),
            kind: Some(AgentResponseKind::Operation),
        };
        let response = AgentResponse::Operation {
            version: 1,
            invocation_id: "diagnostic-owner".into(),
            binding,
            operation,
        };
        let bytes = serde_json::to_vec(&response).unwrap();
        assert_eq!(decode_response(&bytes, &expected).unwrap(), response);
    }
}

#[test]
fn agent_owner_v2_explicit_migration_preserves_uncertain_and_unqualified_v1_terminal() {
    use crate::agent_runtime_owner::{AgentControlId, AgentRuntimePhase};
    for terminal in [AgentControlPhase::Unknown, AgentControlPhase::Completed] {
        let (dir, mut store, device) = enrolled();
        store
            .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
            .unwrap();
        let invocation = uuid::Uuid::from_u128(71);
        let (caller, request) = prepare(&store, device, &invocation.to_string());
        store.commit_agent_request(&request, &caller).unwrap();
        let CommitOperation::Agent { change } = request.operation else {
            unreachable!()
        };
        let phases = if terminal == AgentControlPhase::Unknown {
            vec![terminal]
        } else {
            vec![
                AgentControlPhase::Launching,
                AgentControlPhase::Running,
                terminal,
            ]
        };
        for phase in phases {
            let caller = store.agent_caller(device).unwrap();
            let mut ids = change.ids.clone();
            ids.mutation = format!("{invocation}:{phase:?}");
            let outcome = AgentChange::new(
                &caller,
                ids,
                AgentMutation::Transition {
                    phase,
                    error: (phase == AgentControlPhase::Unknown).then(|| AgentError {
                        code: qos_agent_control_protocol::AgentErrorCode::OutcomeUncertain,
                        message: "Original uncertainty".into(),
                    }),
                },
            )
            .unwrap();
            store
                .commit_agent_request(
                    &OwnerCommitRequest {
                        room: store.state.room.clone(),
                        operation: CommitOperation::Agent {
                            change: Box::new(outcome),
                        },
                    },
                    &caller,
                )
                .unwrap();
        }
        let legacy =
            serde_json::to_value(store.state.private_agent_ledger.as_ref().unwrap()).unwrap();
        store
            .activate_private_agent_runtime_v2(&store.agent_caller(device).unwrap())
            .unwrap();
        let selected = serde_json::to_value(&store.state).unwrap();
        assert_eq!(selected["private_agent_ledger"]["legacy_v1"], legacy);
        drop(store);
        let store = Store::open(dir.path(), "agent-admission").unwrap();
        let observed = store
            .observe_agent_runtime(
                &store.agent_caller(device).unwrap(),
                &AgentControlId::new(invocation).unwrap(),
            )
            .unwrap();
        assert!(
            !observed.qualified_completed,
            "legacy Completed is not native recipient evidence"
        );
        assert_eq!(
            observed.phase,
            if terminal == AgentControlPhase::Completed {
                AgentRuntimePhase::Completed
            } else {
                AgentRuntimePhase::Unknown
            }
        );
    }
}
