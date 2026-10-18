//! Exercise the live policy check with a real admitted task command path.
use super::NativeTaskActionRequirement;
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use ed25519_dalek::SigningKey;
use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath, roles::AgentPreset};
use tilo_automerge::ApplyIncoming;
use tilo_peer_state::{
    SeedState, StatePeer, seed_room,
    workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand},
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

fn certificate(root: u8, device: u8) -> DeviceCert {
    let root = SigningKey::from_bytes(&[root; 32]);
    let device = SigningKey::from_bytes(&[device; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Task policy fixture".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    cert
}

#[test]
fn approved_run_cannot_exceed_its_starters_live_action_permission() {
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let (mut peer, starter, key, now) = policy_fixture(room);
    let authority = WorkspaceTaskAuthority::Person {
        caller: starter.clone(),
    };
    let requirement = approve_task(&mut peer, &authority, now);
    assert_exact_binding(&peer, room, now, &requirement);
    assert_frozen_starter(&mut peer, room, &starter, &key, now, &requirement);
    assert_independent_task_grants(&mut peer, room, &key, now, &authority, &requirement);
    assert_live_starter_revocation(
        &mut peer,
        room,
        &starter,
        &key,
        now,
        &authority,
        &requirement,
    );
}

/// An action the agent's grants answer "ask" for pauses the run for its
/// person, naming the grant, apart from one they deny.
#[test]
fn a_run_is_told_an_action_asks_first() {
    let room = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let (mut peer, starter, key, now) = policy_fixture(room);
    let authority = WorkspaceTaskAuthority::Person { caller: starter };
    let requirement = approve_task(&mut peer, &authority, now);
    let grant = |id: &str, answer| {
        Grant::new(
            id,
            Subject::agent("scribe"),
            action_node(room, &requirement),
            KindSet::UNDOABLE,
            answer,
            Subject::person("governor"),
        )
    };
    peer.governance_mut()
        .put_governed_grants(&[grant("agent-asks", Answer::Ask)], &key, now)
        .unwrap();
    let asks = requirement.check(&peer, room, "scribe", now).unwrap_err();
    assert!(asks.contains("asks a person first"), "{asks}");
    assert!(asks.contains("agent-asks"), "{asks}");
    assert!(
        NativeTaskActionRequirement::awaits_confirmation(&asks),
        "{asks}"
    );
    peer.governance_mut()
        .put_governed_grants(&[grant("agent-denied", Answer::Deny)], &key, now)
        .unwrap();
    let denied = requirement.check(&peer, room, "scribe", now).unwrap_err();
    assert!(denied.contains("not allowed"), "{denied}");
}

fn policy_fixture(room: &str) -> (StatePeer, DeviceCert, SigningKey, std::time::SystemTime) {
    admitted_fixture(room, collab_types::Role::Editor, None)
}

/// A room where the starter holds `role` and the agent is admitted with
/// `preset`, or, without one, holds a room-wide grant of every kind.
fn admitted_fixture(
    room: &str,
    role: collab_types::Role,
    preset: Option<AgentPreset>,
) -> (StatePeer, DeviceCert, SigningKey, std::time::SystemTime) {
    let governor = certificate(1, 2);
    let starter = certificate(3, 4);
    let key = SigningKey::from_bytes(&[2; 32]);
    let now = std::time::SystemTime::now();
    let roster = vec![governor.clone(), starter.clone()];
    let mut doc = AutoCommit::new();
    doc.apply_incoming(
        seed_room(room, &governor, &roster)
            .unwrap()
            .into_iter()
            .map(|bytes| automerge::Change::from_bytes(bytes).unwrap()),
    )
    .unwrap();
    let members = doc.get(ROOT, "members").unwrap().unwrap().1;
    doc.put(
        &members,
        hex::encode(starter.user_public_key),
        automerge::ScalarValue::Bytes(serde_json::to_vec(&role).unwrap()),
    )
    .unwrap();
    doc.commit();
    let mut peer = StatePeer::new(room, governor, roster, RevocationList::new(), now).unwrap();
    peer.install_seed(
        SeedState {
            room_changes: doc
                .get_changes(&[])
                .into_iter()
                .map(|change| change.raw_bytes().to_vec())
                .collect(),
            documents: vec![],
        },
        &key,
        now,
    )
    .unwrap();
    let admission = preset.map_or_else(
        || {
            vec![Grant::new(
                "agent",
                Subject::agent("scribe"),
                TreePath::room(room),
                KindSet::ALL,
                Answer::Allow,
                Subject::person("governor"),
            )]
        },
        |preset| preset.grants(room, "scribe", &Subject::person("governor")),
    );
    peer.governance_mut()
        .put_governed_grants(&admission, &key, now)
        .unwrap();

    (peer, starter, key, now)
}

fn approve_task(
    peer: &mut StatePeer,
    authority: &WorkspaceTaskAuthority,
    now: std::time::SystemTime,
) -> NativeTaskActionRequirement {
    let (_, result) = peer
        .workspace_mut()
        .apply_workspace_task_command(
            uuid::Uuid::from_u128(1),
            [1; 32],
            authority,
            &WorkspaceTaskCommand::QueueInstruction {
                title: "Inspect notes".into(),
                instruction: "Private complete instruction".into(),
                assigned_agent: "scribe".into(),
                route: tilo_desktop_model::WorkspaceTaskRoute::Model,
            },
            now,
        )
        .unwrap();
    let tilo_desktop_model::WorkspaceTaskMutationResult::Queued { task_id } = result else {
        panic!("queue result")
    };
    peer.workspace_mut()
        .apply_workspace_task_command(
            uuid::Uuid::from_u128(2),
            [2; 32],
            authority,
            &WorkspaceTaskCommand::ApproveRun {
                task_id: task_id.clone(),
                agent_id: "scribe".into(),
                expected_run_epoch: 0,
                after_revision: 0,
            },
            now,
        )
        .unwrap();
    NativeTaskActionRequirement {
        binding: Some(super::NativeTaskBinding {
            task_id,
            agent_id: "scribe".into(),
            epoch: 1,
            revision: 0,
            starter: None,
        }),
        app: "so.tilo.notes".into(),
        resource_kind: "document".into(),
        resource: "notes:release".into(),
        action: "append".into(),
        need: tilo_actions::ActionNeed {
            kind: tilo_action_policy::ActionKind::Undoable,
            field: None,
            privileged: false,
        },
        consequential: false,
        confirmation: None,
        invocation: None,
    }
}

/// Exercise the reducer's real host request and starter answer, not a caller flag.
fn record_person_confirmation(
    peer: &mut StatePeer,
    person: &WorkspaceTaskAuthority,
    requirement: &mut NativeTaskActionRequirement,
    now: std::time::SystemTime,
) {
    let contract = tilo_actions::ActionContract {
        kind: requirement.need.kind,
        ..Default::default()
    };
    requirement.invocation = Some(
        super::NativeTaskInvocation::new(
            uuid::Uuid::new_v4(),
            "42",
            Some("{}"),
            &contract,
            "native:test:0",
        )
        .unwrap(),
    );
    record_person_confirmation_for_request(peer, person, requirement, uuid::Uuid::new_v4(), now);
}

/// Preserve the broker's child UUID and request-time metadata through the real answer.
fn record_person_confirmation_for_request(
    peer: &mut StatePeer,
    person: &WorkspaceTaskAuthority,
    requirement: &mut NativeTaskActionRequirement,
    asking: uuid::Uuid,
    now: std::time::SystemTime,
) {
    use tilo_desktop_model::{
        WorkspaceTaskConfirmation, WorkspaceTaskConfirmationAnswer, WorkspaceTaskConfirmationState,
    };
    let binding = requirement.binding.as_ref().unwrap();
    let task_id = binding.task_id.clone();
    let epoch = binding.epoch;
    let host = WorkspaceTaskAuthority::Host {
        attestor: peer.authority().clone(),
    };
    let prior = tilo_peer_state::workspace_task::snapshot(peer.admitted_room().unwrap())
        .unwrap()
        .controls[&task_id]
        .confirmation
        .clone();
    if let Some(prior) = prior {
        peer.workspace_mut()
            .apply_workspace_task_command(
                uuid::Uuid::new_v4(),
                [7; 32],
                &host,
                &WorkspaceTaskCommand::SettleConfirmation {
                    task_id: task_id.clone(),
                    run_epoch: epoch,
                    confirmation_id: prior.id,
                },
                now,
            )
            .unwrap();
    }
    let arguments = serde_json::Map::new();
    let digest = tilo_actions::draft::arguments_digest(&arguments);
    let id = WorkspaceTaskConfirmation::id_for(&requirement.resource, &requirement.action, &digest);
    let contract = &requirement.invocation.as_ref().unwrap().contract;
    let confirmation = WorkspaceTaskConfirmation {
        id: id.clone(),
        asking,
        run_epoch: epoch,
        agent_id: binding.agent_id.clone(),
        app: requirement.app.clone(),
        resource_kind: requirement.resource_kind.clone(),
        resource: requirement.resource.clone(),
        action: requirement.action.clone(),
        kind: contract.kind,
        risk: contract.risk,
        draft: tilo_actions::Draft::default(),
        arguments_digest: digest,
        requested_at: 1,
        state: WorkspaceTaskConfirmationState::Waiting,
    };
    for (authority, command) in [
        (
            &host,
            WorkspaceTaskCommand::RequestConfirmation {
                task_id: task_id.clone(),
                run_epoch: epoch,
                confirmation: Box::new(confirmation),
            },
        ),
        (
            person,
            WorkspaceTaskCommand::AnswerConfirmation {
                task_id,
                run_epoch: epoch,
                confirmation_id: id,
                answer: WorkspaceTaskConfirmationAnswer::Confirm { edits: Vec::new() },
            },
        ),
    ] {
        peer.workspace_mut()
            .apply_workspace_task_command(uuid::Uuid::new_v4(), [8; 32], authority, &command, now)
            .unwrap();
    }
    requirement.confirmation = Some(asking.to_string());
}

/// Match the broker's exact child metadata, with an action-local Ask gate.
fn prepare_exact_confirmation(
    peer: &mut StatePeer,
    requirement: &mut NativeTaskActionRequirement,
    key: &SigningKey,
    now: std::time::SystemTime,
) {
    let contract = tilo_actions::ActionContract {
        kind: tilo_action_policy::ActionKind::Undoable,
        ..Default::default()
    };
    requirement.invocation = Some(
        super::NativeTaskInvocation::new(
            uuid::Uuid::new_v4(),
            "42",
            Some("{}"),
            &contract,
            "native:test:0",
        )
        .unwrap(),
    );
    let node = action_node(peer.room(), requirement);
    peer.governance_mut()
        .put_governed_grants(
            &[Grant::new(
                "exact-action-asks",
                Subject::agent("scribe"),
                node,
                KindSet::UNDOABLE,
                Answer::Ask,
                Subject::person("governor"),
            )],
            key,
            now,
        )
        .unwrap();
}

fn exact_confirmation_binding(
    peer: &StatePeer,
    requirement: &NativeTaskActionRequirement,
) -> tilo_joined_room::task_admission::NativeInvocationBinding {
    let run = requirement.binding.as_ref().unwrap();
    let invocation = requirement.invocation.as_ref().unwrap();
    tilo_joined_room::task_admission::NativeInvocationBinding {
        room: tilo_desktop_model::RoomId::parse(peer.room()).unwrap(),
        task: run.task_id.clone(),
        agent: run.agent_id.clone(),
        epoch: run.epoch,
        revision: run.revision,
        invocation: invocation.id,
        request: invocation.request,
        host: peer.authority().device_public_key,
        app: requirement.app.clone(),
        provider: invocation.provider.clone(),
        resource: requirement.resource.clone(),
        action: requirement.action.clone(),
        arguments_digest: invocation.arguments_digest,
        contract_digest: invocation.contract_digest,
        ceiling_digest: requirement.ceiling_digest().unwrap(),
        private_run_digest: invocation.private_run_digest,
    }
}

#[test]
fn request_time_confirmation_custody_binds_one_exact_child_across_restart() {
    use tilo_joined_room::task_admission::{NativeGovernorAdmission, TaskAdmissionLedger};
    let room = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    let (mut peer, starter, key, now) = policy_fixture(room);
    let person = WorkspaceTaskAuthority::Person { caller: starter };
    let mut requirement = approve_task(&mut peer, &person, now);
    prepare_exact_confirmation(&mut peer, &mut requirement, &key, now);
    let host = peer.authority().clone();
    let asking = uuid::Uuid::new_v4();
    let snapshot =
        tilo_peer_state::workspace_task::snapshot(peer.admitted_room().unwrap()).unwrap();
    let mut ledger = TaskAdmissionLedger::default();
    let error = requirement
        .check_invocation(&peer, &exact_confirmation_binding(&peer, &requirement), now)
        .unwrap_err();
    assert!(
        NativeTaskActionRequirement::awaits_confirmation(&error),
        "{error}"
    );
    ledger
        .freeze_confirmation(&peer, &snapshot, asking, &requirement, &host, now)
        .unwrap();
    let child = requirement.invocation.as_ref().unwrap().id;
    record_person_confirmation_for_request(&mut peer, &person, &mut requirement, asking, now);
    assert_eq!(requirement.invocation.as_ref().unwrap().id, child);
    let snapshot =
        tilo_peer_state::workspace_task::snapshot(peer.admitted_room().unwrap()).unwrap();
    let exact = exact_confirmation_binding(&peer, &requirement);
    ledger
        .check_action(&peer, &snapshot, &requirement, &exact, now)
        .unwrap();
    let reservation = NativeGovernorAdmission {
        room: exact.room,
        peer: &peer,
        ledger: &mut ledger,
        key: &key,
        continuity: 1,
    }
    .reserve(exact.clone(), &host, now, |peer, binding| {
        requirement.check_invocation(peer, binding, now)
    })
    .unwrap();
    assert_eq!(reservation.binding, exact);

    let mut another_child = requirement.clone();
    let invocation = another_child.invocation.as_mut().unwrap();
    invocation.id = uuid::Uuid::new_v4();
    invocation.request = invocation.id;
    let another_exact = exact_confirmation_binding(&peer, &another_child);
    assert!(
        ledger
            .check_action(&peer, &snapshot, &another_child, &another_exact, now)
            .is_err()
    );
    let restarted: TaskAdmissionLedger =
        serde_json::from_slice(&serde_json::to_vec(&ledger).unwrap()).unwrap();
    restarted
        .check_action(&peer, &snapshot, &requirement, &exact, now)
        .unwrap();
    assert!(
        restarted
            .check_action(&peer, &snapshot, &another_child, &another_exact, now)
            .is_err()
    );

    // Rebuild coherent exact bindings: failures must not merely be stale digests.
    for change in [
        "contract",
        "provider",
        "private run",
        "revision",
        "resource",
        "arguments",
    ] {
        let mut changed = requirement.clone();
        let original = changed.invocation.as_ref().unwrap();
        let mut contract = original.contract.clone();
        if change == "contract" {
            contract.kind = tilo_action_policy::ActionKind::Outside;
        }
        changed.invocation = Some(
            super::NativeTaskInvocation::new(
                child,
                if change == "provider" { "43" } else { "42" },
                Some(if change == "arguments" {
                    "{\"extra\":true}"
                } else {
                    "{}"
                }),
                &contract,
                if change == "private run" {
                    "native:test:1"
                } else {
                    "native:test:0"
                },
            )
            .unwrap(),
        );
        if change == "revision" {
            changed.binding.as_mut().unwrap().revision += 1;
        }
        if change == "resource" {
            changed.resource = "notes:other".into();
        }
        let changed_exact = exact_confirmation_binding(&peer, &changed);
        assert!(
            restarted
                .check_action(&peer, &snapshot, &changed, &changed_exact, now)
                .is_err(),
            "accepted changed {change}"
        );
    }
}

#[test]
fn an_answer_cannot_be_retroactively_given_request_time_custody() {
    use tilo_joined_room::task_admission::TaskAdmissionLedger;
    let room = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    let (mut peer, starter, key, now) = policy_fixture(room);
    let person = WorkspaceTaskAuthority::Person { caller: starter };
    let mut requirement = approve_task(&mut peer, &person, now);
    prepare_exact_confirmation(&mut peer, &mut requirement, &key, now);
    let original = requirement.clone();
    let host = peer.authority().clone();
    let asking = uuid::Uuid::new_v4();
    record_person_confirmation_for_request(&mut peer, &person, &mut requirement, asking, now);
    let snapshot =
        tilo_peer_state::workspace_task::snapshot(peer.admitted_room().unwrap()).unwrap();
    let mut ledger = TaskAdmissionLedger::default();
    let exact = exact_confirmation_binding(&peer, &requirement);
    requirement.check_invocation(&peer, &exact, now).unwrap();
    assert!(
        ledger
            .check_action(&peer, &snapshot, &requirement, &exact, now)
            .is_err()
    );
    // Even the original pre-answer requirement cannot create missing custody now.
    assert!(
        ledger
            .freeze_confirmation(&peer, &snapshot, asking, &original, &host, now)
            .is_err()
    );
}

fn assert_exact_binding(
    peer: &StatePeer,
    room: &str,
    now: std::time::SystemTime,
    requirement: &NativeTaskActionRequirement,
) {
    requirement.check(peer, room, "scribe", now).unwrap();
    let mut unbound = requirement.clone();
    unbound.binding = None;
    assert!(unbound.check(peer, room, "scribe", now).is_err());
    let mut stale = requirement.clone();
    stale.binding.as_mut().unwrap().epoch = 2;
    assert!(stale.check(peer, room, "scribe", now).is_err());
    let mut privileged = requirement.clone();
    privileged.need.privileged = true;
    assert!(privileged.check(peer, room, "scribe", now).is_err());
}

fn action_node(room: &str, requirement: &NativeTaskActionRequirement) -> TreePath {
    tilo_actions::action_node(
        &TreePath::room(room),
        &requirement.app,
        &requirement.resource_kind,
        &requirement.resource,
        None,
        &requirement.action,
    )
}

fn assert_frozen_starter(
    peer: &mut StatePeer,
    room: &str,
    starter: &DeviceCert,
    key: &SigningKey,
    now: std::time::SystemTime,
    requirement: &NativeTaskActionRequirement,
) {
    let node = action_node(room, requirement);
    // A later grant to the starter cannot widen an already running task.
    peer.governance_mut()
        .put_governed_grants(
            &[Grant::new(
                "starter-new-outside",
                Subject::person(hex::encode(starter.user_public_key)),
                node,
                KindSet::OUTSIDE,
                Answer::Allow,
                Subject::person("governor"),
            )],
            key,
            now,
        )
        .unwrap();
    let mut widened = requirement.clone();
    widened.need.kind = tilo_action_policy::ActionKind::Outside;
    // The frozen starter asked first for outside effects: the later Allow
    // does not let the run take one unasked, it pauses for the person.
    let error = widened.check(peer, room, "scribe", now).unwrap_err();
    assert!(
        NativeTaskActionRequirement::awaits_confirmation(&error),
        "{error}"
    );
}

fn assert_independent_task_grants(
    peer: &mut StatePeer,
    room: &str,
    key: &SigningKey,
    now: std::time::SystemTime,
    authority: &WorkspaceTaskAuthority,
    requirement: &NativeTaskActionRequirement,
) {
    // A second task for the same agent has an independent grant set.
    let (_, queued) = peer
        .workspace_mut()
        .apply_workspace_task_command(
            uuid::Uuid::from_u128(3),
            [3; 32],
            authority,
            &WorkspaceTaskCommand::QueueInstruction {
                title: "Other work".into(),
                instruction: "Other instruction".into(),
                assigned_agent: "scribe".into(),
                route: tilo_desktop_model::WorkspaceTaskRoute::Model,
            },
            now,
        )
        .unwrap();
    let tilo_desktop_model::WorkspaceTaskMutationResult::Queued { task_id: other } = queued else {
        panic!("queue result")
    };
    peer.workspace_mut()
        .apply_workspace_task_command(
            uuid::Uuid::from_u128(4),
            [4; 32],
            authority,
            &WorkspaceTaskCommand::ApproveRun {
                task_id: other.clone(),
                agent_id: "scribe".into(),
                expected_run_epoch: 0,
                after_revision: 0,
            },
            now,
        )
        .unwrap();
    peer.governance_mut()
        .put_governed_grants(
            &[Grant::new(
                "other-task-deny",
                Subject::task(&other),
                TreePath::room(room),
                KindSet::ALL,
                Answer::Deny,
                Subject::person("governor"),
            )],
            key,
            now,
        )
        .unwrap();
    requirement.check(peer, room, "scribe", now).unwrap();
    let mut other_requirement = requirement.clone();
    other_requirement.binding.as_mut().unwrap().task_id = other;
    assert!(other_requirement.check(peer, room, "scribe", now).is_err());
}

fn assert_live_starter_revocation(
    peer: &mut StatePeer,
    room: &str,
    starter: &DeviceCert,
    key: &SigningKey,
    now: std::time::SystemTime,
    authority: &WorkspaceTaskAuthority,
    requirement: &NativeTaskActionRequirement,
) {
    peer.governance_mut()
        .put_governed_grants(
            &[Grant::new(
                "starter-no-append",
                Subject::person(hex::encode(starter.user_public_key)),
                action_node(room, requirement),
                KindSet::UNDOABLE,
                Answer::Deny,
                Subject::person("governor"),
            )],
            key,
            now,
        )
        .unwrap();
    // Room write and the agent's own grant remain valid: only this exact
    // operation is revoked from the person who consciously started the run.
    assert!(
        peer.permissions(starter.device_public_key, now)
            .unwrap()
            .allows(
                &TreePath::room(room),
                tilo_action_policy::ActionKind::Undoable,
                now
            )
    );
    assert!(
        requirement
            .check(peer, room, "scribe", now)
            .unwrap_err()
            .contains("starter")
    );
    for command in [
        WorkspaceTaskCommand::DirectionForRun {
            task_id: requirement.binding.as_ref().unwrap().task_id.clone(),
            run_epoch: 1,
            after_revision: 0,
            text: "Continue".into(),
        },
        WorkspaceTaskCommand::CancelForRun {
            task_id: requirement.binding.as_ref().unwrap().task_id.clone(),
            run_epoch: 1,
            after_revision: 0,
        },
    ] {
        let error = peer
            .workspace_mut()
            .apply_workspace_task_command(uuid::Uuid::new_v4(), [9; 32], authority, &command, now)
            .unwrap_err();
        assert!(error.contains("every execution permission"), "{error}");
    }
}

#[test]
fn an_action_asking_first_pauses_until_confirmed_and_deny_still_refuses() {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    let room = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let (mut peer, starter, key, now) = policy_fixture(room);
    let authority = WorkspaceTaskAuthority::Person { caller: starter };
    let requirement = approve_task(&mut peer, &authority, now);
    let awaits = |requirement: &NativeTaskActionRequirement, peer: &StatePeer| {
        let error = requirement.check(peer, room, "scribe", now).unwrap_err();
        NativeTaskActionRequirement::awaits_confirmation(&error)
    };

    // An action whose contract confirms (it deletes for good) asks first,
    // though the agent's grants allow its kind.
    let mut purge = requirement.clone();
    purge.consequential = true;
    assert!(awaits(&purge, &peer));
    record_person_confirmation(&mut peer, &authority, &mut purge, now);
    purge.check(&peer, room, "scribe", now).unwrap();

    // The person tells the agent Notes asks first.
    let scope = AnswerScope::app("so.tilo.notes");
    let set = |answer: Answer, peer: &mut StatePeer| {
        peer.governance_mut()
            .put_governed_grants(
                &[agent_answers::grant(
                    room,
                    "scribe",
                    (&scope, tilo_action_policy::ActionKind::Undoable),
                    answer,
                    Subject::person("governor"),
                )],
                &key,
                now,
            )
            .unwrap();
    };
    set(Answer::Ask, &mut peer);
    assert!(awaits(&requirement, &peer));
    let mut confirmed = requirement.clone();
    record_person_confirmation(&mut peer, &authority, &mut confirmed, now);
    confirmed.check(&peer, room, "scribe", now).unwrap();
    let mut forged = confirmed.clone();
    forged.confirmation = Some(uuid::Uuid::new_v4().to_string());
    assert!(forged.check(&peer, room, "scribe", now).is_err());
    let mut changed = confirmed.clone();
    changed.invocation.as_mut().unwrap().arguments = Some(r#"{"extra":true}"#.into());
    assert!(changed.check(&peer, room, "scribe", now).is_err());

    // Not allowed: refused outright, confirmed or not.
    set(Answer::Deny, &mut peer);
    let error = confirmed.check(&peer, room, "scribe", now).unwrap_err();
    assert!(
        !NativeTaskActionRequirement::awaits_confirmation(&error),
        "{error}"
    );

    // Always allowing the one action lifts its contract's confirmation.
    let always = AnswerScope::action("so.tilo.notes", "document", "append");
    peer.governance_mut()
        .put_governed_grants(
            &[agent_answers::grant(
                room,
                "scribe",
                (&always, tilo_action_policy::ActionKind::Undoable),
                Answer::Allow,
                Subject::person("governor"),
            )],
            &key,
            now,
        )
        .unwrap();
    let mut purge = requirement;
    purge.consequential = true;
    purge.check(&peer, room, "scribe", now).unwrap();
}

/// A task a person starts runs on their behalf: an agent admitted with the
/// default presets, which grant it no changes of its own, takes the changes
/// its starter may take, held to the answers the room keeps for it.
#[test]
fn a_run_takes_its_starters_changes_whatever_preset_admitted_its_agent() {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    for preset in [AgentPreset::Observer, AgentPreset::Contributor] {
        let room = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        let (mut peer, starter, key, now) =
            admitted_fixture(room, collab_types::Role::Editor, Some(preset));
        let authority = WorkspaceTaskAuthority::Person { caller: starter };
        let requirement = approve_task(&mut peer, &authority, now);
        requirement
            .check(&peer, room, "scribe", now)
            .unwrap_or_else(|error| panic!("{preset:?}: {error}"));

        // Outside effects still ask first by default.
        let mut outside = requirement.clone();
        outside.need.kind = tilo_action_policy::ActionKind::Outside;
        let asks = outside.check(&peer, room, "scribe", now).unwrap_err();
        assert!(
            NativeTaskActionRequirement::awaits_confirmation(&asks),
            "{asks}"
        );

        // The person's Deny for the agent still refuses.
        peer.governance_mut()
            .put_governed_grants(
                &[agent_answers::grant(
                    room,
                    "scribe",
                    (
                        &AnswerScope::app("so.tilo.notes"),
                        tilo_action_policy::ActionKind::Undoable,
                    ),
                    Answer::Deny,
                    Subject::person("governor"),
                )],
                &key,
                now,
            )
            .unwrap();
        let denied = requirement.check(&peer, room, "scribe", now).unwrap_err();
        assert!(denied.contains("not allowed"), "{denied}");

        // On its own, outside any task, the agent keeps its preset's limits.
        let policy =
            tilo_desktop_model::projection::room_policy_of(peer.admitted_room().unwrap(), room);
        let own = tilo_action_policy::Authority::person(policy.party_of(Subject::agent("scribe")))
            .decide(
                &action_node(room, &requirement),
                tilo_action_policy::ActionKind::Undoable,
                &tilo_action_policy::Context::at_time(now),
            );
        assert_eq!(own.answer, Answer::Deny, "{preset:?}");
    }
}

/// The starter's own rights bound the run, whatever the agent was admitted
/// with: a person who may not change a note starts no run that changes it,
/// and one who may not change the room starts no run at all.
#[test]
fn a_run_never_exceeds_a_starter_who_may_not_change_the_note() {
    let room = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    let (mut peer, starter, key, now) = admitted_fixture(
        room,
        collab_types::Role::Editor,
        Some(AgentPreset::Operator),
    );
    peer.governance_mut()
        .put_governed_grants(
            &[Grant::new(
                "starter-reads-notes",
                Subject::person(hex::encode(starter.user_public_key)),
                TreePath::room(room).app("so.tilo.notes"),
                KindSet::UNDOABLE,
                Answer::Deny,
                Subject::person("governor"),
            )],
            &key,
            now,
        )
        .unwrap();
    let authority = WorkspaceTaskAuthority::Person { caller: starter };
    let requirement = approve_task(&mut peer, &authority, now);
    let refused = requirement.check(&peer, room, "scribe", now).unwrap_err();
    assert!(refused.contains("starter"), "{refused}");

    let (mut peer, commenter, _key, now) = admitted_fixture(
        room,
        collab_types::Role::Commenter,
        Some(AgentPreset::Operator),
    );
    let refused = peer
        .workspace_mut()
        .apply_workspace_task_command(
            uuid::Uuid::from_u128(1),
            [1; 32],
            &WorkspaceTaskAuthority::Person { caller: commenter },
            &WorkspaceTaskCommand::QueueInstruction {
                title: "Translate".into(),
                instruction: "Translate this in place".into(),
                assigned_agent: "scribe".into(),
                route: tilo_desktop_model::WorkspaceTaskRoute::Model,
            },
            now,
        )
        .unwrap_err();
    assert!(refused.contains("write authority"), "{refused}");
}
