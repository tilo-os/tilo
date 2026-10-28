//! A native run pauses at an action asking first, through production RPCs.
#![expect(
    clippy::future_not_send,
    reason = "Capnp RPC clients are intentionally thread confined to the existing LocalSet"
)]
use super::*;
use crate::services::{TrackerAnswer, TrackerQuestion};

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
    put_grant(f, asks_first_for_notes(f));
}

/// The grant [`ask_first_for_notes`] puts.
fn asks_first_for_notes(f: &Fixture) -> tilo_action_policy::Grant {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    agent_answers::grant(
        &room().to_string(),
        "scribe",
        (
            &AnswerScope::app("so.tilo.notes"),
            tilo_action_policy::ActionKind::Undoable,
        ),
        tilo_action_policy::Answer::Ask,
        tilo_action_policy::Subject::person(hex::encode(f.native.certificate.user_public_key)),
    )
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
    append_held(channel, run, invocation, argument, 0).await
}

/// [`append_as`], asking the broker to hold the answer up to `wait_ms`.
async fn append_held(
    channel: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
    invocation: u128,
    argument: String,
    wait_ms: u32,
) -> Result<(u64, tilo_actions::InvocationUpdate), capnp::Error> {
    let mut request = channel.invoke_task_action_request();
    let mut p = request.get();
    p.set_wait_ms(wait_ms);
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

/// The Tasks app's answer to what a run asks is put to the run's person on
/// the host's sheet, with what the action would do and the issue by its
/// title to allow it for; the app's own word answers nothing.
#[tokio::test(flavor = "current_thread")]
async fn the_tasks_app_has_a_runs_question_put_to_its_person_on_the_hosts_sheet() {
    use crate::collab_broker_capnp::run_question::AnswerStep;
    use crate::services::TRACKER_APP;
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let (_target, _stream) = append_target(&f).await;
            let (sheet, mut asked) = tokio::sync::mpsc::unbounded_channel();
            f.state
                .set_app_action_consent(Arc::new(super::store_witness::Sheet(sheet)));
            let person = hex::encode(f.native.certificate.user_public_key);
            let answer = tilo_actions::remote::InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: tilo_actions::remote::OWN_PROVIDER.into(),
                    address: tilo_actions::ActionAddress::new(
                        "tasks.tracker",
                        "answer-confirmation",
                    ),
                },
                argument: tilo_actions::remote::InvokeArgument::Json(
                    serde_json::json!({ "issue": "release", "confirm": true })
                        .as_object()
                        .cloned()
                        .unwrap(),
                ),
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            };
            let step = |app: &str, who: &str| {
                let caller =
                    tilo_actions::remote::Caller::local_person(who).in_room(room().to_string());
                f.state.run_question(app, &caller, &answer)
            };

            assert!(
                matches!(step(TRACKER_APP, &person), AnswerStep::Refuse),
                "nothing is asked, so the app answers nothing"
            );
            ask_append(&f, "Draft line");
            assert!(
                matches!(step("so.tilo.notes", &person), AnswerStep::Serve),
                "another app's call is no answer"
            );
            assert!(
                matches!(step(TRACKER_APP, "someone-else"), AnswerStep::Refuse),
                "only the run's person answers it"
            );

            let AnswerStep::Ask(_once) = step(TRACKER_APP, &person) else {
                panic!("the run's person is asked");
            };
            let (request, _) = asked.try_recv().unwrap();
            assert!(request.confirming);
            assert_eq!(request.app_id, "scribe");
            assert_eq!(request.action_label, "Append");
            assert_eq!(request.draft.unwrap().fields[0].value.line(), "Draft line");
            let preview = request
                .preview
                .expect("the sheet shows the change the step makes");
            assert_eq!(preview.before.text, "Release notes\n");
            assert_eq!(preview.after.text, "Release notes\nDraft line\n");
            assert_eq!(
                request.scope, None,
                "a person who may not grant is offered once"
            );

            *f.native.granted.lock().unwrap() = Some(Vec::new());
            let AnswerStep::Ask(_for_issue) = step(TRACKER_APP, &person) else {
                panic!("the run's person is asked again");
            };
            let (request, _) = asked.try_recv().unwrap();
            assert_eq!(request.scope.as_deref(), Some("Prepare release notes"));
            drop(f);
        })
        .await;
}

/// What the run's person is told without being interrupted.
#[derive(Default)]
struct Notices {
    told: std::sync::Mutex<Vec<String>>,
    withdrawn: std::sync::Mutex<Vec<String>>,
}

impl crate::app_actions::AppActionConsentPrompter for Notices {
    fn ask(
        &self,
        _request: crate::app_actions::AppActionConsentRequest,
    ) -> Option<tokio::sync::oneshot::Receiver<crate::app_actions::AppActionConsent>> {
        panic!("a run's question never raises the sheet on its own")
    }

    fn notify(
        &self,
        request: crate::app_actions::AppActionConsentRequest,
        _answer: crate::app_actions::AnswerLater,
    ) -> bool {
        self.told.lock().unwrap().push(request.handle);
        true
    }

    fn withdraw(&self, handle: &str) {
        self.withdrawn.lock().unwrap().push(handle.to_owned());
    }
}

/// What a run asks reaches its person as a notice, once, and the notice is
/// taken back once they answered.
#[tokio::test(flavor = "current_thread")]
async fn a_runs_question_reaches_its_person_as_a_notice_once() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let (_target, _stream) = append_target(&f).await;
            let notices = Arc::new(Notices::default());
            f.state.set_app_action_consent(notices.clone());
            let rooms = std::collections::BTreeSet::from([room().to_string()]);

            let asked = ask_append(&f, "Draft line");
            f.state.refresh_tracker_runs(&rooms);
            f.state.refresh_tracker_runs(&rooms);
            let handle = format!("run-question:{}", asked.id);
            assert_eq!(*notices.told.lock().unwrap(), [handle.as_str()]);
            assert!(notices.withdrawn.lock().unwrap().is_empty());

            confirm_as(&f, "Draft line");
            f.state.refresh_tracker_runs(&rooms);
            assert_eq!(*notices.withdrawn.lock().unwrap(), [handle]);
            assert_eq!(notices.told.lock().unwrap().len(), 1);
            drop(f);
        })
        .await;
}

/// Nothing was dispatched to the target app (peer 200) that it has not read.
pub(super) fn nothing_delivered() -> bool {
    crate::collab_broker_capnp::unread_invocations(200) == 0
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
            let delivered = stream.read_request().send().promise.await.unwrap();
            let wire = delivered.get().unwrap();
            let argument: serde_json::Value =
                serde_json::from_str(wire.get_argument().unwrap().to_str().unwrap()).unwrap();
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
            let delivered = stream.read_request().send().promise.await.unwrap();
            let wire = delivered.get().unwrap();
            let argument: serde_json::Value =
                serde_json::from_str(wire.get_argument().unwrap().to_str().unwrap()).unwrap();
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
            let asked = ask_append(&f, "Draft line");
            let shown: tilo_actions::Draft = serde_json::from_value(asked.draft.clone()).unwrap();

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
            assert_eq!(paused.draft, shown);
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
                TrackerAnswer::Confirmed {
                    edits: serde_json::Value::Null,
                },
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
            drop(f);
        })
        .await;
}
