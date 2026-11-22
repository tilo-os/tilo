//! A list of new issues in one call, through the action Tasks publishes and
//! with no window used: `create-many`, where an issue may stand under
//! another issue of the same list.
//!
//! The list is one step of the part it goes to, is refused whole with the
//! entry named when one of its issues cannot be made, and makes nothing a
//! call for one issue could not.
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
use tasks::model::{Issue, Status, issue_resource};
use tasks::{App, IssueList};
use ui::prelude::*;

const DAY_MS: i64 = 86_400_000;

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

/// Take back the caller's last step in part `part`, as the chain's own
/// `undo` does for whoever calls it.
fn undo(app: &mut TestApp<App>, part: u64) {
    app.call_on(&chain(), "undo", json!({ "part": part }))
        .expect("the step is taken back");
}

/// A list of new issues that is refused, as its caller reads the refusal.
fn refused_list(app: &mut TestApp<App>, issues: Value) -> String {
    let mut arguments = serde_json::Map::new();
    arguments.insert("issues".into(), issues);
    let error = app
        .call_on(&chain(), "create-many", Value::Object(arguments))
        .expect_err("the list is refused");
    format!("{error:?}")
}

/// Forty new issues as one list, fifteen of them sub-issues of issues of
/// the same list.
fn forty() -> Vec<Value> {
    // Ten pieces of work that are wanted a day apart; ten steps named by
    // their piece's key and five by its place in the list; fifteen more on
    // their own.
    let mut issues = Vec::new();
    for piece in 0..10_i64 {
        issues.push(json!({
            "key": format!("piece-{piece}"),
            "title": format!("Piece {piece}"),
            "project": "Launch",
            "due_ms": DAY_MS.saturating_mul(piece.saturating_add(1)),
        }));
    }
    for piece in 0..5 {
        for step in 0..2 {
            issues.push(json!({
                "title": format!("Piece {piece}, step {step}"),
                "under": format!("piece-{piece}"),
            }));
        }
    }
    for step in 2..7 {
        issues.push(json!({ "title": format!("Piece 0, step {step}"), "under": "1" }));
    }
    for other in 0..15 {
        issues.push(json!({
            "title": format!("Other {other}"),
            "status": "todo",
            "labels": ["later"],
        }));
    }
    issues
}

#[test]
fn forty_issues_of_which_fifteen_are_sub_issues_are_one_call_and_one_undo_step() {
    let mut app = open();
    app.call("create-area", json!({ "name": "Launch" }))
        .expect("the project");
    let issues = forty();
    assert_eq!(issues.len(), 40);
    let titles: Vec<String> = issues
        .iter()
        .map(|issue| issue["title"].as_str().unwrap().to_owned())
        .collect();

    let made = app
        .call_on_output(&chain(), "create-many", json!({ "issues": issues }))
        .expect("one call makes them all")
        .expect("the call answers");
    assert_eq!(made.resources.len(), 40, "it answers every issue it made");

    let now = held(&app);
    assert_eq!(now.len(), 40);
    let listed: Vec<(Id, Issue)> = now.iter().collect();
    assert_eq!(
        listed
            .iter()
            .map(|(_, issue)| issue.title.clone())
            .collect::<Vec<_>>(),
        titles,
        "the tracker's order is the list's"
    );
    for (at, (id, issue)) in listed.iter().enumerate() {
        assert_eq!(
            issue.number,
            u64::try_from(at).unwrap().saturating_add(1),
            "numbered in the order given"
        );
        assert_eq!(
            made.resources[at],
            issue_resource(id),
            "answered in the order given"
        );
    }
    let piece = |piece: usize| {
        listed
            .iter()
            .find(|(_, issue)| issue.title == format!("Piece {piece}"))
            .map(|(id, _)| id.clone())
            .expect("the piece")
    };
    let under = |parent: &Id| {
        listed
            .iter()
            .filter(|(_, issue)| issue.parent.as_ref() == Some(parent))
            .count()
    };
    assert_eq!(under(&piece(0)), 7, "two by its key and five by its place");
    for other in 1..5 {
        assert_eq!(under(&piece(other)), 2);
    }
    assert_eq!(count(&app, |issue| issue.parent.is_some()), 15);
    assert_eq!(
        count(&app, |issue| issue
            .area
            .as_ref()
            .is_some_and(|area| area.label == "Launch")),
        10
    );
    assert_eq!(count(&app, |issue| issue.due_ms.is_some()), 10);
    assert_eq!(count(&app, |issue| issue.status == Status::Todo), 15);
    assert_eq!(
        count(&app, |issue| issue.labels.contains(&"later".to_owned())),
        15
    );
    assert!(
        listed
            .iter()
            .all(|(_, issue)| issue.author == Some(ui::tilo_resource::MemberId::person("ada"))),
        "each is its caller's, as one made alone is"
    );

    // One step of the part's history: one undo takes all forty back.
    undo(&mut app, 0);
    assert!(held(&app).is_empty(), "taken back whole");
}

#[test]
fn a_list_of_new_issues_with_one_that_cannot_be_made_is_refused_whole_naming_it() {
    let mut app = open();
    app.call_on(&chain(), "create", json!({ "title": "Already here" }))
        .expect("create");
    let mut refused = |issues: Value| {
        let why = refused_list(&mut app, issues);
        assert_eq!(held(&app).len(), 1, "nothing of a refused list is made");
        why
    };

    // Under an issue that comes later in the list, by its key and by its
    // place.
    let why = refused(json!([
        { "title": "Step", "under": "piece" },
        { "key": "piece", "title": "Piece" },
    ]));
    assert!(
        why.contains("issue 1 in the list (Step)") && why.contains("first"),
        "{why}"
    );
    let why = refused(json!([{ "title": "Step", "under": "2" }, { "title": "Piece" }]));
    assert!(why.contains("issue 1 in the list (Step)"), "{why}");

    // Under an issue the list does not have, and under itself.
    let why = refused(json!([{ "title": "Piece" }, { "title": "Step", "under": "nobody" }]));
    assert!(
        why.contains("issue 2 in the list (Step)") && why.contains("nobody"),
        "{why}"
    );
    let why = refused(json!([{ "key": "self", "title": "Loop", "under": "self" }]));
    assert!(why.contains("issue 1 in the list (Loop)"), "{why}");

    // Under an issue the tracker does not have.
    let why = refused(json!([{ "title": "Piece" }, { "title": "Step", "parent": "Not here" }]));
    assert!(
        why.contains("issue 2 in the list (Step)") && why.contains("Not here"),
        "{why}"
    );

    // What `create` refuses of one issue refuses the list, naming it.
    let why = refused(json!([
        { "title": "Piece" },
        { "title": "Fine" },
        { "title": "Too much", "estimate": 5000 },
    ]));
    assert!(why.contains("issue 3 in the list (Too much)"), "{why}");
    let why = refused(json!([{ "title": "Piece", "project": "No such project" }]));
    assert!(why.contains("issue 1 in the list (Piece)"), "{why}");

    // A key given twice, no issues at all, and more than a call takes.
    let why = refused(json!([
        { "key": "piece", "title": "Piece" },
        { "key": "piece", "title": "Again" },
    ]));
    assert!(why.contains("issue 2 in the list (Again)"), "{why}");
    refused(json!([]));
    let too_many: Vec<Value> = (0..=tasks::model::MAX_MANY)
        .map(|at| json!({ "title": format!("Issue {at}") }))
        .collect();
    refused(json!(too_many));

    // An issue of the list goes under one the tracker has already.
    app.call_on(
        &chain(),
        "create-many",
        json!({ "issues": [{ "title": "Step", "parent": "Already here" }] }),
    )
    .expect("a sub-issue of an issue that is there");
    let (parent, _) = held(&app).by_number(1).expect("the first issue");
    let (_, step) = held(&app).by_number(2).expect("the second issue");
    assert_eq!(step.parent, Some(parent));
}

#[test]
fn a_list_of_new_issues_is_made_only_by_who_may_make_one() {
    let mut app = open();
    make(&mut app, 2);
    let one_more = json!({ "issues": [{ "title": "One more" }, { "title": "And another" }] });

    // Someone who may only read, or only comment, makes no issue, one at a
    // time or several.
    for role in [ui::Role::Viewer, ui::Role::Commenter] {
        app.set_role(role);
        assert!(
            app.call_on(&chain(), "create", json!({ "title": "One more" }))
                .is_err(),
            "{role:?} makes no issue"
        );
        assert!(
            app.call_on(&chain(), "create-many", one_more.clone())
                .is_err(),
            "{role:?} makes no list of issues"
        );
        assert_eq!(held(&app).len(), 2, "a refused list made nothing");
    }

    // Someone who may edit makes them.
    app.set_role(ui::Role::Editor);
    app.call_on(&chain(), "create-many", one_more)
        .expect("an editor makes several issues");
    assert_eq!(held(&app).len(), 4);
}

#[test]
fn new_issues_that_do_not_fit_the_newest_part_start_the_next_together() {
    ui::i18n::I18nManager::init("en-US");
    // Parts of 500 issues, and the first one full: what is made next
    // starts the second.
    let _parts = ui::framework::PartLimits::scoped(500, usize::MAX);
    let before = 500;
    let mut app = TestApp::<App>::drive(&format!("issues-{before}")).expect("open");
    assert_eq!(app.chain_parts::<IssueList>().len(), 1);

    // The newest part is full: the list starts the next part together,
    // numbered on from the part before, so it is still one step.
    let made = app
        .call_on_output(
            &chain(),
            "create-many",
            json!({ "issues": [
                { "key": "piece", "title": "Past the first part" },
                { "title": "Its step", "under": "piece" },
                { "title": "One on its own" },
            ] }),
        )
        .expect("one call")
        .expect("the call answers");
    let parts = app.chain_parts::<IssueList>();
    assert_eq!(parts.len(), 2, "the list starts the next part");
    assert_eq!(
        parts[0].1.issues.len(),
        before,
        "none went to the part with too little room"
    );
    assert_eq!(parts[1].1.issues.len(), 3);
    let numbers: Vec<u64> = parts[1]
        .1
        .issues
        .values()
        .map(|issue| issue.number)
        .collect();
    let first = u64::try_from(before).unwrap().saturating_add(1);
    assert_eq!(
        numbers,
        [first, first.saturating_add(1), first.saturating_add(2)]
    );
    assert_eq!(made.resources.len(), 3);
    let (piece, _) = held(&app).by_number(first).expect("the piece");
    let (_, step) = held(&app)
        .by_number(first.saturating_add(1))
        .expect("its step");
    assert_eq!(step.parent, Some(piece));

    undo(&mut app, 1);
    assert_eq!(held(&app).len(), before, "the three were one step");
}

/// The list answers the issues it made: each of its ids and the address it
/// is published under, so a caller needs neither a read nor a split address
/// to name them.
#[test]
fn a_list_answers_the_issues_it_made() {
    let mut app = open();
    let output = app
        .call_on_output(
            &chain(),
            "create-many",
            json!({ "issues": [{"title": "a"}, {"title": "b"}] }),
        )
        .expect("the issues are made")
        .expect("the list answered at once");
    let made = output
        .value
        .as_ref()
        .and_then(|value| value.get("made"))
        .and_then(Value::as_array)
        .expect("the answer names what it made");
    let held = held(&app);
    let ids: Vec<String> = held.ids().iter().map(ToString::to_string).collect();
    assert_eq!(made.len(), 2, "{made:?}");
    for row in made {
        let id = row["id"].as_str().unwrap_or_else(|| panic!("{row}"));
        assert!(
            ids.iter().any(|held| held == id),
            "{row} is not held: {ids:?}"
        );
        let address = row["address"].as_str().unwrap_or_else(|| panic!("{row}"));
        assert!(address.ends_with(id), "{row}");
    }
    assert_eq!(output.resources.len(), 2, "{:?}", output.resources);
}

/// Lists of new issues cost the same however many issues the tracker already holds: each list
/// reads back the bodies it wrote and none of the issues before it, and the lists made once the
/// tracker holds a thousand issues take about as long as the first ones did.
#[test]
fn each_list_of_new_issues_costs_the_same_however_many_came_before() {
    const LIST: usize = 200;
    const LISTS: usize = 7;
    let mut app = open();
    let mut lists = Vec::with_capacity(LISTS);
    let mut sections = Vec::with_capacity(LISTS);
    for list in 0..LISTS {
        let issues: Vec<Value> = (list * LIST..(list + 1) * LIST)
            .map(|at| {
                json!({
                    "title": format!("Issue {at}"),
                    "body": format!("## Issue {at}\n\nWhat it is about, with a step or two.\n"),
                    "labels": ["Migration"],
                })
            })
            .collect();
        let began = std::time::Instant::now();
        let ((), observed) = ui::tilo_resource::profiling::observe(|| {
            app.call_on(&chain(), "create-many", json!({ "issues": issues }))
                .expect("the issues are made");
        });
        let took = began.elapsed();
        let calls = |phase: &str| {
            observed
                .iter()
                .find(|sample| sample.phase == phase)
                .map_or(0, |sample| sample.calls)
        };
        lists.push((took, calls("rich_text.read.written")));
        sections.push(calls("rich_text.read.parts"));
    }
    assert_eq!(held(&app).len(), LIST * LISTS);
    let read: Vec<usize> = lists.iter().map(|(_, bodies)| *bodies).collect();
    assert!(
        read.iter().all(|bodies| *bodies == LIST),
        "each list reads back its own {LIST} bodies, no more: {read:?}"
    );
    assert!(
        sections.iter().all(|sections| *sections == 0),
        "no list reads a body section by section from the document: {sections:?}"
    );
    // The first list also opens the part; the median of three steadies a busy machine.
    let median = |lists: &[(std::time::Duration, usize)]| {
        let mut took: Vec<_> = lists.iter().map(|(took, _)| *took).collect();
        took.sort_unstable();
        took[took.len() / 2]
    };
    let early = median(&lists[1..4]);
    let late = median(&lists[LISTS - 3..]);
    assert!(
        late < early * 3 / 2,
        "the lists after {} issues took {late:?}, the early ones {early:?}: {lists:?}",
        LIST * (LISTS - 3)
    );
}
