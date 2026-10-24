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

/// Where a band stands in its container, as the container a rebuild makes
/// in its place can recognise it: the container (its kind, its width and
/// the kinds of the other widgets in it, in order), how many of those come
/// before the band, and the space the container keeps above it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub parent: u64,
    pub index: usize,
    pub gap: f64,
}

/// What a band leaves when it is dropped (`motion::keep`): for the band a
/// rebuild puts in its place to go on from, or, when none does, for its
/// container to draw it leaving.
pub(crate) struct KeptBand {
    pub disclosure: crate::motion::Disclosure,
    /// Where it was laid out or drawn, at its full height.
    pub laid_rect: vello::kurbo::Rect,
    /// Where it stood in its container, if a container placed it.
    pub placement: Option<Placement>,
    /// Its last paint at its full height (at rest, or on its way and
    /// clipped as it was drawn), where it was laid out.
    pub painted: vello::Scene,
    /// It was shown when dropped (or already leaving).
    pub shown: bool,
}

/// A band its view stopped building, leaving: its last paint collapsing
/// from the top with `motion::Disclose`'s exit (from where it was drawn)
/// while what follows moves up by transform. It takes no input and is drawn
/// until it has gone.
struct Ghost {
    /// The container's child it stands before (its followers start here).
    index: usize,
    kept: KeptBand,
    /// Where it is drawn: its slot, at its full height.
    rect: vello::kurbo::Rect,
}

impl Ghost {
    /// How far the widgets after it are drawn below their laid-out places.
    fn offset(&self) -> f64 {
        if !self.kept.disclosure.is_sliding() {
            return 0.0;
        }
        let extent = self.rect.height();
        let gap = self.kept.placement.map_or(0.0, |placement| placement.gap);
        Disclosing::Band {
            extent,
            drawn: self.kept.disclosure.reveal(extent).height,
        }
        .follower_offset(gap)
    }

    /// Draw what is left of it, `dy` below its slot.
    fn draw(&self, scene: &mut vello::Scene, transform: vello::kurbo::Affine, dy: f64) {
        let reveal = self.kept.disclosure.reveal(self.rect.height());
        if reveal.height <= 0.0 || reveal.opacity <= 0.0 {
            return;
        }
        let place = transform * vello::kurbo::Affine::translate((0.0, dy));
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            reveal.opacity,
            place,
            &vello::kurbo::Rect::new(
                self.rect.x0,
                self.rect.y0,
                self.rect.x1,
                self.rect.y0 + reveal.height,
            ),
        );
        let laid = self.kept.laid_rect;
        scene.append(
            &self.kept.painted,
            Some(
                place
                    * vello::kurbo::Affine::translate((
                        self.rect.x0 - laid.x0,
                        self.rect.y0 - laid.y0,
                    )),
            ),
        );
        scene.pop_layer();
    }
}

/// The bands a container's view stopped building, drawn leaving. Dropped
/// with its container, they are left for the container a rebuild puts in
/// its place.
#[derive(Default)]
pub(crate) struct Ghosts(Vec<Ghost>);

impl Ghosts {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Forget the ones that have gone.
    pub fn retain_drawn(&mut self) {
        self.0.retain(|ghost| ghost.kept.disclosure.is_drawn());
    }

    /// After a container's layout: tell each band among `children` where
    /// it stands (`gap(i)` is the space kept above child `i`), and take up
    /// as ghosts the bands a rebuild dropped from this container that
    /// nothing took up. None in a window's first frame, none when the view
    /// was replaced as a whole (the container then differs). `kind` names
    /// the container's kind.
    pub fn find(
        &mut self,
        kind: &str,
        children: &mut [Box<dyn Widget>],
        content: vello::kurbo::Rect,
        gap: impl Fn(usize) -> f64,
        motion: crate::tokens::EffectiveMotion,
    ) {
        self.retain_drawn();
        let parent = placement_parent(kind, content, children.iter().map(|child| &**child));
        let mut index = 0;
        for (position, child) in children.iter_mut().enumerate() {
            if child.is_band() {
                child.note_placement(Placement {
                    parent,
                    index,
                    gap: gap(position),
                });
            } else {
                index += 1;
            }
        }
        if !crate::motion::window_has_shown() {
            return;
        }
        while let Some(mut kept) = crate::motion::take_kept_where::<KeptBand>(|kept| {
            kept.shown
                && kept
                    .placement
                    .is_some_and(|placement| placement.parent == parent)
        }) {
            let Some(placement) = kept.placement else {
                continue;
            };
            if kept.disclosure.is_open() {
                kept.disclosure.leave(motion);
            }
            // Gone by a key's result, or never painted: nothing to draw,
            // and never a blank band.
            if !kept.disclosure.is_drawn() || kept.painted.encoding().is_empty() {
                continue;
            }
            // Its slot: before the widget that followed it, under the one
            // before it.
            let mut others = 0;
            let mut at = children.len();
            let mut bottom: Option<f64> = None;
            for (position, child) in children.iter().enumerate() {
                if !child.is_band() {
                    if others == placement.index {
                        at = position;
                        break;
                    }
                    others += 1;
                }
                let rect = child.get_rect();
                if rect != vello::kurbo::Rect::ZERO && child.disclosing().is_none() {
                    bottom = Some(rect.y1);
                }
            }
            let top = bottom.map_or(content.y0, |bottom| bottom + placement.gap);
            let rect =
                vello::kurbo::Rect::new(content.x0, top, content.x1, top + kept.laid_rect.height());
            self.0.push(Ghost {
                index: at,
                kept,
                rect,
            });
        }
    }

    /// `shifts` (a container's [`follower_shifts`] over `count` children)
    /// with the ghosts' followers moving up with their edges.
    pub fn shift(&self, shifts: Option<Vec<f64>>, count: usize) -> Option<Vec<f64>> {
        if self.0.is_empty() {
            return shifts;
        }
        let mut shifts = shifts.unwrap_or_else(|| vec![0.0; count + 1]);
        for ghost in &self.0 {
            let offset = ghost.offset();
            for shift in shifts.iter_mut().skip(ghost.index) {
                *shift += offset;
            }
        }
        Some(shifts)
    }

    /// Draw them, each under what is drawn shifted before it (`shifts`
    /// from [`Self::shift`]).
    pub fn draw(
        &self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        shifts: Option<&[f64]>,
    ) {
        for ghost in &self.0 {
            let dy = shifts
                .and_then(|shifts| shifts.get(ghost.index).copied())
                .unwrap_or(0.0)
                - ghost.offset();
            ghost.draw(scene, transform, dy);
        }
    }

    /// Leave them for the container a rebuild puts in place of this one.
    pub fn keep(&mut self) {
        for ghost in self.0.drain(..) {
            let Some(placement) = ghost.kept.placement else {
                continue;
            };
            crate::motion::keep(
                format!("leaving {} {}", placement.parent, placement.index),
                None,
                ghost.kept,
            );
        }
    }
}

impl Drop for Ghosts {
    fn drop(&mut self) {
        self.keep();
    }
}

/// A container's identity across rebuilds, for the bands in it: its kind,
/// its content box and the kinds of its widgets other than bands, in order.
fn placement_parent<'a>(
    kind: &str,
    content: vello::kurbo::Rect,
    children: impl IntoIterator<Item = &'a dyn Widget>,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let at = |value: f64| (value * 2.0).round() as i64;
    kind.hash(&mut hasher);
    (at(content.x0), at(content.x1), at(content.y0)).hash(&mut hasher);
    for child in children {
        if !child.is_band() {
            child.concrete_type_name().hash(&mut hasher);
        }
    }
    hasher.finish()
}
