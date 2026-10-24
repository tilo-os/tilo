//! A band on its way in or out (`motion::Disclosure`) and what follows it:
//! a column lays the band out of its flow while it moves, draws the widgets
//! after it shifted by transform, and hits them where they are drawn.

use super::{EventCtx, Widget, route_to_children};
use taffy::NodeId;
use winit::event::WindowEvent;

/// A band on its way in or out (`motion::Disclosure`), as the container it
/// stands in moves what follows it ([`Widget::disclosing`]).
///
/// While a band moves it stands out of its container's layout: what
/// follows it is laid out once where it stands without the band (the larger
/// extent, so nothing below it is left bare), the container puts the band
/// in its slot ([`Widget::place_disclosing`]) and draws what follows it
/// shifted by transform. The layout takes the band in once, at rest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Disclosing {
    /// A band out of the layout while it moves: `extent` is its full
    /// height, `drawn` how much of it shows now.
    Band { extent: f64, drawn: f64 },
    /// A content-sized container a band inside is moving in: it is drawn
    /// this much taller than it is laid out, and what follows it moves as
    /// far.
    Grows(f64),
}

impl Disclosing {
    /// How far below their laid-out places the widgets after it are drawn,
    /// with `gap` between it and the next: a band's gap opens and closes
    /// with it, so nothing jumps by it when the band rests.
    #[must_use]
    pub fn follower_offset(self, gap: f64) -> f64 {
        match self {
            Self::Band { extent, drawn } => {
                if extent <= 0.0 {
                    0.0
                } else {
                    drawn / extent * (extent + gap)
                }
            }
            Self::Grows(by) => by,
        }
    }

    fn is_band(self) -> bool {
        matches!(self, Self::Band { .. })
    }
}

/// How far below its laid-out place each of a column's `children` is drawn
/// this frame, then how much taller the column is drawn than laid out: the
/// widgets after a band on its way move with its edge, `gap(i)` being the
/// space the column keeps above child `i`. `None` when nothing in the column
/// is moving, which is every frame but a band's few.
pub(crate) fn follower_shifts<'a>(
    children: impl IntoIterator<Item = &'a dyn Widget> + Clone,
    gap: impl Fn(usize) -> f64,
) -> Option<Vec<f64>> {
    if children
        .clone()
        .into_iter()
        .all(|child| child.disclosing().is_none())
    {
        return None;
    }
    let mut shift = 0.0;
    let mut shifts: Vec<f64> = children
        .into_iter()
        .enumerate()
        .map(|(index, child)| {
            let at = shift;
            if let Some(band) = child.disclosing() {
                shift += band.follower_offset(gap(index));
            }
            at
        })
        .collect();
    shifts.push(shift);
    Some(shifts)
}

/// Put each band on its way in a column ([`Disclosing::Band`], out of the
/// layout) in its slot: `gap(i)` below the last child above it the layout
/// placed, or at the top of `content` when none is, across `content`'s
/// width. A hidden child (laid out nowhere) places nothing.
pub(crate) fn place_bands<'a>(
    children: impl IntoIterator<Item = &'a mut Box<dyn Widget>>,
    content: vello::kurbo::Rect,
    gap: impl Fn(usize) -> f64,
) {
    let mut bottom: Option<f64> = None;
    for (index, child) in children.into_iter().enumerate() {
        if child.disclosing().is_some_and(Disclosing::is_band) {
            let top = bottom.map_or(content.y0, |bottom| bottom + gap(index));
            child.place_disclosing(vello::kurbo::Rect::new(content.x0, top, content.x1, top));
            continue;
        }
        let rect = child.get_rect();
        if rect != vello::kurbo::Rect::ZERO {
            bottom = Some(rect.y1);
        }
    }
}

/// A laid-out node's content box: its box less its padding.
pub(crate) fn content_box(
    taffy: &taffy::TaffyTree<()>,
    node: NodeId,
    rect: vello::kurbo::Rect,
) -> vello::kurbo::Rect {
    let Ok(layout) = taffy.layout(node) else {
        return rect;
    };
    let padding = layout.padding;
    vello::kurbo::Rect::new(
        rect.x0 + f64::from(padding.left),
        rect.y0 + f64::from(padding.top),
        rect.x1 - f64::from(padding.right),
        rect.y1 - f64::from(padding.bottom),
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
