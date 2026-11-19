//! Several of the tracker's actions in one call, through the `group` every
//! record publishes and with no window used: issues placed, assigned, moved
//! to a project and commented, also together, as an agent that finished a
//! round of work calls it.
//!
//! A group is one step where its issues are kept in one part and one step
//! in each part otherwise, is refused whole when one of its members cannot
//! be, and does nothing a call for one issue could not.

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
use tasks::model::{Issue, Status, issue_resource};
use tasks::{App, IssueList};
use ui::prelude::*;

fn open() -> TestApp<App> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<App>::for_person("ada").expect("open")
}

/// `count` issues titled `Issue 0` onwards, made in one call.
fn make(app: &mut TestApp<App>, count: usize) -> Vec<Id> {
    let issues: Vec<Value> = (0..count)
        .map(|at| json!({ "title": format!("Issue {at}") }))
        .collect();
    app.call_on(&chain(), "create-many", json!({ "issues": issues }))
        .expect("the issues are made");
    held(app).ids()
}

/// How many issues of the tracker `holds` is true of.
fn count(app: &TestApp<App>, holds: impl Fn(&Issue) -> bool) -> usize {
    held(app).values().filter(|issue| holds(issue)).count()
}

/// One of the tracker's actions as a member of a group.
fn member(action: &str, args: Value) -> Value {
    let mut member = serde_json::Map::new();
    member.insert("action".into(), action.into());
    member.insert("args".into(), args);
    Value::Object(member)
}

/// The tracker's `group`: several of its actions as one step.
fn group(app: &mut TestApp<App>, actions: Vec<Value>) -> Result<(), String> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("actions".into(), Value::Array(actions));
    app.call_on(&chain(), "group", Value::Object(arguments))
        .map(drop)
        .map_err(|error| format!("{error:?}"))
}

/// Take back the caller's last step in part `part`, as the chain's own
/// `undo` does for whoever calls it.
fn undo(app: &mut TestApp<App>, part: u64) {
    app.call_on(&chain(), "undo", json!({ "part": part }))
        .expect("the step is taken back");
}

#[test]
fn issues_are_placed_and_commented_several_in_one_group() {
    let mut app = open();
    let ids = make(&mut app, 12);

    // Ten moves on the board, named as a listing reads: by number.
    let places = (1..=10)
        .map(|number| {
            member(
                "place",
                json!({
                    "issue": format!("#{number}"),
                    "status": if number % 2 == 0 { "done" } else { "in-review" },
                }),
            )
        })
        .collect();
    group(&mut app, places).expect("ten issues placed in one call");
    assert_eq!(count(&app, |issue| issue.status == Status::InReview), 5);
    assert_eq!(count(&app, |issue| issue.status == Status::Done), 5);
    assert_eq!(count(&app, |issue| issue.status == Status::Backlog), 2);
    undo(&mut app, 0);
    assert_eq!(
        count(&app, |issue| issue.status == Status::Backlog),
        12,
        "placing them was one step"
    );

    // What a round of work leaves: each issue moved on and told what was
    // done, in one call; two comments on one of them, named by title.
    let mut round: Vec<Value> = Vec::new();
    for at in 0..9 {
        let issue = format!("Issue {at}");
        round.push(member(
            "place",
            json!({ "issue": issue, "status": "in-review" }),
        ));
        round.push(member(
            "comment",
            json!({ "issue": issue, "text": format!("Finding {at}") }),
        ));
    }
    round.push(member(
        "comment",
        json!({ "issue": "Issue 0", "text": "And one more thing" }),
    ));
    group(&mut app, round).expect("a round of work in one call");
    assert_eq!(count(&app, |issue| issue.status == Status::InReview), 9);
    assert_eq!(count(&app, |issue| issue.comments.len() == 1), 8);
    let first = held(&app).get(&ids[0]).unwrap();
    assert_eq!(
        first
            .comments
            .values()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["Finding 0", "And one more thing"],
        "in the order given"
    );
    assert!(
        first
            .comments
            .values()
            .all(|message| message.author == Some(ui::tilo_resource::MemberId::person("ada"))),
        "each is its caller's, as one written alone is"
    );
    undo(&mut app, 0);
    assert_eq!(count(&app, |issue| !issue.comments.is_empty()), 0);
    assert_eq!(
        count(&app, |issue| issue.status == Status::Backlog),
        12,
        "the moves and the comments were one step"
    );
}

#[test]
fn issues_are_assigned_and_moved_to_a_project_several_in_one_group() {
    let mut app = open();
    app.call("create-area", json!({ "name": "Launch" }))
        .expect("the project");
    let ids = make(&mut app, 12);

    // Three assignments, named by address, number and title: two to an
    // agent, given by the person who calls as a single `assign` gives it.
    group(
        &mut app,
        vec![
            member(
                "assign",
                json!({ "issue": issue_resource(&ids[0]), "assignee": "agent:scribe" }),
            ),
            member(
                "assign",
                json!({ "issue": "#2", "assignee": "agent:scribe" }),
            ),
            member(
                "assign",
                json!({ "issue": "Issue 2", "assignee": "outside:Grace" }),
            ),
        ],
    )
    .expect("three issues assigned in one call");
    let given = held(&app).get(&ids[0]).unwrap();
    assert_eq!(given.assigned_agent(), Some("agent:scribe"));
    assert_eq!(
        given.assigned_by,
        Some(ui::tilo_resource::MemberId::person("ada")),
        "an agent is asked for whoever gave it the issue"
    );
    assert!(given.assigned_ms > 0);
    assert_eq!(count(&app, |issue| issue.assigned_agent().is_some()), 2);
    assert_eq!(
        count(&app, |issue| issue.assigned_outsider() == Some("Grace")),
        1
    );

    // Four issues into a project, and one of them out again.
    let moves = (1..=4)
        .map(|number| {
            member(
                "set-project",
                json!({ "issue": format!("#{number}"), "project": "Launch" }),
            )
        })
        .collect();
    group(&mut app, moves).expect("four issues moved in one call");
    let in_project = |issue: &Issue| {
        issue
            .area
            .as_ref()
            .is_some_and(|area| area.label == "Launch")
    };
    assert_eq!(count(&app, in_project), 4);
    group(
        &mut app,
        vec![member("set-project", json!({ "issue": "#4" }))],
    )
    .expect("one moved out of every project");
    assert_eq!(count(&app, in_project), 3);
}

#[test]
fn a_group_with_one_member_that_cannot_be_is_refused_whole_and_says_which() {
    let mut app = open();
    app.call("create-area", json!({ "name": "Launch" }))
        .expect("the project");
    make(&mut app, 3);
    let untouched = |app: &TestApp<App>| {
        count(app, |issue| {
            issue.status == Status::Backlog
                && issue.assignee.is_none()
                && issue.area.is_none()
                && issue.comments.is_empty()
        })
    };

    // An issue the tracker does not have, between two it has.
    let why = group(
        &mut app,
        vec![
            member("place", json!({ "issue": "#1", "status": "done" })),
            member("place", json!({ "issue": "#9", "status": "done" })),
            member("place", json!({ "issue": "#2", "status": "done" })),
        ],
    )
    .expect_err("refused");
    assert!(why.contains("#9"), "{why}");
    assert_eq!(untouched(&app), 3, "none of the group was placed");

    // What the action on one issue refuses refuses the group, naming the
    // issue.
    let why = group(
        &mut app,
        vec![
            member(
                "assign",
                json!({ "issue": "#1", "assignee": "agent:scribe" }),
            ),
            member("comment", json!({ "issue": "#2", "text": "Seen" })),
            member(
                "assign",
                json!({ "issue": "#3", "assignee": "person:nobody" }),
            ),
        ],
    )
    .expect_err("refused");
    assert!(why.contains("#3 Issue 2"), "{why}");
    assert_eq!(untouched(&app), 3, "none of the group was written");

    let why = group(
        &mut app,
        vec![
            member("set-project", json!({ "issue": "#1", "project": "Launch" })),
            member(
                "set-project",
                json!({ "issue": "#2", "project": "No such project" }),
            ),
        ],
    )
    .expect_err("refused");
    assert!(why.contains("No such project"), "{why}");
    assert_eq!(untouched(&app), 3, "none of the group was moved");

    let why = group(
        &mut app,
        vec![
            member("comment", json!({ "issue": "#1", "text": "Short" })),
            member(
                "comment",
                json!({ "issue": "#2", "text": "x".repeat(tasks::model::MAX_MESSAGE_CHARS + 1) }),
            ),
        ],
    )
    .expect_err("refused");
    assert!(why.contains("#2 Issue 1"), "{why}");
    assert_eq!(untouched(&app), 3, "none of the group was said");

    // A member that is no action of the tracker, or lacks an argument.
    let why = group(
        &mut app,
        vec![
            member("place", json!({ "issue": "#1", "status": "done" })),
            member("archive", json!({ "issue": "#2" })),
        ],
    )
    .expect_err("refused");
    assert!(
        why.contains("item 1") || why.contains("member 1"),
        "the refusal says which: {why}"
    );
    let why = group(
        &mut app,
        vec![
            member("place", json!({ "issue": "#1", "status": "done" })),
            member("place", json!({ "issue": "#2" })),
        ],
    )
    .expect_err("refused");
    assert!(
        why.contains("item 1") || why.contains("member 1"),
        "the refusal says which: {why}"
    );
    assert_eq!(untouched(&app), 3);
}

#[test]
fn several_issues_in_one_call_are_what_their_caller_may_do_one_at_a_time() {
    let mut app = open();
    app.call("create-area", json!({ "name": "Launch" }))
        .expect("the project");
    let ids = make(&mut app, 3);
    let first = issue_resource(&ids[0]);
    let one_more = json!({ "issues": [{ "title": "One more" }] });
    let places = || {
        vec![
            member("place", json!({ "issue": "#1", "status": "done" })),
            member("place", json!({ "issue": "#2", "status": "done" })),
        ]
    };
    let comments = || {
        vec![
            member("comment", json!({ "issue": "#1", "text": "Seen" })),
            member("comment", json!({ "issue": "#2", "text": "Seen too" })),
        ]
    };
    let unchanged = |app: &TestApp<App>| {
        held(app).len() == 3
            && count(app, |issue| {
                issue.status == Status::Backlog
                    && issue.assignee.is_none()
                    && issue.area.is_none()
                    && issue.comments.is_empty()
            }) == 3
    };

    // Someone who may only read changes nothing, one issue at a time or
    // several.
    app.set_role(ui::Role::Viewer);
    assert!(
        app.call_on(&first, "place", json!({ "status": "done" }))
            .is_err()
    );
    assert!(
        app.call_on(&first, "comment", json!({ "text": "Seen" }))
            .is_err()
    );
    assert!(group(&mut app, places()).is_err());
    assert!(group(&mut app, comments()).is_err());
    assert!(
        app.call_on(&chain(), "create-many", one_more.clone())
            .is_err()
    );
    assert!(unchanged(&app), "what a viewer was refused changed nothing");

    // Someone who may only comment changes nothing else of an issue, one at a
    // time or several: a group that also moves an issue is refused whole,
    // its comments too.
    app.set_role(ui::Role::Commenter);
    assert!(
        app.call_on(&first, "place", json!({ "status": "done" }))
            .is_err()
    );
    assert!(group(&mut app, places()).is_err());
    let mut mixed = comments();
    mixed.push(member("place", json!({ "issue": "#3", "status": "done" })));
    assert!(group(&mut app, mixed).is_err());
    assert!(
        app.call_on(&chain(), "create-many", one_more.clone())
            .is_err()
    );
    assert!(
        unchanged(&app),
        "what a commenter was refused changed nothing"
    );
    // A group is the edits it carries: someone who may comment comments on
    // several issues in one call, as they comment on one.
    group(&mut app, comments()).expect("a commenter comments on several issues");
    assert_eq!(count(&app, |issue| issue.comments.len() == 1), 2);
    assert_eq!(
        count(&app, |issue| issue.status == Status::Backlog
            && issue.assignee.is_none()),
        3,
        "nothing but the comments was written"
    );

    // Someone who may edit does all of it.
    app.set_role(ui::Role::Editor);
    group(&mut app, places()).expect("an editor places several issues");
    app.call_on(&chain(), "create-many", one_more)
        .expect("an editor makes several issues");
    assert_eq!(held(&app).len(), 4);
    assert_eq!(count(&app, |issue| issue.status == Status::Done), 2);
}

#[test]
fn a_group_reaches_issues_kept_in_two_parts_and_is_refused_for_both() {
    ui::i18n::I18nManager::init("en-US");
    // Parts of 500 issues, and the first one full: what is made next
    // starts the second.
    let _parts = ui::framework::PartLimits::scoped(500, usize::MAX);
    let before = 500;
    let mut app = TestApp::<App>::drive(&format!("issues-{before}")).expect("open");
    // The first part is full: three more issues start the next.
    app.call_on(
        &chain(),
        "create-many",
        json!({ "issues": [
            { "title": "Past the first part" },
            { "title": "And another" },
            { "title": "One on its own" },
        ] }),
    )
    .expect("the issues are made");
    let parts = app.chain_parts::<IssueList>();
    assert_eq!(parts.len(), 2);

    // One call changes an issue of each part.
    let old = parts[0].1.issues.ids().next().expect("an issue").clone();
    let new = parts[1].1.issues.ids().next().expect("an issue").clone();
    let read = |app: &TestApp<App>, id: &Id| held(app).get(id).unwrap();
    let (old_was, new_was) = (read(&app, &old).status, read(&app, &new).status);
    assert_ne!(old_was, Status::Cancelled);
    assert_ne!(new_was, Status::Cancelled);
    let call_off = |issue: &Id| {
        member(
            "place",
            json!({ "issue": issue_resource(issue), "status": "cancelled" }),
        )
    };

    // Refused whole when one member cannot be, whichever part it names:
    // neither part is written.
    let why = group(
        &mut app,
        vec![
            call_off(&old),
            call_off(&new),
            member(
                "place",
                json!({ "issue": "tasks.issues/issues/none", "status": "cancelled" }),
            ),
        ],
    )
    .expect_err("refused");
    assert!(why.contains("none"), "{why}");
    let why = group(
        &mut app,
        vec![
            call_off(&old),
            member(
                "assign",
                json!({ "issue": issue_resource(&new), "assignee": "person:nobody" }),
            ),
        ],
    )
    .expect_err("refused");
    assert!(why.contains("Past the first part"), "{why}");
    assert_eq!(read(&app, &old).status, old_was);
    assert_eq!(read(&app, &new).status, new_was);
    assert!(read(&app, &new).assignee.is_none());

    group(
        &mut app,
        vec![
            call_off(&old),
            call_off(&new),
            member(
                "comment",
                json!({ "issue": issue_resource(&new), "text": "Called off" }),
            ),
            member(
                "comment",
                json!({ "issue": issue_resource(&old), "text": "Called off too" }),
            ),
        ],
    )
    .expect("one call for issues of two parts");
    for issue in [&old, &new] {
        assert_eq!(read(&app, issue).status, Status::Cancelled);
        assert_eq!(read(&app, issue).comments.len(), 1);
    }

    // A part is the unit of history: the call was one step in each part,
    // and each is taken back whole.
    undo(&mut app, 1);
    assert_eq!(read(&app, &new).status, new_was);
    assert!(read(&app, &new).comments.is_empty());
    assert_eq!(read(&app, &old).status, Status::Cancelled);
    undo(&mut app, 0);
    assert_eq!(read(&app, &old).status, old_was);
    assert!(read(&app, &old).comments.is_empty());
}

/// One group creates a parent and a child under it: the child names the
/// parent by the place it is made at, `{"$made": {"member": 0, "item": 0}}`,
/// and the framework fills in the id the first member mints.
#[test]
fn a_group_creates_a_parent_and_a_child_under_it() {
    let mut app = open();
    group(
        &mut app,
        vec![
            member("create", json!({ "title": "Parent" })),
            member(
                "create",
                json!({ "title": "Child", "parent": { "$made": { "member": 0, "item": 0 } } }),
            ),
        ],
    )
    .expect("the parent and its child are made");
    let issues: Vec<(Id, Issue)> = held(&app).iter().collect();
    let parent = issues
        .iter()
        .find(|(_, issue)| issue.title == "Parent")
        .expect("the parent");
    let child = issues
        .iter()
        .find(|(_, issue)| issue.title == "Child")
        .expect("the child");
    assert_eq!(
        child.1.parent.as_ref().map(Id::as_str),
        Some(parent.0.as_str()),
        "the child stands under the issue the same group made"
    );
}

#[test]
fn issues_made_in_one_group_stand_in_the_order_they_were_given() {
    let mut app = open();
    make(&mut app, 2);
    let creates = (0..5)
        .map(|at| member("create", json!({ "title": format!("New {at}") })))
        .collect();
    group(&mut app, creates).expect("the group is applied");

    let mut made: Vec<(u64, String)> = held(&app)
        .values()
        .map(|issue| (issue.number, issue.rank))
        .collect();
    made.sort();
    assert_eq!(made.len(), 7);
    assert!(
        made.windows(2).all(|pair| pair[0].1 < pair[1].1),
        "each issue goes after the one made before it: {made:?}"
    );
}
