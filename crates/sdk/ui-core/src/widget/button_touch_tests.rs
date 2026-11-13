//! A button that hears about a finger after its node was built.

use super::*;

#[test]
fn a_bar_control_keeps_the_bars_height_and_reaches_a_finger_from_its_hit_rect() {
    use crate::interaction::InteractionEnvironment;
    let theme = crate::theme::Theme::light();
    let mut button = Button::icon_only("new", crate::icons::Icon::Plus, "New");
    button.set_interaction_environment(InteractionEnvironment::TOUCH);
    let grown = button.face_height();
    assert!(
        grown >= f64::from(InteractionEnvironment::TOUCH.minimum_target()),
        "a control outside a bar grows to the finger's target: {grown}"
    );
    button.set_window_bar_control(true);
    let bar = button.face_height();
    assert_eq!(bar, ControlSize::Regular.height(&theme));
    assert!(bar < grown, "the bar keeps its control height: {bar}");
    button.rect = vello::kurbo::Rect::new(10.0, 4.0, 10.0 + bar, 4.0 + bar);
    assert_eq!(
        button.hit_rect().height(),
        grown,
        "the finger's target stays in the hit rect"
    );
}

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

/// A bar's controls stand a cluster gap apart, so no control's press target reaches into the
/// one beside it: a mouse's face is the bar's control height and its target reaches past the
/// face into the gap, which at a cluster gap is exactly the room the face leaves; a finger's
/// face is the target itself, and the gap is air between two whole targets. The bar is as wide
/// as a phone and insets what it holds as a bar's body does, so the row has nowhere to put the
/// controls but beside each other.
#[test]
fn bar_controls_a_cluster_gap_apart_keep_their_press_targets_off_each_other() {
    use crate::interaction::InteractionEnvironment;
    use crate::tokens::Rhythm;
    for pointer in [
        InteractionEnvironment::POINTER,
        InteractionEnvironment::TOUCH,
    ] {
        let mut row = crate::widget::Row::new()
            .with_rhythm(Rhythm::Related)
            .with_padding_rhythm(Rhythm::Tight, Rhythm::Tight);
        for key in ["menu", "workspace", "bell"] {
            row = row.push(Box::new(
                Button::icon_only(key, crate::icons::Icon::Bell, key).role(ButtonRole::Bar),
            ));
        }
        let mut harness =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(row), (320, 60));
        harness
            .root_widget_mut()
            .set_interaction_environment(pointer);
        harness.relayout();
        let faces = harness.inspection().root.children;
        let targets: Vec<Rect> = faces
            .iter()
            .map(|face| {
                let rect = Rect::new(
                    face.rect[0],
                    face.rect[1],
                    face.rect[0] + face.rect[2],
                    face.rect[1] + face.rect[3],
                );
                pointer.minimum_target_rect(rect)
            })
            .collect();
        for (index, target) in targets.iter().enumerate() {
            assert!(
                target.x0 >= 0.0 && target.x1 <= 320.0,
                "control {index} reaches from {} to {} in a window 320 wide",
                target.x0,
                target.x1
            );
            if let Some(next) = targets.get(index + 1) {
                assert!(
                    target.x1 <= next.x0 + 0.01,
                    "control {index} reaches to {} and control {} begins at {}: their press \
                     targets share the press between them ({pointer:?})",
                    target.x1,
                    index + 1,
                    next.x0
                );
            }
        }
    }
}
