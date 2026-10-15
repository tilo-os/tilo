//! Exercise the live policy check with a real admitted task command path.
use super::NativeTaskActionRequirement;
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use ed25519_dalek::SigningKey;
use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath};
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

/// An action the agent's grants answer "ask" for is refused to an
/// unattended run, saying it asks first, apart from one they deny.
#[test]
fn an_unattended_run_is_told_an_action_asks_first() {
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
    peer.governance_mut()
        .put_governed_grants(&[grant("agent-denied", Answer::Deny)], &key, now)
        .unwrap();
    let denied = requirement.check(&peer, room, "scribe", now).unwrap_err();
    assert!(denied.contains("not allowed"), "{denied}");
}

fn policy_fixture(room: &str) -> (StatePeer, DeviceCert, SigningKey, std::time::SystemTime) {
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
        automerge::ScalarValue::Bytes(serde_json::to_vec(&collab_types::Role::Editor).unwrap()),
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
    peer.governance_mut()
        .put_governed_grants(
            &[Grant::new(
                "agent",
                Subject::agent("scribe"),
                TreePath::room(room),
                KindSet::ALL,
                Answer::Allow,
                Subject::person("governor"),
            )],
            &key,
            now,
        )
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
    }
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
    assert!(
        widened
            .check(peer, room, "scribe", now)
            .unwrap_err()
            .contains("frozen")
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
