//! The capture studio through the exported accessibility tree, as a
//! screen-reader user or an agent meets it: the studio is modal while
//! choosing, so a control missing from the tree is one they cannot use.

use crate::a11y::Action;
use crate::headless::FrameHarness;
use crate::state::State;
use crate::theme::Theme;
use crate::widget::{
    AudioCaptureMode, CaptureMode, CapturePreview, CaptureStudio, ScreenshotCommand,
    ScreenshotKept, SourceMode,
};

fn harness() -> FrameHarness {
    let mut studio = CaptureStudio::default();
    studio.open();
    let mut h = FrameHarness::for_root(
        State::new(),
        studio,
        (1600, 1000),
        Theme::dark().with_reduce_motion(true),
    );
    h.relayout_at((1600, 1000)).expect("layout");
    h
}

fn activate(h: &mut FrameHarness, name: &str) {
    let node = h
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("{name} is not in the accessibility tree"));
    h.perform_a11y_action(node, &Action::Activate)
        .unwrap_or_else(|error| panic!("activating {name} failed: {error}"));
}

fn studio(h: &FrameHarness) -> &CaptureStudio {
    h.root::<CaptureStudio>()
}

#[test]
fn the_capture_bar_is_named_and_operable_through_accessibility() {
    let mut h = harness();
    for name in [
        "Capture",
        "Screenshot",
        "Recording",
        "Picture in picture",
        "Selection",
        "Window",
        "Screen",
        "Strip",
        "Options",
    ] {
        assert!(
            h.a11y_node_named(name).is_some(),
            "{name} is painted on the capture bar but absent from the accessibility tree"
        );
    }

    activate(&mut h, "Recording");
    assert_eq!(studio(&h).mode(), CaptureMode::Recording);
    assert!(
        h.a11y_node_named("Start recording").is_some(),
        "the shutter is named after what it does now"
    );
    activate(&mut h, "Screenshot");
    activate(&mut h, "Screen");
    assert_eq!(studio(&h).source_mode(), SourceMode::Display);

    // The options open from the tree, and a choice in them applies.
    let pointer_before = studio(&h).include_pointer;
    activate(&mut h, "Options");
    activate(&mut h, "Show pointer");
    assert_ne!(studio(&h).include_pointer, pointer_before);
    assert!(
        h.a11y_node_named("Show pointer").is_none(),
        "choosing closes the options"
    );

    activate(&mut h, "Take screenshot");
    let intents = h.root_mut::<CaptureStudio>().take_intents();
    assert_eq!(intents.len(), 1, "the shutter takes the screenshot");
}

#[test]
fn a_shutter_with_nothing_to_capture_is_disabled_and_refuses_activation() {
    let mut h = harness();
    activate(&mut h, "Selection");
    let shutter = h.a11y_node_named("Take screenshot").expect("shutter");
    let disabled = h
        .a11y_nodes()
        .into_iter()
        .find(|(id, _)| *id == shutter)
        .is_some_and(|(_, node)| node.is_disabled());
    assert!(
        disabled,
        "no selection yet: the shutter reads as unavailable"
    );
    assert!(h.perform_a11y_action(shutter, &Action::Activate).is_err());
    assert!(h.root_mut::<CaptureStudio>().take_intents().is_empty());
}

#[test]
fn a_receipt_action_from_the_tree_leaves_no_send_menu_behind() {
    let mut h = harness();
    {
        let studio = h.root_mut::<CaptureStudio>();
        studio.set_screenshot_finished(
            ScreenshotKept::SAVED,
            800,
            600,
            CapturePreview {
                columns: 1,
                rows: 1,
                pixels: vec![[20, 40, 60, 255]],
            },
        );
        studio.set_screenshot_transition_progress(1_000);
    }
    h.relayout_at((1600, 1000)).expect("layout");

    activate(&mut h, "Send\u{2026}");
    assert!(studio(&h).send_menu_open(), "Send opens its menu");
    activate(&mut h, "Copy");
    let commands: Vec<_> = h
        .root_mut::<CaptureStudio>()
        .take_intents()
        .into_iter()
        .filter_map(|intent| intent.screenshot_command)
        .collect();
    assert_eq!(commands, vec![ScreenshotCommand::CopyLast]);
    assert!(
        !studio(&h).send_menu_open(),
        "the send menu closes with the receipt it belongs to"
    );

    // Nor does a menu left over from an earlier receipt reach the next
    // capture, where it would take Enter and send the last screenshot.
    let studio_mut = h.root_mut::<CaptureStudio>();
    studio_mut.set_screenshot_finished(
        ScreenshotKept::SAVED,
        800,
        600,
        CapturePreview {
            columns: 1,
            rows: 1,
            pixels: vec![[20, 40, 60, 255]],
        },
    );
    studio_mut.set_screenshot_transition_progress(1_000);
    h.relayout_at((1600, 1000)).expect("layout");
    activate(&mut h, "Send\u{2026}");
    h.root_mut::<CaptureStudio>().open();
    assert!(
        !studio(&h).send_menu_open(),
        "a new capture opens without the last receipt's send menu"
    );
}

#[test]
fn keeping_the_last_30_seconds_is_not_announced_as_a_recording() {
    let mut h = harness();
    activate(&mut h, "Recording");
    h.root_mut::<CaptureStudio>()
        .set_replay_active(true, AudioCaptureMode::None);
    h.relayout_at((1600, 1000)).expect("layout");
    assert!(
        h.a11y_node_named("Keep last 30 s").is_some(),
        "the status bar is named for what it keeps"
    );
    assert!(h.a11y_node_named("Stop keeping the last 30 s").is_some());
    assert!(h.a11y_node_named("Recording").is_none());
}
