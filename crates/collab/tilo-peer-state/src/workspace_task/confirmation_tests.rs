use automerge::AutoCommit;
use serde_json::json;
use tilo_actions::{Draft, DraftEdit};
use tilo_desktop_model::{
    WorkspaceTaskConfirmation, WorkspaceTaskConfirmationAnswer, WorkspaceTaskConfirmationState,
    WorkspaceTaskMutationResult, WorkspaceTaskRoute,
};

use crate::workspace_task::{
    AdmittedTaskActor, AppliedTaskCommand, WorkspaceTaskCommand, apply, snapshot,
};

fn person() -> AdmittedTaskActor {
    AdmittedTaskActor::Person {
        id: "person-1".into(),
        device: [1; 32],
    }
}

fn host() -> AdmittedTaskActor {
    AdmittedTaskActor::Host { device: [2; 32] }
}

fn agent() -> AdmittedTaskActor {
    AdmittedTaskActor::Agent {
        id: "scribe".into(),
        device: [2; 32],
    }
}

fn run(
    doc: &mut AutoCommit,
    invocation: u128,
    actor: &AdmittedTaskActor,
    command: &WorkspaceTaskCommand,
) -> Result<AppliedTaskCommand, String> {
    let digest = u8::try_from(invocation % 251).unwrap();
    apply(
        doc,
        uuid::Uuid::from_u128(invocation),
        [digest; 32],
        actor,
        command,
        i64::try_from(invocation).unwrap(),
    )
}

/// A task whose run, epoch 1, is in progress for agent `scribe`.
fn running() -> (AutoCommit, String) {
    let mut doc = AutoCommit::new();
    let queued = run(
        &mut doc,
        1,
        &person(),
        &WorkspaceTaskCommand::Queue {
            title: "Send note X by mail".into(),
            assigned_agent: "scribe".into(),
            route: WorkspaceTaskRoute::Model,
            resource: None,
        },
    )
    .unwrap();
    let WorkspaceTaskMutationResult::Queued { task_id } = queued.result else {
        panic!("queue result")
    };
    run(
        &mut doc,
        2,
        &host(),
        &WorkspaceTaskCommand::StartRun {
            task_id: task_id.clone(),
            agent_id: "scribe".into(),
        },
    )
    .unwrap();
    (doc, task_id)
}

fn send_draft(to: &str) -> (Draft, [u8; 32]) {
    let descriptor = tilo_actions::ActionDescriptor {
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new("inbox", "send_message"),
        resource_kind: "mail.mailbox".into(),
        resource_label: "Inbox".into(),
        action_label: "Send message".into(),
        description: None,
        locus: tilo_actions::ExecutionLocus::ClientLocal,
        kind: tilo_actions::OperationKind::Document,
        contract: tilo_actions::ActionContract::default(),
        input: tilo_actions::InputSpec::None,
        params: vec![tilo_actions::ActionParam::required(
            "message",
            tilo_actions::ParamType::Object,
            "m",
        )],
        view: None,
    };
    let arguments = json!({"message": {"to": to, "subject": "Note X", "body": "Hello"}});
    let arguments = arguments.as_object().unwrap();
    (
        Draft::render(&descriptor, arguments),
        tilo_actions::draft::arguments_digest(arguments),
    )
}

fn confirmation(to: &str) -> WorkspaceTaskConfirmation {
    let (draft, digest) = send_draft(to);
    WorkspaceTaskConfirmation {
        id: WorkspaceTaskConfirmation::id_for("inbox", "send_message", &digest),
        asking: uuid::Uuid::new_v4(),
        run_epoch: 1,
        agent_id: "scribe".into(),
        app: "so.tilo.mail".into(),
        resource_kind: "mail.mailbox".into(),
        resource: "inbox".into(),
        action: "send_message".into(),
        kind: tilo_action_policy::ActionKind::Outside,
        risk: tilo_actions::RiskLevel::Destructive,
        draft,
        arguments_digest: digest,
        requested_at: 3,
        state: WorkspaceTaskConfirmationState::Waiting,
    }
}

fn request(task: &str, confirmation: &WorkspaceTaskConfirmation) -> WorkspaceTaskCommand {
    WorkspaceTaskCommand::RequestConfirmation {
        task_id: task.into(),
        run_epoch: 1,
        confirmation: Box::new(confirmation.clone()),
    }
}

fn answer(task: &str, id: &str, answer: WorkspaceTaskConfirmationAnswer) -> WorkspaceTaskCommand {
    WorkspaceTaskCommand::AnswerConfirmation {
        task_id: task.into(),
        run_epoch: 1,
        confirmation_id: id.into(),
        answer,
    }
}

fn held(doc: &AutoCommit, task: &str) -> Option<WorkspaceTaskConfirmation> {
    snapshot(doc).unwrap().controls[task].confirmation.clone()
}

#[test]
fn a_paused_run_survives_reopening_and_runs_the_confirmed_edit() {
    let (mut doc, task) = running();
    let asked = confirmation("bob@example.org");
    run(&mut doc, 3, &agent(), &request(&task, &asked)).unwrap();

    // The room document is all that is kept: reopened, the pause is there.
    let mut doc = AutoCommit::load(&doc.save()).unwrap();
    let paused = held(&doc, &task).expect("the run is paused");
    assert!(paused.is_waiting());
    let state = snapshot(&doc).unwrap();
    assert_eq!(
        state.tasks[0].status,
        tilo_desktop_model::WorkspaceTaskStatus::InProgress
    );

    let edit = DraftEdit {
        path: vec!["message".into(), "to".into()],
        text: "carol@example.org".into(),
    };
    run(
        &mut doc,
        4,
        &person(),
        &answer(
            &task,
            &asked.id,
            WorkspaceTaskConfirmationAnswer::Confirm {
                edits: vec![edit.clone()],
            },
        ),
    )
    .unwrap();
    let confirmed = held(&doc, &task).unwrap();
    assert_eq!(
        confirmed.state,
        WorkspaceTaskConfirmationState::Confirmed {
            person: "person-1".into(),
            at: 4,
            edits: vec![edit],
        }
    );
    let reviewed = confirmed.reviewed_draft();
    assert_eq!(
        reviewed
            .role(tilo_actions::MessageRole::To)
            .map(|field| field.value.line()),
        Some("carol@example.org".into())
    );
    // Answered once: a second answer is refused.
    assert!(
        run(
            &mut doc,
            5,
            &person(),
            &answer(
                &task,
                &asked.id,
                WorkspaceTaskConfirmationAnswer::Decline { reason: None }
            )
        )
        .is_err()
    );
    // Asking again for the same draft keeps the answer.
    run(&mut doc, 6, &agent(), &request(&task, &asked)).unwrap();
    assert!(!held(&doc, &task).unwrap().is_waiting());

    run(
        &mut doc,
        7,
        &host(),
        &WorkspaceTaskCommand::SettleConfirmation {
            task_id: task.clone(),
            run_epoch: 1,
            confirmation_id: asked.id,
        },
    )
    .unwrap();
    assert!(held(&doc, &task).is_none());
    let verbs: Vec<String> = snapshot(&doc).unwrap().tasks[0]
        .audit
        .iter()
        .map(|event| event.action().to_owned())
        .collect();
    assert!(verbs.contains(&"ask-confirmation".to_owned()));
    assert!(verbs.contains(&"confirm-action".to_owned()));
}

#[test]
fn declining_keeps_the_reason_for_the_agent() {
    let (mut doc, task) = running();
    let asked = confirmation("bob@example.org");
    run(&mut doc, 3, &agent(), &request(&task, &asked)).unwrap();
    run(
        &mut doc,
        4,
        &person(),
        &answer(
            &task,
            &asked.id,
            WorkspaceTaskConfirmationAnswer::Decline {
                reason: Some("  Not to Bob, he is away ".into()),
            },
        ),
    )
    .unwrap();
    let declined = held(&doc, &task).unwrap();
    assert!(matches!(
        declined.state,
        WorkspaceTaskConfirmationState::Declined { ref reason, .. }
            if reason.as_deref() == Some("Not to Bob, he is away")
    ));
    let event = snapshot(&doc).unwrap().tasks[0]
        .audit
        .last()
        .cloned()
        .unwrap();
    assert_eq!(event.action(), "decline-action");
    assert!(
        event.note.is_none(),
        "a private refusal reason must not enter public audit"
    );
    assert!(!format!("{event:?}").contains("Not to Bob"));
}

#[test]
fn a_changed_draft_replaces_the_question_and_an_old_answer_misses_it() {
    let (mut doc, task) = running();
    let first = confirmation("bob@example.org");
    let second = confirmation("dave@example.org");
    assert_ne!(first.id, second.id, "other arguments, another confirmation");
    run(&mut doc, 3, &agent(), &request(&task, &first)).unwrap();
    run(&mut doc, 4, &agent(), &request(&task, &second)).unwrap();
    let refused = run(
        &mut doc,
        5,
        &person(),
        &answer(
            &task,
            &first.id,
            WorkspaceTaskConfirmationAnswer::Confirm { edits: Vec::new() },
        ),
    );
    assert!(refused.is_err(), "the person must review the current draft");
    assert_eq!(held(&doc, &task).unwrap().id, second.id);
}

#[test]
fn only_the_right_parties_ask_answer_and_only_editable_fields_change() {
    let (mut doc, task) = running();
    let asked = confirmation("bob@example.org");
    assert!(run(&mut doc, 3, &person(), &request(&task, &asked)).is_err());
    let mut forged = asked.clone();
    forged.id = "0".repeat(32);
    assert!(run(&mut doc, 3, &agent(), &request(&task, &forged)).is_err());
    run(&mut doc, 3, &agent(), &request(&task, &asked)).unwrap();
    let by_agent = run(
        &mut doc,
        4,
        &agent(),
        &answer(
            &task,
            &asked.id,
            WorkspaceTaskConfirmationAnswer::Confirm { edits: Vec::new() },
        ),
    );
    assert!(by_agent.is_err(), "an agent never confirms for its person");
    let unknown_field = run(
        &mut doc,
        4,
        &person(),
        &answer(
            &task,
            &asked.id,
            WorkspaceTaskConfirmationAnswer::Confirm {
                edits: vec![DraftEdit {
                    path: vec!["message".into(), "bcc".into()],
                    text: "eve@example.org".into(),
                }],
            },
        ),
    );
    assert!(unknown_field.is_err());
    assert!(held(&doc, &task).unwrap().is_waiting());
}

#[test]
fn ending_the_run_ends_its_pause() {
    let (mut doc, task) = running();
    let asked = confirmation("bob@example.org");
    run(&mut doc, 3, &agent(), &request(&task, &asked)).unwrap();
    run(
        &mut doc,
        4,
        &person(),
        &WorkspaceTaskCommand::CancelForRun {
            task_id: task.clone(),
            run_epoch: 1,
            after_revision: 0,
        },
    )
    .unwrap();
    assert!(held(&doc, &task).is_none());
}
