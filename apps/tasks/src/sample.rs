//! What the capture scenarios show: a few issues of a small launch. Never
//! written into a real workspace.
//!
//! The issues are made through the parts' own actions, carried out through
//! the window's chain as the person's, as the window opens: the issues it
//! shows must be the person's own for "Mine" and its count to read as they
//! do.

use ui::framework::Chain;
use ui::prelude::*;
use ui::tilo_resource::NonBlank;

use crate::issue_list::{IssueList, issue_list};
use crate::model::Status;
use crate::{Filter, Issues, Nav, Place, Tasks};

/// What a capture's tracker holds as its window opens, staged by the
/// scenario's fixture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scene {
    /// A small launch's issues.
    Launch,
    /// What a migration from Linear leaves.
    Migrated,
    /// A long tracker of this many issues, for measuring one
    /// (`issues-2500`).
    Long(u64),
}

impl Scene {
    /// The long tracker a scenario named `issues-N` asks for.
    pub(crate) fn long(scenario: &str) -> Option<Self> {
        scenario
            .strip_prefix("issues-")?
            .parse()
            .ok()
            .map(Self::Long)
    }
}

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

/// Fill an empty tracker's issues with `scene`, as `person`'s changes.
pub(crate) fn fill(scene: Scene, chain: &Chain<IssueList>, person: &str) {
    if !chain.parts().is_empty() {
        return;
    }
    match scene {
        Scene::Launch => launch(chain, person),
        Scene::Migrated => migrated(chain, person),
        Scene::Long(count) => long(chain, count),
    }
}

/// `count` issues: every tenth a parent, the nine after it its sub-issues,
/// every seventh one done.
fn long(chain: &Chain<IssueList>, count: u64) {
    let mut parent: Option<Id> = None;
    for number in 1..=count {
        let title = format!("Issue number {number} of a long list");
        let Some(named) = NonBlank::parse(&title) else {
            continue;
        };
        let first = number % 10 == 1;
        let status = if number % 7 == 0 {
            Status::Done
        } else {
            Status::Todo
        };
        let Ok(Some(part)) = chain.invoke(&IssueList::CREATE.with(crate::model::NewIssue {
            status: Some(status),
            parent: if first {
                None
            } else {
                parent.as_ref().map(ui::tilo_resource::items::Ref::from)
            },
            ..crate::model::NewIssue::titled(named)
        })) else {
            continue;
        };
        if first {
            // Found in the part it was written into, not across them all.
            parent = chain
                .parts()
                .iter()
                .find(|(index, _)| *index == part)
                .and_then(|(_, kept)| {
                    kept.issues
                        .iter()
                        .find(|(_, issue)| issue.title == title)
                        .map(|(id, _)| id.clone())
                });
        }
    }
}

/// The issue titled `title`, as the chain holds it now.
fn made(chain: &Chain<IssueList>, title: &str) -> Option<Id> {
    let parts = chain.parts();
    Issues::of(&parts)
        .iter()
        .find(|(_, shown)| shown.title == title)
        .map(|(id, _)| id.clone())
}

/// A small launch's issues, the person's own changes.
fn launch(chain: &Chain<IssueList>, person: &str) {
    for issue in &ISSUES {
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
        if chain
            .invoke(&IssueList::CREATE.with(crate::model::NewIssue {
                body: Some(format!("## Why\n\n{} before the launch.", issue.title)),
                status: Some(issue.status),
                assignee,
                ..crate::model::NewIssue::titled(title)
            }))
            .is_err()
        {
            continue;
        }
        // Some of the tasks have been talked about, so the thread view has
        // something to read and the person's own voice is in it.
        if matches!(issue.status, Status::InProgress | Status::InReview)
            && let Some(id) = made(chain, issue.title)
            && let Some(text) = NonBlank::parse(&format!(
                "Started on {}: the first draft is in the folder.",
                issue.title
            ))
        {
            let _ = chain.invoke(&IssueList::COMMENT.with(issue_list::Comment {
                issue: id,
                text,
                origin: None,
            }));
        }
        let (Some(label), Some(id)) = (
            issue.label.and_then(NonBlank::parse),
            made(chain, issue.title),
        ) else {
            continue;
        };
        let _ = chain.invoke(&IssueList::ADD_LABEL.with(issue_list::AddLabel { issue: id, label }));
    }
}

/// The issues a migration from Linear leaves: added through the import inbox, as opening
/// Tasks after the migration adds them.
fn migrated(chain: &Chain<IssueList>, person: &str) {
    let page = |identifier: &str| format!("https://tracker.example/acme/issue/{identifier}");
    let issue = |identifier: &str, title: &str, status: &str, extra: serde_json::Value| {
        let mut entry = serde_json::json!({
            "version": 1, "source": "linear", "key": identifier, "identifier": identifier,
            "title": title, "body": format!("Moved from Linear.\n\n{title}, as the team wrote it."),
            "status": status, "labels": [], "assignee": null, "comments": [], "links": [],
            "parent": null, "url": page(identifier),
            // Who wrote it there and when, as a migration brings them: the header shows
            // the author, the tool and the day as its provenance row.
            "author": { "name": "Grace Hopper", "handle": "grace" },
            "created_at": "2026-11-02T09:00:00Z"
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
    let _ = crate::import_inbox::apply(chain, &entries);
}

/// The view state a scenario shows.
pub(crate) fn stage(scenario: Option<&str>, nav: &Nav, tasks: &Tasks) {
    match scenario {
        Some("board") => nav.place.set(Place::Board),
        Some("mine") => {
            nav.place.set(Place::List);
            nav.filter.set(Some(Filter::Mine.key()));
        }
        Some("issue" | "migrated-issue") => {
            nav.place.set(Place::List);
            if let Some((id, _)) = tasks.issues().by_number(1) {
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
