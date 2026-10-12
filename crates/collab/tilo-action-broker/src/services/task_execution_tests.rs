//! Exercise the live policy check with a real admitted task command path.
use super::NativeTaskActionRequirement;
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use ed25519_dalek::SigningKey;
use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath};
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
    let governor = certificate(1, 2);
    let starter = certificate(3, 4);
    let key = SigningKey::from_bytes(&[2; 32]);
    let now = std::time::SystemTime::now();
    let roster = vec![governor.clone(), starter.clone()];
    let mut doc = AutoCommit::new();
    doc.apply_changes(
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
    peer.put_governed_grants(
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
    let authority = WorkspaceTaskAuthority::Person {
        caller: starter.clone(),
    };
    let (_, result) = peer
        .apply_workspace_task_command(
            uuid::Uuid::from_u128(1),
            [1; 32],
            &authority,
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
    peer.apply_workspace_task_command(
        uuid::Uuid::from_u128(2),
        [2; 32],
        &authority,
        &WorkspaceTaskCommand::ApproveRun {
            task_id: task_id.clone(),
            agent_id: "scribe".into(),
            expected_run_epoch: 0,
            after_revision: 0,
        },
        now,
    )
    .unwrap();
    let requirement = NativeTaskActionRequirement {
        binding: Some(super::NativeTaskBinding {
            task_id: task_id.clone(),
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
    };
    requirement.check(&peer, room, "scribe", now).unwrap();
    let mut unbound = requirement.clone();
    unbound.binding = None;
    assert!(unbound.check(&peer, room, "scribe", now).is_err());
    let mut stale = requirement.clone();
    stale.binding.as_mut().unwrap().epoch = 2;
    assert!(stale.check(&peer, room, "scribe", now).is_err());
    let mut privileged = requirement.clone();
    privileged.need.privileged = true;
    assert!(privileged.check(&peer, room, "scribe", now).is_err());
    let node = tilo_actions::action_node(
        &TreePath::room(room),
        &requirement.app,
        &requirement.resource_kind,
        &requirement.resource,
        None,
        &requirement.action,
    );
    // A later grant to the starter cannot widen an already running task.
    peer.put_governed_grants(
        &[Grant::new(
            "starter-new-outside",
            Subject::person(hex::encode(starter.user_public_key)),
            node.clone(),
            KindSet::OUTSIDE,
            Answer::Allow,
            Subject::person("governor"),
        )],
        &key,
        now,
    )
    .unwrap();
    let mut widened = requirement.clone();
    widened.need.kind = tilo_action_policy::ActionKind::Outside;
    assert!(
        widened
            .check(&peer, room, "scribe", now)
            .unwrap_err()
            .contains("frozen")
    );
    // A second task for the same agent has an independent grant set.
    let (_, queued) = peer
        .apply_workspace_task_command(
            uuid::Uuid::from_u128(3),
            [3; 32],
            &authority,
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
    peer.apply_workspace_task_command(
        uuid::Uuid::from_u128(4),
        [4; 32],
        &authority,
        &WorkspaceTaskCommand::ApproveRun {
            task_id: other.clone(),
            agent_id: "scribe".into(),
            expected_run_epoch: 0,
            after_revision: 0,
        },
        now,
    )
    .unwrap();
    peer.put_governed_grants(
        &[Grant::new(
            "other-task-deny",
            Subject::task(&other),
            TreePath::room(room),
            KindSet::ALL,
            Answer::Deny,
            Subject::person("governor"),
        )],
        &key,
        now,
    )
    .unwrap();
    requirement.check(&peer, room, "scribe", now).unwrap();
    let mut other_requirement = requirement.clone();
    other_requirement.binding.as_mut().unwrap().task_id = other;
    assert!(other_requirement.check(&peer, room, "scribe", now).is_err());
    peer.put_governed_grants(
        &[Grant::new(
            "starter-no-append",
            Subject::person(hex::encode(starter.user_public_key)),
            node,
            KindSet::UNDOABLE,
            Answer::Deny,
            Subject::person("governor"),
        )],
        &key,
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
            .check(&peer, room, "scribe", now)
            .unwrap_err()
            .contains("starter")
    );
    for command in [
        WorkspaceTaskCommand::DirectionForRun {
            task_id: task_id.clone(),
            run_epoch: 1,
            after_revision: 0,
            text: "Continue".into(),
        },
        WorkspaceTaskCommand::CancelForRun {
            task_id,
            run_epoch: 1,
            after_revision: 0,
        },
    ] {
        let error = peer
            .apply_workspace_task_command(uuid::Uuid::new_v4(), [9; 32], &authority, &command, now)
            .unwrap_err();
        assert!(error.contains("every execution permission"), "{error}");
    }
}
