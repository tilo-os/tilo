//! Ordered, replay-safe presentation events from an agent's live model transport.
//!
//! These events describe output, never executable actions. A retry must use a new
//! reply message and run id; replaying this stream cannot dispatch a tool.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStreamEvent {
    pub run: String,
    /// Starts at one; duplicates and gaps are refused rather than corrupting text.
    pub sequence: u64,
    pub event: AgentOutput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentOutput {
    TextDelta {
        text: String,
    },
    /// Only reasoning explicitly exposed by the provider. Never a synthesized
    /// explanation or a request for private chain-of-thought.
    ReasoningDelta {
        text: String,
    },
    Progress {
        label: String,
    },
    Finished {
        outcome: AgentFinish,
        detail: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentFinish {
    Complete,
    Truncated,
    Refused,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStream {
    pub run: String,
    pub sequence: u64,
    #[serde(default)]
    pub reasoning: String,
    #[serde(default)]
    pub progress: String,
    pub finish: Option<AgentFinish>,
    #[serde(default)]
    pub detail: String,
}

#[cfg(test)]
mod limits_tests;
#[cfg(test)]
mod tests;

impl AgentStream {
    pub fn start(run: &str) -> Self {
        Self {
            run: run.into(),
            sequence: 0,
            reasoning: String::new(),
            progress: String::new(),
            finish: None,
            detail: String::new(),
        }
    }

    /// Validate before touching the document. A completed run is immutable and
    /// a new attempt may not reuse a reply containing prior output or actions.
    pub fn accepts(&self, event: &AgentStreamEvent) -> bool {
        self.finish.is_none()
            && self.run == event.run
            && !self.run.is_empty()
            && self.run.len() <= 256
            && self.sequence.checked_add(1) == Some(event.sequence)
            && match &event.event {
                AgentOutput::TextDelta { text } => text.len() <= 64 * 1024,
                AgentOutput::ReasoningDelta { text } => {
                    self.reasoning.len() + text.len() <= 256 * 1024
                }
                AgentOutput::Progress { label } => label.len() <= 4096,
                AgentOutput::Finished { detail, .. } => detail.len() <= 16 * 1024,
            }
    }

    pub fn apply(&mut self, event: &AgentStreamEvent) {
        self.sequence = event.sequence;
        match &event.event {
            AgentOutput::TextDelta { .. } => {}
            AgentOutput::ReasoningDelta { text } => self.reasoning.push_str(text),
            AgentOutput::Progress { label } => self.progress.clone_from(label),
            AgentOutput::Finished { outcome, detail } => {
                self.finish = Some(*outcome);
                self.detail.clone_from(detail);
            }
        }
    }

    pub fn status_key(&self) -> &'static str {
        match self.finish {
            None => "chat-agent-working",
            Some(AgentFinish::Complete) => "chat-agent-complete",
            Some(AgentFinish::Truncated) => "chat-agent-truncated",
            Some(AgentFinish::Refused) => "chat-agent-refused",
            Some(AgentFinish::Cancelled) => "chat-agent-cancelled",
            Some(AgentFinish::Failed) => "chat-agent-failed",
        }
    }
}
