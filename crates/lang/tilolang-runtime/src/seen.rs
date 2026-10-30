//! Seen marks (task-legibility.md, §2 and §4): per resource, the newest version of it a person
//! has seen. What `unseen(r)` reads, and what the host draws marks from.
//!
//! A mark stays until the person deals with it, never on a timer, and showing a resource does
//! not set it: the person does, by reviewing the run that made it (*Mark all seen*), by opening
//! the resource in a window of its own, by editing it (their own version is the newest they
//! have seen of it), or by undoing what made it.
//!
//! The marks live in the person's own record, kept by the host: the runtime holds them as
//! state slots ([`seen_key`]), which the host fills ([`crate::Runtime::set_seen`]) and which a
//! view that read one runs again on; what the person marks here is queued for the host to keep
//! ([`crate::Runtime::take_seen`]).
//!
//! Which versions are marked at all: every version made in a task run or by an agent, and none
//! the person made themselves. People's changes are not marked (§4, the default).

use serde::{Deserialize, Serialize};

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

/// A seen mark the person set, for the host to keep in their own record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenWrite {
    pub person: String,
    /// The resource, as [`crate::value::ResourceRef::id_string`] writes it.
    pub resource: String,
    pub mark: SeenMark,
}

/// The slot holding `person`'s seen mark of `resource`.
#[must_use]
pub fn seen_key(person: &str, resource: &str) -> StateKey {
    StateKey {
        window: SEEN_WINDOW.to_owned(),
        principal: Some(person.to_owned()),
        state: resource.to_owned(),
    }
}

/// Whether `version` is one the host marks for `person`: made in a task run or by an agent,
/// and not by `person`.
#[must_use]
pub fn is_marked(version: &Version, person: &str) -> bool {
    version.who != person && (version.task.is_some() || version.who.starts_with("agent:"))
}

/// Which of `versions` (oldest first) `person` has not seen, oldest first: those after their
/// mark and after their own newest version, which are marked ([`is_marked`]).
#[must_use]
pub fn unseen(versions: &[Version], person: &str, mark: Option<&SeenMark>) -> Vec<Version> {
    let mut from = 0;
    if let Some(mark) = mark {
        from = match versions.iter().rposition(|v| v.change == mark.change) {
            Some(at) => at + 1,
            None => versions
                .iter()
                .rposition(|v| v.at <= mark.at)
                .map_or(0, |at| at + 1),
        };
    }
    if let Some(own) = versions.iter().rposition(|v| v.who == person) {
        from = from.max(own + 1);
    }
    versions
        .get(from..)
        .unwrap_or_default()
        .iter()
        .filter(|v| is_marked(v, person))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{TaskRun, VersionKind};

    fn version(who: &str, at: &str, change: &str, task: bool) -> Version {
        Version {
            who: who.to_owned(),
            at: at.to_owned(),
            action: "a".to_owned(),
            task: task.then(|| TaskRun {
                id: "t1@1".to_owned(),
                title: "Tidy".to_owned(),
            }),
            kind: VersionKind::Changed,
            fields: Vec::new(),
            resource: "d".to_owned(),
            change: change.to_owned(),
        }
    }

    #[test]
    fn a_runs_versions_after_the_mark_and_the_persons_own_are_unseen() {
        let versions = [
            version("ada", "2026-10-30T09:00:00Z", "c1", false),
            version("agent:a", "2026-10-30T09:01:00Z", "c2", true),
            version("ben", "2026-10-30T09:02:00Z", "c3", false),
            version("agent:a", "2026-10-30T09:03:00Z", "c4", true),
        ];
        let changes = |list: Vec<Version>| list.into_iter().map(|v| v.change).collect::<Vec<_>>();
        // Nothing seen: both of the agent's; Ben's change is a person's, not marked.
        assert_eq!(changes(unseen(&versions, "ada", None)), ["c2", "c4"]);
        // Seen up to c2.
        let mark = SeenMark::of(&versions[1]);
        assert_eq!(changes(unseen(&versions, "ada", Some(&mark))), ["c4"]);
        // Ben edited after c2: what he saw is seen for him, what came after is not.
        assert_eq!(changes(unseen(&versions, "ben", None)), ["c4"]);
        // A mark whose change the log no longer names stands by its time.
        let gone = SeenMark {
            change: "compacted".to_owned(),
            at: "2026-10-30T09:03:00Z".to_owned(),
        };
        assert!(unseen(&versions, "ada", Some(&gone)).is_empty());
    }
}
