//! The owner over its own file, as the standalone runtime keeps it.
use crate::testing::{APP, ROOM, raw_fixture};
use crate::{DocumentRead, NativeWorkspaceOwner, RoomId, open_file};
use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use ed25519_dalek::SigningKey;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::SystemTime,
};
use tilo_peer_state::{
    NeighbourMsg, TransportOrigin,
    owner_commit::{CommitOperation, OwnerCommitRequest},
    workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand, WorkspaceTaskCommandId},
};

struct Enrolled {
    _directory: tempfile::TempDir,
    path: PathBuf,
    owner: NativeWorkspaceOwner,
    room: RoomId,
    now: SystemTime,
    replay: NeighbourMsg,
}

/// Enroll through the file store and admit the governor's history.
fn enrolled() -> Enrolled {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("joined-rooms.json");
    let owner = open_file(&path).unwrap();
    let (intent, bootstrap, messages, governor, now) = raw_fixture();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    let enrollment = crate::testing::finalize(intent, pending, bootstrap).unwrap();
    owner.install(&enrollment, now).unwrap();
    let room = RoomId::parse(ROOM).unwrap();
    let replay = messages
        .iter()
        .rev()
        .find(|message| matches!(message, NeighbourMsg::AppDocument { .. }))
        .expect("fixture has signed app state")
        .clone();
    for message in messages.into_iter().skip(1) {
        owner
            .receive(
                room,
                TransportOrigin::RetainedRelayDelta(governor),
                message,
                now,
            )
            .unwrap();
    }
    Enrolled {
        _directory: directory,
        path,
        owner,
        room,
        now,
        replay,
    }
}

fn member() -> SigningKey {
    SigningKey::from_bytes(&[63; 32])
}

fn admit_test_agent(joined: &Enrolled, agent: &str) {
    set_test_agent_membership(joined, agent, true);
}

fn set_test_agent_membership(joined: &Enrolled, agent: &str, admitted: bool) {
    use collab_types::caps::{CapOverride, Scope, Subject, Verb};
    let key = SigningKey::from_bytes(&[61; 32]);
    let messages = joined
        .owner
        .with_current(joined.room, |view| {
            let before = view.peer().clone();
            let mut candidate = before.clone();
            if admitted {
                candidate.put_governed_capabilities(
                    &[
                        CapOverride::grant(Subject::agent(agent), Scope::Workspace, Verb::Write)
                            .with_id(format!("test-agent-{agent}")),
                    ],
                    &key,
                    joined.now,
                )?;
            } else {
                candidate.remove_governed_capabilities(
                    &[format!("test-agent-{agent}")],
                    &key,
                    joined.now,
                )?;
            }
            candidate.durable_mutation_messages(
                &before,
                &key,
                joined.now,
                view.generation().checked_add(1).unwrap(),
            )
        })
        .unwrap();
    let governor = joined
        .owner
        .with_current(joined.room, |view| Ok(view.governor().device_public_key))
        .unwrap();
    for message in messages {
        joined
            .owner
            .receive(
                joined.room,
                TransportOrigin::RetainedRelayDelta(governor),
                message,
                joined.now,
            )
            .unwrap();
    }
}

fn queued_task(
    joined: &Enrolled,
) -> (
    String,
    tilo_user_identity::DeviceCert,
    tilo_user_identity::DeviceCert,
) {
    admit_test_agent(joined, "scribe");
    let (caller, host) = joined
        .owner
        .with_current(joined.room, |view| {
            Ok((
                view.device().clone(),
                view.host().ok_or("test room has no pinned host")?.clone(),
            ))
        })
        .unwrap();
    let queue_invocation = uuid::Uuid::from_u128(91);
    joined
        .owner
        .commit_workspace_task_command(
            OwnerCommitRequest {
                room: ROOM.into(),
                operation: CommitOperation::WorkspaceTaskCommand {
                    invocation: queue_invocation,
                    authority: WorkspaceTaskAuthority::Person {
                        caller: caller.clone(),
                    },
                    command: WorkspaceTaskCommand::Queue {
                        title: "Prepare the release notes".into(),
                        assigned_agent: "scribe".into(),
                        route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                        resource: None,
                    },
                },
            },
            joined.now,
        )
        .unwrap();
    let result = joined
        .owner
        .workspace_task_result(
            joined.room,
            WorkspaceTaskCommandId {
                device: caller.device_public_key,
                invocation: queue_invocation,
            },
        )
        .unwrap()
        .unwrap();
    let tilo_desktop_model::WorkspaceTaskMutationResult::Queued { task_id } = result else {
        panic!("queue result")
    };
    (task_id, caller, host)
}

fn queued_and_started_task(joined: &Enrolled) -> (String, tilo_user_identity::DeviceCert) {
    let (task_id, caller, host) = queued_task(joined);
    joined
        .owner
        .commit_workspace_task_command(
            OwnerCommitRequest {
                room: ROOM.into(),
                operation: CommitOperation::WorkspaceTaskCommand {
                    invocation: uuid::Uuid::from_u128(92),
                    authority: WorkspaceTaskAuthority::Host { attestor: host },
                    command: WorkspaceTaskCommand::StartRun {
                        task_id: task_id.clone(),
                        agent_id: "scribe".into(),
                    },
                },
            },
            joined.now,
        )
        .unwrap();
    (task_id, caller)
}

fn edit(owner: &NativeWorkspaceOwner, room: RoomId, now: SystemTime) -> OwnerCommitRequest {
    edit_with(owner, room, now, "written while joined")
}

fn edit_with(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    now: SystemTime,
    topic: &str,
) -> OwnerCommitRequest {
    owner
        .with_current(room, |view| edit_from_view(view, now, topic))
        .unwrap()
}

fn edit_from_view(
    view: &crate::NativeCommittedView,
    now: SystemTime,
    topic: &str,
) -> Result<OwnerCommitRequest, String> {
    let mut chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
    let before = chat.get_heads();
    chat.put(ROOT, "topic", topic).unwrap();
    chat.commit();
    let message = view.peer().clone().publish_incremental(
        APP,
        "",
        chat.save_after(&before),
        &member(),
        now,
    )?;
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        return Err("test edit was not a signed document write".into());
    };
    Ok(OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::Document {
            app: APP.into(),
            record: String::new(),
            envelope,
        },
    })
}

#[test]
fn a_joined_room_is_kept_in_its_own_private_file_and_reopens() {
    let joined = enrolled();
    let policy_calls = |owner: &NativeWorkspaceOwner| {
        owner
            .with_current(joined.room, |view| {
                Ok(view
                    .peer()
                    .validated_media_policy()?
                    .ok_or("missing admitted policy")?
                    .calls
                    .len())
            })
            .unwrap()
    };
    assert_eq!(policy_calls(&joined.owner), 1);
    drop(joined.owner);
    let reopened = open_file(&joined.path).unwrap();
    assert_eq!(policy_calls(&reopened), 1);
    assert_eq!(reopened.rooms(), vec![joined.room]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&joined.path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "the record is owner-only");
    }
}

#[test]
fn chat_task_retry_after_owner_restart_keeps_one_executable_task() {
    let joined = enrolled();
    admit_test_agent(&joined, "scribe");
    let caller = joined
        .owner
        .with_current(joined.room, |view| Ok(view.device().clone()))
        .unwrap();
    let parent = uuid::Uuid::from_u128(901);
    let mut request = OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::from_u128(91),
            authority: WorkspaceTaskAuthority::Person {
                caller: caller.clone(),
            },
            command: WorkspaceTaskCommand::Queue {
                title: "Release notes".into(),
                assigned_agent: "scribe".into(),
                route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                resource: Some(Box::new(tilo_desktop_model::WorkspaceTaskResource {
                    app_id: Some("so.tilo.chat".into()),
                    app_label: Some("Chat".into()),
                    id: format!("chat.message:m_{parent}"),
                    kind: "chat.message".into(),
                    label: "Agent request".into(),
                    agent_request: Some(tilo_desktop_model::WorkspaceAgentRequest {
                        chat_record: "chat-record".into(),
                        channel_id: "agent-dm".into(),
                        message_id: format!("m_{parent}"),
                        instruction: "Prepare complete release notes".into(),
                        invocation: serde_json::from_value(serde_json::json!(parent)).unwrap(),
                    }),
                })),
            },
        },
    };
    joined
        .owner
        .commit_workspace_task_command(request.clone(), joined.now)
        .unwrap();
    let first = joined.owner.workspace_task_snapshot(joined.room).unwrap();
    drop(joined.owner);
    let reopened = open_file(&joined.path).unwrap();
    if let CommitOperation::WorkspaceTaskCommand { invocation, .. } = &mut request.operation {
        *invocation = uuid::Uuid::from_u128(92);
    }
    let receipt = reopened
        .commit_workspace_task_command(request.clone(), joined.now)
        .unwrap();
    receipt.validate_for(&request).unwrap();
    let state = reopened.workspace_task_snapshot(joined.room).unwrap();
    assert_eq!(state.tasks, first.tasks);
    assert_eq!(state.controls.values().next().unwrap().receipts.len(), 2);
    let result = reopened
        .workspace_task_result(
            joined.room,
            WorkspaceTaskCommandId {
                device: caller.device_public_key,
                invocation: uuid::Uuid::from_u128(92),
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        result,
        tilo_desktop_model::WorkspaceTaskMutationResult::Queued {
            task_id: first.tasks[0].id.clone(),
        }
    );
    let durable = open_file(&joined.path)
        .unwrap()
        .workspace_task_snapshot(joined.room)
        .unwrap();
    assert_eq!(durable.tasks, state.tasks);
    assert_eq!(durable.controls, state.controls);
}

#[test]
fn typed_task_queue_and_exact_retry_survive_owner_restart() {
    let joined = enrolled();
    admit_test_agent(&joined, "scribe");
    let caller = joined
        .owner
        .with_current(joined.room, |view| Ok(view.device().clone()))
        .unwrap();
    let invocation = uuid::Uuid::from_u128(91);
    let request = OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation,
            authority: WorkspaceTaskAuthority::Person {
                caller: caller.clone(),
            },
            command: WorkspaceTaskCommand::Queue {
                title: "Prepare the release notes".into(),
                assigned_agent: "scribe".into(),
                route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                resource: None,
            },
        },
    };
    let first = joined
        .owner
        .commit_workspace_task_command(request.clone(), joined.now)
        .unwrap();
    let retry = joined
        .owner
        .commit_workspace_task_command(request, joined.now)
        .unwrap();
    assert_eq!(first.binding, retry.binding);
    assert_eq!(first.generation, retry.generation);
    assert_eq!(first.aggregate_hash, retry.aggregate_hash);
    let id = WorkspaceTaskCommandId {
        device: caller.device_public_key,
        invocation,
    };
    assert!(
        joined
            .owner
            .workspace_task_result(joined.room, id)
            .unwrap()
            .is_some()
    );
    drop(joined.owner);
    let reopened = open_file(&joined.path).unwrap();
    let snapshot = reopened.workspace_task_snapshot(joined.room).unwrap();
    assert_eq!(snapshot.tasks.len(), 1);
    assert_eq!(snapshot.tasks[0].assigned_agent.as_deref(), Some("scribe"));
    assert!(
        reopened
            .workspace_task_result(joined.room, id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn run_bound_steering_refuses_stale_epoch_and_revision_without_owner_write() {
    let joined = enrolled();
    let (task_id, caller) = queued_and_started_task(&joined);
    let request = |invocation, epoch, after_revision| OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::from_u128(invocation),
            authority: WorkspaceTaskAuthority::Person {
                caller: caller.clone(),
            },
            command: WorkspaceTaskCommand::DirectionForRun {
                task_id: task_id.clone(),
                run_epoch: epoch,
                after_revision,
                text: "Prioritize compatibility changes".into(),
            },
        },
    };
    let before = std::fs::read(&joined.path).unwrap();
    assert!(
        joined
            .owner
            .commit_workspace_task_command(request(93, 2, 0), joined.now)
            .is_err()
    );
    assert_eq!(std::fs::read(&joined.path).unwrap(), before);
    let exact = request(94, 1, 0);
    let receipt = joined
        .owner
        .commit_workspace_task_command(exact.clone(), joined.now)
        .unwrap();
    receipt.validate_for(&exact).unwrap();
    let saved = std::fs::read(&joined.path).unwrap();
    assert!(
        joined
            .owner
            .commit_workspace_task_command(request(95, 1, 0), joined.now)
            .is_err()
    );
    assert_eq!(std::fs::read(&joined.path).unwrap(), saved);
    let reopened = open_file(&joined.path).unwrap();
    let state = reopened.workspace_task_snapshot(joined.room).unwrap();
    let control = state.controls.get(&task_id).unwrap();
    assert_eq!(control.directions.len(), 1);
    assert_eq!(control.directions[0].revision, 1);
    assert_eq!(
        control
            .active_run
            .as_ref()
            .unwrap()
            .applied_direction_revision,
        0
    );
    let retry = reopened
        .commit_workspace_task_command(exact, joined.now)
        .unwrap();
    assert_eq!(retry.binding, receipt.binding);
    assert_eq!(retry.generation, receipt.generation);
    assert_eq!(retry.aggregate_hash, receipt.aggregate_hash);
    assert_eq!(std::fs::read(&joined.path).unwrap(), saved);
}

#[test]
fn task_action_admission_and_direction_commit_are_one_serialized_owner_turn() {
    let joined = enrolled();
    let (task_id, caller) = queued_and_started_task(&joined);
    let admission_owner = joined.owner.clone();
    let nested_read_owner = joined.owner.clone();
    let room = joined.room;
    let admission_task = task_id.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let admission = std::thread::spawn(move || {
        admission_owner.with_task_action_admission(
            room,
            &admission_task,
            "scribe",
            1,
            0,
            move || {
                nested_read_owner.workspace_task_snapshot(room)?;
                entered_tx.send(()).map_err(|error| error.to_string())?;
                release_rx.recv().map_err(|error| error.to_string())?;
                Ok(())
            },
        )
    });
    entered_rx.recv().unwrap();

    let direction_owner = joined.owner.clone();
    let direction_task = task_id.clone();
    let now = joined.now;
    let (attempted_tx, attempted_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let direction = std::thread::spawn(move || {
        attempted_tx.send(()).unwrap();
        let result = direction_owner.commit_workspace_task_command(
            OwnerCommitRequest {
                room: ROOM.into(),
                operation: CommitOperation::WorkspaceTaskCommand {
                    invocation: uuid::Uuid::from_u128(93),
                    authority: WorkspaceTaskAuthority::Person { caller },
                    command: WorkspaceTaskCommand::DirectionForRun {
                        task_id: direction_task,
                        run_epoch: 1,
                        after_revision: 0,
                        text: "Keep the migration warning prominent".into(),
                    },
                },
            },
            now,
        );
        finished_tx.send(result.is_ok()).unwrap();
    });
    attempted_rx.recv().unwrap();
    assert!(matches!(
        finished_rx.recv_timeout(std::time::Duration::from_millis(100)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    release_tx.send(()).unwrap();
    admission.join().unwrap().unwrap();
    direction.join().unwrap();
    assert!(finished_rx.recv().unwrap());

    assert!(
        joined
            .owner
            .with_task_action_admission(joined.room, &task_id, "scribe", 1, 0, || Ok(()))
            .is_err()
    );
    let host = joined
        .owner
        .with_current(joined.room, |view| {
            view.host()
                .cloned()
                .ok_or_else(|| "test room has no pinned host".to_owned())
        })
        .unwrap();
    joined
        .owner
        .commit_workspace_task_command(
            OwnerCommitRequest {
                room: ROOM.into(),
                operation: CommitOperation::WorkspaceTaskCommand {
                    invocation: uuid::Uuid::from_u128(94),
                    authority: WorkspaceTaskAuthority::Agent {
                        attestor: host,
                        agent_id: "scribe".into(),
                    },
                    command: WorkspaceTaskCommand::AppliedAck {
                        task_id: task_id.clone(),
                        run_epoch: 1,
                        revision: 1,
                    },
                },
            },
            joined.now,
        )
        .unwrap();
    joined
        .owner
        .with_task_action_admission(joined.room, &task_id, "scribe", 1, 1, || Ok(()))
        .unwrap();
}

fn sealed_edit(joined: &Enrolled) -> (OwnerCommitRequest, Vec<u8>) {
    let mut request = edit(&joined.owner, joined.room, joined.now);
    let CommitOperation::Document { envelope, .. } = &mut request.operation else {
        panic!("test edit is a document");
    };
    let opened = envelope.payload.clone();
    envelope.payload = b"sealed local document".to_vec();
    envelope.encryption = Some(collab_types::EncryptionMetadata::mls_application(
        1,
        collab_types::document_channel_id(APP, ROOM, ""),
    ));
    collab_types::sign_op(envelope, &member()).unwrap();
    (request, opened)
}

#[test]
fn opened_sealed_local_write_refuses_bad_plaintext_without_publication() {
    let joined = enrolled();
    let (request, _) = sealed_edit(&joined);
    let before = std::fs::read(&joined.path).unwrap();
    let mut opened = 0;
    let result = joined.owner.commit_document_with_opening(
        request,
        &mut |_| {
            opened += 1;
            Ok(
                collab_types::DocumentMutation::Incremental(b"not Automerge storage".to_vec())
                    .encode()
                    .unwrap(),
            )
        },
        joined.now,
    );
    assert!(
        result.is_err(),
        "sealed ciphertext must not hide malformed plaintext"
    );
    assert_eq!(
        opened, 1,
        "the actual owner must open after envelope validation"
    );
    assert_eq!(std::fs::read(&joined.path).unwrap(), before);
}

#[test]
fn opened_sealed_local_write_refuses_missing_dependencies_without_publication() {
    let joined = enrolled();
    let (request, _) = sealed_edit(&joined);
    let before = std::fs::read(&joined.path).unwrap();
    let mut alien = AutoCommit::new();
    alien.put(ROOT, "unadmitted", "basis").unwrap();
    alien.commit();
    let basis = alien.get_heads();
    alien.put(ROOT, "topic", "orphan").unwrap();
    alien.commit();
    let plaintext = collab_types::DocumentMutation::Incremental(alien.save_after(&basis))
        .encode()
        .unwrap();
    assert!(
        joined
            .owner
            .commit_document_with_opening(request, &mut |_| Ok(plaintext.clone()), joined.now,)
            .is_err(),
        "a sealed write cannot admit an uncommitted dependency basis"
    );
    assert_eq!(std::fs::read(&joined.path).unwrap(), before);
}

#[test]
fn opened_sealed_local_write_verifies_signature_before_opening() {
    let joined = enrolled();
    let (mut request, plaintext) = sealed_edit(&joined);
    let CommitOperation::Document { envelope, .. } = &mut request.operation else {
        panic!("test edit is a document");
    };
    envelope.payload.push(0);
    let before = std::fs::read(&joined.path).unwrap();
    let mut opened = 0;
    assert!(
        joined
            .owner
            .commit_document_with_opening(
                request,
                &mut |_| {
                    opened += 1;
                    Ok(plaintext.clone())
                },
                joined.now,
            )
            .is_err()
    );
    assert_eq!(
        opened, 0,
        "tampered envelopes must never reach channel opening"
    );
    assert_eq!(std::fs::read(&joined.path).unwrap(), before);
}

#[test]
fn opened_sealed_local_write_recovers_from_key_refusal_and_reopens_exact_history() {
    let joined = enrolled();
    let (request, plaintext) = sealed_edit(&joined);
    let before = std::fs::read(&joined.path).unwrap();
    assert!(
        joined
            .owner
            .commit_document_with_opening(
                request.clone(),
                &mut |_| Err("held channel epoch unavailable".into()),
                joined.now,
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&joined.path).unwrap(), before);
    let receipt = joined
        .owner
        .commit_document_with_opening(request.clone(), &mut |_| Ok(plaintext.clone()), joined.now)
        .unwrap();
    receipt.validate_for(&request).unwrap();
    let committed = std::fs::read(&joined.path).unwrap();
    let reopened = open_file(&joined.path).unwrap();
    reopened
        .with_current(joined.room, |view| {
            let bytes = view
                .peer()
                .document(APP, "")
                .ok_or("opened document missing")?;
            let doc = AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
            assert_eq!(
                doc.get(ROOT, "topic").unwrap().unwrap().0.to_string(),
                "\"written while joined\""
            );
            Ok(())
        })
        .unwrap();
    let retry = reopened
        .commit_document_with_opening(
            request,
            &mut |_| panic!("an exact durable retry must not reopen ciphertext"),
            joined.now,
        )
        .unwrap();
    assert_eq!(retry.generation, receipt.generation);
    assert_eq!(std::fs::read(&joined.path).unwrap(), committed);
}

#[test]
fn a_local_write_is_durable_with_its_outgoing_custody_before_its_receipt() {
    let joined = enrolled();
    let request = edit(&joined.owner, joined.room, joined.now);
    let receipt = joined
        .owner
        .commit_document(request.clone(), joined.now)
        .unwrap();
    let committed = std::fs::read(&joined.path).unwrap();
    let retry = joined.owner.commit_document(request, joined.now).unwrap();
    assert_eq!(retry.generation, receipt.generation);
    assert_eq!(
        std::fs::read(&joined.path).unwrap(),
        committed,
        "an exact retry does not rewrite"
    );
    let outgoing = |owner: &NativeWorkspaceOwner| {
        owner
            .with_outgoing(joined.room, joined.now, |_, outgoing| {
                Ok(outgoing.entries().count())
            })
            .unwrap()
    };
    assert_eq!(outgoing(&joined.owner), 1);
    drop(joined.owner);
    assert_eq!(outgoing(&open_file(&joined.path).unwrap()), 1);
}

#[test]
fn prepared_local_write_uses_current_sequence_after_an_intervening_commit() {
    let joined = enrolled();
    let stale = edit_with(&joined.owner, joined.room, joined.now, "stale edit");
    let first = edit_with(
        &joined.owner,
        joined.room,
        joined.now,
        "first committed edit",
    );
    joined.owner.commit_document(first, joined.now).unwrap();
    assert_eq!(
        joined.owner.commit_document(stale, joined.now).unwrap_err(),
        "signed operation conflicts with exact admitted sequence"
    );

    let receipt = joined
        .owner
        .commit_document_with_current(
            joined.room,
            |view| edit_from_view(view, joined.now, "edit from current owner turn"),
            joined.now,
        )
        .unwrap();
    assert!(receipt.generation > 1);
    joined
        .owner
        .with_current(joined.room, |view| {
            let chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
            assert_eq!(
                chat.get(ROOT, "topic").unwrap().unwrap().0.as_str(),
                Some("edit from current owner turn")
            );
            Ok(())
        })
        .unwrap();
}

/// Commit one original edit that the governor has not yet admitted, then give
/// the route a fresh governor checkpoint that does not cover it, so the next
/// `reconcile_outgoing` signs a recovery projection at the dense `floor + 1`.
fn with_pending_original(joined: &Enrolled) {
    let governor_key = SigningKey::from_bytes(&[61; 32]);
    let mut remote = joined
        .owner
        .with_current(joined.room, |view| Ok(view.peer().clone()))
        .unwrap();
    let original = edit_with(
        &joined.owner,
        joined.room,
        joined.now,
        "original awaiting ack",
    );
    joined.owner.commit_document(original, joined.now).unwrap();
    for message in remote
        .durable_bootstrap_messages(&governor_key, joined.now, 600)
        .unwrap()
    {
        joined
            .owner
            .receive(
                joined.room,
                TransportOrigin::RelayDelta(governor_key.verifying_key().to_bytes()),
                message,
                joined.now,
            )
            .unwrap();
    }
}

fn projections(joined: &Enrolled) -> usize {
    joined
        .owner
        .with_current(joined.room, |view| {
            Ok(crate::testing::projection(view)?.len())
        })
        .unwrap()
}

/// A write signed outside the owner turn the way every SDK document signer
/// signs it: with the process-wide unordered sequence, not `floor + 1`.
fn unordered_edit(joined: &Enrolled, topic: &str) -> OwnerCommitRequest {
    use collab_types::{
        DocumentMutation, Ed25519PublicKey, OpClass, SignedOp, document_artifact_id,
        document_attestation_id, next_unordered_document_sequence, sign_op,
    };
    let changes = joined
        .owner
        .with_current(joined.room, |view| {
            let mut chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
            let before = chat.get_heads();
            chat.put(ROOT, "topic", topic).unwrap();
            chat.commit();
            Ok(chat.save_after(&before))
        })
        .unwrap();
    let key = member();
    let mut envelope = SignedOp::new_unsigned(
        DocumentMutation::Incremental(changes).encode().unwrap(),
        document_artifact_id(APP, ROOM, ""),
        Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
        document_attestation_id(APP, ROOM, ""),
        OpClass::Splice,
        joined.now,
        next_unordered_document_sequence(joined.now),
    );
    sign_op(&mut envelope, &key).unwrap();
    OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::Document {
            app: APP.into(),
            record: String::new(),
            envelope: Box::new(envelope),
        },
    }
}

#[test]
fn dense_snapshot_sequence_is_stale_after_an_intervening_recovery_projection() {
    let joined = enrolled();
    with_pending_original(&joined);
    let dense = edit_with(&joined.owner, joined.room, joined.now, "dense edit");
    assert_eq!(projections(&joined), 0);
    joined
        .owner
        .reconcile_outgoing(joined.room, &member(), joined.now)
        .unwrap();
    assert_eq!(
        projections(&joined),
        1,
        "the owner's own turn must sign a recovery projection at floor + 1"
    );
    assert_eq!(
        joined.owner.commit_document(dense, joined.now).unwrap_err(),
        "native local document signed sequence is stale"
    );
}

#[test]
fn unordered_sequence_survives_an_intervening_recovery_projection() {
    let joined = enrolled();
    with_pending_original(&joined);
    let request = unordered_edit(&joined, "unordered edit");
    assert_eq!(projections(&joined), 0);
    joined
        .owner
        .reconcile_outgoing(joined.room, &member(), joined.now)
        .unwrap();
    assert_eq!(
        projections(&joined),
        1,
        "the owner's own turn must sign a recovery projection at floor + 1"
    );
    joined
        .owner
        .commit_document(request.clone(), joined.now)
        .unwrap();
    let CommitOperation::Document { envelope, .. } = &request.operation else {
        panic!("the unordered edit is a document write")
    };
    joined
        .owner
        .with_current(joined.room, |view| {
            assert!(
                view.peer()
                    .lookup_exact_local_document(APP, "", envelope, joined.now)
                    .map_err(|error| error.to_string())?
                    .is_some(),
                "the caller-signed write must leave exact admission evidence"
            );
            let chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
            assert_eq!(
                chat.get(ROOT, "topic").unwrap().unwrap().0.as_str(),
                Some("unordered edit")
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn an_uncertain_write_fences_every_room_until_reopened() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    let joined = enrolled();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let _subscription = joined.owner.observe_fences(Arc::new(move |_, _| {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    let request = edit(&joined.owner, joined.room, joined.now);
    assert!(
        with_failure(Checkpoint::AfterPublish, || joined
            .owner
            .commit_document(request.clone(), joined.now))
        .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(joined.owner.health().is_err());
    assert!(joined.owner.contains_room(joined.room));
    assert!(joined.owner.commit_document(request, joined.now).is_err());
    drop(joined.owner);
    open_file(&joined.path).unwrap().health().unwrap();
}

#[test]
fn a_pending_enrollment_without_a_slot_survives_restart_and_cancels() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("joined-rooms.json");
    let (intent, _, _, _, now) = raw_fixture();
    let id = open_file(&path)
        .unwrap()
        .prepare_enrollment_into(&intent, None, now)
        .unwrap();
    let owner = open_file(&path).unwrap();
    assert!(owner.contains_room(id.room()));
    let invitation =
        tilo_peer_state::join::encode_invitation_text(intent.verified_join().signed_invitation());
    assert_eq!(
        owner
            .pending_enrollment_for_invitation(&invitation)
            .unwrap()
            .map(|pending| pending.id().clone()),
        Some(id.clone()),
        "a host without slots finds its retained proof by the invitation it redeems"
    );
    assert!(
        owner
            .pending_enrollment_for_invitation("another invitation")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        owner.prepare_enrollment_into(&intent, None, now).unwrap(),
        id,
        "a retry reuses the exact persisted proof"
    );
    assert!(owner.cancel_enrollment(id.room(), &id).unwrap());
    assert!(
        open_file(&path)
            .unwrap()
            .pending_enrollment(id.room())
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_member_reads_the_admitted_document_and_a_stranger_does_not() {
    let joined = enrolled();
    let member = member().verifying_key().to_bytes();
    let stranger = SigningKey::from_bytes(&[99; 32]).verifying_key().to_bytes();
    let read = |reader, record: &str, known, maximum| {
        joined.owner.with_current(joined.room, |view| {
            view.read_document(reader, APP, record, known, maximum, joined.now)
        })
    };
    let Ok(DocumentRead::Captured(document)) = read(member, "", None, usize::MAX) else {
        panic!("a member reads what the room admitted");
    };
    let doc = AutoCommit::load(&document.bytes).unwrap();
    assert!(doc.get(ROOT, "channels").unwrap().is_some());
    assert!(matches!(
        read(member, "", Some(document.revision), usize::MAX),
        Ok(DocumentRead::Unchanged(revision)) if revision == document.revision
    ));
    assert!(
        read(member, "", None, document.bytes.len() - 1).is_err(),
        "a document larger than the reader can hold is refused"
    );
    assert!(
        read(stranger, "", None, usize::MAX).is_err(),
        "a device outside the room reads nothing"
    );
    assert!(read(member, "uuid:absent", None, usize::MAX).is_err());
}

#[test]
fn an_unchanged_committed_read_does_not_restore_the_room() {
    let joined = enrolled();
    let member = member().verifying_key().to_bytes();
    let stranger = SigningKey::from_bytes(&[99; 32]).verifying_key().to_bytes();
    let read = |reader, known| {
        joined.owner.with_current(joined.room, |view| {
            view.read_document(reader, APP, "", known, usize::MAX, joined.now)
        })
    };
    crate::document_read::reset_room_restores();
    let Ok(DocumentRead::Captured(document)) = read(member, None) else {
        panic!("the first read admits the document");
    };
    assert!(
        crate::document_read::room_restores() > 0,
        "a first read restores the room so the counter can see a later skip"
    );
    let restores = crate::document_read::room_restores();
    assert!(matches!(
        read(member, Some(document.revision)),
        Ok(DocumentRead::Unchanged(revision)) if revision == document.revision
    ));
    assert_eq!(
        crate::document_read::room_restores(),
        restores,
        "an idle reread of the same revision must not restore the room"
    );
    assert!(
        read(stranger, Some(document.revision)).is_err(),
        "a device outside the room cannot reuse a revision it was never given"
    );
}

/// What a sealed write says, which no file of the keyless owner may hold.
const SECRET: &str = "only key holders read this";

/// A channel's key holder, and its content keys before and after a rotation.
fn rotated_channel() -> (
    channel_crypto::ChannelContentKeys,
    channel_crypto::ChannelContentKeys,
) {
    let channel = collab_types::document_channel_id(APP, ROOM, "");
    let mut group = channel_crypto::ChannelGroup::create(
        channel,
        channel_crypto::MemberKey::generate("steward").unwrap(),
    )
    .unwrap();
    let before = group.content_keys().unwrap();
    group
        .add_member(
            channel_crypto::MemberKey::generate("reader")
                .unwrap()
                .fresh_key_package()
                .unwrap(),
        )
        .unwrap();
    (before, group.content_keys().unwrap())
}

/// Commit a write sealed under `keys`, as an SDK app seals it, at `now`.
fn commit_sealed(
    joined: &Enrolled,
    keys: &channel_crypto::ChannelContentKeys,
) -> collab_types::SignedOp {
    commit_sealed_as(joined, keys, collab_types::ContentRetention::Delta)
}

/// Commit a write sealed under `keys` with the signed storage class `class`.
fn commit_sealed_as(
    joined: &Enrolled,
    keys: &channel_crypto::ChannelContentKeys,
    class: collab_types::ContentRetention,
) -> collab_types::SignedOp {
    let OwnerCommitRequest {
        operation: CommitOperation::Document { mut envelope, .. },
        ..
    } = edit_with(&joined.owner, joined.room, joined.now, SECRET)
    else {
        panic!("a document write")
    };
    let (metadata, sealed) = keys
        .seal_content(envelope.artifact, class, &envelope.payload)
        .unwrap();
    envelope.payload = sealed;
    envelope.encryption = Some(metadata);
    collab_types::sign_op(&mut envelope, &member()).unwrap();
    joined
        .owner
        .commit_document(
            OwnerCommitRequest {
                room: ROOM.into(),
                operation: CommitOperation::Document {
                    app: APP.into(),
                    record: String::new(),
                    envelope: envelope.clone(),
                },
            },
            joined.now,
        )
        .unwrap();
    *envelope
}

fn sent(owner: &NativeWorkspaceOwner, room: RoomId, now: SystemTime) -> Vec<Vec<u8>> {
    owner
        .with_outgoing(room, now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| entry.bytes().to_vec())
                .collect())
        })
        .unwrap()
}

/// Native publication classifies through the one host classifier browser
/// peers use, so a sealed checkpoint is a checkpoint to it there too. A
/// member still never replaces the record's retained checkpoint under its own
/// relay author: the sealed checkpoint leaves as a delta after it, while a
/// cleartext snapshot stays a checkpoint the member never releases.
#[test]
fn native_sealed_checkpoint_is_classified_like_browser() {
    use tilo_peer_state::transport::{Retention, retention};
    let joined = enrolled();
    let (keys, _) = rotated_channel();
    let envelope = commit_sealed_as(&joined, &keys, collab_types::ContentRetention::Checkpoint);
    let [bytes] = sent(&joined.owner, joined.room, joined.now)
        .try_into()
        .expect("the committed sealed checkpoint is released");
    let message: NeighbourMsg = postcard::from_bytes(&bytes).unwrap();
    let NeighbourMsg::AppDocument { envelope: sent, .. } = &message else {
        panic!("a document write")
    };
    assert_eq!(**sent, envelope);

    let (channel, class) = retention(&message, false).expect("sealed state is retained");
    assert_eq!(class, Retention::Checkpoint, "the browser peer's class");
    let (_, recorded, incremental) = crate::publication::metadata(&message).unwrap();
    assert_eq!(recorded, channel, "the same host channel as a browser peer");
    assert!(
        incremental,
        "a member's sealed checkpoint is published as a delta"
    );
    let published = joined
        .owner
        .with_outgoing(joined.room, joined.now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| (entry.channel(), entry.retention()))
                .collect::<Vec<_>>())
        })
        .unwrap();
    assert_eq!(published, [(channel, Retention::Delta)]);

    let mut snapshot = message.clone();
    let mut single_use = message;
    if let NeighbourMsg::AppDocument { envelope, .. } = &mut snapshot {
        envelope.encryption = None;
        envelope.payload = collab_types::DocumentMutation::Snapshot(vec![1])
            .encode()
            .unwrap();
    }
    assert_eq!(
        retention(&snapshot, false).map(|(_, class)| class),
        Some(Retention::Checkpoint)
    );
    assert!(
        !crate::publication::metadata(&snapshot).unwrap().2,
        "a cleartext snapshot stays the governor's checkpoint"
    );
    if let NeighbourMsg::AppDocument { envelope, .. } = &mut single_use {
        envelope.encryption = Some(collab_types::EncryptionMetadata::mls_application(
            0,
            collab_types::document_channel_id(APP, ROOM, ""),
        ));
    }
    assert!(
        crate::publication::metadata(&single_use).is_err(),
        "a single-use MLS message has no retention class"
    );
}

#[test]
fn an_expired_sealed_write_is_renewed_through_its_key_holder_and_replaces_its_original() {
    let joined = enrolled();
    let (before, current) = rotated_channel();
    let original = commit_sealed(&joined, &before);
    let later = joined
        .now
        .checked_add(std::time::Duration::from_secs(301))
        .unwrap();
    assert!(sent(&joined.owner, joined.room, later).is_empty());

    let mut asked = 0;
    joined
        .owner
        .renew_outgoing(
            joined.room,
            &member(),
            &mut |op| {
                asked += 1;
                let metadata = op.encryption.ok_or("not sealed")?;
                let plaintext = current
                    .open_content(op.artifact, &metadata, &op.payload)
                    .map_err(|e| e.to_string())?;
                current
                    .seal_content(
                        op.artifact,
                        collab_types::ContentRetention::Delta,
                        &plaintext,
                    )
                    .map_err(|e| e.to_string())
            },
            later,
        )
        .unwrap();
    assert_eq!(asked, 1);
    let status = joined.owner.outgoing_status(joined.room, later).unwrap();
    assert_eq!(status.pending, 1, "the renewal replaces its original");
    let [bytes] = sent(&joined.owner, joined.room, later).try_into().unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = postcard::from_bytes(&bytes).unwrap() else {
        panic!("a document write")
    };
    collab_types::verify_op(&envelope).unwrap();
    assert_eq!(envelope.seq, original.seq.checked_add(1).unwrap());
    assert_eq!(envelope.timestamp, later);
    let metadata = envelope.encryption.unwrap();
    assert_eq!(metadata.epoch, current.epoch());
    assert_eq!(
        current
            .open_current_content(envelope.artifact, &metadata, &envelope.payload)
            .unwrap()
            .as_slice(),
        before
            .open_content(
                original.artifact,
                &original.encryption.unwrap(),
                &original.payload
            )
            .unwrap()
            .as_slice(),
        "the renewal carries the same content under the current epoch"
    );

    drop(joined.owner);
    let reopened = open_file(&joined.path).unwrap();
    assert_eq!(sent(&reopened, joined.room, later), vec![bytes]);
    for file in std::fs::read_dir(joined.path.parent().unwrap()).unwrap() {
        let contents = std::fs::read(file.unwrap().path()).unwrap();
        assert!(
            !contents
                .windows(SECRET.len())
                .any(|window| window == SECRET.as_bytes()),
            "the keyless owner's files hold the plaintext"
        );
    }
}

#[test]
fn a_refused_renewal_leaves_the_sealed_write_in_custody_as_it_was() {
    let joined = enrolled();
    let (keys, _) = rotated_channel();
    commit_sealed(&joined, &keys);
    let committed = std::fs::read(&joined.path).unwrap();
    let later = joined
        .now
        .checked_add(std::time::Duration::from_secs(301))
        .unwrap();
    joined
        .owner
        .renew_outgoing(
            joined.room,
            &member(),
            &mut |_| Err("this device holds no current key".into()),
            later,
        )
        .unwrap();
    assert_eq!(std::fs::read(&joined.path).unwrap(), committed);
    assert_eq!(
        joined
            .owner
            .outgoing_status(joined.room, later)
            .unwrap()
            .pending,
        1
    );
    assert!(sent(&joined.owner, joined.room, later).is_empty());
    assert!(
        joined
            .owner
            .renew_outgoing(
                joined.room,
                &SigningKey::from_bytes(&[99; 32]),
                &mut |_| panic!("another signer's renewal asked the key holder"),
                later,
            )
            .is_err()
    );
}

#[test]
fn removed_agent_cannot_receive_a_direction_for_its_retained_run() {
    let joined = enrolled();
    let (task_id, caller) = queued_and_started_task(&joined);
    set_test_agent_membership(&joined, "scribe", false);
    joined
        .owner
        .with_current(joined.room, |view| {
            let (_, _, caps) = tilo_peer_state::room::extract_governed_fields(
                view.peer().admitted_room().unwrap(),
            );
            assert!(
                caps.iter()
                    .all(|cap| cap.subject != collab_types::caps::Subject::agent("scribe")),
                "the signed removal must actually remove the agent"
            );
            Ok(())
        })
        .unwrap();
    let before = std::fs::read(&joined.path).unwrap();
    let result = joined.owner.commit_workspace_task_command(
        OwnerCommitRequest {
            room: ROOM.into(),
            operation: CommitOperation::WorkspaceTaskCommand {
                invocation: uuid::Uuid::from_u128(501),
                authority: WorkspaceTaskAuthority::Person { caller },
                command: WorkspaceTaskCommand::DirectionForRun {
                    task_id,
                    run_epoch: 1,
                    after_revision: 0,
                    text: "Prioritize compatibility".into(),
                },
            },
        },
        joined.now,
    );
    assert!(
        result.is_err(),
        "a retained lease must not admit steering to a removed actor"
    );
    assert_eq!(std::fs::read(&joined.path).unwrap(), before);
}

#[test]
fn person_cancel_is_frontier_fenced_and_durable_after_restart() {
    let joined = enrolled();
    let (task_id, caller) = queued_and_started_task(&joined);
    let cancel = |invocation, epoch, revision| OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::from_u128(invocation),
            authority: WorkspaceTaskAuthority::Person {
                caller: caller.clone(),
            },
            command: WorkspaceTaskCommand::CancelForRun {
                task_id: task_id.clone(),
                run_epoch: epoch,
                after_revision: revision,
            },
        },
    };
    let before = std::fs::read(&joined.path).unwrap();
    assert!(
        joined
            .owner
            .commit_workspace_task_command(cancel(601, 0, 0), joined.now)
            .is_err()
    );
    assert!(
        joined
            .owner
            .commit_workspace_task_command(cancel(602, 1, 1), joined.now)
            .is_err()
    );
    assert_eq!(std::fs::read(&joined.path).unwrap(), before);
    let request = cancel(603, 1, 0);
    joined
        .owner
        .commit_workspace_task_command(request.clone(), joined.now)
        .unwrap();
    let bytes = std::fs::read(&joined.path).unwrap();
    assert!(
        joined
            .owner
            .with_task_action_admission(
                joined.room,
                &task_id,
                "scribe",
                1,
                0,
                || -> Result<(), String> { panic!("cancelled task admitted an action") }
            )
            .is_err()
    );
    joined
        .owner
        .commit_workspace_task_command(request, joined.now)
        .unwrap();
    assert_eq!(std::fs::read(&joined.path).unwrap(), bytes);
    let reopened = open_file(&joined.path).unwrap();
    let snapshot = reopened.workspace_task_snapshot(joined.room).unwrap();
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .unwrap();
    assert_eq!(
        task.status,
        tilo_desktop_model::WorkspaceTaskStatus::Cancelled
    );
    assert_eq!(task.audit.last().unwrap().actor, task.created_by);
    assert!(snapshot.controls[&task_id].active_run.is_none());
    assert!(snapshot.controls[&task_id].execution.is_none());
    assert!(
        reopened
            .workspace_task_result(
                joined.room,
                WorkspaceTaskCommandId {
                    device: caller.device_public_key,
                    invocation: uuid::Uuid::from_u128(603)
                }
            )
            .unwrap()
            .is_some()
    );
    assert!(
        reopened
            .commit_workspace_task_command(cancel(604, 1, 0), joined.now)
            .is_err()
    );
}

fn manual_resource(id: &str) -> tilo_desktop_model::WorkspaceTaskResource {
    tilo_desktop_model::WorkspaceTaskResource {
        app_id: Some("so.tilo.chip-design".into()),
        app_label: Some("Chip Design".into()),
        id: id.into(),
        kind: "chip_design.verification".into(),
        label: "Mapped tile evidence".into(),
        agent_request: None,
    }
}

fn manual_request(
    joined: &Enrolled,
    invocation: u128,
    command: WorkspaceTaskCommand,
) -> OwnerCommitRequest {
    let caller = joined
        .owner
        .with_current(joined.room, |view| Ok(view.device().clone()))
        .unwrap();
    OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::from_u128(invocation),
            authority: WorkspaceTaskAuthority::Person { caller },
            command,
        },
    }
}

#[test]
fn manual_task_owner_replay_restart_and_attachment_are_canonical() {
    let joined = enrolled();
    let request = manual_request(
        &joined,
        701,
        WorkspaceTaskCommand::CreateManual {
            title: "Review mapped checkpoint evidence".into(),
            resource: None,
        },
    );
    let receipt = joined
        .owner
        .commit_workspace_task_command(request.clone(), joined.now)
        .unwrap();
    receipt.validate_for(&request).unwrap();
    let snapshot = joined.owner.workspace_task_snapshot(joined.room).unwrap();
    assert_eq!(snapshot.tasks.len(), 1);
    let task = &snapshot.tasks[0];
    let task_id = task.id.clone();
    assert_eq!(task.route, tilo_desktop_model::WorkspaceTaskRoute::Manual);
    assert!(task.assigned_agent.is_none());
    assert_eq!(task.status, tilo_desktop_model::WorkspaceTaskStatus::Queued);
    assert!(task.resource.is_none());
    let author = joined
        .owner
        .with_current(joined.room, |view| {
            Ok(view
                .peer()
                .permissions(view.device().device_public_key, joined.now)?
                .2)
        })
        .unwrap();
    assert_eq!(task.created_by, author);
    assert_eq!(task.audit[0].actor, author);
    assert_eq!(snapshot.controls[&task_id].run_epoch, 0);
    assert!(snapshot.controls[&task_id].active_run.is_none());
    assert_eq!(snapshot.controls[&task_id].receipts.len(), 1);
    joined
        .owner
        .commit_workspace_task_command(request.clone(), joined.now)
        .unwrap();
    assert_eq!(
        joined.owner.workspace_task_snapshot(joined.room).unwrap(),
        snapshot
    );
    let attach = manual_request(
        &joined,
        702,
        WorkspaceTaskCommand::AttachResource {
            task_id: task_id.clone(),
            expected_resource_id: None,
            resource: Box::new(manual_resource("chip_design.verification:actual-pinned-id")),
        },
    );
    joined
        .owner
        .commit_workspace_task_command(attach.clone(), joined.now)
        .unwrap();
    let attached = joined.owner.workspace_task_snapshot(joined.room).unwrap();
    assert_eq!(
        attached.tasks[0].resource.as_ref().unwrap().id,
        "chip_design.verification:actual-pinned-id"
    );
    assert_eq!(attached.tasks[0].audit.len(), 2);
    assert_eq!(attached.tasks[0].audit[1].actor, author);
    assert!(attached.controls[&task_id].active_run.is_none());
    joined
        .owner
        .commit_workspace_task_command(attach, joined.now)
        .unwrap();
    assert_eq!(
        joined.owner.workspace_task_snapshot(joined.room).unwrap(),
        attached
    );
    drop(joined.owner);
    let reopened = open_file(&joined.path).unwrap();
    assert_eq!(
        reopened.workspace_task_snapshot(joined.room).unwrap(),
        attached
    );
    reopened
        .commit_workspace_task_command(request, joined.now)
        .unwrap();
    assert_eq!(
        reopened.workspace_task_snapshot(joined.room).unwrap(),
        attached
    );
}

#[test]
fn manual_attachment_cannot_overwrite_unobserved_or_managed_owner_state() {
    let joined = enrolled();
    let create = manual_request(
        &joined,
        710,
        WorkspaceTaskCommand::CreateManual {
            title: "Review mapping".into(),
            resource: Some(Box::new(manual_resource("chip_design.verification:first"))),
        },
    );
    joined
        .owner
        .commit_workspace_task_command(create, joined.now)
        .unwrap();
    let before = joined.owner.workspace_task_snapshot(joined.room).unwrap();
    let stale = manual_request(
        &joined,
        711,
        WorkspaceTaskCommand::AttachResource {
            task_id: before.tasks[0].id.clone(),
            expected_resource_id: None,
            resource: Box::new(manual_resource("chip_design.verification:second")),
        },
    );
    assert!(
        joined
            .owner
            .commit_workspace_task_command(stale, joined.now)
            .unwrap_err()
            .contains("changed since")
    );
    assert_eq!(
        joined.owner.workspace_task_snapshot(joined.room).unwrap(),
        before
    );
    let exact = manual_request(
        &joined,
        712,
        WorkspaceTaskCommand::AttachResource {
            task_id: before.tasks[0].id.clone(),
            expected_resource_id: Some("chip_design.verification:first".into()),
            resource: Box::new(manual_resource("chip_design.verification:second")),
        },
    );
    joined
        .owner
        .commit_workspace_task_command(exact, joined.now)
        .unwrap();
    let (managed_id, _) = queued_and_started_task(&joined);
    let before = joined.owner.workspace_task_snapshot(joined.room).unwrap();
    let managed = manual_request(
        &joined,
        713,
        WorkspaceTaskCommand::AttachResource {
            task_id: managed_id,
            expected_resource_id: None,
            resource: Box::new(manual_resource("chip_design.verification:second")),
        },
    );
    assert!(
        joined
            .owner
            .commit_workspace_task_command(managed, joined.now)
            .unwrap_err()
            .contains("managed, leased")
    );
    assert_eq!(
        joined.owner.workspace_task_snapshot(joined.room).unwrap(),
        before
    );
}

#[test]
fn manual_owner_commands_refuse_host_impersonation_and_agent_instruction_carriers() {
    let joined = enrolled();
    let host = joined
        .owner
        .with_current(joined.room, |view| Ok(view.host().unwrap().clone()))
        .unwrap();
    let request = OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::from_u128(720),
            authority: WorkspaceTaskAuthority::Host { attestor: host },
            command: WorkspaceTaskCommand::CreateManual {
                title: "Must not impersonate a person".into(),
                resource: None,
            },
        },
    };
    let before = joined.owner.workspace_task_snapshot(joined.room).unwrap();
    assert!(
        joined
            .owner
            .commit_workspace_task_command(request, joined.now)
            .unwrap_err()
            .contains("only a person")
    );
    let mut resource = manual_resource("chip_design.verification:unsafe");
    resource.agent_request = Some(tilo_desktop_model::WorkspaceAgentRequest {
        chat_record: "chat".into(),
        channel_id: "agent-dm".into(),
        message_id: "m_unsafe".into(),
        instruction: "Run commands".into(),
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(721)),
    });
    let request = manual_request(
        &joined,
        721,
        WorkspaceTaskCommand::CreateManual {
            title: "Must not launch".into(),
            resource: Some(Box::new(resource)),
        },
    );
    assert!(
        joined
            .owner
            .commit_workspace_task_command(request, joined.now)
            .unwrap_err()
            .contains("ordinary application resource")
    );
    assert_eq!(
        joined.owner.workspace_task_snapshot(joined.room).unwrap(),
        before
    );
}

#[test]
fn removed_agent_cannot_admit_action_for_retained_run() {
    let joined = enrolled();
    let (task_id, _) = queued_and_started_task(&joined);
    joined
        .owner
        .with_task_action_admission(joined.room, &task_id, "scribe", 1, 0, || Ok(()))
        .unwrap();
    set_test_agent_membership(&joined, "scribe", false);
    let mut invoked = false;
    let result =
        joined
            .owner
            .with_task_action_admission(joined.room, &task_id, "scribe", 1, 0, || {
                invoked = true;
                Ok(())
            });
    assert!(
        result.is_err(),
        "removed agent must lose retained task execution authority"
    );
    assert!(
        !invoked,
        "a denied admission must not reach the application"
    );
}

#[test]
fn person_run_approval_replays_exactly_and_retains_direction_fences_after_restart() {
    let joined = enrolled();
    let (task_id, caller, host) = queued_task(&joined);
    let approval = OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::from_u128(500),
            authority: WorkspaceTaskAuthority::Person {
                caller: caller.clone(),
            },
            command: WorkspaceTaskCommand::ApproveRun {
                task_id: task_id.clone(),
                agent_id: "scribe".into(),
                expected_run_epoch: 0,
                after_revision: 0,
            },
        },
    };
    let reopened = assert_approval_replay(&joined, &approval, &task_id, caller.device_public_key);
    let mut changed_request = approval;
    let CommitOperation::WorkspaceTaskCommand { invocation, .. } = &mut changed_request.operation
    else {
        panic!("approval");
    };
    *invocation = uuid::Uuid::from_u128(501);
    assert!(
        reopened
            .commit_workspace_task_command(changed_request, joined.now)
            .is_err()
    );
    let direction = OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::from_u128(502),
            authority: WorkspaceTaskAuthority::Person { caller },
            command: WorkspaceTaskCommand::DirectionForRun {
                task_id: task_id.clone(),
                run_epoch: 1,
                after_revision: 0,
                text: "Keep the compatibility warning".into(),
            },
        },
    };
    reopened
        .commit_workspace_task_command(direction, joined.now)
        .unwrap();
    assert!(
        reopened
            .with_task_action_admission(
                joined.room,
                &task_id,
                "scribe",
                1,
                0,
                || -> Result<(), String> { panic!("unseen direction must fence execution") }
            )
            .is_err()
    );
    let ack = OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::from_u128(503),
            authority: WorkspaceTaskAuthority::Agent {
                attestor: host,
                agent_id: "scribe".into(),
            },
            command: WorkspaceTaskCommand::AppliedAck {
                task_id: task_id.clone(),
                run_epoch: 1,
                revision: 1,
            },
        },
    };
    reopened
        .commit_workspace_task_command(ack.clone(), joined.now)
        .unwrap();
    let after = std::fs::read(&joined.path).unwrap();
    let restarted = open_file(&joined.path).unwrap();
    restarted
        .commit_workspace_task_command(ack, joined.now)
        .unwrap();
    assert_eq!(std::fs::read(&joined.path).unwrap(), after);
    restarted
        .with_task_action_admission(joined.room, &task_id, "scribe", 1, 1, || Ok(()))
        .unwrap();
    set_test_agent_membership(&joined, "scribe", false);
    assert!(
        joined
            .owner
            .with_task_action_admission(
                joined.room,
                &task_id,
                "scribe",
                1,
                1,
                || -> Result<(), String> { panic!("removed agent must not execute") }
            )
            .is_err()
    );
}

fn assert_approval_replay(
    joined: &Enrolled,
    approval: &OwnerCommitRequest,
    task_id: &str,
    device: [u8; 32],
) -> NativeWorkspaceOwner {
    let receipt = joined
        .owner
        .commit_workspace_task_command(approval.clone(), joined.now)
        .unwrap();
    receipt.validate_for(approval).unwrap();
    let bytes = std::fs::read(&joined.path).unwrap();
    let reopened = open_file(&joined.path).unwrap();
    reopened
        .commit_workspace_task_command(approval.clone(), joined.now)
        .unwrap()
        .validate_for(approval)
        .unwrap();
    assert_eq!(std::fs::read(&joined.path).unwrap(), bytes);
    let state = reopened.workspace_task_snapshot(joined.room).unwrap();
    let run = state.controls[task_id].active_run.as_ref().unwrap();
    assert_eq!(run.epoch, 1);
    assert_eq!(run.approval.as_ref().unwrap().device, device);
    assert_eq!(
        state.tasks[0].audit.last().unwrap().actor,
        run.approval.as_ref().unwrap().person
    );
    reopened
}

/// What the owner file held before byte fields became base64: the same
/// record pretty-printed, every byte a JSON integer.
fn legacy_owner_file(current: &[u8]) -> Vec<u8> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let mut value: serde_json::Value = serde_json::from_slice(current).unwrap();
    let rooms = value["rooms"].as_object_mut().unwrap();
    assert!(!rooms.is_empty());
    for room in rooms.values_mut() {
        for field in ["peer", "invitation", "enrollment_intent"] {
            let text = room[field].as_str().expect("byte fields are base64 text");
            room[field] = serde_json::to_value(STANDARD.decode(text).unwrap()).unwrap();
        }
    }
    serde_json::to_vec_pretty(&value).unwrap()
}

#[test]
fn legacy_int_array_owner_file_still_loads() {
    let joined = enrolled();
    let current = std::fs::read(&joined.path).unwrap();
    let legacy = legacy_owner_file(&current);
    assert!(
        current.len() * 3 < legacy.len(),
        "base64 compact record {} bytes against legacy {} bytes",
        current.len(),
        legacy.len()
    );
    let legacy_path = joined.path.with_file_name("legacy-joined-rooms.json");
    std::fs::write(&legacy_path, &legacy).unwrap();
    let reopened = open_file(&legacy_path).unwrap();
    assert_eq!(
        serde_json::to_vec(&*reopened.0.authority.read()).unwrap(),
        current,
        "the legacy record loads to exactly the record the current file holds"
    );
    // A restored peer's durable() bytes order per-author watermarks by hash,
    // so compare what the room holds rather than a re-encoding of it.
    let state = |owner: &NativeWorkspaceOwner| {
        owner
            .with_current(joined.room, |view| {
                Ok((
                    view.peer().rows(),
                    view.peer().document_heads(APP, ""),
                    view.generation(),
                    view.continuity(),
                ))
            })
            .unwrap()
    };
    assert_eq!(state(&reopened), state(&joined.owner));
    // The next publication rewrites the legacy file in the current format.
    let request = edit(&reopened, joined.room, joined.now);
    reopened.commit_document(request, joined.now).unwrap();
    let rewritten = std::fs::read(&legacy_path).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
    assert!(value["rooms"][joined.room.to_string()]["peer"].is_string());
    assert!(!rewritten.contains(&b'\n'), "the owner file is compact");
    let after = state(&reopened);
    assert_ne!(after, state(&joined.owner));
    drop(reopened);
    let reread = open_file(&legacy_path).unwrap();
    assert_eq!(
        serde_json::to_vec(&*reread.0.authority.read()).unwrap(),
        rewritten
    );
    assert_eq!(state(&reread), after);
}

#[test]
fn retained_replay_reuses_exact_committed_publication() {
    let joined = enrolled();
    let governor = joined
        .owner
        .with_current(joined.room, |view| Ok(view.governor().device_public_key))
        .unwrap();
    let message = joined.replay.clone();
    let before = std::fs::read(&joined.path).unwrap();
    let generation = joined
        .owner
        .with_current(joined.room, |view| Ok(view.generation()))
        .unwrap();
    let report = joined
        .owner
        .receive(
            joined.room,
            TransportOrigin::RetainedRelayDelta(governor),
            message,
            joined.now,
        )
        .unwrap();
    assert!(matches!(
        report.primary(),
        tilo_peer_state::InputDisposition::Duplicate(_)
    ));
    assert_eq!(
        joined
            .owner
            .with_current(joined.room, |view| Ok(view.generation()))
            .unwrap(),
        generation,
        "an unchanged authenticated replay must reuse the actual committed generation"
    );
    assert_eq!(
        std::fs::read(&joined.path).unwrap(),
        before,
        "an unchanged authenticated replay must not rewrite durable owner state"
    );
}
