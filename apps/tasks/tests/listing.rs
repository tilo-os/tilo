//! Where a tracker stands, read in one call through the read its issues
//! publish, with no window used: each row says what a person scans a list of
//! issues by, a `where` answers only the issues asked for, and a long tracker
//! is read a page at a time.

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
use tasks::{App, IssuePart};
use ui::prelude::*;

const DAY_MS: i64 = 86_400_000;

fn open() -> TestApp<App> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<App>::for_person("ada").expect("open")
}

/// One read of the tracker's issues.
fn read(app: &mut TestApp<App>, arguments: Value) -> Value {
    app.call_on(&chain(), "read", arguments)
        .expect("the tracker reads")
        .expect("the read answers")
}

/// A read that is refused, as its caller reads the refusal.
fn refused(app: &mut TestApp<App>, arguments: Value) -> String {
    let error = app
        .call_on(&chain(), "read", arguments)
        .expect_err("the read is refused");
    format!("{error:?}")
}

fn rows(page: &Value) -> Vec<Value> {
    page["issues"].as_array().cloned().unwrap_or_default()
}

/// The issues a page answers, by name, in its order.
fn names(page: &Value) -> Vec<String> {
    rows(page)
        .iter()
        .map(|row| {
            row["name"]
                .as_str()
                .expect("a row names its issue")
                .to_owned()
        })
        .collect()
}

/// The issues a `where` answers, by name, in the order the read gives them.
fn asked(app: &mut TestApp<App>, filter: Value) -> Vec<String> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("where".into(), filter);
    names(&read(app, Value::Object(arguments)))
}

/// Two projects: one with two pieces of work and their steps, moved, dated,
/// assigned and labelled so each row has something to say, one with a single
/// issue; and an issue that works towards neither.
fn tracker(app: &mut TestApp<App>) {
    for project in ["Launch", "Hiring"] {
        app.call("create-area", json!({ "name": project }))
            .expect("the project");
    }
    app.call_on(
        &chain(),
        "create-many",
        json!({ "issues": [
            {
                "key": "notes",
                "title": "Write the notes",
                "project": "Launch",
                "status": "in-progress",
                "due_ms": DAY_MS,
                "priority": "high",
                "assignee": "agent:scribe",
                "labels": ["launch"],
            },
            { "title": "Outline", "under": "notes", "project": "Launch", "status": "done" },
            {
                "title": "Draft",
                "under": "notes",
                "project": "Launch",
                "status": "in-review",
                "assignee": "agent:scribe",
            },
            { "key": "demo", "title": "Record the demo", "project": "Launch", "status": "todo" },
            { "title": "Script", "under": "demo", "project": "Launch" },
            {
                "title": "Read the applications",
                "project": "Hiring",
                "status": "todo",
                "assignee": "outside:Grace",
            },
            { "title": "Tidy the backlog" },
        ] }),
    )
    .expect("the issues");
}

#[test]
fn one_read_says_where_every_issue_stands_and_who_has_it() {
    let mut app = open();
    tracker(&mut app);
    let page = read(&mut app, json!({}));
    let rows = rows(&page);
    assert_eq!(rows.len(), 7, "{page}");
    assert_eq!(
        names(&page).first().map(String::as_str),
        Some("Tidy the backlog"),
        "the newest first"
    );
    let row = |name: &str| {
        rows.iter()
            .find(|row| row["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("a row for {name}: {page}"))
    };

    // What a person scans a list by: its number, where it stands, who has
    // it and who gave it to them, when it is wanted, how urgent it is, its
    // labels and the project it works towards.
    let notes = row("Write the notes");
    assert_eq!(notes["number"], 1, "{notes}");
    assert_eq!(notes["status"], "in-progress", "{notes}");
    assert_eq!(notes["assignee"], "agent:scribe", "{notes}");
    assert!(!notes["assigned_by"].is_null(), "who gave it: {notes}");
    assert!(
        notes["assigned_ms"].as_i64().is_some_and(|at| at > 0),
        "since when: {notes}"
    );
    assert_eq!(notes["due_ms"], DAY_MS, "{notes}");
    assert_eq!(notes["priority"], 2, "{notes}");
    assert_eq!(notes["labels"], json!(["launch"]), "{notes}");
    assert_eq!(notes["area"]["label"], "Launch", "{notes}");

    // The issue it stands under, as that issue's own address ends.
    let outline = row("Outline");
    assert_eq!(outline["status"], "done");
    let parent = outline["parent"].as_str().expect("its parent");
    assert!(
        notes["item"]
            .as_str()
            .is_some_and(|item| item.ends_with(parent)),
        "{outline} under {notes}"
    );

    // What an issue has no value of its row leaves out, and a row says
    // nothing it was not asked: not the description, not the thread.
    let tidy = row("Tidy the backlog");
    assert_eq!(tidy["status"], "backlog");
    for absent in [
        "assignee",
        "assigned_by",
        "parent",
        "due_ms",
        "priority",
        "area",
    ] {
        assert!(tidy.get(absent).is_none(), "{absent} in {tidy}");
    }
    for unasked in ["body", "comments", "title", "links"] {
        assert!(notes.get(unasked).is_none(), "{unasked} in {notes}");
    }
}

#[test]
fn a_row_says_what_an_issue_waits_for_and_since_when() {
    let mut app = open();
    tracker(&mut app);
    let (notes, _) = held(&app).by_number(1).expect("the first issue");
    let until: i64 = 4_102_444_800_000;
    app.call_on(
        &tasks::model::issue_resource(&notes),
        "wait",
        json!({
            "waits": [{ "kind": "reply", "what": "a reply from the supplier" }],
            "until_ms": until,
        }),
    )
    .expect("the issue waits");

    // What long work keeps on an issue comes with its row, as the issue's
    // own read answers it: nothing more to read to see what waits.
    let page = read(&mut app, json!({}));
    let row = rows(&page)
        .into_iter()
        .find(|row| row["name"] == "Write the notes")
        .expect("its row");
    let work = &row["work"];
    assert_eq!(
        work["waits"][0]["what"], "a reply from the supplier",
        "{row}"
    );
    assert_eq!(work["until_ms"], until, "{row}");
    assert!(work["since_ms"].as_i64().is_some_and(|at| at > 0), "{row}");
    // An issue no work was kept on says nothing of it.
    let tidy = rows(&page)
        .into_iter()
        .find(|row| row["name"] == "Tidy the backlog")
        .expect("its row");
    assert!(tidy.get("work").is_none(), "{tidy}");

    // And the issues that wait until then are asked for as such.
    assert_eq!(
        asked(&mut app, json!({ "work.until_ms": until })),
        ["Write the notes"]
    );
}

#[test]
fn a_where_answers_only_the_issues_asked_for() {
    let mut app = open();
    tracker(&mut app);

    // A project by its name, and the issues of no project.
    assert_eq!(
        asked(&mut app, json!({ "project": "Hiring" })),
        ["Read the applications"]
    );
    assert_eq!(asked(&mut app, json!({ "project": "Launch" })).len(), 5);
    assert_eq!(
        asked(&mut app, json!({ "project": null })),
        ["Tidy the backlog"]
    );

    // A status, and any of several.
    assert_eq!(
        asked(&mut app, json!({ "status": "todo" })),
        ["Read the applications", "Record the demo"]
    );
    assert_eq!(
        asked(&mut app, json!({ "status": ["todo", "backlog"] })),
        [
            "Tidy the backlog",
            "Read the applications",
            "Script",
            "Record the demo"
        ]
    );

    // Whose it is, a label it carries, and the issue it stands under, by
    // number and by name.
    assert_eq!(
        asked(&mut app, json!({ "assignee": "agent:scribe" })),
        ["Draft", "Write the notes"]
    );
    assert_eq!(
        asked(&mut app, json!({ "labels": "launch" })),
        ["Write the notes"]
    );
    assert_eq!(
        asked(&mut app, json!({ "parent": "#1" })),
        ["Draft", "Outline"]
    );
    assert_eq!(
        asked(&mut app, json!({ "parent": "Record the demo" })),
        ["Script"]
    );

    // Several at once: every one holds.
    assert_eq!(
        asked(
            &mut app,
            json!({ "project": "Launch", "status": ["in-review", "done"] })
        ),
        ["Draft", "Outline"]
    );
    assert_eq!(
        asked(
            &mut app,
            json!({ "assignee": "agent:scribe", "status": "in-review" })
        ),
        ["Draft"]
    );
    assert!(asked(&mut app, json!({ "status": "cancelled" })).is_empty());

    // What names nothing is refused with what there is, never answered as
    // nothing.
    let why = refused(&mut app, json!({ "where": { "project": "Lunch" } }));
    assert!(why.contains("Lunch") && why.contains("Launch"), "{why}");
    let why = refused(&mut app, json!({ "where": { "stauts": "todo" } }));
    assert!(why.contains("stauts") && why.contains("status"), "{why}");
    let why = refused(&mut app, json!({ "where": { "parent": "#99" } }));
    assert!(why.contains("#99"), "{why}");
}

#[test]
fn a_caller_names_the_fields_a_row_carries() {
    let mut app = open();
    tracker(&mut app);
    let keys = |row: &Value| -> BTreeSet<String> {
        row.as_object().expect("a row").keys().cloned().collect()
    };

    let page = read(
        &mut app,
        json!({ "fields": ["title", "status"], "where": { "status": "done" } }),
    );
    let rows = rows(&page);
    assert_eq!(rows.len(), 1, "{page}");
    assert_eq!(
        keys(&rows[0]),
        ["item", "name", "status", "title"]
            .map(str::to_owned)
            .into_iter()
            .collect()
    );
    assert_eq!(rows[0]["title"], "Outline");

    // No fields: a row names its issue and nothing else.
    let page = read(&mut app, json!({ "fields": [] }));
    for row in &crate::rows(&page) {
        assert_eq!(
            keys(row),
            ["item", "name"].map(str::to_owned).into_iter().collect()
        );
    }

    // A field the issues do not have is refused with the ones they have.
    let why = refused(&mut app, json!({ "fields": ["status", "owner"] }));
    assert!(why.contains("owner") && why.contains("assignee"), "{why}");
}

#[test]
fn a_long_tracker_is_read_a_page_at_a_time_also_with_a_where() {
    let mut app = open();
    let issues: Vec<Value> = (0..30)
        .map(|at| {
            json!({
                "title": format!("Issue {at}"),
                "status": if at % 3 == 0 { "todo" } else { "backlog" },
            })
        })
        .collect();
    app.call_on(&chain(), "create-many", json!({ "issues": issues }))
        .expect("the issues");

    let mut cursor: Option<String> = None;
    let mut seen = Vec::new();
    let mut pages = 0;
    loop {
        let mut arguments = json!({ "where": { "status": "todo" }, "pageSize": 4 });
        if let Some(cursor) = &cursor {
            arguments["cursor"] = json!(cursor);
        }
        let page = read(&mut app, arguments);
        let got = names(&page);
        assert!(
            got.len() <= 4,
            "a page holds at most what it was asked: {page}"
        );
        seen.extend(got);
        pages += 1;
        match page["nextCursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
        assert!(pages < 20, "the read ends");
    }
    let expected: Vec<String> = (0..30)
        .rev()
        .filter(|at| at % 3 == 0)
        .map(|at| format!("Issue {at}"))
        .collect();
    assert_eq!(
        seen, expected,
        "every one once, newest first, over {pages} pages"
    );
    assert!(pages >= 3);
}

#[test]
fn the_read_goes_through_every_part_of_a_tracker() {
    ui::i18n::I18nManager::init("en-US");
    let before = tasks::part::PART_ISSUES - 1;
    let mut app = TestApp::<App>::drive(&format!("issues-{before}")).expect("open");
    app.call_on(
        &chain(),
        "create-many",
        json!({ "issues": [
            { "title": "Past the first part" },
            { "title": "And another" },
            { "title": "One on its own" },
        ] }),
    )
    .expect("the issues");
    let parts = app.chain_parts::<IssuePart>();
    assert_eq!(parts.len(), 2);
    let total = held(&app).len();

    // Every issue of both parts, a page at a time, the newest part first.
    let started = std::time::Instant::now();
    let mut cursor: Option<String> = None;
    let mut seen = BTreeSet::new();
    let mut first = None;
    let mut reads = 0;
    loop {
        let mut arguments = json!({ "pageSize": 200 });
        if let Some(cursor) = &cursor {
            arguments["cursor"] = json!(cursor);
        }
        let page = read(&mut app, arguments);
        reads += 1;
        for row in rows(&page) {
            first.get_or_insert_with(|| row["name"].as_str().unwrap().to_owned());
            assert!(row.get("status").is_some(), "{row}");
            assert!(
                seen.insert(row["item"].as_str().unwrap().to_owned()),
                "an issue is answered once"
            );
        }
        match page["nextCursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
        assert!(reads < 10, "the read ends");
    }
    println!(
        "{total} issues in two parts read in {reads} reads, {} ms",
        started.elapsed().as_millis()
    );
    assert_eq!(seen.len(), total);
    assert_eq!(first.as_deref(), Some("One on its own"));
    assert!(
        reads <= 4,
        "{total} issues at 200 a page took {reads} reads"
    );

    // A where finds the issues it asks for in whichever part keeps them.
    assert!(asked(&mut app, json!({ "status": "cancelled" })).is_empty());
    let old = parts[0].1.issues.ids().next().expect("an issue").clone();
    let new = parts[1].1.issues.ids().next().expect("an issue").clone();
    for issue in [&old, &new] {
        app.call_on(
            &tasks::model::issue_resource(issue),
            "place",
            json!({ "status": "cancelled" }),
        )
        .expect("called off");
    }
    let page = read(&mut app, json!({ "where": { "status": "cancelled" } }));
    let found: BTreeSet<String> = rows(&page)
        .iter()
        .map(|row| row["item"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        found,
        [&old, &new]
            .map(tasks::model::issue_resource)
            .into_iter()
            .collect()
    );
}
