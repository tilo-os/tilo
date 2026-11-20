//! What a click in a window hanging off the screen's edge does to the strip: a click in its
//! content leaves it where it is, a click on its title brings it into view once the click is over.

use super::*;

/// A click in a window hanging off the right edge focuses it and leaves the strip where it was: the
/// window stays under the pointer the person clicked, and the strip rests where their pan or the
/// keyboard left it. Only what has no pointer — a shortcut, a command, a window opening — reveals.
#[test]
fn a_click_in_a_window_partly_off_screen_leaves_it_where_it_was_clicked() {
    let (mut driver, handle) = mount_wide();
    let second = shown_rect(&handle, 1);
    assert!(
        second.x0 < 1200.0 && second.x1 > 1200.0,
        "the second window stands past the right edge: {second:?}"
    );
    driver.click_at(f64::midpoint(second.x0, 1200.0), second.center().y);
    settle_still(&mut driver);
    assert_eq!(
        shown_rect(&handle, 1),
        second,
        "the clicked window stays under the pointer"
    );
    let snapshot = handle.snapshot("g", vec![]);
    assert_eq!(
        snapshot.active.as_ref(),
        Some(&snapshot.entries[1].route),
        "and focused"
    );
}

/// A click on the title of a window hanging off the right edge says "this one": once the button is
/// up the strip eases it wholly into view, so its close and the rest of its bar can be reached. A
/// click in its content leaves it where it is
/// ([`a_click_in_a_window_partly_off_screen_leaves_it_where_it_was_clicked`]).
#[test]
fn a_click_on_the_title_of_a_window_partly_off_screen_brings_it_into_view_after_the_click() {
    let (mut driver, handle) = mount_wide();
    let second = shown_rect(&handle, 1);
    assert!(
        second.x1 > 1200.0,
        "the second window hangs past the right edge: {second:?}"
    );
    let before = pan_offset(&handle);
    // The window's bar stands just above its content: its title, past the tab's icon.
    driver.click_at(second.x0 + 60.0, second.y0 - 12.0);
    settle_still(&mut driver);
    let after = shown_rect(&handle, 1);
    assert!(
        after.x1 <= 1200.0 && after.x0 >= 0.0,
        "the window whose title was clicked stands wholly on screen: {after:?} (pan {before} -> {})",
        pan_offset(&handle)
    );
    let snapshot = handle.snapshot("g", vec![]);
    assert_eq!(
        snapshot.active.as_ref(),
        Some(&snapshot.entries[1].route),
        "and focused"
    );
}
