//! Shared keyboard semantics for interactive chart widgets.

use vello::kurbo::Point;
use winit::event::{ElementState, KeyEvent, TouchPhase, WindowEvent};
use winit::keyboard::{Key, NamedKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartNavAction {
    Previous,
    Next,
    First,
    Last,
    Activate,
    Clear,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChartTouchAction {
    Inspect(Point),
    Activate(Point),
    Clear,
}

/// Tracks one finger and distinguishes a tap from a scroll/drag. Charts may
/// inspect continuously while only activating on a stationary release.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChartTouchTracker {
    active: Option<u64>,
    start: Point,
    moved: bool,
}

impl ChartTouchTracker {
    pub fn handle(&mut self, event: &WindowEvent, scale_factor: f64) -> Option<ChartTouchAction> {
        let WindowEvent::Touch(touch) = event else {
            return None;
        };
        let point = Point::new(
            touch.location.x / scale_factor,
            touch.location.y / scale_factor,
        );
        self.advance(touch.id, touch.phase, point)
    }

    fn advance(&mut self, id: u64, phase: TouchPhase, point: Point) -> Option<ChartTouchAction> {
        match phase {
            TouchPhase::Started if self.active.is_none() => {
                self.active = Some(id);
                self.start = point;
                self.moved = false;
                Some(ChartTouchAction::Inspect(point))
            }
            TouchPhase::Started => {
                // A second finger turns the gesture into navigation, never a
                // latent tap on the first finger.
                self.moved = true;
                None
            }
            TouchPhase::Moved if self.active == Some(id) => {
                self.moved |= self.start.distance(point) > 10.0;
                Some(ChartTouchAction::Inspect(point))
            }
            TouchPhase::Ended if self.active == Some(id) => {
                self.active = None;
                if std::mem::take(&mut self.moved) {
                    Some(ChartTouchAction::Clear)
                } else {
                    Some(ChartTouchAction::Activate(point))
                }
            }
            TouchPhase::Cancelled if self.active == Some(id) => {
                self.active = None;
                self.moved = false;
                Some(ChartTouchAction::Clear)
            }
            _ => None,
        }
    }
}

pub fn chart_nav_action(event: &WindowEvent) -> Option<ChartNavAction> {
    let WindowEvent::KeyboardInput {
        event:
            KeyEvent {
                logical_key,
                state: ElementState::Pressed,
                ..
            },
        ..
    } = event
    else {
        return None;
    };
    match logical_key {
        Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => Some(ChartNavAction::Previous),
        Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) => Some(ChartNavAction::Next),
        Key::Named(NamedKey::Home) => Some(ChartNavAction::First),
        Key::Named(NamedKey::End) => Some(ChartNavAction::Last),
        Key::Named(NamedKey::Enter | NamedKey::Space) => Some(ChartNavAction::Activate),
        Key::Named(NamedKey::Escape) => Some(ChartNavAction::Clear),
        _ => None,
    }
}

pub fn moved_index(current: Option<usize>, len: usize, action: ChartNavAction) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let last = len.saturating_sub(1);
    match action {
        ChartNavAction::Previous => Some(current.unwrap_or(0).checked_sub(1).unwrap_or(last)),
        ChartNavAction::Next => Some(
            current
                .and_then(|index| index.checked_add(1))
                .and_then(|index| index.checked_rem(len))
                .unwrap_or_default(),
        ),
        ChartNavAction::First => Some(0),
        ChartNavAction::Last => Some(last),
        ChartNavAction::Clear => None,
        ChartNavAction::Activate => current,
    }
}

/// Move through a filtered list while preserving the caller's original item
/// indices (for example, skipping invalid or zero-valued data).
pub fn moved_item(
    current: Option<usize>,
    items: &[usize],
    action: ChartNavAction,
) -> Option<usize> {
    let position = current.and_then(|current| items.iter().position(|item| *item == current));
    moved_index(position, items.len(), action).and_then(|position| items.get(position).copied())
}

#[cfg(test)]
mod tests {
    use super::{ChartNavAction, ChartTouchAction, ChartTouchTracker, moved_index};
    use vello::kurbo::Point;
    use winit::event::TouchPhase;

    #[test]
    fn chart_navigation_wraps_and_touch_tracker_classifies_gestures() {
        fn navigation_wraps_and_handles_empty_data() {
            assert_eq!(moved_index(None, 0, ChartNavAction::Next), None);
            assert_eq!(moved_index(None, 3, ChartNavAction::Next), Some(0));
            assert_eq!(moved_index(Some(2), 3, ChartNavAction::Next), Some(0));
            assert_eq!(
                moved_index(Some(usize::MAX), 3, ChartNavAction::Next),
                Some(0)
            );
            assert_eq!(moved_index(Some(0), 3, ChartNavAction::Previous), Some(2));
            assert_eq!(
                super::moved_item(Some(4), &[2, 4, 8], ChartNavAction::Next),
                Some(8)
            );
        }

        fn touch_tracker_activates_taps_but_not_scrolls_or_pinches() {
            let mut tracker = ChartTouchTracker::default();
            let start = Point::new(10.0, 10.0);
            assert_eq!(
                tracker.advance(1, TouchPhase::Started, start),
                Some(ChartTouchAction::Inspect(start))
            );
            assert_eq!(
                tracker.advance(1, TouchPhase::Ended, start),
                Some(ChartTouchAction::Activate(start))
            );

            let _ = tracker.advance(2, TouchPhase::Started, start);
            let moved = Point::new(10.0, 30.0);
            assert_eq!(
                tracker.advance(2, TouchPhase::Moved, moved),
                Some(ChartTouchAction::Inspect(moved))
            );
            assert_eq!(
                tracker.advance(2, TouchPhase::Ended, moved),
                Some(ChartTouchAction::Clear)
            );

            let _ = tracker.advance(3, TouchPhase::Started, start);
            assert_eq!(tracker.advance(4, TouchPhase::Started, start), None);
            assert_eq!(
                tracker.advance(3, TouchPhase::Ended, start),
                Some(ChartTouchAction::Clear)
            );
        }

        navigation_wraps_and_handles_empty_data();
        touch_tracker_activates_taps_but_not_scrolls_or_pinches();
    }
}
