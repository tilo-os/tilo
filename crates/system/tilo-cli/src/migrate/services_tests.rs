//! A tracker from Linear brought into a workspace whose Tasks keeps what it is asked, as Tasks
//! does: issues made a list at a time, comments, moves and links grouped, the calls counted. The
//! calls stay flat as the tracker grows, a second import asks nothing, and an import cut off or refused
//! part-way is finished by the next one without making anything twice.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::panic_in_result_fn,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use migration_sources::service::{
    self, IncomingAuthor, IncomingComment, IncomingGroup, IncomingIssue, IncomingRelation,
    ServiceJournal, ServiceKind, ServiceReport, ServiceReview,
};
use serde_json::{Value, json};

use super::services::{self as services_source, LINEAR};
use super::state::Place;
use super::tests::test_env::{Told, land_as};
use super::workspace::{Answer, Invocation, Refused, Workspace};

/// An issue as the fake Tasks keeps it.
#[derive(Clone, Debug, Default)]
struct Kept {
    title: String,
    body: String,
    url: String,
    parent: Option<String>,
    project: Option<String>,
    comments: Vec<String>,
    links: Vec<String>,
}

/// Tasks as the workspace answers it: each call counted, a list or group made whole or refused
/// whole, as Tasks does.
#[derive(Default)]
struct Tasks {
    calls: usize,
    /// How many issues it made: each one's id is its count.
    made: usize,
    issues: BTreeMap<String, Kept>,
    areas: usize,
    /// A title Tasks refuses, as it refuses a title too long.
    refused_title: Option<String>,
    /// How many calls are answered before the session drops.
    drops_after: Option<usize>,
    /// Whether it answers a list naming an issue made before as not ready yet, as a workspace
    /// does whose earlier issues are still on their way to it.
    earlier_on_their_way: bool,
}

fn refused(why: impl Into<String>) -> anyhow::Error {
    Refused::coded("refused", why).into()
}

fn text(args: &Value, field: &str) -> String {
    args.get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

impl Tasks {
    /// Apply one of the issues' own actions to `issues`.
    fn apply(issues: &mut BTreeMap<String, Kept>, verb: &str, args: &Value) -> Result<()> {
        let id = text(args, "issue");
        if !issues.contains_key(&id) {
            bail!(refused(format!("no issue {id}")));
        }
        match verb {
            "comment" => {
                let said = text(args, "text");
                assert!(args["origin"]["author"].is_string(), "{args}");
                if let Some(issue) = issues.get_mut(&id) {
                    issue.comments.push(said);
                }
            }
            "move-under" => {
                let parent = text(args, "parent");
                if !issues.contains_key(&parent) {
                    bail!(refused(format!("no issue {parent}")));
                }
                if let Some(issue) = issues.get_mut(&id) {
                    issue.parent = Some(parent);
                }
            }
            "link" => {
                let resource = text(args, "resource");
                if let Some(issue) = issues.get_mut(&id) {
                    issue.links.push(resource);
                }
            }
            other => bail!("Tasks has no {other} here"),
        }
        Ok(())
    }

    fn create_many(&mut self, args: &Value) -> Result<Answer> {
        let list = args["issues"].as_array().cloned().unwrap_or_default();
        if self.earlier_on_their_way && list.iter().any(|issue| issue.get("parent").is_some()) {
            bail!(Refused::coded(
                "not_ready",
                "the workspace is not ready for this call yet"
            ));
        }
        assert!(!list.is_empty() && list.len() <= 200, "{}", list.len());
        if let Some(title) = &self.refused_title
            && let Some(at) = list.iter().position(|issue| issue["title"] == *title)
        {
            bail!(refused(format!("issue {} in the list: too long", at + 1)));
        }
        // What Tasks refuses of an entry refuses the list, naming the entry.
        for (at, issue) in list.iter().enumerate() {
            if text(issue, "title").chars().count() > 240 {
                bail!(refused(format!(
                    "issue {} in the list: An issue's title fits on a line",
                    at + 1
                )));
            }
            if let Some(Value::String(parent)) = issue.get("parent")
                && !self.issues.contains_key(parent)
            {
                bail!(refused(format!(
                    "issue {} in the list: no issue named {parent}",
                    at + 1
                )));
            }
        }
        let mut made: Vec<String> = Vec::new();
        for issue in &list {
            let id = format!("issue-{}", self.made);
            self.made += 1;
            let parent = match (
                issue.get("under").and_then(Value::as_str),
                issue.get("parent"),
            ) {
                (Some(place), _) => {
                    let place: usize = place.parse().expect("a place");
                    Some(made[place - 1].clone())
                }
                (None, Some(Value::String(parent))) => Some(parent.clone()),
                _ => None,
            };
            self.issues.insert(
                id.clone(),
                Kept {
                    title: text(issue, "title"),
                    body: text(issue, "body"),
                    url: text(issue, "url"),
                    parent,
                    project: issue["project"].as_str().map(str::to_owned),
                    ..Kept::default()
                },
            );
            made.push(id);
        }
        Ok(Answer::of(Some(json!({
            "made": made.iter().map(|id| json!({"id": id, "address": format!("tasks.issues/issues/{id}")})).collect::<Vec<_>>(),
        }))))
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
            "tasks/tasks.tracker#create-area" => {
                self.areas += 1;
                let id = format!("area-{}", self.areas);
                Ok(Answer::of(Some(json!({"made": [{"id": id}]}))))
            }
            "tasks/tasks.issues#create-many" => self.create_many(&args),
            "tasks/tasks.issues#group" => {
                let members = args["actions"].as_array().cloned().unwrap_or_default();
                assert!(members.len() <= 500, "{}", members.len());
                // A group lands whole or not at all.
                let mut issues = self.issues.clone();
                for member in &members {
                    Self::apply(
                        &mut issues,
                        member["action"].as_str().unwrap_or_default(),
                        &member["args"],
                    )?;
                }
                self.issues = issues;
                Ok(Answer::of(None))
            }
            other => match other.strip_prefix("tasks/tasks.issues#") {
                Some(verb) => Self::apply(&mut self.issues, verb, &args).map(|()| Answer::of(None)),
                None => bail!("no action {other}"),
            },
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
    place
        .keep(&place.path(LINEAR, "service.json"), &journal)
        .unwrap();
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
        assert!(issue.project.is_some());
        let parent =
            (!number.is_multiple_of(5)).then(|| by_page[page(number - number % 5).as_str()]);
        assert_eq!(issue.parent.as_deref(), parent, "{}", issue.title);
        let links = usize::from(number.is_multiple_of(10) && number + 1 < count);
        assert_eq!(issue.links.len(), links, "{}", issue.title);
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
fn a_tracker_comes_over_in_calls_that_grow_only_by_the_batch() {
    let (small, _) = import(50);
    let (large, took) = import(2_000);
    // Opening Tasks and the three projects, then per 200 issues one call making them, one
    // grouping their comments and one their links.
    assert_eq!(small, 1 + 3 + 3, "50 issues");
    assert_eq!(large, 1 + 3 + 3 * 2_000_usize.div_ceil(200), "2,000 issues");
    eprintln!("2,000 issues with 4,000 comments: {large} calls, {took:?} here");
}

#[test]
fn a_run_cut_off_part_way_is_finished_by_the_next_without_making_anything_twice() {
    let dir = tempfile::tempdir().unwrap();
    let place = Place::at(dir.path());
    reviewed(&place);
    let issues = tracker(450);
    // The session drops after the second list of issues is made, before their comments.
    let mut tasks = Tasks {
        drops_after: Some(1 + 3 + 3 + 1),
        ..Tasks::default()
    };
    let cut = invocation(&place, &mut tasks, &issues, "invocation-1");
    assert!(cut.is_err());
    assert_eq!(tasks.issues.len(), 400, "two lists were made");
    assert_eq!(landed(&place), 200, "the first list landed whole");

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

/// An import cut off part-way and resumed, as a person's large tracker goes: what the second import
/// brings sits under and relates to issues the first import made, by what the first import kept of
/// them; a relation to an issue outside the import waits without refusing anything; and a title
/// longer than Tasks takes is cut, said whole in the description. The second import lands
/// everything it has, in a handful of calls.
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
        drops_after: Some(1 + 3 + 3 + 1),
        ..Tasks::default()
    };
    assert!(invocation(&place, &mut tasks, &issues, "invocation-1").is_err());
    assert_eq!(
        landed(&place),
        200,
        "the first list landed, then the import was cut off"
    );
    let by_page = |tasks: &Tasks, page: &str| -> Option<String> {
        tasks
            .issues
            .iter()
            .find(|(_, issue)| issue.url == page)
            .map(|(id, _)| id.clone())
    };
    tasks.drops_after = None;
    let calls = tasks.calls;
    // The person deleted an issue the first import brought: the later issue that names it as its
    // parent still comes over, at the top level, with its own sub-issue under it.
    let deleted = tasks
        .issues
        .iter()
        .find(|(_, issue)| issue.url == page(20))
        .map(|(id, _)| id.clone())
        .expect("the first import made it");
    tasks.issues.remove(&deleted);
    issues[295].parent = Some(page(20));

    invocation(&place, &mut tasks, &issues, "invocation-2").unwrap();
    assert_eq!(landed(&place), 450, "nothing refused");
    let made = |at: usize| &tasks.issues[&by_page(&tasks, &page(at)).unwrap()];
    assert_eq!(made(295).parent, None, "at the top level");
    assert_eq!(
        made(296).parent.as_deref(),
        by_page(&tasks, &page(295)).as_deref(),
        "its own sub-issue under it"
    );
    assert_eq!(made(250).parent, by_page(&tasks, &page(10)));
    let fifteen = by_page(&tasks, &page(15)).unwrap();
    assert!(
        made(260)
            .links
            .contains(&format!("tasks.issues/issues/{fifteen}")),
        "{:?}",
        made(260).links
    );
    let long = made(290);
    assert_eq!(long.title.chars().count(), 240);
    assert!(long.title.ends_with('…'), "{}", long.title);
    assert!(
        long.body.starts_with(issues[290].title.trim()),
        "{}",
        long.body
    );
    // The issues the first import made but did not finish (comments, links) and the 250 new ones:
    // a call per list, per group of comments and per group of links.
    assert!(tasks.calls - calls <= 12, "{} calls", tasks.calls - calls);
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
