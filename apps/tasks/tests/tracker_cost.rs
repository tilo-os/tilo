//! What a tracker costs whoever builds and reads it through the actions Tasks
//! publishes, with no window used: every call is counted as it is made.
//!
//! Work that is kept as issues is a project, its issues and their
//! sub-issues, moved along the board and commented as it goes. The counts
//! here are what that takes call by call, and what reading back where the
//! project stands takes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]


mod common;

use common::{chain, held};
use serde_json::{Value, json};
use tasks::App;
use tasks::model::Status;
use ui::prelude::*;

const DAY_MS: i64 = 86_400_000;

/// The issues of the project that stand at the top, and the sub-issues under
/// each: twenty issues in all.
const TOP: usize = 5;
const UNDER_EACH: usize = 3;
const ISSUES: usize = TOP * (1 + UNDER_EACH);
/// How many of them are moved on the board, and how many are commented.
const MOVED: usize = 10;
const COMMENTED: usize = 10;

/// A caller of Tasks' published actions that counts its calls: each is one
/// action invoked by its id with JSON arguments, as an agent's `tilo_invoke`
/// does.
struct Caller {
    app: TestApp<App>,
    writes: usize,
    reads: usize,
}

impl Caller {
    fn open() -> Self {
        ui::i18n::I18nManager::init("en-US");
        Self {
            app: TestApp::<App>::for_person("ada").expect("open"),
            writes: 0,
            reads: 0,
        }
    }

    /// One action of the tracker's own record: its projects and areas.
    fn tracker(&mut self, action: &str, arguments: Value) {
        self.writes = self.writes.saturating_add(1);
        self.app
            .call(action, arguments)
            .unwrap_or_else(|error| panic!("{action}: {error:?}"));
    }

    /// One action on `resource` that changes issues: the resources it made.
    fn write(&mut self, resource: &str, action: &str, arguments: Value) -> Vec<String> {
        self.writes = self.writes.saturating_add(1);
        self.app
            .call_on_output(resource, action, arguments)
            .unwrap_or_else(|error| panic!("{action}: {error:?}"))
            .map(|output| output.resources)
            .unwrap_or_default()
    }

    /// One read of `resource`.
    fn read(&mut self, resource: &str, arguments: Value) -> Value {
        self.reads = self.reads.saturating_add(1);
        self.app
            .call_on(resource, "read", arguments)
            .unwrap_or_else(|error| panic!("read {resource}: {error:?}"))
            .expect("the read answers")
    }
}

/// When the top-level issue `index` is wanted.
fn due(index: usize) -> i64 {
    DAY_MS.saturating_mul(i64::try_from(index).unwrap().saturating_add(1))
}

/// What the tracker holds once the project is built, whichever calls built
/// it.
fn assert_the_project_stands(caller: &Caller) {
    let held = held(&caller.app);
    assert_eq!(held.len(), ISSUES);
    let issues: Vec<_> = held.values().collect();
    assert!(
        issues.iter().all(|issue| issue
            .area
            .as_ref()
            .is_some_and(|area| area.label == "Launch")),
        "every issue works towards the project"
    );
    let count = |holds: &dyn Fn(&tasks::model::Issue) -> bool| {
        issues.iter().filter(|issue| holds(issue)).count()
    };
    assert_eq!(count(&|issue| issue.parent.is_some()), TOP * UNDER_EACH);
    assert_eq!(count(&|issue| issue.due_ms.is_some()), TOP);
    assert_eq!(count(&|issue| issue.status == Status::InProgress), MOVED);
    assert_eq!(count(&|issue| issue.comments.len() == 1), COMMENTED);
}

/// The project built with the actions that take one issue each: a call per
/// project, per issue, per move on the board and per comment.
#[test]
fn a_project_built_one_issue_a_call_takes_a_call_for_each_issue_move_and_comment() {
    let mut caller = Caller::open();
    caller.tracker(
        "create-area",
        json!({ "name": "Launch", "kind": "project" }),
    );
    let mut made = Vec::new();
    for top in 0..TOP {
        // A sub-issue names its parent, so the parent is made first and its
        // create answers what the sub-issues name.
        let parent = caller
            .write(
                &chain(),
                "create",
                json!({ "title": format!("Piece {top}"), "project": "Launch", "due_ms": due(top) }),
            )
            .into_iter()
            .next()
            .expect("the create answers its issue");
        for under in 0..UNDER_EACH {
            made.extend(caller.write(
                &chain(),
                "create",
                json!({
                    "title": format!("Piece {top}, step {under}"),
                    "project": "Launch",
                    "parent": parent,
                }),
            ));
        }
        made.push(parent);
    }
    assert_eq!(made.len(), ISSUES);
    for issue in made.iter().take(MOVED) {
        caller.write(issue, "place", json!({ "status": "in-progress" }));
    }
    for (at, issue) in made.iter().rev().take(COMMENTED).enumerate() {
        caller.write(issue, "comment", json!({ "text": format!("Finding {at}") }));
    }
    assert_the_project_stands(&caller);
    assert_eq!(
        caller.writes,
        1 + ISSUES + MOVED + COMMENTED,
        "the project, then one call for each issue, each move and each comment"
    );
    assert_eq!(caller.writes, 41);
}

/// The same project with its issues made as one list: the project, its
/// issues with their sub-issues in one call, then a call for each move and
/// each comment.
#[test]
fn the_same_project_with_its_issues_made_as_one_list_takes_a_call_for_them_all() {
    let mut caller = Caller::open();
    caller.tracker(
        "create-area",
        json!({ "name": "Launch", "kind": "project" }),
    );
    let mut issues = Vec::new();
    for top in 0..TOP {
        issues.push(json!({
            "key": format!("piece-{top}"),
            "title": format!("Piece {top}"),
            "project": "Launch",
            "due_ms": due(top),
        }));
        for under in 0..UNDER_EACH {
            issues.push(json!({
                "title": format!("Piece {top}, step {under}"),
                "project": "Launch",
                "under": format!("piece-{top}"),
            }));
        }
    }
    let made = caller.write(&chain(), "create-many", json!({ "issues": issues }));
    assert_eq!(made.len(), ISSUES, "the call answers every issue it made");
    for issue in made.iter().take(MOVED) {
        caller.write(issue, "place", json!({ "status": "in-progress" }));
    }
    for (at, issue) in made.iter().rev().take(COMMENTED).enumerate() {
        caller.write(issue, "comment", json!({ "text": format!("Finding {at}") }));
    }
    assert_the_project_stands(&caller);
    assert_eq!(
        caller.writes,
        2 + MOVED + COMMENTED,
        "the project, its issues, then one call for each move and each comment"
    );
    assert_eq!(caller.writes, 22);
}

/// The same project built several issues a call: the project, its issues
/// with their sub-issues in one list, and the moves and comments together as
/// one group of the tracker's own actions.
#[test]
fn the_same_project_built_several_issues_a_call_takes_three_calls() {
    let mut caller = Caller::open();
    caller.tracker(
        "create-area",
        json!({ "name": "Launch", "kind": "project" }),
    );
    let mut issues = Vec::new();
    for top in 0..TOP {
        issues.push(json!({
            "key": format!("piece-{top}"),
            "title": format!("Piece {top}"),
            "project": "Launch",
            "due_ms": due(top),
        }));
        for under in 0..UNDER_EACH {
            issues.push(json!({
                "title": format!("Piece {top}, step {under}"),
                "project": "Launch",
                "under": format!("piece-{top}"),
            }));
        }
    }
    let made = caller.write(&chain(), "create-many", json!({ "issues": issues }));
    assert_eq!(made.len(), ISSUES, "the call answers every issue it made");
    let moves = made.iter().take(MOVED).map(
        |issue| json!({ "action": "place", "args": { "issue": issue, "status": "in-progress" } }),
    );
    let comments = made
        .iter()
        .rev()
        .take(COMMENTED)
        .enumerate()
        .map(|(at, issue)| {
            json!({ "action": "comment", "args": { "issue": issue, "text": format!("Finding {at}") } })
        });
    let actions: Vec<Value> = moves.chain(comments).collect();
    caller.write(&chain(), "group", json!({ "actions": actions }));
    assert_the_project_stands(&caller);
    assert_eq!(
        caller.writes, 3,
        "the project, its issues, and their moves and comments as one group"
    );
}

/// Reading back where the project stands is one read: each row of the
/// tracker's listing says its issue's status, the issue it stands under,
/// when it is wanted and the project it works towards.
#[test]
fn where_a_project_stands_is_one_read() {
    let mut caller = Caller::open();
    caller.tracker(
        "create-area",
        json!({ "name": "Launch", "kind": "project" }),
    );
    let mut issues = Vec::new();
    for top in 0..TOP {
        issues.push(json!({
            "key": format!("piece-{top}"),
            "title": format!("Piece {top}"),
            "project": "Launch",
            "due_ms": due(top),
        }));
        for under in 0..UNDER_EACH {
            issues.push(json!({
                "title": format!("Piece {top}, step {under}"),
                "project": "Launch",
                "under": format!("piece-{top}"),
                "status": if under == 0 { "in-progress" } else { "backlog" },
            }));
        }
    }
    caller.write(&chain(), "create-many", json!({ "issues": issues }));

    let listing = caller.read(&chain(), json!({}));
    let rows: Vec<Value> = listing["issues"]
        .as_array()
        .expect("the listing names the issues")
        .iter()
        .map(|entry| entry["value"].clone())
        .collect();
    assert_eq!(rows.len(), ISSUES, "{listing}");
    let count = |holds: &dyn Fn(&Value) -> bool| rows.iter().filter(|row| holds(row)).count();
    assert_eq!(count(&|row| row["status"] == "in-progress"), TOP);
    assert_eq!(count(&|row| row["status"] == "backlog"), ISSUES - TOP);
    assert_eq!(count(&|row| !row["parent"].is_null()), TOP * UNDER_EACH);
    assert_eq!(count(&|row| !row["due_ms"].is_null()), TOP);
    assert_eq!(count(&|row| row["area"]["label"] == "Launch"), ISSUES);
    assert_eq!(caller.reads, 1, "the listing says it all");

    // The issues of the project that are being worked on, asked for as such.
    let moving = caller.read(
        &chain(),
        json!({ "where": { "project": "Launch", "status": "in-progress" } }),
    );
    assert_eq!(moving["issues"].as_array().map(Vec::len), Some(TOP));
    assert_eq!(caller.reads, 2);
}
