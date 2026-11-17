//! What a tracker costs whoever builds and reads it through the actions Tasks
//! publishes, with no window used: every call is counted as it is made.
//!
//! Work that is kept as issues is a project, its issues and their
//! sub-issues, moved along the board and commented as it goes. The counts
//! here are what that takes call by call, and what reading back where the
//! project stands takes.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

mod common;

use std::collections::BTreeSet;

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

/// Reading back where the project stands: the tracker's listing names each
/// issue and says nothing else of it, so its status, its parent, when it is
/// wanted and who has it take a read of each issue.
#[test]
fn where_a_project_stands_takes_a_read_of_every_issue() {
    let mut caller = Caller::open();
    caller.tracker(
        "create-area",
        json!({ "name": "Launch", "kind": "project" }),
    );
    for top in 0..TOP {
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
            let made = caller.write(
                &chain(),
                "create",
                json!({
                    "title": format!("Piece {top}, step {under}"),
                    "project": "Launch",
                    "parent": parent,
                }),
            );
            if under == 0 {
                caller.write(&made[0], "place", json!({ "status": "in-progress" }));
            }
        }
    }

    let listing = caller.read(&chain(), json!({}));
    let rows = listing["issues"]
        .as_array()
        .expect("the listing names the issues")
        .clone();
    assert_eq!(rows.len(), ISSUES, "{listing}");
    for row in &rows {
        let said: Vec<&str> = row
            .as_object()
            .expect("a row")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            said,
            ["item", "name"],
            "a row names its issue and says nothing else of it"
        );
    }
    assert_eq!(caller.reads, 1);

    // Where each stands is in the issue itself: one read each.
    let mut statuses = BTreeSet::new();
    let mut nested = 0_usize;
    let mut dated = 0_usize;
    for row in &rows {
        let issue = caller.read(row["item"].as_str().expect("its address"), json!({}));
        assert!(!issue["status"].is_null(), "{issue}");
        statuses.insert(issue["status"].to_string());
        nested = nested.saturating_add(usize::from(!issue["parent"].is_null()));
        dated = dated.saturating_add(usize::from(!issue["due_ms"].is_null()));
    }
    assert_eq!(statuses.len(), 2, "moved and not moved: {statuses:?}");
    assert_eq!(nested, TOP * UNDER_EACH);
    assert_eq!(dated, TOP);
    assert_eq!(
        caller.reads,
        1 + ISSUES,
        "the listing, then one read for each issue"
    );
    assert_eq!(caller.reads, 21);
}
