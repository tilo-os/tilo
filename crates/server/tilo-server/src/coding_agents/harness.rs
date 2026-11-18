//! What a coding harness is to its host: how it is started, how it takes a
//! message, what its output means and which protocol its model speaks.
//!
//! Everything else about running one is the same for all of them: the run in
//! the workspace's machine, its endpoints, the person's model behind them,
//! the conversation its words go to. So which coding agent runs is a
//! parameter ([`Harnesses::named`]), and each one's own ways live in one
//! file that implements [`Harness`] ([`super::claude_code`] is the first).

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::workspaces::person_model::Interaction;

/// The endpoint a harness asks its model at.
pub const MODEL: &str = "model";
/// The endpoint a harness reaches Tilo's own MCP server at.
pub const TILO: &str = "tilo";

/// The protocol a harness speaks to its model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelProtocol {
    /// Anthropic's Messages.
    AnthropicMessages,
}

/// What a harness is started for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Start {
    /// Its own id for a conversation it had before, to take it up again;
    /// none starts a new one.
    pub session: Option<String>,
    /// How its person wants it to work, beside its own instructions.
    pub instruction: Option<String>,
}

/// A harness's program, ready for the machine: its endpoints stand in its
/// arguments and variables as placeholders
/// ([`linux_vm_protocol::served::placeholder`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    /// The endpoints it asks its host at.
    pub endpoints: Vec<String>,
}

/// One thing a harness said, as its host reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Says {
    /// Its own id for this conversation: kept, so a later run takes it up.
    Session(String),
    /// Words for its person: a message of its own.
    Words(String),
    /// What it is doing now, in a few words: a line, not a message.
    Doing(String),
    /// It finished what it was asked: `ok`, or why not in `words`. It stays
    /// ready for the next message.
    Finished { ok: bool, words: String },
}

/// Reads what one run of a harness prints, line by line, in order.
pub trait Reads: Send {
    /// What the harness said with `line` of its output; nothing for a line
    /// that is not its protocol's.
    fn line(&mut self, line: &str) -> Vec<Says>;
}

/// One coding harness's own ways.
pub trait Harness: Send + Sync {
    /// Its id in a record: `claude-code`.
    fn id(&self) -> &'static str;
    /// Its name as a person reads it.
    fn name(&self) -> &'static str;
    /// The protocol its model endpoint speaks.
    fn model_protocol(&self) -> ModelProtocol;
    /// Its program for `start`.
    fn program(&self, start: &Start) -> Program;
    /// `text` as one message on its input: its own way of taking a message
    /// while it works or between two pieces of work.
    fn message(&self, text: &str) -> Vec<u8>;
    /// A reader for one run's output.
    fn reads(&self) -> Box<dyn Reads>;
    /// Which kind of its person's work a request for the model it names
    /// `model` is: what it marks as a small, quick model is a quick
    /// judgment, everything else a part of a task.
    fn kind_of(&self, model: &str) -> Interaction;
}

/// The coding harnesses a host can start, by id.
#[derive(Clone, Default)]
pub struct Harnesses {
    known: BTreeMap<&'static str, Arc<dyn Harness>>,
}

impl Harnesses {
    /// The harnesses every host knows.
    #[must_use]
    pub fn known() -> Self {
        Self::default().with(Arc::new(super::claude_code::ClaudeCode))
    }

    /// These, and `harness`.
    #[must_use]
    pub fn with(mut self, harness: Arc<dyn Harness>) -> Self {
        self.known.insert(harness.id(), harness);
        self
    }

    /// The harness with `id`; the first known when `id` is empty.
    #[must_use]
    pub fn named(&self, id: &str) -> Option<Arc<dyn Harness>> {
        if id.is_empty() {
            return self.known.get(super::claude_code::ID).cloned();
        }
        self.known.get(id).cloned()
    }

    /// Every harness's id, for the words of a refusal.
    #[must_use]
    pub fn ids(&self) -> Vec<&'static str> {
        self.known.keys().copied().collect()
    }
}
