//! The presentation swaps its slide with the theme's cross-fade: the slide a person steps away
//! from stays laid out while it fades out, the one that comes fades in over it, and once the swap
//! has played only the new one stands.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

mod support;

use support::{accessible, settle};

use tilolang_window::Window;

/// The app's own name, as its package declares it.
const APP: &str = "so.tilo.slides";

/// A wide window, where the stage and its rail stand side by side.
const WINDOW: (u32, u32) = (1280, 800);

/// The window at `WINDOW`, its motion on: the harness opens every window with reduced motion, and
/// this test is about the fade.
fn open() -> Window {
    let mut window = support::open_app(
        APP,
        &support::packages_of("slides"),
        WINDOW,
        ui::Theme::light(),
    );
    window
        .harness
        .set_theme(ui::Theme::light().with_reduce_motion(false));
    settle(&mut window);
    window
}

/// Press the control `name` where it stands, as a person does.
fn press(window: &mut Window, name: &str) {
    let node = accessible(window)
        .into_iter()
        .find(|node| {
            node.role == ui::a11y::Role::Button
                && node.name.as_deref() == Some(name)
                && node.bounds.is_some()
        })
        .map_or_else(
            || {
                let names: Vec<String> = accessible(window)
                    .into_iter()
                    .filter_map(|node| node.name)
                    .collect();
                panic!("the window offers `{name}`: {names:?}")
            },
            |node| node.bounds.expect("the control's bounds"),
        );
    window.harness.click_at(
        f64::midpoint(node.x0, node.x1),
        f64::midpoint(node.y0, node.y1),
    );
    settle(window);
}

/// How many pages the cross-fading stage lays out beside each other, while one is swapping:
/// `Some(2)` mid-swap, `Some(1)` for a swap that has landed but not yet been let go, `None`
/// when the stage stands on one slide.
fn pages_in_the_stage(widget: &dyn ui::widget::Widget) -> Option<usize> {
    if let Some(swapped) = widget
        .as_any()
        .and_then(|any| any.downcast_ref::<ui::widget::Swapped>())
    {
        return Some(if swapped.previous().is_some() { 2 } else { 1 });
    }
    let mut found = None;
    widget.for_each_child(&mut |child| {
        if found.is_none() {
            found = pages_in_the_stage(child);
        }
    });
    found
}

/// A slide stepped to while presenting keeps the slide it left laid out under the one that comes
/// until the cross-fade has played.
#[test]
fn a_slide_change_while_presenting_cross_fades_between_the_two_pages() {
    let mut window = open();
    // Two slides, so the presentation has somewhere to step to: the deck's starter gives way to
    // the first added slide, so two presses are what leaves two.
    press(&mut window, "Add slide");
    press(&mut window, "Add slide");
    press(&mut window, "Present");
    assert!(
        ui::widget::find_test_id(window.harness.root_widget(), "Presentation").is_some(),
        "the presentation's stage is shown"
    );
    assert_eq!(
        pages_in_the_stage(window.harness.root_widget()),
        None,
        "the stage shows one slide and has swapped nothing"
    );

    press(&mut window, "Next");
    assert_eq!(
        pages_in_the_stage(window.harness.root_widget()),
        Some(2),
        "the slide stepped away from fades out under the one that comes"
    );

    // Once the fade has played, the slide that came stands on its own.
    window
        .harness
        .settle_for_capture(ui::time::Duration::from_secs(2));
    settle(&mut window);
    assert_ne!(
        pages_in_the_stage(window.harness.root_widget()),
        Some(2),
        "the swap has landed"
    );
    assert!(
        ui::widget::find_test_id(window.harness.root_widget(), "Presentation").is_some(),
        "the stage stands on the slide that came"
    );
}
