use super::*;

#[path = "task_admission/model_host_tests.rs"]
mod model_host_tests;

#[path = "task_admission/local_admission_tests.rs"]
mod local_admission_tests;

#[path = "task_admission/retained_action_tests.rs"]
mod retained_action_tests;

use crate::application_record::tests::{Fixture, fixture};
use tilo_owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand};

fn running() -> (Fixture, NativeInvocationBinding) {
    running_with_model(false)
}

fn running_with_model(model: bool) -> (Fixture, NativeInvocationBinding) {
    let fixture = fixture();
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    grant_running_agent(&fixture, room, model);
    let command = |id, command| OwnerCommitRequest {
        room: room.to_string(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::from_u128(id),
            authority: WorkspaceTaskAuthority::Person {
                caller: fixture.certificate.clone(),
            },
            command,
        },
    };
    fixture
        .owner
        .commit_workspace_task_command(
            command(
                100,
                if model {
                    WorkspaceTaskCommand::QueueInstruction {
                        title: "Review document".into(),
                        instruction: "Inspect the document without disclosing its contents".into(),
                        assigned_agent: "scribe".into(),
                        route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                    }
                } else {
                    WorkspaceTaskCommand::Queue {
                        title: "Review document".into(),
                        assigned_agent: "scribe".into(),
                        route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                        resource: None,
                    }
                },
            ),
            fixture.now,
        )
        .unwrap();
    let task = fixture
        .owner
        .with_current(room, |view| {
            Ok(
                tilo_peer_state::workspace_task::snapshot(view.peer().admitted_room().unwrap())?
                    .tasks[0]
                    .id
                    .clone(),
            )
        })
        .unwrap();
    fixture
        .owner
        .commit_workspace_task_command(
            command(
                101,
                WorkspaceTaskCommand::ApproveRun {
                    task_id: task.clone(),
                    agent_id: "scribe".into(),
                    expected_run_epoch: 0,
                    after_revision: 0,
                },
            ),
            fixture.now,
        )
        .unwrap();
    let binding = NativeInvocationBinding {
        room,
        task,
        agent: "scribe".into(),
        epoch: 1,
        revision: 0,
        invocation: uuid::Uuid::from_u128(102),
        request: uuid::Uuid::from_u128(103),
        host: fixture.certificate.device_public_key,
        app: "so.tilo.word-processor".into(),
        provider: "document-provider".into(),
        resource: "document".into(),
        action: "insert".into(),
        arguments_digest: [1; 32],
        contract_digest: [2; 32],
        ceiling_digest: [4; 32],
        private_run_digest: [3; 32],
    };
    (fixture, binding)
}

fn grant_running_agent(fixture: &Fixture, room: RoomId, model: bool) {
    let messages = fixture
        .owner
        .with_current(room, |view| {
            let mut peer = view.peer().clone();
            let node = tilo_action_policy::TreePath::room(peer.room());
            peer.governance_mut().put_governed_grants(
                &[tilo_action_policy::Grant::new(
                    "admission-test-agent",
                    tilo_action_policy::Subject::agent("scribe"),
                    node,
                    if model {
                        tilo_action_policy::KindSet::ALL
                    } else {
                        tilo_action_policy::KindSet::READ_UNDOABLE
                    },
                    tilo_action_policy::Answer::Allow,
                    tilo_action_policy::Subject::person("governor"),
                )],
                &fixture.key,
                fixture.now,
            )?;
            peer.durable_mutation_messages(
                view.peer(),
                &fixture.key,
                fixture.now,
                view.generation().checked_add(1).unwrap(),
            )
        })
        .unwrap();
    for message in messages {
        fixture
            .owner
            .receive(
                room,
                tilo_peer_state::TransportOrigin::RetainedNativePublication {
                    publisher: fixture.certificate.device_public_key,
                },
                message,
                fixture.now,
            )
            .unwrap();
    }
}

#[test]
fn revoking_agent_between_reservation_and_claim_refuses_dispatch() {
    let (fixture, binding) = running();
    let reservation = reserve(&fixture, binding.clone());
    let messages = fixture
        .owner
        .with_current(binding.room, |view| {
            let mut peer = view.peer().clone();
            peer.governance_mut().remove_governed_grants(
                &[tilo_action_policy::GrantId::new("admission-test-agent")],
                &fixture.key,
                fixture.now,
            )?;
            peer.durable_mutation_messages(
                view.peer(),
                &fixture.key,
                fixture.now,
                view.generation() + 1,
            )
        })
        .unwrap();
    for message in messages {
        fixture
            .owner
            .receive(
                binding.room,
                tilo_peer_state::TransportOrigin::RetainedNativePublication {
                    publisher: fixture.certificate.device_public_key,
                },
                message,
                fixture.now,
            )
            .unwrap();
    }
    assert!(
        fixture
            .owner
            .claim_native_invocation(
                &reservation,
                &fixture.certificate,
                &fixture.key,
                fixture.now,
                |_, _| Ok(())
            )
            .is_err()
    );
}

#[test]
fn claims_reuse_the_persisted_public_baseline_without_rebuilding_history() {
    let (fixture, binding) = running();
    let public_bytes = |owner: &NativeWorkspaceOwner| {
        owner
            .with_current(binding.room, |view| {
                Ok(view.peer().admitted_room().unwrap().clone().save())
            })
            .unwrap()
    };
    let reservation = reserve(&fixture, binding.clone());
    // The first reservation installs a signed replacement; claims must reuse it.
    let before = public_bytes(&fixture.owner);
    fixture
        .owner
        .claim_native_invocation(
            &reservation,
            &fixture.certificate,
            &fixture.key,
            fixture.now,
            |_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(public_bytes(&fixture.owner), before);
    let reopened = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
    assert_eq!(public_bytes(&reopened), before);
}

fn reserve(fixture: &Fixture, binding: NativeInvocationBinding) -> NativeInvocationReservation {
    fixture
        .owner
        .reserve_native_invocation(
            binding,
            &fixture.certificate,
            &fixture.key,
            fixture.now,
            |_, _| Ok(()),
        )
        .unwrap()
}

#[test]
fn exact_reservation_survives_reopen_and_dispatch_intent_is_not_retried() {
    let (fixture, binding) = running();
    let reservation = reserve(&fixture, binding.clone());
    let retry = reserve(&fixture, binding.clone());
    assert_eq!(reservation, retry);
    let permit = fixture
        .owner
        .claim_native_invocation(
            &reservation,
            &fixture.certificate,
            &fixture.key,
            fixture.now,
            |_, _| Ok(()),
        )
        .unwrap();
    let reopened = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
    let mut changed = binding.clone();
    changed.arguments_digest = [4; 32];
    assert!(
        reopened
            .admit_reserved_native_invocation(
                &permit,
                &changed,
                &fixture.certificate,
                || -> Result<(), String> { panic!("changed arguments dispatched") }
            )
            .is_err()
    );
    let failure = reopened.admit_reserved_native_invocation(
        &permit,
        &binding,
        &fixture.certificate,
        || -> Result<(), String> { Err("provider reply lost after dispatch".into()) },
    );
    assert!(failure.is_err());
    let again = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
    assert!(
        again
            .admit_reserved_native_invocation(
                &permit,
                &binding,
                &fixture.certificate,
                || -> Result<(), String> { panic!("ambiguous dispatch replayed") }
            )
            .is_err()
    );
}

#[test]
fn stop_blocks_governor_immediately_but_requires_durable_host_fence() {
    let (fixture, binding) = running();
    let reservation = reserve(&fixture, binding.clone());
    let permit = fixture
        .owner
        .claim_native_invocation(
            &reservation,
            &fixture.certificate,
            &fixture.key,
            fixture.now,
            |_, _| Ok(()),
        )
        .unwrap();
    let fence = fixture
        .owner
        .stop_native_admission(binding.room, &binding.task, binding.epoch, &fixture.key)
        .unwrap();
    assert_eq!(
        fixture
            .owner
            .native_task_admission_state(binding.room, &binding.task, binding.epoch)
            .unwrap(),
        NativeTaskAdmissionState::Stopping
    );
    let mut next = binding.clone();
    next.invocation = uuid::Uuid::from_u128(104);
    next.request = uuid::Uuid::from_u128(105);
    assert!(
        fixture
            .owner
            .reserve_native_invocation(
                next,
                &fixture.certificate,
                &fixture.key,
                fixture.now,
                |_, _| Ok(())
            )
            .is_err()
    );
    assert!(
        fixture
            .owner
            .with_task_action_admission(
                binding.room,
                &binding.task,
                &binding.agent,
                binding.epoch,
                binding.revision,
                || -> Result<(), String> { panic!("legacy admission bypassed stop") }
            )
            .is_err()
    );
    let reopened = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
    assert_eq!(
        reopened
            .native_task_admission_state(binding.room, &binding.task, binding.epoch)
            .unwrap(),
        NativeTaskAdmissionState::Stopping
    );
    let ack = reopened
        .acknowledge_native_host_fence(&fence, &fixture.certificate, &fixture.key)
        .unwrap();
    assert!(ack.dispatched.is_empty());
    reopened
        .receive_native_host_fence(&ack, &fixture.certificate, fixture.now)
        .unwrap();
    assert_eq!(
        reopened
            .native_task_admission_state(binding.room, &binding.task, binding.epoch)
            .unwrap(),
        NativeTaskAdmissionState::Stopped
    );
    assert!(
        reopened
            .admit_reserved_native_invocation(
                &permit,
                &binding,
                &fixture.certificate,
                || -> Result<(), String> { panic!("queued work admitted after host fence") }
            )
            .is_err()
    );
    let final_owner = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
    assert_eq!(
        final_owner
            .acknowledge_native_host_fence(&fence, &fixture.certificate, &fixture.key)
            .unwrap(),
        ack
    );
}

#[test]
fn authenticated_source_mismatch_and_changed_request_have_no_effects() {
    let (fixture, binding) = running();
    let reservation = reserve(&fixture, binding.clone());
    let permit = fixture
        .owner
        .claim_native_invocation(
            &reservation,
            &fixture.certificate,
            &fixture.key,
            fixture.now,
            |_, _| Ok(()),
        )
        .unwrap();
    let mut foreign = fixture.certificate.clone();
    foreign.device_public_key = SigningKey::from_bytes(&[99; 32]).verifying_key().to_bytes();
    assert!(
        fixture
            .owner
            .admit_reserved_native_invocation(
                &permit,
                &binding,
                &foreign,
                || -> Result<(), String> { panic!("foreign source dispatched") }
            )
            .is_err()
    );
    let mut changed = binding.clone();
    changed.request = uuid::Uuid::from_u128(109);
    assert!(
        fixture
            .owner
            .reserve_native_invocation(
                changed,
                &fixture.certificate,
                &fixture.key,
                fixture.now,
                |_, _| Ok(())
            )
            .is_err()
    );
    assert_eq!(
        fixture
            .owner
            .admit_reserved_native_invocation(&permit, &binding, &fixture.certificate, || Ok(1))
            .unwrap(),
        1
    );
}

#[test]
fn effect_policy_is_rechecked_inside_durable_host_admission() {
    let (fixture, binding) = running();
    let reservation = reserve(&fixture, binding.clone());
    let permit = fixture
        .owner
        .claim_native_invocation(
            &reservation,
            &fixture.certificate,
            &fixture.key,
            fixture.now,
            |_, _| Ok(()),
        )
        .unwrap();
    let effects = std::sync::atomic::AtomicUsize::new(0);
    assert!(
        fixture
            .owner
            .admit_reserved_native_invocation_checked(
                &permit,
                &binding,
                &fixture.certificate,
                |_, _| Err("current registered action contract was revoked".into()),
                || {
                    effects.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                },
            )
            .is_err()
    );
    assert_eq!(effects.load(std::sync::atomic::Ordering::SeqCst), 0);
    let reopened = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
    reopened
        .admit_reserved_native_invocation_checked(
            &permit,
            &binding,
            &fixture.certificate,
            |_, _| Ok(()),
            || {
                effects.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(effects.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn private_coordination_debug_never_formats_the_goal() {
    let marker = "private-native-goal-marker";
    let command = NativeAdmissionCommand::PrivateCoordination {
        invocation: uuid::Uuid::new_v4(),
        command: WorkspaceTaskCommand::QueueInstruction {
            title: "Read document".into(),
            instruction: marker.into(),
            assigned_agent: "scribe".into(),
            route: tilo_desktop_model::WorkspaceTaskRoute::Model,
        },
    };
    assert!(!format!("{command:?}").contains(marker));
    let decoded = decode_admission_command(&encode_admission_command(&command).unwrap()).unwrap();
    assert!(!format!("{decoded:?}").contains(marker));
}

#[test]
fn stopped_barrier_retains_dispatched_effects_without_promising_rollback() {
    let (fixture, binding) = running();
    let reservation = reserve(&fixture, binding.clone());
    let permit = fixture
        .owner
        .claim_native_invocation(
            &reservation,
            &fixture.certificate,
            &fixture.key,
            fixture.now,
            |_, _| Ok(()),
        )
        .unwrap();
    fixture
        .owner
        .admit_reserved_native_invocation(&permit, &binding, &fixture.certificate, || Ok(()))
        .unwrap();
    let fence = fixture
        .owner
        .stop_native_admission(binding.room, &binding.task, binding.epoch, &fixture.key)
        .unwrap();
    let ack = fixture
        .owner
        .acknowledge_native_host_fence(&fence, &fixture.certificate, &fixture.key)
        .unwrap();
    assert_eq!(ack.dispatched, BTreeSet::from([binding.invocation]));
    fixture
        .owner
        .receive_native_host_fence(&ack, &fixture.certificate, fixture.now)
        .unwrap();
    assert_eq!(
        fixture
            .owner
            .native_task_admission_state(binding.room, &binding.task, binding.epoch)
            .unwrap(),
        NativeTaskAdmissionState::Stopped
    );
}
