//! Scrolling by copying the pixels that stay on screen.
//!
//! A scroll moves every pixel of a scroller's content, so damage alone
//! repaints its whole viewport on every frame of a scroll. When the content
//! moved by a pure translation since the frame the host last presented, most
//! of those pixels are already in the host's retained target, one delta away.
//!
//! A scroller says so from its `collect_damage` ([`offer`]) instead of
//! damaging its viewport: where its content paints, how far it moved, and
//! what it paints over its content that does not move with it (a scroll bar,
//! an edge fade). A host that keeps the last frame's pixels collects damage
//! inside [`accepting`]; any other host gets the viewport damage as before,
//! because [`offer`] answers `false` there.
//!
//! The host then plans each offer ([`plan`]) against the widget tree it just
//! drew: the copy is clipped by every ancestor's child clip and kept to whole
//! device pixels; a delta that is not a whole number of device pixels, or a
//! scroller drawn under another transform than the window's, is refused (the
//! viewport is damaged instead). What may paint over or under the content
//! without moving with it is damage: the scroller's own overlays, every
//! other widget whose box (with its effect reach) reaches into the viewport,
//! and the edges of every ancestor and of every frame that holds the
//! viewport (a border or an inset shadow drawn over children). Damage a copy
//! reads, those overlays and whatever else changed in place this frame, is
//! carried to where the copy lands ([`carry`]), and what the copy exposed is
//! repainted. The renderer copies first ([`encode_copies`], or
//! [`copy_pixels`] on a CPU canvas), then rasterizes the damage.
//!
//! A relayout's moves are checked against the offers ([`moves_besides`]):
//! content that moved by its scroller's delta is the copy's; content that
//! came, went or stayed put (a long list mounting rows, a pinned title, a
//! sticky header) is damage; anything else that moved makes the host
//! repaint as it would without copies.

use std::cell::RefCell;

use vello::kurbo::{Affine, Rect, Vec2};
use vello::wgpu;

use crate::widget::Widget;

/// How far outside its layout box a widget may paint: anti-aliasing, a
/// shadow, a hover lift. The native window grows every widget's damage by
/// as much, in logical pixels.
pub const EFFECT_REACH: f64 = 16.0;

/// The largest gap, in device pixels, between a delta and a whole number of
/// device pixels that still counts as whole.
const WHOLE_PIXEL: f64 = 1e-3;

/// How far, in logical pixels, a box inside an offering scroller may be from
/// its old box plus the delta and still count as moved by it: layout adds
/// the same origin to both, so only rounding separates them.
#[cfg(feature = "native")]
const SAME_PLACE: f64 = 1e-6;

/// A scroller's claim that, since the frame its host last presented, its
/// content moved by a pure translation within an unchanged viewport.
#[derive(Clone, Debug, PartialEq)]
pub struct ScrollOffer {
    /// The scroller, by address, to find it in the widget tree.
    owner: *const (),
    /// Where its content paints, in logical pixels.
    viewport: Rect,
    /// How far its content moved on screen since that frame, in logical
    /// pixels.
    delta: Vec2,
    /// What it paints over its content inside `viewport` that does not move
    /// with it, in logical pixels.
    overlays: Vec<Rect>,
    /// The transform its content lands on screen with ([`on_screen`]): the
    /// window's, unless an ancestor draws it scaled or moved, and then the
    /// viewport is not where the window shows it.
    transform: Affine,
}

impl ScrollOffer {
    /// Where the content paints, in logical pixels: what to damage when the
    /// copy is not made.
    pub fn viewport(&self) -> Rect {
        self.viewport
    }

    /// How far the content moved on screen, in logical pixels.
    pub fn delta(&self) -> Vec2 {
        self.delta
    }
}

thread_local! {
    static OFFERS: RefCell<Option<Vec<ScrollOffer>>> = const { RefCell::new(None) };
    /// The transform the cache being recorded will be appended with
    /// ([`drawing_into_cache`]).
    static OUTER: std::cell::Cell<Affine> = const { std::cell::Cell::new(Affine::IDENTITY) };
}

/// Puts back the enclosing cache's transform even when drawing unwinds.
struct Outer(Affine);

impl Drop for Outer {
    fn drop(&mut self) {
        OUTER.set(self.0);
    }
}

/// Record `draw` into a cache its caller then appends with `transform`
/// (a container that draws its children at the identity into a retained
/// scene): a scroller drawn inside learns where its content lands on screen
/// ([`on_screen`]).
pub fn drawing_into_cache<R>(transform: Affine, draw: impl FnOnce() -> R) -> R {
    let outer = OUTER.get();
    let _restore = Outer(outer);
    OUTER.set(outer * transform);
    draw()
}

/// Where `transform`, the one a widget draws with, lands on screen: through
/// every cache being recorded around it.
pub fn on_screen(transform: Affine) -> Affine {
    OUTER.get() * transform
}

/// Puts back the enclosing collection even when `collect` unwinds.
struct Collecting(Option<Vec<ScrollOffer>>);

impl Drop for Collecting {
    fn drop(&mut self) {
        OFFERS.set(self.0.take());
    }
}

/// Run `collect` (a widget tree's `collect_damage`) with scroll copies
/// accepted, and return what the scrollers in it offered instead of damaging
/// their viewports. Only a host that presents over its last frame's pixels
/// and copies each planned offer ([`plan`], [`encode_copies`]) may call it.
pub fn accepting(collect: impl FnOnce()) -> Vec<ScrollOffer> {
    let previous = Collecting(OFFERS.replace(Some(Vec::new())));
    collect();
    let offers = OFFERS.take();
    drop(previous);
    offers.unwrap_or_default()
}

/// From a scroller's `collect_damage`: offer to have its host copy what of
/// its content stays on screen. `viewport` is where the content paints,
/// `delta` how far it moved on screen since the frame the host last
/// presented, and `overlays` what the scroller paints over its content that
/// does not move with it, all in logical pixels; `transform` is the one its
/// content landed on screen with this frame ([`on_screen`]). The scroller
/// must still damage whatever inside its content changed besides moving.
///
/// Returns `false` when the host does not copy: the scroller then damages
/// its viewport as it would without copies.
pub fn offer<T: ?Sized>(
    owner: &T,
    viewport: Rect,
    delta: Vec2,
    overlays: Vec<Rect>,
    transform: Affine,
) -> bool {
    OFFERS.with_borrow_mut(|offers| {
        offers.as_mut().is_some_and(|offers| {
            offers.push(ScrollOffer {
                owner: std::ptr::from_ref(owner).cast::<()>(),
                viewport,
                delta,
                overlays,
                transform,
            });
            true
        })
    })
}

/// A widget of one tree walk by identity (its address and depth: a widget
/// held at the start of its parent shares the parent's address), with its
/// layout box.
#[cfg(feature = "native")]
pub(crate) type Placed = ((*const (), usize), Rect);

/// Every widget in tree order, by identity, with its layout box.
#[cfg(feature = "native")]
pub(crate) fn placed(root: &dyn Widget) -> Vec<Placed> {
    fn walk(widget: &dyn Widget, depth: usize, out: &mut Vec<Placed>) {
        out.push((
            (std::ptr::from_ref(widget).cast::<()>(), depth),
            widget.get_rect(),
        ));
        widget.for_each_child(&mut |child| walk(child, depth.saturating_add(1), out));
    }
    let mut out = Vec::new();
    walk(root, 0, &mut out);
    out
}

/// Whether `new` is `old` moved by `delta`.
#[cfg(feature = "native")]
fn moved_by(old: Rect, new: Rect, delta: Vec2) -> bool {
    (new.x0 - old.x0 - delta.x).abs() <= SAME_PLACE
        && (new.x1 - old.x1 - delta.x).abs() <= SAME_PLACE
        && (new.y0 - old.y0 - delta.y).abs() <= SAME_PLACE
        && (new.y1 - old.y1 - delta.y).abs() <= SAME_PLACE
}

/// Where one widget of a tree walk stands with respect to the offers: the
/// offering scroller whose content holds it (the innermost), and whether it
/// is an offering scroller itself.
#[cfg(feature = "native")]
#[derive(Clone, Copy)]
struct Standing<'a> {
    content_of: Option<&'a ScrollOffer>,
    scroller: bool,
}

/// Each widget of `list` (a tree walk) as it stands to `offers`.
#[cfg(feature = "native")]
fn standings<'a>(list: &[Placed], offers: &'a [ScrollOffer]) -> Vec<Standing<'a>> {
    // The offering scrollers the walk is inside, innermost last: the depth
    // of each and its offer.
    let mut open: Vec<(usize, &ScrollOffer)> = Vec::new();
    list.iter()
        .map(|&((widget, depth), _)| {
            while open.last().is_some_and(|&(scroller, _)| depth <= scroller) {
                open.pop();
            }
            let content_of = open.last().map(|&(_, offer)| offer);
            let scroller = offers.iter().find(|offer| offer.owner == widget);
            if let Some(offer) = scroller {
                open.push((depth, offer));
            }
            Standing {
                content_of,
                scroller: scroller.is_some(),
            }
        })
        .collect()
}

/// What a relayout (and a rebuild a widget described) changed besides what
/// scroll copies move.
#[cfg(feature = "native")]
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Moves {
    /// The old and new box of every widget that moved or resized otherwise,
    /// and the box of every widget that came or went outside a scrolled
    /// content: a frame with any repaints as it would without copies.
    pub(crate) besides: Vec<Rect>,
    /// What changed in place inside a scrolled content besides moving with
    /// it: where a widget came or went (a long list mounting the rows a
    /// scroll brought near), and where one that stayed put paints over the
    /// viewport. Damage, which the copies carry ([`carry`]) like the rest.
    pub(crate) carried: Vec<Rect>,
}

/// What moved between `before` and `after` (two walks of one tree, matched
/// by identity) besides what `offers` copy: a widget inside an offering
/// scroller's content that moved by exactly that scroller's delta is the
/// copy's, and one that stayed put (a pinned title above a scrolled body, a
/// sticky header over it) is damage where it is and where the copy took its
/// pixels. The scroller itself must not move.
#[cfg(feature = "native")]
pub(crate) fn moves_besides(before: &[Placed], after: &[Placed], offers: &[ScrollOffer]) -> Moves {
    let old_standings = standings(before, offers);
    let new_standings = standings(after, offers);
    let old: std::collections::HashMap<_, _> = before
        .iter()
        .zip(&old_standings)
        .map(|(&(widget, rect), &standing)| (widget, (rect, standing)))
        .collect();
    let mut moves = Moves::default();
    let mut kept = std::collections::HashSet::with_capacity(after.len());
    for (&(widget, new), &standing) in after.iter().zip(&new_standings) {
        let Some(&(was, was_standing)) = old.get(&widget) else {
            match standing.content_of {
                Some(_) if !standing.scroller => moves.carried.push(new),
                _ => moves.besides.push(new),
            }
            continue;
        };
        kept.insert(widget);
        let explained = match (standing, was_standing) {
            _ if standing.scroller || was_standing.scroller => {
                standing.scroller == was_standing.scroller && was == new
            }
            (
                Standing {
                    content_of: Some(offer),
                    ..
                },
                Standing {
                    content_of: Some(was_offer),
                    ..
                },
            ) if std::ptr::eq(offer, was_offer) => {
                // One that stayed put (a pinned title above the body, a
                // sticky header over it) paints over the content without
                // moving with it: repainted where it reaches into the
                // viewport, and, carried, where the copy took those pixels.
                if was == new && !moved_by(was, new, offer.delta) {
                    let reach = was
                        .inflate(EFFECT_REACH, EFFECT_REACH)
                        .intersect(offer.viewport);
                    if !empty(reach) {
                        moves.carried.push(reach);
                    }
                    true
                } else {
                    moved_by(was, new, offer.delta)
                }
            }
            (
                Standing {
                    content_of: None, ..
                },
                Standing {
                    content_of: None, ..
                },
            ) => was == new,
            _ => false,
        };
        if !explained {
            moves.besides.extend([was, new]);
        }
    }
    for (&(widget, was), standing) in before.iter().zip(&old_standings) {
        if kept.contains(&widget) {
            continue;
        }
        match standing.content_of {
            Some(_) if !standing.scroller => moves.carried.push(was),
            _ => moves.besides.push(was),
        }
    }
    moves
}

/// One planned copy within a host's retained target, in device pixels: the
/// source rectangle, and how far it moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceCopy {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub dx: i32,
    pub dy: i32,
}

impl DeviceCopy {
    /// Where the copy lands, or `None` when it would leave a `width` ×
    /// `height` target.
    pub fn destination(&self, width: u32, height: u32) -> Option<(u32, u32)> {
        let x = self.x.checked_add_signed(self.dx)?;
        let y = self.y.checked_add_signed(self.dy)?;
        let fits = |origin: u32, extent: u32, limit: u32| {
            origin.checked_add(extent).is_some_and(|end| end <= limit)
        };
        (self.width > 0
            && self.height > 0
            && fits(self.x, self.width, width)
            && fits(self.y, self.height, height)
            && fits(x, self.width, width)
            && fits(y, self.height, height))
        .then_some((x, y))
    }

    /// Where the copy lands, as a rectangle in device pixels.
    pub fn destination_rect(&self) -> Rect {
        let x = f64::from(self.x) + f64::from(self.dx);
        let y = f64::from(self.y) + f64::from(self.dy);
        Rect::new(x, y, x + f64::from(self.width), y + f64::from(self.height))
    }
}

/// What a host does with a frame's offers, in device pixels: the copies, in
/// order; the damage they add where something does not move with the
/// content (each copy's overlays, a refused offer's viewport), which the
/// host carries along with the rest of its damage ([`carry`]); and what the
/// copies exposed, repainted as it is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CopyPlan {
    pub copies: Vec<DeviceCopy>,
    pub damage: Vec<Rect>,
    pub exposed: Vec<Rect>,
}

/// One widget as the planner sees it: its identity, box and the clip it
/// applies to its children, and whether it is the root of a tree the window
/// draws beside the app's that encloses it (History, which lays the live
/// app out in its body): an ancestor in all but the walk.
struct Node {
    widget: *const (),
    depth: usize,
    rect: Rect,
    clip: Option<Rect>,
    encloses: bool,
}

fn nodes(roots: &[&dyn Widget]) -> Vec<Node> {
    fn walk(widget: &dyn Widget, depth: usize, out: &mut Vec<Node>) {
        out.push(Node {
            widget: std::ptr::from_ref(widget).cast::<()>(),
            depth,
            rect: widget.get_rect(),
            clip: widget.child_clip_rect(),
            encloses: false,
        });
        widget.for_each_child(&mut |child| walk(child, depth.saturating_add(1), out));
    }
    let mut out = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        let start = out.len();
        walk(*root, 0, &mut out);
        if index > 0
            && let Some(node) = out.get_mut(start)
        {
            node.encloses = true;
        }
    }
    out
}

/// `rect` scaled to device pixels.
fn device(rect: Rect, scale: f64) -> Rect {
    Rect::new(
        rect.x0 * scale,
        rect.y0 * scale,
        rect.x1 * scale,
        rect.y1 * scale,
    )
}

/// The whole device pixels inside `rect`.
fn inner(rect: Rect) -> Rect {
    Rect::new(
        rect.x0.ceil(),
        rect.y0.ceil(),
        rect.x1.floor(),
        rect.y1.floor(),
    )
}

fn empty(rect: Rect) -> bool {
    !(rect.width() > 0.0 && rect.height() > 0.0)
}

/// `outer` without `hole` (which it contains), as up to four rectangles.
fn without(outer: Rect, hole: Rect) -> Vec<Rect> {
    [
        Rect::new(outer.x0, outer.y0, outer.x1, hole.y0),
        Rect::new(outer.x0, hole.y1, outer.x1, outer.y1),
        Rect::new(outer.x0, hole.y0, hole.x0, hole.y1),
        Rect::new(hole.x1, hole.y0, outer.x1, hole.y1),
    ]
    .into_iter()
    .filter(|rect| !empty(*rect))
    .collect()
}

/// The band within `reach` of `rect`'s edges, as four rectangles.
fn edges(rect: Rect, reach: f64) -> [Rect; 4] {
    let outer = rect.inflate(reach, reach);
    [
        Rect::new(outer.x0, outer.y0, outer.x1, rect.y0 + reach),
        Rect::new(outer.x0, rect.y1 - reach, outer.x1, outer.y1),
        Rect::new(outer.x0, outer.y0, rect.x0 + reach, outer.y1),
        Rect::new(rect.x1 - reach, outer.y0, outer.x1, outer.y1),
    ]
}

/// A whole number of device pixels, if `value` is one.
#[allow(
    clippy::cast_possible_truncation,
    reason = "checked against the target's extent, which an i32 holds, before narrowing"
)]
fn whole(value: f64, extent: f64) -> Option<i32> {
    let rounded = value.round();
    ((value - rounded).abs() <= WHOLE_PIXEL && rounded.abs() < extent).then_some(rounded as i32)
}

/// One offer planned against the tree, or `Err` with the damage that
/// replaces it.
fn plan_one(
    offer: &ScrollOffer,
    tree: &[Node],
    scale: f64,
    window: Rect,
) -> Result<(DeviceCopy, Vec<Rect>, Vec<Rect>), Rect> {
    let refused = device(offer.viewport, scale).expand().intersect(window);
    // Drawn under another transform than the window's (an ancestor that
    // scales or moves it), the viewport is not where the window shows it.
    if offer.transform != crate::scale::composite_transform(scale) {
        return Err(window);
    }
    // The scroller: the deepest of the widgets at its address (a decorator
    // holding it first shares it).
    let Some(index) = tree
        .iter()
        .enumerate()
        .filter(|(_, node)| node.widget == offer.owner)
        .max_by_key(|(_, node)| node.depth)
        .map(|(index, _)| index)
    else {
        return Err(refused);
    };
    let owner_depth = tree.get(index).map_or(0, |node| node.depth);
    // Its ancestors, walking back: each widget shallower than every one
    // after it up to the scroller.
    let mut ancestors = Vec::new();
    let mut shallowest = owner_depth;
    for node in tree.get(..index).unwrap_or_default().iter().rev() {
        if node.depth < shallowest {
            shallowest = node.depth;
            ancestors.push(node);
        }
    }
    let subtree_end = tree
        .iter()
        .enumerate()
        .skip(index.saturating_add(1))
        .find(|(_, node)| node.depth <= owner_depth)
        .map_or(tree.len(), |(end, _)| end);
    let logical_window = Rect::new(0.0, 0.0, window.x1 / scale, window.y1 / scale);
    let mut clip = offer.viewport.intersect(logical_window);
    for ancestor in &ancestors {
        if let Some(ancestor_clip) = ancestor.clip {
            clip = clip.intersect(ancestor_clip);
        }
    }
    if empty(clip) {
        return Err(refused);
    }
    let shown = device(clip, scale);
    let outer = shown.expand().intersect(window);
    let whole_pixels = inner(shown);
    let (Some(dx), Some(dy)) = (
        whole(offer.delta.x * scale, whole_pixels.width()),
        whole(offer.delta.y * scale, whole_pixels.height()),
    ) else {
        return Err(outer);
    };
    if dx == 0 && dy == 0 {
        return Err(outer);
    }
    let shift = Vec2::new(f64::from(dx), f64::from(dy));
    let landed = whole_pixels.intersect(whole_pixels + shift);
    if empty(landed) {
        return Err(outer);
    }
    // Everything that does not move with the content is damage where it
    // paints; the host carries it to where the copy takes those pixels.
    let mut fixed: Vec<Rect> = offer.overlays.clone();
    let enclosing = tree.iter().filter(|node| node.encloses);
    for ancestor in ancestors.iter().copied().chain(enclosing) {
        fixed.extend(edges(ancestor.rect, EFFECT_REACH));
    }
    let subtree = index..subtree_end;
    for (at, node) in tree.iter().enumerate() {
        if at == index
            || node.encloses
            || subtree.contains(&at)
            || ancestors
                .iter()
                .any(|ancestor| std::ptr::eq(*ancestor, node))
            || empty(node.rect)
        {
            continue;
        }
        // A widget beside the scroller whose box holds the whole viewport is
        // the frame it sits in (a shell, a window's chrome): like an
        // ancestor, it may paint at its edges. Any other that reaches in
        // paints over or under part of the content.
        if node.rect.contains_rect(clip) {
            fixed.extend(edges(node.rect, EFFECT_REACH));
        } else {
            fixed.push(node.rect.inflate(EFFECT_REACH, EFFECT_REACH));
        }
    }
    let overlays = fixed
        .into_iter()
        .map(|rect| rect.intersect(clip))
        .filter(|rect| !empty(*rect))
        .map(|rect| {
            device(rect, scale)
                .expand()
                .inflate(1.0, 1.0)
                .intersect(outer)
        })
        .collect();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "whole, non-negative device pixels inside the window, which a u32 holds"
    )]
    let copy = DeviceCopy {
        x: (landed.x0 - shift.x) as u32,
        y: (landed.y0 - shift.y) as u32,
        width: landed.width() as u32,
        height: landed.height() as u32,
        dx,
        dy,
    };
    Ok((copy, overlays, without(outer, landed)))
}

/// Plan `offers` against the trees under `roots` just drawn (the app's,
/// then any the window draws beside it, such as History's toolbar), at
/// `scale` device pixels per logical pixel, for a `width` × `height`
/// target: each becomes a copy and the damage around it, or, when it cannot
/// be copied exactly, damage over its whole viewport.
pub fn plan(
    offers: &[ScrollOffer],
    roots: &[&dyn Widget],
    scale: f64,
    (width, height): (u32, u32),
) -> CopyPlan {
    let mut planned = CopyPlan::default();
    if offers.is_empty() {
        return planned;
    }
    let tree = nodes(roots);
    let window = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
    // Where accepted copies read and write: a later copy that overlaps them
    // would read pixels an earlier one moved.
    let mut claimed: Vec<Rect> = Vec::new();
    for offer in offers {
        let reach = device(offer.viewport, scale).expand().intersect(window);
        match plan_one(offer, &tree, scale, window) {
            Ok((copy, overlays, exposed))
                if copy.destination(width, height).is_some()
                    && claimed.iter().all(|area| empty(area.intersect(reach))) =>
            {
                claimed.push(reach);
                planned.copies.push(copy);
                planned.damage.extend(overlays);
                planned.exposed.extend(exposed);
            }
            Ok(_) => planned.damage.push(reach),
            Err(damage) => planned.damage.push(damage),
        }
    }
    planned.damage.retain(|rect| !empty(*rect));
    planned.exposed.retain(|rect| !empty(*rect));
    planned
}

/// Add to `damage` (device pixels, where a frame differs from the last in
/// place) where `copies` take what it covers: a pixel that changed in place
/// and that a copy moves lands, stale, one delta away. Everything but what
/// the copies exposed goes through here, the overlays the plan found
/// included.
pub fn carry(copies: &[DeviceCopy], damage: &mut Vec<Rect>) {
    let changed = damage.len();
    for copy in copies {
        let source = Rect::new(
            f64::from(copy.x),
            f64::from(copy.y),
            f64::from(copy.x) + f64::from(copy.width),
            f64::from(copy.y) + f64::from(copy.height),
        );
        let shift = Vec2::new(f64::from(copy.dx), f64::from(copy.dy));
        for index in 0..changed {
            let Some(rect) = damage.get(index) else {
                continue;
            };
            let taken = rect.intersect(source);
            if !empty(taken) {
                damage.push(taken + shift);
            }
        }
    }
}

/// The most rows (or, for a sideways copy, columns) one piece of a copy
/// moves through the scratch texture, which bounds its size.
const BAND: u32 = 128;

/// A target's scratch space for copies within it, which a copy cannot do in
/// place: WebGPU refuses a copy whose source and destination are the same
/// texture's same subresource. Each copy moves through it in bands of at
/// most [`BAND`] rows, so it holds a band, not a viewport.
#[derive(Default)]
pub struct CopyScratch {
    texture: Option<wgpu::Texture>,
}

impl CopyScratch {
    /// Let go of the scratch texture.
    pub fn release(&mut self) {
        self.texture = None;
    }
}

/// `copy` in pieces of at most [`BAND`] rows across the direction it moves,
/// ordered so that no piece reads pixels an earlier one wrote: the piece
/// nearest where the content moves goes first.
pub(crate) fn bands(copy: DeviceCopy) -> Vec<DeviceCopy> {
    let vertical = copy.dy != 0;
    let extent = if vertical { copy.height } else { copy.width };
    let mut pieces: Vec<DeviceCopy> = (0..extent.div_ceil(BAND))
        .map(|band| {
            let start = band.saturating_mul(BAND);
            let length = BAND.min(extent.saturating_sub(start));
            if vertical {
                DeviceCopy {
                    y: copy.y.saturating_add(start),
                    height: length,
                    ..copy
                }
            } else {
                DeviceCopy {
                    x: copy.x.saturating_add(start),
                    width: length,
                    ..copy
                }
            }
        })
        .collect();
    if copy.dy > 0 || (copy.dy == 0 && copy.dx > 0) {
        pieces.reverse();
    }
    pieces
}

/// Record `copies` within `texture`, in order, each through `scratch`.
/// Returns `false`, recording nothing, when a copy would leave the texture:
/// the caller must then repaint the target whole.
pub fn encode_copies(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    scratch: &mut CopyScratch,
    copies: &[DeviceCopy],
) -> bool {
    let (width, height) = (texture.width(), texture.height());
    if copies
        .iter()
        .any(|copy| copy.destination(width, height).is_none())
    {
        return false;
    }
    let pieces: Vec<DeviceCopy> = copies.iter().copied().flat_map(bands).collect();
    let need = pieces.iter().fold((0, 0), |(w, h), piece| {
        (u32::max(w, piece.width), u32::max(h, piece.height))
    });
    if need.0 == 0 || need.1 == 0 {
        return true;
    }
    let fits = scratch.texture.as_ref().is_some_and(|scratch| {
        scratch.width() >= need.0
            && scratch.height() >= need.1
            && scratch.format() == texture.format()
    });
    if !fits {
        scratch.texture = Some(device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ui-scroll-copy-scratch"),
            size: wgpu::Extent3d {
                width: need.0,
                height: need.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: texture.format(),
            usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        }));
    }
    let Some(scratch) = scratch.texture.as_ref() else {
        return false;
    };
    let at = |texture, x, y| wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: 0,
        origin: wgpu::Origin3d { x, y, z: 0 },
        aspect: wgpu::TextureAspect::All,
    };
    for piece in pieces {
        let Some((x, y)) = piece.destination(width, height) else {
            continue;
        };
        let extent = wgpu::Extent3d {
            width: piece.width,
            height: piece.height,
            depth_or_array_layers: 1,
        };
        encoder.copy_texture_to_texture(at(texture, piece.x, piece.y), at(scratch, 0, 0), extent);
        encoder.copy_texture_to_texture(at(scratch, 0, 0), at(texture, x, y), extent);
    }
    true
}

/// Apply `copies` to `pixels`, a `width`-pixel-wide RGBA8 image whose rows
/// are `stride` bytes apart, in order. Returns `false`, changing nothing,
/// when a copy would leave the image.
pub fn copy_pixels(
    pixels: &mut [u8],
    (width, height, stride): (u32, u32, usize),
    copies: &[DeviceCopy],
) -> bool {
    let fits = copies.iter().all(|copy| {
        copy.destination(width, height).is_some()
            && usize::try_from(height)
                .ok()
                .and_then(|rows| rows.checked_mul(stride))
                .is_some_and(|bytes| bytes <= pixels.len())
    });
    if !fits {
        return false;
    }
    for copy in copies {
        let Some((x, y)) = copy.destination(width, height) else {
            return false;
        };
        let (Ok(from_x), Ok(to_x), Ok(row_bytes)) = (
            usize::try_from(copy.x),
            usize::try_from(x),
            usize::try_from(u64::from(copy.width) * 4),
        ) else {
            return false;
        };
        let rows: Vec<u32> = if copy.dy > 0 {
            (0..copy.height).rev().collect()
        } else {
            (0..copy.height).collect()
        };
        for row in rows {
            let (Ok(from_y), Ok(to_y)) = (
                usize::try_from(copy.y.saturating_add(row)),
                usize::try_from(y.saturating_add(row)),
            ) else {
                return false;
            };
            let from = from_y
                .saturating_mul(stride)
                .saturating_add(from_x.saturating_mul(4));
            let to = to_y
                .saturating_mul(stride)
                .saturating_add(to_x.saturating_mul(4));
            if from.saturating_add(row_bytes) > pixels.len()
                || to.saturating_add(row_bytes) > pixels.len()
            {
                return false;
            }
            pixels.copy_within(from..from.saturating_add(row_bytes), to);
        }
    }
    true
}

#[cfg(test)]
#[path = "scroll_copy_tests.rs"]
mod tests;
