//! A button that hears about a finger after its node was built.

use super::*;

/// A button laid out for a mouse, told afterwards that a finger reaches it (a header's
/// toggle made before the window told it), is the finger's target tall the next time it
/// is laid out, with no node built anew.
#[test]
fn a_button_told_of_a_finger_after_its_node_was_built_grows_when_laid_out_again() {
    use crate::interaction::InteractionEnvironment;
    let mut harness = crate::headless::FrameHarness::new(
        crate::State::new(),
        Box::new(crate::widget::Row::new().push(Box::new(Button::icon_only(
            "pin",
            crate::icons::Icon::Pin,
            "Pin",
        )))),
        (400, 100),
    );
    let height =
        |harness: &crate::headless::FrameHarness| harness.inspection().root.children[0].rect[3];
    assert!((height(&harness) - 32.0).abs() < 0.5, "a mouse's 32");
    harness
        .root_widget_mut()
        .set_interaction_environment(InteractionEnvironment::TOUCH);
    harness.relayout();
    let touch = f64::from(InteractionEnvironment::TOUCH.minimum_target());
    assert!(
        (height(&harness) - touch).abs() < 0.5,
        "a finger's {touch}, not {}",
        height(&harness)
    );
}
