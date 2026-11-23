//! A heading in a note's body: the line's kind and level, made by the format menu or typed as
//! `## `, which the Markdown export writes as the `## ` it is, and whose words search reads as
//! text.

use crate::fixtures::Workspace;
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::{Actor, InvokeOutcome, ResourceRef, Value};

use crate::fixtures::{Replica, field_id};

const DISCUSSION: (&str, &str) = (
    include_str!("../../../../apps/tl/discussion/app.tl"),
    include_str!("../../../../apps/tl/discussion/app.ids"),
);
const NOTES: (&str, &str) = (
    include_str!("../../../../apps/tl/notes/app.tl"),
    include_str!("../../../../apps/tl/notes/app.ids"),
);
const APP: &str = "so.tilo.notes";

fn packages() -> [(&'static str, &'static str); 2] {
    [DISCUSSION, NOTES]
}

/// Ada's workspace with its notebook.
fn workspace() -> (Replica, ResourceRef) {
    let (ada, _loaded) = Replica::new("ada", Workspace::new(), &packages());
    ada.host.set_clock(|| "2026-11-04T09:00:00Z".to_owned());
    let notebook = ada.host.root_of(APP).expect("the notebook");
    (ada, notebook)
}

/// What `action` of the notes app answered, run by `r`'s person.
fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    let id = r
        .host
        .package(APP)
        .expect("the notes package")
        .meta
        .actions
        .iter()
        .find(|held| held.name == action)
        .unwrap_or_else(|| panic!("the action `{action}`"))
        .id
        .clone();
    let who = Actor::from(tilolang_tilo::runtime::ids::MemberId::person(
        r.name.clone(),
    ));
    match r.host.runtime().invoke_action(&who, (id.as_str(), args)) {
        Ok(InvokeOutcome::Done(value)) => value,
        other => panic!("`{action}` answered {other:?}"),
    }
}

/// A note called `title` at the top of the tree, its body saying `body`.
fn noted(r: &Replica, notebook: &ResourceRef, title: &str, body: &str) -> ResourceRef {
    let made = match call(r, "new_note", vec![Value::resource(notebook)]) {
        Value::Ref(at) => (*at.target).clone(),
        other => panic!("a note: {other:?}"),
    };
    call(
        r,
        "retitle",
        vec![Value::resource(&made), Value::text(title)],
    );
    splice(r, &made, 0, 0, body);
    made
}

/// The blocks of `note`'s body as its field holds them.
fn blocks(r: &Replica, note: &ResourceRef) -> Vec<tilo_doc::snapshot::BlockView> {
    match r.host.runtime().read(note, "body") {
        Ok(Value::Rich(rich)) => rich.blocks,
        other => panic!("a note's body: {other:?}"),
    }
}

/// What the line at `block` of `note`'s body is, as the body keeps it: its kind and its heading
/// level.
fn line(r: &Replica, note: &ResourceRef) -> (String, Option<i64>) {
    let block = blocks(r, note).into_iter().next().expect("one line");
    (block.kind.clone(), block.int_attr("level"))
}

/// Write `blocks` as `note`'s body, as the editor does when the person's change lands: the whole
/// of what the control holds.
fn wrote(r: &Replica, note: &ResourceRef, blocks: Vec<tilo_doc::snapshot::BlockView>) {
    let field = field_id(&r.host.package(APP).unwrap().meta, "Note", "body");
    r.edit(note, &field, &Value::Rich(RichValue { blocks }));
}

/// The body's first line, made as prose where the body holds none yet.
fn first_line(
    blocks: &mut Vec<tilo_doc::snapshot::BlockView>,
) -> &mut tilo_doc::snapshot::BlockView {
    if blocks.is_empty() {
        blocks.push(tilo_doc::snapshot::BlockView {
            id: String::new(),
            kind: "paragraph".to_owned(),
            text: String::new(),
            marks: Vec::new(),
            attrs: std::collections::BTreeMap::default(),
            children: Vec::new(),
        });
    }
    blocks.first_mut().expect("a line")
}

/// `r`'s person types `text` at `at` of the first line of `note`'s body.
fn splice(r: &Replica, note: &ResourceRef, at: usize, del: usize, text: &str) {
    let mut blocks = blocks(r, note);
    let first = first_line(&mut blocks);
    let mut chars: Vec<char> = first.text.chars().collect();
    let at = at.min(chars.len());
    let end = at.saturating_add(del).min(chars.len());
    chars.splice(at..end, text.chars());
    first.text = chars.into_iter().collect();
    wrote(r, note, blocks);
}

/// `r`'s person makes the first line of `note`'s body a heading of `level`, as the format menu
/// does.
fn heading(r: &Replica, note: &ResourceRef, level: Option<i64>) {
    let mut blocks = blocks(r, note);
    let first = first_line(&mut blocks);
    if level.is_some() {
        "heading"
    } else {
        "paragraph"
    }
    .clone_into(&mut first.kind);
    match level {
        Some(level) => {
            first
                .attrs
                .insert("level".to_owned(), tilo_doc::snapshot::AttrVal::Int(level));
        }
        None => {
            first.attrs.remove("level");
        }
    }
    wrote(r, note, blocks);
}

/// The Markdown the app exports for `note`.
fn exported(r: &Replica, note: &ResourceRef) -> String {
    match call(r, "export_markdown", vec![Value::resource(note)]) {
        Value::Text(text) => text,
        other => panic!("an export: {other:?}"),
    }
}

#[test]
fn a_heading_made_from_the_format_menu_is_the_lines_kind_and_level_in_the_body() {
    let (ada, notebook) = workspace();
    let note = noted(&ada, &notebook, "Groceries", "Oat milk, eggs.");
    assert_eq!(
        line(&ada, &note),
        ("paragraph".to_owned(), None),
        "a note's body starts as prose"
    );
    heading(&ada, &note, Some(2));
    assert_eq!(
        line(&ada, &note),
        ("heading".to_owned(), Some(2)),
        "the kind and level the body keeps"
    );
    assert_eq!(
        blocks(&ada, &note)[0].text,
        "Oat milk, eggs.",
        "the words are the line's, never the marker's"
    );
    heading(&ada, &note, None);
    assert_eq!(
        line(&ada, &note),
        ("paragraph".to_owned(), None),
        "and body text puts it back"
    );
}

#[test]
fn a_heading_exports_as_a_markdown_heading_and_its_words_are_read() {
    let (ada, notebook) = workspace();
    let note = noted(&ada, &notebook, "Groceries", "Oat milk, eggs.");
    heading(&ada, &note, Some(2));

    let markdown = exported(&ada, &note);
    assert!(
        markdown.starts_with("# Groceries\n\n## Oat milk, eggs."),
        "a heading is written as the `## ` it is: {markdown}"
    );

    // What search and the word count read: the words of the line, never a marker.
    let said = blocks(&ada, &note)[0].text.clone();
    assert_eq!(said, "Oat milk, eggs.");
    let hits = tilolang_tilo::search::every_hit(&ada.host);
    assert!(
        hits.iter()
            .any(|hit| hit.texts.iter().any(|text| text.contains("eggs"))),
        "search finds the note by the words in its heading: {hits:#?}"
    );
}
