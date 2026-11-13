//! Scrolling a view to show something: a find hit, a search result opened in place, a row a click
//! revealed, the item a key moved to off screen. A jump of up to [`ScrollTo::GLIDE_VIEWPORTS`]
//! viewports glides there on a `fast` spring, retargeted by the next jump from where it is drawn and
//! how fast it moves, so pressing Enter in a find field five times is one motion.

use vello::kurbo::Vec2;

use super::named::unit_opacity;
use super::spec::{Direction, MotionSpec, Pace, Reduced, Shortcut};
use super::value::{MotionValue, Timing};
use crate::time::Instant;
use crate::tokens::{EffectiveMotion, MotionSource};

/// What [`ScrollTo::go`] did with a jump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollJump {
    /// The content glides to the target.
    Glide,
    /// The content lands at the target and fades in there.
    FadeIn,
    /// The content is at the target on the next frame.
    Land,
}

/// A scroll view's motion toward what it was asked to show.
#[derive(Clone, Copy, Debug)]
pub struct ScrollTo {
    offset: MotionValue<Vec2>,
    gliding: bool,
    /// The content's opacity after a long jump: 1 at rest.
    fade: MotionValue<f64>,
}

impl Default for ScrollTo {
    fn default() -> Self {
        Self {
            offset: MotionValue::new(Vec2::ZERO).with_tolerance(0.25),
            gliding: false,
            fade: MotionValue::new(1.0),
        }
    }
}

impl ScrollTo {
    /// The glide: `fast` both ways; a key's jump still glides (spatial context, `fast` at most);
    /// reduced motion lands.
    pub const SPEC: MotionSpec = MotionSpec {
        enter: Pace::Fast,
        exit: Pace::Fast,
        reduced: Reduced::Lands,
        shortcut: Shortcut::Context,
    };

    /// The fade-in after a long jump: `fast`, also under reduced motion and for a key's jump.
    pub const FADE: MotionSpec = MotionSpec {
        enter: Pace::Fast,
        exit: Pace::Fast,
        reduced: Reduced::Fades,
        shortcut: Shortcut::Context,
    };

    /// The longest jump, in viewports along either axis, that glides; a longer one fades in at the
    /// target.
    pub const GLIDE_VIEWPORTS: f64 = 3.0;

    const FADE_FROM: f64 = 0.0;

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Move the content from `drawn`, the offset it is drawn at now, to `target`, in a view `viewport`
    /// large.
    #[track_caller]
    pub fn go(
        &mut self,
        drawn: Vec2,
        target: Vec2,
        viewport: Vec2,
        motion: EffectiveMotion,
        now: Instant,
    ) -> ScrollJump {
        match super::recent_input(now) {
            None | Some(MotionSource::Action) => {
                self.land(target);
                ScrollJump::Land
            }
            Some(source) => self.go_from(drawn, target, viewport, motion, source, now),
        }
    }

    /// [`Self::go`] for a jump `source` caused.
    #[track_caller]
    pub fn go_from(
        &mut self,
        drawn: Vec2,
        target: Vec2,
        viewport: Vec2,
        motion: EffectiveMotion,
        source: MotionSource,
        now: Instant,
    ) -> ScrollJump {
        let from = if self.is_gliding(now) {
            self.offset.get(now)
        } else {
            drawn
        };
        let jump = target - from;
        if jump.hypot() < 0.5 {
            if !self.is_gliding(now) {
                self.land(target);
            }
            return ScrollJump::Land;
        }
        let far =
            |extent: f64, along: f64| extent > 0.0 && along.abs() > extent * Self::GLIDE_VIEWPORTS;
        if far(viewport.x, jump.x) || far(viewport.y, jump.y) {
            self.land(target);
            let timing = Self::FADE.timing(Direction::Enter, motion, source);
            if timing.is_instant() {
                return ScrollJump::Land;
            }
            self.fade.snap(Self::FADE_FROM);
            self.fade.set(1.0, timing, now);
            return ScrollJump::FadeIn;
        }
        let timing = Self::SPEC.timing(Direction::Enter, motion, source);
        if timing.is_instant() {
            self.land(target);
            return ScrollJump::Land;
        }
        if !self.is_gliding(now) {
            self.offset.snap(drawn);
        }
        self.offset.set(target, timing, now);
        self.gliding = true;
        ScrollJump::Glide
    }

    /// Settle on `target`, the way a scroll view whose content offers detents comes to rest on one
    /// ([`super::Snap`]): a gesture's release hands its `velocity` to the settle, so a fling coasts on
    /// into the detent without turning back; anything else takes the paged-strip settle (`base`).
    #[track_caller]
    pub fn settle(
        &mut self,
        drawn: Vec2,
        target: Vec2,
        velocity: Vec2,
        motion: EffectiveMotion,
        source: MotionSource,
        now: Instant,
    ) -> ScrollJump {
        let thrown = matches!(source, MotionSource::Gesture) && velocity != Vec2::ZERO;
        let spec = if thrown {
            super::Snap::COAST
        } else {
            super::Snap::SPEC
        };
        let timing = spec.timing(Direction::Enter, motion, source);
        let Timing::Spring(_) = timing else {
            self.land(target);
            return ScrollJump::Land;
        };
        self.fade.snap(1.0);
        self.offset.snap(drawn);
        self.offset.release(target, velocity, timing, now);
        self.gliding = true;
        ScrollJump::Glide
    }

    /// Show `target` on the next frame: keyboard focus moving to a control off screen while tabbing.
    pub fn land(&mut self, target: Vec2) {
        self.offset.snap(target);
        self.gliding = false;
    }

    /// The hand takes over (a wheel, a finger, a bar drag): the motion stops where the content is
    /// drawn. Returns that offset when a glide was under way.
    #[track_caller]
    pub fn take_over(&mut self, now: Instant) -> Option<Vec2> {
        let drawn = self.offset(now);
        self.gliding = false;
        if let Some(at) = drawn {
            self.offset.snap(at);
        }
        self.fade.snap(1.0);
        drawn
    }

    /// Where the content is drawn at `now` while it glides; `None` when the scroller's own offset is
    /// where it is.
    #[track_caller]
    #[must_use]
    pub fn offset(&self, now: Instant) -> Option<Vec2> {
        self.gliding.then(|| self.offset.get(now))
    }

    /// [`Self::offset`] in whole pixels of a display with `device_per_unit` device pixels per logical
    /// pixel; the target itself where it rests.
    #[track_caller]
    #[must_use]
    pub fn offset_in_device_pixels(&self, now: Instant, device_per_unit: f64) -> Option<Vec2> {
        let at = self.offset(now)?;
        if !self.offset.is_moving(now) {
            return Some(self.offset.target());
        }
        let scale = if device_per_unit.is_finite() && device_per_unit > 0.0 {
            device_per_unit
        } else {
            1.0
        };
        Some(Vec2::new(
            (at.x * scale).round() / scale,
            (at.y * scale).round() / scale,
        ))
    }

    /// Where the motion is going, while it glides.
    #[must_use]
    pub fn target(&self) -> Option<Vec2> {
        self.gliding.then(|| self.offset.target())
    }

    /// The content's opacity at `now`: below 1 while it fades in after a long jump.
    #[track_caller]
    #[must_use]
    pub fn opacity(&self, now: Instant) -> f32 {
        unit_opacity(self.fade.get(now))
    }

    /// Whether the content still glides at `now`; once it arrives, the scroller's offset is the target
    /// and the motion asks for nothing.
    #[track_caller]
    #[must_use]
    pub fn is_gliding(&self, now: Instant) -> bool {
        self.gliding && self.offset.is_moving(now)
    }

    /// Whether anything still moves at `now`: the glide or the fade-in.
    #[track_caller]
    #[must_use]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.is_gliding(now) || self.fade.is_moving(now)
    }

    /// When everything has arrived; `None` at rest.
    #[must_use]
    pub fn settles_at(&self) -> Option<Instant> {
        let glide = self.gliding.then(|| self.offset.settles_at()).flatten();
        glide.into_iter().chain(self.fade.settles_at()).max()
    }

    /// Once the glide has arrived, forget it: the scroller's own offset is the target from then on.
    /// Returns the target it arrived at.
    #[track_caller]
    pub fn arrived(&mut self, now: Instant) -> Option<Vec2> {
        if self.gliding && !self.offset.is_moving(now) {
            self.gliding = false;
            return Some(self.offset.target());
        }
        None
    }
}
