//! A notebook written before Notes stored a notebook record (notes at
//! `pages/<id>/{title, parent, order, body}`, `notes_schema_version = 3`)
//! opens with its notes: their titles, tree, bodies and tags, readable and
//! editable through Notes' actions. Read as a record without migrating, every
//! note was untitled and had no body, so an agent's `read` said "has no
//! body" and every edit of a note found no rich text to edit.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT};
use notes::{NotesShell, PageId};
use serde_json::json;
use tilo_document::{RichText, RichTextAction};
use ui::framework::testing::TestApp;

use crate::framework_support::{body, shown};

struct Legacy {
    doc: AutoCommit,
    todo: PageId,
    child: PageId,
    loose: PageId,
    tag: PageId,
}

fn page(doc: &mut AutoCommit, pages: &ObjId, id: PageId, fields: (&str, &str, &str), text: &str) {
    let (title, parent, order) = fields;
    let page = doc.put_object(pages, id.to_string(), ObjType::Map).unwrap();
    let title_text = doc.put_object(&page, "title", ObjType::Text).unwrap();
    doc.splice_text(&title_text, 0, 0, title).unwrap();
    doc.put(&page, "parent", parent).unwrap();
    doc.put(&page, "order", order).unwrap();
    let body = doc.put_object(&page, "body", ObjType::Map).unwrap();
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
    doc.put(ROOT, "notes_schema_version", 3_u64).unwrap();
    let pages = doc.put_object(ROOT, "pages", ObjType::Map).unwrap();
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
    let tags = doc.put_object(ROOT, "tags", ObjType::Map).unwrap();
    let entry = doc
        .put_object(&tags, tag.to_string(), ObjType::Map)
        .unwrap();
    let name = doc.put_object(&entry, "name", ObjType::Text).unwrap();
    doc.splice_text(&name, 0, 0, "later").unwrap();
    let assignments = doc
        .put_object(ROOT, "tag_assignments", ObjType::Map)
        .unwrap();
    doc.put(&assignments, format!("{child}:{tag}"), true)
        .unwrap();
    let assets = doc.put_object(ROOT, "assets", ObjType::Map).unwrap();
    let picture = doc
        .put_object(&assets, "ab".repeat(32), ObjType::Map)
        .unwrap();
    doc.put(&picture, "len", 817_544_u64).unwrap();
    doc.put(&picture, "mime", "image/png").unwrap();
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
