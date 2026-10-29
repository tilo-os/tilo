//! `StripDeck`: a workspace's strips as full-height screens stacked top to
//! bottom, one filling the deck at a time.
//!
//! Every strip is laid out at the deck's whole size where the deck rests,
//! and moved up or down by where the deck stands: a strip never shares the
//! deck's height with another. Going to another strip slides the deck
//! (`motion::Snap`, the paged-strip settle); three fingers up or down move it
//! with the hand and the release settles it on a strip with the fingers'
//! momentum. Only the strips on screen draw, take input or ask for frames,
//! so a deck at rest costs what its one shown strip costs.
//!
//! Each strip keeps its own horizontal pan ([`GroupRow`] owns it): coming
//! back to a strip shows it as it was left.

use super::finger_path::FingerPath;
use crate::motion::{Direction, MotionValue, Snap};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Instant;
use crate::tokens::{EffectiveMotion, MotionSource};
use crate::widget::{AppModel, GroupRow, ModelPath, TaffyTreeExt, Widget};
use std::collections::HashMap;
use taffy::{
    Dimension, LengthPercentage, LengthPercentageAuto, NodeId, Position, Style, TaffyTree,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::WindowEvent;

/// How far past half a strip a released swipe must have carried (with its
/// throw) to go to the next strip: a quarter of the deck, as the design
/// note says; less springs back.
const COMMIT_SHARE: f64 = 0.25;
/// How far past the first or last strip the fingers can pull the deck, as
/// a share of its height, against the shared rubber band.
const OVERPULL_REACH_FRAC: f64 = 0.28;
/// The deck stands within this of a strip (in strips, about a tenth of a
/// pixel on a tall display) when it has arrived.
const POSITION_TOLERANCE: f64 = 1e-4;

/// A drag resting at the deck's top or bottom edge for this long goes to
/// the strip above or below, as resting on a background tab brings it to the
/// front.
const EDGE_REACH_DWELL: crate::time::Duration = super::group_row::DRAG_SPRING_LOAD;
/// How close to the deck's top or bottom edge (or past it, over the top
/// bar) a drag has to rest to reach the next strip.
const EDGE_REACH_BAND_PX: f64 = crate::tokens::motion::EDGE_AUTOSCROLL_BAND;

/// Fingers moving the deck.
#[derive(Clone, Copy, Debug)]
struct Drag {
    /// Where the deck stood when the fingers landed, in strips.
    origin: f64,
}

/// What a released swipe did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StripDeckRelease {
    pub to: usize,
    /// The strip the fingers took the deck from.
    pub from: usize,
}

/// Where the deck stands on one display. Across several displays each
/// shows a strip of its own: going to another strip, or three fingers
/// moving the deck, on one display leaves the others where they are.
#[derive(Clone, Debug)]
struct Slide {
    /// The display (the compositor's output id); `None` for the one slide
    /// of a deck on a single display.
    display: Option<u32>,
    /// The display's rectangle; the deck's own while unknown.
    area: Option<Rect>,
    /// Where it stands, in strips: `1.0` shows the second strip, `1.5`
    /// half of the second and half of the third.
    position: MotionValue<f64>,
    drag: Option<Drag>,
    finger_path: FingerPath,
    /// Where it stood at the last layout: what is on screen.
    laid: f64,
}

impl Slide {
    fn new(display: Option<u32>, at: f64) -> Self {
        Self {
            display,
            area: None,
            position: MotionValue::new(at).with_tolerance(POSITION_TOLERANCE),
            drag: None,
            finger_path: FingerPath::default(),
            laid: at,
        }
    }

    /// A display that just appeared, showing what `base` shows, at rest.
    fn beside(base: &Self, display: u32, area: Rect) -> Self {
        let mut slide = Self::new(Some(display), base.position.target().round());
        slide.area = Some(area);
        slide
    }

    /// Whether the strip at `index` is on screen here at the last layout.
    fn on_screen(&self, index: usize) -> bool {
        #[allow(clippy::cast_precision_loss, reason = "as in `StripDeck::new`")]
        let at = index as f64;
        (at - self.laid).abs() < 1.0 - POSITION_TOLERANCE
    }

    fn moving(&self, now: Instant, height: f64, count: usize) -> bool {
        self.position.is_moving(now)
            || (self.laid - self.position_at(now, height, count)).abs() > f64::EPSILON
    }

    fn between(&self) -> bool {
        (self.laid - self.laid.round()).abs() > POSITION_TOLERANCE
    }

    /// Where it stands at `now`: under the fingers, read on their path at
    /// that instant, or on its slide.
    fn position_at(&self, now: Instant, height: f64, count: usize) -> f64 {
        match self.drag {
            Some(drag) => self.finger_path.resampled(now).map_or_else(
                || self.position.target(),
                |travel| pulled(drag.origin, travel, height, count),
            ),
            None => self.position.get(now),
        }
    }
}

pub struct StripDeck {
    rows: Vec<Box<GroupRow>>,
    nodes: Vec<NodeId>,
    rect: Rect,
    /// Where the deck stands on each display; never empty. The first is
    /// the one rows without a lane per display follow.
    slides: Vec<Slide>,
    motion: EffectiveMotion,
    /// Where each slide stood at the last draw; `None` before one.
    drawn: Option<Vec<f64>>,
    /// A drag resting at an edge: on which display, toward which strip
    /// (`-1` above, `1` below) and since when.
    edge_reach: Option<(Option<u32>, i32, Instant)>,
    /// A strip the deck went to on its own (a drag reaching it), and on
    /// which display, for the host to focus
    /// ([`Self::take_requested_strip`]).
    requested: Option<(Option<u32>, usize)>,
    /// The pointer, as the deck last saw it: input goes to the strips
    /// shown on the display under it.
    cursor: Option<Point>,
    /// The workspace shows its canvas: every strip's windows stand on one
    /// surface, each where the canvas places it, so every row is laid over
    /// the whole deck at once and none slides.
    canvas: bool,
}

impl StripDeck {
    #[must_use]
    pub fn new(rows: Vec<GroupRow>, shown: usize) -> Self {
        let shown = clamp_index(shown, rows.len());
        #[allow(
            clippy::cast_precision_loss,
            reason = "a workspace has a handful of strips"
        )]
        let at = shown as f64;
        Self {
            rows: rows.into_iter().map(Box::new).collect(),
            nodes: Vec::new(),
            rect: Rect::ZERO,
            slides: vec![Slide::new(None, at)],
            motion: EffectiveMotion::default(),
            drawn: None,
            edge_reach: None,
            requested: None,
            cursor: None,
            canvas: false,
        }
    }

    /// Show every strip's windows on one canvas (`true`), or one strip at a
    /// time.
    pub fn set_canvas(&mut self, canvas: bool) {
        if self.canvas != canvas {
            self.canvas = canvas;
            crate::widget::note_layout_moved();
        }
    }

    /// Whether the deck shows the workspace's canvas.
    #[must_use]
    pub fn shows_canvas(&self) -> bool {
        self.canvas
    }

    /// The deck's rect as last laid out.
    #[must_use]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn row(&self, index: usize) -> Option<&GroupRow> {
        self.rows.get(index).map(AsRef::as_ref)
    }

    pub fn row_mut(&mut self, index: usize) -> Option<&mut GroupRow> {
        self.rows.get_mut(index).map(AsMut::as_mut)
    }

    /// The displays the deck stands on, each with its rectangle: one
    /// slide each, so each shows a strip of its own. Fewer than two make
    /// it one deck again. A display keeps its slide across calls; a new
    /// one starts on the strip the first shows.
    pub fn set_displays(&mut self, displays: &[(u32, Rect)]) {
        if displays.len() < 2 {
            let keep = self
                .slides
                .iter()
                .position(|slide| slide.display.is_none())
                .unwrap_or(0);
            let mut slide = self.slides.swap_remove(keep);
            slide.display = None;
            slide.area = None;
            self.slides = vec![slide];
            return;
        }
        let mut old: Vec<Option<Slide>> = self.slides.drain(..).map(Some).collect();
        let base = old
            .first()
            .and_then(Clone::clone)
            .unwrap_or_else(|| Slide::new(None, 0.0));
        self.slides = displays
            .iter()
            .map(|&(id, area)| {
                let kept = old
                    .iter_mut()
                    .find(|slide| slide.as_ref().is_some_and(|s| s.display == Some(id)))
                    .and_then(Option::take);
                kept.map_or_else(
                    || Slide::beside(&base, id, area),
                    |mut slide| {
                        slide.area = Some(area);
                        slide
                    },
                )
            })
            .collect();
    }

    /// The displays the deck stands on, each moving between strips on its
    /// own: `[None]` for a deck on one display.
    pub fn displays(&self) -> Vec<Option<u32>> {
        self.slides.iter().map(|slide| slide.display).collect()
    }

    fn slide_index(&self, display: Option<u32>) -> usize {
        self.slides
            .iter()
            .position(|slide| slide.display == display)
            .unwrap_or(0)
    }

    fn slide(&self, display: Option<u32>) -> &Slide {
        &self.slides[self.slide_index(display)]
    }

    /// The slide of `display`, made for it when the deck had none yet (the
    /// displays are told before the first layout says where they are).
    fn slide_mut(&mut self, display: Option<u32>) -> &mut Slide {
        let index = match (
            display,
            self.slides.iter().position(|s| s.display == display),
        ) {
            (_, Some(index)) => index,
            (None, None) => 0,
            (Some(id), None) => {
                let base = &self.slides[0];
                let mut slide = Slide::beside(base, id, self.rect);
                slide.area = None;
                self.slides.push(slide);
                self.slides.len() - 1
            }
        };
        &mut self.slides[index]
    }

    /// How tall a strip is on `slide`'s display.
    fn height_of(&self, slide: &Slide) -> f64 {
        slide
            .area
            .map(|area| area.intersect(self.rect).height())
            .filter(|h| *h > 0.0)
            .unwrap_or_else(|| self.rect.height())
            .max(1.0)
    }

    /// The strip `display` shows, or is on its way to.
    pub fn shown(&self, display: Option<u32>) -> usize {
        index_of(self.slide(display).position.target(), self.rows.len())
    }

    /// Where the deck stands on `display` now, in strips.
    pub fn position(&self, display: Option<u32>) -> f64 {
        let slide = self.slide(display);
        slide.position_at(
            crate::motion::frame_now(),
            self.height_of(slide),
            self.rows.len(),
        )
    }

    /// Go to the strip at `index` on `display`, sliding there as `source`
    /// asks: a hand gets the paged-strip settle, a shortcut the same within
    /// `fast`. Fingers on it let go. Going where it already goes changes
    /// nothing, so a release's momentum is never restarted. The other
    /// displays stay where they are.
    pub fn show(&mut self, display: Option<u32>, index: usize, source: MotionSource) {
        let index = clamp_index(index, self.rows.len());
        let now = crate::motion::frame_now();
        let count = self.rows.len();
        let motion = self.motion;
        let height = self.height_of(self.slide(display));
        let slide = self.slide_mut(display);
        if slide.drag.is_some() {
            let at = slide.position_at(now, height, count);
            slide.drag = None;
            slide.finger_path.clear();
            slide.position.snap(at);
        }
        #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
        let target = index as f64;
        if (slide.position.target() - target).abs() <= f64::EPSILON {
            return;
        }
        let travel = (slide.position.get(now) - target).abs();
        let timing = Snap::SPEC.timing_over(Direction::Enter, motion, source, travel);
        slide.position.set(target, timing, now);
    }

    /// Show the strip at `index` on `display` at once.
    pub fn jump_to(&mut self, display: Option<u32>, index: usize) {
        let index = clamp_index(index, self.rows.len());
        let slide = self.slide_mut(display);
        slide.drag = None;
        slide.finger_path.clear();
        #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
        slide.position.snap(index as f64);
    }

    /// Whether fingers move the deck on any display.
    pub fn dragging(&self) -> bool {
        self.slides.iter().any(|slide| slide.drag.is_some())
    }

    /// Fingers landed at `at` to move `display`'s strips: they stop where
    /// they are, mid-slide included, and follow the fingers from there.
    pub fn begin_drag(&mut self, display: Option<u32>, at: Instant) {
        let _ = at;
        let now = crate::motion::frame_now();
        let count = self.rows.len();
        let height = self.height_of(self.slide(display));
        let slide = self.slide_mut(display);
        let origin = slide.position_at(now, height, count);
        slide.position.snap(origin);
        slide.finger_path.clear();
        slide.drag = Some(Drag { origin });
    }

    /// The fingers' total `travel` (px; positive toward the strips below)
    /// reported `at`. `display` shows it at once; each frame then shows
    /// where the fingers were at that frame. `false` when nothing moved.
    pub fn drag_to(&mut self, display: Option<u32>, travel: f64, at: Instant) -> bool {
        let count = self.rows.len();
        let height = self.height_of(self.slide(display));
        let slide = self.slide_mut(display);
        let Some(drag) = slide.drag else {
            return false;
        };
        slide.finger_path.push(at, travel);
        let next = pulled(drag.origin, travel, height, count);
        let before = slide.position.target();
        slide.position.snap(next);
        (next - before).abs() > f64::EPSILON
    }

    /// The fingers on `display` lifted at `now` (or the gesture was
    /// `cancelled`): it settles on the strip the swipe carried it to, with
    /// the fingers' momentum. A swipe that carried it past a quarter of the
    /// deck, or threw it there, goes on to the next strip; less springs
    /// back. A cancelled one goes back to where it began. `None` without a
    /// drag.
    pub fn end_drag(
        &mut self,
        display: Option<u32>,
        now: Instant,
        cancelled: bool,
    ) -> Option<StripDeckRelease> {
        let count = self.rows.len();
        let motion = self.motion;
        let height = self.height_of(self.slide(display));
        let slide = self.slide_mut(display);
        let drag = slide.drag.take()?;
        let at = slide.finger_path.resampled(now).map_or_else(
            || slide.position.target(),
            |travel| pulled(drag.origin, travel, height, count),
        );
        let velocity = if cancelled {
            0.0
        } else {
            slide.finger_path.release_velocity(now) / height
        };
        slide.finger_path.clear();
        slide.position.snap(at);
        let from = index_of(drag.origin, count);
        let to = if cancelled {
            from
        } else {
            settle_index(from, (at, Snap::rest_for(at, velocity, motion)), count)
        };
        #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
        let target = to as f64;
        let timing = Snap::SPEC.timing(Direction::Enter, motion, MotionSource::Gesture);
        slide.position.release(target, velocity, timing, now);
        Some(StripDeckRelease { to, from })
    }

    /// The strip the deck went to on its own since the last call, and on
    /// which display: a drag carried to its edge reached it. The host
    /// focuses it, as it would a strip the person chose.
    pub fn take_requested_strip(&mut self) -> Option<(Option<u32>, usize)> {
        self.requested.take()
    }

    /// The display holding `p`, as the deck's slides know them.
    fn display_at(&self, p: Point) -> Option<u32> {
        self.slides
            .iter()
            .find(|slide| slide.area.is_some_and(|area| area.contains(p)))
            .and_then(|slide| slide.display)
    }

    /// Advance a drag resting at the deck's edge to `now`: resting there
    /// for [`EDGE_REACH_DWELL`] slides the display it rests on to the strip
    /// beyond it, so a tab can be carried onto another strip. Resting on
    /// goes on another strip after the same wait. Returns whether the deck
    /// went anywhere.
    fn advance_edge_reach(&mut self, now: Instant) -> bool {
        let at = self.rows.iter().find_map(|row| row.strip_reaching_drag());
        let display = at.and_then(|p| self.display_at(p));
        let slide = self.slide(display);
        let area = slide
            .area
            .map_or(self.rect, |area| area.intersect(self.rect));
        let shown = index_of(slide.position.target(), self.rows.len());
        let toward = at.map_or(0, |p| {
            let above = p.y <= area.y0 + EDGE_REACH_BAND_PX && shown > 0;
            let below =
                p.y >= area.y1 - EDGE_REACH_BAND_PX && shown.saturating_add(1) < self.rows.len();
            if above { -1 } else { i32::from(below) }
        });
        if toward == 0 || self.rect.height() <= 0.0 {
            self.edge_reach = None;
            return false;
        }
        match self.edge_reach {
            Some((on, dir, since)) if dir == toward && on == display => {
                if now.saturating_duration_since(since) < EDGE_REACH_DWELL {
                    return false;
                }
            }
            _ => {
                self.edge_reach = Some((display, toward, now));
                return false;
            }
        }
        let next = if toward < 0 {
            shown.saturating_sub(1)
        } else {
            shown.saturating_add(1)
        };
        self.edge_reach = Some((display, toward, now));
        self.show(display, next, MotionSource::Pointer);
        self.requested = Some((display, next));
        true
    }

    /// Whether any display shows something other than the last layout put
    /// on screen: sliding, or under fingers that moved. Fingers resting on
    /// the pad ask for nothing.
    pub fn moving(&self) -> bool {
        let now = crate::motion::frame_now();
        let count = self.rows.len();
        self.slides
            .iter()
            .any(|slide| slide.moving(now, self.height_of(slide), count))
    }

    /// Whether one strip fills the deck on every display and nothing moves
    /// it: no slide, no fingers on it, laid out on a whole strip.
    pub fn at_rest(&self) -> bool {
        !self.dragging() && !self.moving() && !self.slides.iter().any(Slide::between)
    }

    /// Whether the strip at `index` is on screen on any display at the last
    /// layout.
    pub fn on_screen(&self, index: usize) -> bool {
        if self.canvas {
            return index < self.rows.len();
        }
        self.slides.iter().any(|slide| slide.on_screen(index))
    }

    /// How far down (up, when negative) the strip at `index` is moved from
    /// where it rests on `display`, at the last layout.
    pub fn shift_of(&self, display: Option<u32>, index: usize) -> f64 {
        if self.canvas {
            return 0.0;
        }
        let slide = self.slide(display);
        #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
        let at = index as f64;
        (at - slide.laid) * self.height_of(slide)
    }

    fn visible_rows(&self) -> impl Iterator<Item = &GroupRow> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(i, _)| self.on_screen(*i))
            .map(|(_, row)| row.as_ref())
    }

    /// Where each slide stands at the last layout.
    fn laid(&self) -> Vec<f64> {
        self.slides.iter().map(|slide| slide.laid).collect()
    }
}

/// Where fingers that travelled `travel` px from `origin` put a deck of
/// `count` strips `height` tall: one to one between the first and last
/// strip, against the rubber band past either.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the shared rubber-band token takes f32 screen distances; a deck's height is far \
              inside f32 and the result is widened straight back"
)]
fn pulled(origin: f64, travel: f64, height: f64, count: usize) -> f64 {
    let height = height.max(1.0);
    #[allow(clippy::cast_precision_loss, reason = "as in `StripDeck::new`")]
    let last = count.saturating_sub(1) as f64;
    let requested = origin + travel / height;
    let reach = (height * OVERPULL_REACH_FRAC) as f32;
    if requested < 0.0 {
        -f64::from(crate::tokens::rubber_band(
            (-requested * height) as f32,
            reach,
        )) / height
    } else if requested > last {
        last + f64::from(crate::tokens::rubber_band(
            ((requested - last) * height) as f32,
            reach,
        )) / height
    } else {
        requested
    }
}

/// The strip index nearest `position`, within `count` strips.
fn index_of(position: f64, count: usize) -> usize {
    if count == 0 || position <= 0.0 {
        return 0;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a non-negative position within a handful of strips"
    )]
    let index = position.round() as usize;
    index.min(count - 1)
}

fn clamp_index(index: usize, count: usize) -> usize {
    index.min(count.saturating_sub(1))
}

/// The strip a swipe from strip `from`, released with the deck `at` a
/// position whose throw would come to rest at `rest`, settles on: the next
/// strip in its direction once it carried past [`COMMIT_SHARE`], as many
/// strips as it carried past that, but a throw goes no further than the
/// strip the fingers had reached (one at least): a flick is one strip, a
/// long drag across two is two. Never past either end.
fn settle_index(from: usize, (at, rest): (f64, f64), count: usize) -> usize {
    #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
    let origin = from as f64;
    let travel = rest - origin;
    if travel.abs() < COMMIT_SHARE {
        return from;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a strip count's worth of travel"
    )]
    let (steps, reached) = (
        ((travel.abs() - COMMIT_SHARE).floor() as usize).saturating_add(1),
        ((at - origin).abs().ceil() as usize).max(1),
    );
    let steps = steps.min(reached);
    if travel < 0.0 {
        from.saturating_sub(steps)
    } else {
        clamp_index(from.saturating_add(steps), count)
    }
}

impl Widget for StripDeck {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    /// Every strip's apps run, shown or not (a terminal drains its output
    /// whichever strip it is on).
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        // A slide moves the strips, which their layout places.
        if self.moving() || self.edge_reach.is_some() {
            crate::widget::note_layout_moved();
        }
        let mut changed = false;
        for row in &mut self.rows {
            changed |= row.update(state, path);
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.motion = theme.motion();
        self.nodes = self
            .rows
            .iter_mut()
            .map(|row| row.build_layout(taffy, theme, node_map))
            .collect();
        // Every strip is the deck's whole size, where the deck rests.
        for &node in &self.nodes {
            let mut style = taffy.style(node).cloned().unwrap_or_default();
            style.position = Position::Absolute;
            style.inset = taffy::Rect {
                left: LengthPercentageAuto::length(0.0),
                right: LengthPercentageAuto::length(0.0),
                top: LengthPercentageAuto::length(0.0),
                bottom: LengthPercentageAuto::length(0.0),
            };
            style.size = taffy::Size::auto();
            style.flex_grow = 0.0;
            let _ = taffy.set_style(node, style);
        }
        // The deck takes the height its column leaves; its strips, all
        // absolutely placed, ask for none.
        let style = Style {
            position: Position::Relative,
            size: taffy::Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            min_size: taffy::Size {
                width: Dimension::auto(),
                height: Dimension::length(0.0),
            },
            flex_grow: 1.0,
            flex_basis: Dimension::length(0.0),
            gap: taffy::Size {
                width: LengthPercentage::length(0.0),
                height: LengthPercentage::length(0.0),
            },
            ..Default::default()
        };
        let nodes = self.nodes.clone();
        taffy.widget_node(style, &nodes, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        let now = crate::motion::frame_now();
        self.advance_edge_reach(now);
        let count = self.rows.len();
        let heights: Vec<f64> = self
            .slides
            .iter()
            .map(|slide| self.height_of(slide))
            .collect();
        for (slide, &height) in self.slides.iter_mut().zip(&heights) {
            slide.laid = slide.position_at(now, height, count);
        }
        // A slide moves on in the next frame's layout, whether or not the
        // host runs this deck's update first.
        if self.moving() || self.edge_reach.is_some() {
            crate::widget::note_layout_moved();
        }
        let top_left = Point::new(self.rect.x0, self.rect.y0);
        let base = self.slides[0].laid;
        let base_height = heights.first().copied().unwrap_or(1.0);
        if self.canvas {
            // Every row over the whole deck: the canvas places the windows.
            for (row, &child) in self.rows.iter_mut().zip(&self.nodes) {
                row.set_deck_shift(0.0, &[]);
                row.apply_layout(taffy, child, top_left);
            }
            return;
        }
        for (i, (row, &child)) in self.rows.iter_mut().zip(&self.nodes).enumerate() {
            #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
            let at = i as f64;
            // Each display's lane of the row moves with that display's
            // slide; the row itself, and a row with no lanes, with the
            // first.
            let lanes: Vec<(u32, f64)> = self
                .slides
                .iter()
                .zip(&heights)
                .filter_map(|(slide, &height)| {
                    slide.display.map(|d| (d, (at - slide.laid) * height))
                })
                .collect();
            let shift = (at - base) * base_height;
            row.set_deck_shift(shift, &lanes);
            row.apply_layout(taffy, child, top_left + Vec2::new(0.0, shift));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.motion = theme.motion();
        self.drawn = Some(self.laid());
        // Between two strips, each is cut at the deck's edge, so the one
        // leaving never draws over the top bar or past the display.
        let between = !self.canvas && self.slides.iter().any(Slide::between);
        if between {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &self.rect,
            );
        }
        for i in 0..self.rows.len() {
            if !self.on_screen(i) {
                continue;
            }
            if let Some(row) = self.rows.get_mut(i) {
                crate::widget::draw_child(
                    row.as_mut(),
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    presence,
                );
            }
        }
        if between {
            scene.pop_layer();
        }
    }

    /// Input goes to the strips on screen on the display under the
    /// pointer; a strip holding the pointer (a drag it began) keeps it
    /// wherever it is.
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            ));
        }
        let owner = self
            .rows
            .iter()
            .position(|row| row.wants_pointer_capture())
            .or_else(|| {
                self.rows
                    .iter()
                    .position(|row| row.modal_capture().is_some())
            });
        if let Some(i) = owner {
            if let (Some(row), Some(&node)) = (self.rows.get_mut(i), self.nodes.get(i)) {
                row.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            }
            return;
        }
        let under = self
            .cursor
            .and_then(|p| self.display_at(p))
            .map(|display| self.slide_index(Some(display)));
        let canvas = self.canvas;
        let shown: Vec<bool> = (0..self.rows.len())
            .map(|i| match under {
                _ if canvas => true,
                Some(slide) => self.slides.get(slide).is_some_and(|s| s.on_screen(i)),
                None => self.on_screen(i),
            })
            .collect();
        for (i, (row, &node)) in self.rows.iter_mut().zip(&self.nodes).enumerate() {
            if !shown.get(i).copied().unwrap_or(false) {
                continue;
            }
            row.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                break;
            }
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.rows.iter().any(|row| row.wants_pointer_capture())
    }

    fn needs_animation_frame(&self) -> bool {
        self.moving()
            || self.edge_reach.is_some()
            || self
                .rows
                .iter()
                .any(|row| row.wants_pointer_capture() && row.needs_animation_frame())
            || self.visible_rows().any(Widget::needs_animation_frame)
    }

    fn needs_redraw(&self) -> bool {
        self.drawn.as_ref() != Some(&self.laid())
            || self.moving()
            || self.visible_rows().any(Widget::needs_redraw)
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.drawn.as_ref() != Some(&self.laid()) || self.moving() {
            regions.push(self.paint_bounds());
            return;
        }
        for row in self.visible_rows() {
            row.collect_damage(regions);
        }
    }

    /// A slide moves the strips without changing any size: their boxes
    /// are placed again, never measured again.
    fn is_layout_dirty(&self) -> bool {
        self.moving() || self.rows.iter().any(|row| row.is_layout_dirty())
    }

    fn mark_drawn(&mut self) {
        for i in 0..self.rows.len() {
            if !self.on_screen(i) {
                continue;
            }
            if let Some(row) = self.rows.get_mut(i) {
                row.mark_drawn();
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.rows
            .iter()
            .map(|row| row.as_ref() as &dyn Widget)
            .collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for row in &self.rows {
            visitor(row.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.rows
            .iter_mut()
            .map(|row| row.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        let mut change = crate::widget::StructureChange::None;
        for row in &mut self.rows {
            change = change.join(row.take_structure_change());
        }
        change
    }

    // Every row must clear its flag; a short-circuiting `any` would not.
    #[allow(
        clippy::unnecessary_fold,
        reason = "every row must clear its structure-dirty flag"
    )]
    fn take_structure_dirty(&mut self) -> bool {
        self.rows
            .iter_mut()
            .map(|row| row.take_structure_dirty())
            .fold(false, |acc, dirty| acc || dirty)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn paint_bounds(&self) -> Rect {
        self.visible_rows()
            .map(Widget::paint_bounds)
            .fold(self.rect, |bounds, row| bounds.union(row))
    }

    fn name(&self) -> &'static str {
        "StripDeck"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deck_of(count: usize) -> StripDeck {
        let mut deck = StripDeck::new((0..count).map(|_| GroupRow::new(Vec::new())).collect(), 0);
        deck.rect = Rect::new(0.0, 40.0, 1000.0, 800.0);
        deck
    }

    #[test]
    fn a_drag_resting_at_the_bottom_edge_slides_to_the_strip_below() {
        let _settled = crate::motion::testing::settled();
        let mut deck = deck_of(3);
        let t0 = crate::time::now();
        if let Some(row) = deck.row_mut(0) {
            row.set_external_content_drag_hover(Some(Point::new(500.0, 400.0)));
        }
        assert!(!deck.advance_edge_reach(t0));
        if let Some(row) = deck.row_mut(0) {
            row.set_external_content_drag_hover(Some(Point::new(500.0, 798.0)));
        }
        assert!(!deck.advance_edge_reach(t0));
        assert!(!deck.advance_edge_reach(t0 + EDGE_REACH_DWELL / 2));
        assert!(deck.advance_edge_reach(t0 + EDGE_REACH_DWELL));
        assert_eq!(deck.shown(None), 1, "the deck goes to the strip below");
        assert_eq!(
            deck.take_requested_strip(),
            Some((None, 1)),
            "and asks for it to be focused"
        );
        assert_eq!(deck.take_requested_strip(), None);
        // Resting on goes on another strip after the same wait.
        assert!(!deck.advance_edge_reach(t0 + EDGE_REACH_DWELL + EDGE_REACH_DWELL / 2));
        assert!(deck.advance_edge_reach(t0 + EDGE_REACH_DWELL * 2));
        assert_eq!(deck.shown(None), 2);
        // Never past the last strip.
        assert!(!deck.advance_edge_reach(t0 + EDGE_REACH_DWELL * 4));
        assert_eq!(deck.shown(None), 2);
        if let Some(row) = deck.row_mut(0) {
            row.set_external_content_drag_hover(Some(Point::new(500.0, 10.0)));
        }
        let t1 = t0 + EDGE_REACH_DWELL * 5;
        assert!(!deck.advance_edge_reach(t1));
        assert!(deck.advance_edge_reach(t1 + EDGE_REACH_DWELL));
        assert_eq!(deck.shown(None), 1);
        if let Some(row) = deck.row_mut(0) {
            row.set_external_content_drag_hover(None);
        }
        assert!(!deck.advance_edge_reach(t1 + EDGE_REACH_DWELL * 3));
        assert_eq!(deck.shown(None), 1);
    }

    #[test]
    fn the_deck_follows_the_fingers_and_settles_with_their_momentum() {
        let mut deck = deck_of(3);
        let height = deck.rect.height();
        let t0 = crate::time::now();
        let ms = |n: u64| crate::time::Duration::from_millis(n);
        deck.begin_drag(None, t0);
        // A slow drag a fifth of a screen down follows the fingers 1:1 ...
        for i in 1..=10_u32 {
            let travel = f64::from(i) * height * 0.02;
            deck.drag_to(None, travel, t0 + ms(u64::from(i) * 50));
        }
        assert!((deck.slides[0].position.target() - 0.2).abs() < 1e-6);
        // ... and, released after a pause, springs back.
        let release = deck.end_drag(None, t0 + ms(800), false).expect("a drag");
        assert_eq!(release, StripDeckRelease { to: 0, from: 0 });
        deck.jump_to(None, 0);
        deck.begin_drag(None, t0);
        for i in 1..=4_u32 {
            deck.drag_to(None, f64::from(i) * 25.0, t0 + ms(u64::from(i) * 8));
        }
        let release = deck.end_drag(None, t0 + ms(33), false).expect("a drag");
        assert_eq!(release, StripDeckRelease { to: 1, from: 0 });
        assert!(
            deck.slides[0].position.velocity(t0 + ms(33)) > 0.0,
            "the settle carries the fingers' momentum"
        );
        deck.jump_to(None, 2);
        deck.begin_drag(None, t0);
        deck.drag_to(None, height, t0 + ms(16));
        assert!(deck.slides[0].position.target() < 2.0 + OVERPULL_REACH_FRAC);
        let release = deck.end_drag(None, t0 + ms(400), false).expect("a drag");
        assert_eq!(release.to, 2);
        deck.jump_to(None, 1);
        deck.begin_drag(None, t0);
        deck.drag_to(None, height * 0.9, t0 + ms(16));
        let release = deck.end_drag(None, t0 + ms(32), true).expect("a drag");
        assert_eq!(release, StripDeckRelease { to: 1, from: 1 });
    }

    #[test]
    fn each_display_moves_between_strips_on_its_own() {
        let mut deck = deck_of(3);
        let (left, right) = (
            Rect::new(0.0, 0.0, 500.0, 800.0),
            Rect::new(500.0, 0.0, 1000.0, 800.0),
        );
        deck.set_displays(&[(1, left), (2, right)]);
        assert_eq!(deck.displays(), vec![Some(1), Some(2)]);
        let t0 = crate::time::now();
        let ms = |n: u64| crate::time::Duration::from_millis(n);
        // Fingers on the right display carry it to the strip below ...
        deck.begin_drag(Some(2), t0);
        for i in 1..=4_u32 {
            deck.drag_to(Some(2), f64::from(i) * 60.0, t0 + ms(u64::from(i) * 8));
        }
        assert!(
            deck.slide(Some(2)).position.target() > 0.2,
            "the right display follows"
        );
        assert!(
            deck.slide(Some(1)).position.target().abs() < f64::EPSILON,
            "the left display stays while the fingers move the right"
        );
        let release = deck.end_drag(Some(2), t0 + ms(40), false).expect("a drag");
        assert_eq!(release, StripDeckRelease { to: 1, from: 0 });
        assert_eq!(deck.shown(Some(2)), 1);
        assert_eq!(
            deck.shown(Some(1)),
            0,
            "the left display still shows the first"
        );
        // ... and going to another strip on the left leaves the right.
        deck.show(Some(1), 2, MotionSource::Keyboard);
        assert_eq!(deck.shown(Some(1)), 2);
        assert_eq!(deck.shown(Some(2)), 1);
        // A display that appears starts where the first one is; one display
        // left makes it one deck again.
        deck.set_displays(&[
            (1, left),
            (2, right),
            (3, Rect::new(1000.0, 0.0, 1500.0, 800.0)),
        ]);
        assert_eq!(deck.shown(Some(3)), 2);
        deck.set_displays(&[(2, right)]);
        assert_eq!(deck.displays(), vec![None]);
    }

    #[test]
    fn a_swipe_settles_on_the_strip_it_carried_the_deck_to() {
        assert_eq!(settle_index(1, (1.1, 1.2), 3), 1);
        assert_eq!(settle_index(1, (0.9, 0.8), 3), 1);
        assert_eq!(settle_index(1, (1.3, 1.3), 3), 2);
        assert_eq!(settle_index(1, (0.9, 0.6), 3), 0);
        assert_eq!(settle_index(0, (0.1, 2.4), 4), 1);
        // ... and fingers that carried the deck across two go on by two.
        assert_eq!(settle_index(0, (1.6, 2.4), 4), 2);
        // Never past either end.
        assert_eq!(settle_index(2, (2.5, 3.9), 3), 2);
        assert_eq!(settle_index(0, (-0.2, -1.5), 3), 0);
    }
}
