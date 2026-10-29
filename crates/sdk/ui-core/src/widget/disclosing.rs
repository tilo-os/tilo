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
    /// clipped as it was drawn), where it was laid out; `None` when it was
    /// never drawn. Content that draws nothing still paints.
    pub painted: Option<vello::Scene>,
    /// It was shown when dropped (or already leaving).
    pub shown: bool,
}

/// How far a band is in taking up the motion of the band a rebuild dropped
/// where it stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Carry {
    /// Not laid out yet.
    Fresh,
    /// Its identity matched nothing: its place is looked up once it is
    /// laid out.
    Placing,
    /// Settled: it leaves its motion when dropped.
    Bound,
}

/// What every band keeps of its motion ([`Disclosing::Band`]): how much of
/// it is drawn, where it stands while it is out of its container's layout,
/// what it last painted, and what it leaves when dropped, for the band a
/// rebuild puts in its place or for its column to draw it leaving.
///
/// `visible` is whether the band's owner shows it now; a band that is only
/// there while it shows passes `true`.
pub(crate) struct BandMotion {
    /// How much of the band is drawn while it arrives or leaves: its box
    /// opens or closes from the top, what follows moving with its edge.
    pub disclosure: crate::motion::Disclosure,
    /// The motion the person allows, from the last theme seen.
    pub motion: crate::tokens::EffectiveMotion,
    /// Where the band is drawn while it moves, and was last laid out at
    /// rest: its slot and full height.
    pub laid_rect: vello::kurbo::Rect,
    /// Whether the layout gives the band its place: shown and at rest. On
    /// its way in or out it stands out of the layout, which keeps what
    /// follows where it is.
    pub laid_in_flow: bool,
    carry: Carry,
    /// On its way out of the layout with no container to place it: it lands.
    pub unplaced: bool,
    /// Its layout must be built again: it took up a band on its way, or
    /// arrived, after its first layout.
    pub structure_dirty: bool,
    /// Its last paint at its full height: what its column draws leaving
    /// when its view stops building it; `None` until it is drawn.
    painted: Option<vello::Scene>,
    /// Where it stands in its column.
    placement: Option<Placement>,
}

impl BandMotion {
    /// At rest, shown or not: first paint shows the band at rest; only a
    /// change moves it.
    pub fn new(shown: bool) -> Self {
        Self {
            disclosure: if shown {
                crate::motion::Disclosure::opened()
            } else {
                crate::motion::Disclosure::closed()
            },
            motion: crate::tokens::EffectiveMotion::default(),
            laid_rect: vello::kurbo::Rect::ZERO,
            laid_in_flow: shown,
            carry: Carry::Fresh,
            unplaced: false,
            structure_dirty: false,
            painted: None,
            placement: None,
        }
    }

    /// Whether the layout gives the band its place: shown and not on its
    /// way. A moving band stands out of the layout, in the slot its
    /// container gives it ([`Widget::place_disclosing`]).
    pub fn in_flow(&self, visible: bool) -> bool {
        visible && !self.disclosure.is_sliding()
    }

    /// Whether the band is drawn on its way in or out.
    pub fn in_motion(&self) -> bool {
        self.disclosure.is_moving()
    }

    /// Whether the band should join or leave the layout: it came to rest,
    /// or set off.
    pub fn layout_moved(&self, visible: bool) -> bool {
        self.laid_in_flow != self.in_flow(visible)
    }

    /// Show or hide the band's `node` with it. True when the style changed.
    pub fn style_display(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        node: NodeId,
        visible: bool,
    ) -> bool {
        self.laid_in_flow = self.in_flow(visible);
        let display = if self.laid_in_flow {
            taffy::Display::Flex
        } else {
            taffy::Display::None
        };
        let Ok(current) = taffy.style(node) else {
            return false;
        };
        if current.display == display {
            return false;
        }
        let mut style = current.clone();
        style.display = display;
        taffy.set_style(node, style).is_ok()
    }

    /// At its first build: go on from the band with the same `key` a
    /// rebuild dropped, if there is one.
    pub fn build(&mut self, key: &str, visible: bool) {
        if self.carry != Carry::Fresh {
            return;
        }
        if let Some(kept) = crate::motion::take_kept::<KeptBand>(key) {
            self.take_up(kept, visible);
            self.carry = Carry::Bound;
        } else {
            self.carry = Carry::Placing;
        }
    }

    /// At its first build, for a band known by its key alone (shown while
    /// it is built): go on from the band with the same `key` a rebuild
    /// dropped, else arrive at once. It never takes up the place of a band
    /// with another key, which leaves as this one arrives, and it never
    /// stands in the layout on its way in: what follows is laid out once,
    /// where it stands without it.
    pub fn build_keyed(&mut self, key: &str) {
        if self.carry != Carry::Fresh {
            return;
        }
        self.carry = Carry::Bound;
        match crate::motion::take_kept::<KeptBand>(key) {
            Some(kept) => self.take_up(kept, true),
            None => self.arrive(true, false),
        }
    }

    /// At its first layout, laid out at `rect`, when its key matched
    /// nothing: it takes up the band that stood here before (what it says
    /// changed), or it arrived. Arriving, a shown band moves in unless
    /// `arrives_in_place` or its window is showing its first frame.
    pub fn place_first(&mut self, rect: vello::kurbo::Rect, visible: bool, arrives_in_place: bool) {
        if self.carry != Carry::Placing {
            return;
        }
        self.carry = Carry::Bound;
        match self
            .laid_in_flow
            .then(|| crate::motion::take_kept_at::<KeptBand>(rect))
            .flatten()
        {
            Some(kept) => self.take_up(kept, visible),
            None => self.arrive(visible, arrives_in_place),
        }
        if self.layout_moved(visible) {
            self.structure_dirty = true;
        }
    }

    /// Go on from where the band a rebuild dropped was: shown at rest
    /// stays at rest, opening goes on opening, and a change of shown
    /// against it moves from there.
    fn take_up(&mut self, kept: KeptBand, visible: bool) {
        self.disclosure = kept.disclosure;
        self.laid_rect = kept.laid_rect;
        if self.disclosure.is_open() != visible {
            self.disclosure.show_change(visible, self.motion);
        }
    }

    /// Nothing was here before: a shown band moves in, unless it arrives
    /// in place or its window is showing its first frame.
    fn arrive(&mut self, visible: bool, arrives_in_place: bool) {
        if visible && !arrives_in_place && crate::motion::window_has_shown() {
            self.disclosure = crate::motion::Disclosure::closed();
            self.disclosure.show_change(true, self.motion);
        }
    }

    /// Frames while it moves, and one more to take it into the layout at
    /// rest.
    pub fn needs_frame(&self, visible: bool) -> bool {
        self.in_motion() || self.layout_moved(visible)
    }

    pub fn note_placement(&mut self, placement: Placement) {
        self.placement = Some(placement);
    }

    /// Its slot, where its container puts it while it is out of the
    /// layout: the full height it was last measured at stays.
    pub fn place(&mut self, slot: vello::kurbo::Rect) {
        self.unplaced = false;
        let height = self.laid_rect.height();
        self.laid_rect = vello::kurbo::Rect::new(slot.x0, slot.y0, slot.x1, slot.y0 + height);
    }

    /// Where it paints: while it moves it has no box of its own (`rect`),
    /// but is drawn in its slot.
    pub fn paint_bounds(&self, rect: vello::kurbo::Rect) -> vello::kurbo::Rect {
        if self.in_motion() {
            self.laid_rect
        } else {
            rect
        }
    }

    /// The band on its way, out of the layout; `None` in it or at rest.
    pub fn disclosing(&self) -> Option<Disclosing> {
        if self.laid_in_flow || !self.in_motion() {
            return None;
        }
        let extent = self.laid_rect.height();
        Some(Disclosing::Band {
            extent,
            drawn: self.disclosure.reveal(extent).height,
        })
    }

    /// On its way out of the layout with no container to put it in its
    /// slot, it lands at `visible`, for the next layout to take in. True
    /// when it did.
    pub fn land_unplaced(&mut self, visible: bool) -> bool {
        if !self.unplaced || self.laid_in_flow {
            return false;
        }
        self.disclosure.snap(visible);
        true
    }

    /// The scene to paint the band's contents into this frame, at its full
    /// height and where it is laid out or placed.
    pub fn take_painted(&mut self) -> vello::Scene {
        let mut painted = self.painted.take().unwrap_or_default();
        painted.reset();
        painted
    }

    /// Show `painted` (from [`Self::take_painted`]) at rest, and keep it,
    /// so its column can draw it leaving when its view stops building it.
    pub fn show_at_rest(
        &mut self,
        painted: vello::Scene,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
    ) {
        scene.append(&painted, Some(transform));
        self.painted = Some(painted);
    }

    /// Show `painted` (from [`Self::take_painted`]) as far as the band is
    /// drawn: its box opens or closes from the top over its contents, which
    /// fade in over the second half. Kept: dropped on its way, the band
    /// leaves from where it is drawn with this paint, never blank.
    pub fn show_moving(
        &mut self,
        painted: vello::Scene,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
    ) {
        let band = self.laid_rect;
        let reveal = self.disclosure.reveal(band.height());
        if reveal.height > 0.0 && reveal.opacity > 0.0 {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                reveal.opacity,
                transform,
                &vello::kurbo::Rect::new(band.x0, band.y0, band.x1, band.y0 + reveal.height),
            );
            scene.append(&painted, Some(transform));
            scene.pop_layer();
        }
        self.painted = Some(painted);
    }

    /// The band is dropped: leave its motion under `key` for the band a
    /// rebuild puts in its place, or for its column to draw it leaving.
    /// `rect` is its box in the layout; a band never laid out leaves
    /// nothing.
    pub fn leave(&mut self, key: String, rect: vello::kurbo::Rect, visible: bool) {
        if self.carry != Carry::Bound {
            return;
        }
        let at = if self.laid_in_flow {
            rect
        } else {
            self.laid_rect
        };
        crate::motion::keep(
            key,
            Some(at),
            KeptBand {
                disclosure: self.disclosure,
                laid_rect: at,
                placement: self.placement,
                painted: self.painted.take(),
                shown: visible,
            },
        );
    }
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
        let Some(painted) = &self.kept.painted else {
            return;
        };
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
            painted,
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
            if !kept.disclosure.is_drawn() || kept.painted.is_none() {
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
