//! A conversation turn's ask goes to the person on the system's own sheet,
//! and the turn waits for their answer (`shell.md` §1, "Trust stays
//! system-drawn"; §8.1).
//!
//! The fixture is the confirmation tests' own: Notes' `append` published on
//! peer 200, an agent's grant that asks first for it, and a run whose task
//! id is a conversation turn's message. The one thing this file adds is the
//! person's sheet, which stands where a desktop's sheet stands
//! ([`AppActionConsentPrompter`]) and answers as the person does.
#![expect(
    clippy::future_not_send,
    reason = "the broker's Rc-backed channel handles stay on the test LocalSet"
)]
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
