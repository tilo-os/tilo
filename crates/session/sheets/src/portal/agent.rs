//! An agent's question on a workspace host: who asks, in which run, exactly
//! what would happen and where, and the person's answer for that one
//! invocation. A native session and a page show it on the same sheet, in the
//! same words.

use serde::{Deserialize, Serialize};

use super::{Body, Dialog};
use crate::{word, words};

/// An agent's question, as the sheet shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct AgentQuestion {
    /// The agent, by the name it goes by in History.
    pub agent: String,
    /// The run it asks in, by the title it gave it; empty when it named none.
    #[serde(default)]
    pub run: String,
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
}

impl AgentQuestion {
    /// The sheet asking it, as request `handle`: Allow once, or Deny. The
    /// title says who wants to do what; the run, the record, the arguments
    /// and what it risks follow.
    #[must_use]
    pub fn dialog(&self, handle: &str) -> Dialog {
        Dialog {
            handle: handle.to_owned(),
            title: words(
                "network-agent-question-title",
                &[
                    ("agent", &self.agent),
                    ("action", &self.action),
                    ("doing", &in_sentence(&self.action)),
                ],
            ),
            heading: self.run.clone(),
            detail: words(
                "network-agent-question-detail",
                &[("resource", &self.resource), ("workspace", &self.workspace)],
            ),
            facts: self.arguments.clone(),
            caution: self.consequence(),
            body: Body::Review,
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

/// An action's title as it reads inside a sentence: its first letter in
/// lower case, unless the first word is an initialism (`PDF`).
fn in_sentence(title: &str) -> String {
    let mut chars = title.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    if chars.clone().next().is_some_and(char::is_uppercase) {
        return title.to_owned();
    }
    first.to_lowercase().chain(chars).collect()
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
