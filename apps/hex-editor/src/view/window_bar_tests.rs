//! The hex editor's controls live in the window's bar: the file's name, the
//! edit mode, the inspector's toggle, History, and find and replace in the
//! bytes.

use std::rc::Rc;

use ui::framework::ResourceSurface;
use ui::framework::testing::TestApp;
use ui::widget::toast::ToastKind;
use ui::widget::{FindMode, FindOptions, InPlace, SearchDriver};
use ui::window_toolbar::{SearchOpening, ToolbarEvent};

use super::{INSERT, INSPECTOR, OVERWRITE};
use crate::model::EditMode;
use crate::{HexEditor, HexEditorShell, HexView};

const HEX: FindOptions = FindOptions {
    mode: FindMode::Hex,
    match_case: false,
    whole_word: false,
};

fn sample() -> HexView {
    let mut view = HexView::empty();
    view.present(&Rc::new(HexEditor {
        name: Some(ui::tilo_resource::Atom("sample.png".into())),
        bytes: crate::buffer::ByteBuffer::of(&crate::sample::png_image()),
        opened: 1,
    }));
    view
}

fn action(id: &str) -> ToolbarEvent {
    ToolbarEvent::Action { id: id.into() }
}

/// Every name and action target in `node`'s subtree.
fn labels(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
    out.extend(node.name.clone());
    out.extend(node.action_target.clone());
    for child in &node.children {
        labels(child, out);
    }
}

/// The editor on a file of `bytes`, launched as a person opens it.
fn launched(bytes: &[u8]) -> (tempfile::TempDir, TestApp<HexEditorShell>) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("bytes.bin");
    std::fs::write(&path, bytes).expect("the file is written");
    let mut app = TestApp::<HexEditorShell>::launched(&[path.to_str().expect("a UTF-8 path")])
        .expect("the editor opens the file");
    app.harness().relayout();
    (directory, app)
}

/// Run the window's search for `query` on the view and replace the current
/// match (or every match) with `with`; whether the bytes changed.
fn replace(
    app: &mut TestApp<HexEditorShell>,
    query: &str,
    options: FindOptions,
    with: &str,
    all: bool,
) -> bool {
    let view = app.surface_mut::<HexView>();
    let mut driver = SearchDriver::default();
    driver.open(InPlace::Document);
    driver.drive(view, query, options, None, false);
    let abilities = driver.abilities();
    assert!(abilities.replace && abilities.hex, "{abilities:?}");
    let changed = driver.replace(view, query, options, with, all);
    app.settle();
    changed
}

#[test]
fn the_view_draws_no_header_row_and_its_controls_are_in_the_window_bar() {
    let view = sample();
    let toolbar = view.window_toolbar();
    assert_eq!(
        toolbar.location.map(|location| location.label).as_deref(),
        Some("sample.png"),
        "the bar names the file"
    );
    let views = toolbar.views.expect("the edit mode is the bar's views");
    let ids: Vec<&str> = views.choices.iter().map(|view| view.id.as_str()).collect();
    assert_eq!(ids, [OVERWRITE, INSERT]);
    assert_eq!(views.chosen, OVERWRITE);
    let inspector = toolbar
        .actions
        .iter()
        .find(|action| action.id == INSPECTOR)
        .expect("the inspector's toggle is a bar action");
    assert_eq!(inspector.on, view.drawer.is_open());

    let mut drawn = Vec::new();
    labels(
        &ui::Widget::accessibility(&view).expect("the view's semantics"),
        &mut drawn,
    );
    for own in [
        "Show inspector",
        "Hide inspector",
        "Overwrite",
        "Insert",
        ui::widget::HISTORY_CONTROL_ID,
        INSPECTOR,
    ] {
        assert!(
            !drawn.iter().any(|label| label == own),
            "the view draws no {own} of its own: {drawn:?}"
        );
    }

    assert_eq!(
        HexView::empty().window_toolbar(),
        ui::window_toolbar::WindowToolbar::default(),
        "with no file open the bar has nothing of the editor's"
    );
}

#[test]
fn the_bar_switches_the_edit_mode_and_shows_and_hides_the_inspector() {
    let mut view = sample();
    view.run_window_bar_event(&action(INSERT));
    assert_eq!(view.model.mode(), EditMode::Insert);
    assert_eq!(
        view.window_toolbar().views.map(|views| views.chosen),
        Some(INSERT.to_owned())
    );
    view.run_window_bar_event(&action(OVERWRITE));
    assert_eq!(view.model.mode(), EditMode::Overwrite);

    let open = view.drawer.is_open();
    view.run_window_bar_event(&action(INSPECTOR));
    assert_eq!(view.drawer.is_open(), !open);
    let toggle = view
        .window_toolbar()
        .actions
        .into_iter()
        .find(|action| action.id == INSPECTOR)
        .expect("the inspector's toggle");
    assert_eq!(toggle.on, !open, "the toggle shows the inspector's state");
}

#[test]
fn history_is_the_window_bars_control() {
    let mut app = TestApp::<HexEditorShell>::capture(None).expect("the sample opens");
    app.harness().present_in_window_bar();
    app.harness().relayout();
    assert_eq!(
        app.harness().window_history_control(),
        Some(ui::widget::HistoryControl::Closed),
        "the window's bar offers History"
    );
    app.harness().press_window_history().expect("History opens");
    app.settle();
    assert_eq!(
        app.harness().window_history_control(),
        Some(ui::widget::HistoryControl::Open)
    );
}

#[test]
fn find_and_replace_open_the_window_search() {
    let (_directory, mut app) = launched(&[0xde, 0xad, 0xbe, 0xef]);
    app.harness()
        .perform_named("hex_view", &ui::a11y::Action::Focus)
        .expect("the grid takes the keyboard");
    let mut chord = |key: &str| {
        app.harness().set_modifiers(&["Ctrl".to_owned()]);
        app.harness().key(key);
        app.harness().set_modifiers(&[]);
        app.settle();
        ui::window_toolbar::take_search_request()
    };
    assert_eq!(chord("f"), Some(SearchOpening::Find));
    assert_eq!(chord("h"), Some(SearchOpening::Replace));
}

#[test]
fn the_window_search_replaces_hex_bytes_as_one_undo_step() {
    let (_directory, mut app) = launched(&[0xde, 0xad, 0x00, 0xde, 0xad, 0x01]);
    assert!(replace(&mut app, "de ad", HEX, "be ef", false));
    app.settle();
    assert_eq!(app.state().bytes, [0xbe, 0xef, 0x00, 0xde, 0xad, 0x01]);
    assert!(replace(&mut app, "de ad", HEX, "ca fe", true));
    app.settle();
    assert_eq!(app.state().bytes, [0xbe, 0xef, 0x00, 0xca, 0xfe, 0x01]);
    assert!(app.undo(), "replacing every match is one step");
    assert_eq!(app.state().bytes, [0xbe, 0xef, 0x00, 0xde, 0xad, 0x01]);
    assert!(app.undo(), "replacing one match is one step");
    assert_eq!(app.state().bytes, [0xde, 0xad, 0x00, 0xde, 0xad, 0x01]);
}

/// A clipboard this test has to itself: the process clipboard is shared with
/// every test running beside it.
fn own_clipboard() -> ui::host::HostScope {
    let (services, _recorder) = ui::host::HostServices::recording();
    ui::host::enter(&services)
}

/// Press `key` with Ctrl held on the grid.
fn chord(app: &mut TestApp<HexEditorShell>, key: &str) {
    app.harness().set_modifiers(&["Ctrl".to_owned()]);
    app.harness().key(key);
    app.harness().set_modifiers(&[]);
    app.settle();
}

#[test]
fn copying_pasting_and_cutting_carry_bytes_in_the_focus_mode() {
    let _clipboard = own_clipboard();
    let (_directory, mut app) = launched(&[0xde, 0xad, 0xbe, 0xef]);
    app.harness()
        .perform_named("hex_view", &ui::a11y::Action::Focus)
        .expect("the grid takes the keyboard");
    chord(&mut app, "a");
    chord(&mut app, "c");
    assert_eq!(ui::clipboard_paste().as_deref(), Some("de ad be ef"));

    // Pasted back over the whole selection as one step: one Undo returns the
    // file as it was opened. Overwrite mode keeps the length, so the paste is
    // the same size as what it replaces.
    ui::clipboard_copy("ca fe ff 00");
    chord(&mut app, "v");
    assert_eq!(app.state().bytes, [0xca, 0xfe, 0xff, 0x00]);
    assert!(app.undo(), "a paste is one step");
    assert_eq!(app.state().bytes, [0xde, 0xad, 0xbe, 0xef]);

    // Cut takes the bytes away and keeps them on the clipboard, as one step.
    chord(&mut app, "a");
    chord(&mut app, "x");
    assert_eq!(ui::clipboard_paste().as_deref(), Some("de ad be ef"));
    assert!(app.state().bytes.is_empty(), "cut removed the selection");
    assert!(app.undo(), "a cut is one step");
    assert_eq!(app.state().bytes, [0xde, 0xad, 0xbe, 0xef]);
}

#[test]
fn paste_reads_the_clipboard_in_the_column_the_person_is_in() {
    let _clipboard = own_clipboard();
    let (_directory, mut app) = launched(b"PNGD");
    app.harness()
        .perform_named("hex_view", &ui::a11y::Action::Focus)
        .expect("the grid takes the keyboard");
    // Insert mode, so a paste may be another length than the selection, and
    // the ASCII column, which reads text and writes its UTF-8 bytes.
    let view = app.surface_mut::<HexView>();
    view.model_mut().toggle_mode();
    view.model_mut().toggle_column();
    app.harness().relayout();
    ui::clipboard_copy("!?");
    chord(&mut app, "a");
    chord(&mut app, "v");
    assert_eq!(app.state().bytes, b"!?");

    // The hex column reads hexadecimal bytes.
    let view = app.surface_mut::<HexView>();
    view.model_mut().toggle_column();
    app.harness().relayout();
    ui::clipboard_copy("de ad");
    chord(&mut app, "a");
    chord(&mut app, "v");
    assert_eq!(app.state().bytes, [0xde, 0xad]);

    // Hex text the column cannot read is refused, and the file is untouched.
    ui::clipboard_copy("not bytes");
    chord(&mut app, "v");
    assert_eq!(app.state().bytes, [0xde, 0xad]);
}

#[test]
fn insert_mode_replaces_text_with_utf8_bytes_of_another_length_as_one_undo_step() {
    let (_directory, mut app) = launched(b"cat, cat; cat");
    app.surface_mut::<HexView>()
        .run_window_bar_event(&action(INSERT));
    assert!(replace(
        &mut app,
        "cat",
        FindOptions::default(),
        "lynx\u{e4}",
        true
    ));
    app.settle();
    assert_eq!(
        app.state().bytes,
        "lynx\u{e4}, lynx\u{e4}; lynx\u{e4}".as_bytes()
    );
    assert!(app.undo());
    assert_eq!(app.state().bytes, b"cat, cat; cat");
}

#[test]
fn overwrite_mode_refuses_a_replacement_that_changes_the_length() {
    let (_directory, mut app) = launched(&[0xde, 0xad, 0x00]);
    assert!(!replace(&mut app, "de ad", HEX, "be", true));
    assert!(!replace(&mut app, "de ad", HEX, "be ef 01", false));
    app.settle();
    assert_eq!(
        app.state().bytes,
        [0xde, 0xad, 0x00],
        "the file keeps its length"
    );
    let told = app
        .surface_mut::<HexView>()
        .said
        .get()
        .map(|(text, kind)| (text.clone(), *kind));
    assert!(
        told.as_ref().is_some_and(|(text, kind)| {
            text.starts_with("Couldn't replace in Overwrite mode") && *kind == ToastKind::Error
        }),
        "{told:?}"
    );
    assert!(!app.undo(), "nothing was changed to undo");
}
