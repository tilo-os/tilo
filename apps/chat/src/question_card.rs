//! What a question shows its reader, in words: what an action that asks
//! first would do, the answers while it waits for that very reader, and how
//! it stands once it no longer does. Chat's window and the conversation's
//! page draw the same card from this.

use serde::{Deserialize, Serialize};

use crate::i18n::{t, t_args};
use crate::question::{
    ACTION, ALLOW, ALWAYS, CHOICE, DENY, DONE, DROPPED, Question, STARTED, Standing,
};

/// The most fields of a draft a card lists: what a person reads at a glance.
const MAX_FIELDS: usize = 8;

/// What one line of a question's card is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Line {
    /// What would happen: the action.
    Title,
    /// What it acts on, one field of its draft, or a note about it.
    Field,
    /// How the question stands.
    Status,
}

/// One answer as a button.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Offered {
    /// The answer, by its id.
    pub id: String,
    /// What the button says.
    pub label: String,
    /// Whether it is the answer that goes ahead.
    #[serde(default)]
    pub primary: bool,
}

/// What a question shows to one reader.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QuestionShown {
    /// Its lines, in the order they are read.
    #[serde(default)]
    pub lines: Vec<(String, Line)>,
    /// The answers, while it waits for this very reader.
    #[serde(default)]
    pub answers: Vec<Offered>,
    /// Whether it stands on a card: it shows what would happen.
    #[serde(default)]
    pub card: bool,
    /// Whether the answer pressed is posted as the reader's own message: a
    /// choice, whose asker reads the answer as what its person says next.
    #[serde(default)]
    pub posts: bool,
}

impl QuestionShown {
    /// Everything it says, as a screen reader reads it with the message.
    #[must_use]
    pub fn spoken(&self) -> String {
        self.lines
            .iter()
            .map(|(line, _)| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Who reads a question, and what they know of its conversation.
#[derive(Clone, Copy, Debug)]
pub struct Reader<'a> {
    /// Whether the reader is the person it asks.
    pub mine: bool,
    /// What the reader calls the person it asks, when they know them.
    pub name: Option<&'a str>,
    /// Whether nothing its person wrote in the conversation came after it:
    /// words they typed are an answer to a choice too.
    pub open: bool,
}

/// What the button of the answer `id` says: the asker's own words, else the
/// words of the kind's own answers.
#[must_use]
pub fn answer_label(question: &Question, id: &str) -> String {
    if let Some(label) = question.label(id) {
        return label.to_owned();
    }
    match id {
        ALLOW => t("chat-question-allow"),
        // What "always" covers is the question's own: one event, or one
        // item of the vault where the action would use it.
        ALWAYS if question.uses_item().is_some() => t("chat-question-always-item"),
        ALWAYS => t("chat-question-always"),
        DENY => t("chat-question-deny"),
        other => other.to_owned(),
    }
}

/// How an answered question reads back.
fn answered_line(question: &Question, answer: &str, reader: &Reader<'_>) -> String {
    let said = match (question.kind.0.as_str(), answer) {
        (ACTION, ALLOW) => t("chat-question-allowed"),
        (ACTION, ALWAYS) if question.uses_item().is_some() => {
            t("chat-question-always-item-allowed")
        }
        (ACTION, ALWAYS) => t("chat-question-always-allowed"),
        (ACTION, DENY) => t("chat-question-denied"),
        _ => answer_label(question, answer),
    };
    match (reader.mine, reader.name) {
        (true, _) => t_args("chat-question-you-answered", &[("answer", said.into())]),
        (false, Some(name)) => t_args(
            "chat-question-answered",
            &[("name", name.into()), ("answer", said.into())],
        ),
        (false, None) => t_args("chat-question-someone-answered", &[("answer", said.into())]),
    }
}

/// What `question` shows `reader`; `None` when it has nothing to say to
/// them.
#[must_use]
pub fn shown(question: &Question, reader: &Reader<'_>) -> Option<QuestionShown> {
    let kind = question.kind.0.as_str();
    // A secret's card is worded where its protected field is drawn.
    if kind == crate::question::SECRET {
        return Some(crate::secret_ask::shown(question, reader));
    }
    let mut shown = QuestionShown {
        posts: kind == CHOICE,
        ..QuestionShown::default()
    };
    if kind == ACTION
        && let Some(draft) = question.draft()
    {
        shown.card = true;
        shown.lines.push((draft.action_label.clone(), Line::Title));
        if !draft.resource_label.trim().is_empty() {
            shown.lines.push((
                t_args(
                    "chat-question-on",
                    &[("resource", draft.resource_label.clone().into())],
                ),
                Line::Field,
            ));
        }
        for (label, value) in draft.summary().into_iter().take(MAX_FIELDS) {
            shown.lines.push((
                t_args(
                    "chat-question-field",
                    &[("label", label.into()), ("value", value.into())],
                ),
                Line::Field,
            ));
        }
        // Which item of their vault it would use, in the system's words.
        if let Some(item) = question.uses_item() {
            shown.lines.push((
                t_args(
                    "chat-question-with-item",
                    &[("item", crate::secret_ask::title(&item).into())],
                ),
                Line::Field,
            ));
        }
        if !draft.undoable {
            shown.lines.push((t("chat-question-final"), Line::Field));
        }
    }
    match question.standing() {
        Standing::Waiting if kind == CHOICE && !reader.open => {}
        Standing::Waiting if reader.mine => {
            shown.answers = question
                .answers
                .iter()
                .map(|answer| Offered {
                    id: answer.id.clone(),
                    label: answer_label(question, &answer.id),
                    primary: kind != CHOICE && answer.id == ALLOW,
                })
                .collect();
        }
        Standing::Waiting => shown.lines.push((
            reader.name.map_or_else(
                || t("chat-question-waiting-someone"),
                |name| t_args("chat-question-waiting", &[("name", name.into())]),
            ),
            Line::Status,
        )),
        // A choice's answer is the message its person posted under it.
        Standing::Answered(_) | Standing::Settled(..) if kind == CHOICE => {}
        Standing::Answered(given) => shown.lines.push((
            answered_line(question, &given.answer.0, reader),
            Line::Status,
        )),
        Standing::Settled(given, outcome) => {
            shown.lines.push((
                answered_line(question, &given.answer.0, reader),
                Line::Status,
            ));
            shown.lines.push((
                match outcome.outcome.0.as_str() {
                    STARTED => t("chat-question-started"),
                    DONE => t("chat-question-done"),
                    DROPPED => t("chat-question-dropped"),
                    _ => t("chat-question-failed"),
                },
                Line::Status,
            ));
            if !outcome.detail.is_empty() {
                shown.lines.push((outcome.detail.clone(), Line::Status));
            }
        }
        Standing::Withdrawn(withdrawn) => shown.lines.push((
            if withdrawn.why.is_empty() {
                t("chat-question-withdrawn")
            } else {
                t_args(
                    "chat-question-withdrawn-why",
                    &[("why", withdrawn.why.clone().into())],
                )
            },
            Line::Status,
        )),
    }
    (!shown.lines.is_empty() || !shown.answers.is_empty()).then_some(shown)
}
