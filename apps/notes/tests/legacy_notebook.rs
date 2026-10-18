//! A notebook written before Notes stored a notebook record (notes at
//! `pages/<id>/{title, parent, order, body}`, `notes_schema_version = 3`)
//! opens with its notes: their titles, tree, bodies and tags, readable and
//! editable through Notes' actions. Read as a record without migrating, every
//! note was untitled and had no body, so an agent's `read` said "has no
//! body" and every edit of a note found no rich text to edit.

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ScalarValue};
use notes::{NotesShell, PageId};
use serde_json::json;
use tilo_document::{RichText, RichTextAction};
use ui::framework::testing::TestApp;
use ui::tilo_resource::{Slot, write_key};

use crate::framework_support::{body, shown};

struct Legacy {
    doc: AutoCommit,
    todo: PageId,
    child: PageId,
    loose: PageId,
    tag: PageId,
}

/// The map under `key` of `obj`, made empty.
fn map(doc: &mut AutoCommit, obj: &ObjId, key: &str) -> ObjId {
    Slot::Key(obj, key).object(doc, ObjType::Map).unwrap()
}

/// A scalar `value` under `key` of `obj`, as the old notebook wrote it.
fn scalar(doc: &mut AutoCommit, obj: &ObjId, key: &str, value: ScalarValue) {
    Slot::Key(obj, key).put_scalar(doc, value).unwrap();
}

fn page(doc: &mut AutoCommit, pages: &ObjId, id: PageId, fields: (&str, &str, &str), text: &str) {
    let (title, parent, order) = fields;
    let page = map(doc, pages, &id.to_string());
    write_key(doc, &page, "title", &title.to_owned()).unwrap();
    scalar(doc, &page, "parent", ScalarValue::Str(parent.into()));
    scalar(doc, &page, "order", ScalarValue::Str(order.into()));
    let body = map(doc, &page, "body");
    RichText::initialize_under(doc, &body).unwrap();
    RichTextAction::AppendParagraph { text: text.into() }
        .apply_under(doc, &body)
        .unwrap();
}

/// A notebook as Notes wrote it before the record: a note with a note
/// under it, a top-level note sorted before it by its order key, a tag on
/// the child and a picture kept in the content store.
fn legacy() -> Legacy {
    let (todo, child, loose, tag) = (
        PageId::new_v4(),
        PageId::new_v4(),
        PageId::new_v4(),
        PageId::new_v4(),
    );
    let mut doc = AutoCommit::new();
    scalar(
        &mut doc,
        &ROOT,
        "notes_schema_version",
        ScalarValue::Uint(3),
    );
    let pages = map(&mut doc, &ROOT, "pages");
    page(
        &mut doc,
        &pages,
        todo,
        ("Todo", "", "z0e"),
        "still unable to use it",
    );
    page(
        &mut doc,
        &pages,
        child,
        ("How a computer should work", &todo.to_string(), "z"),
        "ctrl+n, a title, enter",
    );
    page(
        &mut doc,
        &pages,
        loose,
        ("Bugs", "", "j"),
        "tab does not indent",
    );
    let tags = map(&mut doc, &ROOT, "tags");
    let entry = map(&mut doc, &tags, &tag.to_string());
    write_key(&mut doc, &entry, "name", &"later".to_owned()).unwrap();
    let assignments = map(&mut doc, &ROOT, "tag_assignments");
    scalar(
        &mut doc,
        &assignments,
        &format!("{child}:{tag}"),
        ScalarValue::Boolean(true),
    );
    let assets = map(&mut doc, &ROOT, "assets");
    let picture = map(&mut doc, &assets, &"ab".repeat(32));
    scalar(&mut doc, &picture, "len", ScalarValue::Uint(817_544));
    scalar(
        &mut doc,
        &picture,
        "mime",
        ScalarValue::Str("image/png".into()),
    );
    doc.commit();
    Legacy {
        doc,
        todo,
        child,
        loose,
        tag,
    }
}

fn read(app: &mut TestApp<NotesShell>, arguments: serde_json::Value) -> serde_json::Value {
    app.call("read", arguments)
        .unwrap_or_else(|error| panic!("read refused: {error:?}"))
        .expect("read answers a value")
}

#[test]
fn a_room_notebook_from_before_the_record_opens_with_its_notes() {
    ui::i18n::I18nManager::init("en-US");
    let Legacy {
        mut doc,
        todo,
        child,
        loose,
        tag,
    } = legacy();
    let mut app = TestApp::<NotesShell>::with_room_record(&mut doc).unwrap();
    assert!(app.record().is_package_primary());

    let state = shown(&app);
    let roots: Vec<PageId> = state.children_of(None).iter().map(|page| page.id).collect();
    assert_eq!(roots, [loose, todo], "top-level notes keep their order");
    assert_eq!(state.get(child).unwrap().parent, Some(todo));
    assert_eq!(
        state.get(child).unwrap().title,
        "How a computer should work"
    );
    assert!(body(&app, todo).contains(&"still unable to use it".to_owned()));
    assert!(body(&app, child).contains(&"ctrl+n, a title, enter".to_owned()));
    assert_eq!(
        state
            .tags_for_page(child)
            .iter()
            .map(|tag| (tag.id, tag.name.clone()))
            .collect::<Vec<_>>(),
        [(tag, "later".to_owned())]
    );
    assert!(
        app.state().images.contains_key(&"ab".repeat(32)),
        "the picture stays named by its digest"
    );

    // What an agent reads: every note titled, every body there.
    let notebook = read(&mut app, json!({}));
    let mut titles: Vec<String> = notebook["pages"]
        .as_array()
        .expect("pages")
        .iter()
        .map(|page| page["title"].as_str().unwrap_or_default().to_owned())
        .collect();
    titles.sort();
    assert_eq!(titles, ["Bugs", "How a computer should work", "Todo"]);
    let note = read(
        &mut app,
        json!({ "item": notes::actions::page_resource_id(todo) }),
    );
    assert_eq!(note["title"], "Todo");
    assert!(
        note["markdown"]
            .as_str()
            .is_some_and(|markdown| markdown.contains("still unable to use it")),
        "{note}"
    );
    let body_read = read(
        &mut app,
        json!({ "item": notes::actions::body_resource_id(todo) }),
    );
    assert!(
        body_read["markdown"]
            .as_str()
            .is_some_and(|markdown| markdown.contains("still unable to use it")),
        "{body_read}"
    );

    // And every note's body takes edits again.
    crate::framework_support::edit_body(
        &mut app,
        todo,
        &RichTextAction::AppendParagraph {
            text: "fixed".into(),
        },
    );
    assert!(body(&app, todo).contains(&"fixed".to_owned()));

    // The migrated notebook is what opens next time.
    let app = app.reopen().unwrap();
    assert_eq!(shown(&app).get(todo).unwrap().title, "Todo");
    assert!(body(&app, todo).contains(&"fixed".to_owned()));
}

/// A window notebook from before the record is brought into a new room
/// notebook with its notes, not as untitled notes without bodies.
#[test]
fn a_window_notebook_from_before_the_record_is_adopted_with_its_notes() {
    ui::i18n::I18nManager::init("en-US");
    let Legacy {
        mut doc,
        todo,
        child,
        ..
    } = legacy();
    // Stamped as the window record the launcher gave an earlier window.
    assert!(ui::record::init_scoped(
        &mut doc,
        &ui::record::RecordId::window_default(0x41),
        &ui::record::RecordKind::new("notes.notebook"),
        "test-workspace",
        Some("test-workspace"),
    ));
    let app = TestApp::<NotesShell>::with_window_records(&mut [doc]).unwrap();
    let state = shown(&app);
    assert_eq!(state.pages().len(), 3);
    assert_eq!(state.get(todo).unwrap().title, "Todo");
    assert_eq!(state.get(child).unwrap().parent, Some(todo));
    assert!(body(&app, todo).contains(&"still unable to use it".to_owned()));
}
