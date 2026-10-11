//! An agent collaborates in Notes through its actions alone: it creates a
//! page, finds it by title, reads it as Markdown with block ids, turns the
//! list into a checklist, ticks items, rewords one and re-reads the page. No
//! click, no focus and no window: the same door accessibility and automation
//! use.

use notes::NotesShell;
use notes::doc::NotesDoc;
use serde_json::{Value, json};
use ui::framework::testing::TestApp;

fn call(app: &mut TestApp<NotesShell>, resource: &str, action: &str, arguments: Value) -> Value {
    app.call_on(resource, action, arguments)
        .unwrap_or_else(|error| panic!("{action} on {resource} refused: {error:?}"))
        .unwrap_or_else(|| panic!("{action} on {resource} answered nothing"))
}

fn read(app: &mut TestApp<NotesShell>, page: &str) -> Value {
    call(app, "notes.notebook", "read", json!({ "item": page }))
}

/// The page body as lines, ids left off.
fn lines(note: &Value) -> Vec<String> {
    note["markdown"]
        .as_str()
        .expect("the body as Markdown")
        .lines()
        .map(|line| {
            line.rsplit_once(" ^")
                .map_or(line, |(text, _)| text)
                .to_owned()
        })
        .collect()
}

/// The id at the end of the line that starts with `prefix`.
fn id_of(note: &Value, prefix: &str) -> String {
    note["markdown"]
        .as_str()
        .expect("the body as Markdown")
        .lines()
        .find(|line| line.starts_with(prefix))
        .and_then(|line| line.rsplit_once(" ^"))
        .map(|(_, id)| id.to_owned())
        .unwrap_or_else(|| panic!("no line starts with {prefix:?}: {note}"))
}

fn ids(note: &Value) -> Vec<String> {
    note["markdown"]
        .as_str()
        .expect("the body as Markdown")
        .lines()
        .filter_map(|line| line.rsplit_once(" ^").map(|(_, id)| id.to_owned()))
        .collect()
}

#[test]
fn an_agent_turns_the_todo_page_into_a_checklist_ticks_items_and_rewords_one() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    app.call("new-page", json!({ "value": "Groceries" }))
        .unwrap();

    // A page with a title and a Markdown body.
    let created = call(
        &mut app,
        "notes.notebook",
        "create-page",
        json!({ "title": "Todo", "markdown": "# Todo\nbuy milk\nbuy eggs\ncall mum" }),
    );
    let resource = created["resource"]
        .as_str()
        .expect("the page's resource")
        .to_owned();
    assert!(created["page"].is_string(), "{created}");

    // Found by title, exact matches first; the other page is not it.
    let found = call(
        &mut app,
        "notes.notebook",
        "find-page",
        json!({ "title": "todo" }),
    );
    assert_eq!(found["pages"].as_array().unwrap().len(), 1, "{found}");
    assert_eq!(found["pages"][0]["resource"], resource.as_str());
    assert_eq!(found["pages"][0]["title"], "Todo");

    // Read as Markdown, an id on every line.
    let note = read(&mut app, &resource);
    assert_eq!(lines(&note), ["buy milk", "buy eggs", "call mum"]);
    let first_read = ids(&note);
    assert_eq!(first_read.len(), 3, "{note}");
    let body = note["body"].as_str().expect("the body resource").to_owned();

    // The list becomes a checklist by writing each line back, ids kept: the
    // same three blocks, not three new ones.
    let checklist: Vec<String> = note["markdown"]
        .as_str()
        .unwrap()
        .lines()
        .map(|line| format!("- [ ] {line}"))
        .collect();
    let answer = call(
        &mut app,
        &body,
        "replace-body",
        json!({ "markdown": checklist.join("\n") }),
    );
    assert_eq!(answer["changed"].as_array().unwrap().len(), 3, "{answer}");
    assert!(
        answer.get("created").is_none() && answer.get("removed").is_none(),
        "{answer}"
    );
    let note = read(&mut app, &resource);
    assert_eq!(
        lines(&note),
        ["- [ ] buy milk", "- [ ] buy eggs", "- [ ] call mum"]
    );
    assert_eq!(
        ids(&note),
        first_read,
        "the blocks are the ones that were there"
    );

    // Tick one, reword one, add one.
    let milk = id_of(&note, "- [ ] buy milk");
    let eggs = id_of(&note, "- [ ] buy eggs");
    let ticked = call(
        &mut app,
        &body,
        "set-block-done",
        json!({ "block": milk, "done": true }),
    );
    assert_eq!(ticked, json!({ "changed": [milk] }));
    call(
        &mut app,
        &body,
        "set-block-text",
        json!({ "block": eggs, "text": "buy a dozen eggs" }),
    );
    let added = call(
        &mut app,
        &body,
        "insert-block",
        json!({ "kind": "checklist", "text": "water the plants", "position": "end" }),
    );
    assert_eq!(added["created"].as_array().unwrap().len(), 1, "{added}");

    // Find text returns the block to edit by.
    let hits = call(
        &mut app,
        "notes.notebook",
        "find-text",
        json!({ "text": "DOZEN" }),
    );
    assert_eq!(hits["matches"].as_array().unwrap().len(), 1, "{hits}");
    assert_eq!(hits["matches"][0]["block"], eggs.as_str());
    assert_eq!(hits["matches"][0]["title"], "Todo");

    // Re-read.
    let note = read(&mut app, &resource);
    assert_eq!(
        lines(&note),
        [
            "- [x] buy milk",
            "- [ ] buy a dozen eggs",
            "- [ ] call mum",
            "- [ ] water the plants"
        ]
    );
    assert_eq!(
        ids(&note)[..3],
        first_read[..],
        "ticking and rewording kept the blocks"
    );

    // A page can be renamed and found by its new title.
    app.call_on(&resource, "rename", json!({ "value": "Todo today" }))
        .unwrap();
    let found = call(
        &mut app,
        "notes.notebook",
        "find-page",
        json!({ "title": "today" }),
    );
    assert_eq!(found["pages"][0]["title"], "Todo today");

    // Every edit was one undo step: the last one goes, the page holds the rest.
    app.call("undo", json!({})).unwrap();
    app.call("undo", json!({})).unwrap();
    app.settle();
    let note = read(&mut app, &resource);
    assert_eq!(
        lines(&note),
        ["- [x] buy milk", "- [ ] buy a dozen eggs", "- [ ] call mum"],
        "undo takes back the rename and the added item, one step each"
    );
    let titles = app.read_document(|doc| {
        NotesDoc::list_pages(doc)
            .into_iter()
            .map(|page| page.title)
            .collect::<Vec<_>>()
    });
    assert!(titles.contains(&"Todo".to_owned()), "{titles:?}");
}

#[test]
fn a_bad_block_is_refused_by_name_and_writes_nothing() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    let created = call(
        &mut app,
        "notes.notebook",
        "create-page",
        json!({ "title": "Todo", "markdown": "one" }),
    );
    let resource = created["resource"].as_str().unwrap().to_owned();
    let note = read(&mut app, &resource);
    let body = note["body"].as_str().unwrap().to_owned();
    let before = app.read_document(|doc| doc.clone().get_heads());
    let refused = app
        .call_on(&body, "set-block-done", json!({ "block": "nope" }))
        .expect_err("an unknown block");
    assert!(format!("{refused:?}").contains("no block"), "{refused:?}");
    assert_eq!(app.read_document(|doc| doc.clone().get_heads()), before);
}
