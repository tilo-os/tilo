use std::cell::Cell;
use std::rc::Rc;

use super::*;
use crate::headless::FrameHarness;
use crate::state::State;
use crate::widget::{Button, Column, Slot, Spacer};

/// A window that did not open, with a retry that counts its presses.
fn window_failure(retried: &Rc<Cell<u32>>) -> WindowFailure {
    let retry = Rc::clone(retried);
    WindowFailure::new(
        arrival_failure("This window didn't open", "Lounge did not answer.")
            .with_id("failed")
            .with_action(
                Button::new("failed.retry", "Try again")
                    .on_view(move || retry.set(retry.get() + 1)),
            ),
    )
}

fn harness(root: Box<dyn Widget>, size: (u32, u32)) -> FrameHarness {
    let mut harness = FrameHarness::new(State::new(), root, size);
    harness.relayout();
    harness
}

#[test]
fn a_window_failure_is_the_one_failure_banner_centred_at_the_readable_width() {
    let retried = Rc::new(Cell::new(0));
    let failure = window_failure(&retried);
    assert_eq!(failure.children()[0].name(), "Banner", "the shared face");
    let mut h = harness(Box::new(failure), (1600, 1000));
    let readable = f64::from(Theme::dark().tokens.metrics.readable_width);
    let face = h.targets()["failed"];
    assert!(
        (face.width() - readable).abs() < 0.5,
        "a wide window holds it at the readable width: {face:?}"
    );
    assert!(
        (face.center().x - 800.0).abs() <= 1.0 && (face.center().y - 500.0).abs() <= 1.0,
        "centred in the window: {face:?}"
    );
    let node = h.root_widget().children()[0]
        .accessibility()
        .expect("the face is announced");
    assert_eq!(node.name.as_deref(), Some("This window didn't open"));
    assert_eq!(node.description.as_deref(), Some("Lounge did not answer."));

    // A narrow window gives it its width, less the gutter, still centred.
    h.resize((400, 600)).unwrap();
    let face = h.targets()["failed"];
    assert!(face.width() < 400.0 && face.x0 > 0.0, "{face:?}");
    assert!((face.center().x - 200.0).abs() <= 1.0, "{face:?}");
}

#[test]
fn its_retry_answers_the_keyboard_the_pointer_and_assistive_technology() {
    let retried = Rc::new(Cell::new(0));
    let mut h = harness(Box::new(window_failure(&retried)), (900, 700));
    h.key("Tab");
    h.key("Enter");
    assert_eq!(retried.get(), 1, "Tab reaches it and Enter presses it");
    let retry = h.targets()["failed.retry"];
    h.click_at(retry.center().x, retry.center().y);
    assert_eq!(retried.get(), 2, "a click presses it");
    h.perform_named("failed.retry", &crate::a11y::Action::Activate)
        .expect("the retry is addressable");
    assert_eq!(retried.get(), 3, "and so does an assistive activation");
    assert!(
        h.native_app_audit().is_certified(),
        "{:?}",
        h.native_app_audit()
    );
}

#[test]
fn it_stands_at_rest_from_its_first_frame_where_a_window_brings_it_in() {
    // The same banner on its own opens where it arrives in a shown window;
    // the window's face does not.
    for in_window in [false, true] {
        let (slot, handle) = Slot::new(Box::new(Spacer::fixed(10.0)));
        let mut h = harness(Box::new(Column::new().push(Box::new(slot))), (900, 700));
        let _frame = crate::motion::pin_frame();
        let retried = Rc::new(Cell::new(0));
        let face: Box<dyn Widget> = if in_window {
            Box::new(window_failure(&retried))
        } else {
            Box::new(
                arrival_failure("This window didn't open", "Lounge did not answer.")
                    .with_id("failed"),
            )
        };
        handle.set(face);
        h.relayout();
        h.relayout();
        assert_eq!(
            h.root_widget().needs_animation_frame(),
            !in_window,
            "in a window it does not open on its own: the window's arrival cross-fades it"
        );
        if in_window {
            let face = h.targets()["failed"];
            assert!(face.height() > 0.0, "laid out at its full height: {face:?}");
        }
    }
}
