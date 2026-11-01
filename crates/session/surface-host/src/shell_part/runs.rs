//! The runs of the session's agent as the desk's reading names them: each run, the entries of
//! its conversation and what each program in it printed or changed (shell.md §8.1).

use serde::{Deserialize, Serialize};

/// A run of what the person asked the session's agents, as the desk's reading names it
/// (`session.run`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunReading {
    pub key: String,
    pub title: String,
    pub agent: String,
    pub phase: String,
    pub active: bool,
    pub live: String,
}

/// One entry of a run's conversation, as the desk's reading names it (`session.run_entry`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEntryReading {
    pub key: String,
    pub run: String,
    pub seq: i64,
    pub author: String,
    pub by_agent: bool,
    pub step: bool,
    pub text: String,
    pub outcome: String,
    pub answer: String,
}

/// A resource a program of a run printed or changed, as the desk's reading names it
/// (`session.entry_ref`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryRefReading {
    pub key: String,
    pub entry: String,
    pub changed: bool,
    pub resource: String,
    pub app_id: String,
    pub type_kind: String,
    pub label: String,
    pub change: String,
}
