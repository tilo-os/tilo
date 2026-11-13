//! Where a scroll view whose content offers detents comes to rest: after a drag, a fling or a
//! wheel gesture ends, a key moves the view, or anything asks it to show something, it settles
//! on one of the offsets its content reports ([`crate::widget::Widget::snap_points`]) rather
//! than wherever the hand left it.

use vello::kurbo::Vec2;

use crate::motion::{ScrollJump, Snap};
use crate::tokens::{EffectiveMotion, MotionSource};
use crate::widget::Scrollable;

/// Two detents closer than this (logical px) are one: a lane's leading edge
/// and the end of the scrollable content are often the same place.
const SAME_DETENT_PX: f32 = 0.5;

impl Scrollable {
    pub(super) fn snaps(&self) -> bool {
        !self.snap_x.is_empty() || !self.snap_y.is_empty()
    }

    /// Read the offsets the content offers along the axes this view scrolls, in content
    /// coordinates, ascending and within what the view can show.
    pub(super) fn collect_snap_points(&mut self) {
        let mut across = std::mem::take(&mut self.snap_x);
        let mut down = std::mem::take(&mut self.snap_y);
        across.clear();
        down.clear();
        if let Some(child) = &self.child {
            let viewport = self.content_viewport_rect();
            let rect = child.get_rect();
            let lead = Vec2::new(
                rect.x0 - viewport.x0 + f64::from(self.offset_x),
                rect.y0 - viewport.y0 + f64::from(self.offset_y),
            );
            if self.axis_has_horizontal() {
                child.snap_points(crate::widget::ScrollAxis::Horizontal, &mut across);
                placed(
                    &mut across,
                    crate::num::f32_of_f64(lead.x),
                    (self.content_w - self.viewport_w).max(0.0),
                );
            }
            if self.axis_has_vertical() {
                child.snap_points(crate::widget::ScrollAxis::Vertical, &mut down);
                placed(
                    &mut down,
                    crate::num::f32_of_f64(lead.y),
                    self.max_offset_y(),
                );
            }
        }
        self.snap_x = across;
        self.snap_y = down;
    }

    /// `offset` along an axis, taken to the detent nearest it where the
    /// content offers any: what the view shows is arranged one detent at a
    /// time, so showing something out of view shows it from its detent.
    pub(super) fn snap_nearest(&self, points: &[f32], offset: f32) -> f32 {
        Snap::nearest(
            points.iter().map(|point| f64::from(*point)),
            f64::from(offset),
        )
        .map_or(offset, crate::num::f32_of_f64)
    }

    /// `offset` taken to the detent nearest it, where that detent still shows the rect at `at`
    /// by `size` in a `viewport` tall or wide view: what the view is asked to show stays shown,
    /// from the detent it rests on.
    pub(super) fn snap_showing(
        &self,
        points: &[f32],
        offset: f32,
        viewport: f32,
        at: f32,
        size: f32,
    ) -> f32 {
        let detent = self.snap_nearest(points, offset);
        let shown =
            detent <= at + super::FIT_EPS && at + size <= detent + viewport + super::FIT_EPS;
        if shown || size > viewport + super::FIT_EPS {
            detent
        } else {
            offset
        }
    }

    /// The hand let go at the offset the content is drawn at, moving at
    /// `velocity` (logical pixels per second, in content space): the view
    /// settles on the detent the release heads for, along every axis the
    /// content offers them.
    pub(super) fn settle_snap(&mut self, velocity: (f64, f64), source: MotionSource) {
        let now = crate::motion::frame_now();
        let drawn = Vec2::new(f64::from(self.offset_x), f64::from(self.offset_y));
        let across = settle_on(&self.snap_x, drawn.x, velocity.0, self.motion);
        let down = settle_on(&self.snap_y, drawn.y, velocity.1, self.motion);
        let target = Vec2::new(across.unwrap_or(drawn.x), down.unwrap_or(drawn.y));
        // An axis the content offers no detent on stays where it is, and its
        // velocity with it: a fling sideways in a list of lanes must not
        // overshoot up and down.
        let carried = Vec2::new(
            if across.is_some() { velocity.0 } else { 0.0 },
            if down.is_some() { velocity.1 } else { 0.0 },
        );
        self.settle_at(target, carried, source, now);
    }

    /// A step of `dx`, `dy` (a wheel notch, a key): to the detent the step
    /// leads to from where the view is going, so keys and wheel notches walk
    /// the detents one at a time.
    pub(super) fn settle_step(&mut self, dx: f32, dy: f32, source: MotionSource) {
        let now = crate::motion::frame_now();
        let drawn = Vec2::new(f64::from(self.offset_x), f64::from(self.offset_y));
        let going = self.scroll_to.target().unwrap_or(drawn);
        let across = step_on(&self.snap_x, going.x, f64::from(dx))
            .or_else(|| Snap::nearest(self.snap_x.iter().map(|p| f64::from(*p)), going.x))
            .unwrap_or(drawn.x);
        let down = step_on(&self.snap_y, going.y, f64::from(dy))
            .or_else(|| Snap::nearest(self.snap_y.iter().map(|p| f64::from(*p)), going.y))
            .unwrap_or(drawn.y);
        let target = Vec2::new(across, down);
        self.settle_at(target, Vec2::ZERO, source, now);
    }

    /// Move the content onto the detent at `target` by the motion a release
    /// takes there ([`crate::motion::ScrollTo::settle`]).
    fn settle_at(
        &mut self,
        target: Vec2,
        velocity: Vec2,
        source: MotionSource,
        now: crate::time::Instant,
    ) {
        let drawn = Vec2::new(f64::from(self.offset_x), f64::from(self.offset_y));
        self.glide.stop();
        if (target - drawn).hypot() < f64::from(SAME_DETENT_PX) {
            return;
        }
        match self
            .scroll_to
            .settle(drawn, target, velocity, self.motion, source, now)
        {
            ScrollJump::Glide => self.follow_scroll_to(now),
            ScrollJump::Land | ScrollJump::FadeIn => self.move_by(
                crate::num::f32_of_f64(target.x) - self.offset_x,
                crate::num::f32_of_f64(target.y) - self.offset_y,
            ),
        }
        if self.scroll_to.is_moving(now) {
            self.scroll_dirty = true;
        }
        self.wake();
    }
}

/// `points` (each measured from the content's leading edge) as the detents
/// the view scrolls to: moved to `lead`, kept within `0..=max`, ascending,
/// one where two land together.
fn placed(points: &mut Vec<f32>, lead: f32, max: f32) {
    for point in points.iter_mut() {
        *point = (lead + *point).clamp(0.0, max);
    }
    points.sort_by(f32::total_cmp);
    points.dedup_by(|a, b| (*a - *b).abs() < SAME_DETENT_PX);
}

/// The detent a release at `from`, moving at `velocity` (logical pixels per second along the
/// axis), settles on: the one nearest where the fling projects ([`Snap::rest_for`]), never
/// behind the way it was thrown, so a fling that is still moving never snaps the content back
/// against it.
fn settle_on(points: &[f32], from: f64, velocity: f64, motion: EffectiveMotion) -> Option<f64> {
    if points.is_empty() {
        return None;
    }
    let rest = Snap::rest_for(from, velocity, motion);
    let flinging = f64::from(crate::virtual_viewport::KINETIC_MIN_VELOCITY);
    let heads_for = points
        .iter()
        .map(|point| f64::from(*point))
        .filter(|point| {
            if velocity >= flinging {
                *point >= from - f64::from(SAME_DETENT_PX)
            } else if velocity <= -flinging {
                *point <= from + f64::from(SAME_DETENT_PX)
            } else {
                true
            }
        });
    Snap::nearest(heads_for, rest)
}

/// The detent a step into `direction` (`+` toward the end) leads to from
/// `from`: the first one past it, or none at the end of the content.
fn step_on(points: &[f32], from: f64, direction: f64) -> Option<f64> {
    let gap = f64::from(SAME_DETENT_PX);
    if direction > 0.0 {
        points
            .iter()
            .map(|point| f64::from(*point))
            .find(|point| *point > from + gap)
    } else if direction < 0.0 {
        points
            .iter()
            .rev()
            .map(|point| f64::from(*point))
            .find(|point| *point < from - gap)
    } else {
        None
    }
}
