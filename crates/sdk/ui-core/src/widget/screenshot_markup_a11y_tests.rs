//! The markup editor through the exported accessibility tree and the
//! keyboard, the way a screen-reader or keyboard-only person meets it: the
//! editor traps focus, so a toolbar control that is not in the tree, or that
//! Tab cannot reach, is one they cannot use at all.

use std::sync::Arc;

use super::*;
use crate::a11y::Action;
use crate::headless::FrameHarness;
use crate::state::State;
use crate::theme::Theme;

fn harness(width: u32) -> FrameHarness {
    let rgba: Arc<[u8]> = Arc::from(vec![20_u8; 64 * 64 * 4]);
    let mut h = FrameHarness::for_root(
        State::new(),
        ScreenshotMarkup::new(rgba, 64, 64),
        (width, 700),
        Theme::dark().with_reduce_motion(true),
    );
    h.relayout_at((width, 700)).expect("layout");
    h
}

fn activate(h: &mut FrameHarness, name: &str) {
    let node = h
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("{name} is not in the accessibility tree"));
    h.perform_a11y_action(node, &Action::Activate)
        .unwrap_or_else(|error| panic!("activating {name} failed: {error}"));
}

fn commands(h: &mut FrameHarness) -> Vec<ScreenshotMarkupCommand> {
    let taken = h.root_mut::<ScreenshotMarkup>().take_commands();
    h.relayout_at((1200, 700)).expect("layout");
    taken
}

fn focused_label(h: &FrameHarness) -> Option<String> {
    h.a11y_focus()
        .and_then(|(_, node)| node.label().map(str::to_owned))
}

/// Tab until the keyboard is on `name`, failing rather than looping when
/// Tab never reaches it.
fn tab_to(h: &mut FrameHarness, name: &str) {
    for _ in 0..40 {
        if focused_label(h).as_deref() == Some(name) {
            return;
        }
        h.key("Tab");
    }
    panic!("Tab never reaches {name}");
}

#[test]
fn every_toolbar_control_is_named_and_operable_through_accessibility() {
    let mut h = harness(1200);
    for name in [
        "Markup",
        "Close",
        "Undo",
        "Redo",
        "Pen tools",
        "Fit image",
        "Red",
        "Yellow",
        "Blue",
        "Green",
        "White",
        "Open in app",
        "Copy",
        "Save copy",
    ] {
        assert!(
            h.a11y_node_named(name).is_some(),
            "{name} is painted on the markup editor but absent from the accessibility tree"
        );
    }

    activate(&mut h, "Copy");
    assert!(
        matches!(
            commands(&mut h).as_slice(),
            [ScreenshotMarkupCommand::Copy(_)]
        ),
        "activating Copy copies the marked-up image"
    );
    activate(&mut h, "Open in app");
    assert_eq!(
        commands(&mut h),
        vec![ScreenshotMarkupCommand::OpenExternal]
    );

    // The tool button discloses the tools; picking one is a checked choice
    // that closes the picker and renames the button after it.
    assert!(
        h.a11y_node_named("Arrow").is_none(),
        "the picker starts closed"
    );
    activate(&mut h, "Pen tools");
    activate(&mut h, "Arrow");
    assert_eq!(h.root::<ScreenshotMarkup>().tool, MarkupTool::Arrow);
    assert!(h.a11y_node_named("Arrow tools").is_some());
    assert!(
        h.a11y_node_named("Arrow").is_none(),
        "the choice closes the picker"
    );

    activate(&mut h, "Blue");
    assert_eq!(h.root::<ScreenshotMarkup>().color, SWATCHES[2]);
    let blue = h.a11y_node_named("Blue").expect("blue swatch");
    let checked = h
        .a11y_nodes()
        .into_iter()
        .find(|(id, _)| *id == blue)
        .and_then(|(_, node)| node.toggled());
    assert_eq!(
        checked,
        Some(accesskit::Toggled::True),
        "the chosen colour reads as checked"
    );

    activate(&mut h, "Close");
    assert_eq!(commands(&mut h), vec![ScreenshotMarkupCommand::Cancel]);
}

#[test]
fn a_narrow_editor_still_offers_every_colour_through_accessibility() {
    let mut h = harness(440);
    assert!(
        h.a11y_node_named("Red").is_none(),
        "swatches collapse behind Colours"
    );
    activate(&mut h, "Colours");
    activate(&mut h, "Green");
    assert_eq!(h.root::<ScreenshotMarkup>().color, SWATCHES[3]);
}

#[test]
fn tab_reaches_every_toolbar_control_and_enter_activates_the_one_it_is_on() {
    let mut h = harness(1200);
    // Opening the editor puts the keyboard on it; each Tab then moves to
    // the next toolbar control in reading order, wrapping after the last.
    assert_eq!(focused_label(&h).as_deref(), Some("Markup"));
    let mut visited = Vec::new();
    for _ in 0..14 {
        h.key("Tab");
        visited.push(focused_label(&h).unwrap_or_default());
    }
    assert_eq!(
        visited,
        [
            "Close",
            "Undo",
            "Redo",
            "Pen tools",
            "Fit image",
            "Red",
            "Yellow",
            "Blue",
            "Green",
            "White",
            "Open in app",
            "Copy",
            "Save copy",
            "Close",
        ]
    );

    // Enter on Copy copies; it does not save.
    tab_to(&mut h, "Copy");
    h.key("Enter");
    assert!(
        matches!(
            commands(&mut h).as_slice(),
            [ScreenshotMarkupCommand::Copy(_)]
        ),
        "Enter activates the focused control"
    );

    // The tool picker opens from the keyboard, its choices follow the tool
    // button, and choosing one returns the keyboard to the button.
    tab_to(&mut h, "Pen tools");
    h.key("Space");
    h.key("Tab");
    assert_eq!(focused_label(&h).as_deref(), Some("Pen"));
    h.key("Tab");
    h.key("Tab");
    assert_eq!(focused_label(&h).as_deref(), Some("Arrow"));
    h.key("Enter");
    assert_eq!(h.root::<ScreenshotMarkup>().tool, MarkupTool::Arrow);
    assert_eq!(focused_label(&h).as_deref(), Some("Arrow tools"));
}
