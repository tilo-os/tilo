//! A conversation turn's ask goes to the person on the system's own sheet,
//! and the turn waits for their answer (`shell.md` §1, "Trust stays
//! system-drawn"; §8.1).
//!
//! The fixture is the confirmation tests' own: Notes' `append` published on
//! peer 200, an agent's grant that asks first for it, and a run whose task
//! id is a conversation turn's message. The one thing this file adds is the
//! person's sheet, which stands where a desktop's sheet stands
//! ([`AppActionConsentPrompter`]) and answers as the person does.
use super::super::tests::confirmation::{
    append, append_target, ask_first_for_notes, nothing_delivered,
};
use super::*;
use crate::app_actions::{AppActionAnswer, AppActionConsent};
use crate::app_actions::{AppActionConsentPrompter, AppActionConsentRequest};

/// The turn a conversation asks in: Chat's record, the channel, and the
/// message the agent answers.
const CONVERSATION: &str = "chat.workspace/channels/the-conversation/messages/asked";

/// A question on the person's sheet, waiting for their answer.
struct Asked {
    request: AppActionConsentRequest,
    answer: Option<tokio::sync::oneshot::Sender<AppActionAnswer>>,
}

/// The person's sheet: the questions it shows, in the order they arrived,
/// and the answers the person gives them one at a time.
#[derive(Default)]
struct PersonSheet {
    asked: std::sync::Mutex<Vec<Asked>>,
}

impl PersonSheet {
    /// The person answers the oldest question they have not answered.
    fn answer_next(&self, consent: AppActionConsent) {
        let answer = {
            let mut asked = self.asked.lock().unwrap();
            asked.iter_mut().find_map(|asked| asked.answer.take())
        };
        if let Some(answer) = answer {
            let _ = answer.send(AppActionAnswer::unedited(consent));
        }
        // A held ask is decided again when a decision may have changed.
        tilo_identity_host::policy_change::announce();
    }

    fn requests(&self) -> Vec<AppActionConsentRequest> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .map(|asked| asked.request.clone())
            .collect()
    }
}

impl AppActionConsentPrompter for PersonSheet {
    fn ask(
        &self,
        request: AppActionConsentRequest,
    ) -> Option<tokio::sync::oneshot::Receiver<AppActionAnswer>> {
        let (answer, answered) = tokio::sync::oneshot::channel();
        self.asked.lock().unwrap().push(Asked {
            request,
            answer: Some(answer),
        });
        Some(answered)
    }
}

/// The fixture's run is a conversation turn: its task id is a message of a
/// conversation, which is where an ask belongs to the person's sheet rather
/// than to an issue.
fn a_turn(f: &Fixture) {
    f.approve();
    f.on_run(|run| run.issue = CONVERSATION.into());
}

/// The agent's step, its grant asking first: the step is held, the person is
/// asked on their sheet, and their answer runs the step with exactly what
/// they confirmed.
#[tokio::test(flavor = "current_thread")]
async fn a_conversations_ask_goes_to_the_persons_sheet_and_their_answer_runs_it() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            a_turn(&f);
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (_target, stream) = append_target(&f).await;
            ask_first_for_notes(&f);
            let sheet = Arc::new(PersonSheet::default());
            f.state.set_app_action_consent(sheet.clone());

            // The step asks: the child is not started and the person sees
            // the question on the system's sheet.
            let waiting = append(&agent, &run, 9500, "Draft line").await.unwrap_err();
            assert!(
                waiting
                    .to_string()
                    .contains(tilo_actions::draft::AWAITING_CONFIRMATION),
                "{waiting}"
            );
            assert!(nothing_delivered(), "nothing ran before the answer");
            let asked = sheet.requests();
            assert_eq!(asked.len(), 1, "one question on the sheet: {asked:?}");
            assert!(asked[0].confirming, "it confirms a step, not a grant");
            assert_eq!(asked[0].action_label, "Append");
            assert_eq!(
                asked[0]
                    .draft
                    .as_ref()
                    .and_then(|draft| draft.fields.first())
                    .map(|field| field.value.line()),
                Some("Draft line".to_owned()),
                "the sheet shows the step's draft"
            );

            // The person allows this once: the step runs, with what they
            // saw.
            sheet.answer_next(AppActionConsent::Once);
            let (_, update) = append(&agent, &run, 9500, "Draft line").await.unwrap();
            assert!(matches!(
                update,
                tilo_actions::InvocationUpdate::Accepted { .. }
            ));
            let delivered = stream.next().await.unwrap();
            let argument: serde_json::Value =
                serde_json::from_str(&delivered.argument.unwrap()).unwrap();
            assert_eq!(
                argument["text"], "Draft line",
                "what the person confirmed ran"
            );
            drop(f);
        })
        .await;
}

/// The person declines the step: the agent is told in their words, nothing
/// runs, and the turn goes on.
#[tokio::test(flavor = "current_thread")]
async fn a_declined_ask_runs_nothing_and_tells_the_agent_so() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            a_turn(&f);
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (_target, _stream) = append_target(&f).await;
            ask_first_for_notes(&f);
            let sheet = Arc::new(PersonSheet::default());
            f.state.set_app_action_consent(sheet.clone());

            let waiting = append(&agent, &run, 9501, "Draft line").await.unwrap_err();
            assert!(
                waiting
                    .to_string()
                    .contains(tilo_actions::draft::AWAITING_CONFIRMATION),
                "{waiting}"
            );
            assert_eq!(sheet.requests().len(), 1);
            sheet.answer_next(AppActionConsent::Deny);

            let declined = append(&agent, &run, 9501, "Draft line").await.unwrap_err();
            assert!(
                declined.to_string().contains(tilo_actions::draft::DECLINED),
                "{declined}"
            );
            assert!(nothing_delivered(), "a declined step ran nothing");
            drop(f);
        })
        .await;
}

/// The agent asks again while the person decides: the same question stays up
/// once, and the second ask is answered with the wait — never a second
/// question on the sheet.
#[tokio::test(flavor = "current_thread")]
async fn asking_again_while_the_person_decides_raises_one_question() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            a_turn(&f);
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (_target, _stream) = append_target(&f).await;
            ask_first_for_notes(&f);
            let sheet = Arc::new(PersonSheet::default());
            f.state.set_app_action_consent(sheet.clone());

            for _ in 0..3 {
                let waiting = append(&agent, &run, 9502, "Draft line").await.unwrap_err();
                assert!(
                    waiting
                        .to_string()
                        .contains(tilo_actions::draft::AWAITING_CONFIRMATION),
                    "{waiting}"
                );
            }
            assert_eq!(
                sheet.requests().len(),
                1,
                "one question, however often the agent asks: {:?}",
                sheet.requests()
            );
            drop(f);
        })
        .await;
}

/// The question of a turn that ended goes: it stands on nobody's screen.
#[tokio::test(flavor = "current_thread")]
async fn an_ended_turns_question_is_taken_away() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            a_turn(&f);
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (_target, _stream) = append_target(&f).await;
            ask_first_for_notes(&f);
            let sheet = Arc::new(PersonSheet::default());
            f.state.set_app_action_consent(sheet.clone());

            let _ = append(&agent, &run, 9503, "Draft line").await.unwrap_err();
            assert_eq!(sheet.requests().len(), 1);
            // The run ends: its agent reported it done.
            for run in f.native.runs.lock().unwrap().iter_mut() {
                run.active = false;
            }
            f.state.refresh_native_task_delivery();
            f.state
                .refresh_tracker_runs(&std::collections::BTreeSet::from([
                    workspace_id().to_string()
                ]));
            assert!(
                f.state.conversation_asks.is_empty(),
                "the question went with the turn"
            );
            drop(f);
        })
        .await;
}

/// The session's own arrangement, as a turn's host step: `open` places the
/// resource it names, and records what it was handed.
#[derive(Default)]
struct TurnArrangement {
    placed: std::sync::Mutex<Vec<(String, String)>>,
}

impl crate::services::HostStepServices for TurnArrangement {
    fn provider(&self) -> &'static str {
        tilo_actions::remote::session_actions::PROVIDER
    }
    fn app_id(&self) -> &'static str {
        tilo_actions::remote::session_actions::PROVIDER
    }
    fn for_turns(&self) -> bool {
        true
    }
    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        vec![tilo_actions::ActionDescriptor {
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new(
                tilo_actions::remote::session_actions::ARRANGEMENT_RESOURCE,
                tilo_actions::remote::session_actions::OPEN,
            ),
            resource_kind: "shell".into(),
            resource_label: "The person's arrangement".into(),
            action_label: "Open a resource with its own view".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: tilo_actions::ExecutionLocus::default(),
            kind: tilo_actions::OperationKind::Effect,
            contract: tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Undoable,
                ..tilo_actions::ActionContract::default()
            },
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        }]
    }
    fn start(
        &self,
        request: &crate::services::HostStepRequest<'_>,
    ) -> Result<tilo_actions::remote::Pending<crate::services::HostStepOutcome>, String> {
        let resource = match tilo_actions::remote::InvokeArgument::from_slot(request.argument) {
            tilo_actions::remote::InvokeArgument::Json(arguments) => arguments
                .get("resource")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or("the arrangement's open names no resource")?,
            _ => return Err("the arrangement's open takes its arguments by name".into()),
        };
        self.placed
            .lock()
            .unwrap()
            .push((request.actor.id().to_owned(), resource));
        Ok(tilo_actions::remote::Pending::ready(Ok(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

/// The step of [`TurnArrangement`], invoked through the broker's own door by
/// a caller on the turn, as the agent's child is.
fn arrangement_invoke(
    state: &Arc<AppBrokerState>,
    caller: &Caller,
    invocation: u128,
    resource: &str,
) -> Result<tilo_actions::remote::InvocationReceipt, ActionHostError> {
    let arguments =
        serde_json::Map::from_iter([("resource".to_owned(), serde_json::Value::from(resource))]);
    state
        .action_invoke(
            caller,
            None,
            None,
            &InvokeRequest {
                invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(invocation)),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: tilo_actions::remote::session_actions::PROVIDER.into(),
                    address: tilo_actions::ActionAddress::new(
                        tilo_actions::remote::session_actions::ARRANGEMENT_RESOURCE,
                        tilo_actions::remote::session_actions::OPEN,
                    ),
                },
                argument: tilo_actions::remote::InvokeArgument::Json(arguments),
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                appearance: None,
                wait_ms: None,
            },
        )
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("admission answers at once")
}

/// Let the turn's agent `open` through the session's arrangement, or ask it
/// first, as the person who started the turn.
fn arrangement_grant(f: &Fixture, answer: tilo_action_policy::Answer) {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    use tilo_action_policy::{Grant, Grantee, KindSet, TreePath};
    let node = tilo_actions::action_node(
        &TreePath::workspace(workspace_id().to_string()),
        tilo_actions::remote::session_actions::PROVIDER,
        "shell",
        tilo_actions::remote::session_actions::ARRANGEMENT_RESOURCE,
        None,
        tilo_actions::remote::session_actions::OPEN,
    );
    let grant = if answer == tilo_action_policy::Answer::Ask {
        agent_answers::grant(
            &workspace_id().to_string(),
            "scribe",
            (
                &AnswerScope::app(tilo_actions::remote::session_actions::PROVIDER),
                tilo_action_policy::ActionKind::Undoable,
            ),
            tilo_action_policy::Answer::Ask,
            Grantee::person(hex::encode(f.native.certificate.user_public_key)),
        )
    } else {
        Grant::new(
            "fixture-arrangement-open",
            Grantee::agent("scribe"),
            node,
            KindSet::UNDOABLE,
            answer,
            Grantee::person(hex::encode(f.native.certificate.user_public_key)),
        )
    };
    super::put_grant(f, grant);
}

/// A turn's own host step whose grants ask first raises the turn's question
/// on the person's system sheet — the very path an app action's ask takes —
/// and their answer runs the step, exactly as they confirmed it.
#[tokio::test(flavor = "current_thread")]
async fn a_turn_steps_ask_goes_to_the_persons_sheet_and_their_answer_runs_it() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let steps = Arc::new(TurnArrangement::default());
            let f = Fixture::with_services(None, vec![steps.clone()]);
            f.approve();
            let turn = tilo_action_policy::turn::Turn::new(
                "chat.workspace/channels/the-conversation",
                "asked",
            );
            f.on_run(|run| {
                run.issue = CONVERSATION.into();
                run.turn = Some(turn.clone());
            });
            f.caused("asked");
            arrangement_grant(&f, tilo_action_policy::Answer::Ask);
            let sheet = Arc::new(PersonSheet::default());
            f.state.set_app_action_consent(sheet.clone());
            let agent = Caller::agent_turn(
                "scribe",
                hex::encode(f.native.certificate.user_public_key),
                turn,
            )
            .in_workspace(workspace_id().to_string());

            // The step asks: the question stands on the person's sheet, and
            // the child hears the wait the app action's held child hears.
            let waiting =
                arrangement_invoke(&f.state, &agent, 9600, "tl:so.tilo.lang.notes:root-1")
                    .unwrap_err();
            assert!(
                matches!(&waiting, ActionHostError::Refused { reason } if reason.contains(tilo_actions::draft::AWAITING_CONFIRMATION)),
                "{waiting:?}"
            );
            assert!(steps.placed.lock().unwrap().is_empty(), "nothing ran");
            let asked = sheet.requests();
            assert_eq!(asked.len(), 1, "one question on the sheet: {asked:?}");
            assert!(asked[0].confirming, "it confirms a step, not a grant");

            // The person allows this once: the step runs.
            sheet.answer_next(AppActionConsent::Once);
            let receipt =
                arrangement_invoke(&f.state, &agent, 9600, "tl:so.tilo.lang.notes:root-1").unwrap();
            assert!(
                matches!(
                    receipt.status,
                    tilo_actions::remote::ReceiptStatus::Completed { .. }
                ),
                "the step ran: {:?}",
                receipt.status
            );
            assert_eq!(
                steps.placed.lock().unwrap().as_slice(),
                [(
                    "agent:scribe".to_owned(),
                    "tl:so.tilo.lang.notes:root-1".to_owned()
                )],
                "the session's open placed what the person confirmed"
            );
            drop(f);
        })
        .await;
}

/// A turn's session step whose grants allow it runs at once: it is admitted,
/// recorded, and stands its place — no question, no refusal.
#[tokio::test(flavor = "current_thread")]
async fn a_turn_steps_allowed_open_stands_the_place() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let steps = Arc::new(TurnArrangement::default());
            let f = Fixture::with_services(None, vec![steps.clone()]);
            f.approve();
            let turn = tilo_action_policy::turn::Turn::new(
                "chat.workspace/channels/the-conversation",
                "asked",
            );
            f.on_run(|run| {
                run.issue = CONVERSATION.into();
                run.turn = Some(turn.clone());
            });
            f.caused("asked");
            arrangement_grant(&f, tilo_action_policy::Answer::Allow);
            let sheet = Arc::new(PersonSheet::default());
            f.state.set_app_action_consent(sheet.clone());
            let agent = Caller::agent_turn(
                "scribe",
                hex::encode(f.native.certificate.user_public_key),
                turn,
            )
            .in_workspace(workspace_id().to_string());
            let receipt =
                arrangement_invoke(&f.state, &agent, 9601, "tl:so.tilo.lang.tasks:root-1").unwrap();
            assert!(
                matches!(
                    receipt.status,
                    tilo_actions::remote::ReceiptStatus::Completed { .. }
                ),
                "the step ran: {:?}",
                receipt.status
            );
            assert_eq!(
                steps.placed.lock().unwrap().as_slice(),
                [(
                    "agent:scribe".to_owned(),
                    "tl:so.tilo.lang.tasks:root-1".to_owned()
                )]
            );
            assert!(
                sheet.requests().is_empty(),
                "nothing asked: {:?}",
                sheet.requests()
            );
            drop(f);
        })
        .await;
}

/// A session step the broker cannot route, admit or record is answered with
/// its error at once: no turn's question waits on it, and no row is left
/// standing as an accepted child with nothing behind it.
#[tokio::test(flavor = "current_thread")]
async fn a_turn_step_that_cannot_be_routed_is_refused_at_once() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let steps = Arc::new(TurnArrangement::default());
            let f = Fixture::with_services(None, vec![steps]);
            f.approve();
            let turn = tilo_action_policy::turn::Turn::new(
                "chat.workspace/channels/the-conversation",
                "asked",
            );
            f.on_run(|run| {
                run.issue = CONVERSATION.into();
                run.turn = Some(turn.clone());
            });
            f.caused("asked");
            arrangement_grant(&f, tilo_action_policy::Answer::Ask);
            let sheet = Arc::new(PersonSheet::default());
            f.state.set_app_action_consent(sheet);
            let agent = Caller::agent_turn(
                "scribe",
                hex::encode(f.native.certificate.user_public_key),
                turn,
            )
            .in_workspace(workspace_id().to_string());
            // A provider nothing serves at all cannot be routed, so the
            // child is refused with its own reason at once: no row stands as
            // an accepted child with nothing behind it, and no question waits.
            let unrouted = arrangement_invoke_unserved(&f.state, &agent, 9603).unwrap_err();
            assert!(
                matches!(unrouted, ActionHostError::UnknownAction),
                "{unrouted:?}"
            );
            drop(f);
        })
        .await;
}

/// The same step invoked under a provider nothing serves here: it cannot be
/// routed at all.
fn arrangement_invoke_unserved(
    state: &Arc<AppBrokerState>,
    caller: &Caller,
    invocation: u128,
) -> Result<tilo_actions::remote::InvocationReceipt, ActionHostError> {
    state
        .action_invoke(
            caller,
            None,
            None,
            &InvokeRequest {
                invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(invocation)),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: "so.tilo.nowhere".into(),
                    address: tilo_actions::ActionAddress::new(
                        tilo_actions::remote::session_actions::ARRANGEMENT_RESOURCE,
                        tilo_actions::remote::session_actions::OPEN,
                    ),
                },
                argument: tilo_actions::remote::InvokeArgument::Json(serde_json::Map::new()),
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                appearance: None,
                wait_ms: None,
            },
        )
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("admission answers at once")
}

/// A turn's step that asks first with no sheet installed on this device is
/// refused at once, in the app action's own words — never told a question
/// waits which nobody can answer.
#[tokio::test(flavor = "current_thread")]
async fn a_turn_step_that_asks_with_no_sheet_is_refused_at_once() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let steps = Arc::new(TurnArrangement::default());
            let f = Fixture::with_services(None, vec![steps.clone()]);
            f.approve();
            let turn = tilo_action_policy::turn::Turn::new(
                "chat.workspace/channels/the-conversation",
                "asked",
            );
            f.on_run(|run| {
                run.issue = CONVERSATION.into();
                run.turn = Some(turn.clone());
            });
            f.caused("asked");
            arrangement_grant(&f, tilo_action_policy::Answer::Ask);
            // No sheet is installed on this broker.
            let agent = Caller::agent_turn(
                "scribe",
                hex::encode(f.native.certificate.user_public_key),
                turn,
            )
            .in_workspace(workspace_id().to_string());
            let refused =
                arrangement_invoke(&f.state, &agent, 9605, "tl:so.tilo.lang.notes:root-1")
                    .unwrap_err();
            assert!(
                matches!(&refused, ActionHostError::Refused { reason } if !reason.contains(tilo_actions::draft::AWAITING_CONFIRMATION)),
                "refused at once, never said to wait: {refused:?}"
            );
            assert!(steps.placed.lock().unwrap().is_empty(), "nothing ran");
            assert!(
                f.state.conversation_asks.is_empty(),
                "no question is held where none could be asked"
            );
            drop(f);
        })
        .await;
}

/// A session step whose grants ask first, on a turn that is not a
/// conversation's (an issue's run), is refused at once: a session step has
/// no issue to ask on, so telling the agent it waits would hold it forever.
#[tokio::test(flavor = "current_thread")]
async fn a_session_step_that_asks_on_an_issue_run_is_refused_at_once() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let steps = Arc::new(TurnArrangement::default());
            let f = Fixture::with_services(None, vec![steps.clone()]);
            f.approve();
            arrangement_grant(&f, tilo_action_policy::Answer::Ask);
            let sheet = Arc::new(PersonSheet::default());
            f.state.set_app_action_consent(sheet.clone());
            let turn = tilo_action_policy::turn::Turn::new("tasks.tracker/issues/release", "m1");
            f.on_run(|run| run.turn = Some(turn.clone()));
            f.caused("m1");
            let agent = Caller::agent_turn(
                "scribe",
                hex::encode(f.native.certificate.user_public_key),
                turn,
            )
            .in_workspace(workspace_id().to_string());
            let refused =
                arrangement_invoke(&f.state, &agent, 9604, "tl:so.tilo.lang.notes:root-1")
                    .unwrap_err();
            assert!(
                matches!(&refused, ActionHostError::Refused { reason } if !reason.contains(tilo_actions::draft::AWAITING_CONFIRMATION)),
                "refused at once, never said to wait: {refused:?}"
            );
            assert!(steps.placed.lock().unwrap().is_empty(), "nothing ran");
            assert!(
                sheet.requests().is_empty(),
                "no question waits: {:?}",
                sheet.requests()
            );
            drop(f);
        })
        .await;
}
