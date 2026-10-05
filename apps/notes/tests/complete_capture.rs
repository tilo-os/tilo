//! A whole-window capture of Notes draws the open note at its full height,
//! not the part of it the window shows, and leaves the window as it was.

use crate::framework_support::{add_note, open, surface};

#[test]
fn a_long_note_is_captured_whole_and_the_window_keeps_its_place() {
    let mut app = crate::framework_support::notes();
    let body = (1..=52)
        .map(|n| format!("Paragraph {n} of a note longer than its window."))
        .collect::<Vec<_>>()
        .join("\n\n");
    let page = add_note(&mut app, "Complete capture", &body);
    open(&mut app, page);
    let window = app.harness().root_widget().get_rect();
    surface(&mut app, |notes| {
        ui::Widget::apply_primary_vertical_scroll(
            notes.main_panel_mut(),
            ui::widget::PrimaryScrollAction::Absolute(120.0),
        );
    });
    let before = surface(&mut app, |notes| notes.main_panel_mut().scroll_y_for_test());

    let (size, _) = app
        .harness()
        .complete_content()
        .expect("a note longer than the window has more to capture");
    assert!(
        size.height > window.height() * 2.0,
        "the capture holds the whole note: {size:?} for a {window:?} window"
    );
    assert!((size.width - window.width()).abs() < 1.0);

    app.settle();
    assert_eq!(app.harness().root_widget().get_rect(), window);
    let after = surface(&mut app, |notes| notes.main_panel_mut().scroll_y_for_test());
    assert!(
        before > 0.0 && (after - before).abs() < 0.5,
        "the reader's place is kept: {before} then {after}"
    );
    assert!(
        app.harness().targets().contains_key("block_editor"),
        "the window lays out and draws as before"
    );
}

#[test]
fn a_short_note_has_nothing_beyond_its_window() {
    let mut app = crate::framework_support::notes();
    let page = add_note(&mut app, "Short", "One line.");
    open(&mut app, page);
    assert!(app.harness().complete_content().is_none());
}

#[test]
fn a_long_note_handed_over_is_captured_whole() {
    ui::i18n::I18nManager::init("en-US");
    let body = (1..=52)
        .map(|n| format!("Complete capture {n:02} continues beyond the window."))
        .collect::<Vec<_>>()
        .join("\n\n");
    let (file, _receipt) =
        ui::application::OpenFile::new("Complete capture.md", "text/markdown", body.into_bytes())
            .unwrap();
    let mut app = ui::framework::testing::TestApp::<notes::NotesShell>::handed(file).unwrap();
    app.settle();
    assert!(
        app.harness().complete_content().is_some(),
        "a note handed over as a file is captured beyond the window"
    );
}
