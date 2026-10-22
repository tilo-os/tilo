//! Private issues: an issue kept to the people and agents named.
//!
//! A read decision reaches a record, never part of one: everyone who reads
//! the tracker reads every issue in it. So a private issue lives in a record
//! of its own, a tracker of its own holding just that issue: a child record
//! of the workspace's tracker ([`ChildRecord`]), listed there by its id alone
//! (its kind, and whether it is in the trash), and kept by the room to its
//! readers with one grant (`roles::readers_grant`). Nobody else's device is
//! given its key, and every door denies it to them, an agent included unless
//! it is named itself, whoever it acts for.
//!
//! Making an issue private is the tracker's `take` on its `private`
//! container, naming the readers: the room keeps the issue's record to them
//! and the person making it private, then the issue moves whole into it, its
//! runs aside, and leaves the tracker in the same step. It keeps its id, so
//! its address ([`super::issue_resource`]) and every reference to it stay.
//! What the tracker held before stays in its history.

use ui::permissions::policy::Subject;
use ui::record::RecordId;
use ui::tilo_resource::{Edit, EditOp, Id, Model as _, Segment, children::Children};

use super::{Issue, Tracker};

impl ui::framework::ChildRecord for Tracker {
    type Parent = Self;
    const KEPT_TO_READERS: bool = true;
    const TAKE_LABEL: Option<&'static str> = Some("Make private");

    fn container(parent: &Self) -> &Children {
        &parent.private
    }

    fn container_path() -> Vec<Segment> {
        vec![Segment::Field("private".into())]
    }

    /// The issue, everything it holds but its run, into a tracker of its
    /// own, and out of this one.
    fn split(tracker: &Self, issue: &Id) -> Result<(Vec<Edit<Self>>, Vec<Edit<Self>>), String> {
        let held = tracker
            .issues
            .get(issue)
            .ok_or("This issue no longer exists")?;
        if held.active_run().is_some() {
            return Err("Stop the run before making the issue private".into());
        }
        if tracker.children(Some(issue)).next().is_some() {
            return Err(
                "Make its sub-issues private or move them out first: they would be left without it"
                    .into(),
            );
        }
        // A run is approved in the private tracker, by someone who reads
        // it, or not at all.
        let kept = Issue {
            run: None,
            ..held.clone()
        };
        let issues = || vec![Segment::Field("issues".into())];
        Ok((
            vec![Edit::new(
                issues(),
                EditOp::InsertItem {
                    id: Some(issue.clone()),
                    at: 0,
                    value: kept.to_json(),
                },
            )],
            vec![Edit::new(
                issues(),
                EditOp::RemoveItem { id: issue.clone() },
            )],
        ))
    }
}

/// The record private issue `issue` is kept in, or `None` when its id
/// cannot name a record.
#[must_use]
pub fn private_record(issue: &Id) -> Option<RecordId> {
    <Tracker as ui::framework::ChildRecord>::parent_record().child(issue.as_str())
}

/// Who a reader is, from how an assignee names them: `person:<id>` or
/// `agent:<id>`.
#[must_use]
pub fn reader(name: &str) -> Option<Subject> {
    let name = name.trim();
    if let Some(person) = name.strip_prefix("person:") {
        return (!person.is_empty()).then(|| Subject::person(person));
    }
    let agent = name.strip_prefix("agent:")?;
    (!agent.is_empty()).then(|| Subject::agent(agent))
}

/// The readers a list names, each `person:<id>` or `agent:<id>`, separated
/// by commas or spaces; `None` when one is neither.
#[must_use]
pub fn readers(list: &str) -> Option<Vec<Subject>> {
    list.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|name| !name.is_empty())
        .map(reader)
        .collect()
}

#[cfg(test)]
#[path = "private_tests.rs"]
mod tests;
