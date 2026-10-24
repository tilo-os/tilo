//! The capture studio through the exported accessibility tree, as a
//! screen-reader user or an agent meets it: the studio is modal while
//! choosing, so a control missing from the tree is one they cannot use.

use crate::a11y::Action;
use crate::headless::FrameHarness;
use crate::state::State;
use crate::theme::Theme;
use crate::widget::{CaptureMode, CaptureStudio, SourceMode};

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
