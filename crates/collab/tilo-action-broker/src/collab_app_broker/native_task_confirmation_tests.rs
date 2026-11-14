//! A native run pauses at an action asking first, through production RPCs.
#![expect(
    clippy::future_not_send,
    reason = "the broker's Rc-backed channel handles stay on the test LocalSet"
)]
use super::*;
use crate::services::{TrackerAnswer, TrackerQuestion};

/// Notes' `append`, taking a `text`, published on peer 200.
pub(super) async fn append_target(
    f: &Fixture,
) -> (runtime_collab::ChannelHandle, InvocationStreamServer) {
    let handle = Rc::new(f.app_handle(200, "so.tilo.notes", false));
    let target = wire_channel_rc(Rc::clone(&handle));
    let mut append = notes_append();
    append.params = vec![tilo_actions::ActionParam::required(
        "text",
        tilo_actions::ParamType::Text,
        "What to append",
    )];
    target.publish_actions(&[append]).await.unwrap();
    let stream = handle.invocation_feed(true).unwrap();
    (target, stream)
}

/// The person tells the agent Notes asks first.
pub(super) fn ask_first_for_notes(f: &Fixture) {
    put_grant(f, asks_first_for_notes(f));
}

/// The grant [`ask_first_for_notes`] puts.
fn asks_first_for_notes(f: &Fixture) -> tilo_action_policy::Grant {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    agent_answers::grant(
        &workspace_id().to_string(),
        "scribe",
        (
            &AnswerScope::app("so.tilo.notes"),
            tilo_action_policy::ActionKind::Undoable,
        ),
        tilo_action_policy::Answer::Ask,
        tilo_action_policy::Grantee::person(hex::encode(f.native.certificate.user_public_key)),
    )
}

pub(super) async fn append(
    channel: &runtime_collab::ChannelHandle,
    run: &runtime_collab::TaskRunEnvelope,
    invocation: u128,
    text: &str,
) -> Result<(u64, tilo_actions::InvocationUpdate), runtime_portal::NotAvailable> {
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
    channel: &runtime_collab::ChannelHandle,
    run: &runtime_collab::TaskRunEnvelope,
    invocation: u128,
    argument: String,
) -> Result<(u64, tilo_actions::InvocationUpdate), runtime_portal::NotAvailable> {
    let target = runtime_collab::TaskAction {
        argument: Some(&argument),
        ..notes_action()
    };
    let (id, update) = channel
        .invoke_task_action(
            run.run_id,
            &run.lease,
            target,
            &uuid::Uuid::from_u128(invocation).to_string(),
        )
        .await?;
    Ok((id, serde_json::from_str(&update).unwrap()))
}

/// [`append_as`], asking the broker to hold the answer up to `wait_ms`
/// while it stays accepted, as the agent waits for its person.
async fn append_held(
    channel: &runtime_collab::ChannelHandle,
    run: &runtime_collab::TaskRunEnvelope,
    invocation: u128,
    argument: String,
    wait_ms: u32,
) -> Result<(u64, tilo_actions::InvocationUpdate), runtime_portal::NotAvailable> {
    let target = runtime_collab::TaskAction {
        argument: Some(&argument),
        ..notes_action()
    };
    let (id, update) = channel
        .observe_task_child(
            run.run_id,
            &run.lease,
            target,
            &uuid::Uuid::from_u128(invocation).to_string(),
            runtime_collab::TaskChildWait {
                wait_ms,
                seen: tilo_actions::remote::UpdateMark::Accepted,
            },
        )
        .await?;
    Ok((id, serde_json::from_str(&update).unwrap()))
}

/// The question the run asks its person, as its agent asked it on the issue.
pub(super) fn held(f: &Fixture) -> Option<TrackerQuestion> {
    f.native
        .runs
        .lock()
        .unwrap()
        .iter()
        .find(|run| run.issue == f.task)
        .and_then(|run| run.question.clone())
}

/// The agent asks its person, on the issue, about `resource`'s `action`
/// with `arguments`, as the application that publishes it renders it:
/// what an agent does when an action answers that it asks first.
pub(super) fn ask(
    f: &Fixture,
    (resource, action): (&str, &str),
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> TrackerQuestion {
    // Published by the running application, or retained from a closed one.
    let descriptor = f
        .state
        .published_surfaces()
        .into_iter()
        .find_map(|surface| {
            let (_, offered) = surface.action_projection(resource, action)?;
            Some((surface.app_id, offered))
        })
        .expect("the action is published");
    let draft = tilo_actions::Draft::render(
        &descriptor
            .1
            .descriptor(tilo_actions::ExecutionLocus::ClientLocal),
        arguments,
    );
    let digest = hex::encode(tilo_actions::draft::arguments_digest(arguments));
    let question = TrackerQuestion {
        id: format!("question-{digest}"),
        app: descriptor.0,
        resource: tilo_actions::selection::logical_resource_id(resource).into_owned(),
        action: action.into(),
        arguments_digest: digest,
        draft: serde_json::to_value(&draft).unwrap(),
        before: None,
        answer: TrackerAnswer::Waiting,
    };
    f.on_run(|run| run.question = Some(question.clone()));
    question
}

/// The agent asks about appending `text` to the release notes, previewing
/// the tree the append leaves beside the line it adds: its person approves
/// the change, not only the sentence.
pub(super) fn ask_append(f: &Fixture, text: &str) -> TrackerQuestion {
    let arguments = serde_json::json!({ "text": text });
    let mut question = ask(
        f,
        ("notes:release", "append"),
        arguments.as_object().unwrap(),
    );
    let before = crate::services::Preview {
        before: crate::services::PreviewSide {
            text: "Release notes\n".into(),
            spoken: "Release notes".into(),
        },
        after: crate::services::PreviewSide {
            text: format!("Release notes\n{text}\n"),
            spoken: format!("Release notes, then {text}"),
        },
    };
    let recorded = serde_json::to_value(&before).unwrap();
    question.before = Some(recorded.clone());
    f.on_run(|run| {
        if let Some(asked) = run.question.as_mut() {
            asked.before = Some(recorded);
        }
    });
    question
}

/// The person answers the run's question on the issue.
pub(super) fn answer(f: &Fixture, answer: TrackerAnswer) {
    answer_with(f, answer);
}

/// [`answer`], under a name a test that names its sheet's answer `answer`
/// can still call.
pub(super) fn answer_with(f: &Fixture, answer: TrackerAnswer) {
    f.on_run(|run| {
        if let Some(question) = run.question.as_mut() {
            question.answer = answer;
        }
    });
}

/// The person confirms, changing the appended text to `text`.
pub(super) fn confirm_as(f: &Fixture, text: &str) {
    answer(
        f,
        TrackerAnswer::Confirmed {
            edits: serde_json::to_value(vec![tilo_actions::DraftEdit {
                path: vec!["text".into()],
                text: text.into(),
            }])
            .unwrap(),
        },
    );
}

/// Nothing was dispatched to the target app (peer 200) that it has not read.
pub(super) fn nothing_delivered() -> bool {
    crate::collab_app_broker::unread_invocations(200) == 0
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
            assert!(nothing_delivered(), "nothing ran");
            let asked = ask_append(&f, "Draft line");
            assert!(append(&agent, &run, 9100, "Draft line").await.is_err());
            assert_eq!(held(&f).unwrap().id, asked.id);
            assert!(nothing_delivered(), "a waiting question runs nothing");

            confirm_as(&f, "Reviewed line");
            // A different draft is not what they confirmed.
            let other = append(&agent, &run, 9101, "Something else")
                .await
                .unwrap_err();
            assert!(
                other
                    .to_string()
                    .contains(tilo_actions::draft::AWAITING_CONFIRMATION)
            );
            assert!(nothing_delivered());

            let (_, update) = append(&agent, &run, 9100, "Draft line").await.unwrap();
            assert!(matches!(
                update,
                tilo_actions::InvocationUpdate::Accepted { .. }
            ));
            let delivered = stream.next().await.unwrap();
            let argument: serde_json::Value =
                serde_json::from_str(&delivered.argument.unwrap()).unwrap();
            assert_eq!(argument["text"], "Reviewed line", "the person's edit ran");
            drop(f);
        })
        .await;
}

/// The person is shown the change before it runs: the question carries the
/// step's own preview, before and after, so approving it approves a change
/// and not only a sentence, and a person who cannot see the tree reads the
/// same two sides as words.
#[tokio::test(flavor = "current_thread")]
async fn a_waiting_question_shows_the_change_before_anything_runs() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (_target, _stream) = append_target(&f).await;
            ask_first_for_notes(&f);

            let waiting = append(&agent, &run, 9400, "Draft line").await.unwrap_err();
            assert!(
                waiting
                    .to_string()
                    .contains(tilo_actions::draft::AWAITING_CONFIRMATION),
                "{waiting}"
            );
            assert!(nothing_delivered(), "nothing ran before the answer");
            let asked = ask_append(&f, "Draft line");
            assert_eq!(held(&f).unwrap().id, asked.id);
            let preview = crate::services::confirmation_preview(&asked)
                .1
                .expect("the question carries the change preview");
            assert_eq!(preview.before.text, "Release notes\n");
            assert_eq!(preview.after.text, "Release notes\nDraft line\n");
            assert!(!preview.after.spoken.is_empty(), "{:?}", preview.after);
            assert!(nothing_delivered(), "a waiting question runs nothing");
            drop(f);
        })
        .await;
}

/// A held ask ends when its run stops, with no clock: the stop is
/// announced with the tracker's publication.
#[tokio::test(flavor = "current_thread")]
async fn a_held_ask_ends_when_its_run_stops() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (_target, _stream) = append_target(&f).await;
            ask_first_for_notes(&f);
            ask_append(&f, "Draft line");

            let line = serde_json::json!({ "text": "Draft line" }).to_string();
            let held = tokio::task::spawn_local({
                let (agent, run) = (agent.clone(), run.clone());
                async move { append_held(&agent, &run, 9151, line, u32::MAX).await }
            });
            tokio::task::yield_now().await;
            assert!(!held.is_finished(), "the ask is held while nobody answered");

            f.stop();
            tilo_identity_host::policy_change::announce();
            let error = tilo_test_env::within("the stopped run's answer", held)
                .await
                .unwrap()
                .unwrap_err();
            assert!(error.to_string().contains("cancelled"), "{error}");
            assert!(nothing_delivered());
            drop(f);
        })
        .await;
}

/// An ask that waits for its person is held by the host, not asked again
/// on a clock: it runs as soon as the person's answer is announced.
#[tokio::test(flavor = "current_thread")]
async fn a_held_ask_runs_once_its_person_answers() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (_target, stream) = append_target(&f).await;
            ask_first_for_notes(&f);
            ask_append(&f, "Draft line");

            let line = serde_json::json!({ "text": "Draft line" }).to_string();
            let held = tokio::task::spawn_local({
                let (agent, run) = (agent.clone(), run.clone());
                async move { append_held(&agent, &run, 9150, line, 10_000).await }
            });
            tokio::task::yield_now().await;
            assert!(!held.is_finished(), "the ask is held while nobody answered");
            assert!(nothing_delivered());

            confirm_as(&f, "Reviewed line");
            tilo_identity_host::policy_change::announce();
            let (_, update) = tilo_test_env::within("the held ask's answer", held)
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(
                update,
                tilo_actions::InvocationUpdate::Accepted { .. }
            ));
            let delivered = stream.next().await.unwrap();
            let argument: serde_json::Value =
                serde_json::from_str(&delivered.argument.unwrap()).unwrap();
            assert_eq!(argument["text"], "Reviewed line");
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
            let (_target, _stream) = append_target(&f).await;
            ask_first_for_notes(&f);
            assert!(append(&agent, &run, 9200, "Draft line").await.is_err());
            ask_append(&f, "Draft line");
            answer(
                &f,
                TrackerAnswer::Declined {
                    reason: "Wrong document".into(),
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
            assert!(nothing_delivered());
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
            ask_append(&f, "Draft line");
            confirm_as(&f, "Reviewed line");
            append_as(&agent, &run, 9300, wire()).await.unwrap();
            let delivered = stream.next().await.unwrap();
            let argument = delivered.argument.unwrap();
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
