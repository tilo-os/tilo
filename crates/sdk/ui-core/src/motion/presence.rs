//! [`Presence`]: whether something is shown, and how far along its coming
//! or going is.

use super::spec::{Direction, MotionSpec};
use super::value::MotionValue;
use crate::time::Instant;
use crate::tokens::{EffectiveMotion, MotionSource};

/// The openness (`0..=1`) of something that comes and goes, on the spring
/// its named motion picks. The intent ([`Self::is_open`]) changes the moment
/// it is asked for, so input acts on the new state at once; the openness only
/// says how it is drawn.
#[derive(Clone, Copy, Debug)]
pub struct Presence {
    openness: MotionValue<f64>,
    open: bool,
    /// Held at a capture pose ([`Self::held_at`]) until asked to change.
    held: bool,
}

impl Default for Presence {
    fn default() -> Self {
        Self::closed()
    }
}

impl Presence {
    /// Hidden, at rest.
    #[must_use]
    pub fn closed() -> Self {
        Self {
            openness: MotionValue::new(0.0),
            open: false,
            held: false,
        }
    }

    /// Shown, at rest (a surface that starts open).
    #[must_use]
    pub fn opened() -> Self {
        Self {
            openness: MotionValue::new(1.0),
            open: true,
            held: false,
        }
    }

    /// Shown and held at `permille` openness, for capturing a frame of the
    /// motion deterministically: asking to show it again keeps the pose;
    /// hiding it closes from there. Never for normal UI.
    #[doc(hidden)]
    #[must_use]
    pub fn held_at(permille: u16) -> Self {
        Self {
            openness: MotionValue::new(f64::from(permille.min(1000)) / 1000.0),
            open: true,
            held: true,
        }
    }

    /// Show or hide with `spec`'s motion. Asking for the state it is already
    /// in or going to changes nothing; asking for the other one mid-flight
    /// reverses from where it is drawn.
    #[track_caller]
    pub fn show(
        &mut self,
        open: bool,
        spec: MotionSpec,
        motion: EffectiveMotion,
        source: MotionSource,
        now: Instant,
    ) {
        // Only a change of intent moves it: a repeated request, or the same
        // request from another input mid-flight, never restarts or cuts the
        // motion short.
        if open == self.open {
            return;
        }
        self.held = false;
        let timing = spec.timing(Direction::of(open), motion, source);
        self.open = open;
        self.openness.set(if open { 1.0 } else { 0.0 }, timing, now);
    }

    /// Hold the motion for `by` (time spent waiting on a full presenter).
    #[track_caller]
    pub fn defer(&mut self, by: crate::time::Duration) {
        self.openness.defer(by);
    }

    /// Jump to `open`, at rest.
    pub fn snap(&mut self, open: bool) {
        self.held = false;
        self.open = open;
        self.openness.snap(if open { 1.0 } else { 0.0 });
    }

    /// Whether it is held at a capture pose ([`Self::held_at`]): frozen part
    /// way, so an owner that drops a finished motion keeps it.
    pub fn is_held(&self) -> bool {
        self.held
    }

    /// The asked-for state, whatever is drawn.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// How open it is drawn at `now`, `0..=1`.
    #[track_caller]
    pub fn progress(&self, now: Instant) -> f64 {
        self.openness.get(now).clamp(0.0, 1.0)
    }

    /// Whether anything of it is drawn at `now` (open, opening or closing).
    #[track_caller]
    pub fn is_visible(&self, now: Instant) -> bool {
        self.open || self.progress(now) > 0.0
    }

    #[track_caller]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.openness.is_moving(now)
    }

    /// When it arrives; `None` at rest.
    pub fn settles_at(&self) -> Option<Instant> {
        self.openness.settles_at()
    }
}
