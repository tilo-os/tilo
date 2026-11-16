//! Every run of a workspace a host keeps, newest first: what a key that
//! reaches the workspace reads to see what ran there, what waits for the
//! person, and what can still be taken back.
//!
//! A device lists a workspace's agent sessions from its own history index
//! ([`super::workspace_activity`]). A host that keeps a workspace for people
//! and their keys, with no device of theirs, answers from what it holds:
//!
//! - **The records.** Every change an action makes is kept in its record's
//!   history with the undo step it is part of, who made it and when
//!   ([`super::changes`]). A run is its step: one call, or every call of a
//!   run its client named. What the records say of it holds across the
//!   host's restarts.
//! - **The calls the host remembers.** The host keeps the invocations it
//!   was asked for in memory, each with its id, who asked, and how it
//!   stands: queued, waiting for the person, running, ended. A run that
//!   changed no record (one refused, one still waiting) is known from them
//!   alone, and only until the host restarts or lets go of it.
//!
//! [`target`] asks for one page ([`RunsQuery`]); the host answers it at once,
//! running nothing, with [`Runs`].

use serde::{Deserialize, Serialize};

use super::RemoteActionTarget;
use crate::api::ActionAddress;

/// The provider a host answers its workspace's runs under.
pub const PROVIDER: &str = "tilo.runs";
/// The resource the listing is an action of: the workspace.
pub const RESOURCE: &str = "workspace";
/// List the workspace's runs, newest first.
pub const LIST: &str = "list";

/// How many runs a page holds when the query asks for no number.
pub const PAGE: usize = 50;
/// The most runs one page holds.
pub const MOST: usize = 200;

/// What [`LIST`] is invoked on.
#[must_use]
pub fn target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, LIST),
    }
}

/// Whether `target` asks for the listing.
#[must_use]
pub fn is_listing(target: &RemoteActionTarget) -> bool {
    target.provider == PROVIDER
        && target.address.resource == RESOURCE
        && target.address.action == LIST
}

/// One page of a workspace's runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunsQuery {
    /// The newest this many after `cursor`; `0` for [`PAGE`], never more
    /// than [`MOST`].
    pub limit: usize,
    /// Where the page before ended ([`Runs::next_cursor`]); none for the
    /// newest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

impl RunsQuery {
    /// How many runs the page holds.
    #[must_use]
    pub fn page(&self) -> usize {
        if self.limit == 0 {
            PAGE
        } else {
            self.limit.min(MOST)
        }
    }
}

/// Who ran a run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunBy {
    /// The name it goes by: a key's, as the person named it, an agent's, or
    /// a person's.
    pub name: String,
    /// `agent` (every key, and an agent a person's device handed its session
    /// to), `person`, `bot` or `automation`.
    pub kind: String,
}

/// How a run stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// It asked the person, and waits for their answer.
    Waiting,
    /// Admitted, and not ended yet.
    Running,
    /// It ended well.
    #[default]
    Ended,
    /// It ended failing, or the person did not allow it.
    Failed,
    /// It was stopped.
    Cancelled,
    /// Everything it changed was taken back.
    Undone,
}

/// A record a run changed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunRecord {
    /// The app, as a path names it (`word-processor`).
    pub app: String,
    /// The app's id (`so.tilo.word-processor`).
    pub app_id: String,
    /// The record's id there; empty for the app's own record of the
    /// workspace.
    pub record: String,
}

/// One run of a workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Run {
    /// What reading, stopping and taking it back name it by: the invocation
    /// id of its newest call (taking back one call of a run takes back the
    /// whole run). `None` for a run known only from the records, whose calls
    /// the host no longer remembers (it restarted since).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Every call of it the host remembers, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<String>,
    /// The undo step its changes are kept under in every record's history:
    /// what tells it apart there, and here across the host's restarts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    pub by: RunBy,
    /// What its client called it, when it named it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: RunStatus,
    /// Why it failed, typed (`confirmation_required`, ...), never the
    /// failure's words, which may hold what it was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    /// When it started, in milliseconds since the Unix epoch.
    pub started_ms: i64,
    /// When it ended; `None` while it has not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<i64>,
    /// What its last call asked for, `<app>#<action>`, while the host
    /// remembers it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// What its newest change did, as History says it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// How many changes it made to the workspace's records: its edits, and
    /// what it brought in from outside (what a command printed).
    pub changes: usize,
    /// How many of its edits were taken back since; what came from outside
    /// is no edit, and no undo takes it back.
    pub undone: usize,
    /// The records it changed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<RunRecord>,
    /// Its edits still stand and nobody's edit stands on them: taking it back
    /// would take back exactly its own.
    pub undoable: bool,
    /// Why it is not taken back, when someone's change stands on its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo_blocked: Option<String>,
    /// The records keep it: it is listed after the host restarts.
    pub recorded: bool,
    /// The host remembers its calls: `id` reads, stops and takes it back.
    pub remembered: bool,
}

/// A page of a workspace's runs, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Runs {
    pub runs: Vec<Run>,
    /// Runs the workspace holds, of which [`Self::runs`] is a page.
    pub total: usize,
    /// Of those, the runs waiting for the person.
    pub waiting: usize,
    /// Some record could not be read yet: runs that changed only it are
    /// missing until it can.
    pub pending: bool,
    /// Passed back as [`RunsQuery::cursor`] for the next page; `None` on the
    /// last.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_bounded_and_the_listing_named_by_its_target() {
        assert_eq!(RunsQuery::default().page(), PAGE);
        assert_eq!(
            RunsQuery {
                limit: 3,
                cursor: None
            }
            .page(),
            3
        );
        assert_eq!(
            RunsQuery {
                limit: 10_000,
                cursor: None
            }
            .page(),
            MOST
        );
        assert!(is_listing(&target()));
        let mut other = target();
        other.address.action = "undo".into();
        assert!(!is_listing(&other));
        let answer = serde_json::to_value(Run {
            status: RunStatus::Waiting,
            ..Run::default()
        })
        .unwrap();
        assert_eq!(answer["status"], "waiting");
        assert!(answer.get("id").is_none(), "{answer}");
    }
}
