//! A band on its way in or out (`motion::Disclosure`) and what follows it:
//! a column draws the widgets after a disclosing band shifted by
//! transform, and hits them where they are drawn.

use super::{EventCtx, Widget, route_to_children};
use taffy::NodeId;
use winit::event::WindowEvent;

/// A band on its way in or out (`motion::Disclosure`), as the column it
/// stands in moves what follows it ([`Widget::disclosing`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Disclosing {
    /// Its resting height.
    pub extent: f64,
    /// How much of it is drawn now.
    pub drawn: f64,
    /// Whether the layout already gives it its height (it is opening) or
    /// none (it is closing).
    pub laid: bool,
}

impl Disclosing {
    /// How far below their laid-out places the widgets after the band are
    /// drawn, with `gap` between it and the next: the gap opens and closes
    /// with the band, so nothing jumps by it when the band rests.
    #[must_use]
    pub fn follower_offset(self, gap: f64) -> f64 {
        if self.extent <= 0.0 {
            return 0.0;
        }
        let span = self.extent + gap;
        let drawn = self.drawn / self.extent * span;
        let laid = if self.laid { span } else { 0.0 };
        drawn - laid
    }
}

/// How far below its laid-out place each of a column's `children` is drawn
/// this frame: the widgets after a band disclosing
/// ([`Widget::disclosing`]) move with its edge. `None` when nothing in the
/// column is moving, which is every frame but a band's few.
pub(crate) fn follower_shifts<'a>(
    children: impl IntoIterator<Item = &'a dyn Widget> + Clone,
    gap: f64,
) -> Option<Vec<f64>> {
    if children
        .clone()
        .into_iter()
        .all(|child| child.disclosing().is_none())
    {
        return None;
    }
    let mut shift = 0.0;
    Some(
        children
            .into_iter()
            .map(|child| {
                let at = shift;
                if let Some(band) = child.disclosing() {
                    shift += band.follower_offset(gap);
                }
                at
            })
            .collect(),
    )
}

/// [`route_to_children`] for a column whose children may be drawn shifted
/// (`shifts`, from [`follower_shifts`]): each child is hit where it is
/// drawn.
pub(crate) fn route_to_shifted_children(
    children: &mut [Box<dyn Widget>],
    nodes: &[NodeId],
    shifts: Option<&[f64]>,
    event: &WindowEvent,
    ctx: &mut EventCtx<'_>,
) {
    let Some(shifts) = shifts else {
        route_to_children(children, nodes, event, ctx);
        return;
    };
    let shifted = |index: usize, scale_factor: f64| {
        let dy = shifts.get(index).copied().unwrap_or(0.0);
        event_offset(event, vello::kurbo::Vec2::new(0.0, dy), scale_factor)
    };
    let owner = children
        .iter()
        .position(|c| c.wants_pointer_capture())
        .or_else(|| children.iter().position(|c| c.modal_capture().is_some()));
    if let Some(idx) = owner {
        if let (Some(child), Some(&node)) = (children.get_mut(idx), nodes.get(idx)) {
            let event = shifted(idx, ctx.scale_factor);
            child.handle_event(&event, &mut ctx.reborrow(ctx.path, node));
        }
        return;
    }
    for (index, (child, &node)) in children.iter_mut().zip(nodes).enumerate() {
        let event = shifted(index, ctx.scale_factor);
        child.handle_event(&event, &mut ctx.reborrow(ctx.path, node));
        if *ctx.handled {
            break;
        }
    }
}

/// `event` as a widget drawn `offset` (logical pixels) from where it is
/// laid out meets it: a pointer or touch position moved back by `offset`,
/// so content on its way (a band disclosing above it, a pane gliding) is
/// hit-tested where it is drawn (`motion::Pose::to_local`). Every other
/// event, and any event at a zero offset, passes through untouched.
#[must_use]
pub fn event_offset(
    event: &WindowEvent,
    offset: vello::kurbo::Vec2,
    scale_factor: f64,
) -> std::borrow::Cow<'_, WindowEvent> {
    if offset == vello::kurbo::Vec2::ZERO {
        return std::borrow::Cow::Borrowed(event);
    }
    let back = |position: winit::dpi::PhysicalPosition<f64>| {
        winit::dpi::PhysicalPosition::new(
            position.x - offset.x * scale_factor,
            position.y - offset.y * scale_factor,
        )
    };
    match event {
        WindowEvent::CursorMoved {
            device_id,
            position,
        } => std::borrow::Cow::Owned(WindowEvent::CursorMoved {
            device_id: *device_id,
            position: back(*position),
        }),
        WindowEvent::Touch(touch) => {
            std::borrow::Cow::Owned(WindowEvent::Touch(winit::event::Touch {
                location: back(touch.location),
                ..*touch
            }))
        }
        _ => std::borrow::Cow::Borrowed(event),
    }
}
