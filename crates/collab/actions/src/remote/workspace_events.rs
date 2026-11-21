//! What a workspace has its host do at a time or when something happens: its
//! scheduled work and its events.
//!
//! An event is a Shortcut the host runs with no device of anyone's on: a
//! reminder at a moment, a summary every morning, a look at every tenth
//! thing that arrives. It names what starts it (its trigger), optionally a
//! question a model answers about what happened (only on a yes does it go
//! on) and a count (only every so many times), and the instruction it hands
//! on. It keeps a note between its runs: what it is for, how it is wanted,
//! and what the last run left for the next. It may say how its result is
//! told ([`SetUp::told_as`]): as a brief, which is told with a notice, or as
//! news, which stays in the list.
//!
//! These are the workspace's, not one person's: every member whose role
//! reaches them sees and changes the same events, and a member's assistant
//! works with them on that member's behalf. Who set an event up is kept as
//! its author ([`EventView::by`]), and the instruction it hands over starts
//! that author's assistant's own work, which still asks before anything
//! leaves the workspace, unless the event carries that person's permission
//! for exactly that action (`PERMIT`), which only the person gives and
//! takes back.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::person_telling::Kind;
use super::{ActionAddress, RemoteActionTarget};
use crate::MemberId;

/// The provider that answers a workspace's events under.
pub const PROVIDER: &str = "tilo.events";
/// The resource the events are actions of.
pub const RESOURCE: &str = "events";
/// The kind of that resource.
pub const KIND: &str = "events.list";
/// Every event of the workspace, as an [`Events`].
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
    /// How its result is told to its person: `reminder`, `brief` or `news`,
    /// the kinds a thing told has ([`Kind`]) but a question. Left out, a
    /// moment is a reminder and everything else is news.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub told_as: Option<String>,
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
    /// How its result is told, as [`SetUp::told_as`]; empty takes the word
    /// away, and it is told as an event that says nothing is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub told_as: Option<String>,
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
    /// How its result is told, where its person or their assistant said.
    /// Left out, a moment is told as a reminder and everything else as
    /// news.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub told_as: Option<Kind>,
    /// Its note, every part present: `instruction`, `style`, `guidance`,
    /// `accepted`, `rejected` and `kept`.
    pub note: Value,
    /// Whether it is on; false while paused.
    pub on: bool,
    /// How many times it ran.
    pub runs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<LastRun>,
    /// When it was set up, ms since the Unix epoch.
    pub set_up_at_ms: i64,
    /// Who set it up: kept as its author. The instruction it hands over
    /// starts that member's assistant's work.
    pub by: MemberId,
}

/// Every event of a workspace, in the order they were set up.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Events {
    pub events: Vec<EventView>,
}
