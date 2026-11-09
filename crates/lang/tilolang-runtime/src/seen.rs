//! Seen marks (task-legibility.md, §2 and §4): per resource, the newest version of it a person
//! has seen. What `unseen(r)` reads, and what the host draws marks from.
//!
//! A mark stays until the person deals with it, never on a timer, and showing a resource does
//! not set it: the person does, by reviewing the run that made it (*Mark all seen*), by opening
//! the resource in a window of its own, by editing it (their own version is the newest they
//! have seen of it), or by undoing what made it.
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
        person: String,
        resource: String,
        mark: Option<SeenMark>,
    },
    /// `person` reviewed the run `run` (its group): every version it made is seen.
    Run { person: String, run: String },
}

/// The slot holding `person`'s seen mark of `resource`.
#[must_use]
pub fn seen_key(person: &str, resource: &str) -> StateKey {
    StateKey::runtime(SEEN_WINDOW, Some(person.to_owned()), resource)
}

/// What a catch-up took away, so that it can be given back: the marks `person` had of the
/// resources they marked seen in one move (`Runtime::mark_all_seen`), as they had them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenUndo {
    pub person: String,
    /// Each resource, and the mark it had before (`None` where it had none).
    pub before: Vec<(String, Option<SeenMark>)>,
}

/// The slot holding the runs `person` reviewed: the state no resource id names.
#[must_use]
pub fn runs_key(person: &str) -> StateKey {
    seen_key(person, "$runs")
}

/// Whether `version` is one the host marks for `person`: made by someone else, a person or an
/// agent. A change of their own is never marked: they made it and so have seen it.
#[must_use]
pub fn is_marked(version: &Version, person: &str) -> bool {
    version.who != person
}

/// Which of `versions` (oldest first) `person` has not seen, oldest first: those after their
/// mark and after their own newest version, which are marked ([`is_marked`]) and not of a run
/// they reviewed (`runs`).
#[must_use]
pub fn unseen(
    versions: &[Version],
    person: &str,
    mark: Option<&SeenMark>,
    runs: &[String],
) -> Vec<Version> {
    let mut from = 0;
    if let Some(mark) = mark {
        from = match versions.iter().rposition(|v| v.change == mark.change) {
            Some(at) => at.saturating_add(1),
            None => versions
                .iter()
                .rposition(|v| v.at <= mark.at)
                .map_or(0, |at| at.saturating_add(1)),
        };
    }
    if let Some(own) = versions.iter().rposition(|v| v.who == person) {
        from = from.max(own.saturating_add(1));
    }
    versions
        .get(from..)
        .unwrap_or_default()
        .iter()
        .filter(|v| is_marked(v, person))
        .filter(|v| !v.run.as_ref().is_some_and(|run| runs.contains(&run.id)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Run, VersionKind};

    fn version(who: &str, at: &str, change: &str, run: bool) -> Version {
        Version {
            who: who.to_owned(),
            at: at.to_owned(),
            action: "a".to_owned(),
            run: run.then(|| Run {
                id: "t1@1".to_owned(),
                title: "Tidy".to_owned(),
            }),
            kind: VersionKind::Changed,
            fields: Vec::new(),
            resource: "d".to_owned(),
            change: change.to_owned(),
            op: 0,
            count: 1,
            left: None,
            placed: Vec::new(),
            from: None,
            suggested_by: None,
        }
    }

    #[test]
    fn versions_after_the_mark_and_the_persons_own_are_unseen() {
        let versions = [
            version("ada", "2026-10-30T09:00:00Z", "c1", false),
            version("agent:a", "2026-10-30T09:01:00Z", "c2", true),
            version("ben", "2026-10-30T09:02:00Z", "c3", false),
            version("agent:a", "2026-10-30T09:03:00Z", "c4", true),
        ];
        let changes = |list: Vec<Version>| list.into_iter().map(|v| v.change).collect::<Vec<_>>();
        // Nothing seen: everyone else's, the agent's and Ben's alike.
        assert_eq!(
            changes(unseen(&versions, "ada", None, &[])),
            ["c2", "c3", "c4"]
        );
        // Seen up to c2.
        let mark = SeenMark::of(&versions[1]);
        assert_eq!(
            changes(unseen(&versions, "ada", Some(&mark), &[])),
            ["c3", "c4"]
        );
        // Ben edited after c2: what he saw is seen for him, what came after is not.
        assert_eq!(changes(unseen(&versions, "ben", None, &[])), ["c4"]);
        // A mark whose change the log no longer names stands by its time.
        let gone = SeenMark {
            change: "compacted".to_owned(),
            at: "2026-10-30T09:03:00Z".to_owned(),
        };
        assert!(unseen(&versions, "ada", Some(&gone), &[]).is_empty());
        // A reviewed run's versions are seen, whatever the marks: what Ben changed after it
        // stands, as it was no step of the run's.
        assert_eq!(
            changes(unseen(&versions, "ada", None, &["t1@1".to_owned()])),
            ["c3"]
        );
    }

    #[test]
    fn a_change_is_marked_by_who_made_it_whatever_the_type() {
        // The same type all three: a person's change is marked as an agent's, and nobody's
        // own change is.
        let versions = [
            version("ada", "2026-10-30T09:00:00Z", "c1", false),
            version("ben", "2026-10-30T09:02:00Z", "c3", false),
            version("agent:a", "2026-10-30T09:03:00Z", "c4", true),
        ];
        let changes = |list: Vec<Version>| list.into_iter().map(|v| v.change).collect::<Vec<_>>();
        assert_eq!(changes(unseen(&versions, "ada", None, &[])), ["c3", "c4"]);
        // Still not one's own: Ben's own newest change bounds what he has seen, so the
        // agent's later change is all he has not.
        assert_eq!(changes(unseen(&versions, "ben", None, &[])), ["c4"]);
        assert!(is_marked(&versions[1], "ada"));
        assert!(is_marked(&versions[2], "ada"));
        assert!(!is_marked(&versions[0], "ada"));
    }

    #[test]
    fn a_record_made_from_another_counts_as_a_change_to_it() {
        let mut comment = version("ben", "2026-10-30T09:02:00Z", "c3", false);
        comment.resource = "comment".to_owned();
        comment.from = Some(crate::value::ResourceRef::parse_id("task", "Task").unwrap());
        // Ben said it about the task: Ada has not seen the task change, nor what he said.
        let changes = |list: Vec<Version>| list.into_iter().map(|v| v.change).collect::<Vec<_>>();
        assert_eq!(
            changes(unseen(&[comment.clone()], "ada", None, &[])),
            ["c3"]
        );
        // Not something Ben has to read back: it was his own change.
        assert!(unseen(&[comment], "ben", None, &[]).is_empty());
    }
}
