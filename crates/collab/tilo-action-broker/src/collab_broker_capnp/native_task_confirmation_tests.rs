//! A native run pauses at an action asking first, through production RPCs.
#![expect(
    clippy::future_not_send,
    reason = "Capnp RPC clients are intentionally thread confined to the existing LocalSet"
)]
use super::*;
use tilo_desktop_model::{WorkspaceTaskConfirmationAnswer, WorkspaceTaskConfirmationState};

/// Notes' `append`, taking a `text`, published on peer 200.
async fn append_target(f: &Fixture) -> (channel_handle::Client, invocation_stream::Client) {
    let target = f.channel(200, false);
    let mut publish = target.publish_actions_request();
    publish.get().set_app_id("so.tilo.notes");
    {
        let mut row = publish.get().init_actions(1).get(0);
        row.set_resource("notes:release");
        row.set_action("append");
        row.set_resource_kind("document");
        row.set_resource_label("Release notes");
        row.set_action_label("Append");
        row.set_operation_kind("effect");
        let params = vec![tilo_actions::ActionParam::required(
            "text",
            tilo_actions::ParamType::Text,
            "What to append",
        )];
        row.set_params_json(serde_json::to_string(&params).unwrap());
        let contract = tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Undoable,
            execution: tilo_actions::ExecutionMode::Background { cancellable: true },
            result: tilo_actions::ResultSpec::Job,
            ..Default::default()
        };
        row.set_contract_json(serde_json::to_string(&contract).unwrap());
    }
    publish.send().promise.await.unwrap();
    let mut subscribe = target.subscribe_invocations_request();
    subscribe.get().set_stable_invocation_ids(true);
    let stream = subscribe
        .send()
        .promise
        .await
        .unwrap()
        .get()
        .unwrap()
        .get_stream()
        .unwrap();
    (target, stream)
}

/// The person tells the agent Notes asks first.
fn ask_first_for_notes(f: &Fixture) {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    put_grant(
        f,
        agent_answers::grant(
            &room().to_string(),
            "scribe",
            (
                &AnswerScope::app("so.tilo.notes"),
                tilo_action_policy::ActionKind::Undoable,
            ),
            tilo_action_policy::Answer::Ask,
            tilo_action_policy::Subject::person(hex::encode(f.native.certificate.user_public_key)),
        ),
    );
}

async fn append(
    channel: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
    invocation: u128,
    text: &str,
) -> Result<(u64, tilo_actions::InvocationUpdate), capnp::Error> {
    append_as(
        channel,
        run,
        invocation,
        serde_json::json!({ "text": text }).to_string(),
    )
    .await
}

/// [`append`] with `argument` exactly as the agent sends it.
async fn append_as(
    channel: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
    invocation: u128,
    argument: String,
) -> Result<(u64, tilo_actions::InvocationUpdate), capnp::Error> {
    let mut request = channel.invoke_task_action_request();
    let mut p = request.get();
    p.set_run_id(run.run_id);
    p.set_lease(&run.lease);
    p.set_peer_id(200);
    p.set_resource("notes:release");
    p.set_action("append");
    p.set_argument(argument);
    p.set_has_argument(true);
    p.set_invocation(uuid::Uuid::from_u128(invocation).to_string());
    let response = request.send().promise.await?;
    let value = response.get()?;
    Ok((
        value.get_id(),
        serde_json::from_str(value.get_update_json()?.to_str().unwrap()).unwrap(),
    ))
}

pub(super) fn held(f: &Fixture) -> Option<tilo_desktop_model::WorkspaceTaskConfirmation> {
    f.private_confirmation()
}

pub(super) fn answer(f: &Fixture, id: &str, answer: WorkspaceTaskConfirmationAnswer) {
    f.person_command(WorkspaceTaskCommand::AnswerConfirmation {
        task_id: f.task.clone(),
        run_epoch: 1,
        confirmation_id: id.into(),
        answer,
    })
    .unwrap();
}

pub(super) async fn nothing_delivered(stream: &invocation_stream::Client) -> bool {
    tokio::time::timeout(
        Duration::from_millis(30),
        stream.read_request().send().promise,
    )
    .await
    .is_err()
}

#[tokio::test(flavor = "current_thread")]
async fn an_asking_action_pauses_the_run_and_runs_exactly_what_the_person_confirmed() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (_target, stream) = append_target(&f).await;
            ask_first_for_notes(&f);

            let waiting = append(&agent, &run, 9100, "Draft line").await.unwrap_err();
            assert!(
                waiting
                    .to_string()
                    .contains(tilo_actions::draft::AWAITING_CONFIRMATION),
                "{waiting}"
            );
            assert!(nothing_delivered(&stream).await, "nothing ran");
            let asked = held(&f).expect("the run is paused at the draft");
            assert!(asked.is_waiting());
            assert_eq!(asked.draft.action_label, "Append");
            assert_eq!(asked.draft.fields[0].value.line(), "Draft line");
            // Asking again waits on the same question.
            assert!(append(&agent, &run, 9100, "Draft line").await.is_err());
            assert_eq!(held(&f).unwrap().id, asked.id);
            let snapshot = f.native.owner.workspace_task_snapshot(room()).unwrap();
            assert_eq!(snapshot.tasks[0].status, WorkspaceTaskStatus::InProgress);

            // The person changes the text and confirms.
            answer(
                &f,
                &asked.id,
                WorkspaceTaskConfirmationAnswer::Confirm {
                    edits: vec![tilo_actions::DraftEdit {
                        path: vec!["text".into()],
                        text: "Reviewed line".into(),
                    }],
                },
            );
            assert!(matches!(
                held(&f).unwrap().state,
                WorkspaceTaskConfirmationState::Confirmed { .. }
            ));

            // A different draft is not what they confirmed.
            let other = append(&agent, &run, 9101, "Something else")
                .await
                .unwrap_err();
            assert!(
                other
                    .to_string()
                    .contains(tilo_actions::draft::AWAITING_CONFIRMATION)
            );
            assert!(nothing_delivered(&stream).await);
            assert_ne!(held(&f).unwrap().id, asked.id, "it is a new question");

            // Back to the confirmed draft: it is a new question again, the
            // earlier answer having been replaced, so confirm it once more.
            let again = append(&agent, &run, 9100, "Draft line").await.unwrap_err();
            assert!(
                again
                    .to_string()
                    .contains(tilo_actions::draft::AWAITING_CONFIRMATION)
            );
            let asked = held(&f).unwrap();
            answer(
                &f,
                &asked.id,
                WorkspaceTaskConfirmationAnswer::Confirm {
                    edits: vec![tilo_actions::DraftEdit {
                        path: vec!["text".into()],
                        text: "Reviewed line".into(),
                    }],
                },
            );
            let (_, update) = append(&agent, &run, 9100, "Draft line").await.unwrap();
            assert!(matches!(
                update,
                tilo_actions::InvocationUpdate::Accepted { .. }
            ));
            let delivered = stream.read_request().send().promise.await.unwrap();
            let wire = delivered.get().unwrap();
            let argument: serde_json::Value =
                serde_json::from_str(wire.get_argument().unwrap().to_str().unwrap()).unwrap();
            assert_eq!(argument["text"], "Reviewed line", "the person's edit ran");
            assert!(held(&f).is_none(), "the answer reached the run");
            let replay = append(&agent, &run, 9102, "Draft line").await.unwrap_err();
            assert!(
                replay
                    .to_string()
                    .contains(tilo_actions::draft::AWAITING_CONFIRMATION),
                "a second UUID borrowed the consumed person answer: {replay}"
            );
            assert!(
                nothing_delivered(&stream).await,
                "the confirmation ran twice"
            );
            drop(f);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_declined_action_tells_the_agent_why_and_never_runs() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (_target, stream) = append_target(&f).await;
            ask_first_for_notes(&f);
            assert!(append(&agent, &run, 9200, "Draft line").await.is_err());
            let asked = held(&f).unwrap();
            answer(
                &f,
                &asked.id,
                WorkspaceTaskConfirmationAnswer::Decline {
                    reason: Some("Wrong document".into()),
                },
            );
            let declined = append(&agent, &run, 9200, "Draft line")
                .await
                .unwrap_err()
                .to_string();
            assert!(
                declined.contains(tilo_actions::draft::DECLINED),
                "{declined}"
            );
            assert!(declined.contains("Wrong document"), "{declined}");
            assert!(nothing_delivered(&stream).await);
            assert!(held(&f).is_none(), "the agent was told");
            drop(f);
        })
        .await;
}

/// Agents send arguments in the wire form every caller uses, JSON behind
/// `ARGUMENTS_JSON_PREFIX`: it asks first as a bare object does, and what the
/// person confirmed reaches the application in that same form.
#[tokio::test(flavor = "current_thread")]
async fn arguments_in_the_agents_wire_form_ask_first_and_run_as_confirmed() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (_target, stream) = append_target(&f).await;
            ask_first_for_notes(&f);
            let wire = || {
                format!(
                    "{}{}",
                    tilo_actions::ARGUMENTS_JSON_PREFIX,
                    serde_json::json!({ "text": "Draft line" })
                )
            };
            let waiting = append_as(&agent, &run, 9300, wire()).await.unwrap_err();
            assert!(
                waiting
                    .to_string()
                    .contains(tilo_actions::draft::AWAITING_CONFIRMATION),
                "{waiting}"
            );
            let asked = held(&f).expect("the run is paused at the draft");
            assert_eq!(asked.draft.fields[0].value.line(), "Draft line");
            answer(
                &f,
                &asked.id,
                WorkspaceTaskConfirmationAnswer::Confirm {
                    edits: vec![tilo_actions::DraftEdit {
                        path: vec!["text".into()],
                        text: "Reviewed line".into(),
                    }],
                },
            );
            append_as(&agent, &run, 9300, wire()).await.unwrap();
            let delivered = stream.read_request().send().promise.await.unwrap();
            let wire = delivered.get().unwrap();
            let argument = wire.get_argument().unwrap().to_str().unwrap();
            let argument: serde_json::Value = serde_json::from_str(
                argument
                    .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                    .expect("the confirmed arguments keep the agent's form"),
            )
            .unwrap();
            assert_eq!(argument["text"], "Reviewed line");
            drop(f);
        })
        .await;
}

/// An agent that restarts while its run waits at a draft gets the run again
/// with the paused step, so it resumes there rather than planning the task
/// over: the draft it carries rebuilds exactly the arguments the person was
/// asked about, and asking with them runs what the person confirmed.
#[tokio::test(flavor = "current_thread")]
async fn a_run_delivered_again_carries_the_step_it_is_paused_at() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            assert!(
                run.paused.is_none(),
                "a first delivery is paused at nothing"
            );
            let (_target, stream) = append_target(&f).await;
            ask_first_for_notes(&f);
            append(&agent, &run, 9100, "Draft line").await.unwrap_err();
            let asked = held(&f).expect("the run is paused at the draft");

            f.state.remove_connection(100);
            let restarted = f.channel(101, true);
            let resumed = delivered(&restarted).await;
            assert_eq!(resumed.run_id, run.run_id, "the same run, not a new one");
            let paused = resumed
                .paused
                .clone()
                .expect("delivered with its paused step");
            assert_eq!(paused.app, "so.tilo.notes");
            assert_eq!(paused.resource, "notes:release");
            assert_eq!(paused.action, "append");
            assert_eq!(paused.draft, asked.draft);
            let arguments = paused
                .draft
                .arguments()
                .expect("the draft shows every argument");
            assert_eq!(
                tilo_actions::draft::arguments_digest(&arguments),
                paused.arguments_digest,
                "the draft rebuilds exactly what the person was asked about"
            );

            answer(
                &f,
                &asked.id,
                WorkspaceTaskConfirmationAnswer::Confirm { edits: Vec::new() },
            );
            let (_, update) = append_as(
                &restarted,
                &resumed,
                9200,
                serde_json::Value::Object(arguments).to_string(),
            )
            .await
            .unwrap();
            assert!(matches!(
                update,
                tilo_actions::InvocationUpdate::Accepted { .. }
            ));
            let delivered = stream.read_request().send().promise.await.unwrap();
            let wire = delivered.get().unwrap();
            let argument: serde_json::Value =
                serde_json::from_str(wire.get_argument().unwrap().to_str().unwrap()).unwrap();
            assert_eq!(argument["text"], "Draft line");
            assert!(held(&f).is_none(), "the answer reached the run");
            drop(f);
        })
        .await;
}
