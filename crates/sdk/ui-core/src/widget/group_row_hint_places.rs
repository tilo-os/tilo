//! What a strip's row tells control hints ([`crate::control_hints`]): where
//! its windows' bars and edges are, and whether windows lie out of view;
//! and what the person did with them, as it happens.

use super::GroupRow;
use crate::control_hints::performed::{self, Performed};
use crate::widget::{Widget, WindowEdge};
use vello::kurbo::{Line, Point, Rect};

/// Where a row's controls are for control hints, in the row's coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HintPlaces {
    /// The row's view.
    pub viewport: Rect,
    /// How many windows the row holds.
    pub windows: usize,
    /// Windows lie past the view's leading and trailing edge.
    pub beyond: (bool, bool),
    /// Where the bar under the pointer takes its window: the stretch of
    /// it the pointer rests on, between the bar's tabs and controls.
    pub hovered_bar: Option<Rect>,
    /// Where the focused window's bar takes it: the stretch nearest its
    /// trailing end. Most of a bar is tabs, a location and controls, which
    /// a drag does not take the window by.
    pub focused_bar: Option<Rect>,
    /// The focused window's edge nearest the pointer that resizes it, as
    /// the line it is lit along, and which edge it is.
    pub focused_edge: Option<(Line, WindowEdge)>,
}

impl GroupRow {
    /// Where this row's controls are for control hints.
    #[must_use]
    pub fn hint_places(&self) -> HintPlaces {
        let viewport = self.viewport();
        let (seen_from, seen_to) = self.visible_canvas_span();
        let beyond = self.group_canvas_spans().iter().fold(
            (false, false),
            |(before, after), (_, x0, x1)| {
                (
                    before || *x0 < seen_from - 1.0,
                    after || *x1 > seen_to + 1.0,
                )
            },
        );
        let shown = |i: usize, group: &&super::WindowGroup| {
            !self.layout_hides_group(i) && !group.get_rect().intersect(viewport).is_zero_area()
        };
        let hovered_bar = self
            .groups
            .iter()
            .enumerate()
            .find(|(i, group)| shown(*i, group) && group.bar_grab_hit(self.last_cursor))
            .map(|(_, group)| grab_stretch(group, self.last_cursor.x));
        let focused = self
            .groups
            .iter()
            .enumerate()
            .find(|(i, group)| group.focused && shown(*i, group))
            .map(|(_, group)| group);
        HintPlaces {
            viewport,
            windows: self.groups.len(),
            beyond,
            hovered_bar,
            focused_bar: focused.and_then(trailing_grab_stretch),
            focused_edge: focused.and_then(|group| nearest_edge(group, viewport, self.last_cursor)),
        }
    }

    /// The pointer moved: resting on a window's bar, where the window can
    /// be taken, is noted for control hints.
    pub(super) fn note_bar_hover(&self) {
        if self.group_drag.is_none()
            && self.edge_drag.is_none()
            && self
                .groups
                .iter()
                .any(|group| group.bar_grab_hit(self.last_cursor))
        {
            performed::note(Performed::HoveredWindowBar);
        }
    }
}

/// How finely a bar is walked for where it takes its window.
const GRAB_STEP: f64 = 4.0;

/// The stretch of `group`'s bar around `x`, a point that takes the window,
/// as far as the bar takes it on either side.
fn grab_stretch(group: &super::WindowGroup, x: f64) -> Rect {
    let bar = group.bar_rect();
    let y = bar.center().y;
    let takes = |x: f64| x >= bar.x0 && x <= bar.x1 && group.bar_grab_hit(Point::new(x, y));
    let mut from = x;
    while takes(from - GRAB_STEP) {
        from -= GRAB_STEP;
    }
    let mut to = x;
    while takes(to + GRAB_STEP) {
        to += GRAB_STEP;
    }
    Rect::new(from, bar.y0, to, bar.y1)
}

/// The stretch of `group`'s bar nearest its trailing end that takes the
/// window, if any does.
fn trailing_grab_stretch(group: &super::WindowGroup) -> Option<Rect> {
    let bar = group.bar_rect();
    let y = bar.center().y;
    std::iter::successors(Some(bar.x1 - 1.0), |x| Some(x - GRAB_STEP))
        .take_while(|x| *x > bar.x0)
        .find(|x| group.bar_grab_hit(Point::new(*x, y)))
        .map(|x| grab_stretch(group, x))
}

/// The edge of `group` nearest `pointer` that a drag resizes and that is in
/// `viewport`, along the window's body under its bar.
fn nearest_edge(
    group: &super::WindowGroup,
    viewport: Rect,
    pointer: Point,
) -> Option<(Line, WindowEdge)> {
    let rect = group.get_rect();
    let top = group.bar_rect().y1.max(rect.y0);
    let inset = 12.0;
    let line_at = |x: f64| Line::new((x, top + inset), (x, rect.y1 - inset));
    let candidates = [
        group
            .leading_resize_grab_rect()
            .map(|_| (line_at(rect.x0), WindowEdge::Leading)),
        group
            .resize_grab_rect()
            .map(|_| (line_at(rect.x1), WindowEdge::Trailing)),
    ];
    let usable: Vec<(Line, WindowEdge)> = candidates
        .into_iter()
        .flatten()
        .filter(|(line, _)| line.p0.x > viewport.x0 + 1.0 && line.p0.x < viewport.x1 - 1.0)
        .filter(|(line, _)| line.p1.y > line.p0.y)
        .collect();
    // The trailing edge, the one between windows that a resize most often
    // takes; the leading one when the pointer rests close by it.
    let near_leading = rect.contains(pointer) && pointer.x < rect.x0 + LEADING_SHARE * rect.width();
    let wanted = if near_leading {
        WindowEdge::Leading
    } else {
        WindowEdge::Trailing
    };
    usable
        .iter()
        .find(|(_, edge)| *edge == wanted)
        .or_else(|| usable.first())
        .copied()
}

/// How far into a window, as a share of its width, the pointer counts as
/// near its leading edge.
const LEADING_SHARE: f64 = 0.25;
