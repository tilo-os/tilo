//! [`Placed`] — a surface that appears where it stands, or out of the control that asked for it, leaves
//! from there and moves when its place changes — a window, a place on a canvas, a panel over the
//! screen. A shell draws its windows, its canvas places and its floating surfaces from a view, and a
//! view is built again whenever the layout changes (`shell.md` §7: motion stays in the Rust
//! components, never in the view).

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
    presence: Presence,
    /// Where it is drawn (canvas or layout coordinates): the spring that carries it from the place it
    /// last stood to the one it is laid out at.
    rect: MotionValue<Rect>,
    target: Rect,
    /// It has been placed once: the first placement rests (first paint shows the resting state; motion
    /// explains changes).
    settled: bool,
    /// The motion it last drew with, so a departure after the theme changed keeps the person's reduced
    /// motion.
    motion: EffectiveMotion,
    /// The control its arrival comes out of, and its departure goes back to, in its own coordinates:
    /// the dock icon or the search row the person pressed. `None` for a surface that grows where it
    /// stands.
    control: Option<Point>,
    /// It takes its control from where the pointer was at its first placement: a window the person
    /// pressed for, which the pointer was on. Never for a surface a shortcut or an agent opened.
    pointer_control: bool,
}

impl Default for Placed {
    fn default() -> Self {
        Self {
            presence: Presence::opened(),
            rect: MotionValue::new(Rect::ZERO),
            target: Rect::ZERO,
            settled: false,
            motion: EffectiveMotion::default(),
            control: None,
            pointer_control: false,
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

    /// The surface comes out of where the pointer was at its first placement, when a pointer asked
    /// for it: the control the person pressed. A shortcut or an agent's change is no pointer's, so
    /// such a surface grows where it stands.
    #[must_use]
    pub fn arriving_out_of_pointer(mut self) -> Self {
        self.pointer_control = true;
        self
    }

    /// Where it is drawn this frame, its motion advanced to `now`: laid out at `rect`, up (`open`) or
    /// going, with `motion` the person allows and `source` what asked for the change.
    pub fn follow(
        &mut self,
        rect: Rect,
        open: bool,
        motion: EffectiveMotion,
        source: MotionSource,
        now: Instant,
    ) {
        self.follow_with(rect, open, WindowMotion::OPEN, motion, source, now);
    }

    /// [`Self::follow`] on the motion `spec` names, for a surface that arrives on a pace of its
    /// own (a banner rising from its edge on the panel slide).
    pub fn follow_with(
        &mut self,
        rect: Rect,
        open: bool,
        spec: super::MotionSpec,
        motion: EffectiveMotion,
        source: MotionSource,
        now: Instant,
    ) {
        self.motion = motion;
        // The control the person pressed for it, taken the frame it is first placed: the pointer is
        // where they pressed it. Only a press within the last moment is one (a window a sync brings
        // later grows where it stands, whatever the pointer last touched).
        if !self.settled
            && self.pointer_control
            && self.control.is_none()
            && super::source::recent_input(now).is_some_and(|source| !source.lands_at_once())
        {
            self.control = super::source::last_pointer();
        }
        // The first placement rests where it stands. A surface built up shows at rest (the first
        // paint shows the resting state); one built closed and asked to be up on its first frame
        // comes in, as a window the shell just opened does.
        if !self.settled {
            self.settled = true;
            self.rect.snap(rect);
            self.target = rect;
            if open && !self.presence.is_open() {
                self.presence.snap(false);
                self.presence.show(true, spec, motion, source, now);
            } else {
                self.presence.snap(open);
            }
            return;
        }
        self.presence.show(open, spec, motion, source, now);
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

    /// Whether where it stands is still moving at `now`: what its owner lays out again. Its
    /// arrival or departure is a pose it is drawn in, and only repaints.
    pub fn is_gliding(&self, now: Instant) -> bool {
        self.rect.is_moving(now)
    }

    /// When it arrives or rests; `None` while it is still.
    pub fn settles_at(&self) -> Option<Instant> {
        self.presence.settles_at().max(self.rect.settles_at())
    }

    /// The pose it is drawn under at `now`, in a surface laid out at `rest`: its content's arrival — out
    /// of the control it was asked for from, or where it stands — and the transform carrying it from
    /// where it stood toward where it is laid out.
    pub fn pose(&self, rest: Rect, now: Instant) -> Pose {
        let progress = self.presence.progress(now);
        let content = match self.control {
            Some(control) => WindowMotion::flying_pose(progress, rest, control, self.motion),
            None => WindowMotion::content_pose(progress, rest, self.motion),
        };
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

    /// The ground it stands on moved (a canvas panned or zoomed under it): it stands at `rect` at once,
    /// its coming or going untouched, as the windows in it do. `false` for one never placed, which
    /// [`Self::follow`] places.
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
    /// ([`MotionValue::sampled_after`]). Never for a live frame, which reads [`Self::rect`] at the
    /// frame's instant.
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

    tilo_test_env::scenario! {
        fn a_placed_surface_rests_glides_comes_and_goes;

        phase a_surface_first_placed_rests_where_it_stands;
        {
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

        phase a_moved_surface_glides_to_its_new_place_over_several_frames_then_rests;
        {
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

        phase a_surface_shown_and_hidden_comes_and_goes_and_a_still_theme_lands_at_once;
        {
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
            let mid = now + Duration::from_millis(40);
            let _later = crate::motion::pin_frame_at(mid);
            let coming = placed.openness().progress(mid);
            assert!(coming > 0.0 && coming < 1.0, "it is coming in: {coming}");
            assert!(placed.openness().is_moving(mid));
            drop(_later);

            let mut still_surface = Placed::closed();
            still_surface.follow(rect(0.0), false, still(), MotionSource::Pointer, now);
            still_surface.follow(rect(0.0), true, still(), MotionSource::Pointer, now);
            assert_eq!(
                still_surface.progress(now),
                1.0,
                "shown on the frame it happens"
            );
            assert!(!still_surface.is_moving(now), "and it asks for nothing");

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
}
