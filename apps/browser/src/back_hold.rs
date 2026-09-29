//! Pointer-only navigation-button hold gesture.
use ui::time::{Duration, Instant};

use ui::webview::NavigationDirection;

#[derive(Default)]
pub(crate) struct NavigationHold {
    pressed: Option<(Instant, NavigationDirection)>,
    consumed: bool,
}
impl NavigationHold {
    pub fn press(&mut self, now: Instant, direction: NavigationDirection) {
        self.pressed = Some((now, direction));
        self.consumed = false;
    }
    pub fn tick(&mut self, now: Instant) -> Option<NavigationDirection> {
        if self.waiting()
            && self.pressed.is_some_and(|(start, _)| {
                now.saturating_duration_since(start) >= Duration::from_millis(450)
            })
        {
            self.consumed = true;
            return self.pressed.map(|(_, direction)| direction);
        }
        None
    }
    pub fn cancel(&mut self) {
        self.pressed = None;
        self.consumed = true;
    }
    pub fn release(&mut self) -> Option<NavigationDirection> {
        let pressed = self.pressed.take();
        if std::mem::take(&mut self.consumed) {
            None
        } else {
            pressed.map(|(_, direction)| direction)
        }
    }
    pub fn waiting(&self) -> bool {
        self.pressed.is_some() && !self.consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Short click, hold and cancellation of the back button gesture.
    #[test]
    fn back_hold_distinguishes_click_hold_and_cancellation() {
        // Phase `short_click_backs_but_hold_opens_once_and_suppresses_release`.
        {
            let now = ui::time::now();
            let mut hold = NavigationHold::default();
            hold.press(now, NavigationDirection::Back);
            assert_eq!(hold.tick(now + Duration::from_millis(100)), None);
            assert_eq!(hold.release(), Some(NavigationDirection::Back));
            assert!(!hold.waiting());
            hold.press(now, NavigationDirection::Forward);
            assert_eq!(hold.tick(now + Duration::from_millis(449)), None);
            assert_eq!(
                hold.tick(now + Duration::from_millis(450)),
                Some(NavigationDirection::Forward)
            );
            assert_eq!(hold.tick(now + Duration::from_millis(600)), None);
            assert_eq!(hold.release(), None);
            assert!(!hold.waiting());
        }

        // Phase `pointer_departure_or_focus_loss_cancels_without_late_back`.
        {
            let now = ui::time::now();
            let mut hold = NavigationHold::default();
            hold.press(now, NavigationDirection::Back);
            hold.cancel();
            assert_eq!(hold.tick(now + Duration::from_secs(1)), None);
            assert_eq!(hold.release(), None);
            assert!(!hold.waiting());
            hold.press(now, NavigationDirection::Forward);
            assert_eq!(hold.release(), Some(NavigationDirection::Forward));
        }
    }
}
