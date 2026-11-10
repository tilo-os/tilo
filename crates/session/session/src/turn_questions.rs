//! A conversation turn's ask, put to the person on the system's sheet.
//!
//! An agent's step inside a conversation turn whose grants or contract ask
//! first is held by the broker (`tilo_action_broker`'s `conversation_ask`).
//! The person's answer arrives here as an [`AppActionConsentRequest`], and
//! the sheet that draws the question is this session's own
//! ([`crate::questions`], on the portal's trusted sheet). The conversation
//! may point at that ask; it never draws it (`docs/designs/app-language/
//! shell.md` §1, "Trust stays system-drawn").
//!
//! An answer is the person's own for that one step: the sheet's confirm is
//! Allow once, its cancel is Deny, and there is no "always allow" row — the
//! standing answer for an agent's action is a grant, given in the
//! workspace's settings, never a click on a question.

use std::sync::Arc;

use tilo_action_broker::app_actions::{
    AppActionAnswer, AppActionConsent, AppActionConsentPrompter, AppActionConsentRequest,
};
use tilo_service::{Answer, Asked, Question, Questions};
use tokio::sync::oneshot;

/// The kind of question a conversation turn's ask is: the sheet composes it
/// ([`crate::questions`]), and only a turn's ask is under it.
pub(crate) const KIND: &str = "session.turn-question";

/// Ask the person on this session's sheet what an agent's step inside a
/// conversation asks first.
pub(crate) fn install(
    broker: Option<&tilo_action_broker::collab_broker::BrokerHandle>,
    questions: Option<Questions>,
) {
    let (Some(broker), Some(questions)) = (broker, questions) else {
        // No sheet is up here, or no broker runs: the broker refuses the
        // step with why, and the agent says so in its reply.
        return;
    };
    let Some(actions) = broker.app_action_broker() else {
        return;
    };
    actions.set_app_action_consent(Arc::new(TurnQuestions { questions }));
}

/// What a conversation turn's ask shows: the draft as the person reviews
/// it, and what it acts on by its labels.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(crate) struct TurnDetails {
    pub(crate) resource_label: String,
    pub(crate) action_label: String,
    pub(crate) draft: tilo_actions::Draft,
}

/// The session's sheet, as an agent's conversation-turn ask reaches it.
struct TurnQuestions {
    questions: Questions,
}

impl AppActionConsentPrompter for TurnQuestions {
    fn ask(&self, request: AppActionConsentRequest) -> Option<oneshot::Receiver<AppActionAnswer>> {
        // Only a conversation turn's step, never a grant or a Store
        // disclosure: those name the person and the app asking, and a turn's
        // ask names neither (the broker holds it for the run's person).
        if !request.confirming || !request.app_id.is_empty() || !request.person.is_empty() {
            return None;
        }
        let draft = request.draft.clone().unwrap_or_default();
        let details = serde_json::to_value(TurnDetails {
            resource_label: request.resource_label.clone(),
            action_label: request.action_label.clone(),
            draft,
        })
        .ok()?;
        let asked = self.questions.ask(Question {
            handle: format!("turn-question:{}", uuid::Uuid::new_v4().simple()),
            kind: KIND.to_owned(),
            app_id: None,
            label: request.action_label,
            details,
        });
        let (answer, answered) = oneshot::channel();
        // The person answers on the sheet in their own time, and the
        // broker's thread waits for it; nothing here holds the session's
        // loop.
        let spawned = std::thread::Builder::new()
            .name("turn-question-answer".into())
            .spawn(move || {
                let _ = answer.send(answer_to(wait(asked).as_ref()));
            });
        if spawned.is_err() {
            return None;
        }
        Some(answered)
    }
}

/// `asked`, waited for on this thread until the person answers or the
/// sheet goes away. A session that goes away ends the wait too: the
/// question is withdrawn with it.
fn wait(mut asked: Asked) -> Option<Answer> {
    // No deadline of the ask's own: the person decides in their own time.
    // The wait ends when the question does — answered, declined or
    // withdrawn.
    asked.until(None).flatten()
}

/// What the person's answer to a turn's question means for the step.
fn answer_to(answer: Option<&Answer>) -> AppActionAnswer {
    // Confirming is the answer; a question that asks nothing else carries
    // no choice, so its presence is the person's allow.
    if answer.is_some() {
        AppActionAnswer::unedited(AppActionConsent::Once)
    } else {
        AppActionAnswer::DENY
    }
}
