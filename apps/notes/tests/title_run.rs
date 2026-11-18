//! Typing a note's title renames the note at every keystroke, so the tree,
//! the window's bar and its tab follow. What the person did is still one
//! rename: one History row and one undo step, until they pause or someone
//! else changes the notebook in between.

use crate::framework_support::{surface, with_note};
use notes::{NotesApp, NotesShell};
use ui::framework::testing::TestApp;

/// Wall time of the first edit, in milliseconds.
const T0: u64 = 1_800_000_000_000;

fn title(app: &TestApp<NotesShell>, page: notes::PageId) -> String {
    crate::framework_support::notes_doc(app, notes::read::list_pages)
        .into_iter()
        .find(|shown| shown.id == page)
        .map(|shown| shown.title)
        .expect("the note is listed")
}

/// Click the title (which selects it) and type `text` over it, at `at_ms`.
fn retitle(app: &mut TestApp<NotesShell>, text: &str, at_ms: u64) {
    tilo_clock::with_wall_ms(at_ms, || {
        app.harness().relayout_at((1200, 800)).unwrap();
        let center = surface(app, |notes: &mut NotesApp| {
            notes.main_title_center_for_test()
        });
        app.harness().click_at(center.x, center.y);
        app.harness().type_text(text);
        app.harness().key("Enter");
    });
}

/// The renames History lists, oldest first, as it reads them.
fn renames(app: &TestApp<NotesShell>) -> Vec<String> {
    // The notes, and their history, are kept in the first part.
    let mut doc = app
        .peer_document_of(&notes::note_list::chain().part(0))
        .expect("the part is held");
    let rows = ui::history_source::HistorySource::new()
        .without_resource_genesis()
        .entries(&mut doc);
    let entries: Vec<_> = rows.iter().filter_map(|row| row.context.clone()).collect();
    app.sentences(&entries)
        .into_iter()
        .filter(|sentence| sentence.starts_with("Renamed"))
        .collect()
}

#[test]
fn typing_a_title_is_one_rename_in_history_and_one_undo_step() {
    let (mut app, page) = with_note("Welcome", "Hello");
    retitle(&mut app, "Road", T0);
    assert_eq!(title(&app, page), "Road");
    assert_eq!(renames(&app), ["Renamed Welcome to Road"]);
    assert!(app.undo(), "the rename is undoable");
    assert_eq!(
        title(&app, page),
        "Welcome",
        "one undo takes back the whole title"
    );
    assert!(app.redo());
    assert_eq!(title(&app, page), "Road", "and one redo brings it back");
}

#[test]
fn renames_a_pause_apart_are_two_renames() {
    let (mut app, page) = with_note("Welcome", "Hello");
    retitle(&mut app, "Road", T0);
    retitle(&mut app, "Trip", T0 + 60_000);
    assert_eq!(
        renames(&app),
        ["Renamed Welcome to Road", "Renamed Road to Trip"]
    );
    assert!(app.undo());
    assert_eq!(
        title(&app, page),
        "Road",
        "the later rename is a step of its own"
    );
    assert!(app.undo());
    assert_eq!(title(&app, page), "Welcome");
}

#[test]
fn a_members_change_in_between_ends_the_rename() {
    let (mut app, page) = with_note("Welcome", "Hello");
    tilo_clock::with_wall_ms(T0, || {
        app.harness().relayout_at((1200, 800)).unwrap();
        let center = surface(&mut app, |notes: &mut NotesApp| {
            notes.main_title_center_for_test()
        });
        app.harness().click_at(center.x, center.y);
        app.harness().type_text("Ro");
        crate::framework_support::part_edit(&mut app, page, |doc| {
            let first = notes::read::page_block_snapshots(doc, page)[0].0.clone();
            crate::component_support::edit_page_body(
                doc,
                page,
                &tilo_document::RichTextAction::SpliceText {
                    region: tilo_document::action::RegionKind::Body,
                    block: first,
                    at: 0,
                    del: 0,
                    ins: "Hi. ".into(),
                },
            )
            .unwrap();
        });
        app.harness().type_text("ad");
        app.harness().key("Enter");
    });
    assert_eq!(title(&app, page), "Road");
    assert_eq!(
        renames(&app),
        ["Renamed Welcome to Ro", "Renamed Ro to Road"]
    );
    assert!(app.undo());
    assert_eq!(title(&app, page), "Ro");
}
