//! Seen marks (task-legibility.md, §2 and §4): per resource, the newest version of it a person
//! has seen. What `unseen(r)` reads, and what the host draws marks from.
//!
//! A mark stays until the person deals with it, never on a timer, and showing a resource does
//! not set it: the person does, by reviewing the run that made it (*Mark all seen*), by opening
//! the resource in a window of its own, by editing it (their own version is the newest they
//! have seen of it), by undoing what made it, or by watching the change land where their window
//! shows it, its live mark on screen ([`crate::Runtime::seen_live`]).
//!
//! The marks live under the person's entries of the workspace's record of them, kept by the
//! host: the runtime holds them as state slots ([`seen_key`]), which the host fills
//! ([`crate::Runtime::set_seen`]) and which a view that read one runs again on; what the person
//! marks here is queued for the host to keep
//! ([`crate::Runtime::take_seen`]).
//!
//! Which versions are marked at all: every version someone else made, a person's or an
//! agent's, and none the person made themselves. A type declares nothing about it, and the
//! view chooses whether to show the marks it reads ([`unseen`], the host's row, header and
//! field marks).

use serde::{Deserialize, Serialize};

use crate::ids::Actor;
use crate::store::Version;
use crate::view_state::StateKey;

/// The window the slots of seen marks are kept under.
pub const SEEN_WINDOW: &str = "$seen";

/// The newest version of a resource a person has seen: the change that made it, and when.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeenMark {
    /// The change, as [`Version::change`] names it.
    pub change: String,
    /// When it was made, as [`Version::at`]: where a mark whose change the log no longer
    /// names stands among the versions.
    pub at: String,
}

impl SeenMark {
    /// The mark of having seen `version`.
    #[must_use]
    pub fn of(version: &Version) -> Self {
        Self {
            change: version.change.clone(),
            at: version.at.clone(),
        }
    }
}

/// What the person marked seen, for the host to keep under their entries of the workspace's
/// record: the system's one `mark seen` action, of resources or of a whole run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeenWrite {
    /// `person` has seen `resource` (as [`crate::value::ResourceRef::id_string`] writes it) up
    /// to `mark`; `None` takes a mark back (an undone catch-up).
    Resource {
        person: Actor,
        resource: String,
        mark: Option<SeenMark>,
    },
    /// `person` reviewed the run `run` (its group): every version it made is seen.
    Run { person: Actor, run: String },
}

/// The slot holding `person`'s seen mark of `resource`.
#[must_use]
pub fn seen_key(person: &Actor, resource: &str) -> StateKey {
    StateKey::runtime(SEEN_WINDOW, Some(person.clone()), resource)
}

/// What a catch-up took away, so that it can be given back: the marks `person` had of the
/// resources they marked seen in one move (`Runtime::mark_all_seen`), as they had them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenUndo {
    pub person: Actor,
    /// Each resource, and the mark it had before (`None` where it had none).
    pub before: Vec<(String, Option<SeenMark>)>,
}

/// The slot holding the runs `person` reviewed: the state no resource id names.
#[must_use]
pub fn runs_key(person: &Actor) -> StateKey {
    seen_key(person, "$runs")
}

/// Whether `version` is one the host marks for `person`: made by someone else, a person or an
/// agent. A change of their own is never marked: they made it and so have seen it.
#[must_use]
pub fn is_marked(version: &Version, person: &Actor) -> bool {
    // A resource's making is not a change of anyone's: it came to be, and nothing is caught up
    // on by its arriving.
    version.action != crate::store::CREATE_ACTION && version.who.as_ref() != Some(person)
}

/// Which of `versions` (oldest first) `person` has not seen, oldest first: those after their
/// mark and after their own newest version, which are marked ([`is_marked`]) and not of a run
/// they reviewed (`runs`).
#[must_use]
pub fn unseen(
    versions: &[Version],
    person: &Actor,
    mark: Option<&SeenMark>,
    runs: &[String],
) -> Vec<Version> {
    let mut from = mark.map_or(0, |mark| after_mark(versions, mark));
    if let Some(own) = versions
        .iter()
        .rposition(|v| v.who.as_ref() == Some(person))
    {
        from = from.max(own.saturating_add(1));
    }
    unseen_after(versions.get(from..).unwrap_or_default(), person, runs)
}

/// Which of `versions` (oldest first, all of them after the person's mark) `person` has not
/// seen, oldest first: what [`unseen`] reads, for a page of a record's history read from the
/// mark on rather than the whole of it.
#[must_use]
pub fn unseen_after(versions: &[Version], person: &Actor, runs: &[String]) -> Vec<Version> {
    let from = versions
        .iter()
        .rposition(|v| v.who.as_ref() == Some(person))
        .map_or(0, |own| own.saturating_add(1));
    versions
        .get(from..)
        .unwrap_or_default()
        .iter()
        .filter(|v| is_marked(v, person))
        .filter(|v| !v.run.as_ref().is_some_and(|run| runs.contains(&run.id)))
        .cloned()
        .collect()
}

/// Where in `versions` (oldest first, by [`Version::order`]) the versions after the one `mark`
/// names start: after the change it names, or, when the record no longer holds that change,
/// after the last version made at or before its time. Counting what follows costs the log no
/// copy, so a catch-up reads a page of a record's history without the whole of it.
#[must_use]
pub fn after_mark(versions: &[Version], mark: &SeenMark) -> usize {
    let made_by = versions.partition_point(|v| v.at.as_str() <= mark.at.as_str());
    let same_moment = versions
        .get(..made_by)
        .unwrap_or_default()
        .partition_point(|v| v.at.as_str() < mark.at.as_str());
    versions
        .get(same_moment..made_by)
        .unwrap_or_default()
        .iter()
        .position(|v| v.change == mark.change)
        .map_or(made_by, |at| {
            same_moment.saturating_add(at).saturating_add(1)
        })
}
