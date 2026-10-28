//! A frame held for the local reads it would show ([`crate::arriving`]).

/// Until when a frame waits for the local reads it would show: the latest
/// instant a widget holds it to
/// ([`crate::widget::Widget::holds_frame_until`]), while that
/// is still ahead of `now`.
pub(super) fn frame_held_until(
    hold: Option<crate::time::Instant>,
    now: crate::time::Instant,
) -> Option<crate::time::Instant> {
    hold.filter(|until| *until > now)
}

#[cfg(test)]
#[path = "held_frame_tests.rs"]
mod tests;
