//! Run journals and suggestions.
//!
//! An invocation that runs other actions — an action that calls another app's action, an
//! agent's program — is a run: a journal of the ordered results of every action it invoked and
//! every model call and read it made. A suspended run is just a journal on disk; resuming runs
//! the code again from the start with the journal answering everything that already happened.
//! That is sound because replay decides every effect: the clock, reads and draws are answered
//! from the journal too.

use serde::{Deserialize, Serialize};

use crate::ids::{ActionId, ModuleHash, Principal, RunId};
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
    /// The authority or the person refused; the run saw an error value.
    Denied { action: ActionId, reason: String },
    /// The action trapped and rolled back; the run saw an error value.
    Failed { action: ActionId, reason: String },
    /// A model call and the answer it got.
    Model { kind: String, result: Value },
    /// A value the run read from a resource. Replay answers reads from the journal, so a
    /// replayed run sees what it saw the first time and takes the same branches, even after
    /// the actions it approved have changed the resources.
    Read { value: Value },
    /// The clock and the person's zone, read once per invocation.
    Clock(Clock),
    /// A draw: `value` was one of `0..upto`.
    Random { upto: i64, value: i64 },
    /// A request to a service outside Tilo, and the answer it got.
    Web {
        request: WebRequest,
        response: WebResponse,
    },
    /// A write the run made, and the item it made, when it made one. A run that waited is run
    /// again after the change it made before the call is already in the record: replay answers
    /// the write from the journal and makes it no more, so it neither doubles what it wrote nor
    /// takes back what the call it made wrote on top of it.
    Wrote { item: Option<String> },
    /// A person was told something (`notify`), and the run carries on: a replay tells them no
    /// more than the first run did.
    Notified,
}

impl JournalEntry {
    /// Whether the entry records something the run read of the world outside Tilo, which a
    /// change carries beside the action so a person can see what it fetched, drew and when it
    /// ran. Reads of resources are not outside effects: they are the run's own working.
    #[must_use]
    pub fn is_outside_effect(&self) -> bool {
        matches!(
            self,
            Self::Clock(_)
                | Self::Random { .. }
                | Self::Web { .. }
                | Self::Model { .. }
                | Self::Notified
        )
    }
}

/// The action a run is in the middle of running: it suspended before the call could be made,
/// or before an answer it made the call for was given.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub action: ActionId,
    pub args: Vec<Value>,
    /// The module that made the call, so the run goes on deciding it as that module's: a call
    /// from another package is not the run's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<ModuleHash>,
    /// The resource the call is made from, when it is made from one: what the resources the
    /// action makes were made from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<crate::value::ResourceRef>,
    /// Who runs the call, when a scenario named them with `invoke(..., by:)`: the callee runs as
    /// that person rather than as the run's own invoker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub who: Option<Principal>,
    /// The moment the call acts at, when the same `invoke` named one: the callee reads `now` as
    /// that moment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// Whether the call is seed data's: a scenario's own code is the host's making, so the
    /// action it calls runs as a scenario's does, without asking a person first.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub seed: bool,
}

/// What a suspended run waits for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Pending {
    /// A person's answer to an action the run invoked.
    Ask(Suggestion),
    /// The action the run invoked next: it runs from the journal, and the run goes on from its
    /// result.
    Step(Step),
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
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    /// The action the run runs, or `None` for a run that is only a suggestion waiting for an
    /// answer before the action starts.
    // Journals written before the action/run split spell this key `flow`; it stays the stored
    // spelling so a saved run still resumes.
    #[serde(rename = "flow", default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionId>,
    pub args: Vec<Value>,
    pub invoker: Principal,
    pub chain: Vec<ModuleHash>,
    /// The resource the run was invoked from, when it was made from one: what the resources its
    /// actions create were made from, kept across a suspension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<crate::value::ResourceRef>,
    /// The moment a scenario's `invoke(..., at:)` made this run act at: what its clock reads,
    /// so the call it is happens at the time the scenario says and a resume still does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// Who proposed the run, when the person running it accepted a suggestion: recorded in the
    /// changes its actions make, so history says who suggested it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_by: Option<String>,
    pub entries: Vec<JournalEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<Pending>,
    pub finished: bool,
    /// Who approved what the run does, when a person did: recorded in the changes its actions
    /// make.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<Principal>,
    /// How many outside effects the run has queued: the next one's key counts from it, so a run
    /// that goes on after a wait queues none under a key it used.
    #[serde(default)]
    pub effects: u32,
    /// The run this one is a step of, when it was invoked from another run: its result goes
    /// back there and it goes on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<RunId>,
    /// The invocation every change this run makes belongs to: the run at the top of the calls
    /// it made, carried down to every action it calls, so a run's changes are one action of
    /// the workspace and fail together. A run that waits for a person goes on as a new one
    /// ([`crate::exec`]).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub invocation: String,
}

/// How a run ended for now.
#[derive(Clone, Debug, PartialEq)]
pub enum RunOutcome {
    Done(Value),
    Suspended(Suggestion),
}

/// A person's answer to a suggestion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Approve,
    Decline,
}
