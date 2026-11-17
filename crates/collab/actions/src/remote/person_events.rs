//! What a person has the host do for them at a time or when something
//! happens: their scheduled work and their events.
//!
//! An event is a Shortcut the host runs for one person, with no device of
//! theirs on: a reminder at a moment, a summary every morning, a look at
//! every tenth thing that arrives. It names what starts it (its trigger),
//! optionally a question the person's own model answers about what happened
//! (only on a yes does it go on) and a count (only every so many times), and
//! the instruction its person's assistant is then handed. It keeps a note
//! between its runs: what it is for, how its person wants it done, and what
//! the last run left for the next.
//!
//! The person sets one up, or their assistant does it for them: these
//! actions are the same for both, and are answered only for the person the
//! caller is or acts for, in a workspace that is theirs. Nobody else's
//! agent reaches another person's events. Setting an event up gives nothing
//! new reach: the work it starts is the assistant's own, which still asks
//! before anything leaves the workspace, unless the event carries its
//! person's permission for exactly that action ([`PERMIT`]), which only the
//! person gives and takes back.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ActionAddress, RemoteActionTarget};

/// The provider that answers a person's events under.
pub const PROVIDER: &str = "tilo.events";
/// The resource the events are actions of.
pub const RESOURCE: &str = "events";
/// The kind of that resource.
pub const KIND: &str = "events.list";
/// Every event of the person in the workspace, as an [`Events`].
pub const LIST: &str = "list";
/// Set an event up ([`SetUp`]), answering it as an [`EventView`].
pub const SET_UP: &str = "set-up";
/// Change an event ([`Change`]), answering it as an [`EventView`].
pub const CHANGE: &str = "change";
/// Pause an event ([`Named`]): nothing that comes while it is paused is
/// answered, also not later.
pub const PAUSE: &str = "pause";
/// Let a paused event go on ([`Named`]).
pub const RESUME: &str = "resume";
/// Remove an event ([`Named`]), answering the events that remain.
pub const REMOVE: &str = "remove";
/// Let the work an event starts take one action that leaves the workspace
/// without asking ([`Permission`]). The person's alone: an agent is refused.
pub const PERMIT: &str = "permit";
/// Take such a permission back ([`Permission`]). The person's alone.
pub const WITHDRAW: &str = "withdraw-permission";

/// The target of action `action` of [`RESOURCE`].
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// An event to set up.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetUp {
    /// What its person reads it by.
    pub name: String,
    /// What starts it, as the line a Shortcut opens with: `once at
    /// 2026-11-17T11:32:00Z`, `every day at 09:00`, `every Monday, Thursday
    /// at 17:30`, `every 3600 seconds`, or a change in the workspace.
    pub when: String,
    /// A question the person's own model answers with yes or no about what
    /// happened; the event goes on only on a yes. Left out, it always does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only_if: Option<String>,
    /// Go on only every so many times: 10 for every tenth. Left out, every
    /// time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<u64>,
    /// What the person's assistant is told to do when it goes on.
    pub instruction: String,
    /// What the event is for, in its person's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// How what it writes should read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// What its person cares about and how the work is done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
}

/// A change to an event. What is left out stays as it is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Change {
    /// The event's id.
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its trigger, as [`SetUp::when`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Its filter, as [`SetUp::only_if`]; empty takes the filter away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only_if: Option<String>,
    /// Its count, as [`SetUp::every`]; 1 goes on every time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    /// The note's parts, each taking the place of what was there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
    /// An answer the person accepted, to keep as an example.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted: Option<String>,
    /// An answer the person rejected, to keep as an example.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected: Option<String>,
    /// What this run keeps for the next: what it saw last, where it
    /// stopped. It takes the place of what the last run kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept: Option<Value>,
}

/// An event, named by its id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Named {
    pub event: String,
}

/// A permission of an event: one action that leaves the workspace, which
/// the work the event starts may take without asking.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Permission {
    pub event: String,
    /// A grant rule that allows exactly one action.
    pub rule: crate::policy::Rule,
}

/// Whether an event is on a clock or waits for something to happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sort {
    /// It fires at a time: once, or again and again.
    Scheduled,
    /// It fires when something happens.
    Event,
}

/// How an event's last run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ended {
    Ran,
    /// What it handed over waits for its person's answer.
    Waits,
    Refused,
    Failed,
}

/// An event's last run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LastRun {
    /// When it ended, ms since the Unix epoch.
    pub at_ms: i64,
    pub ended: Ended,
    /// Why it was refused or failed, in words for its person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// One event, as [`LIST`] and every action on it answer it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventView {
    pub id: String,
    pub name: String,
    pub sort: Sort,
    /// Its trigger in words: the line a Shortcut opens with.
    pub when: String,
    /// The next time it fires, for one on a clock that has one, ms since
    /// the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_at_ms: Option<i64>,
    /// Its filter: the question the person's model answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only_if: Option<String>,
    /// It goes on every so many times.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<u64>,
    /// Where its count stands.
    pub count: u64,
    /// What the person's assistant is told.
    pub instruction: String,
    /// Its note, every part present: `instruction`, `style`, `guidance`,
    /// `accepted`, `rejected` and `kept`.
    pub note: Value,
    /// Whether it is on; false while paused.
    pub on: bool,
    /// The actions that leave the workspace it may take without asking.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permits: Vec<crate::policy::Rule>,
    /// How many times it ran.
    pub runs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<LastRun>,
    /// When it was set up, ms since the Unix epoch.
    pub set_up_at_ms: i64,
}

/// Every event of a person in a workspace, in the order they were set up.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Events {
    pub events: Vec<EventView>,
}
