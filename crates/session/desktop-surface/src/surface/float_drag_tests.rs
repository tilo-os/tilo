//! A window dragged past every display: where the float it becomes belongs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures: a failed step is the assertion"
)]

use super::DesktopSurface;
use super::tests::{carry, desktop};
use super::*;
use tilo_desktop_model::Display;
use ui::kurbo::Point;

/// A tab dragged out beyond every display is let go where no strip shows: its window floats there,
/// in the display the hand let it go over, and belongs to that display from then on.
#[test]
fn a_tab_dragged_off_the_screen_floats_on_the_display_it_is_let_go_over() {
    let (surface, handle) = desktop(2);
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1280, 800),
        Theme::dark(),
    );
    let display = |id: u32, x: f64| Display {
        id,
        monitor: format!("monitor-{id}"),
        x,
        y: 0.0,
        width: 640.0,
        height: 800.0,
        primary: false,
    };
    assert!(handle.set_displays(&[display(0, 0.0), display(1, 640.0)]));
    for _ in 0..4 {
        frame.relayout();
    }
    let tab = handle.read(|tree| tree.workspaces[0].strips[0].window_groups[0].tabs[0].id);
    let pill = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .and_then(|row| row.groups.first()?.tab_rects().first().copied())
        .expect("the first window shows its tab");
    // Past the right display's edge: no strip shows there, so the window is let go floating.
    carry(&mut frame, pill.center(), Point::new(1400.0, 400.0));
    let group = handle
        .read(|tree| {
            tree.workspaces[0].strips[0]
                .window_groups
                .iter()
                .find(|g| g.tabs.iter().any(|t| t.id == tab))
                .map(|g| g.id)
        })
        .expect("the first window is still the person's");
    assert!(
        handle.read(|tree| tree.workspaces[0].layout.float_of(group).is_some()),
        "its window floats where it was let go"
    );
    assert_eq!(
        handle.display_of(group),
        Some(1),
        "and belongs to the display it was let go over"
    );
}
