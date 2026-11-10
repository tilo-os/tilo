//! An agent's question on a workspace host: what it wants to do, on what and
//! where, and the person's answer for that one invocation. A native session
//! and a page show it on the same sheet, in the same words.

use serde::{Deserialize, Serialize};

use super::{Body, Dialog};
use crate::{word, words};

/// An agent's question, as the sheet shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct AgentQuestion {
    /// The agent, as the host names it.
    pub agent: String,
    /// What it wants to do.
    pub action: String,
    /// What it acts on.
    pub resource: String,
    /// What the host says the action risks.
    pub risk: String,
    /// The workspace it acts in.
    pub workspace: String,
}

impl AgentQuestion {
    /// The sheet asking it, as request `handle`: Allow once, or Deny.
    #[must_use]
    pub fn dialog(&self, handle: &str) -> Dialog {
        let mut detail = words(
            "network-agent-question-detail",
            &[("resource", &self.resource), ("workspace", &self.workspace)],
        );
        if !self.risk.is_empty() {
            detail.push('\n');
            detail.push_str(&words(
                "network-agent-question-risk",
                &[("risk", &risk_words(&self.risk))],
            ));
        }
        Dialog {
            handle: handle.to_owned(),
            title: words(
                "network-agent-question-title",
                &[("agent", &self.agent), ("action", &self.action)],
            ),
            // The title names the agent already.
            heading: String::new(),
            detail,
            body: Body::Review,
            confirm_label: word("network-agent-question-allow"),
            cancel_label: word("portal-deny"),
            ..Dialog::default()
        }
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
