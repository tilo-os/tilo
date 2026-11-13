//! A heading in a note's body: the line's kind and level in the record, made by the format menu
//! or typed as `## `, which another person's typing merges into, which the Markdown export
//! writes as the `## ` it is, and whose words search reads as text.

use automerge::AutoCommit;
use serde_json::Value as Json;
use tilo_document::action::RegionKind;
use tilo_resource::{Edit, EditOp, Id, Segment};
use tilolang_runtime::{InvokeOutcome, Principal, ResourceRef, Value};
use tilolang_tilo::action::{Authorship, DynAction, DynEdit};

use crate::fixtures::{Replica, field_id};

const DISCUSSION: (&str, &str) = (
    include_str!("../../../../apps/tl/discussion/app.tl"),
    include_str!("../../../../apps/tl/discussion/app.ids"),
);
const NOTES: (&str, &str) = (
    include_str!("../../../../apps/tl/notes/app.tl"),
    include_str!("../../../../apps/tl/notes/app.ids"),
);
const APP: &str = "so.tilo.lang.notes";

fn packages() -> [(&'static str, &'static str); 2] {
    [DISCUSSION, NOTES]
}

/// Ada's workspace with its notebook, and Grace's replica of it.
fn workspace() -> (Replica, Replica, ResourceRef) {
    let (mut ada, _loaded) = Replica::new("ada", AutoCommit::new(), &packages());
    ada.host.set_clock(|| "2026-11-04T09:00:00Z".to_owned());
    ada.commit();
    let notebook = ada.host.root_of(APP).expect("the notebook");
    let grace = ada.fork("grace", &packages());
    (ada, grace, notebook)
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
    let who = Principal::new(r.name.clone());
    match r.host.runtime().invoke_action(&who, (id.as_str(), args)) {
        Ok(InvokeOutcome::Done(value)) => value,
        other => panic!("`{action}` answered {other:?}"),
    }
}

/// A note called `title` at the top of the tree, its body saying `body`.
fn noted(r: &mut Replica, notebook: &ResourceRef, title: &str, body: &str) -> ResourceRef {
    let made = match call(r, "new_note", vec![Value::resource(notebook)]) {
        Value::Ref(at) => (*at.target).clone(),
        other => panic!("a note: {other:?}"),
    };
    call(
        r,
        "retitle",
        vec![Value::resource(&made), Value::text(title)],
    );
    // The note is in the record before its body is edited, as a window's writes reach it one
    // change after the other.
    r.commit();
    splice(r, &made, 0, 0, body);
    r.commit();
    made
}

/// The blocks of `note`'s body as the record holds them.
fn blocks(r: &Replica, note: &ResourceRef) -> Vec<tilo_doc::snapshot::BlockView> {
    match r.host.runtime().read(note, "body") {
        Ok(Value::Rich(rich)) => rich.blocks,
        other => panic!("a note's body: {other:?}"),
    }
}

/// What the line at `block` of `note`'s body is, as the record keeps it: its kind and its
/// heading level.
fn line(r: &Replica, note: &ResourceRef) -> (String, Option<i64>) {
    let block = blocks(r, note).into_iter().next().expect("one line");
    (block.kind.clone(), block.int_attr("level"))
}

/// `r`'s person edits the `field` of `item` with `action`, as its editor writes one step of
/// theirs: a rich-text action at the field's path, kept in the record.
fn rich_edit(
    r: &mut Replica,
    item: &ResourceRef,
    field: &str,
    action: tilo_document::RichTextAction,
) {
    let ty = "Note";
    let meta = r.host.package(APP).unwrap().meta;
    let path = vec![
        Segment::Item(Id::from_raw(item.item.as_deref().unwrap())),
        Segment::Field(field_id(&meta, ty, field)),
    ];
    let rich = serde_json::to_value(&action).unwrap();
    r.apply(&DynAction {
        app: APP.to_owned(),
        kind: format!("tl.{APP}.{ty}"),
        action_id: "$edit".to_owned(),
        args: Json::Null,
        authorship: Authorship::default(),
        edits: vec![DynEdit::Edit {
            id: item.doc.clone(),
            edit: Edit::new(path, EditOp::Rich { rich }),
        }],
    });
}

/// `r`'s person types `text` at `at` of the first line of `note`'s body.
fn splice(r: &mut Replica, note: &ResourceRef, at: usize, del: usize, text: &str) {
    let block = blocks(r, note).into_iter().next().expect("one line").id;
    rich_edit(
        r,
        note,
        "body",
        tilo_document::RichTextAction::SpliceText {
            region: RegionKind::Body,
            block,
            at,
            del,
            ins: text.to_owned(),
        },
    );
}

/// `r`'s person makes the first line of `note`'s body a heading of `level`, as the format menu
/// does.
fn heading(r: &mut Replica, note: &ResourceRef, level: Option<i64>) {
    let block = blocks(r, note).into_iter().next().expect("one line").id;
    rich_edit(
        r,
        note,
        "body",
        tilo_document::RichTextAction::SetParagraphStyle {
            region: RegionKind::Body,
            blocks: vec![block],
            kind: if level.is_some() {
                "heading"
            } else {
                "paragraph"
            }
            .to_owned(),
            level,
            ordered: None,
        },
    );
}

/// The Markdown the app exports for `note`.
fn exported(r: &Replica, note: &ResourceRef) -> String {
    match call(r, "export_markdown", vec![Value::resource(note)]) {
        Value::Text(text) => text,
        other => panic!("an export: {other:?}"),
    }
}

#[test]
fn a_heading_made_from_the_format_menu_is_the_lines_kind_and_level_in_the_record() {
    let (mut ada, _grace, notebook) = workspace();
    let note = noted(&mut ada, &notebook, "Groceries", "Oat milk, eggs.");
    assert_eq!(
        line(&ada, &note),
        ("paragraph".to_owned(), None),
        "a note's body starts as prose"
    );
    heading(&mut ada, &note, Some(2));
    assert_eq!(
        line(&ada, &note),
        ("heading".to_owned(), Some(2)),
        "the block attribute the action log keeps"
    );
    assert_eq!(
        blocks(&ada, &note)[0].text,
        "Oat milk, eggs.",
        "the words are the line's, never the marker's"
    );
    heading(&mut ada, &note, None);
    assert_eq!(
        line(&ada, &note),
        ("paragraph".to_owned(), None),
        "and body text puts it back"
    );
}

#[test]
fn a_heading_merges_with_another_persons_typing_in_the_same_line() {
    let (mut ada, mut grace, notebook) = workspace();
    let note = noted(&mut ada, &notebook, "Groceries", "Oat milk");
    ada.sync_with(&mut grace);

    // At once: Ada makes the line a heading, Grace types into it.
    heading(&mut ada, &note, Some(2));
    splice(&mut grace, &note, 0, 0, "Two ");
    ada.sync_with(&mut grace);

    assert_eq!(
        line(&ada, &note),
        ("heading".to_owned(), Some(2)),
        "the heading stands on Ada's side"
    );
    assert_eq!(line(&grace, &note), line(&ada, &note), "and on Grace's");
    assert_eq!(
        blocks(&ada, &note)[0].text,
        blocks(&grace, &note)[0].text,
        "both replicas read one line"
    );
    assert_eq!(
        blocks(&ada, &note)[0].text,
        "Two Oat milk",
        "the words both wrote are in it"
    );
    assert_eq!(
        ada.host.store().record(&note.doc).unwrap(),
        grace.host.store().record(&note.doc).unwrap(),
        "the record they hold is the same"
    );
}

#[test]
fn a_heading_exports_as_a_markdown_heading_and_its_words_are_read() {
    let (mut ada, _grace, notebook) = workspace();
    let note = noted(&mut ada, &notebook, "Groceries", "Oat milk, eggs.");
    heading(&mut ada, &note, Some(2));

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
