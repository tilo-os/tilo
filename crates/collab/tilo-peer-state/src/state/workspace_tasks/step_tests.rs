//! Actual `StatePeer` admission and room reducer; no child execution claim here.
use super::*;
use tilo_desktop_model::{
    WorkspaceTaskMutationResult, WorkspaceTaskStepOutcome, WorkspaceTaskStepState,
};
use tilo_user_identity::DeviceCert;

struct Fixture {
    peer: StatePeer,
    certificate: DeviceCert,
    now: SystemTime,
    task: String,
    child: tilo_actions::InvocationId,
}
impl Fixture {
    fn new() -> Self {
        let (mut peer, certificate, _, now) = super::tests::peer();
        let (_, result) = peer
            .apply_workspace_task_command(
                uuid::Uuid::new_v4(),
                [1; 32],
                &WorkspaceTaskAuthority::Person {
                    caller: certificate.clone(),
                },
                &WorkspaceTaskCommand::Queue {
                    title: "Step commitments".into(),
                    assigned_agent: "scribe".into(),
                    route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                    resource: None,
                },
                now,
            )
            .unwrap();
        let WorkspaceTaskMutationResult::Queued { task_id } = result else {
            panic!("queued native task");
        };
        peer.apply_workspace_task_command(
            uuid::Uuid::new_v4(),
            [2; 32],
            &WorkspaceTaskAuthority::Host {
                attestor: certificate.clone(),
            },
            &WorkspaceTaskCommand::StartRun {
                task_id: task_id.clone(),
                agent_id: "scribe".into(),
            },
            now,
        )
        .unwrap();
        Self {
            peer,
            certificate,
            now,
            task: task_id,
            child: tilo_actions::InvocationId::new(),
        }
    }
    fn prepared(&self) -> WorkspaceTaskCommand {
        WorkspaceTaskCommand::PrepareStep {
            task_id: self.task.clone(),
            run_epoch: 1,
            index: 0,
            invocation: self.child,
            intent_digest: [3; 32],
        }
    }
    fn committed(&self) -> WorkspaceTaskCommand {
        WorkspaceTaskCommand::CommitStep {
            task_id: self.task.clone(),
            run_epoch: 1,
            index: 0,
            invocation: self.child,
            intent_digest: [3; 32],
            outcome: WorkspaceTaskStepOutcome::Succeeded,
            output_digest: [4; 32],
        }
    }
    fn host(
        &mut self,
        command: &WorkspaceTaskCommand,
    ) -> Result<WorkspaceTaskMutationResult, String> {
        let digest = *blake3::hash(&postcard::to_stdvec(command).unwrap()).as_bytes();
        self.peer
            .apply_workspace_task_command(
                uuid::Uuid::new_v4(),
                digest,
                &WorkspaceTaskAuthority::Host {
                    attestor: self.certificate.clone(),
                },
                command,
                self.now,
            )
            .map(|(_, result)| result)
    }
    fn snapshot(&self) -> crate::workspace_task::WorkspaceTaskSnapshot {
        crate::workspace_task::snapshot(self.peer.admitted_room().unwrap()).unwrap()
    }
}

#[test]
fn native_step_host_guard_precedes_receipt_replay_for_person_and_agent() {
    let mut fixture = Fixture::new();
    let prepare = fixture.prepared();
    let invocation = uuid::Uuid::new_v4();
    let digest = *blake3::hash(&postcard::to_stdvec(&prepare).unwrap()).as_bytes();
    fixture
        .peer
        .apply_workspace_task_command(
            invocation,
            digest,
            &WorkspaceTaskAuthority::Host {
                attestor: fixture.certificate.clone(),
            },
            &prepare,
            fixture.now,
        )
        .unwrap();
    let before = fixture.peer.durable().unwrap();
    for authority in [
        WorkspaceTaskAuthority::Person {
            caller: fixture.certificate.clone(),
        },
        WorkspaceTaskAuthority::Agent {
            attestor: fixture.certificate.clone(),
            agent_id: "scribe".into(),
        },
    ] {
        assert!(
            fixture
                .peer
                .apply_workspace_task_command(invocation, digest, &authority, &prepare, fixture.now)
                .is_err()
        );
        assert!(
            fixture
                .peer
                .apply_workspace_task_command(
                    uuid::Uuid::new_v4(),
                    [8; 32],
                    &authority,
                    &fixture.committed(),
                    fixture.now
                )
                .is_err()
        );
        assert_eq!(fixture.peer.durable().unwrap(), before);
    }
}

#[test]
fn native_step_cancel_refuses_new_admission_but_exact_prepared_child_can_settle() {
    let mut fixture = Fixture::new();
    fixture.host(&fixture.prepared()).unwrap();
    fixture
        .peer
        .apply_workspace_task_command(
            uuid::Uuid::new_v4(),
            [9; 32],
            &WorkspaceTaskAuthority::Person {
                caller: fixture.certificate.clone(),
            },
            &WorkspaceTaskCommand::CancelForRun {
                task_id: fixture.task.clone(),
                run_epoch: 1,
                after_revision: 0,
            },
            fixture.now,
        )
        .unwrap();
    let next = WorkspaceTaskCommand::PrepareStep {
        task_id: fixture.task.clone(),
        run_epoch: 1,
        index: 1,
        invocation: tilo_actions::InvocationId::new(),
        intent_digest: [5; 32],
    };
    let before = fixture.peer.durable().unwrap();
    assert!(fixture.host(&next).is_err());
    assert_eq!(fixture.peer.durable().unwrap(), before);
    fixture.host(&fixture.committed()).unwrap();
    let snapshot = fixture.snapshot();
    assert_eq!(
        snapshot.tasks[0].status,
        tilo_desktop_model::WorkspaceTaskStatus::Cancelled
    );
    let control = &snapshot.controls[&fixture.task];
    assert!(control.active_run.is_none());
    assert_eq!(control.steps.len(), 1);
    assert_eq!(
        control.steps[0].state,
        WorkspaceTaskStepState::Committed {
            outcome: WorkspaceTaskStepOutcome::Succeeded,
            output_digest: [4; 32]
        }
    );
    assert!(fixture.host(&next).is_err());
}

#[test]
fn native_step_changed_intent_epoch_index_and_terminal_refuse_without_mutation() {
    let mut fixture = Fixture::new();
    fixture.host(&fixture.prepared()).unwrap();
    let before = fixture.peer.durable().unwrap();
    for command in [
        WorkspaceTaskCommand::PrepareStep {
            task_id: fixture.task.clone(),
            run_epoch: 1,
            index: 0,
            invocation: fixture.child,
            intent_digest: [7; 32],
        },
        WorkspaceTaskCommand::PrepareStep {
            task_id: fixture.task.clone(),
            run_epoch: 2,
            index: 0,
            invocation: fixture.child,
            intent_digest: [3; 32],
        },
        WorkspaceTaskCommand::PrepareStep {
            task_id: fixture.task.clone(),
            run_epoch: 1,
            index: 2,
            invocation: tilo_actions::InvocationId::new(),
            intent_digest: [3; 32],
        },
        WorkspaceTaskCommand::CommitStep {
            task_id: fixture.task.clone(),
            run_epoch: 1,
            index: 0,
            invocation: tilo_actions::InvocationId::new(),
            intent_digest: [3; 32],
            outcome: WorkspaceTaskStepOutcome::Succeeded,
            output_digest: [4; 32],
        },
    ] {
        assert!(fixture.host(&command).is_err());
        assert_eq!(fixture.peer.durable().unwrap(), before);
    }
    fixture.host(&fixture.committed()).unwrap();
    let committed = fixture.peer.durable().unwrap();
    let altered = WorkspaceTaskCommand::CommitStep {
        task_id: fixture.task.clone(),
        run_epoch: 1,
        index: 0,
        invocation: fixture.child,
        intent_digest: [3; 32],
        outcome: WorkspaceTaskStepOutcome::Failed,
        output_digest: [6; 32],
    };
    assert!(fixture.host(&altered).is_err());
    assert_eq!(fixture.peer.durable().unwrap(), committed);
}

#[test]
fn native_step_wire_appends_after_landed_cancel_and_budget_and_preserves_old_results() {
    let old = [6, 1, b't', 7];
    assert_eq!(
        postcard::from_bytes::<WorkspaceTaskMutationResult>(&old).unwrap(),
        WorkspaceTaskMutationResult::ExecutionReported {
            task_id: "t".into(),
            sequence: 7
        }
    );
    assert_eq!(
        postcard::to_stdvec(&WorkspaceTaskMutationResult::ExecutionReported {
            task_id: "t".into(),
            sequence: 7
        })
        .unwrap(),
        old
    );
    let mut fixture = Fixture::new();
    assert_eq!(postcard::to_stdvec(&fixture.prepared()).unwrap()[0], 13);
    assert_eq!(postcard::to_stdvec(&fixture.committed()).unwrap()[0], 14);
    let prepared = fixture.host(&fixture.prepared()).unwrap();
    assert_eq!(postcard::to_stdvec(&prepared).unwrap()[0], 9);
    let committed = fixture.host(&fixture.committed()).unwrap();
    let encoded = postcard::to_stdvec(&committed).unwrap();
    assert_eq!(encoded[0], 10);
    assert_eq!(
        postcard::from_bytes::<WorkspaceTaskMutationResult>(&encoded).unwrap(),
        committed
    );
    let control: tilo_desktop_model::WorkspaceTaskControl =
        serde_json::from_value(serde_json::json!({
        "run_epoch":1, "directions":[], "receipts":[] }))
        .unwrap();
    assert!(control.steps.is_empty());
    assert_eq!(
        serde_json::to_value(control).unwrap(),
        serde_json::json!({
        "run_epoch":1, "directions":[], "receipts":[] })
    );
}

#[test]
fn native_step_current_agent_revocation_refuses_even_cached_host_prepare() {
    let mut fixture = Fixture::new();
    let command = fixture.prepared();
    let invocation = uuid::Uuid::new_v4();
    let digest = *blake3::hash(&postcard::to_stdvec(&command).unwrap()).as_bytes();
    let authority = WorkspaceTaskAuthority::Host {
        attestor: fixture.certificate.clone(),
    };
    fixture
        .peer
        .apply_workspace_task_command(invocation, digest, &authority, &command, fixture.now)
        .unwrap();
    fixture
        .peer
        .apply_workspace_membership_command(
            &fixture.certificate,
            &crate::workspace_membership::WorkspaceMembershipCommand::RemoveAgent {
                agent: "scribe".into(),
            },
            &ed25519_dalek::SigningKey::from_bytes(&[2; 32]),
            fixture.now,
        )
        .unwrap();
    let before = fixture.peer.durable().unwrap();
    assert!(
        fixture
            .peer
            .apply_workspace_task_command(invocation, digest, &authority, &command, fixture.now,)
            .is_err()
    );
    assert_eq!(fixture.peer.durable().unwrap(), before);
}

#[test]
fn native_step_snapshot_rejects_malformed_or_duplicate_retained_commitments() {
    use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, transaction::Transactable};
    let mut fixture = Fixture::new();
    fixture.host(&fixture.prepared()).unwrap();
    let pristine = fixture.peer.admitted_room().unwrap().clone();
    let control = fixture.snapshot().controls[&fixture.task].clone();
    let step = control.steps[0].clone();
    let mut malformed = Vec::new();
    let mut duplicate = control.clone();
    duplicate.steps.push(step.clone());
    malformed.push(duplicate);
    for changed in [
        tilo_desktop_model::WorkspaceTaskStep {
            index: 256,
            ..step.clone()
        },
        tilo_desktop_model::WorkspaceTaskStep {
            run_epoch: 2,
            ..step.clone()
        },
        tilo_desktop_model::WorkspaceTaskStep {
            intent_digest: [0; 32],
            ..step.clone()
        },
        tilo_desktop_model::WorkspaceTaskStep {
            state: WorkspaceTaskStepState::Committed {
                outcome: WorkspaceTaskStepOutcome::Succeeded,
                output_digest: [0; 32],
            },
            ..step
        },
    ] {
        let mut invalid = control.clone();
        invalid.steps[0] = changed;
        malformed.push(invalid);
    }
    let mut gap = control.clone();
    gap.steps[0].index = 1;
    malformed.push(gap);
    let mut unsettled_predecessor = control.clone();
    let mut successor = step.clone();
    successor.index = 1;
    successor.invocation = tilo_actions::InvocationId::new();
    unsettled_predecessor.steps.push(successor);
    malformed.push(unsettled_predecessor);
    let mut oversized = control;
    oversized.steps = vec![step; 1025];
    malformed.push(oversized);
    for invalid in malformed {
        let mut doc = pristine.clone();
        let Some((automerge::Value::Object(ObjType::Map), map)) = doc
            .get(ROOT, crate::workspace_task::TASK_CONTROL_KEY)
            .unwrap()
        else {
            panic!("actual task control map");
        };
        doc.put(
            &map,
            &fixture.task,
            ScalarValue::Bytes(serde_json::to_vec(&invalid).unwrap()),
        )
        .unwrap();
        assert!(crate::workspace_task::snapshot(&doc).is_err());
        assert!(
            crate::workspace_task::result_of(
                &doc,
                crate::workspace_task::WorkspaceTaskCommandId {
                    device: fixture.certificate.device_public_key,
                    invocation: uuid::Uuid::new_v4(),
                },
            )
            .is_err()
        );
    }
    assert!(crate::workspace_task::snapshot(&pristine).is_ok());
}

#[test]
fn native_step_agent_cannot_finish_done_before_the_host_settles_the_admitted_child() {
    let mut fixture = Fixture::new();
    fixture.host(&fixture.prepared()).unwrap();
    let finish = WorkspaceTaskCommand::Finish {
        task_id: fixture.task.clone(),
        run_epoch: 1,
        outcome: crate::workspace_task::WorkspaceTaskFinish::Done,
        summary: "Agent finished conversation".into(),
    };
    let authority = WorkspaceTaskAuthority::Agent {
        attestor: fixture.certificate.clone(),
        agent_id: "scribe".into(),
    };
    let before = fixture.peer.durable().unwrap();
    assert!(
        fixture
            .peer
            .apply_workspace_task_command(
                uuid::Uuid::new_v4(),
                [8; 32],
                &authority,
                &finish,
                fixture.now
            )
            .is_err()
    );
    assert_eq!(fixture.peer.durable().unwrap(), before);
    fixture.host(&fixture.committed()).unwrap();
    fixture
        .peer
        .apply_workspace_task_command(
            uuid::Uuid::new_v4(),
            [9; 32],
            &authority,
            &finish,
            fixture.now,
        )
        .unwrap();
    assert_eq!(
        fixture.snapshot().tasks[0].status,
        tilo_desktop_model::WorkspaceTaskStatus::Done
    );
}
