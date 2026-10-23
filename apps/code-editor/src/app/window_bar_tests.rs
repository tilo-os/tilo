//! The editor's controls live in the window's bar: the shown file, the
//! bottom panel's toggle, and find and replace in the buffer.

use ui::widget::{FindOptions, InPlace, SearchDriver, Widget};
use ui::window_toolbar::{SearchOpening, ToolbarEvent};

use super::super::{CodeApp, EditorDoc};
use super::BOTTOM_PANEL;
use crate::commands::Command;

fn synced_sample() -> crate::test_support::Sample {
    let mut sample = crate::test_support::sample();
    sample.open("src/main.rs");
    sample.app.settle();
    sample
}

fn buffer_text(app: &CodeApp, doc: &ui::widget::Edits<'_>) -> String {
    let node = app.session.active_buffer().expect("a file is shown").node;
    EditorDoc::read_buffer_key(doc, &app.buffer_key(node)).expect("the buffer's text")
}

fn panel_action(app: &CodeApp) -> ui::window_toolbar::ToolbarAction {
    app.window_toolbar()
        .actions
        .into_iter()
        .find(|action| action.id == BOTTOM_PANEL)
        .expect("the bottom panel's toggle is a bar action")
}

#[test]
fn the_editor_draws_no_header_row_and_its_controls_are_in_the_window_bar() {
    let mut sample = synced_sample();
    sample.editor(|app| {
        let toolbar = app.window_toolbar();
        assert_eq!(
            toolbar.location.map(|location| location.label).as_deref(),
            Some("main.rs"),
            "the bar names the shown file"
        );
        let panel = panel_action(app);
        assert!(panel.enabled, "a live project's panel toggle acts");
        assert_eq!(panel.on, app.panel_visible);

        let children: Vec<&str> = app.children().iter().map(|child| child.name()).collect();
        for drawn in ["FindBar", "Button", "BottomPanelToggle", "Text"] {
            assert!(
                !children.contains(&drawn),
                "the window draws no {drawn} of its own: {children:?}"
            );
        }
    });
}

#[test]
fn the_bar_shows_and_hides_the_bottom_panel() {
    let mut sample = synced_sample();
    let panel = sample.editor(|app| {
        let panel = app.panel_visible;
        app.run_window_bar_event(&ToolbarEvent::Action {
            id: BOTTOM_PANEL.into(),
        });
        panel
    });
    sample.editor(|app| {
        assert_eq!(app.panel_visible, !panel);
        assert_eq!(
            panel_action(app).on,
            !panel,
            "the toggle shows the panel's state"
        );
    });
}

#[test]
fn find_and_replace_open_the_window_search() {
    let mut sample = synced_sample();
    let asked = sample.with_doc(|app, doc| {
        app.apply_command(Command::Find, doc);
        let find = ui::window_toolbar::take_search_request();
        app.apply_command(Command::Replace, doc);
        let replace = ui::window_toolbar::take_search_request();
        (find, replace)
    });
    assert_eq!(
        asked,
        (Some(SearchOpening::Find), Some(SearchOpening::Replace))
    );
}

#[test]
fn the_window_search_finds_steps_and_replaces_in_the_buffer() {
    let mut sample = synced_sample();
    let found = sample.with_doc(|app, _doc| {
        let mut driver = SearchDriver::default();
        driver.open(InPlace::Document);
        driver.drive(app, "primes", FindOptions::default(), None, false);
        assert!(driver.abilities().replace, "the buffer can replace");
        let found = app.find_matches.len();
        assert!(
            found >= 2,
            "main.rs says primes more than once, got {found}"
        );
        assert_eq!(app.editor.find_match_count_for_test(), found);

        driver.drive(app, "primes", FindOptions::default(), Some(true), false);
        let m = app.find_matches[1];
        assert_eq!(driver.position(), Some((2, found)), "stepped to the second");
        assert_eq!(app.editor.primary_caret(), (m.line, m.col), "on the match");

        assert!(driver.replace(app, "primes", FindOptions::default(), "evens", false));
        assert_eq!(
            driver.position().map(|(_, total)| total),
            Some(found - 1),
            "one match fewer, found again in the edited text"
        );
        assert!(driver.replace(app, "primes", FindOptions::default(), "evens", true));
        assert_eq!(driver.matches(), 0);
        found
    });
    sample.app.settle();
    sample.with_doc(move |app, doc| {
        let text = buffer_text(app, doc);
        assert!(!text.contains("primes"), "every match was replaced");
        assert_eq!(
            text.matches("evens").count(),
            found,
            "the edits reached the document"
        );
    });
}

#[test]
fn a_capture_shows_the_panel_toggle_but_it_cannot_act() {
    let mut capture =
        ui::framework::testing::TestApp::<crate::CodeEditorShell>::capture(Some("selection"))
            .unwrap();
    let before = crate::test_support::editor(&mut capture, |app| {
        assert!(!panel_action(app).enabled);
        app.panel_visible
    });
    crate::test_support::editor(&mut capture, |app| {
        app.run_window_bar_event(&ToolbarEvent::Action {
            id: BOTTOM_PANEL.into(),
        });
    });
    assert_eq!(
        crate::test_support::editor(&mut capture, |app| app.panel_visible),
        before
    );
}
