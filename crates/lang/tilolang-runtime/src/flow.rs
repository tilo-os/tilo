//! Flow journals and suggestions.
//!
//! A flow run is a journal: the ordered results of every `invoke` and `model_call`. A
//! suspended run is just a journal on disk; resuming re-runs the flow from the start with the
//! journal answering every call that already happened. That is sound because a flow is
//! deterministic: no clock, no randomness, and every read goes through the host and is
//! journaled too.

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
    /// The clock and the person's zone, read once per invocation (convergence.md, "Actions").
    Clock(Clock),
    /// A draw: `value` was one of `0..upto`.
    Random { upto: i64, value: i64 },
    /// A request to a service outside Tilo, and the answer it got.
    Web {
        request: WebRequest,
        response: WebResponse,
    },
}

/// The clock and the person's zone, as an invocation read them: what it pinned at its first
/// read, and what a replay answers from instead of asking the host again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clock {
    /// The moment, ISO-8601 UTC text with the seconds (`2026-06-30T13:40:00Z`).
    pub now: String,
    /// The person's zone as its IANA name, when the host named one this build knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
    /// How many seconds their clock is ahead of UTC, which is what reads a day where the host
    /// named no zone.
    #[serde(default)]
    pub offset: i64,
}

/// A request to a service outside Tilo, as it was made.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WebRequest {
    /// `GET` or `POST`.
    pub method: String,
    pub url: String,
    /// The headers asked for, each `Name: value`, as `web.get` and `web.post` write them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<String>,
    /// A `POST`'s body, as the text the language gave.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The tag a `GET` sent as `If-None-Match`, so an unchanged answer comes back as a `304`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_none_match: Option<String>,
}

/// What the service answered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WebResponse {
    /// The status the service answered with; 0 when none answered (a timeout, no network, a
    /// refused host), and `status_text` then says why.
    pub status: u16,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status_text: String,
    /// The headers it sent, each `Name: value`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<String>,
    /// The body: a JSON string when it read as text, `{"base64": "..."}` when it did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
}

impl WebResponse {
    /// Whether the service answered success (`200..=299`).
    #[must_use]
    pub fn ok(&self) -> bool {
        (200..=299).contains(&self.status)
    }

    /// The body as bytes.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        match &self.body {
            Some(serde_json::Value::String(text)) => text.as_bytes().to_vec(),
            Some(serde_json::Value::Object(map)) => map
                .get("base64")
                .and_then(serde_json::Value::as_str)
                .map(|text| {
                    use base64::Engine as _;
                    base64::engine::general_purpose::STANDARD
                        .decode(text)
                        .unwrap_or_default()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}

/// Everything needed to replay a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    /// The flow being run, or `None` for a standalone suggestion of one action.
    pub flow: Option<FlowId>,
    pub args: Vec<Value>,
    pub invoker: Principal,
    pub chain: Vec<ModuleHash>,
    /// The resource the run was invoked from, when it was made from one: what the resources its
    /// actions create were made from, kept across a suspension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<crate::value::ResourceRef>,
    /// Who proposed the run, when the person running it accepted a suggestion: recorded in the
    /// changes its actions make, so history says who suggested it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_by: Option<String>,
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
