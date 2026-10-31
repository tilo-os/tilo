//! Moving a strip under the person's hand: how a wheel or a swipe travels,
//! where a pan leaves it, and which window a pan leaves in view.
//!
//! The row itself does the moving; this module only turns input into travel
//! and travel into a resting place, so the same rules hold for a wheel, a
//! trackpad and a drag.

use ui::widget::GroupRow;
use ui::winit::event::MouseScrollDelta;

use tilo_desktop_model::GroupId;

/// Logical pixels a line of sideways wheel travels: the 40 px to the line a
/// trackpad's pixels are weighed against when telling sideways from
/// vertical, so both kinds of input move the strip alike.
const WHEEL_LINE_PX: f64 = 40.0;

/// A wheel event's sideways travel in logical pixels when horizontal motion
/// dominates, positive forward (toward later windows, like Super+Right).
/// Winit's deltas are negative for a wheel turned (or fingers scrolling)
/// rightward, which scrolls content toward its later columns and so moves
/// forward; it reports pixels physical. The shell's overlay host classifies
/// wheel input the same way.
pub(super) fn sideways(delta: MouseScrollDelta, scale_factor: f64) -> Option<f64> {
    let (dx, dy) = match delta {
        MouseScrollDelta::LineDelta(x, y) => {
            (f64::from(x) * WHEEL_LINE_PX, f64::from(y) * WHEEL_LINE_PX)
        }
        MouseScrollDelta::PixelDelta(p) => {
            let scale = scale_factor.max(f64::EPSILON);
            (p.x / scale, p.y / scale)
        }
    };
    (dx.abs() > dy.abs()).then_some(-dx)
}

/// A wheel event's vertical travel in logical pixels, positive for a wheel
/// turned (or fingers scrolling) downward, which winit reports negative.
pub(super) fn vertical(delta: MouseScrollDelta, scale_factor: f64) -> f64 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => -f64::from(y) * WHEEL_LINE_PX,
        MouseScrollDelta::PixelDelta(p) => -p.y / scale_factor.max(f64::EPSILON),
    }
}

/// The window a pan left the person looking at: the focused one while it is
/// still wholly in view, else the one showing the most of itself, the one
/// nearer the middle of the view between two showing as much.
pub(super) fn panned_to(row: &GroupRow, focused: Option<GroupId>) -> Option<GroupId> {
    let (left, right) = row.visible_canvas_span();
    let middle = f64::midpoint(left, right);
    let shown = |x0: f64, x1: f64| (x1.min(right) - x0.max(left)).max(0.0);
    let spans = row.group_canvas_spans();
    if spans
        .iter()
        .any(|&(id, x0, x1)| Some(id) == focused && shown(x0, x1) >= x1 - x0 - 0.5)
    {
        return focused;
    }
    spans
        .iter()
        .filter(|&&(_, x0, x1)| shown(x0, x1) > 0.0)
        .min_by(|&&(_, a0, a1), &&(_, b0, b1)| {
            let (a, b) = (shown(a0, a1), shown(b0, b1));
            if (a - b).abs() > 0.5 {
                b.total_cmp(&a)
            } else {
                let off = |x0: f64, x1: f64| (f64::midpoint(x0, x1) - middle).abs();
                off(a0, a1).total_cmp(&off(b0, b1))
            }
        })
        .map(|&(id, _, _)| id)
}

/// Pan `row` to `offset` the way the shell's direct strip pan does, within
/// the legal range, and keep it there when the strip's geometry changes. A
/// strip showing a maximized window stays at home.
pub(super) fn pan_row(row: &mut GroupRow, offset: f64) -> bool {
    if row.groups.iter().any(|g| g.maximized) {
        return false;
    }
    let target = offset.clamp(0.0, row.pan_max_scroll_x());
    let now = ui::time::now();
    row.begin_strip_pan();
    let moved = row.update_strip_pan(target - row.scroll_x(), now);
    row.end_strip_pan(now);
    if moved {
        row.hold_strip_pan();
    }
    moved
}

/// Whether `row` can pan `travel` from where it rests.
pub(super) fn can_pan(row: &GroupRow, travel: f64) -> bool {
    if row.groups.iter().any(|g| g.maximized) {
        return false;
    }
    let offset = row.scroll_x();
    if travel > 0.0 {
        offset < row.pan_max_scroll_x() - 0.5
    } else {
        travel < 0.0 && offset > 0.5
    }
}
