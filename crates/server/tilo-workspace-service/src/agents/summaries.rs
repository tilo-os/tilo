//! A summary an agent keeps of a conversation longer than a turn is shown, so what it said
//! before the window goes on reaching the turns after it.
//!
//! A turn is told only the newest part of the conversation its ask is in
//! ([`crate::asks::around`], [`super::asked`]). Where more precedes the ask than that, the turn is
//! offered one tool beside the agent tools, `keep-summary`: it keeps, on the conversation it is
//! made from, what came before, as a record of the agent package (`so.tilo.agent`) keyed by the
//! conversation and the agent. The next turn there is told that summary before the window
//! ([`Turn::summary`]), so the summary and the window together carry the conversation.
//!
//! The summary is the model's: the server knows where it is keyed and what it reaches, and says
//! neither a word of what it says nor that it said it to a log.

use serde_json::{Value, json};
use tilo_action_policy::MemberId;

use super::{Ask, ToolDefinition, Turn};
use crate::asks::{self, ResourceRef};
use crate::workspaces::Workspaces;

/// The tool that keeps a summary of what a conversation said before the window.
const KEEP_SUMMARY: &str = "keep-summary";

/// The most a summary may say, in bytes: every later turn there is told it.
const MAX_SUMMARY: usize = 8 * 1024;

const NO_TEXT: &str = "a summary says something: give `text`";

/// What a turn that was shown less of its conversation than came before it keeps that with:
/// which conversation the summary is keyed by, and the item it reaches.
#[derive(Clone)]
pub(super) struct Summarizing {
    workspaces: Workspaces,
    workspace: String,
    agent: MemberId,
    conversation: ResourceRef,
    through: String,
    through_at: String,
}

impl Summarizing {
    /// What `turn` keeps a summary of its conversation with, where more precedes its ask than
    /// the turn is told; `None` for a turn shown the whole rest of it, and for any other.
    pub(super) fn of(workspaces: &Workspaces, workspace: &str, turn: &Turn) -> Option<Self> {
        let summarize = turn.summarize.as_ref()?;
        if !matches!(&turn.ask, Ask::Asked { .. }) {
            return None;
        }
        Some(Self {
            workspaces: workspaces.clone(),
            workspace: workspace.to_owned(),
            agent: MemberId::agent(turn.agent.id.0.clone()),
            conversation: summarize.conversation.clone(),
            through: summarize.through.clone(),
            through_at: summarize.through_at.clone(),
        })
    }

    /// Keep what the model says with tool `input`, as this turn's agent.
    async fn keep(&self, input: &Value) -> Result<String, String> {
        let text = input
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or(NO_TEXT)?;
        let text = within(text, MAX_SUMMARY);
        let packages = super::asked::packages()?;
        let (agent, conversation) = (self.agent.clone(), self.conversation.clone());
        let (reaches, wrote_at) = (self.through.clone(), self.through_at.clone());
        self.workspaces
            .with_tl_record(
                &self.workspace,
                &packages,
                super::asked::viewer(),
                move |opened| {
                    asks::keep_summary(opened, &agent, &conversation, (&text, &reaches, &wrote_at))
                },
            )
            .await?;
        Ok("Kept.".to_owned())
    }
}

/// `words` up to `most` bytes, on a character boundary, without trailing space.
fn within(words: &str, most: usize) -> String {
    if words.len() <= most {
        return words.to_owned();
    }
    let mut kept = String::new();
    for c in words.chars() {
        if kept.len().saturating_add(c.len_utf8()) > most {
            break;
        }
        kept.push(c);
    }
    kept.trim_end().to_owned()
}

/// The tool a turn that was shown less of its conversation than came before it keeps that with,
/// where it is shown that much; none for any other turn.
pub(super) fn tools(turn: &Turn) -> Vec<ToolDefinition> {
    if turn.summarize.is_none() {
        return Vec::new();
    }
    vec![ToolDefinition {
        name: KEEP_SUMMARY.to_owned(),
        description: "Keep a summary of what this conversation said before the part you are \
                      shown, so the turns after this one know it too: you are shown only its \
                      newest part. Say in a few sentences what the earlier part said that \
                      someone answering later needs. What you kept before is what it says now."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The summary, in a few sentences",
                },
            },
            "required": ["text"],
        }),
    }]
}

/// What tool `name` of a turn answers when it is the summary tool, as its model reads it, and
/// whether it failed; `None` for every other tool. A turn shown the whole of its conversation is
/// refused it.
pub(super) async fn call(
    summarizing: Option<&Summarizing>,
    name: &str,
    input: &Value,
) -> Option<(String, bool)> {
    if name != KEEP_SUMMARY {
        return None;
    }
    let Some(summarizing) = summarizing else {
        return Some((
            "Nothing is summarized here: you are shown the whole of this conversation.".to_owned(),
            true,
        ));
    };
    let (summarizing, input) = (summarizing.clone(), input.clone());
    Some(match summarizing.keep(&input).await {
        Ok(said) => (said, false),
        Err(refused) => (refused, true),
    })
}

/// What a turn that may keep a summary of what came before its window is told about doing so,
/// after what every agent on a server is told; nothing for any other turn.
pub(super) fn told_how(turn: &Turn) -> &'static str {
    if turn.summarize.is_none() {
        return "";
    }
    " You are shown only the newest part of this conversation; what came before is in the \
     summary you kept, where you kept one. Before you answer, keep that summary current with \
     `keep-summary`: in a few sentences, what the earlier part said that later turns need."
}
