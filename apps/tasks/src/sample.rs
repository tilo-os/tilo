//! What the capture scenarios show: a few issues of a small launch. Never
//! written into a real workspace.
//!
//! The issues are made through the tracker's own actions — a [`Replica`]
//! applies each one through the same door a person's does — and then staged
//! as the record's start, since a capture cannot wait for a handful of
//! changes, and the issues it shows must be the person's own for "Mine" and
//! its count to read as they do.

use ui::prelude::*;

use crate::model::{Status, Tracker, tracker};
use crate::{Filter, Nav, Place};

pub(crate) const SCENARIOS: &[&str] = &["issues", "board", "issue", "mine"];

/// One issue of the scene: its title, where it stands, whose it is, and its
/// label.
struct Sample {
    title: &'static str,
    status: Status,
    /// `true` when it is the person's own.
    mine: bool,
    /// The room's agent.
    agent: bool,
    label: Option<&'static str>,
}

const ISSUES: [Sample; 6] = [
    Sample {
        title: "Write the launch notes",
        status: Status::InProgress,
        mine: true,
        agent: false,
        label: Some("launch"),
    },
    Sample {
        title: "Record the two-minute demo",
        status: Status::Todo,
        mine: true,
        agent: false,
        label: Some("launch"),
    },
    Sample {
        title: "Tidy the onboarding docs",
        status: Status::Todo,
        mine: false,
        agent: true,
        label: None,
    },
    Sample {
        title: "Pick the venue",
        status: Status::Done,
        mine: false,
        agent: false,
        label: Some("venue"),
    },
    Sample {
        title: "Answer the press questions",
        status: Status::Backlog,
        mine: false,
        agent: false,
        label: None,
    },
    Sample {
        title: "Check the slides in dark mode",
        status: Status::InReview,
        mine: true,
        agent: false,
        label: Some("venue"),
    },
];

/// The tracker a scenario shows, its issues the person's own changes.
pub(crate) fn tracker(person: &str) -> Tracker {
    let mut replica = Replica::<Tracker>::new();
    let mut at = ui::time::unix_now_ms_i64().saturating_sub(60_000);
    for issue in &ISSUES {
        at = at.saturating_add(1_000);
        let assignee = if issue.mine {
            Some(crate::model::person_assignee(person).0)
        } else if issue.agent {
            Some(crate::model::agent_assignee("claude").0)
        } else {
            None
        };
        replica = replica.as_author(person);
        replica.set_now_ms(at);
        if replica
            .apply(Tracker::CREATE.with(tracker::Create {
                title: issue.title.to_owned(),
                parent: None,
                body: Some(format!("## Why\n\n{} before the launch.", issue.title)),
                status: Some(issue.status),
                assignee,
            }))
            .is_err()
        {
            continue;
        }
        let Some(label) = issue.label else {
            continue;
        };
        let made = replica.state().ok().and_then(|state| {
            state
                .issues
                .iter()
                .find(|(_, shown)| shown.title == issue.title)
                .map(|(id, _)| id.clone())
        });
        if let Some(id) = made {
            at = at.saturating_add(1_000);
            replica.set_now_ms(at);
            let _ = replica.apply(Tracker::ADD_LABEL.with(tracker::AddLabel {
                issue: id,
                label: label.to_owned(),
            }));
        }
    }
    replica.state().unwrap_or_default()
}

/// The view state a scenario shows.
pub(crate) fn stage(scenario: Option<&str>, nav: &Nav, tracker: &Tracker) {
    match scenario {
        Some("board") => nav.place.set(Place::Board),
        Some("mine") => {
            nav.place.set(Place::List);
            nav.filter.set(Some(Filter::Mine.key()));
        }
        Some("issue") => {
            nav.place.set(Place::List);
            if let Some((id, _)) = tracker.by_number(1) {
                nav.open(id);
            }
        }
        _ => nav.place.set(Place::List),
    }
}
