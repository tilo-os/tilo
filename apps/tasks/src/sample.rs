//! What the capture scenarios show: a few issues of a small launch. Never
//! written into a real workspace.
//!
//! The issues are made through the tracker's own actions — a [`Replica`]
//! applies each one through the same door a person's does — and then staged
//! as the record's start, since a capture cannot wait for a handful of
//! changes, and the issues it shows must be the person's own for "Mine" and
//! its count to read as they do.

use ui::prelude::*;
use ui::tilo_resource::NonBlank;

use crate::model::{Status, Tracker, tracker};
use crate::{Filter, Nav, Place};

pub(crate) const SCENARIOS: &[&str] = &[
    "issues",
    "board",
    "issue",
    "mine",
    "thread",
    "todo",
    "migrated",
    "migrated-issue",
];

/// One issue of the scene: its title, where it stands, whose it is, and its
/// label.
struct Sample {
    title: &'static str,
    status: Status,
    /// `true` when it is the person's own.
    mine: bool,
    /// The workspace's agent.
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
        let Some(title) = NonBlank::parse(issue.title) else {
            continue;
        };
        replica = replica.as_author(person);
        replica.set_now_ms(at);
        if replica
            .apply(Tracker::CREATE.with(crate::model::NewIssue {
                title,
                parent: None,
                body: Some(format!("## Why\n\n{} before the launch.", issue.title)),
                status: Some(issue.status),
                assignee,
                url: None,
                project: None,
                priority: None,
                estimate: None,
                due_ms: None,
                labels: None,
                origin: None,
            }))
            .is_err()
        {
            continue;
        }
        // Some of the tasks have been talked about, so the thread view has
        // something to read and the person's own voice is in it.
        if matches!(issue.status, Status::InProgress | Status::InReview)
            && let Some(id) = made(&replica, issue.title)
            && let Some(text) = NonBlank::parse(&format!(
                "Started on {}: the first draft is in the folder.",
                issue.title
            ))
        {
            at = at.saturating_add(1_000);
            replica = replica.as_author(person);
            replica.set_now_ms(at);
            replica
                .apply(Tracker::COMMENT.with(tracker::Comment {
                    issue: id.clone(),
                    text,
                    origin: None,
                }))
                .ok();
        }
        let Some(label) = issue.label.and_then(NonBlank::parse) else {
            continue;
        };
        let Some(id) = made(&replica, issue.title) else {
            continue;
        };
        at = at.saturating_add(1_000);
        replica.set_now_ms(at);
        let _ = replica.apply(Tracker::ADD_LABEL.with(tracker::AddLabel { issue: id, label }));
    }
    replica.state().unwrap_or_default()
}

/// The tracker a migration from Linear leaves: issues added through the import inbox, as
/// opening Tasks after the migration adds them.
pub(crate) fn migrated(person: &str) -> Tracker {
    let page = |identifier: &str| format!("https://tracker.example/acme/issue/{identifier}");
    let issue = |identifier: &str, title: &str, status: &str, extra: serde_json::Value| {
        let mut entry = serde_json::json!({
            "version": 1, "source": "linear", "key": identifier, "identifier": identifier,
            "title": title, "body": format!("Moved from Linear.\n\n{title}, as the team wrote it."),
            "status": status, "labels": [], "assignee": null, "comments": [], "links": [],
            "parent": null, "url": page(identifier)
        });
        if let (Some(entry), serde_json::Value::Object(extra)) = (entry.as_object_mut(), extra) {
            entry.extend(extra);
        }
        entry
    };
    let entries = [
        issue(
            "ENG-1",
            "Ship the migration",
            "in-progress",
            serde_json::json!({
                "assignee": crate::model::person_assignee(person).0,
                "labels": ["migration", "launch"],
                "comments": [
                    {"author": "Grace", "at": "2026-11-02T09:00:00Z", "text": "The review page lists every collection now."},
                    {"author": "Ada", "at": "2026-11-03T10:30:00Z", "text": "Resuming after a stop works; next is mail."}
                ],
                "links": [page("ENG-3")]
            }),
        ),
        issue(
            "ENG-2",
            "Write the review page",
            "todo",
            serde_json::json!({"parent": page("ENG-1"), "labels": ["migration"]}),
        ),
        issue(
            "ENG-3",
            "Remove the old importer",
            "done",
            serde_json::json!({}),
        ),
        issue(
            "ENG-4",
            "Mail account setup",
            "backlog",
            serde_json::json!({"labels": ["mail"]}),
        ),
        issue(
            "OPS-1",
            "Renew certificates",
            "cancelled",
            serde_json::json!({}),
        ),
    ];
    let mut replica = Replica::<Tracker>::new();
    let mut doc = replica.doc_mut().clone();
    if crate::import_inbox::apply(&mut doc, &entries, person).is_err() {
        return Tracker::default();
    }
    <Tracker as ui::tilo_resource::Resource>::read(&doc).unwrap_or_default()
}

/// The issue `title` names, as the replica has it now.
fn made(replica: &Replica<Tracker>, title: &str) -> Option<Id> {
    replica.state().ok().and_then(|state| {
        state
            .issues
            .iter()
            .find(|(_, shown)| shown.title == title)
            .map(|(id, _)| id.clone())
    })
}

/// The view state a scenario shows.
pub(crate) fn stage(scenario: Option<&str>, nav: &Nav, tracker: &Tracker) {
    match scenario {
        Some("board") => nav.place.set(Place::Board),
        Some("mine") => {
            nav.place.set(Place::List);
            nav.filter.set(Some(Filter::Mine.key()));
        }
        Some("issue" | "migrated-issue") => {
            nav.place.set(Place::List);
            if let Some((id, _)) = tracker.by_number(1) {
                nav.open(id);
            }
        }
        Some("thread") => nav.place.set(Place::Thread),
        Some("todo") => {
            nav.place.set(Place::List);
            nav.filter.set(Some(Filter::Todo.key()));
        }
        _ => nav.place.set(Place::List),
    }
}
