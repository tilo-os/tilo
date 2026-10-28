//! Flow journals and suggestions (implementation design 4.5).
//!
//! A flow run is a journal: the ordered results of every `invoke` and `model_call`. A
//! suspended run is just a journal on disk; resuming re-runs the flow from the start with the
//! journal answering every call that already happened.

use serde::{Deserialize, Serialize};

use crate::ids::{ActionId, FlowId, ModuleHash, Principal, RunId};
use crate::value::Value;

/// An action applied to arguments, waiting for a person's decision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    pub run: RunId,
    pub action: ActionId,
    pub args: Vec<Value>,
    pub invoker: Principal,
    /// The authorship chain that led to the invocation, outermost first.
    pub chain: Vec<ModuleHash>,
}

/// One journaled step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum JournalEntry {
    /// The action ran; this is its result.
    Invoked { action: ActionId, result: Value },
    /// The authority or the person refused; the flow saw an error value.
    Denied { action: ActionId, reason: String },
    /// The action trapped and rolled back; the flow saw an error value.
    Failed { action: ActionId, reason: String },
    /// A model call and the answer it got.
    Model { kind: String, result: Value },
    /// A value the flow read from a resource. Replay answers reads from the journal, so a
    /// replayed flow sees what it saw the first time and takes the same branches, even after
    /// the actions it approved have changed the resources.
    Read { value: Value },
}

/// Everything needed to replay a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    /// The flow being run, or `None` for a standalone suggestion of one action.
    pub flow: Option<FlowId>,
    pub args: Vec<Value>,
    pub invoker: Principal,
    pub chain: Vec<ModuleHash>,
    pub entries: Vec<JournalEntry>,
    pub pending: Option<Suggestion>,
    pub finished: bool,
}

/// How a run ended for now.
#[derive(Clone, Debug, PartialEq)]
pub enum FlowOutcome {
    Done(Value),
    Suspended(Suggestion),
}

/// A person's answer to a suggestion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Approve,
    Decline,
}
