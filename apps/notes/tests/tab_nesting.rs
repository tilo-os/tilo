//! Tab in a note, as a person presses it in the framework-hosted app: it
//! nests the line under the one above in the notebook, Shift+Tab and undo
//! take that back, and Escape lets the next Tab leave the editor.

use notes::actions::create_page_action;
use notes::read;
use notes::{NotesShell, PageId};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;

fn two_lines() -> (TestApp<NotesShell>, PageId) {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    let page = PageId::new_v4();
    app.invoke(Invoke::apply(&create_page_action(page, None, "First")).unwrap())
        .unwrap();
    let rect = app.harness().targets()["block_editor"];
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    app.harness().type_text("one");
    app.harness().key("Enter");
    app.harness().type_text("two");
    (app, page)
}

/// Each line of the note with how deep it is nested.
fn outline(app: &TestApp<NotesShell>, page: PageId) -> Vec<(String, u8)> {
    app.read_document(|doc| {
        read::page_blocks(doc, page)
            .into_iter()
            .map(|block| (block.text, block.nesting))
            .collect()
    })
}

fn nested(lines: &[(&str, u8)]) -> Vec<(String, u8)> {
    lines
        .iter()
        .map(|(text, depth)| ((*text).to_owned(), *depth))
        .collect()
}

#[test]
fn tab_nests_the_line_under_the_one_above() {
    let (mut app, page) = two_lines();
    app.harness().key("Tab");
    assert_eq!(outline(&app, page), nested(&[("one", 0), ("two", 1)]));
    // What a screen reader (and the booted-image check) reads of it.
    let described = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some("Paragraph block 2"))
        .and_then(|(_, node)| node.description().map(str::to_owned));
    assert_eq!(described.as_deref(), Some("Level 2"));
    app.harness().set_modifiers(&["shift".into()]);
    app.harness().key("Tab");
    app.harness().set_modifiers(&[]);
    assert_eq!(outline(&app, page), nested(&[("one", 0), ("two", 0)]));
    app.harness().key("Tab");
    assert!(app.undo(), "nesting is undoable");
    assert_eq!(outline(&app, page), nested(&[("one", 0), ("two", 0)]));
    app.harness().type_text("!");
    assert_eq!(
        outline(&app, page),
        nested(&[("one", 0), ("two!", 0)]),
        "the caret stays where it was"
    );
}

#[test]
fn escape_then_tab_leaves_the_editor() {
    let (mut app, page) = two_lines();
    let editor = app.harness().focused();
    app.harness().key("Escape");
    app.harness().key("Tab");
    assert_ne!(app.harness().focused(), editor, "Tab moved focus on");
    assert_eq!(outline(&app, page), nested(&[("one", 0), ("two", 0)]));
}
