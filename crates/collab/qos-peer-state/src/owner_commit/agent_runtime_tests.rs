use super::*;

const V1: &str = r#"{"version":1,"records":{},"mutations":{}}"#;
const V2: &str = r#"{"version":2,"writer_floor":2,"legacy_v1":{"version":1,"records":{},"mutations":{}},"facts":{}}"#;
const ROOM: &str = "00000000-0000-0000-0000-000000000001";

#[test]
fn agent_runtime_v1_literal_and_explicit_v2_reader() {
    let old: PrivateAgentLedger = serde_json::from_str(V1).unwrap();
    assert_eq!(serde_json::to_string(&old).unwrap(), V1);
    let old_reader: Result<PrivateAgentLedger, _> = serde_json::from_str(V2);
    assert!(old_reader.is_err(), "v1 reader must refuse v2");
    for bytes in [V1, V2] {
        let selected: PrivateAgentState = serde_json::from_str(bytes).unwrap();
        selected.validate(ROOM, 1).unwrap();
        assert_eq!(serde_json::to_string(&selected).unwrap(), bytes);
    }
}

#[test]
fn agent_runtime_strict_version_floor_shape_and_duplicates() {
    for bytes in [
        V2.replace("\"writer_floor\":2", "\"writer_floor\":1"),
        V2.replace("\"writer_floor\":2,", ""),
        V2.replacen("\"version\":2", "\"version\":3", 1),
        V2.replacen("\"version\":2", "\"version\":2,\"version\":2", 1),
        V2.replace("\"records\":{}", "\"records\":{},\"records\":{}"),
        V2.replace("\"facts\":{}", "\"facts\":{},\"surprise\":null"),
        V2.replace("\"mutations\":{}", "\"mutations\":{},\"surprise\":null"),
    ] {
        let decoded = serde_json::from_str::<PrivateAgentState>(&bytes);
        assert!(
            decoded
                .and_then(|v| v.validate(ROOM, 1).map_err(serde::de::Error::custom))
                .is_err(),
            "accepted {bytes}"
        );
    }
}

fn runtime_admission() -> PrivateAgentLedgerV2 {
    let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
    let mut caller = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Frozen private caller".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut caller, &root).unwrap();
    let id = |n| uuid::Uuid::from_u128(n).to_string();
    let request = qos_agent_control_protocol::canonical_request(
        &qos_agent_control_protocol::AgentRequest::Spawn {
            version: 1,
            name: "Private agent".into(),
            task: "Retained private task".into(),
            grant_set_id: id(8),
        },
    )
    .unwrap();
    let mut change = AgentRuntimeChange {
        mutation: format!("{}:admitted", id(2)),
        invocation: id(2),
        expected_generation: 1,
        expected_governance: "a".repeat(64),
        payload_digest: [0; 32],
        fact: AgentRuntimeFact::Admitted {
            ids: AgentExecutionIdsV2 {
                agent_id: id(3),
                task_id: id(4),
                run_id: id(5),
                launch_id: id(6),
                message_id: id(7),
            },
            intent: AgentIntentV2 {
                scope: AgentInvocationScopeV2 {
                    machine: "fixture".into(),
                    room: ROOM.into(),
                    host: ROOM.into(),
                    caller,
                },
                route: AgentProviderRouteV2 {
                    app_id: "so.tilo.agent".into(),
                    app_peer: "1".into(),
                    cluster_id: ROOM.into(),
                    record_id: String::new(),
                    resource: format!("workspace:{ROOM}:agents"),
                    resource_kind: "workspace.agents".into(),
                    action: "spawn".into(),
                },
                descriptor_revision: "b".repeat(64),
                request: String::from_utf8(request).unwrap(),
                grants: vec![GrantAction {
                    app_id: "dev.qos.word-processor".into(),
                    record_id: "tab:0000000000000001".into(),
                    resource: "document".into(),
                    action: "insert".into(),
                }],
            },
            parent_invocation: None,
            reservation: AgentReservationV2::new(true),
            timestamp_ms: 1,
        },
    };
    change.payload_digest = *blake3::hash(&change.business_bytes().unwrap()).as_bytes();
    let binding = OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::AgentRuntime {
            change: Box::new(change.clone()),
        },
    }
    .binding()
    .unwrap();
    PrivateAgentLedgerV2 {
        version: 2,
        writer_floor: 2,
        legacy_v1: PrivateAgentLedger::default(),
        facts: std::collections::BTreeMap::from([(
            change.mutation.clone(),
            AgentRuntimeFactReceipt {
                binding,
                generation: 2,
                change,
            },
        )]),
    }
}

#[test]
fn agent_runtime_nonempty_admission_reader_preserves_private_intent() {
    let ledger = runtime_admission();
    ledger.validate(ROOM, 2).unwrap();
    let bytes = serde_json::to_string(&ledger).unwrap();
    let selected: PrivateAgentState = serde_json::from_str(&bytes).unwrap();
    selected.validate(ROOM, 2).unwrap();
    assert_eq!(serde_json::to_string(&selected).unwrap(), bytes);
    let missing_parent = bytes.replace("\"parent_invocation\":null,", "");
    assert!(serde_json::from_str::<PrivateAgentState>(&missing_parent).is_err());
}

fn rebind(receipt: &mut AgentRuntimeFactReceipt) {
    receipt.change.payload_digest =
        *blake3::hash(&receipt.change.business_bytes().unwrap()).as_bytes();
    receipt.binding = OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::AgentRuntime {
            change: Box::new(receipt.change.clone()),
        },
    }
    .binding()
    .unwrap();
}
fn append_fact(ledger: &mut PrivateAgentLedgerV2, fact: AgentRuntimeFact) {
    append_for(ledger, uuid::Uuid::from_u128(2).to_string(), fact);
}
fn append_for(ledger: &mut PrivateAgentLedgerV2, invocation: String, fact: AgentRuntimeFact) {
    let previous = ledger.facts.values().map(|r| r.generation).max().unwrap();
    let mut receipt = AgentRuntimeFactReceipt {
        binding: ledger.facts.values().next().unwrap().binding.clone(),
        generation: previous + 1,
        change: AgentRuntimeChange {
            mutation: fact.mutation_key(&invocation),
            invocation,
            expected_generation: previous,
            expected_governance: "a".repeat(64),
            payload_digest: [0; 32],
            fact,
        },
    };
    rebind(&mut receipt);
    assert!(
        ledger
            .facts
            .insert(receipt.change.mutation.clone(), receipt)
            .is_none()
    );
}
pub(super) fn complete_history() -> PrivateAgentLedgerV2 {
    let mut ledger = runtime_admission();
    let id = |n| uuid::Uuid::from_u128(n).to_string();
    let binding = ManagedBindingFactsV2 {
        room: ROOM.into(),
        app: "so.tilo.agent".into(),
        agent: id(3),
        task: id(4),
        run: id(5),
        launch: id(6),
        generation: 3,
    };
    let runtime = ManagedRuntimeFactsV2 {
        runtime_epoch: id(9),
        package_id: "so.tilo.agent".into(),
        package_hash: "a".repeat(64),
        executable_hash: "b".repeat(64),
        runtime_hash: "c".repeat(64),
    };
    append_fact(
        &mut ledger,
        AgentRuntimeFact::LaunchIntended {
            binding: binding.clone(),
            runtime: runtime.clone(),
        },
    );
    let process = ChannelProcessFactsV2 {
        binding: binding.clone(),
        runtime: runtime.clone(),
        process: ProcessIdentityV2 {
            pid: 100,
            start: 1000,
        },
        supervisor: ProcessIdentityV2 {
            pid: 99,
            start: 999,
        },
        channel_id: id(10),
    };
    append_fact(
        &mut ledger,
        AgentRuntimeFact::ProcessBound {
            launch_fact: format!("{}:launch_intended", id(2)),
            process,
        },
    );
    let envelope = format!(
        r#"{{"version":1,"message_id":"{}","task_id":"{}","run_id":"{}","launch_id":"{}","runtime_epoch":"{}","recipient_agent":"{}","channel_lease":"{}","task":{{"version":1,"task_id":"{}","title":"Retained private task","run_id":1,"lease":"{}","planning":"model","action_context":{{"group":{{"id":"{}","kind":"task","label":"{}"}},"purpose":null}}}}}}"#,
        id(7),
        id(4),
        id(5),
        id(6),
        id(9),
        id(3),
        id(10),
        id(4),
        id(11),
        id(4),
        id(4)
    );
    append_fact(
        &mut ledger,
        AgentRuntimeFact::DispatchIntended {
            process_fact: format!("{}:process_bound", id(2)),
            envelope,
        },
    );
    append_fact(
        &mut ledger,
        AgentRuntimeFact::Completed {
            dispatch_fact: format!("{}:dispatch_intended", id(2)),
            acceptance: RecipientAcceptanceV2 {
                version: 1,
                message_id: id(7),
                task_id: id(4),
                run_id: id(5),
                launch_id: id(6),
                runtime_epoch: id(9),
                recipient_agent: id(3),
                channel_lease: id(10),
                acceptance_sequence: 1,
            },
            timestamp_ms: 2,
        },
    );
    append_fact(
        &mut ledger,
        AgentRuntimeFact::TaskFinished {
            completed_fact: format!("{}:completed", id(2)),
            acceptance_sequence: 1,
            update_sequence: 2,
            outcome: TaskOutcomeV2::Succeeded,
            text: "Retained result".into(),
            timestamp_ms: 3,
        },
    );
    append_fact(
        &mut ledger,
        AgentRuntimeFact::ProcessExited {
            launch_fact: format!("{}:launch_intended", id(2)),
            exit: SupervisorExitV2 {
                runtime,
                binding,
                supervisor: ProcessIdentityV2 {
                    pid: 99,
                    start: 999,
                },
                exit_code: Some(0),
                signal: None,
            },
            timestamp_ms: 4,
        },
    );
    ledger
}
#[test]
fn agent_runtime_full_retained_history_and_exact_evidence_references() {
    let ledger = complete_history();
    ledger.validate(ROOM, 8).unwrap();
    let wire = serde_json::to_string(&ledger).unwrap();
    let selected: PrivateAgentState = serde_json::from_str(&wire).unwrap();
    selected.validate(ROOM, 8).unwrap();
    assert_eq!(serde_json::to_string(&selected).unwrap(), wire);
    let invocation = uuid::Uuid::from_u128(2).to_string();
    let mut missing = ledger.clone();
    missing.facts.remove(&format!("{invocation}:process_bound"));
    assert!(missing.validate(ROOM, 8).is_err());
    let mut wrong = ledger.clone();
    let receipt = wrong
        .facts
        .get_mut(&format!("{invocation}:completed"))
        .unwrap();
    let AgentRuntimeFact::Completed { acceptance, .. } = &mut receipt.change.fact else {
        unreachable!()
    };
    acceptance.channel_lease = uuid::Uuid::from_u128(12).to_string();
    rebind(receipt);
    assert!(
        wrong.validate(ROOM, 8).is_err(),
        "a recomputed digest cannot substitute recipient custody"
    );
    let mut reserved = ledger.clone();
    let receipt = reserved
        .facts
        .get_mut(&format!("{invocation}:admitted"))
        .unwrap();
    let AgentRuntimeFact::Admitted { reservation, .. } = &mut receipt.change.fact else {
        unreachable!()
    };
    reservation.control_bytes = 0;
    rebind(receipt);
    assert!(reserved.validate(ROOM, 8).is_err());
    let mut regressed = ledger;
    append_fact(
        &mut regressed,
        AgentRuntimeFact::Unknown {
            error: AgentError {
                code: qos_agent_control_protocol::AgentErrorCode::OutcomeUncertain,
                message: "late uncertainty".into(),
            },
            timestamp_ms: 5,
        },
    );
    assert!(
        regressed.validate(ROOM, 9).is_err(),
        "terminal control cannot regress"
    );
}
#[test]
fn agent_runtime_unknown_never_authorizes_an_execution_edge() {
    let mut ledger = runtime_admission();
    append_fact(
        &mut ledger,
        AgentRuntimeFact::Unknown {
            error: AgentError {
                code: qos_agent_control_protocol::AgentErrorCode::OutcomeUncertain,
                message: "host restarted".into(),
            },
            timestamp_ms: 2,
        },
    );
    ledger.validate(ROOM, 3).unwrap();
    let history = complete_history();
    let AgentRuntimeFact::LaunchIntended { binding, runtime } = &history.facts
        [&format!("{}:launch_intended", uuid::Uuid::from_u128(2))]
        .change
        .fact
    else {
        unreachable!()
    };
    let mut binding = binding.clone();
    binding.generation = 4;
    append_fact(
        &mut ledger,
        AgentRuntimeFact::LaunchIntended {
            binding,
            runtime: runtime.clone(),
        },
    );
    assert!(
        ledger.validate(ROOM, 4).is_err(),
        "unknown must not mint another launch intent"
    );
}
#[test]
fn agent_runtime_diagnostics_and_nullable_fields_are_strict_on_restore() {
    let mut ledger = runtime_admission();
    append_fact(
        &mut ledger,
        AgentRuntimeFact::Unknown {
            error: AgentError {
                code: qos_agent_control_protocol::AgentErrorCode::UnknownInvocation,
                message: "invalid absent operation".into(),
            },
            timestamp_ms: 2,
        },
    );
    assert!(ledger.validate(ROOM, 3).is_err());
    let complete = complete_history();
    let bytes = serde_json::to_string(&complete).unwrap();
    assert!(
        serde_json::from_str::<PrivateAgentState>(
            &bytes
                .replace("\"signal\":null,", "")
                .replace(",\"signal\":null", "")
        )
        .is_err()
    );
}

pub(super) fn frozen_v1(terminal: AgentControlPhase) -> PrivateAgentLedger {
    let source = runtime_admission();
    let admission = source.facts.values().next().unwrap();
    let AgentRuntimeFact::Admitted {
        ids: runtime_ids,
        intent,
        ..
    } = &admission.change.fact
    else {
        unreachable!()
    };
    let invocation = admission.change.invocation.clone();
    let ids = AgentIds {
        mutation: format!("{invocation}:prepare"),
        invocation: invocation.clone(),
        agent: runtime_ids.agent_id.clone(),
        task: runtime_ids.task_id.clone(),
        run: runtime_ids.run_id.clone(),
    };
    let mut ledger = PrivateAgentLedger::default();
    let mut record = PrivateAgentRecord {
        ids: ids.clone(),
        invocation: invocation.clone(),
        caller: intent.scope.caller.clone(),
        request: intent.request.as_bytes().to_vec(),
        grants: intent.grants.clone(),
        phase: AgentControlPhase::Admitted,
        admitted_generation: 2,
        last_generation: 2,
        error: None,
    };
    let mut changes = vec![AgentMutation::Prepare {
        invocation: invocation.clone(),
        request: record.request.clone(),
        grants: record.grants.clone(),
    }];
    match terminal {
        AgentControlPhase::Admitted => (),
        AgentControlPhase::Unknown => changes.push(AgentMutation::Transition {
            phase: AgentControlPhase::Unknown,
            error: Some(AgentError {
                code: qos_agent_control_protocol::AgentErrorCode::OutcomeUncertain,
                message: "Retained uncertainty".into(),
            }),
        }),
        AgentControlPhase::Completed => {
            for phase in [
                AgentControlPhase::Launching,
                AgentControlPhase::Running,
                AgentControlPhase::Completed,
            ] {
                changes.push(AgentMutation::Transition { phase, error: None });
            }
        }
        _ => unreachable!(),
    }
    for (index, mutation) in changes.into_iter().enumerate() {
        let generation = index as u64 + 2;
        let mut change = AgentChange {
            ids: ids.clone(),
            caller: record.caller.clone(),
            expected_generation: generation - 1,
            expected_governance: "a".repeat(64),
            payload_digest: [0; 32],
            mutation,
        };
        if let AgentMutation::Transition { phase, error } = &change.mutation {
            change.ids.mutation = format!("{invocation}:{phase:?}");
            record.phase = *phase;
            record.error = error.clone();
            record.last_generation = generation;
        }
        change.payload_digest = *blake3::hash(&change.business_bytes().unwrap()).as_bytes();
        let binding = OwnerCommitRequest {
            room: ROOM.into(),
            operation: CommitOperation::Agent {
                change: Box::new(change.clone()),
            },
        }
        .binding()
        .unwrap();
        ledger.mutations.insert(
            change.ids.mutation.clone(),
            AgentMutationReceipt {
                binding,
                generation,
                invocation: invocation.clone(),
                change,
            },
        );
    }
    ledger.records.insert(invocation, record);
    ledger.validate(ROOM, 5).unwrap();
    ledger
}

#[test]
fn agent_runtime_frozen_v1_and_v2_literal_bytes() {
    for (literal, generation) in [
        (include_str!("fixtures/agent-v1-admitted.json"), 2),
        (include_str!("fixtures/agent-v1-unknown.json"), 3),
        (include_str!("fixtures/agent-v1-completed.json"), 5),
        (include_str!("fixtures/agent-v2-admitted.json"), 2),
        (include_str!("fixtures/agent-v2-completed.json"), 8),
    ] {
        let decoded: PrivateAgentState = serde_json::from_str(literal).unwrap();
        decoded.validate(ROOM, generation).unwrap();
        assert_eq!(serde_json::to_string(&decoded).unwrap(), literal.trim_end());
    }
    for (phase, literal) in [
        (
            AgentControlPhase::Admitted,
            include_str!("fixtures/agent-v1-admitted.json"),
        ),
        (
            AgentControlPhase::Unknown,
            include_str!("fixtures/agent-v1-unknown.json"),
        ),
        (
            AgentControlPhase::Completed,
            include_str!("fixtures/agent-v1-completed.json"),
        ),
    ] {
        assert_eq!(
            serde_json::to_string(&frozen_v1(phase)).unwrap(),
            literal.trim_end()
        );
    }
    assert_eq!(
        postcard::to_allocvec(&CommitTarget::Agent {
            mutation: "x".into()
        })
        .unwrap(),
        b"\x03\x01x"
    );
    assert_eq!(
        postcard::to_allocvec(&CommitTarget::AgentRuntime {
            mutation: "x".into()
        })
        .unwrap(),
        b"\x04\x01x"
    );
}

#[test]
fn agent_runtime_message_binds_original_process_and_exit_fences_late_acceptance() {
    let mut ledger = complete_history();
    let id = |n| uuid::Uuid::from_u128(n).to_string();
    let exit = ledger
        .facts
        .remove(&format!("{}:process_exited", id(2)))
        .unwrap()
        .change
        .fact;
    let AgentRuntimeFact::Admitted { intent, .. } =
        &ledger.facts[&format!("{}:admitted", id(2))].change.fact
    else {
        unreachable!()
    };
    let mut intent = intent.clone();
    intent.route.action = "message".into();
    intent.request = String::from_utf8(
        qos_agent_control_protocol::canonical_request(
            &qos_agent_control_protocol::AgentRequest::Message {
                version: 1,
                agent_id: id(3),
                text: "Follow up".into(),
            },
        )
        .unwrap(),
    )
    .unwrap();
    append_for(
        &mut ledger,
        id(20),
        AgentRuntimeFact::Admitted {
            ids: AgentExecutionIdsV2 {
                agent_id: id(3),
                task_id: id(21),
                run_id: id(22),
                launch_id: id(6),
                message_id: id(23),
            },
            intent,
            parent_invocation: Some(id(2)),
            reservation: AgentReservationV2::new(false),
            timestamp_ms: 5,
        },
    );
    let envelope = format!(
        r#"{{"version":1,"message_id":"{}","task_id":"{}","run_id":"{}","launch_id":"{}","runtime_epoch":"{}","recipient_agent":"{}","channel_lease":"{}","task":{{"version":1,"task_id":"{}","title":"Follow up","run_id":2,"lease":"{}","planning":"model","action_context":{{"group":{{"id":"{}","kind":"task","label":"{}"}},"purpose":null}}}}}}"#,
        id(23),
        id(21),
        id(22),
        id(6),
        id(9),
        id(3),
        id(10),
        id(21),
        id(24),
        id(21),
        id(21)
    );
    append_for(
        &mut ledger,
        id(20),
        AgentRuntimeFact::DispatchIntended {
            process_fact: format!("{}:process_bound", id(2)),
            envelope,
        },
    );
    let accepted = AgentRuntimeFact::Completed {
        dispatch_fact: format!("{}:dispatch_intended", id(20)),
        acceptance: RecipientAcceptanceV2 {
            version: 1,
            message_id: id(23),
            task_id: id(21),
            run_id: id(22),
            launch_id: id(6),
            runtime_epoch: id(9),
            recipient_agent: id(3),
            channel_lease: id(10),
            acceptance_sequence: 2,
        },
        timestamp_ms: 6,
    };
    let mut stopped = ledger.clone();
    append_fact(&mut stopped, exit);
    append_for(&mut stopped, id(20), accepted.clone());
    assert!(
        stopped.validate(ROOM, 11).is_err(),
        "parent exit must fence message completion too"
    );
    append_for(&mut ledger, id(20), accepted);
    ledger.validate(ROOM, 10).unwrap();
    let mut skipped = ledger.clone();
    let receipt = skipped
        .facts
        .get_mut(&format!("{}:completed", id(20)))
        .unwrap();
    let AgentRuntimeFact::Completed { acceptance, .. } = &mut receipt.change.fact else {
        unreachable!()
    };
    acceptance.acceptance_sequence = 3;
    rebind(receipt);
    assert!(
        skipped.validate(ROOM, 10).is_err(),
        "complete retained history cannot skip recipient sequence 2"
    );
    let mut wrong = ledger;
    let receipt = wrong
        .facts
        .get_mut(&format!("{}:admitted", id(20)))
        .unwrap();
    let AgentRuntimeFact::Admitted {
        parent_invocation, ..
    } = &mut receipt.change.fact
    else {
        unreachable!()
    };
    *parent_invocation = Some(id(77));
    rebind(receipt);
    assert!(wrong.validate(ROOM, 10).is_err());
}

fn replace_context(ledger: &mut PrivateAgentLedgerV2, context: serde_json::Value) {
    let key = format!("{}:dispatch_intended", uuid::Uuid::from_u128(2));
    let receipt = ledger.facts.get_mut(&key).unwrap();
    let AgentRuntimeFact::DispatchIntended { envelope, .. } = &mut receipt.change.fact else {
        unreachable!()
    };
    let decoded: serde_json::Value = serde_json::from_str(envelope).unwrap();
    let old = serde_json::to_string(&decoded["task"]["action_context"]).unwrap();
    let replacement = serde_json::to_string(&context).unwrap();
    *envelope = envelope.replacen(
        &format!("\"action_context\":{old}"),
        &format!("\"action_context\":{replacement}"),
        1,
    );
    rebind(receipt);
}

#[test]
fn agent_runtime_review_strict_nested_action_context() {
    let valid = complete_history();
    valid.validate(ROOM, 8).unwrap();
    let key = format!("{}:dispatch_intended", uuid::Uuid::from_u128(2));
    let AgentRuntimeFact::DispatchIntended { envelope, .. } = &valid.facts[&key].change.fact else {
        unreachable!()
    };
    let decoded: serde_json::Value = serde_json::from_str(envelope).unwrap();
    let context = decoded["task"]["action_context"].clone();
    let mut expanded = context.clone();
    expanded["action_label"] = serde_json::json!("Task");
    expanded["app"] = serde_json::json!({"id":"so.tilo.agent","label":"Agent"});
    expanded["resource"] = serde_json::json!({"id":"task","kind":"task","label":"Task"});
    expanded["facets"] = serde_json::json!([{"key":"task","value":"opaque-id"}]);
    let mut accepted = valid.clone();
    replace_context(&mut accepted, expanded);
    accepted.validate(ROOM, 8).unwrap();
    for (field, value) in [
        ("facets", serde_json::json!(0)),
        ("action_label", serde_json::json!(false)),
        ("surprise", serde_json::json!(null)),
        (
            "app",
            serde_json::json!({"id":"so.tilo.agent","label":"Agent","surprise":null}),
        ),
        (
            "resource",
            serde_json::json!({"id":"task","kind":0,"label":"Task"}),
        ),
        (
            "facets",
            serde_json::json!([{"key":"task","value":"id","surprise":null}]),
        ),
    ] {
        let mut context = context.clone();
        context[field] = value;
        let mut bad = valid.clone();
        replace_context(&mut bad, context);
        assert!(
            bad.validate(ROOM, 8).is_err(),
            "accepted malformed/unknown {field}"
        );
    }
    let mut context = context;
    context["group"]["surprise"] = serde_json::json!(null);
    let mut bad = valid;
    replace_context(&mut bad, context);
    assert!(
        bad.validate(ROOM, 8).is_err(),
        "accepted unknown group field"
    );
}

#[test]
fn agent_runtime_review_first_acceptance_is_one_and_bounded() {
    let valid = complete_history();
    valid.validate(ROOM, 8).unwrap();
    for sequence in [2, 1025, u64::MAX] {
        let mut bad = valid.clone();
        for receipt in bad.facts.values_mut() {
            match &mut receipt.change.fact {
                AgentRuntimeFact::Completed { acceptance, .. } => {
                    acceptance.acceptance_sequence = sequence
                }
                AgentRuntimeFact::TaskFinished {
                    acceptance_sequence,
                    ..
                } => *acceptance_sequence = sequence,
                _ => continue,
            }
            rebind(receipt);
        }
        assert!(
            bad.validate(ROOM, 8).is_err(),
            "accepted first sequence {sequence}"
        );
    }
}

#[test]
fn agent_runtime_grant_resource_bound_alias_roundtrip_and_field_limits() {
    let mut ledger = runtime_admission();
    let resource = format!(
        "tilo-ui-bound:v1:00000000-0000-0000-0000-000000000001:00000000-0000-0000-0000-000000000002:{}",
        "636861742e63617465676f72793a6361745f686f6d655f74657874"
    );
    assert!(resource.len() > 128);
    let receipt = ledger.facts.values_mut().next().unwrap();
    let AgentRuntimeFact::Admitted { intent, .. } = &mut receipt.change.fact else {
        panic!()
    };
    intent.grants[0].record_id = "@qos:package-primary".into();
    intent.grants[0].resource = resource;
    intent
        .validate(ROOM)
        .expect("actual component-qualified grant must be a valid retained intent");
    let original = intent.clone();
    for (resource, accepted) in [
        ("a".repeat(4096), true),
        ("a".repeat(4097), false),
        (String::new(), false),
        ("é".into(), false),
        (" padded".into(), false),
        ("padded ".into(), false),
        ("a\tb".into(), false),
        ("a\0b".into(), false),
        ("a\u{7f}b".into(), false),
    ] {
        let mut candidate = original.clone();
        candidate.grants[0].resource = resource;
        assert_eq!(candidate.validate(ROOM).is_ok(), accepted);
    }
    for field in 0..3 {
        let mut candidate = original.clone();
        let grant = &mut candidate.grants[0];
        let value = match field {
            0 => &mut grant.app_id,
            1 => &mut grant.record_id,
            _ => &mut grant.action,
        };
        *value = "a".repeat(129);
        assert!(
            candidate.validate(ROOM).is_err(),
            "resource expansion widened a different identity"
        );
    }
    let mut candidate = original.clone();
    candidate.grants = (0..64)
        .map(|i| {
            let mut grant = original.grants[0].clone();
            grant.resource = "a".repeat(4096);
            grant.action = format!("action-{i}");
            grant
        })
        .collect();
    assert!(candidate.validate(ROOM).is_ok());
    candidate.grants.push(original.grants[0].clone());
    assert!(
        candidate.validate(ROOM).is_err(),
        "resource expansion widened grant count"
    );
    rebind(receipt);
    ledger.validate(ROOM, 2).unwrap();
    let bytes = serde_json::to_string(&ledger).unwrap();
    let restored: PrivateAgentState = serde_json::from_str(&bytes).unwrap();
    restored.validate(ROOM, 2).unwrap();
    assert_eq!(
        serde_json::to_string(&restored).unwrap(),
        bytes,
        "reader acceptance must not rewrite ledger bytes"
    );
}

fn accepted_history() -> PrivateAgentLedgerV2 {
    let mut ledger = complete_history();
    let invocation = uuid::Uuid::from_u128(2).to_string();
    ledger.facts.remove(&format!("{invocation}:task_finished"));
    ledger.facts.remove(&format!("{invocation}:process_exited"));
    ledger
}
fn generation(ledger: &PrivateAgentLedgerV2) -> u64 {
    ledger.facts.values().map(|r| r.generation).max().unwrap()
}
fn bound(step: u32, child: u8) -> AgentRuntimeFact {
    AgentRuntimeFact::TaskStepBound {
        dispatch_fact: format!("{}:dispatch_intended", uuid::Uuid::from_u128(2)),
        step,
        child: hex::encode([child; 48]),
        intent: hex::encode([7; 32]),
    }
}
fn committed(step: u32, json: &str) -> AgentRuntimeFact {
    AgentRuntimeFact::TaskStepCommitted {
        bound_fact: format!("{}:task_step_bound:{step}", uuid::Uuid::from_u128(2)),
        step,
        result: hex::encode(*blake3::hash(json.as_bytes()).as_bytes()),
        result_json: json.into(),
        continuation: hex::encode(format!("next:{}", step + 1)),
    }
}
fn finished(outcome: TaskOutcomeV2) -> AgentRuntimeFact {
    AgentRuntimeFact::TaskFinished {
        completed_fact: format!("{}:completed", uuid::Uuid::from_u128(2)),
        acceptance_sequence: 1,
        update_sequence: 2,
        outcome,
        text: "Retained result".into(),
        timestamp_ms: 3,
    }
}
fn with(
    ledger: &PrivateAgentLedgerV2,
    fact: AgentRuntimeFact,
) -> Result<PrivateAgentLedgerV2, String> {
    let mut next = ledger.clone();
    append_fact(&mut next, fact);
    next.validate(ROOM, generation(&next)).map(|()| next)
}
#[test]
fn agent_runtime_task_steps_bind_commit_in_order_and_gate_success() {
    let accepted = accepted_history();
    accepted.validate(ROOM, generation(&accepted)).unwrap();
    // Commit before bind, or a step other than the next, is refused.
    assert!(with(&accepted, committed(0, "{}")).is_err());
    assert!(with(&accepted, bound(1, 1)).is_err());
    let one = with(&accepted, bound(0, 1)).unwrap();
    assert_eq!(
        one.facts
            .keys()
            .filter(|k| k.ends_with(":task_step_bound:0"))
            .count(),
        1
    );
    // A second step cannot bind while the first is uncommitted.
    assert!(with(&one, bound(1, 2)).is_err());
    // Success cannot be recorded over a bound uncommitted step; failure can.
    assert!(with(&one, finished(TaskOutcomeV2::Succeeded)).is_err());
    let failed = with(&one, finished(TaskOutcomeV2::Failed)).unwrap();
    // The open step's exact result may still settle after a failed task.
    with(&failed, committed(0, r#"{"ok":true}"#)).unwrap();
    // Invalid result JSON, duplicate keys and oversize are refused.
    assert!(with(&one, committed(0, "{not json")).is_err());
    assert!(with(&one, committed(0, r#"{"a":1,"a":2}"#)).is_err());
    let big = format!("\"{}\"", "x".repeat(MAX_AGENT_STEP_RESULT_BYTES));
    assert!(with(&one, committed(0, &big)).is_err());
    let settled = with(&one, committed(0, r#"{"ok":true}"#)).unwrap();
    // The same child cannot be bound to another step.
    assert!(with(&settled, bound(1, 1)).is_err());
    let two = with(&settled, bound(1, 2)).unwrap();
    assert!(with(&two, finished(TaskOutcomeV2::Succeeded)).is_err());
    let done = with(&two, committed(1, r#"{"ok":2}"#)).unwrap();
    let done = with(&done, finished(TaskOutcomeV2::Succeeded)).unwrap();
    // No new step after the task finished.
    assert!(with(&done, bound(2, 3)).is_err());
    // Canonical private state round trip.
    let wire = serde_json::to_string(&PrivateAgentState::V2(done.clone())).unwrap();
    let back: PrivateAgentState = serde_json::from_str(&wire).unwrap();
    back.validate(ROOM, generation(&done)).unwrap();
    assert_eq!(serde_json::to_string(&back).unwrap(), wire);
    // A step fact under a singleton key is not a valid change identity.
    let key = format!("{}:task_step_committed:0", uuid::Uuid::from_u128(2));
    let mut change = settled.facts[&key].change.clone();
    change.mutation = format!("{}:task_step_committed", uuid::Uuid::from_u128(2));
    change.payload_digest = *blake3::hash(&change.business_bytes().unwrap()).as_bytes();
    assert!(change.validate().is_err());
}
