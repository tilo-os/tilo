//! A tracker from Linear brought into a workspace whose tl Tasks keeps what it is asked, through
//! the app's own actions: an origin and an issue each, then the issue's body, project, parent,
//! labels, day and the relations it is linked by. A second import asks nothing, and an import cut
//! off or refused part-way is finished by the next one without making anything twice.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::panic_in_result_fn,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;

use crate::service::{
    self, IncomingAuthor, IncomingComment, IncomingGroup, IncomingIssue, IncomingRelation,
    ServiceJournal, ServiceKind, ServiceReport, ServiceReview,
};
use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::services::{self as services_source, LINEAR};
use super::state::Place;
use super::tests::test_env::{Told, land_as};
use super::writer::{Answer, Invocation, Refused, Workspace};

/// An issue as the fake Tasks keeps it.
#[derive(Clone, Debug, Default)]
struct Kept {
    title: String,
    body: String,
    url: String,
    reference: String,
    parent: Option<String>,
    project: Option<String>,
    comments: Vec<String>,
    links: Vec<String>,
}

/// Tasks as the workspace answers it, through the app's own actions: each call counted, an issue
/// made once by its origin and its reference.
#[derive(Default)]
struct Tasks {
    calls: usize,
    /// How many origins, issues and contacts it made: each one's id is its count.
    made: usize,
    issues: BTreeMap<String, Kept>,
    /// Each origin's page and the name the service gave it, by its id.
    origins: BTreeMap<String, (String, String)>,
    areas: usize,
    contacts: usize,
    /// A title Tasks refuses, as it refuses a title from a service it cannot keep.
    refused_title: Option<String>,
    /// How many calls are answered before the session drops.
    drops_after: Option<usize>,
    /// Whether it answers an issue as not ready yet, as a workspace does whose earlier issues are
    /// still on their way to it.
    earlier_on_their_way: bool,
}

/// How many calls a cut-off session answers: past the projects and people a hand-over opens
/// with, into its first issues.
const CUT_AFTER: usize = 60;

fn refused(why: impl Into<String>) -> anyhow::Error {
    Refused::coded("refused", why).into()
}

fn text(args: &Value, field: &str) -> String {
    args.get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The id part of a resource address (`tl:so.tilo.tasks:task-2` is `task-2`).
fn id_of(address: &str) -> String {
    address
        .rsplit_once(':')
        .map_or_else(|| address.to_owned(), |(_, id)| id.to_owned())
}

impl Tasks {
    /// Make `issue` with the origin it was given, or answer the one an earlier import made.
    fn import_issue(&mut self, args: &Value) -> Result<Answer> {
        if self.earlier_on_their_way {
            bail!(Refused::coded(
                "not_ready",
                "the workspace is not ready for this call yet"
            ));
        }
        let title = text(args, "title");
        if let Some(refused_title) = &self.refused_title
            && title == *refused_title
        {
            bail!(refused("An issue's title fits on a line"));
        }
        let (url, _) = self
            .origins
            .get(&id_of(&text(args, "origin")))
            .cloned()
            .unwrap_or_default();
        let key = text(args, "key");
        if let Some((address, _)) = self
            .issues
            .iter()
            .find(|(_, issue)| issue.url == url && issue.reference == key)
        {
            // The same issue from the same place is the one made before: importing it again
            // makes none, and answers the task it made.
            return Ok(Answer::of(Some(json!({"id": id_of(address)}))));
        }
        let id = format!("task-{}", self.made);
        self.made += 1;
        self.issues.insert(
            format!("tl:so.tilo.tasks:{id}"),
            Kept {
                title,
                url,
                reference: key,
                ..Kept::default()
            },
        );
        Ok(Answer::of(Some(json!({"id": id}))))
    }

    /// Say `text` in the thread of the task `$from` names.
    fn comment(&mut self, args: &Value) -> Result<()> {
        let address = args["$from"]["ref"].as_str().unwrap_or_default();
        let Some(issue) = self.issues.get_mut(address) else {
            bail!(refused(format!("no task {address}")));
        };
        issue.comments.push(text(args, "text"));
        Ok(())
    }

    /// Apply `verb` to the task or project at `address`.
    fn apply_item(&mut self, address: &str, verb: &str, args: &Value) -> Result<()> {
        let Some(issue) = self.issues.get_mut(address) else {
            // A project's own verbs change no issue.
            return match verb {
                "rename" | "set_kind" => Ok(()),
                other => bail!(refused(format!("no task {address} for {other}"))),
            };
        };
        match verb {
            "edit-text" => issue.body = text(args, "value"),
            "file" => {
                issue.project = args.get("p").and_then(Value::as_str).map(str::to_owned);
            }
            "move_to" => {
                issue.parent = args
                    .get("parent")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            "relate" => issue.links.push(text(args, "other")),
            "assign"
            | "estimate"
            | "schedule"
            | "label"
            | "set_priority_none"
            | "set_priority_low"
            | "set_priority_medium"
            | "set_priority_high"
            | "set_priority_urgent" => {}
            other => bail!(refused(format!("Tasks has no {other} here"))),
        }
        Ok(())
    }
}

impl Workspace for Tasks {
    fn act(&mut self, _: &Invocation, action: &str, args: Value) -> Result<Answer> {
        self.calls += 1;
        if self.drops_after.is_some_and(|after| self.calls > after) {
            bail!("the workspace's session closed");
        }
        match action {
            "tasks#open" => Ok(Answer::of(None)),
            "so.tilo.tasks#new_project" => {
                self.areas += 1;
                Ok(Answer::of(Some(
                    json!({"id": format!("area-{}", self.areas)}),
                )))
            }
            "so.tilo.tasks#add_origin" => {
                self.made += 1;
                let id = format!("origin-{}", self.made);
                self.origins
                    .insert(id.clone(), (text(&args, "url"), text(&args, "reference")));
                Ok(Answer::of(Some(json!({"id": id}))))
            }
            "so.tilo.tasks#import_issue" => self.import_issue(&args),
            "so.tilo.contacts#named_or_new" => {
                self.contacts += 1;
                Ok(Answer::of(Some(
                    json!({"id": format!("contact-{}", self.contacts)}),
                )))
            }
            "so.tilo.lang.discussion#comment" => self.comment(&args).map(|()| Answer::of(None)),
            other => {
                let Some(rest) = other.strip_prefix("so.tilo.tasks/") else {
                    bail!("no action {other}");
                };
                let Some((address, verb)) = rest.rsplit_once('#') else {
                    bail!("no action {other}");
                };
                self.apply_item(address, verb, &args)
                    .map(|()| Answer::of(None))
            }
        }
    }

    fn ask(&mut self, action: &str, _: Value) -> Result<Answer> {
        bail!("{action} is not asked by a invocation")
    }

    fn put(&mut self, _: &[u8]) -> Result<String> {
        bail!("issues upload nothing")
    }
}

fn page(at: usize) -> String {
    format!("https://linear.example/acme/issue/ENG-{at}/an-issue")
}

/// `count` issues as Linear answers them: every fifth with four sub-issues after it, in three
/// projects, two comments each, and every tenth related to the one after it.
fn tracker(count: usize) -> Vec<IncomingIssue> {
    (0..count)
        .map(|at| IncomingIssue {
            source: LINEAR.into(),
            key: format!("k{at}"),
            identifier: format!("ENG-{at}"),
            title: format!("Issue {at}"),
            body: format!("What issue {at} is about."),
            status: "todo".into(),
            labels: vec!["Migration".into()],
            assignee: None,
            comments: (0..2)
                .map(|n| IncomingComment {
                    author: "Grace".into(),
                    handle: None,
                    at: "2025-09-01T10:00:00Z".into(),
                    updated_at: String::new(),
                    url: String::new(),
                    text: format!("Comment {n} on issue {at}"),
                })
                .collect(),
            links: Vec::new(),
            parent: (!at.is_multiple_of(5)).then(|| page(at - at % 5)),
            url: page(at),
            author: Some(IncomingAuthor {
                name: "Ada".into(),
                handle: None,
            }),
            created_at: "2025-08-01T10:00:00Z".into(),
            updated_at: String::new(),
            team: Some(IncomingGroup {
                key: "eng".into(),
                name: "ENG".into(),
            }),
            project: Some(IncomingGroup {
                key: format!("p{}", at % 3),
                name: format!("Project {}", at % 3),
            }),
            priority: None,
            estimate: None,
            due: None,
            relations: if at.is_multiple_of(10) && at + 1 < count {
                vec![IncomingRelation {
                    kind: "blocks".into(),
                    page: page(at + 1),
                }]
            } else {
                Vec::new()
            },
        })
        .collect()
}

fn reviewed(place: &Place) {
    let journal = ServiceJournal::new(ServiceReview {
        kind: ServiceKind::Linear,
        account: "linear-acme".into(),
        title: "Acme".into(),
        groups: Vec::new(),
    });
    place.keep_field(LINEAR, "service", &journal).unwrap();
}

/// One import of `issues` into `tasks`, as `tilo migrate start linear` makes it once Linear answered.
fn invocation(place: &Place, tasks: &mut Tasks, issues: &[IncomingIssue], id: &str) -> Result<()> {
    let issues = issues.to_vec();
    let mut told = Told {
        yes: true,
        ..Told::default()
    };
    land_as(
        place,
        LINEAR,
        &Invocation {
            id: id.into(),
            title: "From Linear".into(),
        },
        tasks,
        &mut told,
        |lander| {
            services_source::land_with(LINEAR, lander, |journal, landing, save, cancel| {
                let mut report = ServiceReport::default();
                service::hand_issues(issues, journal, landing, &mut report, save, cancel)?;
                Ok(report)
            })
        },
    )
    .map(|_| ())
}

fn landed(place: &Place) -> usize {
    services_source::landed(place, LINEAR).unwrap()
}

/// Everything `count` issues bring is in Tasks, once.
fn assert_all_there(tasks: &Tasks, count: usize) {
    assert_eq!(tasks.issues.len(), count);
    assert_eq!(tasks.areas, 3, "one project each, made once");
    let by_page: BTreeMap<&str, &str> = tasks
        .issues
        .iter()
        .map(|(id, issue)| (issue.url.as_str(), id.as_str()))
        .collect();
    for issue in tasks.issues.values() {
        let number: usize = issue
            .title
            .strip_prefix("Issue ")
            .and_then(|n| n.parse().ok())
            .unwrap();
        assert_eq!(issue.comments.len(), 2, "{}", issue.title);
        assert_eq!(issue.body, format!("What issue {number} is about."));
        assert!(issue.project.is_some());
        let parent =
            (!number.is_multiple_of(5)).then(|| by_page[page(number - number % 5).as_str()]);
        assert_eq!(issue.parent.as_deref(), parent, "{}", issue.title);
        let links = usize::from(number.is_multiple_of(10) && number + 1 < count);
        assert_eq!(issue.links.len(), links, "{}", issue.title);
        if links == 1 {
            assert_eq!(
                issue.links[0],
                *by_page[page(number + 1).as_str()],
                "{} is linked to the one after it",
                issue.title
            );
        }
    }
}

fn import(count: usize) -> (usize, std::time::Duration) {
    let dir = tempfile::tempdir().unwrap();
    let place = Place::at(dir.path());
    reviewed(&place);
    let issues = tracker(count);
    let mut tasks = Tasks::default();
    let started = std::time::Instant::now();
    invocation(&place, &mut tasks, &issues, "invocation-1").unwrap();
    let took = started.elapsed();
    assert_all_there(&tasks, count);
    assert_eq!(landed(&place), count);

    // A second import finds every issue there and asks the workspace nothing.
    let before = tasks.calls;
    invocation(&place, &mut tasks, &issues, "invocation-2").unwrap();
    assert_eq!(tasks.calls, before, "a second invocation asks nothing");
    assert_all_there(&tasks, count);
    (before, took)
}

#[test]
fn a_tracker_comes_over_once_and_a_second_import_asks_nothing() {
    let (small, _) = import(50);
    let (large, took) = import(2_000);
    // Each issue is two calls (its origin and itself) and what it carries; a second import asks
    // none of them again.
    assert!(large > small, "a larger tracker costs more to bring over");
    eprintln!("2,000 issues with 4,000 comments: {large} calls, {took:?} here");
}

#[test]
fn a_run_cut_off_part_way_is_finished_by_the_next_without_making_anything_twice() {
    let dir = tempfile::tempdir().unwrap();
    let place = Place::at(dir.path());
    reviewed(&place);
    let issues = tracker(450);
    // The session drops while the first issues are being made.
    let mut tasks = Tasks {
        drops_after: Some(CUT_AFTER),
        ..Tasks::default()
    };
    let cut = invocation(&place, &mut tasks, &issues, "invocation-1");
    assert!(cut.is_err());
    let made = tasks.issues.len();
    assert!(
        made > 0 && made < 450,
        "{made} issues were made before the cut"
    );
    assert_eq!(landed(&place), 0, "an unfinished hand-over lands nothing");

    tasks.drops_after = None;
    invocation(&place, &mut tasks, &issues, "invocation-2").unwrap();
    assert_all_there(&tasks, 450);
    assert_eq!(landed(&place), 450);
}

#[test]
fn a_refused_issue_is_named_and_the_rest_of_its_list_lands() {
    let dir = tempfile::tempdir().unwrap();
    let place = Place::at(dir.path());
    reviewed(&place);
    let issues = tracker(50);
    let mut tasks = Tasks {
        refused_title: Some("Issue 17".into()),
        ..Tasks::default()
    };
    let error = invocation(&place, &mut tasks, &issues, "invocation-1").unwrap_err();
    let said = format!("{error:#}");
    assert!(said.contains("49 brought over, 1 refused"), "{said}");
    assert!(said.contains("ENG-17: "), "{said}");
    assert_eq!(tasks.issues.len(), 49);
    assert_eq!(landed(&place), 49, "the refused issue is not counted");

    // Once Tasks takes it, the next import brings only it.
    tasks.refused_title = None;
    invocation(&place, &mut tasks, &issues, "invocation-2").unwrap();
    assert_all_there(&tasks, 50);
}

/// An import cut off part-way and resumed, as a person's large tracker goes: what the second
/// import brings sits under and relates to issues the first import made, by what the first import
/// kept of them; a relation to an issue outside the import waits without refusing anything; and
/// a title from a service is kept whole.
#[test]
fn a_resumed_import_brings_issues_naming_ones_an_earlier_import_brought_or_none_brought() {
    let dir = tempfile::tempdir().unwrap();
    let place = Place::at(dir.path());
    reviewed(&place);
    let mut issues = tracker(450);
    // Across the cut: sub-issues and relations of issues the first import made.
    issues[250].parent = Some(page(10));
    issues[260].relations.push(IncomingRelation {
        kind: "blocks".into(),
        page: page(15),
    });
    // Outside the import.
    issues[270].relations.push(IncomingRelation {
        kind: "related".into(),
        page: "https://linear.example/other/issue/OPS-1/elsewhere".into(),
    });
    issues[290].title = "A title that goes on. ".repeat(20);

    let mut tasks = Tasks {
        drops_after: Some(CUT_AFTER),
        ..Tasks::default()
    };
    assert!(invocation(&place, &mut tasks, &issues, "invocation-1").is_err());
    assert!(
        !tasks.issues.is_empty(),
        "some issue was made before the cut"
    );

    tasks.drops_after = None;
    invocation(&place, &mut tasks, &issues, "invocation-2").unwrap();
    assert_eq!(landed(&place), 450, "nothing refused");
    let by_page = |tasks: &Tasks, page: &str| -> Option<String> {
        tasks
            .issues
            .iter()
            .find(|(_, issue)| issue.url == page)
            .map(|(id, _)| id.clone())
    };
    let made = |at: usize| &tasks.issues[&by_page(&tasks, &page(at)).unwrap()];
    assert_eq!(made(250).parent, by_page(&tasks, &page(10)));
    let fifteen = by_page(&tasks, &page(15)).unwrap();
    assert!(made(260).links.contains(&fifteen), "{:?}", made(260).links);
    let long = made(290);
    // Whole, with only the space around it trimmed, as every imported title is.
    assert_eq!(long.title, issues[290].title.trim());
    assert_eq!(long.body, issues[290].body, "its description is kept whole");
}

/// A workspace whose earlier issues are still on their way answers not ready, and keeps doing so
/// after the call asked again: the import stops there, refusing nothing and making no issue
/// without its parent, and the next one brings the rest where they belong.
#[test]
fn a_workspace_not_ready_stops_the_import_without_refusing_or_unnesting() {
    let dir = tempfile::tempdir().unwrap();
    let place = Place::at(dir.path());
    reviewed(&place);
    let mut tasks = Tasks::default();
    invocation(&place, &mut tasks, &tracker(50), "invocation-1").unwrap();

    let mut issues = tracker(60);
    issues[52].parent = Some(page(10));
    tasks.earlier_on_their_way = true;
    let stopped = invocation(&place, &mut tasks, &issues, "invocation-2").unwrap_err();
    let said = format!("{stopped:#}");
    assert!(!said.contains("refused"), "{said}");
    assert_eq!(tasks.issues.len(), 50, "nothing made while not ready");

    tasks.earlier_on_their_way = false;
    invocation(&place, &mut tasks, &issues, "invocation-3").unwrap();
    assert_eq!(landed(&place), 60);
    let id_of = |at: usize| {
        tasks
            .issues
            .iter()
            .find(|(_, issue)| issue.url == page(at))
            .map(|(id, _)| id.clone())
    };
    let fifty_two = id_of(52).unwrap();
    assert_eq!(
        tasks.issues[&fifty_two].parent,
        id_of(10),
        "under its parent"
    );
}
