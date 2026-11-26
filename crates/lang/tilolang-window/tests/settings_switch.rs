//! The pane an app opens comes forward, and what a window shows changes through the shared
//! navigation shell: on a phone, where the list of places stands in front, the pane a search row
//! or an agent opens is the one a person sees, as a place chosen from the list would be.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

mod support;

use support::{accessible, open, open_pane, said, shows};

/// A phone's window, where the shell shows one pane at a time.
const PHONE: (u32, u32) = (390, 844);

/// A wide window, where the list and the content stand side by side.
const LAPTOP: (u32, u32) = (1280, 800);

fn places_in_front(window: &tilolang_window::Window) -> Vec<String> {
    accessible(window)
        .into_iter()
        .filter(|node| node.role == tilo_ui_base::a11y::Role::TreeItem)
        .filter(|node| {
            node.bounds.is_some_and(|b| {
                b.x0 >= -0.5
                    && b.x1 <= f64::from(PHONE.0) + 0.5
                    && b.y0 >= -0.5
                    && b.y1 <= f64::from(PHONE.1) + 0.5
                    && b.x1 - b.x0 > 1.0
                    && b.y1 - b.y0 > 1.0
            })
        })
        .filter_map(|node| node.name)
        .collect()
}

/// A phone's window opens on the list of places, and the pane a setting or an agent opens comes
/// forward in its place.
#[test]
fn the_pane_an_app_opens_comes_forward_on_a_phone() {
    let mut window = open(PHONE, tilo_ui_base::Theme::light());
    assert!(
        !places_in_front(&window).is_empty(),
        "a phone's window opens on the list of places: {:?}",
        said(&window)
    );
    assert!(
        !shows(&window, "Output"),
        "and not on a pane: {:?}",
        said(&window)
    );

    open_pane(&mut window, "sound");

    assert!(
        shows(&window, "Output"),
        "the pane the app opened is the one shown: {:?}",
        said(&window)
    );
    assert!(
        places_in_front(&window).is_empty(),
        "the list goes behind what the app showed: {:?}",
        said(&window)
    );
}

/// A wide window keeps its places beside the pane it shows, whichever pane that is.
#[test]
fn a_wide_window_keeps_its_places_beside_the_pane() {
    let mut window = open(LAPTOP, tilo_ui_base::Theme::light());
    open_pane(&mut window, "power");
    assert!(shows(&window, "Power mode"), "{:?}", said(&window));
    let places: Vec<String> = accessible(&window)
        .into_iter()
        .filter(|node| node.role == tilo_ui_base::a11y::Role::TreeItem)
        .filter_map(|node| node.name)
        .collect();
    assert!(
        places.iter().any(|place| place == "Sound"),
        "the list of places stands beside the pane: {places:?}"
    );
}
