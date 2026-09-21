#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::widget::Scrollable;

#[test]
fn scrollable_clamps_to_content_bounds() {
    let mut s = Scrollable::new_vertical();
    s.set_content_size(200.0, 1000.0);
    s.set_viewport_size(300.0, 100.0);
    s.scroll_by(0.0, 50_000.0);
    assert_eq!(s.offset_y(), 900.0);
    s.scroll_by(0.0, -50_000.0);
    assert_eq!(s.offset_y(), 0.0);
}

#[test]
fn scrollable_scroll_into_view_vertical() {
    let mut s = Scrollable::new_vertical();
    s.set_content_size(200.0, 1000.0);
    s.set_viewport_size(200.0, 100.0);
    s.scroll_into_view(0.0, 600.0, 200.0, 20.0);
    assert!(s.offset_y() >= 520.0 && s.offset_y() <= 600.0);
}

#[test]
fn scrollable_momentum_decays() {
    let mut s = Scrollable::new_vertical();
    s.set_content_size(200.0, 10000.0);
    s.set_viewport_size(200.0, 500.0);
    s.fling(0.0, 1500.0);
    let before = s.offset_y();
    for _ in 0..120 {
        s.tick(1.0 / 60.0);
    }
    let after = s.offset_y();
    assert!(after > before);
    assert!(s.momentum_speed() < 10.0);
}
