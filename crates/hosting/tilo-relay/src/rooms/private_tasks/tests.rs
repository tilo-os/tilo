use super::*;
use tilo_action_policy::{Answer, Subject};
use tilo_desktop_model::{WorkspaceTaskMutationResult, WorkspaceTaskRoute};
use tilo_peer_state::workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand};

#[test]
fn person_goal_read_uses_durable_private_custody_and_exact_current_certificate() {
    let directory = tempfile::tempdir().unwrap();
    let (key, cert) = super::super::tests::governor();
    let now = SystemTime::now();
    let mut room =
        GovernedRoom::create(directory.path(), "Private task", &cert, &key, now).unwrap();
    let room_id = room.room().to_owned();
    room.peer
        .governance_mut()
        .put_governed_grants(
            &[tilo_action_policy::Grant::new(
                "task-reader-agent",
                Subject::agent("scribe"),
                TreePath::room(room_id),
                tilo_action_policy::KindSet::READ_UNDOABLE,
                Answer::Allow,
                Subject::person(hex::encode(cert.user_public_key)),
            )],
            &key,
            now,
        )
        .unwrap();
    let (_, result) = room
        .peer
        .workspace_mut()
        .apply_workspace_task_command(
            uuid::Uuid::new_v4(),
            [1; 32],
            &WorkspaceTaskAuthority::Person {
                caller: cert.clone(),
            },
            &WorkspaceTaskCommand::Queue {
                title: "Summarize document".into(),
                assigned_agent: "scribe".into(),
                route: WorkspaceTaskRoute::Model,
                resource: None,
            },
            now,
        )
        .unwrap();
    let WorkspaceTaskMutationResult::Queued { task_id } = &result else {
        panic!("queued task")
    };
    let task_id = task_id.clone();
    let secret = "Private goal never included in a room publication";
    room.peer
        .workspace_mut()
        .bind_workspace_task_private_instruction(
            &result,
            &workspace_task::private_instruction_reference(secret),
        )
        .unwrap();
    let texts = BTreeMap::from([(
        task_id.clone(),
        TaskText {
            instruction: Some(secret.into()),
            ..Default::default()
        },
    )]);
    let bytes = serde_json::to_vec(&texts).unwrap();
    tilo_files::native::atomic::write_private(
        &room.path.with_extension("private-task-text-v1.json"),
        &bytes,
    )
    .unwrap();
    room.store_peer().unwrap();
    room.persist().unwrap();
    let reopened = GovernedRoom::open(room.path.clone(), &cert, &key).unwrap();
    let reply = reopened
        .private_task_command(
            &cert,
            PrivateOperation::TaskInstructions {
                task: task_id.clone(),
                epoch: 0,
            },
        )
        .unwrap();
    let PrivateReply::TaskInstructions(body) = reply else {
        panic!("private instructions")
    };
    let text: TaskText = serde_json::from_str(&body).unwrap();
    assert_eq!(text.instruction.as_deref(), Some(secret));
    let public = workspace_task::snapshot(reopened.peer.admitted_room().unwrap()).unwrap();
    assert!(!serde_json::to_string(&public).unwrap().contains(secret));
    assert!(
        !reopened
            .peer
            .durable()
            .unwrap()
            .windows(secret.len())
            .any(|bytes| bytes == secret.as_bytes())
    );
    assert!(
        !std::fs::read(&reopened.path)
            .unwrap()
            .windows(secret.len())
            .any(|bytes| bytes == secret.as_bytes())
    );
    // The private file alone cannot replace the instruction committed by the person.
    let changed = BTreeMap::from([(
        task_id.clone(),
        TaskText {
            instruction: Some("replaced private instruction".into()),
            ..Default::default()
        },
    )]);
    tilo_files::native::atomic::write_private(
        &reopened.path.with_extension("private-task-text-v1.json"),
        &serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    assert!(
        reopened
            .private_task_command(
                &cert,
                PrivateOperation::TaskInstructions {
                    task: task_id.clone(),
                    epoch: 0,
                },
            )
            .is_err()
    );
    tilo_files::native::atomic::write_private(
        &reopened.path.with_extension("private-task-text-v1.json"),
        &bytes,
    )
    .unwrap();

    let mut forged = cert.clone();
    forged.device_label = "uncertified caller claim".into();
    assert!(
        reopened
            .private_task_command(
                &forged,
                PrivateOperation::TaskInstructions {
                    task: task_id.clone(),
                    epoch: 0,
                }
            )
            .is_err()
    );
    // A current owner's certificate is not itself physical-host exact-run
    // custody, and a native wrapper cannot watch a queued/nonexistent run.
    let watch = PrivateOperation::TaskMailboxWatch {
        task: task_id.clone(),
        epoch: 0,
        after_sequence: 0,
    };
    assert!(reopened.private_task_command(&cert, watch.clone()).is_err());
    assert!(reopened.private_task_watch(&cert, watch.clone()).is_err());
    assert!(
        reopened
            .private_task_watch(
                &cert,
                PrivateOperation::NativeTaskRead {
                    task: task_id.clone(),
                    epoch: 0,
                    agent: "scribe".into(),
                    evidence: Vec::new(),
                    operation: Box::new(watch),
                }
            )
            .is_err()
    );
    // A person cannot turn its owner's certificate into an agent sender.
    assert!(
        reopened
            .private_task_command(
                &cert,
                PrivateOperation::TaskSend {
                    recipient: task_id,
                    invocation: uuid::Uuid::new_v4(),
                    send: tilo_actions::task_messaging::TaskMessageSend {
                        run_epoch: 1,
                        text: "context".into()
                    },
                }
            )
            .is_err()
    );
}

#[test]
fn mailbox_validation_requires_exact_retained_run_cursor_and_never_model_delivery() {
    let address = TaskRunAddress {
        task_id: "task:mail".into(),
        run_epoch: 1,
        agent_id: "scribe".into(),
    };
    let id = uuid::Uuid::new_v4().to_string();
    let message = TaskMessage {
        version: 1,
        message_id: id.clone(),
        source: address.clone(),
        recipient: address.clone(),
        received_at: 1,
        sequence: 1,
        text: "private untrusted context".into(),
        status: TaskMessageStatus::Accepted,
    };
    let mut store = Mailboxes {
        version: 1,
        room: "room".into(),
        sequence: 1,
        messages: BTreeMap::from([(id.clone(), message)]),
        watermarks: BTreeMap::from([(serde_json::to_string(&address).unwrap(), 1)]),
        outputs: BTreeMap::new(),
    };
    store.validate("room").unwrap();
    let encoded = serde_json::to_vec(&store).unwrap();
    let restored: Mailboxes = serde_json::from_slice(&encoded).unwrap();
    restored.validate("room").unwrap();
    assert!(restored.validate("other-room").is_err());
    store.messages.get_mut(&id).unwrap().status = TaskMessageStatus::DeliveredToModel;
    assert!(store.validate("room").is_err());
    store.messages.clear();
    assert!(store.validate("room").is_err());
}
