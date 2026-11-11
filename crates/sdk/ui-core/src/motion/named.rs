//! The named motions, each defined once: a call site says which one it plays, and its pace, curve,
//! pose and reduced variant are decided here.

use super::pose::Pose;
use super::spec::{Direction, MotionSpec, Pace, Reduced, Shortcut};
use super::spring::SpringSpec;
use super::value::{MotionValue, Timing};
use crate::time::Instant;
use crate::tokens::{EffectiveMotion, MotionSource};
use vello::kurbo::{Affine, Point, Rect, Vec2};

/// `x` clamped to `0..=1` as an opacity.
#[allow(
    clippy::cast_possible_truncation,
    reason = "opacities are 0..=1 and only ever drawn at f32 precision"
)]
#[must_use]
pub fn unit_opacity(x: f64) -> f32 {
    x.clamp(0.0, 1.0) as f32
}

/// The progress of a part of a motion running over `from..to` of the whole one's progress, `0..=1`;
/// parts share its spring, so they stay in step when it is interrupted.
#[must_use]
pub fn stage(progress: f64, from: f64, to: f64) -> f64 {
    if to <= from {
        return if progress >= to { 1.0 } else { 0.0 };
    }
    ((progress - from) / (to - from)).clamp(0.0, 1.0)
}

fn scale_about(origin: Point, scale: f64) -> Affine {
    Affine::scale_about(scale, origin)
}

// ---------------------------------------------------------------------------
// 1. Appear / dismiss
// ---------------------------------------------------------------------------

/// A popover, menu, sheet or dialog coming from the control that summoned it and going back to it,
/// both about the origin nearest that control; the constants below are its shares and reach.
pub struct Appear;

/// How one [`Appear`] surface is drawn at one progress, about its origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coming {
    /// Of its settled size.
    pub scale: f64,
    /// 0..=1.
    pub opacity: f32,
    /// Toward its anchor, in logical pixels: nothing once it rests.
    pub offset: Vec2,
}

impl Appear {
    pub const SPEC: MotionSpec = MotionSpec {
        enter: Pace::Base,
        exit: Pace::Fast,
        reduced: Reduced::Fades,
        shortcut: Shortcut::Lands,
    };
    /// A surface is drawn on the frame after the press, with the motion already under way (50 to 90
    /// ms into a 210 ms motion in a software-rendered session), so it starts where that frame still
    /// reads clearly smaller than at rest, not where it would read at the instant of the press.
    const ENTER_SCALE: f64 = 0.92;
    /// The share of its size it ends leaving at.
    const EXIT_SCALE: f64 = 0.97;
    /// How far toward its anchor it starts arriving and ends leaving, in logical pixels.
    pub const REACH: f64 = 8.0;

    /// How `surface` is drawn `progress` of the way through arriving or leaving `direction`, its
    /// anchor the point nearest the summoning control.
    #[must_use]
    pub fn coming(
        progress: f64,
        direction: Direction,
        surface: Rect,
        anchor: Point,
        motion: EffectiveMotion,
    ) -> Coming {
        let p = progress.clamp(0.0, 1.0);
        if motion.reduced() {
            return Coming {
                scale: 1.0,
                opacity: unit_opacity(p),
                offset: Vec2::ZERO,
            };
        }
        let from = match direction {
            Direction::Enter => Self::ENTER_SCALE,
            Direction::Exit => Self::EXIT_SCALE,
        };
        let eased = Self::eased(p, direction);
        Coming {
            scale: from + (1.0 - from) * eased,
            opacity: unit_opacity(eased),
            offset: Self::toward(surface, anchor) * (1.0 - eased),
        }
    }

    /// Arriving, it eases so the first frame after a press is its faintest and smallest, since a
    /// motion linear in the clock would be a third of the way in by then; leaving, opacity goes at
    /// a steady rate, so every frame of the way out still shows some of it.
    fn eased(progress: f64, direction: Direction) -> f64 {
        match direction {
            Direction::Enter => progress * progress,
            Direction::Exit => progress,
        }
    }

    /// [`Self::coming`] as a [`Pose`], in the surface's own coordinates.
    #[must_use]
    pub fn pose(
        progress: f64,
        direction: Direction,
        surface: Rect,
        anchor: Point,
        motion: EffectiveMotion,
    ) -> Pose {
        let coming = Self::coming(progress, direction, surface, anchor, motion);
        Pose {
            opacity: coming.opacity,
            transform: if coming.scale >= 1.0 && coming.offset == Vec2::ZERO {
                Affine::IDENTITY
            } else {
                Affine::translate(coming.offset)
                    * scale_about(Self::origin(surface, anchor), coming.scale)
            },
        }
    }

    /// How far and which way it stands from where it rests at its first frame.
    fn toward(surface: Rect, anchor: Point) -> Vec2 {
        let out = Self::origin(surface, anchor) - surface.center();
        let length = out.hypot();
        if length > f64::EPSILON {
            out * (Self::REACH / length)
        } else {
            Vec2::ZERO
        }
    }

    /// The point of `surface` nearest `anchor`: where it appears from.
    #[must_use]
    pub fn origin(surface: Rect, anchor: Point) -> Point {
        Point::new(
            anchor.x.clamp(surface.x0, surface.x1),
            anchor.y.clamp(surface.y0, surface.y1),
        )
    }
}

// ---------------------------------------------------------------------------
// 2. Drill in / back
// ---------------------------------------------------------------------------

/// Moving deeper into a panel and back: Quick Settings submenus, push navigation, settings
/// sub-pages. Each page is posed by its offset from one continuous depth ([`DrillDepth`]).
pub struct Drill;

impl Drill {
    pub const SPEC: MotionSpec = MotionSpec {
        enter: Pace::Base,
        exit: Pace::Base,
        reduced: Reduced::Fades,
        shortcut: Shortcut::Lands,
    };
    /// Switching to another workspace by pointer: the whole view goes along the same shared axis,
    /// `slow` toward a later workspace and `base` back; a shortcut lands.
    pub const WORKSPACE: MotionSpec = MotionSpec {
        enter: Pace::Slow,
        exit: Pace::Base,
        reduced: Reduced::Fades,
        shortcut: Shortcut::Lands,
    };
    const SHIFT_PX: f64 = 28.0;
    const SHIFT_FRACTION: f64 = 0.1;
    const FADE_RATE: f64 = 1.6;
    /// How far the column behind a whole-column push trails.
    const PARALLAX: f64 = 0.3;

    /// The pose of a page `offset` levels from the drawn depth, in a panel `width` wide; invisible
    /// pages need not be drawn.
    #[must_use]
    pub fn pose(offset: f64, width: f64, motion: EffectiveMotion) -> Pose {
        let shown = unit_opacity(1.0 - Self::FADE_RATE * offset.abs());
        let shift = if motion.reduced() {
            0.0
        } else {
            offset.clamp(-1.0, 1.0) * Self::SHIFT_PX.min(width.max(0.0) * Self::SHIFT_FRACTION)
        };
        Pose {
            opacity: shown,
            transform: Affine::translate((shift, 0.0)),
        }
    }

    /// The pose of a whole column `offset` columns from the drawn one, in a window `width` wide.
    #[must_use]
    pub fn screen_pose(offset: f64, width: f64, motion: EffectiveMotion) -> Pose {
        if motion.reduced() {
            return Pose {
                opacity: unit_opacity(1.0 - offset.abs()),
                transform: Affine::IDENTITY,
            };
        }
        let o = offset.clamp(-1.0, 1.0);
        let shift = if o >= 0.0 {
            o * width
        } else {
            o * width * Self::PARALLAX
        };
        Pose {
            opacity: 1.0,
            transform: Affine::translate((shift, 0.0)),
        }
    }
}

/// The drawn depth of a drill-in stack.
#[derive(Clone, Copy, Debug)]
pub struct DrillDepth {
    depth: MotionValue<f64>,
}

impl Default for DrillDepth {
    fn default() -> Self {
        Self::at(0)
    }
}

impl DrillDepth {
    /// At rest at `level` (0 is the root).
    #[must_use]
    pub fn at(level: usize) -> Self {
        Self {
            depth: MotionValue::new(level_f64(level)),
        }
    }

    /// Follow the stack to `level`. Deeper is an enter, shallower an exit.
    #[track_caller]
    pub fn go(
        &mut self,
        level: usize,
        motion: EffectiveMotion,
        source: MotionSource,
        now: Instant,
    ) {
        let target = level_f64(level);
        let direction = Direction::of(target >= self.depth.target());
        let timing = Drill::SPEC.timing(direction, motion, source);
        self.depth.set(target, timing, now);
    }

    /// Jump to `level`.
    pub fn snap(&mut self, level: usize) {
        self.depth.snap(level_f64(level));
    }

    /// Drive the depth directly (a back swipe), at rest where the hand is.
    pub fn track(&mut self, depth: f64) {
        self.depth.snap(depth);
    }

    /// Release a swipe at `velocity` (levels per second) toward `level`.
    #[track_caller]
    pub fn release(&mut self, level: usize, velocity: f64, motion: EffectiveMotion, now: Instant) {
        let timing = Drill::SPEC.timing(Direction::Exit, motion, MotionSource::Gesture);
        self.depth.release(level_f64(level), velocity, timing, now);
    }

    #[track_caller]
    pub fn get(&self, now: Instant) -> f64 {
        self.depth.get(now)
    }

    /// The level it is going to.
    pub fn target(&self) -> f64 {
        self.depth.target()
    }

    #[track_caller]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.depth.is_moving(now)
    }

    pub fn settles_at(&self) -> Option<Instant> {
        self.depth.settles_at()
    }
}

fn level_f64(level: usize) -> f64 {
    f64::from(u32::try_from(level).unwrap_or(u32::MAX))
}

// ---------------------------------------------------------------------------
// 3. Expand / collapse
// ---------------------------------------------------------------------------

/// A disclosure, an accordion, a panel whose height follows its content: the clip height moves over
/// content laid out once at its full height and fades in over the second half; open at `base`,
/// close at `fast`, longer for tall reveals.
pub struct Disclose;

/// How much of a disclosed block shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reveal {
    /// The clip height.
    pub height: f64,
    /// The content's opacity.
    pub opacity: f32,
    /// The chevron's turn, `0..=1` of a quarter turn.
    pub turn: f64,
}

impl Disclose {
    pub const SPEC: MotionSpec = MotionSpec {
        enter: Pace::Base,
        exit: Pace::Fast,
        reduced: Reduced::Lands,
        shortcut: Shortcut::Lands,
    };
    /// A reveal this tall takes the token's time; taller ones take longer.
    pub const REFERENCE_PX: f64 = 240.0;

    /// How a height (or any extent) goes from `from` to `to`.
    #[must_use]
    pub fn timing(from: f64, to: f64, motion: EffectiveMotion, source: MotionSource) -> Timing {
        Self::SPEC.timing_over(
            Direction::of(to >= from),
            motion,
            source,
            (to - from).abs() / Self::REFERENCE_PX,
        )
    }

    /// The reveal at `progress` of a block `full_height` tall.
    #[must_use]
    pub fn reveal(progress: f64, full_height: f64) -> Reveal {
        let p = progress.clamp(0.0, 1.0);
        Reveal {
            height: full_height * p,
            opacity: unit_opacity(2.0 * p - 1.0),
            turn: p,
        }
    }
}

/// An extent that follows its content (a panel's height when its page changes), on the
/// expand/collapse motion.
#[derive(Clone, Copy, Debug)]
pub struct Extent {
    value: MotionValue<f64>,
    known: bool,
}

impl Default for Extent {
    fn default() -> Self {
        Self {
            value: MotionValue::new(0.0).with_tolerance(0.25),
            known: false,
        }
    }
}

impl Extent {
    /// Follow `to`; the first extent is taken as it is, so nothing grows on first paint.
    #[track_caller]
    pub fn follow(&mut self, to: f64, motion: EffectiveMotion, source: MotionSource, now: Instant) {
        if !self.known {
            self.known = true;
            self.value.snap(to);
            return;
        }
        let from = self.value.get(now);
        self.value
            .set(to, Disclose::timing(from, to, motion, source), now);
    }

    /// Jump to `to`.
    pub fn snap(&mut self, to: f64) {
        self.known = true;
        self.value.snap(to);
    }

    #[track_caller]
    pub fn get(&self, now: Instant) -> f64 {
        self.value.get(now)
    }

    pub fn target(&self) -> f64 {
        self.value.target()
    }

    /// Whether it has measured its content yet: the first extent is taken as it is, so a window
    /// that follows it can ask for that height before the first frame that follows it is drawn.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        self.known
    }

    #[track_caller]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.value.is_moving(now)
    }

    pub fn settles_at(&self) -> Option<Instant> {
        self.value.settles_at()
    }
}

// ---------------------------------------------------------------------------
// 4. Panel slide
// ---------------------------------------------------------------------------

/// A sidebar, drawer or inspector sliding in from its edge. Content beside a docked panel is laid
/// out at the wider of its two widths and slides with it, so it reflows once, never per frame
/// ([`Self::content_width`]).
pub struct PanelSlide;

impl PanelSlide {
    pub const SPEC: MotionSpec = MotionSpec {
        enter: Pace::Base,
        exit: Pace::Base,
        reduced: Reduced::Lands,
        shortcut: Shortcut::Lands,
    };

    /// How much of a panel `extent` wide shows at `progress`.
    #[must_use]
    pub fn shown(progress: f64, extent: f64) -> f64 {
        extent * progress.clamp(0.0, 1.0)
    }

    /// The pose of a modal edge sheet at `progress`: it slides in from its edge by a quarter of its
    /// width (at most 48 px) while it fades in, so it reads as coming from the edge without
    /// crossing the screen; `trailing` sheets come from the right.
    #[must_use]
    pub fn sheet_pose(progress: f64, width: f64, trailing: bool, motion: EffectiveMotion) -> Pose {
        let p = progress.clamp(0.0, 1.0);
        let travel = if motion.reduced() {
            0.0
        } else {
            (width.max(0.0) * 0.25).min(48.0) * (1.0 - p)
        };
        Pose {
            opacity: unit_opacity(stage(p, 0.0, 0.6)),
            transform: Affine::translate((if trailing { travel } else { -travel }, 0.0)),
        }
    }

    /// The width content beside the panel is laid out at: `settled` at rest, the widest it will be
    /// (`widest`) while the panel moves, so no frame re-lays it out.
    #[must_use]
    pub fn content_width(settled: f64, widest: f64, moving: bool) -> f64 {
        if moving { settled.max(widest) } else { settled }
    }
}

// ---------------------------------------------------------------------------
// 5. Cross-fade
// ---------------------------------------------------------------------------

/// Swapping one content for another in place (the bar's clock and a live activity, an image over
/// its placeholder): the old fades out over the first 60 %, the new in over the whole `fast`
/// spring, nothing moves. Also the timing for a fade that carries no content of its own.
pub const CROSS_FADE: MotionSpec = MotionSpec {
    enter: Pace::Fast,
    exit: Pace::Fast,
    reduced: Reduced::Fades,
    shortcut: Shortcut::Lands,
};

#[derive(Clone, Debug)]
pub struct CrossFade<K> {
    shown: K,
    previous: Option<(K, f32)>,
    progress: MotionValue<f64>,
}

impl<K: Clone + PartialEq> CrossFade<K> {
    pub const SPEC: MotionSpec = CROSS_FADE;
    const OUT_BY: f64 = 0.6;

    /// Showing `shown`, at rest.
    pub fn new(shown: K) -> Self {
        Self {
            shown,
            previous: None,
            progress: MotionValue::new(1.0),
        }
    }

    /// Show `next`; a no-op when it is already shown.
    #[track_caller]
    pub fn show(&mut self, next: K, motion: EffectiveMotion, source: MotionSource, now: Instant) {
        if next == self.shown {
            return;
        }
        let timing = Self::SPEC.timing(Direction::Enter, motion, source);
        self.show_with(next, timing, now);
    }

    /// Show `next`, data that arrived (a read's answer replacing its placeholder): no hand caused
    /// it, so it fades at `fast` rather than landing or waiting on input; a no-op when already
    /// shown.
    #[track_caller]
    pub fn arrive(&mut self, next: K, motion: EffectiveMotion, now: Instant) {
        if next == self.shown {
            return;
        }
        let timing = Timing::Spring(SpringSpec::settling_within(
            motion.tokens().durations.fast(),
        ));
        self.show_with(next, timing, now);
    }

    #[track_caller]
    fn show_with(&mut self, next: K, timing: Timing, now: Instant) {
        if timing.is_instant() {
            self.shown = next;
            self.previous = None;
            self.progress.snap(1.0);
            return;
        }
        let drawn = unit_opacity(self.progress.get(now));
        let old = std::mem::replace(&mut self.shown, next);
        self.previous = Some((old, drawn));
        self.progress.snap(0.0);
        self.progress.set(1.0, timing, now);
    }

    /// What is shown (the newest content).
    pub fn shown(&self) -> &K {
        &self.shown
    }

    /// The outgoing content and its opacity, while it still shows.
    #[track_caller]
    pub fn outgoing(&self, now: Instant) -> Option<(&K, f32)> {
        if !self.progress.is_moving(now) {
            return None;
        }
        let p = self.progress.get(now);
        self.previous
            .as_ref()
            .map(|(k, from)| (k, from * unit_opacity(1.0 - p / Self::OUT_BY)))
            .filter(|(_, o)| *o > 0.0)
    }

    /// The incoming (shown) content's opacity.
    #[track_caller]
    pub fn incoming(&self, now: Instant) -> f32 {
        unit_opacity(self.progress.get(now))
    }

    #[track_caller]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.progress.is_moving(now)
    }

    pub fn settles_at(&self) -> Option<Instant> {
        self.progress.settles_at()
    }
}

// ---------------------------------------------------------------------------
// 6. List insert / remove / reorder
// ---------------------------------------------------------------------------

/// Rows arriving, leaving and changing places: a survivor springs from its old place to its new one
/// (`base`), arrivals and departures fade while their slots open and close (`fast`), driven by
/// [`crate::collection_transition::RowMotions`]; a change that would move the row under an aiming
/// pointer waits ([`held_by_aim`]).
pub struct ListChange;

impl ListChange {
    pub const MOVE: MotionSpec = MotionSpec {
        enter: Pace::Base,
        exit: Pace::Base,
        reduced: Reduced::Lands,
        shortcut: Shortcut::Lands,
    };
    pub const FADE: MotionSpec = MotionSpec {
        enter: Pace::Fast,
        exit: Pace::Fast,
        reduced: Reduced::Fades,
        shortcut: Shortcut::Lands,
    };
}

/// Whether a motion that would move `rect` must wait: the pointer is aiming at it.
#[must_use]
pub fn held_by_aim(aim: &crate::widget::aim::Trajectory, rect: Rect, now: Instant) -> bool {
    aim.aims_at(rect, now)
}

// ---------------------------------------------------------------------------
// 7. Scroll / carousel snap
// ---------------------------------------------------------------------------

/// A carousel or paged strip settling on a detent: the release projects the fling
/// ([`Self::rest_for`]), picks the nearest detent and carries the release velocity into it (`base`,
/// no bounce); a wheel notch retargets, so spins accumulate into one motion.
pub struct Snap;

impl Snap {
    pub const SPEC: MotionSpec = MotionSpec {
        enter: Pace::Base,
        exit: Pace::Base,
        reduced: Reduced::Lands,
        shortcut: Shortcut::Context,
    };
    /// A wheel step's glide (zooming a canvas a notch): `fast`, retargeted by every further notch.
    pub const WHEEL: MotionSpec = MotionSpec {
        enter: Pace::Fast,
        exit: Pace::Fast,
        reduced: Reduced::Lands,
        shortcut: Shortcut::Context,
    };
    /// A thrown surface coasting on: released with the fingers' velocity toward where the release
    /// projects ([`Self::rest_for`]), it slows exponentially over the gesture token's coast and
    /// rests there without turning back.
    pub const COAST: MotionSpec = MotionSpec {
        enter: Pace::Coast,
        exit: Pace::Coast,
        reduced: Reduced::Lands,
        shortcut: Shortcut::Lands,
    };

    /// Where a release at `position` moving at `velocity` would come to rest: the projection a
    /// flick is judged by.
    #[must_use]
    pub fn rest_for(position: f64, velocity: f64, motion: EffectiveMotion) -> f64 {
        let coast = f64::from(motion.tokens().gesture.fling_projection_ms) / 1000.0;
        position + velocity * coast
    }

    #[must_use]
    pub fn nearest(detents: impl IntoIterator<Item = f64>, position: f64) -> Option<f64> {
        detents
            .into_iter()
            .min_by(|a, b| (a - position).abs().total_cmp(&(b - position).abs()))
    }
}

// ---------------------------------------------------------------------------
// 8. Shared element / zoom
// ---------------------------------------------------------------------------

/// One thing becoming another in place (a window and its overview card, a photo and its full view):
/// one rect spring between the two (`slow` in, `base` out, longer across the screen) and the
/// contents cross-fade over the middle half.
pub struct Zoom;

impl Zoom {
    pub const SPEC: MotionSpec = MotionSpec {
        enter: Pace::Slow,
        exit: Pace::Base,
        reduced: Reduced::Fades,
        shortcut: Shortcut::Lands,
    };

    /// The workspace overview: every window flying from its place on its strip into the overview's
    /// picture of the workspace, and back. Where the windows went is the point, so a shortcut
    /// (Super+Tab) keeps it as spatial context at `fast`.
    pub const OVERVIEW: MotionSpec = MotionSpec {
        enter: Pace::Slow,
        exit: Pace::Base,
        reduced: Reduced::Fades,
        shortcut: Shortcut::Context,
    };

    /// A corner radius between `from` (0) and `to` (1) at `progress`.
    #[must_use]
    pub fn radius_at(from: f64, to: f64, progress: f64) -> f64 {
        let p = progress.clamp(0.0, 1.0);
        from + (to - from) * p
    }

    /// The transform that draws content laid out in `rest` into `drawn`.
    #[must_use]
    pub fn fit(rest: Rect, drawn: Rect) -> Affine {
        let sx = if rest.width() > 0.0 {
            drawn.width() / rest.width()
        } else {
            1.0
        };
        let sy = if rest.height() > 0.0 {
            drawn.height() / rest.height()
        } else {
            1.0
        };
        // p ↦ drawn.origin + s·(p − rest.origin), as one matrix.
        Affine::new([
            sx,
            0.0,
            0.0,
            sy,
            drawn.x0 - sx * rest.x0,
            drawn.y0 - sy * rest.y0,
        ])
    }

    /// The rect between `from` (0) and `to` (1) at `progress`.
    #[must_use]
    pub fn rect_at(from: Rect, to: Rect, progress: f64) -> Rect {
        let p = progress.clamp(0.0, 1.0);
        Rect::new(
            from.x0 + (to.x0 - from.x0) * p,
            from.y0 + (to.y0 - from.y0) * p,
            from.x1 + (to.x1 - from.x1) * p,
            from.y1 + (to.y1 - from.y1) * p,
        )
    }

    /// The opacities of the source and destination content at `progress`: the swap is over the
    /// middle half.
    #[must_use]
    pub fn contents(progress: f64) -> (f32, f32) {
        let p = progress.clamp(0.0, 1.0);
        let incoming = unit_opacity((p - 0.25) * 2.0);
        (1.0 - incoming, incoming)
    }

    /// How a travel from `from` to `to` is timed: distance-aware against the larger of the two
    /// sizes.
    #[must_use]
    pub fn timing(
        from: Rect,
        to: Rect,
        direction: Direction,
        motion: EffectiveMotion,
        source: MotionSource,
    ) -> Timing {
        let travel = (to.center() - from.center()).hypot();
        let reference = from.width().max(to.width()).max(1.0);
        Self::SPEC.timing_over(direction, motion, source, travel / reference)
    }
}

// ---------------------------------------------------------------------------
// 9. Windows on the strip
// ---------------------------------------------------------------------------

/// Windows opening, closing, moving and resizing on the strip: rects spring on `springs.heavy`
/// (critically damped, mass you can feel), longer for a long move; an opening window's content
/// fades in and scales 0.96 → 1 about its centre (`slow`); a drag is welded to the hand.
pub struct WindowMotion;

impl WindowMotion {
    /// A group's rect following its slot.
    pub const MOVE: MotionSpec = MotionSpec {
        enter: Pace::Heavy,
        exit: Pace::Heavy,
        reduced: Reduced::Lands,
        shortcut: Shortcut::Lands,
    };
    /// A window's content arriving and leaving: arriving is a whole-view change (`slow`, long
    /// enough to read where it went), leaving `base`.
    pub const OPEN: MotionSpec = MotionSpec {
        enter: Pace::Slow,
        exit: Pace::Base,
        reduced: Reduced::Fades,
        shortcut: Shortcut::Lands,
    };
    /// The other windows on a display fading while one is maximized over
    /// them (rather than being squashed), and back on restore: `fast`;
    /// under reduced motion they are gone or back at once.
    pub const SIBLINGS: MotionSpec = MotionSpec {
        enter: Pace::Fast,
        exit: Pace::Fast,
        reduced: Reduced::Lands,
        shortcut: Shortcut::Lands,
    };
    const START_SCALE: f64 = 0.96;

    /// The pose of a window's content at `progress` of its opening, in a frame `rect`.
    #[must_use]
    pub fn content_pose(progress: f64, rect: Rect, motion: EffectiveMotion) -> Pose {
        let p = progress.clamp(0.0, 1.0);
        if motion.reduced() || p >= 1.0 {
            return Pose {
                opacity: unit_opacity(p),
                transform: Affine::IDENTITY,
            };
        }
        Pose {
            opacity: unit_opacity(p),
            transform: scale_about(
                rect.center(),
                Self::START_SCALE + (1.0 - Self::START_SCALE) * p,
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// 9b. Ambient chrome
// ---------------------------------------------------------------------------

/// Chrome that comes back on its own and goes when it would be in the way: desk widgets, the Super
/// surface's time card. Leaving is instant, since motion would draw the eye for nothing; arriving
/// unasked is slow and opacity only; arriving asked for is `fast`.
pub struct Ambient;

impl Ambient {
    /// Arriving on its own.
    pub const UNASKED: MotionSpec = MotionSpec {
        enter: Pace::Slow,
        exit: Pace::Instant,
        reduced: Reduced::Fades,
        shortcut: Shortcut::Lands,
    };
    /// Arriving because the person asked.
    pub const ASKED: MotionSpec = MotionSpec {
        enter: Pace::Fast,
        exit: Pace::Instant,
        reduced: Reduced::Fades,
        shortcut: Shortcut::Lands,
    };

    /// The spec for an arrival the person did or did not ask for.
    #[must_use]
    pub fn spec(asked: bool) -> MotionSpec {
        if asked { Self::ASKED } else { Self::UNASKED }
    }
}

/// Something shown for a while that then goes on its own: a volume readout, an acknowledgement, a
/// player's controls. A readout arrives at `fast`, a confirmation is there at once; both hold, then
/// fade over `base` so they are gone exactly when they expire. Pure functions of two instants, so
/// the owner keeps no motion state.
pub struct Transient;

impl Transient {
    /// A readout that arrives (a volume level, a player's controls).
    pub const READOUT: MotionSpec = MotionSpec {
        enter: Pace::Fast,
        exit: Pace::Base,
        reduced: Reduced::Lands,
        shortcut: Shortcut::Context,
    };
    /// A confirmation, there the moment the action is done; also chrome the pointer wakes (a
    /// player's controls).
    pub const CONFIRMATION: MotionSpec = MotionSpec {
        enter: Pace::Instant,
        exit: Pace::Base,
        reduced: Reduced::Lands,
        shortcut: Shortcut::Context,
    };

    /// How long its exit fade takes.
    #[must_use]
    pub fn exit_time(spec: MotionSpec, motion: EffectiveMotion) -> crate::time::Duration {
        spec.settle_time(Direction::Exit, motion, MotionSource::Pointer)
    }

    /// How long its arrival takes.
    #[must_use]
    pub fn enter_time(spec: MotionSpec, motion: EffectiveMotion) -> crate::time::Duration {
        spec.settle_time(Direction::Enter, motion, MotionSource::Pointer)
    }

    /// When the exit fade starts, for something that expires at `expires`.
    #[must_use]
    pub fn exit_starts(spec: MotionSpec, expires: Instant, motion: EffectiveMotion) -> Instant {
        expires
            .checked_sub(Self::exit_time(spec, motion))
            .unwrap_or(expires)
    }

    /// How far through its exit fade something leaving at `since` is at `now`, `0..=1`: the fade
    /// takes the motion's own exit time.
    #[must_use]
    pub fn exit_progress(
        spec: MotionSpec,
        since: Instant,
        now: Instant,
        motion: EffectiveMotion,
    ) -> f64 {
        let fade = Self::exit_time(spec, motion).as_secs_f64().max(1e-3);
        (now.saturating_duration_since(since).as_secs_f64() / fade).clamp(0.0, 1.0)
    }

    /// Its opacity at `now`, shown since `shown` and gone at `expires`.
    #[must_use]
    pub fn opacity(
        spec: MotionSpec,
        (shown, expires): (Instant, Instant),
        now: Instant,
        motion: EffectiveMotion,
    ) -> f32 {
        if now >= expires {
            return 0.0;
        }
        let source = MotionSource::Pointer;
        let arrived = spec.progress_after(
            Direction::Enter,
            motion,
            source,
            now.saturating_duration_since(shown),
        );
        let fade_from = Self::exit_starts(spec, expires, motion);
        let left = if now < fade_from {
            0.0
        } else {
            spec.progress_after(
                Direction::Exit,
                motion,
                source,
                now.saturating_duration_since(fade_from),
            )
        };
        unit_opacity(arrived * (1.0 - left))
    }

    /// Whether its opacity changes at `now` (arriving or fading), so its owner needs frames.
    #[must_use]
    pub fn is_moving(
        spec: MotionSpec,
        (shown, expires): (Instant, Instant),
        now: Instant,
        motion: EffectiveMotion,
    ) -> bool {
        if now >= expires {
            return false;
        }
        now.saturating_duration_since(shown) < Self::enter_time(spec, motion)
            || (now >= Self::exit_starts(spec, expires, motion)
                && !Self::exit_time(spec, motion).is_zero())
    }
}

/// Something following a stream of samples it does not control (a peer's pointer, heard about
/// thirty times a second): between two samples it glides over the gap so it arrives as the next is
/// due ([`Self::glide`]), and when the stream goes quiet it fades over the caller's retirement
/// window. It has no token: the pace is the data's own.
#[crate::component::not_a_component(framework, "motion the framework applies")]
pub struct Follow;

impl Follow {
    /// An agent's pointer travelling to the control it is about to act on,
    /// so the person sees where it acts: `slow`, longer across the screen.
    /// Reduced motion: it is there at once.
    pub const APPROACH: MotionSpec = MotionSpec {
        enter: Pace::Slow,
        exit: Pace::Fast,
        reduced: Reduced::Lands,
        shortcut: Shortcut::Context,
    };

    /// The longest a glide between two samples takes, so a pointer that sat still goes where it was
    /// just put without drifting lazily there; a stream heard thirty times a second glides about 33
    /// ms.
    pub const GLIDE_MAX: crate::time::Duration = crate::time::Duration::from_millis(100);

    /// How long something whose stream went quiet takes to fade out, at the end of its owner's
    /// retirement window.
    pub const FADE: crate::time::Duration = crate::time::Duration::from_millis(400);

    /// How far a glide that started at `since` and spans `span` has come at `now`, eased, `0..=1`.
    #[must_use]
    pub fn glide(since: Instant, span: crate::time::Duration, now: Instant) -> f32 {
        if span.is_zero() {
            return 1.0;
        }
        let t = (now.saturating_duration_since(since).as_secs_f64() / span.as_secs_f64())
            .clamp(0.0, 1.0);
        unit_opacity(t * t * (3.0 - 2.0 * t))
    }

    /// The opacity of something that goes `left` from now, fading over the last `fade` of it; whole
    /// while `fade` is zero (reduced motion).
    #[must_use]
    pub fn fading(left: crate::time::Duration, fade: crate::time::Duration) -> f32 {
        if fade.is_zero() || left >= fade {
            return 1.0;
        }
        unit_opacity(left.as_secs_f64() / fade.as_secs_f64())
    }
}

/// A mark a collaborator just left (the run a peer typed) going on its own: whole when made, it
/// steps down evenly over [`Self::LIFE`] in [`Self::STEPS`] steps, each step a wake deadline, so a
/// typing window repaints a few times a second rather than at every refresh.
pub struct Trace;

impl Trace {
    /// How long a mark shows, its taper included.
    pub const LIFE: crate::time::Duration = crate::time::Duration::from_millis(2500);
    /// How many steps it tapers in.
    pub const STEPS: u32 = 10;
    /// One step of the taper.
    const STEP: crate::time::Duration = match Self::LIFE.checked_div(Self::STEPS) {
        Some(step) => step,
        None => crate::time::Duration::ZERO,
    };

    /// Which step a mark `age` old is in; `None` once it has gone.
    #[must_use]
    pub fn step(age: crate::time::Duration) -> Option<u32> {
        (age < Self::LIFE).then(|| {
            u32::try_from(age.as_nanos() / Self::STEP.as_nanos().max(1)).unwrap_or(u32::MAX)
        })
    }

    /// The opacity of a mark `age` old, step by step; `None` once it has gone.
    #[must_use]
    pub fn alpha(age: crate::time::Duration) -> Option<f32> {
        Self::step(age).map(|step| (1.0 - step as f32 / Self::STEPS as f32).clamp(0.0, 1.0))
    }

    /// When a mark made at `born` leaves the step it is in at `now`; `None` once it has gone.
    #[must_use]
    pub fn next_step(born: Instant, now: Instant) -> Option<Instant> {
        let step = Self::step(now.saturating_duration_since(born))?;
        born.checked_add(Self::STEP.saturating_mul(step.saturating_add(1)))
    }
}

/// A reaction floating up from the pointer it was sent from: it rises, slowing as it goes, and
/// fades over the second half of a fixed life, since the reaction is the content of that moment and
/// has to be readable before it goes.
pub struct Float;

impl Float {
    /// How long a reaction is shown.
    pub const LIFE: crate::time::Duration = crate::time::Duration::from_millis(1200);
    /// How far it rises over its life, in logical pixels.
    pub const RISE_PX: f64 = 48.0;

    /// How far above where it started it is drawn, and how opaque, at `age`; `None` once it is
    /// gone.
    #[must_use]
    pub fn at(age: crate::time::Duration, motion: EffectiveMotion) -> Option<(f64, f32)> {
        if age >= Self::LIFE {
            return None;
        }
        let t = (age.as_secs_f64() / Self::LIFE.as_secs_f64()).clamp(0.0, 1.0);
        let rise = if motion.reduced() {
            0.0
        } else {
            Self::RISE_PX * (1.0 - (1.0 - t) * (1.0 - t))
        };
        let opacity = if t < 0.5 { 1.0 } else { (1.0 - t) * 2.0 };
        Some((rise, unit_opacity(opacity)))
    }
}

/// A state that is live for as long as it lasts: the soft ring on the face of someone in a call. It
/// breathes slowly, the only motion that loops, so it is reserved for something happening right
/// now; where motion may not loop it holds at full.
pub struct Breathe;

impl Breathe {
    /// One breath, in and out.
    pub const PERIOD: crate::time::Duration = crate::time::Duration::from_millis(1800);

    /// How strong it is `since` it began, `0.35..=1`.
    #[must_use]
    pub fn level(since: crate::time::Duration, motion: EffectiveMotion) -> f32 {
        if !motion.loops() {
            return 1.0;
        }
        let phase = (since.as_secs_f64() / Self::PERIOD.as_secs_f64()).fract();
        let wave = 0.5 - 0.5 * (phase * std::f64::consts::TAU).cos();
        unit_opacity(0.35 + 0.65 * wave)
    }

    /// When to paint it next: a breath is drawn at a gentle frame rate, not the display's.
    #[must_use]
    pub fn next_frame(now: Instant, motion: EffectiveMotion) -> Option<Instant> {
        motion
            .loops()
            .then(|| now + crate::time::Duration::from_millis(66))
    }
}

// ---------------------------------------------------------------------------
// 10. Press and hover feedback
// ---------------------------------------------------------------------------

/// The pressed or hovered tint of a control: it comes at once (zero latency is the feedback) and
/// goes over `fast`; list rows and menu items use the instant state, as hovering across a list is
/// too frequent to leave trails.
#[derive(Clone, Copy, Debug)]
pub struct Feedback {
    level: MotionValue<f64>,
}

impl Default for Feedback {
    fn default() -> Self {
        Self {
            level: MotionValue::new(0.0),
        }
    }
}

impl Feedback {
    /// Turn the feedback on or off at `now`.
    #[track_caller]
    pub fn set(&mut self, on: bool, motion: EffectiveMotion, now: Instant) {
        if on {
            self.level.snap(1.0);
        } else {
            let fast = SpringSpec::settling_within(motion.tokens().durations.fast());
            self.level.set(0.0, Timing::Spring(fast), now);
        }
    }

    /// How strongly the tint shows at `now`, `0..=1`.
    #[track_caller]
    pub fn level(&self, now: Instant) -> f32 {
        unit_opacity(self.level.get(now))
    }

    #[track_caller]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.level.is_moving(now)
    }

    pub fn settles_at(&self) -> Option<Instant> {
        self.level.settles_at()
    }
}

/// A person's voice on their face while they talk in a call: the ring is there the moment their
/// voice is heard and eases out over `fast` when they pause, so it pulses with their speech.
#[derive(Clone, Copy, Debug, Default)]
pub struct Speaking {
    level: Feedback,
}

impl Speaking {
    /// Whether they are heard at `now`.
    #[track_caller]
    pub fn set(&mut self, speaking: bool, motion: EffectiveMotion, now: Instant) {
        if motion.reduced() && !speaking {
            self.level = Feedback::default();
            return;
        }
        self.level.set(speaking, motion, now);
    }

    /// How strongly the ring shows at `now`, `0..=1`.
    #[track_caller]
    pub fn level(&self, now: Instant) -> f32 {
        self.level.level(now)
    }

    #[track_caller]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.level.is_moving(now)
    }

    pub fn settles_at(&self) -> Option<Instant> {
        self.level.settles_at()
    }
}

// ---------------------------------------------------------------------------
// 11. Attention
// ---------------------------------------------------------------------------

/// Calling the person's eye: a refusal shakes (a wrong password), something that needs them pulses
/// once; never loops.
#[derive(Clone, Copy, Debug)]
pub struct Attention {
    /// A horizontal offset in px (shake) or a scale above 1 (pulse).
    amount: MotionValue<f64>,
    tint: MotionValue<f64>,
}

impl Default for Attention {
    fn default() -> Self {
        Self {
            amount: MotionValue::new(0.0).with_tolerance(0.05),
            tint: MotionValue::new(0.0),
        }
    }
}

impl Attention {
    /// A highlight that flashes and fades over `base` (an agent's click landing).
    pub const FLASH: MotionSpec = MotionSpec {
        enter: Pace::Base,
        exit: Pace::Base,
        reduced: Reduced::Fades,
        shortcut: Shortcut::Lands,
    };

    /// The shake's spring: lightly damped, so it swings a few times.
    const SHAKE: SpringSpec = SpringSpec {
        response: 0.18,
        damping: 0.3,
    };
    const SHAKE_PX: f64 = 8.0;
    const PULSE: f64 = 0.06;

    /// Refuse: shake sideways (or flash, under reduced motion).
    #[track_caller]
    pub fn refuse(&mut self, motion: EffectiveMotion, now: Instant) {
        if motion.reduced() {
            self.flash(motion, now);
            return;
        }
        let w = Self::SHAKE.omega();
        let z = Self::SHAKE.damping;
        let wd = w * (1.0 - z * z).sqrt();
        let first_peak = (wd / (z * w)).atan() / wd;
        let gain = (-z * w * first_peak).exp() * (wd * first_peak).sin() / wd;
        let kick = Self::SHAKE_PX / gain.max(1e-6);
        self.amount
            .release(0.0, kick, Timing::Spring(Self::SHAKE), now);
    }

    /// Pulse once: up by 6 % and back (or flash, under reduced motion).
    #[track_caller]
    pub fn pulse(&mut self, motion: EffectiveMotion, now: Instant) {
        if motion.reduced() {
            self.flash(motion, now);
            return;
        }
        let spring = SpringSpec::settling_within(motion.tokens().durations.base());
        // A critically damped spring kicked from rest peaks at v0 / (ω e).
        let kick = Self::PULSE * spring.omega() * std::f64::consts::E;
        self.amount.release(0.0, kick, Timing::Spring(spring), now);
    }

    #[track_caller]
    fn flash(&mut self, motion: EffectiveMotion, now: Instant) {
        self.tint.snap(1.0);
        let base = SpringSpec::settling_within(motion.tokens().durations.base());
        self.tint.set(0.0, Timing::Spring(base), now);
    }

    /// The shake's horizontal offset at `now`, in px.
    #[track_caller]
    pub fn offset(&self, now: Instant) -> Vec2 {
        Vec2::new(self.amount.get(now), 0.0)
    }

    /// The pulse's scale at `now` (1 at rest).
    #[track_caller]
    pub fn scale(&self, now: Instant) -> f64 {
        1.0 + self.amount.get(now)
    }

    /// The reduced-motion tint at `now`, `0..=1`.
    #[track_caller]
    pub fn tint(&self, now: Instant) -> f32 {
        unit_opacity(self.tint.get(now))
    }

    #[track_caller]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.amount.is_moving(now) || self.tint.is_moving(now)
    }

    pub fn settles_at(&self) -> Option<Instant> {
        match (self.amount.settles_at(), self.tint.settles_at()) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }
}

// ---------------------------------------------------------------------------
// 12. Controls
// ---------------------------------------------------------------------------

/// A control showing its new value: a switch's knob and tint, a checkbox's fill and mark, a
/// slider's thumb, a badge's count. [`Self::MOVE`] is where the moving part is (`fast`, critically
/// damped); [`Self::SHOW`] is what the control shows for its state (`fast`, a fade under reduced
/// motion too). Only a click or a finger moves a control; a key, an assistive action or a value the
/// person did not just cause shows on the next frame.
pub struct ControlChange;

impl ControlChange {
    pub const MOVE: MotionSpec = MotionSpec {
        enter: Pace::Fast,
        exit: Pace::Fast,
        reduced: Reduced::Lands,
        shortcut: Shortcut::Lands,
    };
    pub const SHOW: MotionSpec = MotionSpec {
        enter: Pace::Fast,
        exit: Pace::Fast,
        reduced: Reduced::Fades,
        shortcut: Shortcut::Lands,
    };
}

/// Ask the frame pump for frames until a motion that just started settles.
fn ask_frames_until(settles: Option<Instant>) {
    if let Some(until) = settles {
        super::clock::want_frames_until(until);
    }
}

/// A control's moving part (a switch's knob, a slider's thumb) as a share of its travel, `0..=1`: a
/// click sends it on [`ControlChange::MOVE`], a second click mid-flight retargets from where it is
/// drawn, the hand holds it exactly where it is ([`Self::track`]), and let go it settles carrying
/// the hand's velocity.
#[derive(Clone, Copy, Debug)]
pub struct Thumb {
    at: MotionValue<f64>,
}

impl Default for Thumb {
    fn default() -> Self {
        Self::at(0.0)
    }
}

impl Thumb {
    /// Resting at `position`.
    #[must_use]
    pub fn at(position: f64) -> Self {
        Self {
            at: MotionValue::new(position.clamp(0.0, 1.0)),
        }
    }

    /// Go to `to` as `source` asked: a click or a finger moves it, a key or an action lands it.
    #[track_caller]
    pub fn go(&mut self, to: f64, motion: EffectiveMotion, source: MotionSource, now: Instant) {
        let timing = ControlChange::MOVE.timing(Direction::Enter, motion, source);
        self.at.set(to.clamp(0.0, 1.0), timing, now);
        ask_frames_until(self.at.settles_at());
    }

    /// Held by the hand at `to`: drawn exactly there, from this frame.
    pub fn track(&mut self, to: f64) {
        self.at.snap(to.clamp(0.0, 1.0));
    }

    /// Let go, settling on `to` with the hand's `velocity`.
    #[track_caller]
    pub fn release(&mut self, to: f64, velocity: f64, motion: EffectiveMotion, now: Instant) {
        let timing = ControlChange::MOVE.timing(Direction::Enter, motion, MotionSource::Gesture);
        self.at.release(to.clamp(0.0, 1.0), velocity, timing, now);
        ask_frames_until(self.at.settles_at());
    }

    /// Rest at `to` at once.
    pub fn snap(&mut self, to: f64) {
        self.at.snap(to.clamp(0.0, 1.0));
    }

    /// Where it is drawn at `now`, never past either end of its travel.
    #[track_caller]
    pub fn get(&self, now: Instant) -> f64 {
        self.at.get(now).clamp(0.0, 1.0)
    }

    /// Where it is going (or is, at rest).
    pub fn target(&self) -> f64 {
        self.at.target()
    }

    #[track_caller]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.at.is_moving(now)
    }

    pub fn settles_at(&self) -> Option<Instant> {
        self.at.settles_at()
    }
}

/// What a control shows for its state, `0` off to `1` on (a switch's tint, a checkbox's fill and
/// mark). It comes in over `fast` and goes quicker on [`ControlChange::SHOW`], continuing from what
/// is drawn mid-flight.
#[derive(Clone, Copy, Debug)]
pub struct Mark {
    level: MotionValue<f64>,
}

impl Default for Mark {
    fn default() -> Self {
        Self::at(false)
    }
}

impl Mark {
    /// How much quicker the mark goes than it comes: the cross-fade's outgoing share.
    const OUT_BY: f64 = 0.6;

    /// Resting on or off.
    #[must_use]
    pub fn at(on: bool) -> Self {
        Self {
            level: MotionValue::new(if on { 1.0 } else { 0.0 }),
        }
    }

    /// Show `on` as `source` asked.
    #[track_caller]
    pub fn set(&mut self, on: bool, motion: EffectiveMotion, source: MotionSource, now: Instant) {
        let timing = match ControlChange::SHOW.timing(Direction::of(on), motion, source) {
            Timing::Spring(spring) if !on => Timing::Spring(spring.slowed(Self::OUT_BY)),
            timing => timing,
        };
        self.level.set(if on { 1.0 } else { 0.0 }, timing, now);
        ask_frames_until(self.level.settles_at());
    }

    /// Rest on or off at once.
    pub fn snap(&mut self, on: bool) {
        self.level.snap(if on { 1.0 } else { 0.0 });
    }

    /// Whether it is on (or turning on).
    pub fn is_on(&self) -> bool {
        self.level.target() >= 0.5
    }

    /// How much of the on state shows at `now`, `0..=1`.
    #[track_caller]
    pub fn level(&self, now: Instant) -> f32 {
        unit_opacity(self.level.get(now))
    }

    #[track_caller]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.level.is_moving(now)
    }

    pub fn settles_at(&self) -> Option<Instant> {
        self.level.settles_at()
    }
}
