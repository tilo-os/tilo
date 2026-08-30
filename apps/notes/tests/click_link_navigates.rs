//! Phase 4: clicking on a `Mark::Link(page_id)` span in the editor
//! navigates the app to that page.
//!
//! Drives `NotesApp::navigate_to` directly — full click-through tests
//! require a live winit event loop and per-glyph hit-tests, which
//! we don't run in the headless test harness. The hit-test logic
//! is unit-tested separately in `ui::widget::rich_text_line` via
//! `link_at_byte`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use notes::NotesApp;
use notes::sidebar::OutEvent;
use tempfile::TempDir;

#[test]
fn navigate_to_changes_selected_page() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
    let root_id = app.workspace_pages_for_test()[0].id;
    // Create a second page.
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    let pages = app.workspace_pages_for_test();
    assert_eq!(pages.len(), 2);
    let second_id = pages.iter().find(|p| p.id != root_id).unwrap().id;

    // After create the selection should already be the new page.
    assert_eq!(app.selected_for_test(), Some(second_id));

    // navigate_to drives the same path the click-on-link handler
    // uses.
    app.navigate_to(root_id);
    assert_eq!(app.selected_for_test(), Some(root_id));
}

#[test]
fn link_at_byte_finds_link_in_block() {
    use ui::widget::marks::{Mark, MarkSpan};
    use ui::widget::rich_text_line::link_at_byte;
    let id = uuid::Uuid::from_u128(42);
    let marks = vec![MarkSpan::new(6..13, Mark::Link(id))];
    assert_eq!(link_at_byte(&marks, 8), Some(id));
    assert_eq!(link_at_byte(&marks, 0), None);
}
