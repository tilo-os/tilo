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

use tilo_action_broker::app_actions::{AppActionAnswer, AppActionConsent, AppActionConsentRequest};
use tilo_service::{Answer, Asked, Question, Questions};
use tokio::sync::oneshot;

/// The kind of question a conversation turn's ask is: the sheet composes it
/// ([`crate::questions`]), and only a turn's ask is under it.
pub(crate) const KIND: &str = "session.turn-question";

/// Whether `request` is a conversation turn's step: never a grant, a run's question or a Store
/// disclosure. Those name the person and the app asking, and a turn's ask names neither (the
/// broker holds it for the run's person). The broker has one prompter, the desk's sheet
/// ([`crate::run_questions::Sheet`]), and it hands such a step here.
pub(crate) fn is_turn_ask(request: &AppActionConsentRequest) -> bool {
    request.confirming && request.app_id.is_empty() && request.person.is_empty()
}

/// What a conversation turn's ask shows: the draft as the person reviews
/// it, and what it acts on by its labels.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(crate) struct TurnDetails {
    pub(crate) resource_label: String,
    pub(crate) action_label: String,
    pub(crate) draft: tilo_actions::Draft,
}

/// Put a conversation turn's step ([`is_turn_ask`]) to the person on the session's sheet.
pub(crate) fn ask(
    questions: &Questions,
    request: AppActionConsentRequest,
) -> Option<oneshot::Receiver<AppActionAnswer>> {
    let draft = request.draft.clone().unwrap_or_default();
    let details = serde_json::to_value(TurnDetails {
        resource_label: request.resource_label.clone(),
        action_label: request.action_label.clone(),
        draft,
    })
    .ok()?;
    let asked = questions.ask(Question {
        handle: format!("turn-question:{}", uuid::Uuid::new_v4().simple()),
        kind: KIND.to_owned(),
        app_id: None,
        label: request.action_label,
        details,
    });
    let (answer, answered) = oneshot::channel();
    // The person answers on the sheet in their own time, and asks nothing of
    // the session's loop meanwhile. Their answer is a change the broker's
    // decision reads, so it is announced: that is what wakes a held child.
    let spawned = std::thread::Builder::new()
        .name("turn-question-answer".into())
        .spawn(move || {
            let _ = answer.send(answer_to(wait(asked).as_ref()));
            tilo_identity_host::policy_change::announce();
        });
    if spawned.is_err() {
        return None;
    }
    Some(answered)
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
