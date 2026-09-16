//! Pointer-only Back gesture. Keyboard/accessibility activation stays a click.
use std::time::{Duration, Instant};

#[derive(Default)]
pub(crate) struct BackHold {
    pressed: Option<Instant>,
    consumed: bool,
}
impl BackHold {
    pub fn press(&mut self, now: Instant) {
        self.pressed = Some(now);
        self.consumed = false;
    }
    pub fn tick(&mut self, now: Instant) -> bool {
        if self.waiting()
            && self.pressed.is_some_and(|start| {
                now.saturating_duration_since(start) >= Duration::from_millis(450)
            })
        {
            self.consumed = true;
            return true;
        }
        false
    }
    pub fn cancel(&mut self) {
        self.pressed = None;
        self.consumed = true;
    }
    pub fn release(&mut self) -> bool {
        self.pressed.take().is_some() && !std::mem::take(&mut self.consumed)
    }
    pub fn waiting(&self) -> bool {
        self.pressed.is_some() && !self.consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_click_backs_but_hold_opens_once_and_suppresses_release() {
        let now = Instant::now();
        let mut hold = BackHold::default();
        hold.press(now);
        assert!(!hold.tick(now + Duration::from_millis(100)));
        assert!(hold.release());
        assert!(!hold.waiting());
        hold.press(now);
        assert!(!hold.tick(now + Duration::from_millis(449)));
        assert!(hold.tick(now + Duration::from_millis(450)));
        assert!(!hold.tick(now + Duration::from_millis(600)));
        assert!(!hold.release());
        assert!(!hold.waiting());
    }
    #[test]
    fn pointer_departure_or_focus_loss_cancels_without_late_back() {
        let now = Instant::now();
        let mut hold = BackHold::default();
        hold.press(now);
        hold.cancel();
        assert!(!hold.tick(now + Duration::from_secs(1)));
        assert!(!hold.release());
        assert!(!hold.waiting());
        hold.press(now);
        assert!(hold.release());
    }
}
