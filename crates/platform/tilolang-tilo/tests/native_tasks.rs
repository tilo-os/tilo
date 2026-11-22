//! A tracker the native Tasks app wrote, opened by the tilolang Tasks: its migration carries
//! every project, area and issue over once, as one change of the tracker, each issue keeping the
//! id it had: its title, its description, its priority, the day and moment it was due, who had
//! it, its labels, the project it worked towards and the issue it stood under. What was said in
//! an issue's thread is written under its description. The native app's documents are read,
//! never written.
//!
//! `legacy/rust_tasks.hex` is what the native app's own actions wrote: its tracker record and the
//! part its issues are kept in, saved as that app stores them. The test that wrote it was deleted
//! with the app; the bytes stay, so the conversion keeps proving itself against what the app
//! really wrote.

use automerge::AutoCommit;
use tilo_actions::policy::Role;
use tilolang_runtime::members::Member;
use tilolang_runtime::{ResourceRef, Value};

use crate::fixtures::Replica;
use crate::tasks::call;

const TASKS_APP: &str = "so.tilo.tasks";
const DISCUSSION: (&str, &str) = (
    include_str!("../../../../apps/tl/discussion/app.tl"),
    include_str!("../../../../apps/tl/discussion/app.ids"),
);
const TASKS: (&str, &str) = (
    include_str!("../../../../apps/tl/tasks/app.tl"),
    include_str!("../../../../apps/tl/tasks/app.ids"),
);

/// Where a workspace keeps the native tracker's record: its default document, the record the
/// chain of issues belongs to. The native app's test harness wrote it under the package's own
/// primary id instead.
const RECORD: &str = "derived:default@tab:0000000000000000";

/// The issues the fixture's tracker holds, by the ids the native app gave them, in its order: a
/// parent before the issues under it, siblings in the order of their ranks.
const ISSUES: [&str; 6] = [
    "4390222c76109f55cc20",
    "202ad31c99536036ee92",
    "d9c57d5ae086bfe4d8a4",
    "78fe892e1ea20bce9aa9",
    "ef8d7cec9135103cc346",
    "b669ccca41538f8467f3",
];
/// The projects and areas the fixture's tracker holds, in its order.
const PROJECTS: [(&str, &str); 2] = [
    ("cc5c9ea68bef68bb1272", "Launch"),
    ("0deee4480c8fc00777aa", "Office"),
];

/// The documents of the fixture, each by its document id in a workspace.
fn native_documents() -> Vec<(String, AutoCommit)> {
    include_str!("legacy/rust_tasks.hex")
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (id, hex) = line.split_once('\t').expect("an id and the document");
            let bytes: Vec<u8> = hex
                .as_bytes()
                .chunks(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect();
            let id = if id == "@tilo:package-primary" {
                RECORD
            } else {
                id
            };
            (id.to_owned(), AutoCommit::load(&bytes).unwrap())
        })
        .collect()
}

/// Ada's workspace with the tilolang Tasks on it, the native app's `documents` beside it, and its
/// tracker as the window opens it.
fn opened(documents: &[(String, AutoCommit)]) -> (Replica, ResourceRef) {
    let (ada, _) = Replica::new("ada", AutoCommit::new(), &[DISCUSSION, TASKS]);
    ada.host.set_clock(|| "2026-11-22T10:00:00Z".to_owned());
    ada.host.runtime().set_members(
        vec![Member::person("ada", Role::Owner), Member::agent("claude")],
        None,
    );
    ada.host.runtime().set_earlier(TASKS_APP, documents);
    let tracker = ada.host.root_of(TASKS_APP).expect("the tracker");
    (ada, tracker)
}

fn read(ada: &Replica, at: &ResourceRef, field: &str) -> Value {
    ada.host.runtime().read(at, field).unwrap()
}

fn listed(ada: &Replica, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
    match read(ada, at, field) {
        Value::List(items) => items
            .iter()
            .map(|item| match item {
                Value::Ref(r) => (*r.target).clone(),
                other => panic!("a resource: {other:?}"),
            })
            .collect(),
        other => panic!("a collection reads as a list: {other:?}"),
    }
}

fn id_of(item: &ResourceRef) -> String {
    item.item.clone().expect("an item of a collection")
}

fn text(ada: &Replica, at: &ResourceRef, field: &str) -> String {
    match read(ada, at, field) {
        Value::Text(text) => text,
        other => panic!("{field} reads {other:?}"),
    }
}

fn opt_text(ada: &Replica, at: &ResourceRef, field: &str) -> Option<String> {
    match read(ada, at, field) {
        Value::Opt(Some(held)) => match *held {
            Value::Text(text) => Some(text),
            other => panic!("{field} reads {other:?}"),
        },
        Value::Opt(None) => None,
        other => panic!("{field} reads {other:?}"),
    }
}

fn done(ada: &Replica, at: &ResourceRef) -> bool {
    match read(ada, at, "done") {
        Value::Bool(done) => done,
        other => panic!("done reads {other:?}"),
    }
}

fn priority(ada: &Replica, at: &ResourceRef) -> String {
    match read(ada, at, "priority") {
        Value::Enum(case) => case.variant,
        other => panic!("a priority: {other:?}"),
    }
}

fn labels(ada: &Replica, at: &ResourceRef) -> Vec<String> {
    match read(ada, at, "labels") {
        Value::List(names) => names
            .into_iter()
            .map(|name| match name {
                Value::Text(name) => name,
                other => panic!("a label reads {other:?}"),
            })
            .collect(),
        other => panic!("the labels read {other:?}"),
    }
}

/// What a task's notes say, each block of its rich text a line.
fn notes(ada: &Replica, at: &ResourceRef) -> String {
    match read(ada, at, "notes") {
        Value::Rich(rich) => rich
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("the notes read {other:?}"),
    }
}

/// The carried tracker, as the native app showed it: the projects in its order, and the issues in
/// its order, each keeping the id it had and under what it worked towards and stood under.
fn assert_carried(ada: &Replica, tracker: &ResourceRef) {
    let projects = listed(ada, tracker, "projects");
    let names: Vec<String> = projects.iter().map(|p| text(ada, p, "name")).collect();
    assert_eq!(names, ["Launch", "Office"], "each area is a project");
    let project_ids: Vec<String> = projects.iter().map(id_of).collect();
    assert_eq!(
        project_ids,
        PROJECTS.map(|(id, _)| id.to_owned()),
        "each keeps the id it had"
    );

    let tasks = listed(ada, tracker, "items");
    let ids: Vec<String> = tasks.iter().map(id_of).collect();
    assert_eq!(
        ids,
        ISSUES.map(str::to_owned),
        "every issue, each keeping its id, a parent before the issues under it"
    );
    let titles: Vec<String> = tasks.iter().map(|t| text(ada, t, "title")).collect();
    assert_eq!(
        titles,
        [
            "Book the venue",
            "Call the caterer",
            "Send the invitations",
            "Fix the printer",
            "Draft the budget",
            "Rent a boat",
        ]
    );
    let done: Vec<bool> = tasks.iter().map(|t| done(ada, t)).collect();
    assert_eq!(
        done,
        [false, false, true, false, false, true],
        "a finished issue is a done task"
    );
    assert_eq!(
        opt_text(ada, &tasks[0], "due").as_deref(),
        Some("2026-12-04"),
        "the day it was due"
    );
    assert_eq!(
        opt_text(ada, &tasks[1], "due").as_deref(),
        Some("2026-12-08")
    );
    assert_eq!(
        opt_text(ada, &tasks[1], "at").as_deref(),
        Some("2026-12-08T15:30:00Z"),
        "the moment it was due at"
    );
    // The parent is the task the issue stood under.
    match read(ada, &tasks[1], "parent") {
        Value::Opt(Some(held)) => {
            assert_eq!(
                id_of(&match *held {
                    Value::Ref(r) => (*r.target).clone(),
                    other => panic!("a parent: {other:?}"),
                }),
                ISSUES[0]
            );
        }
        other => panic!("the caterer stood under the venue: {other:?}"),
    }
    // Each works towards the project its area named, and none where it named none.
    let project_of = |t: &ResourceRef| match read(ada, t, "project") {
        Value::Opt(Some(held)) => match *held {
            Value::Ref(r) => Some(id_of(&r.target)),
            other => panic!("a project: {other:?}"),
        },
        Value::Opt(None) => None,
        other => panic!("a project: {other:?}"),
    };
    assert_eq!(project_of(&tasks[0]).as_deref(), Some(PROJECTS[0].0));
    assert_eq!(project_of(&tasks[1]).as_deref(), Some(PROJECTS[0].0));
    assert_eq!(project_of(&tasks[2]).as_deref(), Some(PROJECTS[0].0));
    assert_eq!(project_of(&tasks[3]), None);
    assert_eq!(project_of(&tasks[4]).as_deref(), Some(PROJECTS[1].0));
    assert_eq!(project_of(&tasks[5]), None);

    let carried: Vec<Vec<String>> = tasks.iter().map(|t| labels(ada, t)).collect();
    assert_eq!(carried[0], ["events"]);
    assert_eq!(carried[3], ["hardware", "office"]);
    assert_eq!(carried[1], Vec::<String>::new());
    // The cases' ids, as `apps/tl/tasks/app.ids` holds them: high, none, urgent, low.
    let levels: Vec<String> = tasks.iter().map(|t| priority(ada, t)).collect();
    assert_eq!(
        levels,
        [
            "clxahlh2", "itg256eu", "itg256eu", "d6avlyc5", "mq2uqfpl", "itg256eu"
        ],
        "the level the issue kept, 1 urgent to 4 low"
    );

    let said = notes(ada, &tasks[0]);
    assert!(
        said.contains("Three venues answered.") && said.contains("Ask about parking"),
        "the issue's description, as it was written: {said:?}"
    );
    assert!(
        said.contains("Said in its thread") && said.contains("2026-11-21: The hall replied first."),
        "what was said in its thread, with who said it, when, and what: {said:?}"
    );
    assert!(
        listed(ada, tracker, "sections").is_empty(),
        "nothing was in a list of a board"
    );
}

#[test]
fn a_native_tracker_is_carried_over_once_as_one_change() {
    let documents = native_documents();
    let before: Vec<Vec<automerge::ChangeHash>> = documents
        .iter()
        .map(|(_, doc)| doc.clone().get_heads())
        .collect();
    let (mut ada, tracker) = opened(&documents);
    assert_eq!(
        ada.commit(),
        1,
        "the tracker, as the package makes it when it opens"
    );
    assert!(
        listed(&ada, &tracker, "items").is_empty(),
        "nothing is carried before the migration runs"
    );
    assert_eq!(ada.host.migrate(), 1, "the tracker's migration runs");
    assert_eq!(ada.commit(), 1, "as one change");
    assert_carried(&ada, &tracker);

    let migration = ada
        .host
        .package(TASKS_APP)
        .unwrap()
        .meta
        .migrations()
        .find(|m| m.name == "carry_native_tracker")
        .unwrap()
        .id
        .clone();
    let versions = ada.host.runtime().versions(&tracker).unwrap();
    assert_eq!(
        versions.last().map(|v| v.action.as_str()),
        Some(migration.as_str()),
        "history names the migration as the change that carried the tracker: {versions:?}"
    );
    assert_eq!(
        versions.iter().filter(|v| v.action == migration).count(),
        1,
        "one version of the tracker"
    );

    assert_eq!(ada.host.migrate(), 0, "it runs once");
    assert_eq!(ada.commit(), 0);
    let grace = ada.fork("grace", &[DISCUSSION, TASKS]);
    grace.host.runtime().set_earlier(TASKS_APP, &documents);
    assert_eq!(
        grace.host.migrate(),
        0,
        "another device with the same documents finds it done"
    );
    assert_carried(&grace, &tracker);

    let after: Vec<Vec<automerge::ChangeHash>> = documents
        .iter()
        .map(|(_, doc)| doc.clone().get_heads())
        .collect();
    assert_eq!(before, after, "the native app's documents are not written");
}

#[test]
fn a_carried_task_waits_and_goes_on() {
    let (mut ada, tracker) = opened(&native_documents());
    assert_eq!(ada.host.migrate(), 1);
    ada.commit();
    let task = listed(&ada, &tracker, "items")[0].clone();
    call(
        &ada,
        "wait",
        vec![
            Value::resource(&task),
            Value::text("the venue's reply"),
            Value::text("2026-12-01T10:00:00Z"),
            Value::text(""),
        ],
    );
    assert_eq!(text(&ada, &task, "waiting_for"), "the venue's reply");
    assert_eq!(
        opt_text(&ada, &task, "waiting_until").as_deref(),
        Some("2026-12-01T10:00:00Z")
    );
    call(&ada, "resume", vec![Value::resource(&task)]);
    assert_eq!(
        text(&ada, &task, "waiting_for"),
        "",
        "it waits for nothing again"
    );
    assert_eq!(opt_text(&ada, &task, "waiting_until"), None);
}
