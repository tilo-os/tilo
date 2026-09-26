#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::Duration;
use tilo_ui::widget::tooltip::Tooltip;

#[test]
fn tooltip_reveals_after_default_or_custom_delay() {
    // Case `tooltip_reveals_after_delay`.
    {
        let mut t = Tooltip::new("Open workspace settings");
        assert!(!t.is_visible());
        t.on_hover_enter(Duration::from_millis(0));
        t.tick(Duration::from_millis(200));
        assert!(!t.is_visible());
        t.tick(Duration::from_millis(500)); // cumulative 700ms > 600ms default
        assert!(t.is_visible());
        t.on_hover_leave();
        assert!(!t.is_visible());
    }

    // Case `tooltip_custom_delay`.
    {
        let mut t = Tooltip::new("x").with_delay(Duration::from_millis(100));
        t.on_hover_enter(Duration::from_millis(0));
        t.tick(Duration::from_millis(150));
        assert!(t.is_visible());
    }
}
