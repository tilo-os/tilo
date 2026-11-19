//! What a tracker moved in from another service costs Tasks, made as `tilo migrate` makes it:
//! 2,000 issues 200 to a `create-many`, a fifth of them with sub-issues under them in the same
//! list, then their 4,000 comments and their links as groups of the issues' own actions.
//!
//! The calls and what landed are asserted; the time is printed for reading
//! (`cargo test -p tasks --test migration_cost -- --ignored --nocapture`), never asserted: on a
//! shared machine it varies with the load.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

mod common;

use common::{chain, held};
use serde_json::{Value, json};
use tasks::App;
use ui::prelude::*;

const ISSUES: usize = 2_000;
const LIST: usize = 200;
const COMMENTS: usize = 2;

fn origin(at: usize) -> Value {
    json!({
        "author": "Grace Hopper",
        "handle": "grace@example.com",
        "source": "Linear",
        "url": format!("https://linear.example/acme/issue/ENG-{at}"),
        "reference": format!("ENG-{at}"),
        "created_ms": 1_756_720_800_000_i64,
        "updated_ms": null,
    })
}

#[test]
#[ignore = "a measurement, read with --ignored --nocapture"]
fn a_moved_in_tracker_of_two_thousand_issues_is_a_few_calls_per_two_hundred() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    let started = std::time::Instant::now();
    let mut calls = 0_usize;
    for project in 0..3 {
        app.call(
            "create-area",
            json!({ "name": format!("Project {project}"), "kind": "project" }),
        )
        .expect("the project is made");
        calls += 1;
    }
    let made_at = std::time::Instant::now();
    let mut made: Vec<String> = Vec::with_capacity(ISSUES);
    for first in (0..ISSUES).step_by(LIST) {
        let issues: Vec<Value> = (first..first + LIST)
            .map(|at| {
                let mut issue = json!({
                    "title": format!("Issue {at}"),
                    "body": format!("What issue {at} is about, as it was written there."),
                    "status": "todo",
                    "url": format!("https://linear.example/acme/issue/ENG-{at}"),
                    "project": format!("Project {}", at % 3),
                    "labels": ["Migration"],
                    "origin": origin(at),
                });
                if at % 5 != 0 {
                    issue["under"] = json!((at - at % 5 - first + 1).to_string());
                }
                issue
            })
            .collect();
        let answered = app
            .call_on_output(&chain(), "create-many", json!({ "issues": issues }))
            .expect("the issues are made")
            .expect("it answers")
            .resources;
        assert_eq!(answered.len(), LIST, "every issue made is answered");
        made.extend(answered);
        calls += 1;
    }
    let issues_took = made_at.elapsed();
    let comments_at = std::time::Instant::now();
    for list in made.chunks(LIST) {
        let comments: Vec<Value> = list
            .iter()
            .flat_map(|issue| {
                (0..COMMENTS).map(move |n| {
                    json!({ "action": "comment", "args": {
                        "issue": issue,
                        "text": format!("Comment {n}, moved in with its author and date."),
                        "origin": origin(n),
                    }})
                })
            })
            .collect();
        app.call_on(&chain(), "group", json!({ "actions": comments }))
            .expect("the comments land");
        calls += 1;
        let links: Vec<Value> = list
            .iter()
            .step_by(10)
            .zip(list.iter().skip(1).step_by(10))
            .map(|(from, to)| {
                json!({ "action": "link", "args": {
                    "issue": from, "app": "so.tilo.tasks", "kind": "tasks.tracker.issues",
                    "resource": to, "label": "Blocks",
                }})
            })
            .collect();
        app.call_on(&chain(), "group", json!({ "actions": links }))
            .expect("the links land");
        calls += 1;
    }
    let comments_took = comments_at.elapsed();
    let took = started.elapsed();

    let tracker = held(&app);
    assert_eq!(tracker.len(), ISSUES);
    let issues: Vec<_> = tracker.values().collect();
    assert_eq!(
        issues.iter().filter(|issue| issue.parent.is_some()).count(),
        ISSUES / 5 * 4
    );
    assert_eq!(
        issues
            .iter()
            .map(|issue| issue.comments.len())
            .sum::<usize>(),
        ISSUES * COMMENTS
    );
    assert_eq!(calls, 3 + ISSUES / LIST * 3);
    eprintln!(
        "{ISSUES} issues with {} comments in {calls} calls: {took:?} (issues {issues_took:?}, comments and links {comments_took:?})",
        ISSUES * COMMENTS
    );
}
