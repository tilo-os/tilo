//! An agent's question on a workspace host: who asks, in which group, exactly
//! what would happen and where, and the person's answer for that one
//! invocation. Or a question the agent words itself ([`AgentAsk`]): a decision
//! or something only the person knows, answered with one of its choices or
//! in their own words. A native session and a page show it on the same sheet,
//! in the same words.

use serde::{Deserialize, Serialize};

use super::{Body, Dialog};
use crate::{word, words};

/// An agent's question, as the sheet shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct AgentQuestion {
    /// The agent, by the name it goes by in History.
    pub agent: String,
    /// The group it asks in, by the title it gave it; empty when it named none.
    #[serde(default)]
    pub title: String,
    /// The action, by the title its app declares.
    pub action: String,
    /// The record it acts on, by its name.
    pub resource: String,
    /// The arguments it would run with, each by its label; secrets withheld.
    #[serde(default)]
    pub arguments: Vec<(String, String)>,
    /// What the host says the action risks, by its risk level's name.
    pub risk: String,
    /// What the action is able to do (`read`, `undoable`, `outside`).
    #[serde(default)]
    pub kind: String,
    /// Whether Undo takes it back.
    #[serde(default)]
    pub undoable: bool,
    /// The workspace it acts in.
    pub workspace: String,
    /// The page the question is about, by its origin, when it asks about one:
    /// what leaves goes from there.
    #[serde(default)]
    pub page: Option<String>,
    /// The filled page as a picture, by its content address, when the host
    /// kept one: what the person sees before they allow it.
    #[serde(default)]
    pub picture: Option<String>,
    /// What the agent asks in its own words, where it asks something rather
    /// than to allow an action: the action fields are empty then.
    #[serde(default)]
    pub ask: Option<AgentAsk>,
    /// Whether a lasting grant is offered for this action.
    #[serde(default)]
    pub allow_always: bool,
}

/// The Allow always button's position among the approval answers.
pub const ALLOW_ALWAYS: usize = 1;

/// A question an agent words itself, as the sheet asks it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct AgentAsk {
    /// The question, in the agent's words.
    pub question: String,
    /// The answers the person picks from, in order.
    #[serde(default)]
    pub choices: Vec<String>,
    /// Whether the person may type an answer of their own.
    #[serde(default)]
    pub typed: bool,
}

impl AgentAsk {
    /// The sheet asking it: each choice a button of its own where nothing is
    /// typed, else a field to type in under the choices, and Send. Not now
    /// leaves it waiting, unanswered; Decline tells the agent no.
    fn dialog(&self, handle: &str, asked: &AgentQuestion) -> Dialog {
        let body = if self.typed {
            Body::Entry {
                masked: false,
                required: self.choices.is_empty(),
                choices: self.choices.clone(),
            }
        } else {
            Body::Answers {
                choices: self.choices.clone(),
                suggested: usize::MAX,
                remember: None,
            }
        };
        Dialog {
            handle: handle.to_owned(),
            title: words("agent-ask-title", &[("agent", &asked.agent)]),
            heading: asked.title.clone(),
            detail: self.question.clone(),
            facts: vec![(word("agent-ask-workspace"), asked.workspace.clone())],
            body,
            confirm_label: word("agent-ask-send"),
            cancel_label: word("agent-ask-decline"),
            defer_label: Some(word("agent-ask-not-now")),
            // Nothing is picked until the person picks it.
            selected: usize::MAX,
            ..Dialog::default()
        }
    }

    /// The answer the sheet gives: the choice `chosen` names, else what was
    /// `typed`, where it takes typing. `None` while it holds neither.
    #[must_use]
    pub fn reply(&self, chosen: Option<usize>, typed: &str) -> Option<String> {
        let typed = typed.trim();
        if self.typed && !typed.is_empty() {
            return Some(typed.to_owned());
        }
        chosen.and_then(|at| self.choices.get(at)).cloned()
    }
}

impl AgentQuestion {
    /// The sheet asking it, as request `handle`. The
    /// title says who wants to do what; the group, the record, the arguments
    /// and what it risks follow, and the picture of the page it leaves from.
    #[must_use]
    pub fn dialog(&self, handle: &str) -> Dialog {
        if let Some(ask) = &self.ask {
            return ask.dialog(handle, self);
        }
        let mut facts = self.arguments.clone();
        if let Some(page) = self.page.as_ref().filter(|page| !page.is_empty()) {
            facts.insert(0, (word("network-agent-question-page"), page.clone()));
        }
        Dialog {
            handle: handle.to_owned(),
            title: words(
                "network-agent-question-title",
                &[("agent", &self.agent), ("action", &self.action)],
            ),
            heading: self.title.clone(),
            detail: words(
                "network-agent-question-detail",
                &[("resource", &self.resource), ("workspace", &self.workspace)],
            ),
            facts,
            caution: self.consequence(),
            picture: self.picture.clone(),
            body: if self.allow_always {
                Body::Answers {
                    choices: vec![
                        word("network-agent-question-allow"),
                        word("portal-allow-always"),
                    ],
                    suggested: 0,
                    remember: None,
                }
            } else {
                Body::Review
            },
            confirm_label: word("network-agent-question-allow"),
            cancel_label: word("portal-deny"),
            ..Dialog::default()
        }
    }

    /// What allowing it risks, as the action declares it: acting outside
    /// Tilo, revealing private data, or what Undo cannot take back. `None`
    /// for an action Undo takes back that risks nothing more.
    fn consequence(&self) -> Option<String> {
        let key = match (self.kind.as_str(), self.risk.as_str()) {
            ("outside", _) if self.undoable => "outside",
            ("outside", _) => "outside-final",
            (_, "sensitive") => "sensitive",
            (_, "privileged") => "privileged",
            (_, "destructive") => "destructive",
            (kind, "none" | "low" | "") => {
                if self.undoable || kind != "undoable" {
                    return None;
                }
                "final"
            }
            _ => "unknown",
        };
        Some(word(&format!("network-agent-question-{key}")))
    }
}

/// What an action risks, in the person's words: an unknown name reads as
/// the gravest.
#[must_use]
pub fn risk_words(name: &str) -> String {
    match name {
        "none" | "low" | "destructive" | "privileged" | "sensitive" => {
            word(&format!("network-agent-risk-{name}"))
        }
        _ => word("network-agent-risk-something"),
    }
}
