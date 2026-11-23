//! The agent's sheets as the session's questions.
//!
//! The native session has no dialog queue of its own: the agent asks each
//! prompt as a question ([`tilo_service::Questions`]) of kind
//! [`KIND`], under the handle `polkit:<cookie>` the agent names its sheet
//! by, listed under polkit's own message. The session's sheet reads the
//! question's details as [`AuthenticateDetails`] and answers through
//! [`Questions::answer`] directly, so the typed secret never travels as an
//! action's arguments:
//!
//! - [`Answer::choice`] picks the identity to authenticate as, counted
//!   from zero in [`AuthenticateDetails::identities`]; without one, the
//!   identity the prompt was asked for.
//! - `values[0]` is what was typed; PAM decides what it accepts, an empty
//!   answer included.
//!
//! Declining the question cancels the request; polkit withdrawing it
//! withdraws the question.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tilo_service::{Answer, Question, Questions};

use crate::service::{AgentHandle, SheetAnswer, Sheets};
use crate::{PromptAnswer, PromptView, StockNotice};

/// The kind of the questions the agent asks.
pub const KIND: &str = "polkit.authenticate";

/// What the sheet shows above the prompt when the last attempt went wrong.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Notice {
    /// The agent's own words: a Fluent key ([`StockNotice::message_key`])
    /// the sheet resolves in its catalogue.
    Message {
        /// The Fluent message to show.
        key: String,
    },
    /// PAM's or polkit's own text, shown as it came.
    Verbatim(String),
}

impl Notice {
    /// `text` as the agent handed it: one of its stock notices' keys, or
    /// PAM's text.
    fn of(text: String) -> Self {
        if StockNotice::ALL
            .iter()
            .any(|notice| notice.message_key() == text)
        {
            Self::Message { key: text }
        } else {
            Self::Verbatim(text)
        }
    }
}

/// The details of a [`KIND`] question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticateDetails {
    /// polkit's message for the action, as polkit worded it.
    pub message: String,
    /// The polkit action id, shown so the request can be checked.
    pub action_id: String,
    /// Who may authorise it, as user names; more than one offers a choice.
    pub identities: Vec<String>,
    /// The identity being authenticated, in [`Self::identities`].
    pub identity: usize,
    /// PAM's prompt, e.g. "Password:", as PAM worded it.
    pub prompt: String,
    /// Whether the answer is shown as typed.
    pub echo: bool,
    /// What went wrong last time, or PAM's informational text.
    pub notice: Option<Notice>,
}

impl From<PromptView> for AuthenticateDetails {
    fn from(view: PromptView) -> Self {
        Self {
            message: view.message,
            action_id: view.action_id,
            identities: view.identities,
            identity: view.identity,
            prompt: view.prompt,
            echo: view.echo,
            notice: view.notice.map(Notice::of),
        }
    }
}

/// The prompt's answer from the person's answer to its question.
fn prompt_answer(mut answer: Answer, asked_for: usize) -> PromptAnswer {
    // The typed buffer itself moves on to PAM's helper.
    let secret = answer
        .values
        .first_mut()
        .map(|value| std::mem::take(&mut **value))
        .unwrap_or_default();
    PromptAnswer {
        identity: answer.choice.unwrap_or(asked_for),
        secret,
    }
}

/// The agent's sheets as questions in `questions`.
#[derive(Clone, Debug)]
pub struct QuestionSheets {
    questions: Questions,
}

impl QuestionSheets {
    #[must_use]
    pub fn new(questions: Questions) -> Self {
        Self { questions }
    }
}

impl Sheets for QuestionSheets {
    fn ask(&self, handle: &str, view: PromptView) -> SheetAnswer {
        let asked_for = view.identity;
        let label = view.message.clone();
        let details = serde_json::to_value(AuthenticateDetails::from(view))
            .unwrap_or(serde_json::Value::Null);
        let asked = self.questions.ask(Question {
            handle: handle.to_owned(),
            kind: KIND.to_owned(),
            app_id: None,
            label,
            details,
        });
        Box::pin(async move { asked.await.map(|answer| prompt_answer(answer, asked_for)) })
    }

    fn cancel(&self, handle: &str) {
        self.questions.decline(handle);
    }

    /// The notice's Fluent key: the session's sheet resolves it in its own
    /// catalogue ([`Notice::Message`]), and shows PAM's text verbatim.
    fn stock_notice(&self, notice: StockNotice) -> String {
        notice.message_key().to_owned()
    }
}

/// Serve the agent on the system bus, asking in `questions`, and register
/// it with polkit for this login session.
///
/// # Errors
///
/// When the system bus, logind or polkit cannot be reached.
pub async fn serve(questions: Questions) -> zbus::Result<AgentHandle> {
    crate::service::serve(Arc::new(QuestionSheets::new(questions))).await
}

/// [`serve`] on `connection`.
///
/// # Errors
///
/// When the object cannot be served, or logind or polkit cannot be reached
/// on `connection`.
pub async fn serve_on(
    connection: zbus::Connection,
    questions: Questions,
) -> zbus::Result<AgentHandle> {
    crate::service::serve_on(connection, Arc::new(QuestionSheets::new(questions))).await
}
