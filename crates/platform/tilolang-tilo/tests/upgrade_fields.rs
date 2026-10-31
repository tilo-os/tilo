//! A package whose field changed from `Text` to `RichText`: the records keep their words.
//!
//! The Notes fixture (`tests/legacy/notes.hex`) was written by the build that kept a note's
//! body as plain text. Opened by a build that keeps it as rich text, every note's body survives
//! with the same words, an edit after the upgrade lands as a rich splice, and running the
//! upgrade again writes nothing.

use automerge::AutoCommit;
use tilolang_runtime::meta::FieldKind;
use tilolang_runtime::store::{FieldSpec, FieldValue, Store as _};
use tilolang_runtime::value::ResourceRef;

use crate::fixtures::Replica;

const APP: &str = "so.tilo.lang.notes";
const OLD: &str = include_str!("../../../../apps/tl/notes/app.tl");
const OLD_IDS: &str = include_str!("../../../../apps/tl/notes/app.ids");

/// The same package, its note's body kept as rich text: what a new build ships. The views and
/// the scenario follow the type: the row shows the body's first line (`excerpt`), the editor
/// edits it with the shared rich input, and the scenario's notes say their body as rich text.
fn rich_notes() -> (String, &'static str) {
    let mut source = OLD.replace("body: Text (body)", "body: RichText (body)");
    source = source.replace("secondary: n.body,", "secondary: n.body.excerpt,");
    source = source.replace(
        "ui.TextInput(value: n.body, placeholder: \"Write something\", lines: 8)",
        "rich_input(bind: n.body, placeholder: \"Write something\")",
    );
    // Every note the scenario makes says its body as rich text.
    source = wrap_rich_bodies(&source);
    assert!(source.contains("body: RichText (body)"), "{source}");
    assert!(source.contains("body: RichText(\""), "{source}");
    (source, OLD_IDS)
}

/// `source` with every `body: "text"` of a note's construction said as `body: RichText("text")`.
fn wrap_rich_bodies(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some((head, tail)) = rest.split_once("body: \"") {
        out.push_str(head);
        out.push_str("body: RichText(\"");
        let Some((text, after)) = tail.split_once('"') else {
            out.push_str(tail);
            return out;
        };
        out.push_str(text);
        out.push_str("\")");
        rest = after;
    }
    out.push_str(rest);
    out
}

/// The saved room a fixture holds as hex.
fn saved(hex: &str) -> AutoCommit {
    let digits: Vec<u8> = hex.bytes().filter(u8::is_ascii_hexdigit).collect();
    let bytes: Vec<u8> = digits
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    AutoCommit::load(&bytes).unwrap()
}

/// The id of the `field` declared on the type `ty` of the package.
fn field_id(replica: &Replica, ty: &str, field: &str) -> String {
    let meta = replica.host.package(APP).unwrap().meta;
    meta.types
        .iter()
        .find(|t| t.name == ty)
        .and_then(|t| t.fields.iter().find(|f| f.name == field))
        .map(|f| f.id.clone())
        .unwrap()
}

/// The id of the type `ty`.
fn type_id(replica: &Replica, ty: &str) -> String {
    let meta = replica.host.package(APP).unwrap().meta;
    meta.types
        .iter()
        .find(|t| t.name == ty)
        .map(|t| t.id.clone())
        .unwrap()
}

/// Every note's id, in the order the notebook lists them.
fn note_ids(replica: &Replica) -> Vec<String> {
    let notes = replica.host.root_of(APP).unwrap();
    let field = field_id(replica, "Notes", "items");
    let spec = FieldSpec {
        id: field,
        kind: FieldKind::Collection,
    };
    match replica.host.store().read(&notes, &spec).unwrap() {
        FieldValue::Items(ids) => ids,
        other => panic!("{other:?}"),
    }
}

/// Each note's body as plain text, as the fixture holds it: the fixture was written before
/// items were flat, so a note is a map nested in the notebook's `items` collection, under the
/// note's id, its map in the entry's `v`, its body a text object read whole. The room is walked
/// rather than read at a path, so a note wherever the notebook stores it is found.
fn plain_bodies(doc: &AutoCommit, ids: &[String]) -> Vec<String> {
    use automerge::{ObjId, ObjType, ROOT, ReadDoc as _, Value as AmValue};
    // Every map in the room, by the key it is stored under: a note's map is the `v` of an
    // entry keyed by the note's id.
    fn walk(
        doc: &AutoCommit,
        obj: &ObjId,
        body: &str,
        found: &mut std::collections::BTreeMap<String, String>,
    ) {
        for key in doc.keys(obj) {
            let Some((AmValue::Object(ObjType::Map), held)) = doc.get(obj, &key).unwrap() else {
                continue;
            };
            // A nested entry: its `v` is the note's map.
            if let Some((AmValue::Object(ObjType::Map), note)) = doc.get(&held, "v").unwrap()
                && let Some((AmValue::Object(ObjType::Text), text)) = doc.get(&note, body).unwrap()
            {
                found.insert(key.clone(), doc.text(&text).unwrap());
            }
            walk(doc, &held, body, found);
        }
    }
    let body = body_field();
    let mut found = std::collections::BTreeMap::new();
    walk(doc, &ROOT, &body, &mut found);
    ids.iter()
        .map(|id| {
            found
                .get(id)
                .unwrap_or_else(|| panic!("the fixture holds no note {id}"))
                .clone()
        })
        .collect()
}

/// The id the package's ids lock gives the declaration `name`.
fn locked(name: &str) -> String {
    OLD_IDS
        .lines()
        .find_map(|line| {
            line.strip_prefix(name)
                .and_then(|rest| rest.strip_prefix(" = "))
        })
        .map_or_else(
            || panic!("the package's ids lock names {name}"),
            |id| id.trim().to_owned(),
        )
}

/// The id the Notes app locks for a note's body, from its ids lock.
fn body_field() -> String {
    locked("Note.body")
}

/// Each note's body as rich text, read by the build keeping it so: what it says.
fn rich_bodies(replica: &Replica, ids: &[String]) -> Vec<String> {
    ids.iter()
        .map(|id| {
            let (FieldValue::Rich(rich), _) = rich_body(replica, id) else {
                panic!("{id} does not read as rich text");
            };
            rich.written_text()
        })
        .collect()
}

fn rich_body(replica: &Replica, id: &str) -> (FieldValue, ResourceRef) {
    let notes = replica.host.root_of(APP).unwrap();
    let at = notes.item(id, type_id(replica, "Note"));
    let field = field_id(replica, "Note", "body");
    let value = replica
        .host
        .store()
        .read(
            &at,
            &FieldSpec {
                id: field,
                kind: FieldKind::RichText,
            },
        )
        .unwrap();
    (value, at)
}

/// The fixture's room, opened by the build keeping the body as rich text, and the ids of its
/// notes: nothing is written yet, the notes still hold the plain text the old build wrote.
fn room() -> (Replica, Vec<String>) {
    let room = saved(include_str!("legacy/notes.hex"));
    let (source, lock) = rich_notes();
    let replica = Replica::new("ada", room, &[(source.as_str(), lock)]).0;
    assert!(
        replica.host.take_committed().is_empty(),
        "opening the room wrote nothing"
    );
    let notes = note_ids(&replica);
    assert!(!notes.is_empty(), "the fixture holds notes");
    (replica, notes)
}

#[test]
fn every_bodys_words_survive_the_type_change() {
    let (later, notes) = room();
    // What the old build left: each note's body as a text object.
    let before = plain_bodies(&later.doc, &notes);
    assert!(before.iter().any(|text| !text.is_empty()), "{before:?}");

    later.host.migrate();
    let committed = later.host.take_committed();
    assert!(!committed.is_empty(), "the upgrade wrote the notes");

    assert_eq!(
        rich_bodies(&later, &notes),
        before,
        "every note's words survive"
    );
}

#[test]
fn an_edit_after_the_upgrade_lands_as_a_rich_splice() {
    use tilolang_runtime::UiNode;

    fn walk<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
        out.push(node);
        for child in &node.children {
            walk(child, out);
        }
    }

    let (mut later, notes) = room();
    later.host.migrate();
    // The upgrade is committed through the executor, as any action is.
    for done in later.host.take_committed() {
        later.apply(&done.action);
    }
    assert_eq!(later.host.migrate(), 0, "committed, the upgrade is done");

    // The note's window edits its body the way the shared rich input does: the whole value,
    // written as the splices and marks of the paragraphs that differ.
    let (_, at) = rich_body(&later, &notes[0]);
    let view = later
        .host
        .open_resource(&at, None)
        .unwrap_or_else(|error| panic!("the note's window opens: {error}"));
    let mut all = Vec::new();
    walk(view.tree(), &mut all);
    let input = all
        .into_iter()
        .find(|node| node.kind == "ui.RichInput")
        .map(|node| node.key.clone())
        .expect("the note's window has a rich input");
    let (FieldValue::Rich(before), _) = rich_body(&later, &notes[0]) else {
        panic!("the body reads as rich text");
    };
    let mut typed = before.clone();
    typed.push_text(" and more");
    view.edit(&input, &tilolang_runtime::value::Value::Rich(typed.clone()))
        .expect("the shared rich input edits the body");
    later.commit();

    let (FieldValue::Rich(after), _) = rich_body(&later, &notes[0]) else {
        panic!("the body reads as rich text");
    };
    let mut expected = before.written_text();
    expected.push_str(" and more");
    assert_eq!(after.written_text(), expected);
    assert_eq!(
        later.host.migrate(),
        0,
        "an edited note is not turned again"
    );
}

#[test]
fn an_upgrade_run_again_writes_nothing() {
    let (mut later, _) = room();
    later.host.migrate();
    let first = later.host.take_committed();
    assert!(!first.is_empty(), "the first run wrote the notes");
    for done in &first {
        later.apply(&done.action);
    }
    assert_eq!(later.host.migrate(), 0);
    assert!(
        later.host.take_committed().is_empty(),
        "a second run writes nothing"
    );
}
