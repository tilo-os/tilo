//! [`Placed`] — a surface that appears where it stands, leaves from there, and moves when its
//! place changes — a window on a desk, a place on a canvas, a panel over the screen.
//!
//! A shell draws its windows, its canvas places and its floating surfaces from a view, and a
//! view is built again whenever the arrangement changes (`shell.md` §7: motion stays in the Rust
//! components, never in the view). So the motion is the component's own: it is told where it
//! stands and whether it is up, keeps where and how open it was drawn, and moves from there.
//!
//! It composes two of the library's named motions and adds nothing of its own:
//!
//! - **Coming and going** is [`WindowMotion::OPEN`] on [`Presence`]: the surface fades and scales
//!   from 0.96 about its centre as it arrives, and fades out as it leaves.
//! - **Moving** is [`WindowMotion::MOVE`] on a [`MotionValue<Rect>`]: the surface springs from the
//!   rectangle it was last drawn at to the one it is laid out at, so a window carried to another
//!   strip or a place dragged on the canvas glides there instead of jumping.
//!
//! Both land at once for a still theme and for reduced motion (the specs say so), so a pixelated
//! theme and a person who asked for less motion see the change on the frame it happens. Every
//! motion asks the frame pump for frames only while it moves ([`MotionValue`] does), so a surface
//! at rest costs nothing.

use vello::kurbo::{Affine, Point, Rect};

use super::named::WindowMotion;
use super::pose::Pose;
use super::presence::Presence;
use super::value::MotionValue;
use crate::time::Instant;
use crate::tokens::{EffectiveMotion, MotionSource};

/// One surface's arrival, departure and movement, sampled once per drawn frame.
#[derive(Clone, Debug)]
pub struct Placed {
    /// Whether it is drawn at all, and how far into its arrival or departure.
    presence: Presence,
    /// Where it is drawn (canvas or layout coordinates): the spring that carries it from the
    /// place it last stood to the one it is laid out at.
    rect: MotionValue<Rect>,
    /// The rectangle it is laid out at, as of the last frame.
    target: Rect,
    /// It has been placed once: the first placement rests (first paint shows the resting
    /// state; motion explains changes).
    settled: bool,
    /// The motion it last drew with, so a departure after the theme changed keeps the person's
    /// reduced motion.
    motion: EffectiveMotion,
}

impl Default for Placed {
    fn default() -> Self {
        Self {
            presence: Presence::opened(),
            rect: MotionValue::new(Rect::ZERO),
            target: Rect::ZERO,
            settled: false,
            motion: EffectiveMotion::default(),
        }
    }
}

impl Placed {
    /// A surface that is up, at rest, from its first paint.
    #[must_use]
    pub fn opened() -> Self {
        Self {
            presence: Presence::opened(),
            ..Self::default()
        }
    }

    /// A surface that is not up, at rest: it arrives when it is shown.
    #[must_use]
    pub fn closed() -> Self {
        Self {
            presence: Presence::closed(),
            ..Self::default()
        }
    }

    /// Where it is drawn this frame, its motion advanced to `now`: laid out at `rect`, up
    /// (`open`) or going, with `motion` the person allows and `source` what asked for the change.
    ///
    /// Call it once a frame, where the widget draws or lays out, so every read in that frame sees
    /// the same value.
    pub fn follow(
        &mut self,
        rect: Rect,
        open: bool,
        motion: EffectiveMotion,
        source: MotionSource,
        now: Instant,
    ) {
        self.motion = motion;
        // The first placement rests where it stands. A surface built up shows at rest (the first
        // paint shows the resting state); one built closed and asked to be up on its first frame
        // comes in, as a window the desk just opened does.
        if !self.settled {
            self.settled = true;
            self.rect.snap(rect);
            self.target = rect;
            if open && !self.presence.is_open() {
                self.presence.snap(false);
                self.presence
                    .show(true, WindowMotion::OPEN, motion, source, now);
            } else {
                self.presence.snap(open);
            }
            return;
        }
        self.presence
            .show(open, WindowMotion::OPEN, motion, source, now);
        if (rect.x0 - self.target.x0).abs() > 0.5
            || (rect.y0 - self.target.y0).abs() > 0.5
            || (rect.x1 - self.target.x1).abs() > 0.5
            || (rect.y1 - self.target.y1).abs() > 0.5
        {
            self.target = rect;
            self.rect.set(
                rect,
                WindowMotion::MOVE.timing(super::Direction::Enter, motion, source),
                now,
            );
        }
    }

    /// Where it is drawn at `now`, whatever the last frame laid out: the rectangle the move has
    /// reached, or the one it stands at once it has arrived.
    pub fn rect(&self, now: Instant) -> Rect {
        self.rect.get(now)
    }

    /// How far into coming or going it is at `now`, `0..=1`.
    pub fn progress(&self, now: Instant) -> f64 {
        self.presence.progress(now)
    }

    /// Whether anything of it is drawn at `now`: up, coming or leaving.
    pub fn is_visible(&self, now: Instant) -> bool {
        self.presence.is_visible(now)
    }

    /// Whether it is still moving at `now` (its owner asks for frames).
    pub fn is_moving(&self, now: Instant) -> bool {
        self.presence.is_moving(now) || self.rect.is_moving(now)
    }

    /// When it arrives or rests; `None` while it is still.
    pub fn settles_at(&self) -> Option<Instant> {
        self.presence.settles_at().max(self.rect.settles_at())
    }

    /// The pose it is drawn under at `now`, in a surface laid out at `rest`: its content's
    /// arrival, and the transform carrying it from where it stood toward where it is laid out.
    ///
    /// A surface that is coming or going scales and fades about its own centre
    /// ([`WindowMotion::content_pose`]); one that only moves is drawn at its place plus the
    /// offset the move has still to cover.
    pub fn pose(&self, rest: Rect, now: Instant) -> Pose {
        let content = WindowMotion::content_pose(self.presence.progress(now), rest, self.motion);
        let drawn = self.rect.get(now);
        let offset = drawn.origin() - rest.origin();
        if offset.x.abs() < 0.05 && offset.y.abs() < 0.05 {
            return content;
        }
        Pose {
            opacity: content.opacity,
            transform: Affine::translate(offset) * content.transform,
        }
    }

    /// The ground it stands on moved (a canvas panned or zoomed under it): it stands at `rect`
    /// at once, its coming or going untouched, as the windows in it do. `false` for one never
    /// placed, which [`Self::follow`] places.
    pub fn carry(&mut self, rect: Rect) -> bool {
        if self.settled {
            self.target = rect;
            self.rect.snap(rect);
        }
        self.settled
    }

    /// Jump to `rect` and `open`, at rest: a capture or a test posing a surface where it stands.
    pub fn snap(&mut self, rect: Rect, open: bool) {
        self.settled = true;
        self.target = rect;
        self.rect.snap(rect);
        self.presence.snap(open);
    }

    /// The centre of where it is drawn at `now`, for a caller that anchors something to it.
    #[must_use]
    pub fn centre(&self, now: Instant) -> Point {
        self.rect.get(now).center()
    }

    /// The move's value, for a capture or a test that poses it at a chosen point of its run
    /// ([`MotionValue::sampled_after`]). Never for a live frame, which reads [`Self::rect`] at
    /// the frame's instant.
    #[must_use]
    pub fn movement(&self) -> &MotionValue<Rect> {
        &self.rect
    }

    /// The coming or going, for a capture or a test posing it (`Presence::progress` at a chosen
    /// instant). Never for a live frame, which reads [`Self::progress`].
    #[must_use]
    pub fn openness(&self) -> &Presence {
        &self.presence
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::testing;
    use crate::time::Duration;
    use crate::tokens::Motion;

    fn still() -> EffectiveMotion {
        EffectiveMotion::new(
            Motion {
                still: true,
                ..Motion::default()
            },
            false,
        )
    }

    fn reduced() -> EffectiveMotion {
        EffectiveMotion::new(Motion::default(), true)
    }

    fn rect(x: f64) -> Rect {
        Rect::new(x, 0.0, x + 200.0, 120.0)
    }

    #[test]
    fn a_surface_first_placed_rests_where_it_stands() {
        let _guard = testing::frozen();
        let mut placed = Placed::opened();
        let now = crate::motion::frame_now();
        placed.follow(
            rect(40.0),
            true,
            EffectiveMotion::default(),
            MotionSource::Pointer,
            now,
        );
        assert_eq!(placed.rect(now), rect(40.0));
        assert!(placed.pose(rect(40.0), now).is_rest());
        assert!(!placed.is_moving(now), "a first paint asks for nothing");
    }

    #[test]
    fn a_moved_surface_glides_to_its_new_place_over_several_frames_then_rests() {
        let _frame = crate::motion::pin_frame();
        let mut placed = Placed::opened();
        let now = crate::motion::frame_now();
        placed.follow(
            rect(40.0),
            true,
            EffectiveMotion::default(),
            MotionSource::Pointer,
            now,
        );
        // It is laid out at 400 but drawn from where it stood, and moves toward 400.
        placed.follow(
            rect(400.0),
            true,
            EffectiveMotion::default(),
            MotionSource::Pointer,
            now,
        );
        let start = placed.movement().sampled_after(Duration::ZERO).0;
        assert!(
            (start.x0 - 40.0).abs() < 1.0,
            "it starts where it stood: {start:?}"
        );
        let mid = placed.movement().sampled_after(Duration::from_millis(80)).0;
        assert!(
            mid.x0 > start.x0 && mid.x0 < 400.0,
            "it is on its way: {mid:?}"
        );
        let arrived = placed.movement().sampled_after(Duration::from_secs(3)).0;
        assert!((arrived.x0 - 400.0).abs() < 0.5, "it arrives: {arrived:?}");
        // At rest the value is its target and asks for nothing.
        assert!(!placed.movement().is_moving(now + Duration::from_secs(3)));
        assert!(!placed.is_moving(now + Duration::from_secs(3)));
    }

    #[test]
    fn a_surface_shown_and_hidden_comes_and_goes_and_a_still_theme_lands_at_once() {
        let _frame = crate::motion::pin_frame();
        let mut placed = Placed::closed();
        let now = crate::motion::frame_now();
        placed.follow(
            rect(0.0),
            false,
            EffectiveMotion::default(),
            MotionSource::Pointer,
            now,
        );
        assert!(!placed.is_visible(now));
        placed.follow(
            rect(0.0),
            true,
            EffectiveMotion::default(),
            MotionSource::Pointer,
            now,
        );
        // Read a little into the arrival at that frame's own instant.
        let mid = now + Duration::from_millis(40);
        let _later = crate::motion::pin_frame_at(mid);
        let coming = placed.openness().progress(mid);
        assert!(coming > 0.0 && coming < 1.0, "it is coming in: {coming}");
        assert!(placed.openness().is_moving(mid));
        drop(_later);

        // A still theme jumps: shown at once, and nothing asks for a frame.
        let mut still_surface = Placed::closed();
        still_surface.follow(rect(0.0), false, still(), MotionSource::Pointer, now);
        still_surface.follow(rect(0.0), true, still(), MotionSource::Pointer, now);
        assert_eq!(
            still_surface.progress(now),
            1.0,
            "shown on the frame it happens"
        );
        assert!(!still_surface.is_moving(now), "and it asks for nothing");

        // Reduced motion lands movement too.
        let mut reduced_surface = Placed::opened();
        reduced_surface.follow(rect(40.0), true, reduced(), MotionSource::Pointer, now);
        reduced_surface.follow(rect(400.0), true, reduced(), MotionSource::Pointer, now);
        assert_eq!(
            reduced_surface.rect(now),
            rect(400.0),
            "a reduced motion lands at its place at once"
        );
        assert!(!reduced_surface.is_moving(now));
    }
}
