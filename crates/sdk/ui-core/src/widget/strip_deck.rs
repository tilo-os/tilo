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

pub struct StripDeck {
    rows: Vec<Box<GroupRow>>,
    nodes: Vec<NodeId>,
    rect: Rect,
    /// Where the deck stands, in strips: `1.0` shows the second strip,
    /// `1.5` half of the second and half of the third.
    position: MotionValue<f64>,
    drag: Option<Drag>,
    finger_path: FingerPath,
    motion: EffectiveMotion,
    /// Where the deck stood at the last layout: what is on screen.
    laid: f64,
    /// Where it stood at the last draw; `None` before one.
    drawn: Option<f64>,
    /// A drag resting at an edge: toward which strip (`-1` above, `1`
    /// below) and since when.
    edge_reach: Option<(i32, Instant)>,
    /// A strip the deck went to on its own (a drag reaching it), for the
    /// host to focus ([`Self::take_requested_strip`]).
    requested: Option<usize>,
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
            position: MotionValue::new(at).with_tolerance(POSITION_TOLERANCE),
            drag: None,
            finger_path: FingerPath::default(),
            motion: EffectiveMotion::default(),
            laid: at,
            drawn: None,
            edge_reach: None,
            requested: None,
        }
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

    /// The strip the deck shows, or is on its way to.
    pub fn shown(&self) -> usize {
        index_of(self.position.target(), self.rows.len())
    }

    /// Where the deck stands now, in strips.
    pub fn position(&self) -> f64 {
        self.position_at(crate::motion::frame_now())
    }

    /// Go to the strip at `index`, sliding there as `source` asks: a hand
    /// gets the paged-strip settle, a shortcut the same within `fast`.
    /// Fingers on the deck let go of it. Going where the deck already goes
    /// changes nothing, so a release's momentum is never restarted.
    pub fn show(&mut self, index: usize, source: MotionSource) {
        let index = clamp_index(index, self.rows.len());
        let now = crate::motion::frame_now();
        if self.drag.take().is_some() {
            let at = self.position_at(now);
            self.finger_path.clear();
            self.position.snap(at);
        }
        #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
        let target = index as f64;
        if (self.position.target() - target).abs() <= f64::EPSILON {
            return;
        }
        let travel = (self.position.get(now) - target).abs();
        let timing = Snap::SPEC.timing_over(Direction::Enter, self.motion, source, travel);
        self.position.set(target, timing, now);
    }

    /// Show the strip at `index` at once.
    pub fn jump_to(&mut self, index: usize) {
        let index = clamp_index(index, self.rows.len());
        self.drag = None;
        self.finger_path.clear();
        #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
        self.position.snap(index as f64);
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Fingers landed at `at`: the deck stops where it is, mid-slide
    /// included, and follows them from there.
    pub fn begin_drag(&mut self, at: Instant) {
        let _ = at;
        let origin = self.position_at(crate::motion::frame_now());
        self.position.snap(origin);
        self.finger_path.clear();
        self.drag = Some(Drag { origin });
    }

    /// The fingers' total `travel` (px; positive toward the strips below)
    /// reported `at`. The deck shows it at once; each frame then shows
    /// where the fingers were at that frame. `false` when nothing moved.
    pub fn drag_to(&mut self, travel: f64, at: Instant) -> bool {
        let Some(drag) = self.drag else {
            return false;
        };
        self.finger_path.push(at, travel);
        let next = self.pulled(drag.origin, travel);
        let before = self.position.target();
        self.position.snap(next);
        (next - before).abs() > f64::EPSILON
    }

    /// The fingers lifted at `now` (or the gesture was `cancelled`): the
    /// deck settles on the strip the swipe carried it to, with the fingers'
    /// momentum. A swipe that carried it past a quarter of the deck, or
    /// threw it there, goes on to the next strip; less springs back. A
    /// cancelled one goes back to where it began. `None` without a drag.
    pub fn end_drag(&mut self, now: Instant, cancelled: bool) -> Option<StripDeckRelease> {
        let drag = self.drag.take()?;
        let height = self.rect.height().max(1.0);
        let at = self.finger_path.resampled(now).map_or_else(
            || self.position.target(),
            |travel| self.pulled(drag.origin, travel),
        );
        let velocity = if cancelled {
            0.0
        } else {
            self.finger_path.release_velocity(now) / height
        };
        self.finger_path.clear();
        self.position.snap(at);
        let from = index_of(drag.origin, self.rows.len());
        let to = if cancelled {
            from
        } else {
            settle_index(
                from,
                (at, Snap::rest_for(at, velocity, self.motion)),
                self.rows.len(),
            )
        };
        #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
        let target = to as f64;
        let timing = Snap::SPEC.timing(Direction::Enter, self.motion, MotionSource::Gesture);
        self.position.release(target, velocity, timing, now);
        Some(StripDeckRelease { to, from })
    }

    /// The strip the deck went to on its own since the last call: a drag
    /// carried to its edge reached it. The host focuses it, as it would a
    /// strip the person chose.
    pub fn take_requested_strip(&mut self) -> Option<usize> {
        self.requested.take()
    }

    /// Advance a drag resting at the deck's edge to `now`: resting there
    /// for [`EDGE_REACH_DWELL`] slides the deck to the strip beyond it, so a
    /// tab can be carried onto another strip. Resting on goes on another
    /// strip after the same wait. Returns whether the deck went anywhere.
    fn advance_edge_reach(&mut self, now: Instant) -> bool {
        let shown = self.shown();
        let toward = self
            .rows
            .iter()
            .find_map(|row| row.strip_reaching_drag())
            .map_or(0, |p| {
                let above = p.y <= self.rect.y0 + EDGE_REACH_BAND_PX && shown > 0;
                let below = p.y >= self.rect.y1 - EDGE_REACH_BAND_PX
                    && shown.saturating_add(1) < self.rows.len();
                if above { -1 } else { i32::from(below) }
            });
        if toward == 0 || self.rect.height() <= 0.0 {
            self.edge_reach = None;
            return false;
        }
        match self.edge_reach {
            Some((dir, since)) if dir == toward => {
                if now.saturating_duration_since(since) < EDGE_REACH_DWELL {
                    return false;
                }
            }
            _ => {
                self.edge_reach = Some((toward, now));
                return false;
            }
        }
        let next = if toward < 0 {
            shown.saturating_sub(1)
        } else {
            shown.saturating_add(1)
        };
        self.edge_reach = Some((toward, now));
        self.show(next, MotionSource::Pointer);
        self.requested = Some(next);
        true
    }

    /// Whether the deck shows something other than the last layout put on
    /// screen: sliding, or under fingers that moved. Fingers resting on the
    /// pad ask for nothing.
    pub fn moving(&self) -> bool {
        let now = crate::motion::frame_now();
        self.position.is_moving(now) || (self.laid - self.position_at(now)).abs() > f64::EPSILON
    }

    /// Whether one strip fills the deck and nothing moves it: no slide, no
    /// fingers on it, laid out on a whole strip.
    pub fn at_rest(&self) -> bool {
        self.drag.is_none()
            && !self.moving()
            && (self.laid - self.laid.round()).abs() <= POSITION_TOLERANCE
    }

    /// Whether the strip at `index` is on screen at the last layout.
    pub fn on_screen(&self, index: usize) -> bool {
        #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
        let at = index as f64;
        (at - self.laid).abs() < 1.0 - POSITION_TOLERANCE
    }

    /// How far down (up, when negative) the strip at `index` is moved from
    /// where it rests, at the last layout.
    pub fn shift_of(&self, index: usize) -> f64 {
        #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
        let at = index as f64;
        (at - self.laid) * self.rect.height()
    }

    /// Where the deck stands at `now`: under the fingers, read on their
    /// path at that instant, or on its slide.
    fn position_at(&self, now: Instant) -> f64 {
        match self.drag {
            Some(drag) => self.finger_path.resampled(now).map_or_else(
                || self.position.target(),
                |travel| self.pulled(drag.origin, travel),
            ),
            None => self.position.get(now),
        }
    }

    /// Where fingers that travelled `travel` px from `origin` put the deck:
    /// one to one between the first and last strip, against the rubber band
    /// past either.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the shared rubber-band token takes f32 screen distances; a deck's height is far \
                  inside f32 and the result is widened straight back"
    )]
    fn pulled(&self, origin: f64, travel: f64) -> f64 {
        let height = self.rect.height().max(1.0);
        #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
        let last = self.rows.len().saturating_sub(1) as f64;
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

    fn visible_rows(&self) -> impl Iterator<Item = &GroupRow> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(i, _)| self.on_screen(*i))
            .map(|(_, row)| row.as_ref())
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
        self.laid = self.position_at(now);
        let height = self.rect.height();
        let top_left = Point::new(self.rect.x0, self.rect.y0);
        for (i, (row, &child)) in self.rows.iter_mut().zip(&self.nodes).enumerate() {
            #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
            let shift = (i as f64 - self.laid) * height;
            row.set_deck_shift(shift);
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
        self.drawn = Some(self.laid);
        // Between two strips, each is cut at the deck's edge, so the one
        // leaving never draws over the top bar or past the display.
        let between = self.laid.fract().abs() > POSITION_TOLERANCE;
        if between {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &self.rect,
            );
        }
        let laid = self.laid;
        for (i, row) in self.rows.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
            let on_screen = (i as f64 - laid).abs() < 1.0 - POSITION_TOLERANCE;
            if on_screen {
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

    /// Input goes to the strips on screen; a strip holding the pointer (a
    /// drag it began) keeps it wherever it is.
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let laid = self.laid;
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
        for (i, (row, &node)) in self.rows.iter_mut().zip(&self.nodes).enumerate() {
            #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
            let on_screen = (i as f64 - laid).abs() < 1.0 - POSITION_TOLERANCE;
            if !on_screen {
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
        self.drawn != Some(self.laid)
            || self.moving()
            || self.visible_rows().any(Widget::needs_redraw)
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.drawn != Some(self.laid) || self.moving() {
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
        let laid = self.laid;
        for (i, row) in self.rows.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss, reason = "as in `new`")]
            let on_screen = (i as f64 - laid).abs() < 1.0 - POSITION_TOLERANCE;
            if on_screen {
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
        assert_eq!(deck.shown(), 1, "the deck goes to the strip below");
        assert_eq!(
            deck.take_requested_strip(),
            Some(1),
            "and asks for it to be focused"
        );
        assert_eq!(deck.take_requested_strip(), None);
        // Resting on goes on another strip after the same wait.
        assert!(!deck.advance_edge_reach(t0 + EDGE_REACH_DWELL + EDGE_REACH_DWELL / 2));
        assert!(deck.advance_edge_reach(t0 + EDGE_REACH_DWELL * 2));
        assert_eq!(deck.shown(), 2);
        // Never past the last strip.
        assert!(!deck.advance_edge_reach(t0 + EDGE_REACH_DWELL * 4));
        assert_eq!(deck.shown(), 2);
        if let Some(row) = deck.row_mut(0) {
            row.set_external_content_drag_hover(Some(Point::new(500.0, 10.0)));
        }
        let t1 = t0 + EDGE_REACH_DWELL * 5;
        assert!(!deck.advance_edge_reach(t1));
        assert!(deck.advance_edge_reach(t1 + EDGE_REACH_DWELL));
        assert_eq!(deck.shown(), 1);
        if let Some(row) = deck.row_mut(0) {
            row.set_external_content_drag_hover(None);
        }
        assert!(!deck.advance_edge_reach(t1 + EDGE_REACH_DWELL * 3));
        assert_eq!(deck.shown(), 1);
    }

    #[test]
    fn the_deck_follows_the_fingers_and_settles_with_their_momentum() {
        let mut deck = deck_of(3);
        let height = deck.rect.height();
        let t0 = crate::time::now();
        let ms = |n: u64| crate::time::Duration::from_millis(n);
        deck.begin_drag(t0);
        // A slow drag a fifth of a screen down follows the fingers 1:1 ...
        for i in 1..=10_u32 {
            let travel = f64::from(i) * height * 0.02;
            deck.drag_to(travel, t0 + ms(u64::from(i) * 50));
        }
        assert!((deck.position.target() - 0.2).abs() < 1e-6);
        // ... and, released after a pause, springs back.
        let release = deck.end_drag(t0 + ms(800), false).expect("a drag");
        assert_eq!(release, StripDeckRelease { to: 0, from: 0 });
        deck.jump_to(0);
        deck.begin_drag(t0);
        for i in 1..=4_u32 {
            deck.drag_to(f64::from(i) * 25.0, t0 + ms(u64::from(i) * 8));
        }
        let release = deck.end_drag(t0 + ms(33), false).expect("a drag");
        assert_eq!(release, StripDeckRelease { to: 1, from: 0 });
        assert!(
            deck.position.velocity(t0 + ms(33)) > 0.0,
            "the settle carries the fingers' momentum"
        );
        deck.jump_to(2);
        deck.begin_drag(t0);
        deck.drag_to(height, t0 + ms(16));
        assert!(deck.position.target() < 2.0 + OVERPULL_REACH_FRAC);
        let release = deck.end_drag(t0 + ms(400), false).expect("a drag");
        assert_eq!(release.to, 2);
        deck.jump_to(1);
        deck.begin_drag(t0);
        deck.drag_to(height * 0.9, t0 + ms(16));
        let release = deck.end_drag(t0 + ms(32), true).expect("a drag");
        assert_eq!(release, StripDeckRelease { to: 1, from: 1 });
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
