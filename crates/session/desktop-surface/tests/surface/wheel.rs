//! What a wheel does to the window under it and to the strip it stands
//! in: which of the two a gesture moves, and which gestures move neither.

use super::*;

/// A wheel held with the zoom modifier is a zoom, which a browser also sends
/// for a trackpad's pinch: it reaches the window under the pointer like any
/// other and never pans the strip, sideways or not.
#[test]
fn a_zoom_wheel_never_pans_the_strip() {
    let (mut driver, handle, _) = mount_apps(&["Long", "B", "C", "D", "E"]);
    point_at_the_window(&mut driver, &handle);
    let home = pan_offset(&handle);
    driver.send(&ui::winit::event::WindowEvent::ModifiersChanged(
        ui::winit::event::Modifiers::from(ui::winit::keyboard::ModifiersState::CONTROL),
    ));
    // Sideways over an app that leaves the sideways wheel to the strip, and
    // the same over the strip's own chrome.
    for _ in 0..8 {
        page_wheel(&mut driver, 24.0, 0.0);
    }
    assert!(
        (pan_offset(&handle) - home).abs() < 0.5,
        "a zoom wheel pans nothing: {}",
        pan_offset(&handle)
    );
    // Over the window's bar, where a plain wheel pans the strip too.
    let (_, window) = handle.shown_tabs()[0];
    driver.send_host_event(&ui::synthetic_input::cursor_moved(
        window.center().x,
        window.y0 + 6.0,
    ));
    driver.relayout();
    for _ in 0..8 {
        page_wheel(&mut driver, 0.0, 24.0);
    }
    assert!(
        (pan_offset(&handle) - home).abs() < 0.5,
        "not over the window's bar either: {}",
        pan_offset(&handle)
    );

    // The modifier released, the same wheel pans the strip again.
    driver.send(&ui::winit::event::WindowEvent::ModifiersChanged(
        ui::winit::event::Modifiers::from(ui::winit::keyboard::ModifiersState::empty()),
    ));
    point_at_the_window(&mut driver, &handle);
    let before = pan_offset(&handle);
    for _ in 0..4 {
        page_wheel(&mut driver, 24.0, 0.0);
    }
    assert!(
        pan_offset(&handle) > before + 1.0,
        "without the modifier the wheel pans: {}",
        pan_offset(&handle)
    );
}
