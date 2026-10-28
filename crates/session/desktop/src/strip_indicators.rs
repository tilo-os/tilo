//! When the strip ribbon and the strip rail show.
//!
//! They answer "where am I among the strips, and along this one?", which
//! only matters while you are moving. Shown all the time they are clutter
//! along two screen edges, so they appear when the position they report
//! changes (a scroll, a pan, a keyboard step, a switch to another strip or
//! workspace), stay while it keeps changing or while the pointer is on
//! them, and go a moment after it stops, as a confirmation goes
//! ([`ui::motion::Transient::CONFIRMATION`]): there at once, then faded out.

use std::cell::Cell;

use tilo_desktop_model::StripId;
use ui::time::{Duration, Instant};

/// How long the indicators stay fully up after the position last changed.
pub const STRIP_INDICATOR_HOLD: Duration = Duration::from_millis(1_200);

/// How they come and go.
const MOTION: ui::motion::MotionSpec = ui::motion::Transient::CONFIRMATION;

/// The motion the person allows, reduced or not.
fn effective(reduce_motion: bool) -> ui::tokens::EffectiveMotion {
    ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), reduce_motion)
}

/// Their fade-out after the hold.
fn fade(reduce_motion: bool) -> Duration {
    ui::motion::Transient::exit_time(MOTION, effective(reduce_motion))
}

/// The hold and the fade together: how long after the last change they are
/// gone.
fn shown_for(reduce_motion: bool) -> Duration {
    STRIP_INDICATOR_HOLD.saturating_add(fade(reduce_motion))
}

/// What the indicators report. Any change to it is navigation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StripPosition {
    pub strip: Option<StripId>,
    pub ribbon: (usize, usize),
    pub rail: (usize, usize),
    pub rail_visible: Vec<usize>,
    /// The active strip's scroll offset in half-pixel steps.
    pub scroll_half_px: i64,
    pub hovered: bool,
}

#[derive(Debug, Default)]
pub struct StripIndicators {
    last: Option<StripPosition>,
    /// When the position last changed; `None` once the fade has finished.
    changed_at: Option<Instant>,
    hovered: bool,
    /// A direct pan came to rest somewhere new. The frame it rests on is
    /// composited from the layers the pan began with, so the indicators
    /// show from the next frame on.
    show_on_next_frame: bool,
    /// Reduced motion, as of the last paint.
    reduce_motion: Cell<bool>,
}

impl StripIndicators {
    /// Record the position about to be painted at `now`. The first
    /// observation is the session's starting point, not navigation.
    pub fn observe(&mut self, position: StripPosition, now: Instant) {
        self.hovered = position.hovered;
        if std::mem::take(&mut self.show_on_next_frame)
            || self.last.as_ref().is_some_and(|last| *last != position)
        {
            self.changed_at = Some(now);
        }
        self.last = Some(position);
        if self.changed_at.is_some_and(|at| {
            now.saturating_duration_since(at) >= shown_for(self.reduce_motion.get())
        }) {
            self.changed_at = None;
        }
    }

    /// A direct pan came to rest at `position`, on a frame that must not
    /// change the chrome: take it as the resting point now, and if the pan
    /// moved anything, show them from the next frame on, as any other
    /// navigation does. Without this a touchpad pan, the commonest way along
    /// a strip, never showed the window rail at all.
    pub fn settle_after_pan(&mut self, position: StripPosition) {
        self.hovered = position.hovered;
        if self.last.as_ref().is_some_and(|last| *last != position) {
            self.show_on_next_frame = true;
        }
        self.last = Some(position);
    }

    /// Opacity at `now`. Under reduced motion they are whole until they go.
    pub fn alpha_at(&self, now: Instant, reduce_motion: bool) -> f32 {
        self.reduce_motion.set(reduce_motion);
        if self.hovered {
            return 1.0;
        }
        let Some(window) = self.shown_window(reduce_motion) else {
            return 0.0;
        };
        ui::motion::Transient::opacity(MOTION, window, now, effective(reduce_motion))
    }

    /// When they were shown and when they are gone, while they are up.
    fn shown_window(&self, reduce_motion: bool) -> Option<(Instant, Instant)> {
        let at = self.changed_at?;
        Some((at, at.checked_add(shown_for(reduce_motion))?))
    }

    /// Frames are needed only while fading; the opaque hold is static.
    pub fn needs_animation_frame_at(&self, now: Instant) -> bool {
        let reduce_motion = self.reduce_motion.get();
        !self.hovered
            && self.shown_window(reduce_motion).is_some_and(|window| {
                now >= window
                    .0
                    .checked_add(STRIP_INDICATOR_HOLD)
                    .unwrap_or(window.0)
                    && ui::motion::Transient::is_moving(
                        MOTION,
                        window,
                        now,
                        effective(reduce_motion),
                    )
            })
    }

    /// Wakes the loop when the fade begins; from there the fade's own
    /// animation frames carry it to the end. Nothing is owed past the fade
    /// start: an overdue deadline that only a paint could clear would spin
    /// the loop whenever nothing else asks for a paint.
    pub fn next_wake_deadline_at(&self, now: Instant) -> Option<Instant> {
        // Due now: the next frame's observation shows them and clears it.
        if self.show_on_next_frame {
            return Some(now);
        }
        let fade_start = self.changed_at?.checked_add(STRIP_INDICATOR_HOLD)?;
        (now < fade_start).then_some(fade_start)
    }
}

/// Where the long segment of the strip ribbon or rail stands while focus
/// moves along it: it slides from the segment it left to the one that took
/// focus (`ui::motion::ListChange::MOVE`), and a shortcut's move lands at
/// once. A run that gained or lost a segment starts at rest.
#[derive(Default)]
pub struct SlidingFocus {
    value: Option<ui::motion::MotionValue<f64>>,
    count: usize,
}

impl SlidingFocus {
    /// Follow a run of `count` segments whose focus is `focused`; returns
    /// where its long segment is drawn at `now`.
    pub fn follow(
        &mut self,
        (count, focused): (usize, usize),
        motion: ui::tokens::EffectiveMotion,
        now: Instant,
    ) -> f64 {
        use ui::motion::{Direction, ListChange, MotionValue};
        let target = ui::num::f64_of_usize(focused);
        match self.value.as_mut() {
            Some(value) if self.count == count => {
                // Targets are whole segment indices: a different one is at
                // least a whole segment away.
                if (value.target() - target).abs() >= 0.5 {
                    let timing = ListChange::MOVE.timing(
                        Direction::Enter,
                        motion,
                        ui::motion::input_source(),
                    );
                    value.set(target, timing, now);
                }
            }
            _ => {
                self.value = Some(MotionValue::new(target));
                self.count = count;
            }
        }
        self.value.as_ref().map_or(target, |value| value.get(now))
    }

    pub fn is_moving(&self, now: Instant) -> bool {
        self.value
            .as_ref()
            .is_some_and(|value| value.is_moving(now))
    }

    /// Where the long segment is drawn at `now`, while it slides.
    pub fn sliding_at(&self, now: Instant) -> Option<f64> {
        self.value
            .as_ref()
            .filter(|value| value.is_moving(now))
            .map(|value| value.get(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(focused: usize, scroll: i64) -> StripPosition {
        StripPosition {
            strip: Some(StripId(1)),
            ribbon: (2, 0),
            rail: (4, focused),
            rail_visible: vec![focused],
            scroll_half_px: scroll,
            hovered: false,
        }
    }

    #[test]
    fn hidden_at_rest_shown_on_navigation_then_faded_and_settled() {
        let t0 = Instant::now();
        let mut s = StripIndicators::default();
        s.observe(position(0, 0), t0);
        assert_eq!(
            s.alpha_at(t0, false),
            0.0,
            "the starting position is not navigation"
        );
        assert_eq!(s.next_wake_deadline_at(t0), None);

        let t1 = t0 + Duration::from_millis(50);
        s.observe(position(0, 40), t1);
        assert_eq!(s.alpha_at(t1, false), 1.0, "a scroll shows them at once");
        assert!(!s.needs_animation_frame_at(t1), "the hold is static");
        assert_eq!(s.next_wake_deadline_at(t1), Some(t1 + STRIP_INDICATOR_HOLD));

        let mid_fade = t1 + STRIP_INDICATOR_HOLD + fade(false) / 2;
        let a = s.alpha_at(mid_fade, false);
        assert!(a > 0.0 && a < 1.0, "fading, got {a}");
        assert!(s.needs_animation_frame_at(mid_fade));
        assert_eq!(
            s.alpha_at(mid_fade, true),
            0.0,
            "under reduced motion they go at the end of the hold"
        );
        s.alpha_at(mid_fade, false);

        let end = t1 + STRIP_INDICATOR_HOLD + fade(false);
        assert_eq!(s.alpha_at(end, false), 0.0);
        assert_eq!(
            s.next_wake_deadline_at(mid_fade),
            None,
            "the fade's own frames carry it"
        );
        assert_eq!(
            s.next_wake_deadline_at(end),
            None,
            "nothing is owed after it"
        );
        assert!(!s.needs_animation_frame_at(end));
    }

    #[test]
    fn a_direct_pan_shows_them_from_the_frame_after_it_rests() {
        let t0 = Instant::now();
        let mut s = StripIndicators::default();
        s.observe(position(0, 0), t0);
        // The pan's frames are not observed; it rests somewhere new.
        let rest = t0 + Duration::from_millis(400);
        s.settle_after_pan(position(1, 900));
        assert_eq!(
            s.alpha_at(rest, false),
            0.0,
            "the frame the pan rests on keeps the chrome it began with"
        );
        assert_eq!(
            s.next_wake_deadline_at(rest),
            Some(rest),
            "the next frame is owed at once"
        );
        let next = rest + Duration::from_millis(8);
        s.observe(position(1, 900), next);
        assert_eq!(s.alpha_at(next, false), 1.0, "the next frame shows them");
        assert_eq!(
            s.next_wake_deadline_at(next),
            Some(next + STRIP_INDICATOR_HOLD),
            "and the owed frame is paid: only the fade start remains"
        );

        let later = next + STRIP_INDICATOR_HOLD + fade(false);
        s.observe(position(1, 900), later);
        s.settle_after_pan(position(1, 900));
        assert_eq!(s.next_wake_deadline_at(later), None);
        s.observe(position(1, 900), later + Duration::from_millis(8));
        assert_eq!(s.alpha_at(later + Duration::from_millis(8), false), 0.0);
    }

    #[test]
    fn continued_navigation_and_hover_keep_them_up() {
        let t0 = Instant::now();
        let mut s = StripIndicators::default();
        s.observe(position(0, 0), t0);
        let mut t = t0;
        for step in 1..=10 {
            t = t0 + STRIP_INDICATOR_HOLD / 2 * step;
            s.observe(position(usize::try_from(step % 4).unwrap(), 0), t);
            assert_eq!(s.alpha_at(t, false), 1.0);
        }
        let mut hovered = position(2, 0);
        hovered.hovered = true;
        s.observe(hovered, t);
        let later = t + STRIP_INDICATOR_HOLD * 10;
        assert_eq!(
            s.alpha_at(later, false),
            1.0,
            "the pointer on them keeps them"
        );
        s.observe(position(2, 0), later);
        assert_eq!(
            s.alpha_at(later, false),
            1.0,
            "leaving them restarts the hold"
        );
        assert_eq!(
            s.alpha_at(later + STRIP_INDICATOR_HOLD + fade(false), false),
            0.0
        );
    }
}
