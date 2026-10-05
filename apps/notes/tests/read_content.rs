//! A notebook is read through its published `read` action, as a script or an
//! agent reads it: the pages it holds, then one page's title and blocks.

use notes::NotesShell;
use notes::actions::page_body_id;
use notes::doc::NotesDoc;
use serde_json::json;
use tilo_document::{RichText, RichTextAction};
use ui::framework::testing::TestApp;
use ui::permissions::Role;
use ui::tilo_resource::Invoke;

fn read(app: &mut TestApp<NotesShell>, arguments: serde_json::Value) -> serde_json::Value {
    app.call("read", arguments)
        .unwrap_or_else(|error| panic!("read refused: {error:?}"))
        .expect("read answers a value")
}

#[test]
fn a_note_written_through_actions_reads_back_through_actions() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    app.call("new-page", json!({ "value": "Groceries" }))
        .unwrap();

    let notebook = read(&mut app, json!({}));
    assert_eq!(notebook["kind"], "notes.notebook");
    let pages = notebook["pages"].as_array().expect("pages");
    let page = pages
        .iter()
        .find(|page| page["title"] == "Groceries")
        .unwrap_or_else(|| panic!("the new note is not listed: {notebook}"));
    let item = page["item"].as_str().expect("item id").to_owned();

    let id = app.read_document(|doc| {
        NotesDoc::list_pages(doc)
            .into_iter()
            .find(|page| page.title == "Groceries")
            .expect("page")
            .id
    });
    app.invoke_child::<RichText>(
        &page_body_id(id),
        Invoke::apply(&RichTextAction::AppendParagraph {
            text: "oat milk".into(),
        })
        .unwrap(),
    )
    .unwrap();

    let note = read(&mut app, json!({ "item": item }));
    assert_eq!(note["kind"], "notes.page");
    assert_eq!(note["title"], "Groceries");
    assert!(
        note["text"]
            .as_str()
            .is_some_and(|text| text.contains("oat milk")),
        "{note}"
    );
    assert!(
        note["blocks"].as_array().is_some_and(|blocks| blocks
            .iter()
            .any(|block| block["text"] == "oat milk"
                && block["id"].as_str().is_some_and(|id| !id.is_empty()))),
        "{note}"
    );

    let missing = app.call("read", json!({ "item": "notes.page:not-a-page" }));
    assert!(missing.is_err(), "an unknown note is refused: {missing:?}");
}

/// Reading needs read permission only: a viewer reads, as a viewer sees.
#[test]
fn a_viewer_reads_the_notebook() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::with_role(Role::Viewer).unwrap();
    let notebook = read(&mut app, json!({}));
    assert!(notebook["pages"].is_array(), "{notebook}");
}

/// `read` is published as a read: a view tagged as the document read, so a
/// caller that just opened Notes finds it without knowing the app.
#[test]
fn read_is_published_as_the_notebooks_document_read() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    let resources = app.harness().resources();
    let notebook = resources
        .iter()
        .find(|resource| resource.id.as_str() == "notes.notebook")
        .expect("the notebook is published");
    let read = notebook
        .actions
        .iter()
        .find(|action| action.id == "read")
        .expect("the notebook publishes read");
    assert_eq!(read.kind, tilo_actions::OperationKind::View);
    assert!(read.contract.reads_document());
}
