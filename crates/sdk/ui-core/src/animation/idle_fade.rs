//! Chrome that shows on activity and fades out on its own once things are
//! quiet: an overlay scroll bar after scrolling, a block's drag handle after
//! the pointer moves. It is fully visible for a hold after the last activity,
//! then fades; something that holds it (the pointer on it, a drag) keeps it
//! visible. Its owner sleeps through the hold on
//! [`IdleFade::fade_deadline_at`] and asks for frames only while
//! [`IdleFade::fading_at`], so resting chrome costs no frames.

use crate::time::{Duration, Instant};

/// How long fading chrome takes to go, once its hold is over: every piece of
/// chrome that fades does so alike.
pub const CHROME_FADE: Duration = Duration::from_millis(320);

/// The idle fade of a piece of chrome: when it was last woken, how long it
/// then stays fully visible, and how long it takes to fade out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdleFade {
    last_activity: Option<Instant>,
    hold: Duration,
    fade: Duration,
}

impl IdleFade {
    /// Chrome that stays for `hold` after the last activity, then fades
    /// over `fade`. It starts hidden.
    #[must_use]
    pub const fn new(hold: Duration, fade: Duration) -> Self {
        Self {
            last_activity: None,
            hold,
            fade,
        }
    }

    /// The same fade, last woken at `last_activity`.
    #[must_use]
    pub(crate) const fn woken_at(mut self, last_activity: Option<Instant>) -> Self {
        self.last_activity = last_activity;
        self
    }

    /// Activity that shows the chrome happened at `now`.
    pub fn wake(&mut self, now: Instant) {
        self.last_activity = Some(now);
    }

    /// Hide the chrome at once (the person started something it would be in
    /// the way of).
    pub fn hide(&mut self) {
        self.last_activity = None;
    }

    /// Whether nothing has woken the chrome since it was made or hidden.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.last_activity.is_none()
    }

    /// The chrome's opacity at `now`; `held` keeps it fully visible.
    #[must_use]
    pub fn opacity_at(&self, now: Instant, held: bool) -> f32 {
        if held {
            return 1.0;
        }
        let Some(activity) = self.last_activity else {
            return 0.0;
        };
        let elapsed = now.saturating_duration_since(activity);
        // `checked_sub` doubles as the "still within the hold" test.
        let Some(since_hold) = elapsed.checked_sub(self.hold) else {
            return 1.0;
        };
        if since_hold >= self.fade {
            return 0.0;
        }
        1.0 - since_hold.as_secs_f32() / self.fade.as_secs_f32()
    }

    /// Whether the chrome is fading at `now`, so its owner needs frames.
    #[must_use]
    pub fn fading_at(&self, now: Instant, held: bool) -> bool {
        !held
            && self.last_activity.is_some_and(|activity| {
                let elapsed = now.saturating_duration_since(activity);
                elapsed >= self.hold && elapsed < self.hold.saturating_add(self.fade)
            })
    }

    /// When the fade starts, if it is still ahead at `now`: the owner's next
    /// wake deadline.
    #[must_use]
    pub fn fade_deadline_at(&self, now: Instant, held: bool) -> Option<Instant> {
        if held {
            return None;
        }
        self.last_activity?
            .checked_add(self.hold)
            .filter(|deadline| *deadline > now)
    }

    /// When the fade will have run its course, if the chrome was woken.
    #[must_use]
    pub fn fade_end(&self) -> Option<Instant> {
        self.last_activity?
            .checked_add(self.hold)?
            .checked_add(self.fade)
    }

    /// Whether the fade has run its course at `now`.
    #[must_use]
    pub fn finished_at(&self, now: Instant) -> bool {
        self.last_activity.is_some_and(|activity| {
            now.saturating_duration_since(activity) >= self.hold.saturating_add(self.fade)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_holds_then_fades_and_sleeps_once_gone() {
        let hold = Duration::from_secs(3);
        let mut fade = IdleFade::new(hold, CHROME_FADE);
        let t0 = crate::time::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        assert!(fade.is_idle());
        assert_eq!(fade.opacity_at(t0, false), 0.0, "it starts hidden");
        assert_eq!(fade.opacity_at(t0, true), 1.0, "held, it shows");
        fade.wake(t0);
        assert_eq!(fade.opacity_at(at(2_999), false), 1.0, "the hold");
        assert!(
            !fade.fading_at(at(2_999), false),
            "the hold needs no frames"
        );
        assert_eq!(fade.fade_deadline_at(at(10), false), Some(at(3_000)));
        let midway = fade.opacity_at(at(3_160), false);
        assert!(midway > 0.0 && midway < 1.0, "fading: {midway}");
        assert!(fade.fading_at(at(3_160), false));
        assert_eq!(fade.fade_deadline_at(at(3_160), false), None);
        assert_eq!(fade.opacity_at(at(3_320), false), 0.0);
        assert!(
            !fade.fading_at(at(3_320), false),
            "gone, it needs no frames"
        );
        assert!(fade.finished_at(at(3_320)));
        fade.wake(at(4_000));
        fade.hide();
        assert_eq!(fade.opacity_at(at(4_000), false), 0.0, "hidden at once");
        assert_eq!(fade.fade_deadline_at(at(4_000), false), None);
    }
}
