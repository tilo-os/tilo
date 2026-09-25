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
    // `admitted()` holds one live spawn; fill the remaining live seats.
    for n in 82..=(80 + MAX_LIVE_PRIVATE_AGENTS as u128) {
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
                store.admit_agent_candidate(
                    &caller,
                    uuid::Uuid::from_u128(81 + MAX_LIVE_PRIVATE_AGENTS as u128),
                    &input
                ),
                Err(AgentCommitError::Capacity {
                    limit: MAX_LIVE_PRIVATE_AGENTS
                })
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
            .admit_agent_candidate(
                &caller,
                uuid::Uuid::from_u128(81 + MAX_LIVE_PRIVATE_AGENTS as u128),
                &input
            )
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
/// Commit a host-signed room capability change: a governance (policy)
/// revision change such as an invite/join or a role change produces.
fn change_governance(store: &mut Store) {
    use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
    let mut doc = store.peer().unwrap().admitted_room().unwrap().clone();
    doc.set_actor(automerge::ActorId::from(
        store.key().verifying_key().to_bytes().to_vec(),
    ));
    let caps = doc.get(ROOT, "capabilities").unwrap().unwrap().1;
    let cap = collab_types::caps::CapOverride::restrict_role_in_app(
        collab_types::Role::Viewer,
        "so.tilo.chat",
        collab_types::caps::Verb::Write,
    );
    doc.put(
        &caps,
        "governance-change-test",
        ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
    )
    .unwrap();
    doc.commit();
    let message = qos_peer_state::NeighbourMsg::SyncResp {
        room: store.state.room.clone(),
        changes: doc
            .get_changes(&[])
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect(),
    };
    let now = store.policy_now_ns().unwrap();
    store
        .receive(
            qos_peer_state::TransportOrigin::RelayDelta(store.key().verifying_key().to_bytes()),
            message,
            now,
        )
        .unwrap();
}
#[test]
fn agent_governance_change_stops_the_admitted_task_but_a_new_message_repins_it() {
    let (_dir, mut store, custody) = admitted();
    let governance = custody.admission.change.expected_governance.clone();
    let AgentRuntimeFact::Admitted { ids, intent, .. } = &custody.admission.change.fact else {
        unreachable!()
    };
    let (agent_id, spawn_intent) = (ids.agent_id.clone(), intent.clone());
    let caller = spawn_intent.scope.caller.clone();
    let mut process = None;
    for fact in remaining_history(&store, &custody) {
        if matches!(fact, AgentRuntimeFact::ProcessExited { .. }) {
            break;
        }
        if let AgentRuntimeFact::ProcessBound { process: facts, .. } = &fact {
            process = Some(facts.clone());
        }
        store
            .append_effect_fact(&custody.id(), &governance, fact)
            .unwrap();
    }
    let process = process.unwrap();
    assert!(store.agent_process_grants_data(&process, &caller).is_ok());
    change_governance(&mut store);
    let current = store.agent_caller(caller.device_public_key).unwrap();
    assert_ne!(current.governance_revision(), governance, "policy changed");
    // The admitted task and its process authority stop at the next check.
    assert!(store.current_agent_effect(&custody.id()).is_err());
    assert!(store.agent_process_grants_data(&process, &caller).is_err());
    // A new owner Message to the idle agent is authorized under current
    // governance (the invite/join regression: it was refused forever).
    let (_, grants) = store
        .agent_process_grants_pinned(&process, &caller, GovernancePin::NewControl)
        .unwrap();
    assert_eq!(grants, spawn_intent.grants);
    let mut message = spawn_intent.clone();
    message.route.action = "message".into();
    message.request = String::from_utf8(
        qos_agent_control_protocol::canonical_request(&AgentRequest::Message {
            version: 1,
            agent_id,
            text: "One more sentence".into(),
        })
        .unwrap(),
    )
    .unwrap();
    let message_id = uuid::Uuid::from_u128(82);
    let AgentAdmissionResult::Fresh(_) = store
        .admit_agent_candidate(&current, message_id, &message)
        .unwrap()
    else {
        panic!("fresh message admission")
    };
    let message_id = AgentControlId::new(message_id).unwrap();
    // The Message task is pinned to the current revision; the process is
    // re-pinned by it; the original task stays stopped.
    assert!(store.current_agent_effect(&message_id).is_ok());
    assert!(store.agent_process_grants_data(&process, &caller).is_ok());
    assert!(store.current_agent_effect(&custody.id()).is_err());
    // A later policy change stops the Message task in turn.
    change_governance_again(&mut store);
    assert!(store.current_agent_effect(&message_id).is_err());
    assert!(store.agent_process_grants_data(&process, &caller).is_err());
}
fn change_governance_again(store: &mut Store) {
    use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
    let mut doc = store.peer().unwrap().admitted_room().unwrap().clone();
    doc.set_actor(automerge::ActorId::from(
        store.key().verifying_key().to_bytes().to_vec(),
    ));
    let caps = doc.get(ROOT, "capabilities").unwrap().unwrap().1;
    doc.put(
        &caps,
        "governance-change-test-2",
        ScalarValue::Bytes(b"{}".to_vec()),
    )
    .unwrap();
    doc.commit();
    let message = qos_peer_state::NeighbourMsg::SyncResp {
        room: store.state.room.clone(),
        changes: doc
            .get_changes(&[])
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect(),
    };
    let now = store.policy_now_ns().unwrap();
    let _ = store.receive(
        qos_peer_state::TransportOrigin::RelayDelta(store.key().verifying_key().to_bytes()),
        message,
        now,
    );
}
/// A completed, idle Spawn with its retained process facts (no exit).
fn idle_spawn() -> (
    tempfile::TempDir,
    Store,
    AgentAttemptCustody,
    ChannelProcessFactsV2,
    Vec<AgentRuntimeFact>,
) {
    let (dir, mut store, custody) = admitted();
    let governance = custody.admission.change.expected_governance.clone();
    let mut process = None;
    let mut rest = vec![];
    let mut exited = false;
    for fact in remaining_history(&store, &custody) {
        if exited || matches!(fact, AgentRuntimeFact::ProcessExited { .. }) {
            exited = true;
            rest.push(fact);
            continue;
        }
        if let AgentRuntimeFact::ProcessBound { process: facts, .. } = &fact {
            process = Some(facts.clone());
        }
        store
            .append_effect_fact(&custody.id(), &governance, fact)
            .unwrap();
    }
    (dir, store, custody, process.unwrap(), rest)
}
fn message_intent(custody: &AgentAttemptCustody, text: &str) -> AgentIntentV2 {
    let AgentRuntimeFact::Admitted { ids, intent, .. } = &custody.admission.change.fact else {
        unreachable!()
    };
    let mut message = intent.clone();
    message.route.action = "message".into();
    message.request = String::from_utf8(
        qos_agent_control_protocol::canonical_request(&AgentRequest::Message {
            version: 1,
            agent_id: ids.agent_id.clone(),
            text: text.into(),
        })
        .unwrap(),
    )
    .unwrap();
    message
}
/// Narrow the owner's Writer permission to Read (a policy narrowing that
/// covers the Spawn's granted Writer action and record).
fn narrow_owner_writer(store: &mut Store) {
    use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
    let mut doc = store.peer().unwrap().admitted_room().unwrap().clone();
    doc.set_actor(automerge::ActorId::from(
        store.key().verifying_key().to_bytes().to_vec(),
    ));
    let caps = doc.get(ROOT, "capabilities").unwrap().unwrap().1;
    let cap = collab_types::caps::CapOverride::restrict_role_in_app(
        collab_types::Role::Owner,
        "so.tilo.word-processor",
        collab_types::caps::Verb::Read,
    );
    doc.put(
        &caps,
        "narrow-owner-writer",
        ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
    )
    .unwrap();
    doc.commit();
    let message = qos_peer_state::NeighbourMsg::SyncResp {
        room: store.state.room.clone(),
        changes: doc
            .get_changes(&[])
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect(),
    };
    let now = store.policy_now_ns().unwrap();
    store
        .receive(
            qos_peer_state::TransportOrigin::RelayDelta(store.key().verifying_key().to_bytes()),
            message,
            now,
        )
        .unwrap();
}
#[test]
fn agent_message_after_policy_narrowing_cannot_revive_spawn_grants() {
    let (_dir, mut store, custody, _, _) = idle_spawn();
    let AgentRuntimeFact::Admitted { intent, .. } = &custody.admission.change.fact else {
        unreachable!()
    };
    let device = intent.scope.caller.device_public_key;
    narrow_owner_writer(&mut store);
    let current = store.agent_caller(device).unwrap();
    let error = store
        .admit_agent_candidate(
            &current,
            uuid::Uuid::from_u128(90),
            &message_intent(&custody, "more"),
        )
        .err()
        .expect("narrowed grants must refuse the Message");
    assert!(
        error.to_string().contains("grants no longer permitted"),
        "{error}"
    );
    assert!(
        !store
            .runtime_ledger()
            .unwrap()
            .facts
            .contains_key(&format!("{}:admitted", uuid::Uuid::from_u128(90))),
        "refused Message left no admission"
    );
}
#[test]
fn agent_message_admission_records_the_current_revision_and_replays_existing() {
    let (_dir, mut store, custody, _, _) = idle_spawn();
    let AgentRuntimeFact::Admitted { intent, .. } = &custody.admission.change.fact else {
        unreachable!()
    };
    let device = intent.scope.caller.device_public_key;
    // A caller proof captured before the change (as by authorize()).
    let stale = store.agent_caller(device).unwrap();
    change_governance(&mut store);
    let id = uuid::Uuid::from_u128(91);
    let message = message_intent(&custody, "after join");
    let AgentAdmissionResult::Fresh(_) = store.admit_agent_candidate(&stale, id, &message).unwrap()
    else {
        panic!("fresh admission")
    };
    let current = store.agent_caller(device).unwrap();
    let recorded = store
        .runtime_ledger()
        .unwrap()
        .facts
        .get(&format!("{id}:admitted"))
        .unwrap()
        .change
        .expected_governance
        .clone();
    assert_eq!(
        recorded,
        current.governance_revision(),
        "pinned to the newer revision"
    );
    // A second policy change: replaying the same Message is an observation,
    // never a re-pin, and its task is refused at its next step.
    change_governance_again(&mut store);
    let current = store.agent_caller(device).unwrap();
    assert!(matches!(
        store.admit_agent_candidate(&current, id, &message).unwrap(),
        AgentAdmissionResult::Existing(_)
    ));
    assert_eq!(
        store
            .runtime_ledger()
            .unwrap()
            .facts
            .get(&format!("{id}:admitted"))
            .unwrap()
            .change
            .expected_governance,
        recorded
    );
    assert!(
        store
            .current_agent_effect(&AgentControlId::new(id).unwrap())
            .is_err()
    );
}
#[test]
fn agent_new_control_refuses_other_device_exited_and_uncertain_processes() {
    let (_dir, mut store, custody, process, rest) = idle_spawn();
    let AgentRuntimeFact::Admitted { intent, .. } = &custody.admission.change.fact else {
        unreachable!()
    };
    let caller = intent.scope.caller.clone();
    change_governance(&mut store);
    assert!(
        store
            .agent_process_grants_pinned(&process, &caller, GovernancePin::NewControl)
            .is_ok()
    );
    // A different device (here the host's own certificate) is not the caller.
    let other = store.state.bound.as_ref().unwrap().authority.clone();
    assert!(
        store
            .agent_process_grants_pinned(&process, &other, GovernancePin::NewControl)
            .is_err()
    );
    // Uncertain (reply lost before acceptance): no grant authority.
    let (_d2, mut uncertain, c2) = admitted();
    let g2 = c2.admission.change.expected_governance.clone();
    let AgentRuntimeFact::Admitted { intent: i2, .. } = &c2.admission.change.fact else {
        unreachable!()
    };
    let caller2 = i2.scope.caller.clone();
    let mut p2 = None;
    for fact in remaining_history(&uncertain, &c2) {
        if matches!(fact, AgentRuntimeFact::Completed { .. }) {
            break;
        }
        if let AgentRuntimeFact::ProcessBound { process: facts, .. } = &fact {
            p2 = Some(facts.clone());
        }
        uncertain.append_effect_fact(&c2.id(), &g2, fact).unwrap();
    }
    let p2 = p2.unwrap();
    change_governance(&mut uncertain);
    uncertain
        .mark_agent_unknown(
            &c2.id(),
            AgentError {
                code: AgentErrorCode::OutcomeUncertain,
                message: "lost".into(),
            },
        )
        .unwrap();
    // Exited process: refused.
    let governance = store
        .runtime_ledger()
        .unwrap()
        .facts
        .get(&format!("{}:admitted", custody.id().invocation()))
        .unwrap()
        .change
        .expected_governance
        .clone();
    for fact in rest {
        if matches!(fact, AgentRuntimeFact::ProcessExited { .. }) {
            store
                .append_effect_fact(&custody.id(), &governance, fact)
                .unwrap();
        }
    }
    assert!(
        store
            .agent_process_grants_pinned(&process, &caller, GovernancePin::NewControl)
            .is_err()
    );
    assert!(
        uncertain
            .agent_process_grants_pinned(&p2, &caller2, GovernancePin::NewControl)
            .is_err(),
        "uncertain agent has no grant authority for a new control"
    );
}
