use super::*;
use crate::native_footprint_vendor_tests::assert_precharged;
use crate::{PeerLimits, PeerRegistry};
use automerge::resource_limits::NativeAllowance;
const ROOM: &str = "00000000-0000-0000-0000-000000000001";
#[test]
fn private_validation_paid_actual_v1_and_complete_v2() {
    let fixtures = [
        PrivateAgentState::V1(crate::owner_commit::agent_runtime_tests::frozen_v1(
            AgentControlPhase::Unknown,
        )),
        PrivateAgentState::V2(crate::owner_commit::agent_runtime_tests::complete_history()),
    ];
    for state in fixtures {
        state.validate(ROOM, 8).unwrap();
        let initial = NativeAllowance::new(4096).unwrap();
        let mut registry = PeerRegistry::new(initial.clone()).unwrap();
        let before = initial.retained_bytes();
        let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
        let mut turn = registry
            .begin_turn(current.clone(), PeerLimits::default())
            .unwrap();
        let work = turn.work().unwrap();
        assert_precharged(&current, || state.validate_paid(ROOM, 8, &work)).unwrap();
        assert_eq!(initial.retained_bytes(), before);
    }
}

fn measured(state: &PrivateAgentState, room: &str, generation: u64) {
    let expected = state.validate(room, generation);
    let initial = NativeAllowance::new(4096).unwrap();
    let mut registry = PeerRegistry::new(initial.clone()).unwrap();
    let baseline = initial.retained_bytes();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let work = turn.work().unwrap();
    let current_before = current.retained_bytes();
    let actual = assert_precharged(&current, || state.validate_paid(room, generation, &work));
    assert_eq!(
        actual.as_ref().map(|_| ()).map_err(|e| e.diagnostic()),
        expected.as_ref().map(|_| ()).map_err(|e| Some(e.as_str()))
    );
    assert_eq!(initial.retained_bytes(), baseline);
    drop(actual);
    assert_eq!(current.retained_bytes(), current_before);
}
fn rebind(r: &mut AgentRuntimeFactReceipt) {
    r.change.payload_digest = *blake3::hash(&r.change.business_bytes().unwrap()).as_bytes();
    r.binding = OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::AgentRuntime {
            change: Box::new(r.change.clone()),
        },
    }
    .binding()
    .unwrap();
}
fn change_envelope(mut f: impl FnMut(&mut String)) -> PrivateAgentState {
    let mut v = crate::owner_commit::agent_runtime_tests::complete_history();
    for r in v.facts.values_mut() {
        if let AgentRuntimeFact::DispatchIntended { envelope, .. } = &mut r.change.fact {
            f(envelope);
            rebind(r);
        }
    }
    PrivateAgentState::V2(v)
}
#[test]
fn private_validation_paid_errors_keep_original_order_and_credit() {
    let mut v = crate::owner_commit::agent_runtime_tests::complete_history();
    v.writer_floor = 1;
    v.legacy_v1.version = 9;
    measured(&PrivateAgentState::V2(v), ROOM, 8);
    let mut v = crate::owner_commit::agent_runtime_tests::complete_history();
    v.legacy_v1 = crate::owner_commit::agent_runtime_tests::frozen_v1(AgentControlPhase::Unknown);
    v.legacy_v1
        .records
        .values_mut()
        .next()
        .unwrap()
        .caller
        .signature = [0; 64];
    v.facts.values_mut().next().unwrap().generation = 0;
    measured(&PrivateAgentState::V2(v), ROOM, 8);
    let mut v = crate::owner_commit::agent_runtime_tests::complete_history();
    v.facts.values_mut().next().unwrap().change.payload_digest = [0; 32];
    measured(&PrivateAgentState::V2(v), ROOM, 8);
    measured(
        &PrivateAgentState::V1(PrivateAgentLedger::default()),
        "not-a-room",
        1,
    );
}
#[test]
fn private_validation_paid_request_json_and_diagnostic_paths() {
    for request in [
        "{\"kind\":\"agent_spawn\",\"version\":1,\"name\":false}".to_string(),
        format!(
            "{{\"{}\":0,\"kind\":\"agent_spawn\"}}",
            "界\\\"".repeat(300)
        ),
        "{\"kind\":\"agent_spawn\",\"version\":1,\"version\":1}".into(),
        "{".into(),
        "x".repeat(65537),
    ] {
        let mut v = crate::owner_commit::agent_runtime_tests::frozen_v1(AgentControlPhase::Unknown);
        v.records.values_mut().next().unwrap().request = request.into_bytes();
        measured(&PrivateAgentState::V1(v), ROOM, 8);
    }
}
#[test]
fn private_validation_paid_envelope_value_copies_full_numbers_and_malformed() {
    for value in [
        "[true,null,{},[],1e100,5e-324]".to_string(),
        format!(
            "{{\"{}\":[{}]}}",
            "escaped\\\"界".repeat(60),
            (0..300)
                .map(|_| "{\"value\":\"retained\\ntext\"}")
                .collect::<Vec<_>>()
                .join(",")
        ),
        format!("1.{}1", "0".repeat(1200)),
        "1e-999999999999999999999999".into(),
        "2.4703282292062327208828439643411068618252990130716238221279284125e-324".into(),
    ] {
        measured(
            &change_envelope(|bytes| {
                let marker = "\"action_context\":";
                let start = bytes.find(marker).unwrap() + marker.len();
                bytes.replace_range(start..bytes.len() - 2, &value);
            }),
            ROOM,
            8,
        );
    }
    for bytes in [
        "{".to_string(),
        "[".repeat(129),
        "{\"x\":0,\"x\":1}".into(),
        "x".repeat(128 * 1024 + 1),
    ] {
        measured(&change_envelope(|e| *e = bytes.clone()), ROOM, 8);
    }
}
#[test]
fn private_validation_paid_error_outlives_work_and_registry_and_pressure_is_retryable() {
    let state = PrivateAgentState::V2(PrivateAgentLedgerV2 {
        version: 2,
        writer_floor: 1,
        legacy_v1: PrivateAgentLedger::default(),
        facts: Default::default(),
    });
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let current = NativeAllowance::new(8 * 1024 * 1024).unwrap();
    let current_baseline = current.retained_bytes();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let work = turn.work().unwrap();
    let pressure = work.reserve_output(current.available_bytes()).unwrap();
    assert!(
        assert_precharged(&current, || state.validate_paid(ROOM, 1, &work))
            .unwrap_err()
            .is_capacity()
    );
    drop(pressure);
    let failure = assert_precharged(&current, || state.validate_paid(ROOM, 1, &work)).unwrap_err();
    let reservation_bytes = cost::validation(&state, ROOM).unwrap();
    assert!(current.retained_bytes() >= current_baseline + reservation_bytes);
    drop(work);
    drop(turn);
    drop(registry);
    assert_eq!(
        current.retained_bytes(),
        current_baseline + reservation_bytes
    );
    assert_eq!(
        failure.diagnostic(),
        Some("unsupported private executor writer floor")
    );
    drop(failure);
    assert_eq!(current.retained_bytes(), current_baseline);
}

fn append(v: &mut PrivateAgentLedgerV2, fact: AgentRuntimeFact) {
    let first = v
        .facts
        .values()
        .find(|r| matches!(r.change.fact, AgentRuntimeFact::Admitted { .. }))
        .unwrap();
    let mut c = first.change.clone();
    c.invocation = first.change.invocation.clone();
    c.mutation = fact.mutation_key(&c.invocation);
    c.expected_generation = v.facts.values().map(|r| r.generation).max().unwrap();
    c.fact = fact;
    c.payload_digest = *blake3::hash(&c.business_bytes().unwrap()).as_bytes();
    let binding = OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::AgentRuntime {
            change: Box::new(c.clone()),
        },
    }
    .binding()
    .unwrap();
    v.facts.insert(
        c.mutation.clone(),
        AgentRuntimeFactReceipt {
            binding,
            generation: c.expected_generation + 1,
            change: c,
        },
    );
}
#[test]
fn private_validation_paid_remaining_fact_variants_and_grant_limits() {
    for fact in [
        AgentRuntimeFact::Failed {
            error: AgentError {
                code: qos_agent_control_protocol::AgentErrorCode::InternalError,
                message: "qualified pre-effect failure".into(),
            },
            timestamp_ms: 1,
        },
        AgentRuntimeFact::Unknown {
            error: AgentError {
                code: qos_agent_control_protocol::AgentErrorCode::OutcomeUncertain,
                message: "uncertain result".into(),
            },
            timestamp_ms: 1,
        },
        AgentRuntimeFact::TaskUnknown {
            error: AgentError {
                code: qos_agent_control_protocol::AgentErrorCode::OutcomeUncertain,
                message: "uncertain task".into(),
            },
            timestamp_ms: 1,
        },
    ] {
        let mut v = crate::owner_commit::agent_runtime_tests::complete_history();
        let floor = if matches!(fact, AgentRuntimeFact::Failed { .. }) {
            2
        } else {
            5
        };
        v.facts.retain(|_, r| r.generation <= floor);
        append(&mut v, fact);
        v.validate(ROOM, 8).unwrap();
        measured(&PrivateAgentState::V2(v), ROOM, 8);
    }
    for (count, bytes) in [(1, 4096), (64, 128), (64, 4096), (65, 128), (1, 4097)] {
        let mut v = crate::owner_commit::agent_runtime_tests::complete_history();
        v.facts.retain(|_, r| r.generation == 2);
        let r = v.facts.values_mut().next().unwrap();
        let AgentRuntimeFact::Admitted { intent, .. } = &mut r.change.fact else {
            unreachable!()
        };
        let grant = intent.grants[0].clone();
        intent.grants = (0..count)
            .map(|i| {
                let mut g = grant.clone();
                g.action = format!("action-{i}");
                g.resource = "r".repeat(bytes);
                g
            })
            .collect();
        rebind(r);
        measured(&PrivateAgentState::V2(v), ROOM, 8);
    }
}
fn many_v1(count: usize) -> PrivateAgentState {
    let template =
        crate::owner_commit::agent_runtime_tests::frozen_v1(AgentControlPhase::Completed);
    let mut out = PrivateAgentLedger::default();
    for n in 0..count {
        let mut r = template.records.values().next().unwrap().clone();
        let prefix = |s: &str| format!("{n}:{s}");
        r.invocation = prefix(&r.invocation);
        r.ids.mutation = prefix(&r.ids.mutation);
        r.ids.invocation = prefix(&r.ids.invocation);
        r.ids.agent = prefix(&r.ids.agent);
        r.ids.task = prefix(&r.ids.task);
        r.ids.run = prefix(&r.ids.run);
        for old in template.mutations.values() {
            let mut m = old.clone();
            m.invocation = r.invocation.clone();
            m.change.ids = r.ids.clone();
            m.change.ids.mutation = prefix(&old.change.ids.mutation);
            if let AgentMutation::Prepare { invocation, .. } = &mut m.change.mutation {
                *invocation = r.invocation.clone();
            }
            m.change.payload_digest = *blake3::hash(&m.change.business_bytes().unwrap()).as_bytes();
            m.binding = OwnerCommitRequest {
                room: ROOM.into(),
                operation: CommitOperation::Agent {
                    change: Box::new(m.change.clone()),
                },
            }
            .binding()
            .unwrap();
            out.mutations.insert(m.change.ids.mutation.clone(), m);
        }
        out.records.insert(r.invocation.clone(), r);
    }
    PrivateAgentState::V1(out)
}
#[test]
fn private_validation_paid_real_tree_splits_and_admission_limit() {
    for count in [0, 1, 5, 11, 23, 127, 1024] {
        let v = many_v1(count);
        v.validate(ROOM, 8).unwrap();
        measured(&v, ROOM, 8);
    }
    let v = many_v1(1025);
    assert!(v.validate(ROOM, 8).is_err());
    measured(&v, ROOM, 8);
}
#[test]
fn private_validation_paid_preflight_is_allocation_free_at_zero_and_exact_budget() {
    let state = PrivateAgentState::V2(crate::owner_commit::agent_runtime_tests::complete_history());
    let initial = NativeAllowance::new(4096).unwrap();
    let mut registry = PeerRegistry::new(initial).unwrap();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let work = turn.work().unwrap();
    let pressure = work.reserve_output(current.available_bytes()).unwrap();
    assert!(
        assert_precharged(&current, || state.validate_paid(ROOM, 8, &work))
            .unwrap_err()
            .is_capacity()
    );
    drop(pressure);
    let needed = cost::validation(&state, ROOM).unwrap();
    let pressure = work
        .reserve_output(current.available_bytes() - needed + 1)
        .unwrap();
    assert!(
        assert_precharged(&current, || state.validate_paid(ROOM, 8, &work))
            .unwrap_err()
            .is_capacity()
    );
    drop(pressure);
    let pressure = work
        .reserve_output(current.available_bytes() - needed)
        .unwrap();
    assert_precharged(&current, || state.validate_paid(ROOM, 8, &work)).unwrap();
    drop(pressure);
}

#[test]
fn private_validation_paid_task_step_facts_and_malformed_results() {
    let invocation = uuid::Uuid::from_u128(2).to_string();
    let base = || {
        let mut v = crate::owner_commit::agent_runtime_tests::complete_history();
        v.facts.retain(|_, r| r.generation <= 6);
        v
    };
    let bound = |step: u32, child: u8| AgentRuntimeFact::TaskStepBound {
        dispatch_fact: format!("{invocation}:dispatch_intended"),
        step,
        child: hex::encode([child; 48]),
        intent: hex::encode([7; 32]),
    };
    let committed =
        |step: u32, json: String, continuation: usize| AgentRuntimeFact::TaskStepCommitted {
            bound_fact: format!("{invocation}:task_step_bound:{step}"),
            step,
            result: hex::encode([3; 32]),
            result_json: json,
            continuation: hex::encode(vec![9; continuation]),
        };
    for (json, continuation) in [
        (r#"{"ok":true}"#.to_string(), 0),
        (format!("[{}]", vec!["1.5e300"; 2000].join(",")), 16 * 1024),
        (format!("\"{}\"", "x".repeat(60 * 1024)), 1),
        ("{bad".to_string(), 4),
        (r#"{"a":1,"a":2}"#.to_string(), 4),
        ("x".repeat(MAX_AGENT_STEP_RESULT_BYTES + 1), 4),
    ] {
        let mut v = base();
        append(&mut v, bound(0, 1));
        append(&mut v, committed(0, json, continuation));
        append(&mut v, bound(1, 2));
        let generation = v.facts.values().map(|r| r.generation).max().unwrap();
        measured(&PrivateAgentState::V2(v), ROOM, generation);
    }
}
