//! New note is on screen once: in the page tree's header while Notes draws
//! its own header, in the window's bar while the desktop's bar hosts it.

use crate::framework_support::{add_note, notes, open, surface};

/// Whether the window's bar declares New note.
fn bar_has_new_note() -> bool {
    ui::window_toolbar::declared()
        .is_some_and(|bar| bar.actions.iter().any(|action| action.id == "new-note"))
}

// USER REQUIREMENT (2026-10-21): "\"New note\" appears in both the window bar
// and the page tree header; keep the one in the bar."
#[test]
fn new_note_is_in_the_bar_and_never_twice() {
    let mut app = notes();
    let page = add_note(&mut app, "Welcome", "");
    open(&mut app, page);
    app.harness().relayout_at((1280, 720)).unwrap();
    assert!(
        surface(&mut app, |notes| notes
            .window_has_control_for_test("new-note")),
        "without a desktop bar, the page tree's header has New note"
    );

    ui::window_toolbar::set_hosted(true);
    for _ in 0..3 {
        app.harness().relayout_at((1280, 720)).unwrap();
    }
    assert!(bar_has_new_note(), "the window's bar has New note");
    assert!(
        surface(&mut app, |notes| notes.drawer_open_for_test()),
        "the page tree is open beside the note"
    );
    assert!(
        !surface(&mut app, |notes| notes
            .window_has_control_for_test("new-note")),
        "with the bar's New note on screen, the page tree's header has none"
    );

    ui::window_toolbar::set_hosted(false);
    for _ in 0..3 {
        app.harness().relayout_at((1280, 720)).unwrap();
    }
    assert!(
        surface(&mut app, |notes| notes
            .window_has_control_for_test("new-note")),
        "the bar gone, New note is back in the page tree's header"
    );
}
