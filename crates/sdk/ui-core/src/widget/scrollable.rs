use crate::num::f32_of_f64;
use crate::time::{Duration, Instant};
use crate::widget::{AppModel, ModelPath};
use std::cell::Cell;
use std::rc::Rc;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::scrollbar::Scrollbar;
use crate::widget::{Element, Widget};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::BlendMode;
use winit::event::{ElementState, MouseButton, WindowEvent};

#[path = "scrollable_bars.rs"]
mod bars;
#[path = "scrollable_input.rs"]
mod input;

#[derive(Clone, Copy, Debug)]
pub enum ScrollAxis {
    Vertical,
    Horizontal,
    Both,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CursorMoveOutcome {
    /// The child still needs this move to update its hover state.
    Forward,
    /// The move was consumed by an active scrollbar drag.
    DragConsumed,
    /// The same position was already routed and nothing moved underneath it.
    Coalesced,
}

/// The thumb and its margins before a theme is known (`metrics.scroll_thumb`
/// and `(metrics.scrollbar_gutter - scroll_thumb) / 2` at 1x).
const BAR_THICKNESS: f64 = 8.0;
const BAR_MARGIN: f64 = 4.0;
const MIN_THUMB_LEN: f64 = crate::widget::scrollbar::MIN_THUMB_LEN;
/// Content must exceed the viewport by more than this (logical px) before
/// its scrollbar is shown.
const FIT_EPS: f32 = 0.5;
/// How far the bar's grab/reveal zone extends past its track, so the thin
/// thumb stays grabbable near the edge.
const HOVER_EXPAND: f64 = 6.0;
const LINE_STEP: f32 = 48.0;
#[allow(
    clippy::cast_possible_truncation,
    reason = "PAGE_FRACTION is a fixed 0..1 fraction; narrowing to f32 loses only unused precision"
)]
const PAGE_FRACTION: f32 = crate::widget::scrollbar::PAGE_FRACTION as f32;
const HOLD_INITIAL_MS: u64 = 300;
const HOLD_REPEAT_MS: u64 = 55;

/// `Instant + Duration` can panic on overflow; the fallback (keep `from`
/// unchanged) is unreachable outside decades of continuous uptime and just
/// means the deadline fires on the very next tick instead.
fn deadline_after(from: Instant, ms: u64) -> Instant {
    from.checked_add(Duration::from_millis(ms)).unwrap_or(from)
}

/// A track click-and-hold in progress: the bar keeps paging toward the
/// press point until released or the thumb reaches the click.
#[derive(Clone, Copy, Debug)]
struct TrackHold {
    vertical: bool,
    /// `+1` toward the end, `-1` toward the start.
    dir: f64,
    /// The press point; paging stops once the thumb reaches it.
    anchor: Point,
    next_at: Instant,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, orthogonal flag (fade state, drag state, layout mode, ...); they aren't a combinatorial state machine, so folding them into an enum/bitflags type would only obscure the field names at each call site"
)]
pub struct Scrollable {
    axis: ScrollAxis,
    reveal_focus: bool,
    last_focus_path: Option<Vec<usize>>,
    offset_x: f32,
    offset_y: f32,
    content_w: f32,
    content_h: f32,
    /// How far below the content's top its deepest widget reached when the
    /// layout last solved it (`layout::max_bottom_edge`), kept while it
    /// only moves.
    walked_h: f32,
    viewport_w: f32,
    viewport_h: f32,
    /// The glide after a flick: the lists' decay
    /// ([`crate::virtual_viewport::glide_step`]), in whole device pixels so
    /// a host that copies what a scroll keeps on screen copies every frame
    /// of it.
    glide: crate::virtual_viewport::ScrollGlide,
    /// The motion toward what [`Self::scroll_into_view`] was asked to show.
    scroll_to: crate::motion::ScrollTo,
    /// Device pixels per logical pixel on the display the last input came
    /// from: glides and jumps step in whole ones.
    device_scale: f64,
    /// The motion the theme allows, sampled by the latest draw.
    motion: crate::tokens::EffectiveMotion,
    /// The content's opacity in the latest draw: below 1 while it fades in
    /// after a long jump.
    fade_drawn: Cell<f32>,
    /// The content's opacity in the frame the host last presented.
    fade_shown: f32,
    /// Pins the outer viewport to an exact height instead of `percent(1.0)`,
    /// which resolves against nothing (viewport collapses to zero) when the
    /// parent has no definite height. See [`Scrollable::fill_parent_height`]
    /// for the flex-grow alternative.
    fixed_height: Option<f32>,
    /// Lets a document-size viewport shrink to its content while still
    /// becoming scrollable before it crowds its parent. This is deliberately
    /// distinct from `fixed_height`: dialogs often have short and long pages
    /// in one flow and should not reserve a large empty viewport for the
    /// short page.
    max_height: Option<f32>,
    height_limit_dirty: bool,
    /// Absorbs the parent's remaining flex height instead of claiming
    /// `percent(1.0)`. `fixed_height` wins if both are set.
    fill_parent_height: bool,
    /// The content is wrapped to fill at least the viewport
    /// ([`Self::with_content_min_viewport_height`]).
    content_fills_viewport: bool,
    child: Option<Element>,
    /// Retained Taffy identity for `child`; refreshed with the layout tree so
    /// event and scroll-layout paths never allocate `taffy.children()`.
    child_node: Option<NodeId>,
    rect: Rect,
    /// True whenever `scroll_by`/`tick` actually moved `offset_*`; a wheel
    /// event that only mutates this touches no descendant. Cleared by
    /// `mark_drawn`.
    scroll_dirty: bool,
    show_scrollbar: bool,
    /// Whether an overflowing viewport itself participates in sequential
    /// keyboard focus. Forms generally want Tab to enter their first control
    /// directly; document viewers may keep the viewport stop for arrow/Page
    /// navigation.
    sequential_focus: bool,
    /// Lets `MouseInput::Pressed` hit-test the thumb/track without a prior
    /// hover edge.
    last_cursor: Point,
    cursor_initialized: bool,
    vbar: Scrollbar,
    hbar: Scrollbar,
    /// `true` (default): bar fades in on activity, out when idle. `false`:
    /// always full opacity ([`Scrollable::always_visible`]).
    overlay: bool,
    /// `None` until first woken, so a freshly-shown list has no bar until
    /// content overflows or the user interacts.
    last_activity: Option<Instant>,
    /// Pointer inside a bar's reveal zone: pins the overlay visible and
    /// expands the track chrome.
    pointer_near_bar: bool,
    /// Rising-edge latch so the bar flashes into view the first frame its
    /// content overflows, then obeys the fade.
    was_overflowing: bool,
    focused: bool,
    hold: Option<TrackHold>,
    /// Visual-only dirty flag (bar fade/hover): triggers a redraw but,
    /// unlike [`Self::scroll_dirty`], no `apply_layout`.
    bar_dirty: bool,
    /// Whether the last submitted scene contained a visible overlay bar.
    /// This keeps one terminal frame alive to erase a fully faded bar.
    bar_visible_last_draw: Cell<bool>,
    /// Set by the frame that first omits a previously visible bar, then
    /// consumed by `mark_drawn` after its track damage is submitted.
    terminal_fade_damage: Cell<bool>,
    /// Theme setting sampled by the latest draw. It prevents a reduced-motion
    /// bar (which intentionally remains visible) from requesting fade ticks.
    reduce_motion_active: bool,
    /// Release-velocity samples of the continuous (touchpad/touchscreen)
    /// gesture in progress; a fast lift turns into [`Self::fling`].
    kinetic: crate::virtual_viewport::KineticSampler,
    /// The give past the ends while fingers pull the content there, and its
    /// spring back once they let go ([`crate::motion::RubberBand`]).
    band: crate::motion::RubberBand,
    /// Survives this widget being dropped and rebuilt — needed by apps that
    /// recreate their whole widget subtree every frame. Every offset
    /// mutation writes back through `clamp`. See
    /// [`Scrollable::with_persisted_offset`].
    persisted_offset: Option<Rc<Cell<(f32, f32)>>>,
    /// The trailing lane reserved for the vertical bar
    /// (`metrics.scrollbar_gutter`), resolved in `build_layout`.
    content_gutter: f32,
    /// Thumb thickness and the margin around it in the gutter, from the
    /// theme (`metrics.scroll_thumb`, `metrics.scrollbar_gutter`).
    bar_thickness: f64,
    bar_margin: f64,
    /// The rounded corners of the window this view sits in, learned where
    /// it last drew: its bars keep out of them.
    corners: crate::widget::scrollbar::SurfaceCorners,
    /// Whether the content overflowed on the last layout, so the bar's
    /// lane is reserved: vertical (trailing edge) and horizontal (bottom).
    /// Content that fits keeps the full width.
    reserve_vertical: bool,
    /// The lanes (vertical, horizontal) this layout found the content
    /// overflowing without: kept until the layout is applied, so content
    /// that fits only beside its own bar does not drop the bar and overflow
    /// again, pass after pass. A retained tree is laid out again without
    /// being rebuilt, so applying the layout is what ends it.
    lanes_needed: (bool, bool),
    /// The scroll view's own size when `lanes_needed` was seen. A pass that
    /// sizes the view differently (a parent measuring its content on the
    /// way to its own size) says nothing about the box the view gets, so
    /// what it needed is forgotten.
    lanes_needed_in: Option<(f32, f32)>,
    /// Keep the themed trailing lane even when vertical content fits.
    stable_vertical_gutter: bool,
    reserve_horizontal: bool,
    /// See [`Scrollable::with_edge_fade`].
    edge_fade: bool,
    /// Reports the trailing lane currently reserved, for a pinned footer
    /// that shares this scroller's measure.
    gutter_probe: Option<Rc<Cell<f32>>>,
    /// Reserved bottom lane that is excluded from both painting and scroll
    /// metrics. Sticky-footer dialogs use this to end overflowing prose on a
    /// clean boundary instead of clipping half a text baseline against the
    /// footer.
    bottom_content_inset: f32,
    /// Additional bottom lane used only when the resolved viewport is this
    /// compact or shorter. Long-form setup pages use it to end their initial
    /// frame between blocks without wasting space on a normal-height dialog.
    compact_bottom_content_inset: Option<(f32, f32)>,
    /// Stable semantic key for the scroll group. Without one the group's id
    /// follows its layout node, so no control inside it has a durable
    /// identity and none is published as an action resource.
    semantic_key: Option<String>,
    /// What the frame being drawn shows of the content, and what the last
    /// frame the host presented showed: a scroll between the two that moved
    /// nothing else is offered to the host as a copy
    /// ([`crate::scroll_copy::offer`]).
    shown: crate::scroll_copy::ShownFrames<Shown>,
}

/// Where a frame shows the content: its offset, the clip it paints into,
/// the transform it is drawn with, and what paints over the content there
/// without moving with it (the bars' tracks, the edge fades' bands).
#[derive(Clone, Debug, PartialEq)]
struct Shown {
    offset: (f64, f64),
    clip: Rect,
    transform: Affine,
    overlays: Vec<Rect>,
}

impl crate::scroll_copy::Shown for Shown {
    fn scroll(&self) -> vello::kurbo::Vec2 {
        vello::kurbo::Vec2::new(self.offset.0, self.offset.1)
    }

    fn clip(&self) -> Rect {
        self.clip
    }

    fn transform(&self) -> Affine {
        self.transform
    }

    /// The content's own widgets say what changed in it.
    fn same_view(&self, _other: &Self) -> bool {
        true
    }

    fn overlays(&self, out: &mut Vec<Rect>) {
        out.extend(&self.overlays);
    }
}

impl Scrollable {
    /// Box this widget in the SDK so thin apps reuse its shared implementation.
    /// This keeps the widget's vtable instantiation out of the app.
    #[must_use]
    pub fn into_widget(self) -> Box<dyn crate::widget::Widget> {
        Box::new(self)
    }

    pub fn new(axis: ScrollAxis) -> Self {
        Self {
            axis,
            reveal_focus: false,
            last_focus_path: None,
            offset_x: 0.0,
            offset_y: 0.0,
            content_w: 0.0,
            content_h: 0.0,
            walked_h: 0.0,
            viewport_w: 0.0,
            viewport_h: 0.0,
            glide: crate::virtual_viewport::ScrollGlide::new(),
            scroll_to: crate::motion::ScrollTo::new(),
            device_scale: 1.0,
            motion: crate::tokens::EffectiveMotion::default(),
            fade_drawn: Cell::new(1.0),
            fade_shown: 1.0,
            fixed_height: None,
            max_height: None,
            fill_parent_height: false,
            content_fills_viewport: false,
            child: None,
            child_node: None,
            rect: Rect::ZERO,
            scroll_dirty: false,
            height_limit_dirty: false,
            show_scrollbar: true,
            sequential_focus: true,
            last_cursor: Point::ZERO,
            cursor_initialized: false,
            vbar: Scrollbar::vertical(),
            hbar: Scrollbar::horizontal(),
            overlay: true,
            last_activity: None,
            pointer_near_bar: false,
            was_overflowing: false,
            focused: false,
            hold: None,
            bar_dirty: false,
            bar_visible_last_draw: Cell::new(false),
            terminal_fade_damage: Cell::new(false),
            reduce_motion_active: false,
            kinetic: crate::virtual_viewport::KineticSampler::new(),
            band: crate::motion::RubberBand::new(),
            persisted_offset: None,
            content_gutter: 0.0,
            bar_thickness: BAR_THICKNESS,
            bar_margin: BAR_MARGIN,
            corners: crate::widget::scrollbar::SurfaceCorners::default(),
            reserve_vertical: false,
            lanes_needed: (false, false),
            lanes_needed_in: None,
            stable_vertical_gutter: false,
            reserve_horizontal: false,
            edge_fade: false,
            gutter_probe: None,
            bottom_content_inset: 0.0,
            compact_bottom_content_inset: None,
            semantic_key: None,
            shown: crate::scroll_copy::ShownFrames::default(),
        }
    }
    pub fn new_vertical() -> Self {
        Self::new(ScrollAxis::Vertical)
    }
    pub fn new_horizontal() -> Self {
        Self::new(ScrollAxis::Horizontal)
    }
    #[must_use]
    pub fn with_child(mut self, c: Element) -> Self {
        self.child = Some(if self.content_fills_viewport {
            Box::new(ViewportContent::new(c))
        } else {
            c
        });
        self
    }
    /// Let auto-height content fill at least the measured viewport height,
    /// retaining its intrinsic height when it overflows. This does not change
    /// an explicitly sized child's authored height. Ordinary form fields stay
    /// content-sized unless their Scrollable opts in.
    ///
    /// Adds a presentational single-child wrapper to normal widget traversal;
    /// the content keeps its identity, focus and action bindings. Where the
    /// scroll view's height comes from its content rather than its parent,
    /// there is no viewport to fill and the content keeps its own height.
    #[must_use]
    pub fn with_content_min_viewport_height(mut self) -> Self {
        if !self.content_fills_viewport {
            if let Some(child) = self.child.take() {
                self.child = Some(Box::new(ViewportContent::new(child)));
            }
            self.content_fills_viewport = true;
        }
        self
    }

    /// Give the scroll group a stable semantic key, so the controls it
    /// scrolls keep durable identities and stay reachable as actions.
    #[must_use]
    pub fn with_semantic_key(mut self, key: impl Into<String>) -> Self {
        self.semantic_key = Some(key.into());
        self
    }

    /// The bar still auto-hides per-axis whenever content fits regardless
    /// of this flag; `false` suppresses it unconditionally.
    #[must_use]
    pub fn with_scrollbar(mut self, show: bool) -> Self {
        self.show_scrollbar = show;
        self
    }

    /// Keep the themed vertical scrollbar lane when content fits, so sibling
    /// pages use the same content width across an overflow transition. The
    /// scrollbar itself still appears only when scrolling is possible.
    #[must_use]
    pub fn with_stable_vertical_gutter(mut self) -> Self {
        self.stable_vertical_gutter = true;
        self
    }

    /// Keep scrolling, pointer bars, wheel input and descendant focus intact,
    /// but skip the viewport itself in sequential Tab traversal.
    #[must_use]
    pub fn without_sequential_focus(mut self) -> Self {
        self.sequential_focus = false;
        self
    }

    #[must_use]
    pub fn always_visible(mut self) -> Self {
        self.overlay = false;
        self
    }

    #[must_use]
    pub fn with_height(mut self, px: f32) -> Self {
        self.fixed_height = Some(px);
        self
    }

    /// Keep the viewport only as tall as its content up to `px`. Overflowing
    /// content remains scrollable, while a short page keeps an intrinsic,
    /// compact surface rather than a large empty pane.
    #[must_use]
    pub fn with_max_height(mut self, px: f32) -> Self {
        self.max_height = Some(px.max(0.0));
        self
    }

    /// Keep the viewport as tall as its content and let it shrink only when
    /// its parent runs out of room, so the content scrolls just in a window
    /// too short to show it. `with_max_height` without a number.
    #[must_use]
    pub fn fit_content_height(mut self) -> Self {
        self.max_height = Some(f32::INFINITY);
        self
    }

    /// Resize a content-sized viewport without replacing its child/editor.
    /// The shared measured solver updates the retained node's constraint.
    pub fn set_max_height(&mut self, px: f32) {
        let height = Some(px.max(0.0));
        if self.max_height != height {
            self.max_height = height;
            self.height_limit_dirty = true;
            self.scroll_dirty = true;
        }
    }

    /// Take the height the parent leaves: the rest of a column under a
    /// fixed bar, the whole of a grid row or a fixed-height container,
    /// including at the root of a window whose content has a max width.
    #[must_use]
    pub fn fill_parent_height(mut self) -> Self {
        self.fill_parent_height = true;
        self
    }

    /// Fade the content out at an edge while more of it can scroll in from
    /// there (`ui::surface::content_edge_fade` against the ground it is
    /// drawn on), so a cut-off line reads as "more below" rather than as
    /// clipped. Nothing fades at an edge the content has reached.
    #[must_use]
    pub fn with_edge_fade(mut self) -> Self {
        self.edge_fade = true;
        self
    }

    /// [`Self::with_cursor`] on a scroller that is already built.
    pub(crate) fn set_cursor(&mut self, cursor: Point) {
        self.last_cursor = cursor;
    }

    /// [`Self::with_semantic_key`] on a scroller that is already built.
    pub(crate) fn set_semantic_key(&mut self, key: impl Into<String>) {
        self.semantic_key = Some(key.into());
    }

    /// [`Self::with_persisted_offset`] on a scroller that is already built.
    pub(crate) fn set_persisted_offset(&mut self, cell: Rc<Cell<(f32, f32)>>) {
        let (x, y) = cell.get();
        self.offset_x = x;
        self.offset_y = y;
        self.persisted_offset = Some(cell);
    }

    /// Share the trailing lane this scroller reserves for its bar (zero
    /// while the content fits) with a sibling laid out to the same measure.
    pub(crate) fn with_gutter_probe(mut self, probe: Rc<Cell<f32>>) -> Self {
        self.gutter_probe = Some(probe);
        self
    }

    /// The seeded offset is clamped to the content and viewport the first
    /// layout resolves, before the content is placed, so a restored offset
    /// past the end of shorter content never shows a blank frame.
    #[must_use]
    pub fn with_persisted_offset(mut self, cell: Rc<Cell<(f32, f32)>>) -> Self {
        let (x, y) = cell.get();
        self.offset_x = x;
        self.offset_y = y;
        self.persisted_offset = Some(cell);
        self
    }

    /// Seed the most recent pointer position when rebuilding a retained view.
    ///
    /// Mouse wheel and button events do not carry a position.  A parent that
    /// recreates its scrollable every frame must therefore carry its last
    /// cursor into the replacement, otherwise a still click or wheel after a
    /// rebuild is incorrectly treated as being outside the viewport.
    #[must_use]
    pub fn with_cursor(mut self, cursor: Point) -> Self {
        self.last_cursor = cursor;
        self
    }

    /// Reserve a clean bottom edge inside the viewport for a sticky-footer
    /// separation. Content is clipped above this lane and the effective
    /// viewport used by scrolling shrinks by the same amount, so the final
    /// content remains reachable.
    #[must_use]
    pub fn with_bottom_content_inset(mut self, px: f32) -> Self {
        self.bottom_content_inset = px.max(0.0);
        self
    }

    /// Reserve a larger clean bottom edge only for a compact resolved
    /// viewport. This is evaluated after layout, so callers need not guess
    /// how a max-height dialog will shrink on a short display.
    #[must_use]
    pub fn with_compact_bottom_content_inset(mut self, max_viewport_height: f32, px: f32) -> Self {
        self.compact_bottom_content_inset = Some((max_viewport_height.max(0.0), px.max(0.0)));
        self
    }

    fn active_bottom_content_inset(&self) -> f32 {
        if self.content_h > self.viewport_h + FIT_EPS {
            self.compact_bottom_content_inset.map_or(
                self.bottom_content_inset,
                |(max_height, compact)| {
                    if self.viewport_h <= max_height {
                        self.bottom_content_inset.max(compact)
                    } else {
                        self.bottom_content_inset
                    }
                },
            )
        } else {
            0.0
        }
    }

    fn effective_viewport_h(&self) -> f32 {
        (self.viewport_h - self.active_bottom_content_inset()).max(0.0)
    }

    /// The actual interactive/paint viewport after any active sticky-footer
    /// separation has been reserved.
    pub fn content_viewport_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            (self.rect.y1 - f64::from(self.active_bottom_content_inset())).max(self.rect.y0),
        )
    }

    /// What the content is clipped to when it paints: the viewport, grown
    /// sideways by `row_bleed` where the viewport does not scroll that way.
    /// A row's hover and selection lift stands that far outside the content
    /// edge by design, so a list flush with a vertical scroller's edges
    /// keeps its rounded lift instead of a band cut square at the edge.
    /// Hit-testing and culling stay on [`Self::content_viewport_rect`].
    fn paint_clip_rect(&self, theme: &Theme) -> Rect {
        let viewport = self.content_viewport_rect();
        if matches!(self.axis, ScrollAxis::Vertical) {
            viewport.inflate(crate::chrome::row_bleed(theme), 0.0)
        } else {
            viewport
        }
    }

    pub fn set_content_size(&mut self, w: f32, h: f32) {
        self.content_w = w;
        self.content_h = h;
        self.clamp();
        self.cursor_initialized = false;
    }
    pub fn set_viewport_size(&mut self, w: f32, h: f32) {
        self.viewport_w = w;
        self.viewport_h = h;
        self.clamp();
        self.cursor_initialized = false;
    }
    pub fn offset_x(&self) -> f32 {
        self.offset_x
    }
    pub fn offset_y(&self) -> f32 {
        self.offset_y
    }

    pub fn max_offset_y(&self) -> f32 {
        (self.content_h - self.effective_viewport_h()).max(0.0)
    }

    pub fn set_offset_y(&mut self, y: f32) {
        self.offset_y = y;
        self.clamp();
        self.cursor_initialized = false;
    }

    pub fn vertical_overflows(&self) -> bool {
        self.content_h > self.effective_viewport_h() + FIT_EPS
    }

    /// For a manually-driven (child-less) Scrollable, when the caller paints
    /// content itself rather than through this widget's `draw`. Pair with
    /// [`Scrollable::set_content_size`].
    pub fn set_viewport_rect(&mut self, rect: Rect) {
        self.rect = rect;
        self.set_viewport_size(
            f32_of_f64(rect.width().max(0.0)),
            f32_of_f64(rect.height().max(0.0)),
        );
    }

    pub fn draw_bar_overlay(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        self.draw_scrollbars(scene, transform, theme);
    }
    pub fn momentum_speed(&self) -> f32 {
        let (vx, vy) = self.glide.velocity();
        f32_of_f64(vx.hypot(vy))
    }

    /// Move the content by `dx`, `dy`: the hand (a wheel, a key, a bar
    /// drag) or the caller takes over from any jump in progress.
    pub fn scroll_by(&mut self, dx: f32, dy: f32) {
        self.scroll_to.take_over(crate::motion::frame_now());
        self.move_by(dx, dy);
    }

    fn move_by(&mut self, dx: f32, dy: f32) {
        let (ox, oy) = (self.offset_x, self.offset_y);
        match self.axis {
            ScrollAxis::Vertical => {
                self.offset_y += dy;
            }
            ScrollAxis::Horizontal => {
                self.offset_x += dx;
            }
            ScrollAxis::Both => {
                self.offset_x += dx;
                self.offset_y += dy;
            }
        }
        self.clamp();
        // Only signal dirty if the clamp didn't eat the entire delta — at
        // the content edges offset_y doesn't actually change. Bitwise
        // comparison is intentional: this asks "did clamp change the exact
        // stored value at all", not "are these two offsets close".
        if self.offset_x.to_bits() != ox.to_bits() || self.offset_y.to_bits() != oy.to_bits() {
            self.scroll_dirty = true;
            self.cursor_initialized = false;
            self.wake();
        }
    }

    /// The offset the content is drawn at: where it is scrolled, and past
    /// an end by the give fingers pull it there with.
    fn drawn_offset(&self) -> (f64, f64) {
        let give = self
            .band
            .give(crate::motion::frame_now(), self.device_scale);
        (
            f64::from(self.offset_x) + give.x,
            f64::from(self.offset_y) + give.y,
        )
    }

    /// Where the content's origin lands on screen, as drawn.
    fn content_origin(&self) -> Point {
        let (x, y) = self.drawn_offset();
        Point::new(self.rect.x0 - x, self.rect.y0 - y)
    }

    /// Whether the give moved since the content was placed with it.
    fn give_unplaced(&self) -> bool {
        self.band
            .needs_frame(crate::motion::frame_now(), self.device_scale)
    }

    fn wake(&mut self) {
        self.last_activity = Some(crate::time::now());
        self.bar_dirty = true;
    }
    /// Glide on at `vx`, `vy` (logical pixels per second, in content
    /// space); zero stops a glide. Only along the axes this view scrolls:
    /// a flick that wavers sideways in a list that scrolls up and down
    /// glides up or down alone.
    pub fn fling(&mut self, vx: f32, vy: f32) {
        let vx = if self.axis_has_horizontal() { vx } else { 0.0 };
        let vy = if self.axis_has_vertical() { vy } else { 0.0 };
        self.glide.set_device_scale(self.device_scale);
        self.glide.fling(f64::from(vx), f64::from(vy));
    }

    /// Advance a glide to `now` (the frame's time): by the time since the
    /// last step, or since the fling began — never by an idle gap before it.
    pub fn advance_momentum(&mut self, now: crate::time::Instant) {
        let (dx, dy) = self.glide.advance(now);
        self.glided(dx, dy);
    }

    /// Advance a glide by `dt` seconds ([`crate::virtual_viewport::glide_step`]).
    pub fn tick(&mut self, dt: f32) {
        let elapsed = Duration::from_secs_f64(f64::from(dt.max(0.0)));
        let (dx, dy) = self.glide.advance_by(elapsed);
        self.glided(dx, dy);
    }

    /// Move by a glide's step; an axis the content cannot move further
    /// along stops, and the other glides on. Each axis is judged alone: in
    /// a short frame one axis's step can round to nothing while the
    /// other's moves a pixel, and that is no end.
    fn glided(&mut self, dx: f64, dy: f64) {
        let before = (self.offset_x, self.offset_y);
        self.move_by(f32_of_f64(dx), f32_of_f64(dy));
        if dx != 0.0 && self.offset_x.to_bits() == before.0.to_bits() {
            self.glide.stop_x();
        }
        if dy != 0.0 && self.offset_y.to_bits() == before.1.to_bits() {
            self.glide.stop_y();
        }
    }

    /// Follow a jump in progress ([`Self::scroll_into_view`]) to where it is
    /// drawn at `now`.
    fn follow_scroll_to(&mut self, now: Instant) {
        let Some(at) = self
            .scroll_to
            .offset_in_device_pixels(now, self.device_scale)
        else {
            return;
        };
        self.move_by(
            f32_of_f64(at.x) - self.offset_x,
            f32_of_f64(at.y) - self.offset_y,
        );
        let _ = self.scroll_to.arrived(now);
    }

    /// A jump still moves, or has arrived and waits for the frame that
    /// puts the content at its target.
    fn jump_in_progress(&self) -> bool {
        self.scroll_to.target().is_some()
            || self.scroll_to.is_moving(crate::motion::frame_now())
            || self.fade_drawn.get() < 1.0
    }

    /// The content fades in after a long jump, or the last frame drew it
    /// still fading: its whole viewport repaints.
    fn content_fading(&self) -> bool {
        self.fade_drawn.get() < 1.0
            || self.fade_shown < 1.0
            || self.scroll_to.opacity(crate::motion::frame_now()) < 1.0
    }
    /// Reveal newly focused controls while allowing manual scroll away from
    /// unchanged focus. Opt-in for form panels; ordinary scrolling is unchanged.
    #[must_use]
    pub fn with_focus_reveal(mut self) -> Self {
        self.reveal_focus = true;
        self
    }

    /// Show the content rect at `x`, `y` (`w` by `h`, in content
    /// coordinates), moving as little as it takes. A jump the person's hand
    /// or key just asked for moves there ([`crate::motion::ScrollTo`]): it
    /// glides when short and fades in at the target when long; one nobody
    /// asked for (a sync, a restored view) or an agent's lands at once.
    pub fn scroll_into_view(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let now = crate::motion::frame_now();
        let (tx, ty) = self.offset_showing(x, y, w, h);
        let viewport = vello::kurbo::Vec2::new(
            f64::from(self.viewport_w),
            f64::from(self.effective_viewport_h()),
        );
        let drawn = vello::kurbo::Vec2::new(f64::from(self.offset_x), f64::from(self.offset_y));
        let target = vello::kurbo::Vec2::new(f64::from(tx), f64::from(ty));
        self.glide.stop();
        if self.scroll_to.go(drawn, target, viewport, self.motion, now)
            == crate::motion::ScrollJump::Glide
        {
            self.follow_scroll_to(now);
        } else {
            self.move_by(tx - self.offset_x, ty - self.offset_y);
        }
        if self.scroll_to.is_moving(now) {
            self.scroll_dirty = true;
        }
    }

    /// [`Self::scroll_into_view`] that lands at once: keyboard focus moving
    /// to a control out of view while tabbing.
    pub fn scroll_into_view_at_once(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let (tx, ty) = self.offset_showing(x, y, w, h);
        self.glide.stop();
        self.scroll_to
            .land(vello::kurbo::Vec2::new(f64::from(tx), f64::from(ty)));
        self.move_by(tx - self.offset_x, ty - self.offset_y);
    }

    /// The offset that shows the content rect at `x`, `y`, `w` by `h`,
    /// moving the least from where the content is going.
    fn offset_showing(&self, x: f32, y: f32, w: f32, h: f32) -> (f32, f32) {
        let (mut ox, mut oy) = self
            .scroll_to
            .target()
            .map_or((self.offset_x, self.offset_y), |target| {
                (f32_of_f64(target.x), f32_of_f64(target.y))
            });
        if y < oy {
            oy = y;
        } else if y + h > oy + self.effective_viewport_h() {
            oy = (y + h - self.effective_viewport_h()).max(0.0);
        }
        if x < ox {
            ox = x;
        } else if x + w > ox + self.viewport_w {
            ox = (x + w - self.viewport_w).max(0.0);
        }
        let max_x = (self.content_w - self.viewport_w).max(0.0);
        (ox.clamp(0.0, max_x), oy.clamp(0.0, self.max_offset_y()))
    }
    /// Reserve the bar's lane on the edge it scrolls along while the solved
    /// content overflows the viewport, and give it back once it fits.
    /// Narrower content is usually taller, not always: a grid whose cells
    /// scale with the width can overflow at full width and fit beside the
    /// lane. A lane the content needed then stays for the rest of the layout,
    /// so this settles within two steps.
    fn follow_overflow(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        layout: &taffy::Layout,
    ) -> bool {
        let Some(child) = self.child_node else {
            return false;
        };
        let Ok(content) = taffy.layout(child).copied() else {
            return false;
        };
        let viewport_w = layout.size.width - layout.padding.right;
        let viewport_h = layout.size.height - layout.padding.bottom;
        let content_w = content.size.width.max(content.content_size.width);
        let content_h = content.size.height.max(content.content_size.height);
        let vertical = self.axis_has_vertical()
            && self.show_scrollbar
            && viewport_h > 0.0
            && (self.stable_vertical_gutter
                || content_h > viewport_h - self.active_bottom_content_inset() + FIT_EPS);
        let horizontal = self.axis_has_horizontal()
            && self.show_scrollbar
            && viewport_w > 0.0
            && content_w > viewport_w + FIT_EPS;
        // A lane the content overflowed without stays: content that fits
        // only because the lane narrowed it would overflow again without it.
        // Only in a box of the same size: one the view was measured at on
        // the way to its own size needed nothing of the box it gets.
        let size = (layout.size.width, layout.size.height);
        if self.lanes_needed_in != Some(size) {
            self.lanes_needed = (false, false);
            self.lanes_needed_in = Some(size);
        }
        if vertical && !self.reserve_vertical {
            self.lanes_needed.0 = true;
        }
        if horizontal && !self.reserve_horizontal {
            self.lanes_needed.1 = true;
        }
        let vertical = vertical || (self.reserve_vertical && self.lanes_needed.0);
        let horizontal = horizontal || (self.reserve_horizontal && self.lanes_needed.1);
        if vertical == self.reserve_vertical && horizontal == self.reserve_horizontal {
            return false;
        }
        self.reserve_vertical = vertical;
        self.reserve_horizontal = horizontal;
        if let Some(probe) = &self.gutter_probe {
            probe.set(if vertical { self.content_gutter } else { 0.0 });
        }
        let Ok(current) = taffy.style(node) else {
            return false;
        };
        let mut style = current.clone();
        style.padding.right =
            taffy::LengthPercentage::length(if vertical { self.content_gutter } else { 0.0 });
        style.padding.bottom =
            taffy::LengthPercentage::length(if horizontal { self.content_gutter } else { 0.0 });
        taffy.set_style(node, style).is_ok()
    }

    fn clamp(&mut self) {
        let max_x = (self.content_w - self.viewport_w).max(0.0);
        let max_y = self.max_offset_y();
        self.offset_x = self.offset_x.clamp(0.0, max_x);
        self.offset_y = self.offset_y.clamp(0.0, max_y);
        // Single write-back point: every offset mutation funnels through here.
        if let Some(cell) = &self.persisted_offset {
            cell.set((self.offset_x, self.offset_y));
        }
    }

    fn axis_has_vertical(&self) -> bool {
        matches!(self.axis, ScrollAxis::Vertical | ScrollAxis::Both)
    }
    fn axis_has_horizontal(&self) -> bool {
        matches!(self.axis, ScrollAxis::Horizontal | ScrollAxis::Both)
    }
}

/// A presentational Grid surface at least as tall as the scroll view's
/// viewport. Container supplies the ordinary single-child lifecycle; we
/// register its layout node as this wrapper's own node. The child style is
/// never rewritten.
///
/// The minimum is a percentage of the viewport, so it resolves only where
/// the scroll view's height is given to it. It was the scroll view's last
/// solved height, measured between passes: where an ancestor took its height
/// from content, that height became the content's floor, and with the scroll
/// view and this wrapper adjusting a pass apart the two traded heights
/// without settling.
struct ViewportContent {
    inner: crate::widget::Container,
}

impl ViewportContent {
    fn new(child: Element) -> Self {
        Self {
            inner: crate::widget::Container::new()
                .with_no_padding()
                .with_child(child),
        }
    }
}

impl crate::widget::Decorator for ViewportContent {
    type Inner = crate::widget::Container;

    fn inner(&self) -> &crate::widget::Container {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut crate::widget::Container {
        &mut self.inner
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use taffy::style_helpers::{fr, length, min_content, minmax};
        let node = self.inner.build_layout(taffy, theme, nodes);
        let _ = taffy.set_style(
            node,
            Style {
                display: taffy::Display::Grid,
                // The flexible track fills short content; its intrinsic minimum
                // keeps a long measured editor taller than the viewport.
                grid_template_rows: vec![minmax(min_content(), fr(1.0_f32))],
                grid_template_columns: vec![minmax(length(0.0_f32), fr(1.0_f32))],
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::auto(),
                },
                min_size: Size {
                    width: Dimension::length(0.0),
                    height: Dimension::percent(1.0),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
        );
        crate::widget::decorator::register_node(nodes, node, self);
        node
    }

    fn name(&self) -> &'static str {
        "ScrollableContent"
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.inner
            .for_each_child(&mut |child| child.collect_damage(regions));
    }
}

impl Widget for Scrollable {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn primary_vertical_scroll(&self) -> Option<crate::widget::PrimaryScrollRegion> {
        Some(crate::widget::PrimaryScrollRegion {
            key: "primary",
            owner_generation: None,
            metrics: self.v_metrics(),
        })
    }
    fn apply_primary_vertical_scroll(
        &mut self,
        action: crate::widget::PrimaryScrollAction,
    ) -> crate::widget::PrimaryScrollResult {
        action.apply_to(self, Self::primary_vertical_scroll, |s, _, next| {
            s.set_offset_y(next as f32);
        })
    }

    fn child_clip_rect(&self) -> Option<Rect> {
        Some(self.content_viewport_rect())
    }
    fn sync(&mut self, s: &AppModel, p: &ModelPath) {
        if let Some(c) = &mut self.child {
            c.sync(s, p);
        }
    }

    /// A glide in progress advances here, once per frame, and moves only
    /// this view: like a wheel that only pans
    /// ([`crate::widget::note_view_only_pan`]) it is no change of the model,
    /// so it does not report one; reporting it synced the app and
    /// republished its resources on every frame of every fling. The host
    /// places the content where the glide took it
    /// ([`crate::widget::note_placement_moved`]).
    fn update(&mut self, s: &AppModel, p: &ModelPath) -> bool {
        if self.glide.is_active() {
            self.advance_momentum(crate::time::now());
        }
        // The give springing back moves the content like a glide does.
        if self.give_unplaced() {
            self.scroll_dirty = true;
        }
        // A jump in progress, like a glide, moves only this view.
        self.follow_scroll_to(crate::motion::frame_now());
        // Click-and-hold auto-repeat: the host wakes on the repeat's
        // deadline (`next_wake_deadline`), not on animation frames.
        let held = self.hold.is_some() && {
            let before = (self.offset_x, self.offset_y);
            self.advance_hold(crate::time::now());
            (self.offset_x, self.offset_y) != before
        };
        if self.scroll_dirty {
            crate::widget::note_placement_moved();
        }
        let child = self.child.as_mut().is_some_and(|c| c.update(s, p));
        held || child
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.height_limit_dirty = false;
        self.lanes_needed = (false, false);
        self.child_node = self
            .child
            .as_mut()
            .map(|c| c.build_layout(taffy, theme, node_map));
        // Content that only scrolls sideways is as wide as it wants to be
        // (its max-content width), not squeezed into the viewport.
        if matches!(self.axis, ScrollAxis::Horizontal)
            && let Some(child) = self.child_node
            && let Ok(current) = taffy.style(child)
        {
            let mut style = current.clone();
            style.size.width = Dimension::auto();
            style.flex_shrink = 0.0;
            let _ = taffy.set_style(child, style);
        }
        let height = match self.fixed_height {
            Some(px) => Dimension::length(px),
            None if self.fill_parent_height => Dimension::auto(),
            None if self.max_height.is_some() => Dimension::auto(),
            None => Dimension::percent(1.0),
        };
        let metrics = &theme.tokens.metrics;
        self.bar_thickness = f64::from(metrics.scroll_thumb);
        self.bar_margin =
            f64::from((metrics.scrollbar_gutter - metrics.scroll_thumb).max(0.0) / 2.0);
        // A scroll view keeps `metrics.scrollbar_gutter` free on its trailing
        // edge (and its bottom edge for a horizontal bar) while its content
        // overflows, so its bar never covers content; content that fits
        // keeps the full width. `adjust_layout_style` follows the overflow.
        let gutter = if self.show_scrollbar {
            metrics.scrollbar_gutter
        } else {
            0.0
        };
        self.content_gutter = gutter;
        let pad_right =
            taffy::LengthPercentage::length(if self.reserve_vertical && self.axis_has_vertical() {
                gutter
            } else {
                0.0
            });
        let pad_bottom = taffy::LengthPercentage::length(
            if self.reserve_horizontal && self.axis_has_horizontal() {
                gutter
            } else {
                0.0
            },
        );
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height,
            },
            // Growing into the parent's room never takes a content-sized
            // minimum, or a long page would push its parent taller.
            min_size: Size {
                width: Dimension::auto(),
                height: if self.fill_parent_height {
                    Dimension::length(0.0)
                } else {
                    Dimension::auto()
                },
            },
            flex_basis: if self.fill_parent_height {
                Dimension::length(0.0)
            } else {
                Dimension::auto()
            },
            align_self: self.fill_parent_height.then_some(taffy::AlignSelf::Stretch),
            // Vertical content must keep its intrinsic height so overflow is
            // measurable. Scope the flex-row cross-axis opt-out to this
            // parent; child `align_self` remains available to normal Row and
            // other manual compositions. Horizontal-only content retains the
            // default cross-axis stretch to the viewport height.
            align_items: self
                .axis_has_vertical()
                .then_some(taffy::AlignItems::FlexStart),
            max_size: Size {
                width: Dimension::auto(),
                height: self
                    .max_height
                    .filter(|px| px.is_finite())
                    .map_or(Dimension::auto(), Dimension::length),
            },
            flex_grow: if self.fill_parent_height { 1.0 } else { 0.0 },
            overflow: taffy::Point {
                x: taffy::Overflow::Hidden,
                y: taffy::Overflow::Hidden,
            },
            padding: taffy::Rect {
                left: taffy::LengthPercentage::length(0.0),
                right: pad_right,
                top: taffy::LengthPercentage::length(0.0),
                bottom: pad_bottom,
            },
            ..Default::default()
        };
        let node = match self.child_node {
            Some(c) => taffy.parent_node(style, &[c]),
            None => taffy.leaf_node(style),
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        _text: &mut TextRenderer,
    ) -> bool {
        let mut changed = self.height_limit_dirty;
        if changed {
            let Ok(mut style) = taffy.style(node).cloned() else {
                return false;
            };
            style.max_size.height = self.max_height.map_or(Dimension::auto(), Dimension::length);
            style.size.height = match self.fixed_height {
                Some(px) => Dimension::length(px),
                None if self.fill_parent_height => Dimension::length(0.0),
                None => Dimension::auto(),
            };
            if taffy.set_style(node, style).is_err() {
                return false;
            }
            self.height_limit_dirty = false;
        }
        let Ok(layout) = taffy.layout(node).copied() else {
            return changed;
        };
        changed |= self.follow_overflow(taffy, node, &layout);
        changed
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // Geometry may have moved beneath a stationary pointer. The next
        // same-position event must therefore be routed once again.
        self.cursor_initialized = false;
        // This layout is over: the next one checks the lanes afresh, so
        // content that now fits at full width gives its bar back.
        self.lanes_needed = (false, false);
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = crate::widget::placed(&l, origin);
        self.viewport_w = l.size.width;
        self.viewport_h = l.size.height;
        // Measure the solved content, including descendant overflow, before
        // clamping. Clamping against only taffy's estimate can repeatedly lose
        // the last pixel when rounded child edges extend beyond that estimate.
        if let Some(child_node) = self.child_node {
            let cl = taffy.layout(child_node).copied().unwrap_or_default();
            self.content_w = cl.content_size.width.max(cl.size.width);
            self.content_h = cl.content_size.height.max(cl.size.height);
        }
        let previous_offset = (self.offset_x, self.offset_y);
        let placing_again = crate::widget::placing_again(taffy);
        // Content placed below knows what this view shows of it
        // (`widget::layout_clip`), so a long collection lays out only that.
        let clip = self.content_viewport_rect();
        let origin = self.content_origin();
        if let Some(c) = &mut self.child {
            if let Some(child_node) = self.child_node {
                crate::widget::with_layout_clip(clip, || {
                    c.apply_layout(taffy, child_node, origin);
                });
                let cl = taffy.layout(child_node).copied().unwrap_or_default();
                if c.child_clip_rect().is_some() {
                    self.content_w = cl.size.width;
                    self.content_h = cl.size.height;
                } else {
                    self.content_w = cl.content_size.width.max(cl.size.width);
                    self.content_h = cl.content_size.height.max(cl.size.height);
                }

                // taffy's content_size can still cap at the available block for
                // a flex column shorter than the literal sum of its children.
                // Walk the subtree and take the deeper of the two so scrolling
                // always reaches the real last pixel; placed again, the
                // content only moved and the walk would find the same.
                if !placing_again {
                    let max_y = super::layout::max_bottom_edge(c.as_ref());
                    self.walked_h = f32_of_f64((max_y - origin.y).max(0.0));
                }
                if self.walked_h > self.content_h {
                    self.content_h = self.walked_h;
                }
            }
        }
        self.clamp();
        let origin = self.content_origin();
        if previous_offset != (self.offset_x, self.offset_y)
            && let (Some(c), Some(child_node)) = (&mut self.child, self.child_node)
        {
            // A restored offset or a resize may need clamping. Publish
            // geometry at the final offset in this same layout pass.
            crate::widget::with_layout_clip(clip, || {
                c.apply_layout(taffy, child_node, origin);
            });
        }
        // Flash the overlay bar the first frame content overflows.
        // Placed again, focus is where it was and stands revealed.
        if self.reveal_focus && !placing_again {
            fn focused(
                widget: &dyn Widget,
                path: &mut Vec<usize>,
                viewport_height: f64,
            ) -> Option<(Vec<usize>, Rect)> {
                if widget.is_focusable() && widget.is_focused() {
                    return Some((path.clone(), widget.get_rect()));
                }
                for (index, child) in widget.children().into_iter().enumerate() {
                    path.push(index);
                    if let Some((path, rect)) = focused(child, path, viewport_height) {
                        let rect = widget
                            .focus_reveal_context()
                            .filter(|context| context.height() <= viewport_height)
                            .unwrap_or(rect);
                        return Some((path, rect));
                    }
                    path.pop();
                }
                None
            }
            let focus = self.child.as_ref().and_then(|child| {
                focused(
                    child.as_ref(),
                    &mut Vec::new(),
                    f64::from(self.effective_viewport_h()),
                )
            });
            let path = focus.as_ref().map(|(path, _)| path.clone());
            if path != self.last_focus_path {
                self.last_focus_path = path;
                if let Some((_, rect)) = focus {
                    self.scroll_into_view_at_once(
                        f32_of_f64(rect.x0 - self.rect.x0) + self.offset_x,
                        f32_of_f64(rect.y0 - self.rect.y0) + self.offset_y,
                        f32_of_f64(rect.width()),
                        f32_of_f64(rect.height()),
                    );
                    let clip = self.content_viewport_rect();
                    let origin = self.content_origin();
                    if let (Some(child), Some(node)) = (&mut self.child, self.child_node) {
                        crate::widget::with_layout_clip(clip, || {
                            child.apply_layout(taffy, node, origin);
                        });
                    }
                }
            }
        }
        let scrollable_now = self.vbar_active() || self.hbar_active();
        // Content that has just begun to overflow is something to scroll, not
        // something the person did: an overlay bar appears when they scroll
        // (or reach for it), never because a window was opened at a size its
        // content does not fit or because a resize made it overflow. A bar
        // that is always drawn is repainted so the thumb shows at once.
        if scrollable_now && !self.was_overflowing {
            self.bar_dirty = true;
            if !self.overlay && self.last_activity.is_none() {
                self.wake();
            }
        }
        self.was_overflowing = scrollable_now;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.reduce_motion_active = theme.reduce_motion;
        self.motion = theme.motion();
        let corners = crate::widget::scrollbar::SurfaceCorners::seen(theme, transform);
        if corners != self.corners {
            self.corners = corners;
            self.bar_dirty = true;
        }
        let clip = self.paint_clip_rect(theme);
        self.shown.note(Shown {
            offset: self.drawn_offset(),
            clip,
            transform: crate::scroll_copy::on_screen(transform),
            overlays: self.copy_overlays(clip),
        });
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &clip,
        );
        let previous_viewport = presence.push_viewport(self.content_viewport_rect());
        // After a long jump the content fades in where it landed.
        let opacity = self.scroll_to.opacity(crate::motion::frame_now());
        self.fade_drawn.set(opacity);
        let fading = opacity < 1.0;
        if fading {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                BlendMode::default(),
                opacity,
                transform,
                &clip,
            );
        }
        // Unless fading in, what a copy keeps is not drawn (`draw_cull`).
        let keep = self.shown.keep().filter(|_| !fading);
        if let Some(c) = &mut self.child {
            crate::draw_cull::keeping(scene, keep, |scene| {
                crate::widget::draw_child(c.as_mut(), scene, transform, tr, theme, presence);
            });
        }
        if fading {
            scene.pop_layer();
        }
        presence.pop_viewport(previous_viewport);
        scene.pop_layer();
        self.draw_edge_fades(scene, transform, theme);
        self.draw_scrollbars(scene, transform, theme);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "ordered pointer capture, child-first wheel dispatch, and clip routing share one event boundary"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                match self.on_cursor_moved(*position, ctx.scale_factor) {
                    CursorMoveOutcome::DragConsumed => {
                        *ctx.handled = true;
                        return;
                    }
                    // Only the scrollbar bookkeeping is coalesced. Descendants
                    // may have lost hover on CursorLeft, or changed their own
                    // interaction state without moving this pointer.
                    CursorMoveOutcome::Forward | CursorMoveOutcome::Coalesced => {}
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let cur = self.last_cursor;
                // Thumb press → drag; track press → page + hold.
                if self.on_track_or_thumb_press(cur) {
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let had_hold = self.hold.take().is_some();
                if self.vbar.release() || self.hbar.release() || had_hold {
                    self.scroll_dirty = true;
                    *ctx.handled = true;
                    return;
                }
            }
            // A non-nav key (guard true, `on_navigation_key` false) falls
            // through to the child below, same as an unfocused/released one.
            WindowEvent::KeyboardInput { event: ke, .. }
                if self.focused
                    && ke.state == ElementState::Pressed
                    && self.on_navigation_key(&ke.logical_key) =>
            {
                *ctx.handled = true;
                return;
            }
            _ => {}
        }
        if let WindowEvent::MouseWheel { delta, phase, .. } = event {
            if *ctx.handled || !self.content_viewport_rect().contains(self.last_cursor) {
                return;
            }
            // The deepest region under the pointer gets first refusal. At its
            // boundary it leaves the event unhandled so this viewport can move.
            if let Some(child) = &mut self.child
                && let Some(child_node) = self.child_node
            {
                child.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
                if *ctx.handled {
                    return;
                }
            }
            // Ctrl+scroll zooms (`crate::content_zoom`): what no child took
            // goes to the window, never into a scroll.
            if crate::content_zoom::is_zoom_scroll(event, ctx.modifiers) {
                return;
            }
            // Only consume when it moved the view, so an at-the-edge wheel
            // bubbles to an outer/sibling scroll region.
            *ctx.handled = self.on_wheel(
                delta,
                *phase,
                ctx.modifiers.state().shift_key(),
                ctx.scale_factor,
            );
            // Every child refused it and it moved only this view: nothing
            // the app publishes changed.
            if *ctx.handled {
                crate::widget::note_view_only_pan();
            }
            return;
        }

        // Clipping is an interaction boundary as well as a paint boundary.
        // In particular, a control translated beneath a sticky footer must
        // not retain hover or receive a press merely because its un-clipped
        // layout rect still contains the pointer. A child that already owns
        // pointer capture continues receiving motion/release so drag and
        // pressed-state cleanup remain correct.
        let child_has_capture = self
            .child
            .as_ref()
            .is_some_and(|child| child.wants_pointer_capture());
        let pointer_in_content = match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.content_viewport_rect().contains(ctx.logical(position))
            }
            WindowEvent::MouseInput { .. } => {
                self.content_viewport_rect().contains(self.last_cursor)
            }
            _ => true,
        };
        // A capturing child also gets presses outside: what it shows past
        // the clip (an open menu) is chosen or dismissed by them.
        let child_pointer_allowed = pointer_in_content
            || (child_has_capture
                && matches!(
                    event,
                    WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
                ));
        if !child_pointer_allowed {
            return;
        }
        if let Some(child) = &mut self.child
            && let Some(child_node) = self.child_node
        {
            child.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
        }
        // An already-focused editor can consume a press without requesting
        // focus again. Only use the pane as a fallback for an unhandled
        // background press; never displace the child's existing focus.
        if !*ctx.handled
            && ctx.focus_target.is_none()
            && pointer_in_content
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            )
        {
            *ctx.focus_target = Some(ctx.node_id);
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.child
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if let Some(child) = &self.child {
            visitor(child.as_ref());
        }
    }

    /// Its content under what the view shows of it: a long column below
    /// visits only the children near the viewport.
    fn for_each_child_in_clip(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if let Some(child) = &self.child {
            crate::widget::with_layout_clip(self.content_viewport_rect(), || {
                visitor(child.as_ref());
            });
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.child
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "Scrollable"
    }

    /// Non-overflowing wrappers stay out of the tab order.
    fn is_focusable(&self) -> bool {
        self.sequential_focus && self.show_scrollbar && (self.vbar_active() || self.hbar_active())
    }
    fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.wake();
        }
    }
    fn is_focused(&self) -> bool {
        self.focused
    }

    fn wants_pointer_capture(&self) -> bool {
        self.vbar.is_dragging()
            || self.hbar.is_dragging()
            || self.hold.is_some()
            || self
                .child
                .as_ref()
                .is_some_and(|c| c.wants_pointer_capture())
    }

    fn needs_animation_frame(&self) -> bool {
        self.glide.is_active()
            || self.give_unplaced()
            || self.jump_in_progress()
            || self.fade_in_progress()
            || self.needs_terminal_fade_frame()
            || self
                .child
                .as_ref()
                .is_some_and(|c| c.needs_animation_frame())
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.fade_start_deadline_at(crate::motion::frame_now())
            .into_iter()
            .chain(self.hold.map(|hold| hold.next_at))
            .chain(
                self.child
                    .as_ref()
                    .and_then(|child| child.next_wake_deadline()),
            )
            .min()
    }

    /// Wheel activity damages the viewport immediately. During the opaque
    /// hold the thumb stays unchanged; only the fade itself needs new damage.
    fn needs_redraw(&self) -> bool {
        self.scroll_dirty
            || self.content_fading()
            || self.bar_dirty
            || self.fade_in_progress()
            || self.needs_terminal_fade_frame()
            || self.child.as_ref().is_some_and(|c| c.needs_redraw())
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.content_fading() {
            regions.push(self.rect);
            return;
        }
        if self.scroll_dirty {
            match self.shown.compare(self, regions) {
                crate::scroll_copy::Since::Copied => {
                    // What changed in the content besides moving, where its
                    // widgets say; the host carries it with the copy.
                    let clip = self.shown.drawing().map_or(self.rect, |shown| shown.clip);
                    if let Some(child) = &self.child {
                        let first = regions.len();
                        crate::widget::with_layout_clip(self.content_viewport_rect(), || {
                            child.collect_damage(regions);
                        });
                        if let Some(changed) = regions.get_mut(first..) {
                            for rect in changed {
                                *rect = rect.intersect(clip);
                            }
                        }
                    }
                    return;
                }
                crate::scroll_copy::Since::Repaint => {
                    regions.push(self.rect);
                    return;
                }
                // Scrolled back where the screen shows it: only what changed.
                crate::scroll_copy::Since::Still => {}
            }
        }

        if let Some(child) = &self.child {
            let first = regions.len();
            crate::widget::with_layout_clip(self.content_viewport_rect(), || {
                child.collect_damage(regions);
            });
            if let Some(child_regions) = regions.get_mut(first..) {
                for rect in child_regions {
                    *rect = rect.intersect(self.rect);
                }
            }
        }
        if self.bar_dirty || self.fade_in_progress() || self.needs_terminal_fade_frame() {
            for bar in [self.vertical_track(), self.horizontal_track()]
                .into_iter()
                .flatten()
            {
                regions.push(bar);
            }
        }
    }

    /// A scroll demands `apply_layout` even though nothing changed size.
    fn is_layout_dirty(&self) -> bool {
        self.scroll_dirty || self.child.as_ref().is_some_and(|c| c.is_layout_dirty())
    }

    /// `scroll_dirty` alone must NOT force a full `compute_layout` — only a
    /// measure-dirty descendant does — or every scroll frame
    /// re-solves the whole content tree.
    fn layout_needs_measure(&self) -> bool {
        self.child
            .as_ref()
            .is_some_and(|c| c.layout_needs_measure())
    }

    fn mark_drawn(&mut self) {
        // Not drawn this frame (hidden, culled, or a cache above it reused),
        // what the screen shows where it lies is not known to be its content:
        // the next scroll repaints its viewport before copies resume.
        self.shown.present();
        self.fade_shown = self.fade_drawn.get();
        self.scroll_dirty = false;
        self.bar_dirty = false;
        self.terminal_fade_damage.set(false);
        if let Some(c) = &mut self.child {
            c.mark_drawn();
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn reveal_descendant(&mut self, target: *const dyn Widget) -> bool {
        let Some(child) = &mut self.child else {
            return false;
        };
        if !child.reveal_descendant(target) {
            return false;
        }
        let rect = child.get_rect();
        let viewport = self.content_viewport_rect();
        let x = crate::num::f32_of_f64(rect.x0 - viewport.x0 + f64::from(self.offset_x));
        let y = crate::num::f32_of_f64(rect.y0 - viewport.y0 + f64::from(self.offset_y));
        // Keep enough surrounding context to identify a form control after
        // keyboard traversal. Merely placing a focused field flush against
        // the viewport edge can hide its label and the adjacent action,
        // producing an apparently anonymous text box on compact setup pages.
        // The margin is capped so very small viewports still prioritise the
        // control itself.
        let context = (self.effective_viewport_h() * 0.12).clamp(16.0, 56.0);
        self.scroll_into_view_at_once(
            (x - context).max(0.0),
            (y - context).max(0.0),
            crate::num::f32_of_f64(rect.width()) + context * 2.0,
            crate::num::f32_of_f64(rect.height()) + context * 2.0,
        );
        true
    }

    /// The meaningful label belongs to whatever it wraps.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        let node = AccessNode::new(Role::Group);
        Some(match &self.semantic_key {
            Some(key) => node.with_semantic_key(key.clone()),
            None => node,
        })
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty())
    }

    /// The view adds no structure of its own: a change its content can
    /// describe (a long list mounting the rows a scroll brought near) stays
    /// described.
    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        self.child
            .as_mut()
            .map_or(crate::widget::StructureChange::None, |child| {
                child.take_structure_change()
            })
    }
}

impl crate::widget::controls::ActionControls for Scrollable {
    fn action_control_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        crate::widget::controls::children_action_control_bindings(self)
    }
    fn has_action_control_bindings(&self) -> bool {
        crate::widget::controls::children_have_action_control_bindings(self)
    }
}

#[cfg(test)]
#[path = "scrollable_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "scrollable_lane_tests.rs"]
mod lane_tests;
