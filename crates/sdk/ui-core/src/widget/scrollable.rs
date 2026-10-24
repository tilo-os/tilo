use crate::num::f32_of_f64;
use crate::time::{Duration, Instant};
use crate::widget::{AppModel, ModelPath};
use std::cell::Cell;
use std::rc::Rc;

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::scrollbar::{PressOutcome, ScrollMetrics, Scrollbar};
use crate::widget::{Element, Widget};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::BlendMode;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::keyboard::{Key, NamedKey};

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
    /// space); zero stops a glide.
    pub fn fling(&mut self, vx: f32, vy: f32) {
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

    /// Move by a glide's step; content that cannot move further stops it.
    fn glided(&mut self, dx: f64, dy: f64) {
        let before = (self.offset_x, self.offset_y);
        self.move_by(f32_of_f64(dx), f32_of_f64(dy));
        if (self.offset_x, self.offset_y) == before && (dx != 0.0 || dy != 0.0) {
            self.glide.stop();
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

    fn vbar_active(&self) -> bool {
        self.show_scrollbar
            && self.axis_has_vertical()
            && self.effective_viewport_h() > 0.0
            && self.content_h > self.effective_viewport_h() + FIT_EPS
    }
    fn hbar_active(&self) -> bool {
        self.show_scrollbar
            && self.axis_has_horizontal()
            && self.viewport_w > 0.0
            && self.content_w > self.viewport_w + FIT_EPS
    }

    /// Leaves room in the bottom-right corner when the horizontal bar is
    /// also active.
    fn vertical_track(&self) -> Option<Rect> {
        if !self.vbar_active() {
            return None;
        }
        let (thickness, margin) = (self.bar_thickness, self.bar_margin);
        let x1 = self.rect.x1 - margin;
        let x0 = x1 - thickness;
        let y0 = self.rect.y0 + margin;
        let mut y1 = self.content_viewport_rect().y1 - margin;
        if self.hbar_active() {
            y1 -= thickness + margin;
        }
        let track = self.corners.clear(Rect::new(x0, y0, x1, y1), true);
        if track.height() <= MIN_THUMB_LEN.min(1.0) {
            return None;
        }
        Some(track)
    }

    fn horizontal_track(&self) -> Option<Rect> {
        if !self.hbar_active() {
            return None;
        }
        let (thickness, margin) = (self.bar_thickness, self.bar_margin);
        let y1 = self.rect.y1 - margin;
        let y0 = y1 - thickness;
        let x0 = self.rect.x0 + margin;
        let mut x1 = self.rect.x1 - margin;
        if self.vbar_active() {
            x1 -= thickness + margin;
        }
        let track = self.corners.clear(Rect::new(x0, y0, x1, y1), false);
        if track.width() <= MIN_THUMB_LEN.min(1.0) {
            return None;
        }
        Some(track)
    }

    fn v_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            f64::from(self.content_h),
            f64::from(self.effective_viewport_h()),
            f64::from(self.offset_y),
        )
    }
    fn h_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            f64::from(self.content_w),
            f64::from(self.viewport_w),
            f64::from(self.offset_x),
        )
    }

    fn vertical_thumb(&self) -> Option<Rect> {
        let track = self.vertical_track()?;
        self.vbar.thumb_rect(track, self.v_metrics())
    }

    fn horizontal_thumb(&self) -> Option<Rect> {
        let track = self.horizontal_track()?;
        self.hbar.thumb_rect(track, self.h_metrics())
    }

    /// `ScrollAxis::Both` has no single thumb and always returns `None`.
    pub fn thumb_rect(&self, axis: ScrollAxis) -> Option<Rect> {
        match axis {
            ScrollAxis::Vertical => self.vertical_thumb(),
            ScrollAxis::Horizontal => self.horizontal_thumb(),
            ScrollAxis::Both => None,
        }
    }

    fn vertical_hover_zone(&self) -> Option<Rect> {
        let t = self.vertical_track()?;
        Some(Rect::new(t.x0 - HOVER_EXPAND, t.y0, t.x1, t.y1))
    }
    fn horizontal_hover_zone(&self) -> Option<Rect> {
        let t = self.horizontal_track()?;
        Some(Rect::new(t.x0, t.y0 - HOVER_EXPAND, t.x1, t.y1))
    }

    fn cursor_near_bar(&self, cur: Point) -> bool {
        self.vertical_hover_zone().is_some_and(|z| z.contains(cur))
            || self
                .horizontal_hover_zone()
                .is_some_and(|z| z.contains(cur))
    }

    fn bar_pinned(&self) -> bool {
        self.focused
            || self.hold.is_some()
            || self.pointer_near_bar
            || self.vbar.is_active()
            || self.hbar.is_active()
    }

    /// `now`/`reduce_motion` injected so the fade curve is unit-testable.
    fn bar_opacity_at(&self, now: Instant, reduce_motion: bool) -> f32 {
        let held = !self.overlay || reduce_motion || self.bar_pinned();
        crate::widget::scrollbar::BarFade::opacity(self.last_activity, now, held)
    }

    fn bar_opacity(&self, theme: &Theme) -> f32 {
        self.bar_opacity_at(crate::motion::frame_now(), theme.reduce_motion)
    }

    fn fade_held(&self) -> bool {
        !self.overlay || self.reduce_motion_active || self.bar_pinned()
    }

    fn fade_in_progress_at(&self, now: Instant) -> bool {
        crate::widget::scrollbar::BarFade::fading(self.last_activity, now, self.fade_held())
    }

    fn fade_in_progress(&self) -> bool {
        self.fade_in_progress_at(crate::motion::frame_now())
    }

    fn fade_start_deadline_at(&self, now: Instant) -> Option<Instant> {
        crate::widget::scrollbar::BarFade::fade_deadline(self.last_activity, now, self.fade_held())
    }

    /// Once the fade deadline passes, the compositor still needs one frame to
    /// replace the last visible thumb with the transparent scene. The opaque
    /// hold is neither an animation nor an expired fade.
    fn needs_terminal_fade_frame_at(&self, now: Instant) -> bool {
        self.terminal_fade_damage.get()
            || (self.bar_visible_last_draw.get()
                && !self.fade_held()
                && crate::widget::scrollbar::BarFade::finished(self.last_activity, now))
    }

    fn needs_terminal_fade_frame(&self) -> bool {
        self.needs_terminal_fade_frame_at(crate::motion::frame_now())
    }

    fn advance_hold(&mut self, now: Instant) {
        let Some(hold) = self.hold else { return };
        if now < hold.next_at {
            return;
        }
        // Reschedule first so a hold paused at the click point still ticks.
        if let Some(h) = &mut self.hold {
            h.next_at = deadline_after(now, HOLD_REPEAT_MS);
        }
        let page = if hold.vertical {
            self.effective_viewport_h() * PAGE_FRACTION
        } else {
            self.viewport_w * PAGE_FRACTION
        };
        let reached = if hold.vertical {
            match self.vertical_thumb() {
                Some(thumb) => {
                    thumb.contains(hold.anchor)
                        || (hold.dir > 0.0 && thumb.y1 >= hold.anchor.y)
                        || (hold.dir < 0.0 && thumb.y0 <= hold.anchor.y)
                }
                None => true,
            }
        } else {
            match self.horizontal_thumb() {
                Some(thumb) => {
                    thumb.contains(hold.anchor)
                        || (hold.dir > 0.0 && thumb.x1 >= hold.anchor.x)
                        || (hold.dir < 0.0 && thumb.x0 <= hold.anchor.x)
                }
                None => true,
            }
        };
        if reached {
            return;
        }
        if hold.vertical {
            self.scroll_by(0.0, f32_of_f64(hold.dir) * page);
        } else {
            self.scroll_by(f32_of_f64(hold.dir) * page, 0.0);
        }
    }

    /// The edges more content can scroll in from, while edge fades are on.
    fn faded_edges(&self) -> Vec<crate::surface::ContentEdge> {
        use crate::surface::ContentEdge;
        let mut edges = Vec::new();
        if !self.edge_fade {
            return edges;
        }
        if self.axis_has_vertical() {
            if self.offset_y > FIT_EPS {
                edges.push(ContentEdge::Top);
            }
            if self.max_offset_y() - self.offset_y > FIT_EPS {
                edges.push(ContentEdge::Bottom);
            }
        }
        if self.axis_has_horizontal() {
            let max_x = (self.content_w - self.viewport_w).max(0.0);
            if self.offset_x > FIT_EPS {
                edges.push(ContentEdge::Leading);
            }
            if max_x - self.offset_x > FIT_EPS {
                edges.push(ContentEdge::Trailing);
            }
        }
        edges
    }

    /// What this view paints over its content inside `clip` that does not
    /// move with it: its bars' tracks and the bands its edge fades cover.
    fn copy_overlays(&self, clip: Rect) -> Vec<Rect> {
        let mut overlays: Vec<Rect> = [self.vertical_track(), self.horizontal_track()]
            .into_iter()
            .flatten()
            .collect();
        if self.edge_fade {
            let reach = crate::surface::CONTENT_FADE_PX;
            if self.axis_has_vertical() {
                overlays.push(Rect::new(clip.x0, clip.y0, clip.x1, clip.y0 + reach));
                overlays.push(Rect::new(clip.x0, clip.y1 - reach, clip.x1, clip.y1));
            }
            if self.axis_has_horizontal() {
                overlays.push(Rect::new(clip.x0, clip.y0, clip.x0 + reach, clip.y1));
                overlays.push(Rect::new(clip.x1 - reach, clip.y0, clip.x1, clip.y1));
            }
        }
        overlays
    }

    fn draw_edge_fades(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let edges = self.faded_edges();
        if edges.is_empty() {
            return;
        }
        // The content's own lanes: the bar's gutter is not content.
        let mut area = self.content_viewport_rect();
        if self.reserve_vertical {
            area.x1 -= f64::from(self.content_gutter);
        }
        if self.reserve_horizontal {
            area.y1 -= f64::from(self.content_gutter);
        }
        let ground = theme.tokens.colors.surfaces.content;
        for edge in edges {
            crate::surface::content_edge_fade(scene, transform, area, edge, ground);
        }
    }

    fn draw_scrollbars(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let opacity = self.bar_opacity(theme);
        let visible = self.show_scrollbar && opacity > 0.003;
        if self.bar_visible_last_draw.replace(visible) && !visible {
            self.terminal_fade_damage.set(true);
        }
        if !visible {
            return;
        }
        let radius = self.bar_thickness / 2.0;
        // An idle overlay bar is just the thumb, like macOS/GNOME.
        let expanded = !self.overlay || self.bar_pinned();
        let track_color = crate::widget::scrollbar::track_color(theme).with_alpha_factor(opacity);
        for (track, thumb, active) in [
            (
                self.vertical_track(),
                self.vertical_thumb(),
                self.vbar.is_active(),
            ),
            (
                self.horizontal_track(),
                self.horizontal_thumb(),
                self.hbar.is_active(),
            ),
        ] {
            if let (Some(track), true) = (track, expanded) {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    track_color,
                    None,
                    &RoundedRect::from_rect(track, radius),
                );
            }
            if let Some(thumb) = thumb {
                // Hover/drag/focus brightens the derived idle thumb.
                let color = crate::widget::scrollbar::thumb_color(theme, active || self.focused)
                    .with_alpha_factor(opacity);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    color,
                    None,
                    &RoundedRect::from_rect(thumb, radius),
                );
            }
        }
    }

    /// Cursor motion: continues an active drag, or refreshes bar
    /// hover/fade state. Distinguishes a drag from a duplicate no-op so the
    /// latter never creates an undo boundary or synchronization request.
    fn on_cursor_moved(
        &mut self,
        position: winit::dpi::PhysicalPosition<f64>,
        scale_factor: f64,
    ) -> CursorMoveOutcome {
        let cur = Point::new(position.x / scale_factor, position.y / scale_factor);
        // A duplicate position carries no hover information. Coalesce it at
        // the viewport instead of routing it through every retained child.
        // Active drags are excluded because their geometry may have changed
        // independently between ticks while the pointer stayed still.
        if self.cursor_initialized
            && cur == self.last_cursor
            && !self.vbar.is_dragging()
            && !self.hbar.is_dragging()
        {
            return CursorMoveOutcome::Coalesced;
        }
        self.cursor_initialized = true;
        self.last_cursor = cur;
        if self.vbar.is_dragging() {
            if let Some(track) = self.vertical_track()
                && let Some(target) = self.vbar.drag_to(track, self.v_metrics(), cur)
            {
                self.scroll_by(0.0, f32_of_f64(target) - self.offset_y);
            }
            return CursorMoveOutcome::DragConsumed;
        }
        if self.hbar.is_dragging() {
            if let Some(track) = self.horizontal_track()
                && let Some(target) = self.hbar.drag_to(track, self.h_metrics(), cur)
            {
                self.scroll_by(f32_of_f64(target) - self.offset_x, 0.0);
            }
            return CursorMoveOutcome::DragConsumed;
        }
        // Falls through so the child updates its own hover too.
        let mut changed = false;
        if let Some(track) = self.vertical_track() {
            changed |= self.vbar.update_hover(track, self.v_metrics(), cur);
        } else {
            changed |= self.vbar.set_hover(false);
        }
        if let Some(track) = self.horizontal_track() {
            changed |= self.hbar.update_hover(track, self.h_metrics(), cur);
        } else {
            changed |= self.hbar.set_hover(false);
        }
        // Only edge transitions touch the fade timer.
        let near = self.cursor_near_bar(cur);
        if near != self.pointer_near_bar {
            self.pointer_near_bar = near;
            self.wake();
            changed = true;
        }
        if changed {
            self.bar_dirty = true;
        }
        CursorMoveOutcome::Forward
    }

    /// Thumb press begins a drag; track press pages toward the click and,
    /// if it actually moved, arms a hold for auto-repeat. Returns `true`
    /// when either bar consumed the press; `focus_target`/`node_id` are
    /// handled by the caller since they don't depend on this outcome.
    fn on_track_or_thumb_press(&mut self, cur: Point) -> bool {
        if let Some(track) = self.vertical_track() {
            match self.vbar.press(track, self.v_metrics(), cur) {
                PressOutcome::Thumb => {
                    self.scroll_dirty = true;
                    self.wake();
                    return true;
                }
                PressOutcome::Page(target) => {
                    let dir = (target - f64::from(self.offset_y)).signum();
                    self.scroll_by(0.0, f32_of_f64(target) - self.offset_y);
                    if dir != 0.0 {
                        self.hold = Some(TrackHold {
                            vertical: true,
                            dir,
                            anchor: cur,
                            next_at: deadline_after(crate::time::now(), HOLD_INITIAL_MS),
                        });
                    }
                    return true;
                }
                PressOutcome::Miss => {}
            }
        }
        if let Some(track) = self.horizontal_track() {
            match self.hbar.press(track, self.h_metrics(), cur) {
                PressOutcome::Thumb => {
                    self.scroll_dirty = true;
                    self.wake();
                    return true;
                }
                PressOutcome::Page(target) => {
                    let dir = (target - f64::from(self.offset_x)).signum();
                    self.scroll_by(f32_of_f64(target) - self.offset_x, 0.0);
                    if dir != 0.0 {
                        self.hold = Some(TrackHold {
                            vertical: false,
                            dir,
                            anchor: cur,
                            next_at: deadline_after(crate::time::now(), HOLD_INITIAL_MS),
                        });
                    }
                    return true;
                }
                PressOutcome::Miss => {}
            }
        }
        false
    }

    /// Page/Home/End/Arrow navigation while focused. Returns `true` when
    /// the key moved the view; a non-nav key falls through to the child.
    fn on_navigation_key(&mut self, key: &Key) -> bool {
        let page_v = self.effective_viewport_h() * PAGE_FRACTION;
        let page_h = self.viewport_w * PAGE_FRACTION;
        // A step big enough that `clamp` caps it at the content edge.
        let full = self.content_h.max(self.content_w);
        let has_v = self.axis_has_vertical();
        match key {
            Key::Named(NamedKey::PageDown) if has_v => self.scroll_by(0.0, page_v),
            Key::Named(NamedKey::PageUp) if has_v => self.scroll_by(0.0, -page_v),
            Key::Named(NamedKey::PageDown) => self.scroll_by(page_h, 0.0),
            Key::Named(NamedKey::PageUp) => self.scroll_by(-page_h, 0.0),
            Key::Named(NamedKey::Home) if has_v => self.scroll_by(0.0, -full),
            Key::Named(NamedKey::End) if has_v => self.scroll_by(0.0, full),
            Key::Named(NamedKey::Home) => self.scroll_by(-full, 0.0),
            Key::Named(NamedKey::End) => self.scroll_by(full, 0.0),
            Key::Named(NamedKey::ArrowDown) if has_v => self.scroll_by(0.0, LINE_STEP),
            Key::Named(NamedKey::ArrowUp) if has_v => self.scroll_by(0.0, -LINE_STEP),
            Key::Named(NamedKey::ArrowRight) if self.axis_has_horizontal() => {
                self.scroll_by(LINE_STEP, 0.0);
            }
            Key::Named(NamedKey::ArrowLeft) if self.axis_has_horizontal() => {
                self.scroll_by(-LINE_STEP, 0.0);
            }
            _ => return false,
        }
        true
    }

    /// Wheel delta → scroll offset; shift remaps vertical onto X for a
    /// plain wheel (a tilt wheel's own X delta passes through untouched).
    /// Returns `true` when the view actually moved, so an at-the-edge
    /// wheel can bubble to an outer/sibling scroll region.
    ///
    /// Pixel deltas (touchpads, touchscreens) follow the fingers 1:1; when
    /// the gesture's `Ended` phase arrives while they were still moving, the
    /// content keeps gliding and decelerates, unless motion is reduced.
    fn on_wheel(
        &mut self,
        delta: &MouseScrollDelta,
        phase: TouchPhase,
        shift: bool,
        scale_factor: f64,
    ) -> bool {
        // winit y-positive means "scroll up", but content is drawn at
        // (… − offset), so wheel-up must DECREASE offset_y — negate.
        const LINE_PX: f32 = 60.0;
        self.device_scale = scale_factor;
        // The hand takes over a jump in progress from where it is drawn.
        self.scroll_to.take_over(crate::motion::frame_now());
        let pixel = matches!(delta, MouseScrollDelta::PixelDelta(_));
        if pixel {
            match phase {
                TouchPhase::Ended => {
                    let release = self.kinetic.release(crate::time::now());
                    let now = crate::motion::frame_now();
                    // Let go past an end, the give springs back instead.
                    if self.band.is_stretched(now) {
                        let (vx, vy) = release.unwrap_or_default();
                        self.band.release(
                            vello::kurbo::Vec2::new(vx, vy),
                            self.band_viewport(),
                            self.motion,
                            now,
                        );
                        self.scroll_dirty = true;
                        return true;
                    }
                    return match release {
                        Some((vx, vy)) if !self.reduce_motion_active => {
                            self.fling(f32_of_f64(vx), f32_of_f64(vy));
                            true
                        }
                        _ => false,
                    };
                }
                TouchPhase::Cancelled => {
                    self.kinetic.reset();
                    if self.band.is_stretched(crate::motion::frame_now()) {
                        self.band.release(
                            vello::kurbo::Vec2::ZERO,
                            self.band_viewport(),
                            self.motion,
                            crate::motion::frame_now(),
                        );
                        self.scroll_dirty = true;
                    }
                    return false;
                }
                TouchPhase::Started => self.kinetic.reset(),
                TouchPhase::Moved => {}
            }
        } else {
            self.kinetic.reset();
        }
        // Fingers (or a wheel) back on the input catch a glide in progress.
        self.fling(0.0, 0.0);
        let (mut dx, mut dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (*x * -LINE_PX, *y * -LINE_PX),
            MouseScrollDelta::PixelDelta(pp) => (
                -f32_of_f64(pp.x / scale_factor),
                -f32_of_f64(pp.y / scale_factor),
            ),
        };
        if shift && self.axis_has_horizontal() && dx == 0.0 {
            dx = dy;
            dy = 0.0;
        }
        if pixel {
            return self.pan(dx, dy);
        }
        let before = (self.offset_x, self.offset_y);
        self.scroll_by(dx, dy);
        self.offset_x.to_bits() != before.0.to_bits()
            || self.offset_y.to_bits() != before.1.to_bits()
    }

    /// Fingers moved the content by `dx`, `dy`: one to one inside, and past
    /// an end through the rubber band; moving back takes the give in first.
    /// Returns whether anything moved.
    fn pan(&mut self, dx: f32, dy: f32) -> bool {
        let now = crate::motion::frame_now();
        let hand = vello::kurbo::Vec2::new(f64::from(dx), f64::from(dy));
        let viewport = self.band_viewport();
        let rest = self.band.take_in(hand, viewport, self.motion, now);
        let took_in = rest != hand;
        let before = (self.offset_x, self.offset_y);
        self.scroll_by(f32_of_f64(rest.x), f32_of_f64(rest.y));
        let moved = vello::kurbo::Vec2::new(
            f64::from(self.offset_x - before.0),
            f64::from(self.offset_y - before.1),
        );
        // What the content could not take, on an axis it scrolls along.
        let excess = vello::kurbo::Vec2::new(
            if self.axis_has_horizontal() && self.content_w > self.viewport_w + FIT_EPS {
                rest.x - moved.x
            } else {
                0.0
            },
            if self.axis_has_vertical() && self.max_offset_y() > 0.0 {
                rest.y - moved.y
            } else {
                0.0
            },
        );
        if excess != vello::kurbo::Vec2::ZERO {
            self.band.pull(excess, viewport, self.motion, now);
        }
        let gave = took_in || self.band.is_stretched(now);
        if gave {
            self.scroll_dirty = true;
            self.cursor_initialized = false;
            self.wake();
        }
        if moved == vello::kurbo::Vec2::ZERO && !gave {
            self.kinetic.reset();
            return false;
        }
        self.kinetic.record(hand.x, hand.y, crate::time::now());
        true
    }

    /// The view the give is measured against.
    fn band_viewport(&self) -> vello::kurbo::Vec2 {
        vello::kurbo::Vec2::new(
            f64::from(self.viewport_w),
            f64::from(self.effective_viewport_h()),
        )
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

/// Maximum `rect.y1` across a widget subtree, for finding the true bottom
/// edge when taffy's `content_size` under-reports.
fn max_bottom_edge(w: &dyn Widget) -> f64 {
    let mut deepest = w.get_rect().y1;
    if w.child_clip_rect().is_some() {
        return deepest;
    }
    for c in w.children() {
        deepest = deepest.max(max_bottom_edge(c));
    }
    deepest
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
                // always reaches the real last pixel.
                let scroll_origin_y = origin.y;
                let max_y = max_bottom_edge(c.as_ref());
                let walked_h = f32_of_f64((max_y - scroll_origin_y).max(0.0));
                if walked_h > self.content_h {
                    self.content_h = walked_h;
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
        if self.reveal_focus {
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
        if scrollable_now && !self.was_overflowing && self.last_activity.is_none() {
            self.wake();
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
        if let Some(c) = &mut self.child {
            crate::widget::draw_child(c.as_mut(), scene, transform, tr, theme, presence);
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
mod tests {
    use super::*;
    use crate::testing::{Dispatch, dispatch};
    use crate::widget::scrollbar::BAR_HOLD_MS as VISIBLE_HOLD_MS;
    // The bar fades as all chrome does.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the chrome fade is a few hundred milliseconds"
    )]
    const FADE_MS: u64 = crate::animation::CHROME_FADE.as_millis() as u64;
    use automerge::AutoCommit;

    fn vertical(content_h: f32) -> Scrollable {
        let mut s = Scrollable::new_vertical();
        s.rect = Rect::new(0.0, 0.0, 200.0, 300.0);
        s.set_viewport_size(200.0, 300.0);
        s.set_content_size(200.0, content_h);
        s
    }

    use crate::synthetic_input::cursor_moved;
    fn mouse(state: ElementState) -> WindowEvent {
        crate::synthetic_input::mouse_input(state, MouseButton::Left)
    }

    struct EventSpy {
        rect: Rect,
        pointer_events: Rc<Cell<u32>>,
        draw_viewport: Rc<Cell<Option<Rect>>>,
        activations: Rc<Cell<u32>>,
    }

    impl Widget for EventSpy {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let style = Style {
                size: Size::from_lengths(200.0, 500.0),
                ..Default::default()
            };
            taffy.widget_leaf(style, node_map, self)
        }

        crate::rect_geometry!();

        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            presence: &PresenceCtx,
        ) {
            self.draw_viewport.set(presence.viewport.get());
        }

        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
            if matches!(
                event,
                WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
            ) {
                self.pointer_events
                    .set(self.pointer_events.get().saturating_add(1));
            }
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            ) {
                self.activations
                    .set(self.activations.get().saturating_add(1));
                *ctx.focus_target = Some(ctx.node_id);
            }
        }

        fn name(&self) -> &'static str {
            "EventSpy"
        }

        fn reveal_descendant(&mut self, target: *const dyn Widget) -> bool {
            std::ptr::addr_eq(std::ptr::from_ref::<dyn Widget>(self), target)
        }
    }

    /// Records the layout clip it is placed under.
    struct ClipSpy {
        rect: Rect,
        clip: Rc<Cell<Option<Rect>>>,
    }

    impl Widget for ClipSpy {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let style = Style {
                size: Size::from_lengths(200.0, 2000.0),
                ..Default::default()
            };
            taffy.widget_leaf(style, node_map, self)
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            self.rect = crate::widget::layout_rect(taffy, node, origin);
            self.clip.set(crate::widget::layout_clip());
        }

        fn name(&self) -> &'static str {
            "ClipSpy"
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    /// A 40 px row that counts the times its damage is asked for.
    struct DamageCounter {
        rect: Rect,
        asked: Rc<Cell<usize>>,
    }

    impl Widget for DamageCounter {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let style = Style {
                size: Size::from_lengths(200.0, 40.0),
                ..Default::default()
            };
            taffy.widget_leaf(style, node_map, self)
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            self.rect = crate::widget::layout_rect(taffy, node, origin);
        }

        fn collect_damage(&self, regions: &mut Vec<Rect>) {
            self.asked.set(self.asked.get() + 1);
            regions.push(self.rect);
        }

        fn name(&self) -> &'static str {
            "DamageCounter"
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "a give at rest is exactly zero")]
    fn fingers_past_the_top_pull_the_content_through_the_band_and_it_springs_back() {
        use crate::time::testing::with_monotonic_time;
        let start = crate::time::now();
        let mut s = vertical(1000.0);
        with_monotonic_time(start, || {
            assert!(s.pan(0.0, -60.0), "the give is a move");
            let (_, y) = s.drawn_offset();
            assert!(
                y < 0.0 && y > -60.0,
                "the content gives, less than the hand: {y}"
            );
            assert_eq!(s.offset_y, 0.0, "the scroll itself stays at the end");
            // Moving back takes the give in before the content scrolls.
            assert!(s.pan(0.0, 30.0));
            let (_, back) = s.drawn_offset();
            assert!(back < 0.0 && back > y, "{y} then {back}");
            assert_eq!(s.offset_y, 0.0);
            let lift = MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, 0.0));
            assert!(s.on_wheel(&lift, TouchPhase::Ended, false, 1.0));
            assert!(s.needs_animation_frame() || s.band.is_moving(start));
        });
        with_monotonic_time(start + Duration::from_secs(1), || {
            assert_eq!(s.drawn_offset(), (0.0, 0.0), "sprung back to the end");
        });
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "a give at rest is exactly zero")]
    fn under_reduced_motion_fingers_stop_at_the_end() {
        use crate::time::testing::with_monotonic_time;
        let mut s = vertical(1000.0);
        s.motion = crate::tokens::EffectiveMotion::new(crate::tokens::Motion::default(), true);
        with_monotonic_time(crate::time::now(), || {
            assert!(!s.pan(0.0, -60.0), "nothing moved: the wheel may bubble");
            assert_eq!(s.drawn_offset(), (0.0, 0.0));
        });
    }

    #[test]
    fn a_long_column_scrolled_asks_only_the_rows_near_the_view_for_damage() {
        let asked = Rc::new(Cell::new(0));
        let column = (0..2000).fold(
            crate::widget::Column::new().with_intrinsic_height(),
            |column, _| {
                column.push(Box::new(DamageCounter {
                    rect: Rect::ZERO,
                    asked: asked.clone(),
                }))
            },
        );
        let mut scroll = Scrollable::new_vertical().with_child(Box::new(column));
        let mut taffy = TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = scroll.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(400.0),
                    height: taffy::AvailableSpace::Definite(400.0),
                },
            )
            .unwrap();
        scroll.offset_y = 20_000.0;
        scroll.apply_layout(&taffy, node, Point::ZERO);
        let mut regions = Vec::new();
        scroll.collect_damage(&mut regions);
        let visited = asked.get();
        // Only the rows that meet the view are asked, and all of them: the
        // first starts at its top, and none could fit below the last.
        let view = scroll.content_viewport_rect();
        let rows: Vec<Rect> = regions
            .iter()
            .copied()
            .filter(|rect| rect.width() == 200.0)
            .collect();
        assert_eq!(rows.len(), visited, "each row asked says it changed");
        let meets = |rect: &Rect| rect.y1 > view.y0 && rect.y0 < view.y1;
        assert!(
            rows.iter().all(meets),
            "only rows meeting {view:?} are asked: {rows:?}"
        );
        let pitch = rows[1].y0 - rows[0].y0;
        assert!(
            rows[0].y0 <= view.y0 && rows[rows.len() - 1].y0 + pitch >= view.y1,
            "every row meeting {view:?} is asked: {rows:?}"
        );
        assert!(
            regions
                .iter()
                .all(|rect| rect.y1 > -30.0 && rect.y0 < 430.0)
        );

        // The copy plan's walk of the tree visits the same few.
        let mut walked = 0;
        fn count(widget: &dyn Widget, walked: &mut usize) {
            *walked += 1;
            widget.for_each_child_in_clip(&mut |child| count(child, walked));
        }
        count(&scroll, &mut walked);
        assert!(walked < 20, "{walked} widgets walked");
    }

    #[test]
    fn content_is_placed_under_the_viewport_as_its_layout_clip() {
        let clip = Rc::new(Cell::new(None));
        let mut scroll = Scrollable::new_vertical().with_child(Box::new(ClipSpy {
            rect: Rect::ZERO,
            clip: clip.clone(),
        }));
        let mut taffy = TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = scroll.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(400.0),
                    height: taffy::AvailableSpace::Definite(300.0),
                },
            )
            .unwrap();
        assert_eq!(crate::widget::layout_clip(), None);
        scroll.apply_layout(&taffy, node, Point::new(10.0, 20.0));
        assert_eq!(clip.get(), Some(scroll.content_viewport_rect()));
        assert_eq!(
            crate::widget::layout_clip(),
            None,
            "the clip ends with the content's placement"
        );

        let outer = Rect::new(0.0, 0.0, 400.0, 100.0);
        crate::widget::with_layout_clip(outer, || {
            scroll.apply_layout(&taffy, node, Point::new(10.0, 20.0));
        });
        assert_eq!(
            clip.get(),
            Some(scroll.content_viewport_rect().intersect(outer))
        );
    }

    #[test]
    fn focus_reveal_scrolls_to_new_focus_with_context_but_respects_manual_scroll() {
        {
            let mut scroll = vertical(900.0).with_child(Box::new(EventSpy {
                rect: Rect::new(12.0, 520.0, 188.0, 568.0),
                pointer_events: Rc::default(),
                draw_viewport: Rc::default(),
                activations: Rc::default(),
            }));
            scroll.rect = Rect::new(0.0, 0.0, 200.0, 300.0);
            let target = {
                let child = scroll.child.as_deref_mut().expect("child");
                std::ptr::from_ref::<dyn Widget>(child)
            };

            assert!(scroll.reveal_descendant(target));
            assert!(
                scroll.offset_y() <= 484.0,
                "focus reveal should retain at least 36px of label/context above the field; offset={} ",
                scroll.offset_y()
            );
            assert!(
                scroll.offset_y() + 300.0 >= 604.0,
                "the focused field plus context below must remain visible"
            );
        }

        {
            use crate::{LocalState, widget::TextInput};
            let mut input = TextInput::from_state(&LocalState::new(String::new())).with_id("last");
            input.set_focus(true);
            let mut column = Column::new().with_intrinsic_height();
            for _ in 0..15 {
                column = column.push(Box::new(Text::new("Spacer row")));
            }
            column = column.push(Box::new(input));
            let mut scroll = Scrollable::new_vertical()
                .with_height(150.0)
                .with_focus_reveal()
                .with_child(Box::new(column));
            let theme = Theme::dark();
            let mut tree = TaffyTree::<()>::new();
            let mut nodes = HashMap::new();
            let mut text = TextRenderer::new();
            let node = scroll.build_layout(&mut tree, &theme, &mut nodes);
            tree.compute_layout_with_measure(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(400.0),
                    height: taffy::AvailableSpace::Definite(150.0),
                },
                |known, available, node, _, _| {
                    nodes
                        .get(&node)
                        .map(|p| unsafe { (**p).measure(known, available, &mut text) })
                        .unwrap_or(Size::ZERO)
                },
            )
            .unwrap();
            scroll.apply_layout(&tree, node, Point::ZERO);
            assert!(
                scroll.offset_y() > 0.0,
                "keyboard focus below viewport must be revealed"
            );
            let rect = scroll
                .child
                .as_ref()
                .unwrap()
                .children()
                .last()
                .unwrap()
                .get_rect();
            assert!(
                rect.y0 >= scroll.rect.y0 && rect.y1 <= scroll.rect.y1 + 0.5,
                "focused input must be visible: {rect:?}"
            );
            scroll.set_offset_y(0.0);
            scroll.apply_layout(&tree, node, Point::ZERO);
            assert_eq!(
                scroll.offset_y(),
                0.0,
                "manual scrolling must not snap back to unchanged focus"
            );
        }
    }

    #[test]
    fn field_validation_focus_reveal_fits_context_or_prioritises_protected_input() {
        use crate::headless::FrameHarness;
        use crate::widget::{Field, TextInput, TextInputKind};
        use crate::{LocalState, State};

        fn named<'a>(root: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
            if root.test_id() == Some(id) {
                return Some(root);
            }
            root.children()
                .into_iter()
                .find_map(|child| named(child, id))
        }
        fn correction<'a>(root: &'a dyn Widget, message: &str) -> Option<&'a dyn Widget> {
            if root.accessibility().is_some_and(|node| {
                node.name.as_deref() == Some(message)
                    && node.live == Some(crate::a11y::Politeness::Assertive)
            }) {
                return Some(root);
            }
            root.children()
                .into_iter()
                .find_map(|child| correction(child, message))
        }
        for scale in [1.0, 2.0] {
            for oversized in [false, true] {
                let message = if oversized {
                    "Use a different password. ".repeat(80)
                } else {
                    "Use at least 15 characters.".to_string()
                };
                let value = LocalState::new("private sample password".to_string());
                let mut input = TextInput::from_state(&value)
                    .with_id("validation-password")
                    .with_kind(TextInputKind::Password)
                    .with_secret(true);
                input.set_focus(true);
                let field = Field::new("Password", input)
                    .with_error_live(&message, crate::a11y::Politeness::Assertive);
                let mut content = Column::new().with_intrinsic_height();
                for _ in 0..20 {
                    content = content.push(Box::new(Text::new("Earlier form content")));
                }
                content = content.push(Box::new(field));
                let scroll = Scrollable::new_vertical()
                    .fill_parent_height()
                    .with_focus_reveal()
                    .with_child(Box::new(content));
                let mut harness = FrameHarness::new_with_theme(
                    State::new(),
                    Box::new(scroll),
                    (400, 300),
                    Theme::dark().with_ui_scale(scale).with_reduce_motion(true),
                );
                harness.with_widget_mut::<Scrollable, _>(|scroll| {
                    let viewport = scroll.content_viewport_rect();
                    let input = named(scroll, "validation-password").unwrap();
                    let access = input.accessibility().unwrap();
                    assert!(input.is_focused());
                    assert!(access.state.invalid && access.state.protected);
                    assert!(access.value.is_none(), "focus must not expose the password");
                    let input = input.get_rect();
                    let caption = correction(scroll, &message).unwrap().get_rect();
                    let context_height = caption.y1 - input.y0;
                    assert!(scroll.offset_y() > 0.0, "focus must reveal a field after earlier content");
                    assert!(input.y0 >= viewport.y0 && input.y1 <= viewport.y1,
                        "actual protected input remains fully visible: {input:?}, {viewport:?}");
                    if oversized {
                        assert!(context_height > viewport.height(),
                            "fixture must exercise the too-tall context fallback: {context_height}, {viewport:?}");
                        assert!(caption.y1 > viewport.y1,
                            "oversized caption cannot displace the correction input");
                    } else {
                        assert!(context_height <= viewport.height(),
                            "fixture must exercise fitting shared Field context");
                        assert!(caption.y0 >= viewport.y0 && caption.y1 <= viewport.y1,
                            "entire actual correction caption is revealed with input: {caption:?}, {viewport:?}");
                    }
                    scroll.set_offset_y(0.0);
                }).unwrap();
                // The harness ran the production layout after that deliberate
                // scroll. Unchanged focus must not pull the person back down.
                harness
                    .with_widget_mut::<Scrollable, _>(|scroll| {
                        assert_eq!(scroll.offset_y(), 0.0);
                        let input = named(scroll, "validation-password").unwrap();
                        assert!(input.is_focused());
                        assert!(
                            input.get_rect().y0 > scroll.content_viewport_rect().y1,
                            "manual scrolling really moved the focused field out of view"
                        );
                    })
                    .unwrap();
                harness.with_widget_mut::<Scrollable, _>(|scroll| {
                    assert_eq!(scroll.offset_y(), 0.0,
                        "later layout must continue respecting manual scroll with unchanged focus");
                }).unwrap();
            }
        }
    }

    #[test]
    fn draw_publishes_the_viewport_and_cursor_motion_is_coalesced_until_scroll() {
        {
            let observed = Rc::new(Cell::new(None));
            let mut scroll = Scrollable::new_vertical().with_child(Box::new(EventSpy {
                rect: Rect::new(20.0, 30.0, 420.0, 330.0),
                pointer_events: Rc::default(),
                activations: Rc::default(),
                draw_viewport: observed.clone(),
            }));
            scroll.set_viewport_rect(Rect::new(20.0, 30.0, 420.0, 330.0));
            let presence = PresenceCtx::default();
            let outer = Rect::new(0.0, 0.0, 800.0, 600.0);
            presence.viewport.set(Some(outer));
            scroll.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                &presence,
            );
            assert_eq!(observed.get(), Some(scroll.content_viewport_rect()));
            assert_eq!(presence.viewport.get(), Some(outer));
        }

        {
            let mut s = vertical(1200.0);
            let position = winit::dpi::PhysicalPosition::new(40.0, 80.0);

            assert_eq!(s.on_cursor_moved(position, 1.0), CursorMoveOutcome::Forward,);
            assert_eq!(
                s.on_cursor_moved(position, 1.0),
                CursorMoveOutcome::Coalesced,
            );
            assert!(
                !dispatch(&mut s, &cursor_moved(position.x, position.y)),
                "a coalesced move must not become a handled user action",
            );
        }

        {
            let mut s = vertical(1200.0);
            let position = winit::dpi::PhysicalPosition::new(40.0, 80.0);
            assert_eq!(s.on_cursor_moved(position, 1.0), CursorMoveOutcome::Forward,);
            assert_eq!(
                s.on_cursor_moved(position, 1.0),
                CursorMoveOutcome::Coalesced,
            );

            s.scroll_by(0.0, 10.0);
            assert_eq!(
                s.on_cursor_moved(position, 1.0),
                CursorMoveOutcome::Forward,
                "content moving under a stationary pointer requires one fresh route",
            );
        }
    }

    #[test]
    fn a_section_receives_same_position_motion_after_pointer_reentry() {
        use crate::widget::{Section, SectionHeader, Text};

        let opened = Rc::new(Cell::new(None));
        let seen = Rc::clone(&opened);
        let section = Section::new(vec![Box::new(Text::new("Details"))])
            .with_header(SectionHeader::group("Advanced options"))
            .collapsed()
            .on_toggle(move |open| seen.set(Some(open)));
        let mut scroll = Scrollable::new_vertical()
            .with_height(150.0)
            .with_child(Box::new(section));
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let mut text = TextRenderer::new();
        let node = scroll.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        tree.compute_layout_with_measure(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(400.0),
                height: taffy::AvailableSpace::Definite(150.0),
            },
            |known, available, node, _, _| {
                nodes
                    .get(&node)
                    .map(|p| {
                        // SAFETY: scroll exclusively owns the unchanged widget
                        // tree; its layout map remains valid throughout measurement.
                        unsafe { (**p).measure(known, available, &mut text) }
                    })
                    .unwrap_or(Size::ZERO)
            },
        )
        .unwrap();
        scroll.apply_layout(&tree, node, Point::ZERO);
        let at = scroll.child.as_ref().unwrap().children()[0]
            .get_rect()
            .center();
        assert!(scroll.content_viewport_rect().contains(at));
        let mut sender = Dispatch::new().layout(&tree, node);
        for event in [
            cursor_moved(at.x, at.y),
            WindowEvent::CursorLeft {
                device_id: winit::event::DeviceId::dummy(),
            },
            cursor_moved(at.x, at.y),
            mouse(ElementState::Pressed),
            mouse(ElementState::Released),
        ] {
            sender.send(&mut scroll, &event);
        }
        assert_eq!(opened.get(), Some(true), "the real disclosure must open");
        assert!(scroll.child.as_ref().unwrap().children().len() > 1);
    }

    #[test]
    fn a_scrolled_disclosure_gets_motion_after_leaving_a_text_field() {
        use crate::LocalState;
        use crate::widget::{Column, Section, SectionHeader, Spacer, Text, TextInput};

        for scale in [1.0, 2.0] {
            let opened = Rc::new(Cell::new(None));
            let seen = Rc::clone(&opened);
            let mut scroll = Scrollable::new_vertical()
                .with_height(150.0)
                .with_child(Box::new(
                    Column::new()
                        .with_intrinsic_height()
                        .push(Box::new(TextInput::from_state(&LocalState::new(
                            String::new(),
                        ))))
                        .push(Box::new(Spacer::fixed(300.0)))
                        .push(Box::new(
                            Section::new(vec![Box::new(Text::new("Details"))])
                                .with_header(SectionHeader::group("Advanced options"))
                                .collapsed()
                                .on_toggle(move |open| seen.set(Some(open))),
                        )),
                ));
            let theme = Theme::dark().with_text_scale(scale);
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let mut text = TextRenderer::new();
            let node = scroll.build_layout(&mut tree, &theme, &mut nodes);
            tree.compute_layout_with_measure(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(400.0),
                    height: taffy::AvailableSpace::Definite(150.0),
                },
                |known, available, node, _, _| {
                    nodes
                        .get(&node)
                        .map(|p| {
                            // SAFETY: scroll exclusively owns the unchanged widget tree;
                            // the node map remains valid for this measurement pass.
                            unsafe { (**p).measure(known, available, &mut text) }
                        })
                        .unwrap_or(Size::ZERO)
                },
            )
            .unwrap();
            scroll.apply_layout(&tree, node, Point::ZERO);
            let field = scroll.child.as_ref().unwrap().children()[0]
                .get_rect()
                .center();
            let mut measured = Dispatch::new().layout(&tree, node);
            measured.send(&mut scroll, &cursor_moved(field.x, field.y));
            scroll.set_offset_y(scroll.max_offset_y());
            scroll.apply_layout(&tree, node, Point::ZERO);
            assert!(
                scroll.offset_y() > 0.0,
                "the disclosure was scrolled into view"
            );
            let at = scroll.child.as_ref().unwrap().children()[2].children()[0]
                .get_rect()
                .center();
            assert!(scroll.content_viewport_rect().contains(at));
            assert!(
                !measured
                    .send(&mut scroll, &cursor_moved(at.x, at.y))
                    .handled,
                "hover transitions do not consume motion intended for a sibling"
            );
            assert!(
                measured
                    .send(&mut scroll, &mouse(ElementState::Pressed))
                    .handled
            );
            assert!(
                measured
                    .send(&mut scroll, &mouse(ElementState::Released))
                    .handled
            );
            assert_eq!(
                opened.get(),
                Some(true),
                "the real scrolled disclosure opens at scale {scale}"
            );
        }
    }

    #[test]
    fn content_insets_keep_the_child_clear_of_footer_lanes_and_the_bar() {
        {
            let pointer_events = Rc::new(Cell::new(0));
            let activations = Rc::new(Cell::new(0));
            let mut scroll = Scrollable::new_vertical()
                .with_child(Box::new(EventSpy {
                    rect: Rect::ZERO,
                    pointer_events: Rc::clone(&pointer_events),
                    draw_viewport: Rc::default(),
                    activations: Rc::clone(&activations),
                }))
                .with_height(100.0)
                .with_bottom_content_inset(20.0);
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let mut tr = TextRenderer::new();
            let node = scroll.build_layout(&mut taffy, &theme, &mut node_map);
            let parent = taffy.parent_node(
                Style {
                    size: Size::from_lengths(200.0, 100.0),
                    ..Default::default()
                },
                &[node],
            );
            taffy
                .compute_layout_with_measure(
                    parent,
                    Size {
                        width: taffy::AvailableSpace::Definite(200.0),
                        height: taffy::AvailableSpace::Definite(100.0),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = node_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, &mut tr) }
                        } else {
                            Size::ZERO
                        }
                    },
                )
                .unwrap();
            scroll.apply_layout(&taffy, node, Point::ZERO);

            let mut sender = Dispatch::new().layout(&taffy, node);
            let mut focus_target = None;
            for event in [cursor_moved(40.0, 90.0), mouse(ElementState::Pressed)] {
                focus_target = sender
                    .send(&mut scroll, &event)
                    .focus_target
                    .or(focus_target);
            }
            assert_eq!(
                pointer_events.get(),
                0,
                "the clipped child sees no pointer event"
            );
            assert_eq!(
                activations.get(),
                0,
                "an invisible child action cannot activate"
            );
            assert_eq!(
                focus_target, None,
                "the footer lane cannot focus the scroll view"
            );

            for event in [cursor_moved(40.0, 70.0), mouse(ElementState::Pressed)] {
                focus_target = sender
                    .send(&mut scroll, &event)
                    .focus_target
                    .or(focus_target);
            }
            assert_eq!(
                pointer_events.get(),
                2,
                "visible content still receives pointer input"
            );
            assert_eq!(activations.get(), 1, "visible child action still activates");
            assert!(focus_target.is_some(), "visible content remains focusable");
        }

        {
            const INSET: f32 = 12.0;
            let mut scroll = Scrollable::new_vertical().with_bottom_content_inset(INSET);
            scroll.set_viewport_rect(Rect::new(20.0, 30.0, 420.0, 330.0));
            scroll.set_content_size(400.0, 500.0);

            let clip = scroll.content_viewport_rect();
            assert_eq!(clip, Rect::new(20.0, 30.0, 420.0, 318.0));
            assert_eq!(scroll.effective_viewport_h(), 288.0);
            assert_eq!(
                scroll.vertical_track().expect("content overflows").y1,
                clip.y1 - BAR_MARGIN,
                "the scrollbar and content clip both stop above the footer lane",
            );

            scroll.set_offset_y(f32::MAX);
            assert_eq!(scroll.offset_y(), 212.0);
            assert_eq!(
                scroll.offset_y() + scroll.effective_viewport_h(),
                scroll.content_h,
                "reserving the paint-free lane must not make the final content unreachable",
            );
        }

        {
            let mut scroll = Scrollable::new_vertical().with_bottom_content_inset(12.0);
            scroll.set_viewport_rect(Rect::new(20.0, 30.0, 420.0, 330.0));
            scroll.set_content_size(400.0, 300.0);

            assert_eq!(scroll.content_viewport_rect(), scroll.rect);
            assert_eq!(scroll.effective_viewport_h(), 300.0);
            assert!(!scroll.vertical_overflows());
            assert!(scroll.vertical_track().is_none());
            scroll.set_offset_y(f32::MAX);
            assert_eq!(scroll.offset_y(), 0.0, "fitting content has no max scroll");
        }

        {
            use std::collections::HashMap;
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let mut tr = TextRenderer::new();
            let mut scroll = Scrollable::new_vertical()
                .with_child(tall_child())
                .with_height(300.0);
            let node = scroll.build_layout(&mut taffy, &theme, &mut node_map);
            let parent = taffy.parent_node(
                Style {
                    size: Size::from_lengths(400.0, 300.0),
                    ..Default::default()
                },
                &[node],
            );
            taffy
                .compute_layout_with_measure(
                    parent,
                    Size {
                        width: taffy::AvailableSpace::Definite(400.0),
                        height: taffy::AvailableSpace::Definite(300.0),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = node_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, &mut tr) }
                        } else {
                            Size::ZERO
                        }
                    },
                )
                .unwrap();
            // The host's adjust pass reserves the lane once the content
            // overflows; solve again as the host does.
            assert!(scroll.adjust_layout_style(&mut taffy, node, &mut tr));
            taffy
                .compute_layout_with_measure(
                    parent,
                    Size {
                        width: taffy::AvailableSpace::Definite(400.0),
                        height: taffy::AvailableSpace::Definite(300.0),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = node_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, &mut tr) }
                        } else {
                            Size::ZERO
                        }
                    },
                )
                .unwrap();
            assert!(
                !scroll.adjust_layout_style(&mut taffy, node, &mut tr),
                "the lane settles in one step"
            );
            scroll.apply_layout(&taffy, node, Point::ZERO);

            let child = &scroll.children()[0];
            let child_right = child.get_rect().x1;
            let track = scroll.vertical_track().expect("tall content shows the bar");
            assert!(
                child_right <= track.x0 + 0.5,
                "content right edge {child_right} must not reach the bar track {} (gutter reserved)",
                track.x0,
            );
            assert!(
                (scroll.rect.x1 - child_right - f64::from(theme.tokens.metrics.scrollbar_gutter))
                    .abs()
                    < 1.0,
                "child is inset by exactly the gutter: viewport right {}, child right {child_right}",
                scroll.rect.x1,
            );
        }
    }

    #[test]
    fn thumb_geometry_tracks_ratio_offset_minimum_and_visibility() {
        {
            let short = vertical(600.0).vertical_thumb().expect("bar shown at 2x");
            let tall = vertical(1200.0).vertical_thumb().expect("bar shown at 4x");
            assert!(
                short.height() > tall.height(),
                "thumb must shrink as content grows: 2x={}, 4x={}",
                short.height(),
                tall.height(),
            );
            let track_len = 300.0 - 2.0 * BAR_MARGIN;
            let expected = 0.5 * track_len;
            assert!(
                (short.height() - expected).abs() < 1.0,
                "2x content thumb should be ≈half the track ({expected}), got {}",
                short.height(),
            );
        }

        {
            let thumb = vertical(100_000.0).vertical_thumb().expect("bar shown");
            assert!(
                thumb.height() >= MIN_THUMB_LEN - 1e-6,
                "thumb {} below min {}",
                thumb.height(),
                MIN_THUMB_LEN,
            );
        }

        {
            let s = vertical(150.0);
            assert!(s.thumb_rect(ScrollAxis::Vertical).is_none());
            assert!(s.vertical_track().is_none());
            let s = vertical(300.0);
            assert!(s.thumb_rect(ScrollAxis::Vertical).is_none());
        }

        {
            let mut s = vertical(1200.0);
            s = s.with_scrollbar(false);
            assert!(s.thumb_rect(ScrollAxis::Vertical).is_none());
        }

        {
            let mut s = vertical(1200.0);
            let top = s.vertical_thumb().unwrap();
            let track = s.vertical_track().unwrap();
            assert!(
                (top.y0 - track.y0).abs() < 1e-6,
                "thumb starts at track top"
            );

            s.scroll_by(0.0, 100_000.0); // clamp to max
            let bottom = s.vertical_thumb().unwrap();
            assert!(
                (bottom.y1 - track.y1).abs() < 1e-3,
                "fully scrolled thumb should reach track bottom: {} vs {}",
                bottom.y1,
                track.y1,
            );
        }
    }

    #[test]
    fn thumb_drags_and_track_presses_scroll_by_offset_and_page() {
        {
            let mut s = vertical(1200.0); // max_scroll = 900
            let thumb = s.vertical_thumb().unwrap();
            let bar_x = s.rect.x1 - BAR_MARGIN - BAR_THICKNESS / 2.0;
            let mid_y = f64::midpoint(thumb.y0, thumb.y1);

            dispatch(&mut s, &cursor_moved(bar_x, mid_y));
            assert!(
                dispatch(&mut s, &mouse(ElementState::Pressed)),
                "press on thumb handled"
            );
            assert!(s.vbar.is_dragging(), "press on thumb begins a drag");
            assert!(s.wants_pointer_capture(), "drag requests pointer capture");
            assert_eq!(s.offset_y(), 0.0);

            dispatch(&mut s, &cursor_moved(bar_x, mid_y + 100.0));
            assert!(s.offset_y() > 0.0, "drag down increases offset");
            assert!(s.offset_y() <= 900.0, "offset stays clamped to max");
            let after_down = s.offset_y();

            dispatch(&mut s, &cursor_moved(bar_x, mid_y - 100.0));
            assert!(s.offset_y() < after_down, "drag up decreases offset");

            dispatch(&mut s, &mouse(ElementState::Released));
            assert!(!s.vbar.is_dragging(), "release ends the drag");
        }

        {
            let mut s = vertical(1200.0);
            let thumb = s.vertical_thumb().unwrap();
            let bar_x = s.rect.x1 - BAR_MARGIN - BAR_THICKNESS / 2.0;
            let below_y = thumb.y1 + 5.0;
            dispatch(&mut s, &cursor_moved(bar_x, below_y));
            assert!(
                dispatch(&mut s, &mouse(ElementState::Pressed)),
                "track click handled"
            );
            assert!(!s.vbar.is_dragging(), "track click does not start a drag");
            let paged = s.offset_y();
            assert!(paged > 0.0, "click below thumb pages down: {paged}");
            let page_frac = PAGE_FRACTION;
            assert!(
                (paged - 300.0 * page_frac).abs() < 1.0,
                "page step should be ≈viewport*{page_frac}, got {paged}",
            );
        }

        {
            let mut s = both(1200.0, 1200.0);
            let track = s.horizontal_track().expect("h-bar shown");
            let thumb = s.thumb_rect(ScrollAxis::Horizontal).expect("h-thumb");
            let mid_x = f64::midpoint(thumb.x0, thumb.x1);
            let bar_y = track.y0 + track.height() / 2.0;
            dispatch(&mut s, &cursor_moved(mid_x, bar_y));
            assert!(
                dispatch(&mut s, &mouse(ElementState::Pressed)),
                "press on h-thumb handled"
            );
            assert!(s.hbar.is_dragging(), "h-thumb press begins a drag");
            dispatch(&mut s, &cursor_moved(mid_x + 80.0, bar_y));
            assert!(
                s.offset_x() > 0.0,
                "drag right increases offset_x: {}",
                s.offset_x()
            );
            dispatch(&mut s, &mouse(ElementState::Released));
            assert!(!s.hbar.is_dragging(), "release ends the h-drag");
        }

        {
            let mut s = vertical(3000.0); // viewport 300, max_scroll 2700
            let bar_x = s.rect.x1 - BAR_MARGIN - BAR_THICKNESS / 2.0;
            let thumb = s.vertical_thumb().unwrap();
            let click_y = thumb.y1 + (s.rect.y1 - thumb.y1) * 0.8;
            dispatch(&mut s, &cursor_moved(bar_x, click_y));
            assert!(
                dispatch(&mut s, &mouse(ElementState::Pressed)),
                "track press handled"
            );
            assert!(
                s.hold.is_some(),
                "a track press below the thumb arms a hold"
            );
            assert!(!s.needs_animation_frame(), "a repeat is a wait, not motion");
            assert_eq!(
                s.next_wake_deadline(),
                s.hold.map(|h| h.next_at),
                "a held track pins the bar and wakes for its next page"
            );
            let after_first = s.offset_y();
            assert!(after_first > 0.0, "first page moved down: {after_first}");
            assert!(
                s.wants_pointer_capture(),
                "a live hold captures the pointer"
            );

            let mut prev = after_first;
            let mut steps = 0;
            for _ in 0..40 {
                let due = s.hold.map(|h| h.next_at).unwrap();
                s.advance_hold(due + Duration::from_millis(1));
                if s.offset_y() > prev {
                    steps += 1;
                    prev = s.offset_y();
                } else {
                    break; // thumb reached the click point → paging stopped
                }
            }
            assert!(
                steps >= 2,
                "the hold auto-repeats past the first page ({steps} extra steps)"
            );

            let thumb_now = s.vertical_thumb().unwrap();
            assert!(
                thumb_now.y1 >= click_y - BAR_THICKNESS && thumb_now.y0 <= click_y + 1.0,
                "paging stopped at the click, thumb=({},{}) click_y={click_y}",
                thumb_now.y0,
                thumb_now.y1,
            );

            assert!(
                dispatch(&mut s, &mouse(ElementState::Released)),
                "release handled"
            );
            assert!(s.hold.is_none(), "release ends the hold");
        }
    }

    #[test]
    fn keyboard_scrolling_and_focusability_require_scrollable_focus() {
        {
            use crate::a11y::Role;
            let s = vertical(1200.0);
            let node = Widget::accessibility(&s).expect("scroll container is a region");
            assert_eq!(node.role, Role::Group);
            assert_eq!(
                node.name, None,
                "a scroll container carries no name of its own"
            );
            assert_eq!(node.value, None);
        }

        {
            assert!(
                Widget::is_focusable(&vertical(1200.0)),
                "overflowing is focusable"
            );
            assert!(
                !Widget::is_focusable(&vertical(150.0)),
                "fitting is not focusable"
            );
            let mut hidden = vertical(1200.0);
            hidden.show_scrollbar = false;
            assert!(!Widget::is_focusable(&hidden));
        }

        {
            let mut s = vertical(1200.0); // viewport 300, max_scroll 900
            s.set_focus(true);
            assert!(s.is_focused());

            assert!(
                dispatch(&mut s, &named_key(NamedKey::PageDown)),
                "PageDown handled"
            );
            let page = 300.0 * PAGE_FRACTION;
            assert!(
                (s.offset_y() - page).abs() < 0.5,
                "PageDown ≈ 0.9 viewport, got {}",
                s.offset_y()
            );

            assert!(dispatch(&mut s, &named_key(NamedKey::End)), "End handled");
            assert!(
                (s.offset_y() - 900.0).abs() < 1e-3,
                "End → bottom, got {}",
                s.offset_y()
            );

            assert!(
                dispatch(&mut s, &named_key(NamedKey::PageUp)),
                "PageUp handled"
            );
            assert!(
                (s.offset_y() - (900.0 - page)).abs() < 0.5,
                "PageUp from bottom, got {}",
                s.offset_y()
            );

            assert!(dispatch(&mut s, &named_key(NamedKey::Home)), "Home handled");
            assert_eq!(s.offset_y(), 0.0, "Home → top");

            assert!(
                dispatch(&mut s, &named_key(NamedKey::ArrowDown)),
                "ArrowDown handled"
            );
            assert!(
                (s.offset_y() - LINE_STEP).abs() < 1e-3,
                "ArrowDown = one line, got {}",
                s.offset_y()
            );
        }

        {
            let mut s = vertical(1200.0);
            assert!(!s.is_focused());
            assert!(
                !dispatch(&mut s, &named_key(NamedKey::PageDown)),
                "unfocused key not handled"
            );
            assert_eq!(s.offset_y(), 0.0, "unfocused container does not scroll");
        }
    }

    #[test]
    fn wheel_scrolls_vertically_and_horizontally() {
        {
            let mut s = vertical(1200.0);
            let before = s.offset_y();
            let ev = crate::synthetic_input::mouse_wheel(0.0, -1.0);
            assert!(dispatch(&mut s, &ev), "wheel handled");
            assert!(s.offset_y() > before, "wheel down increases offset");
        }

        {
            use winit::keyboard::ModifiersState;
            let mut s = both(1200.0, 1200.0);
            let shift = winit::event::Modifiers::from(ModifiersState::SHIFT);
            let wheel = WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta: MouseScrollDelta::LineDelta(0.0, -1.0), // a plain vertical wheel
                phase: winit::event::TouchPhase::Moved,
            };
            assert!(
                Dispatch::new()
                    .modifiers(shift)
                    .send(&mut s, &wheel)
                    .handled,
                "shift+wheel handled"
            );
            assert!(
                s.offset_x() > 0.0,
                "shift+wheel scrolls X, got x={}",
                s.offset_x()
            );
            assert_eq!(s.offset_y(), 0.0, "shift+wheel leaves Y untouched");
        }

        {
            let mut s = both(1200.0, 1200.0);
            let tilt = WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta: MouseScrollDelta::LineDelta(-1.0, 0.0), // horizontal tilt
                phase: winit::event::TouchPhase::Moved,
            };
            assert!(dispatch(&mut s, &tilt), "tilt wheel handled");
            assert!(
                s.offset_x() > 0.0,
                "tilt wheel scrolls X, got x={}",
                s.offset_x()
            );
        }
    }

    use crate::widget::{Column, Row, Text};
    use std::collections::HashMap;

    #[test]
    fn changing_max_height_relayouts_the_retained_scroll_tree() {
        let mut scroll = Box::new(
            Scrollable::new_vertical()
                .with_child(tall_child())
                .with_max_height(300.0),
        );
        let theme = Theme::dark();
        let mut taffy = TaffyTree::new();
        let mut nodes = HashMap::new();
        let mut text = TextRenderer::new();
        let node = scroll.build_layout(&mut taffy, &theme, &mut nodes);
        let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
        // SAFETY: the boxed tree owns this exact stable node map.
        unsafe { bindings.rebuild(scroll.as_ref(), &taffy, node, &nodes) };
        let child_node = scroll.child_node;
        let child = scroll.child.as_deref().unwrap() as *const dyn Widget;
        for height in [300.0, 120.0, 240.0] {
            scroll.set_max_height(height);
            // SAFETY: boxed retained widgets remain alive and uniquely owned
            // for both the measurement and style-settling passes.
            unsafe {
                crate::widget::layout::compute_widget_layout(
                    &mut taffy,
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(400.0),
                        height: taffy::AvailableSpace::MaxContent,
                    },
                    &nodes,
                    &bindings,
                    &mut text,
                )
            }
            .unwrap();
            scroll.apply_layout(&taffy, node, Point::ZERO);
            assert!(
                (scroll.get_rect().height() - f64::from(height)).abs() < 0.5,
                "retained maximum {height} produced viewport {}, style {:?}",
                scroll.get_rect().height(),
                taffy.style(node).unwrap(),
            );
            assert_eq!(scroll.child_node, child_node);
            assert!(std::ptr::addr_eq(
                child,
                scroll.child.as_deref().unwrap() as *const dyn Widget
            ));
            assert!(scroll.content_h > scroll.viewport_h);
        }
    }

    fn viewport_height_under_indefinite_parent(mut scroll: Scrollable) -> f64 {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let scroll_node = scroll.build_layout(&mut taffy, &theme, &mut node_map);
        let parent = taffy.parent_node(
            Style {
                display: taffy::Display::Block,
                size: Size {
                    width: Dimension::length(400.0),
                    height: Dimension::auto(),
                },
                ..Default::default()
            },
            &[scroll_node],
        );
        taffy
            .compute_layout_with_measure(
                parent,
                Size::max_content(),
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = node_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, &mut tr) }
                    } else {
                        Size::ZERO
                    }
                },
            )
            .unwrap();
        scroll.apply_layout(&taffy, scroll_node, Point::ZERO);
        scroll.get_rect().height()
    }

    /// Solve `root` the way the host does: measure, adjust, repeat.
    fn host_solve(root: &mut dyn Widget, width: f32, height: f32) {
        use std::collections::HashMap;
        let theme = Theme::dark();
        let mut tree = TaffyTree::<()>::new();
        let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = root.build_layout(&mut tree, &theme, &mut nodes);
        let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
        let mut text = crate::font_catalog::fixture_renderer();
        // SAFETY: exact live, exclusively owned widget tree and matching maps.
        unsafe {
            bindings.rebuild(root, &tree, node, &nodes);
            crate::widget::compute_widget_layout(
                &mut tree,
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(width),
                    height: taffy::AvailableSpace::Definite(height),
                },
                &nodes,
                &bindings,
                &mut text,
            )
            .unwrap();
        }
        root.apply_layout(&tree, node, Point::ZERO);
    }

    fn scroll_of(root: &mut dyn Widget) -> &mut Scrollable {
        fn find(widget: &mut dyn Widget) -> Option<&mut Scrollable> {
            if widget.name() == "Scrollable" {
                return widget.as_any_mut()?.downcast_mut::<Scrollable>();
            }
            widget.children_mut().into_iter().find_map(find)
        }
        find(root).expect("a scrollable")
    }

    #[test]
    fn the_bar_lane_is_reserved_only_while_the_content_overflows() {
        let short = || -> Element {
            Box::new(
                Column::new()
                    .with_intrinsic_height()
                    .push(Box::new(Text::new("one line"))),
            )
        };
        let mut fits = Scrollable::new_vertical()
            .with_child(short())
            .with_height(300.0);
        host_solve(&mut fits, 400.0, 300.0);
        let child = fits.children()[0].get_rect();
        assert!(
            (child.x1 - fits.rect.x1).abs() < 0.5,
            "fitting content keeps the full width: {child:?}"
        );

        let mut overflows = Scrollable::new_vertical()
            .with_child(tall_child())
            .with_height(300.0);
        host_solve(&mut overflows, 400.0, 300.0);
        let child = overflows.children()[0].get_rect();
        let gutter = f64::from(Theme::dark().tokens.metrics.scrollbar_gutter);
        assert!(
            (overflows.rect.x1 - child.x1 - gutter).abs() < 0.5,
            "{child:?}"
        );
        let track = overflows.vertical_track().expect("bar");
        let thumb = f64::from(Theme::dark().tokens.metrics.scroll_thumb);
        assert!((track.width() - thumb).abs() < 1e-6);
        assert!(
            ((overflows.rect.x1 - track.x1) - (track.x0 - child.x1)).abs() < 0.5,
            "the thumb is centred in its lane"
        );
    }

    #[test]
    fn descendant_bottom_remains_reachable_after_repeated_layout() {
        for gap in [0.25, 0.5, 0.75, 1.25] {
            let mut content = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(gap);
            for index in 0..31 {
                content = content.push(Box::new(Text::new(&format!("Row {index}"))));
            }
            let mut scroll = Scrollable::new_vertical()
                .with_height(257.0)
                .with_child(Box::new(content));
            host_solve(&mut scroll, 311.0, 257.0);
            scroll.set_offset_y(scroll.max_offset_y());
            for _ in 0..3 {
                host_solve(&mut scroll, 311.0, 257.0);
                let bottom = max_bottom_edge(scroll.children()[0]);
                assert!(
                    bottom <= scroll.content_viewport_rect().y1,
                    "layout must not pull the last descendant below the viewport: {bottom}"
                );
            }
        }
    }

    #[test]
    fn a_restored_offset_is_clamped_before_the_content_is_placed() {
        let cell = Rc::new(Cell::new((0.0_f32, 50_000.0_f32)));
        let mut scroll = Scrollable::new_vertical()
            .with_child(tall_child())
            .with_height(300.0)
            .with_persisted_offset(Rc::clone(&cell));
        host_solve(&mut scroll, 400.0, 300.0);
        let content = scroll.children()[0].get_rect();
        assert!(
            content.y1 >= scroll.rect.y1 - 0.5,
            "the end of the content fills the viewport, no blank frame: {content:?}"
        );
        assert!(cell.get().1 <= scroll.max_offset_y() + 0.01);
    }

    #[test]
    fn horizontal_content_is_laid_out_at_its_max_content_width() {
        let wide = Row::new()
            .no_wrap()
            .with_gap(0.0)
            .push(Box::new(
                crate::widget::Container::new().with_size(500.0, 20.0),
            ))
            .push(Box::new(
                crate::widget::Container::new().with_size(500.0, 20.0),
            ));
        let mut scroll = Scrollable::new_horizontal()
            .with_child(Box::new(wide))
            .with_height(80.0);
        host_solve(&mut scroll, 300.0, 80.0);
        assert!(scroll.children()[0].get_rect().width() >= 999.0);
        assert!(scroll.hbar_active(), "wide content scrolls sideways");
        let gutter = f64::from(Theme::dark().tokens.metrics.scrollbar_gutter);
        assert!(
            (scroll.rect.height() - f64::from(scroll.viewport_h)).abs() < 0.5
                && scroll.children()[0].get_rect().height() <= 80.0 - gutter + 0.5,
            "the bar's lane at the bottom is reserved"
        );
    }

    #[test]
    fn fill_parent_height_fills_a_row_or_a_root_container() {
        let mut root = crate::widget::Container::new()
            .with_no_padding()
            .with_max_width(500.0)
            .filling()
            .with_child(Box::new(
                Scrollable::new_vertical()
                    .fill_parent_height()
                    .with_child(tall_child()),
            ));
        host_solve(&mut root, 800.0, 600.0);
        let scroll = scroll_of(&mut root);
        assert!(
            (scroll.rect.height() - 600.0).abs() < 0.5,
            "the scroller takes the root's height: {:?}",
            scroll.rect
        );
        assert!(scroll.vertical_overflows());
    }

    #[test]
    fn edge_fades_show_only_where_more_content_can_scroll_in() {
        use crate::surface::ContentEdge;
        let mut scroll = vertical(900.0).with_edge_fade();
        assert_eq!(scroll.faded_edges(), vec![ContentEdge::Bottom]);
        scroll.set_offset_y(100.0);
        assert_eq!(
            scroll.faded_edges(),
            vec![ContentEdge::Top, ContentEdge::Bottom]
        );
        scroll.set_offset_y(f32::MAX);
        assert_eq!(scroll.faded_edges(), vec![ContentEdge::Top]);
        assert!(vertical(900.0).faded_edges().is_empty(), "off unless asked");
        assert!(
            vertical(100.0).with_edge_fade().faded_edges().is_empty(),
            "fits"
        );
    }

    fn tall_child() -> Element {
        let mut col = Column::new().with_intrinsic_height();
        for i in 0..40 {
            col = col.push(Box::new(Text::new(&format!("row {i}"))));
        }
        Box::new(col)
    }

    fn can_scroll_under_indefinite_parent(mut scroll: Scrollable) -> (f64, bool) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let scroll_node = scroll.build_layout(&mut taffy, &theme, &mut node_map);
        let parent = taffy.parent_node(
            Style {
                display: taffy::Display::Block,
                size: Size {
                    width: Dimension::length(400.0),
                    height: Dimension::auto(),
                },
                ..Default::default()
            },
            &[scroll_node],
        );
        taffy
            .compute_layout_with_measure(
                parent,
                Size::max_content(),
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = node_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, &mut tr) }
                    } else {
                        Size::ZERO
                    }
                },
            )
            .unwrap();
        scroll.apply_layout(&taffy, scroll_node, Point::ZERO);
        let vh = scroll.get_rect().height();
        scroll.scroll_by(0.0, 200.0);
        (vh, scroll.offset_y() > 0.0)
    }

    #[test]
    fn height_constraints_decide_scrollability_under_indefinite_parents() {
        {
            let h = viewport_height_under_indefinite_parent(
                Scrollable::new_vertical()
                    .with_child(tall_child())
                    .with_height(300.0),
            );
            assert!(
                (h - 300.0).abs() < 0.5,
                "with_height must pin the viewport to 300 even under an \
             indefinite-height parent; got {h}"
            );
        }

        {
            let (vh, scrolled) = can_scroll_under_indefinite_parent(
                Scrollable::new_vertical()
                    .with_child(tall_child())
                    .with_height(300.0),
            );
            assert!((vh - 300.0).abs() < 0.5, "viewport pinned to 300, got {vh}");
            assert!(
                scrolled,
                "a pinned-viewport pane with tall content must scroll"
            );
        }

        {
            let short = viewport_height_under_indefinite_parent(
                Scrollable::new_vertical()
                    .with_child(Box::new(Text::new("one compact line")))
                    .with_max_height(300.0),
            );
            assert!(
                short < 100.0,
                "a short document must not reserve the full cap: {short}"
            );

            let (tall, scrolled) = can_scroll_under_indefinite_parent(
                Scrollable::new_vertical()
                    .with_child(tall_child())
                    .with_max_height(300.0),
            );
            assert!(
                (tall - 300.0).abs() < 0.5,
                "a long document must stop at the cap: {tall}"
            );
            assert!(scrolled, "a capped long document must remain scrollable");
        }

        {
            let (_vh, scrolled) = can_scroll_under_indefinite_parent(
                Scrollable::new_vertical().with_child(tall_child()),
            );
            assert!(
                !scrolled,
                "legacy default is expected to be unscrollable under an \
             indefinite parent; the fix is opt-in via with_height / \
             fill_parent_height"
            );
        }
    }

    #[test]
    fn scroll_offset_persists_reclamps_and_needs_no_remeasure() {
        {
            use std::collections::HashMap;
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let mut tr = TextRenderer::new();
            let mut scroll = Scrollable::new_vertical()
                .with_child(tall_child())
                .with_height(300.0);
            let node = scroll.build_layout(&mut taffy, &theme, &mut node_map);
            let parent = taffy.parent_node(
                Style {
                    size: Size::from_lengths(400.0, 300.0),
                    ..Default::default()
                },
                &[node],
            );
            taffy
                .compute_layout_with_measure(
                    parent,
                    Size {
                        width: taffy::AvailableSpace::Definite(400.0),
                        height: taffy::AvailableSpace::Definite(300.0),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = node_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, &mut tr) }
                        } else {
                            Size::ZERO
                        }
                    },
                )
                .unwrap();
            scroll.apply_layout(&taffy, node, Point::ZERO);
            scroll.scroll_by(0.0, 40.0);
            assert!(
                scroll.is_layout_dirty(),
                "a scroll needs apply_layout to reposition"
            );
            assert!(
                !scroll.layout_needs_measure(),
                "a laid-out subtree needs no taffy re-solve on a pure scroll",
            );
        }

        {
            let cell = Rc::new(Cell::new((0.0f32, 0.0f32)));

            let mut a = vertical(1200.0).with_persisted_offset(cell.clone());
            a.scroll_by(0.0, 250.0);
            assert_eq!(a.offset_y(), 250.0);
            assert_eq!(
                cell.get().1,
                250.0,
                "scroll writes back into the shared cell"
            );

            let b = Scrollable::new_vertical().with_persisted_offset(cell);
            assert_eq!(
                b.offset_y(),
                250.0,
                "rebuilt Scrollable resumes the saved offset"
            );

            let c = Scrollable::new_vertical();
            assert_eq!(c.offset_y(), 0.0, "an unbound Scrollable is unaffected");
        }

        {
            let cell = Rc::new(Cell::new((0.0f32, 0.0f32)));
            let mut s = Scrollable::new_vertical().with_persisted_offset(cell.clone());
            s.set_content_size(200.0, 1200.0);
            s.set_viewport_size(200.0, 300.0); // max scroll 900
            s.scroll_by(0.0, 900.0);
            assert_eq!(
                cell.get().1,
                900.0,
                "scrolled to the bottom, cell tracks it"
            );
            // The directory shrinks: content now only 400 tall → max scroll 100.
            s.set_content_size(200.0, 400.0);
            assert_eq!(s.offset_y(), 100.0, "offset clamped to the new max");
            assert_eq!(cell.get().1, 100.0, "cell rewritten with the clamped value");
        }
    }

    fn both(content_w: f32, content_h: f32) -> Scrollable {
        let mut s = Scrollable::new(ScrollAxis::Both);
        s.rect = Rect::new(0.0, 0.0, 300.0, 300.0);
        s.set_viewport_size(300.0, 300.0);
        s.set_content_size(content_w, content_h);
        s
    }

    use crate::synthetic_input::named_key;

    #[test]
    fn bars_keep_out_of_the_windows_rounded_corners_only() {
        let draw = |s: &mut Scrollable, theme: &Theme| {
            s.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                theme,
                &PresenceCtx::empty(),
            );
        };
        let mut square = both(900.0, 900.0);
        let mut theme = Theme::dark();
        draw(&mut square, &theme);
        let (v_square, h_square) = (
            square.vertical_track().expect("tall content shows the bar"),
            square
                .horizontal_track()
                .expect("wide content shows the bar"),
        );

        let radius = f64::from(theme.tokens.radii.tile);
        theme.surface = Some(crate::theme::SurfaceShape::rounded(
            Rect::new(0.0, 0.0, 300.0, 300.0),
            radius,
        ));
        let mut rounded = both(900.0, 900.0);
        draw(&mut rounded, &theme);
        let v = rounded.vertical_track().expect("bar");
        let h = rounded.horizontal_track().expect("bar");
        assert!((v.y0 - radius).abs() < 1e-9, "the top corner: {v:?}");
        assert!(
            (h.x0 - radius).abs() < 1e-9,
            "the bottom-left corner: {h:?}"
        );
        assert!(v.y1 <= v_square.y1 && h.x1 <= h_square.x1);

        // Square again: the same window, edge to edge.
        theme.surface = Some(crate::theme::SurfaceShape::rounded(
            Rect::new(0.0, 0.0, 300.0, 300.0),
            0.0,
        ));
        draw(&mut rounded, &theme);
        assert_eq!(rounded.vertical_track(), Some(v_square));
        assert_eq!(rounded.horizontal_track(), Some(h_square));

        theme.surface = Some(crate::theme::SurfaceShape::rounded(
            Rect::new(0.0, 0.0, 300.0, 300.0),
            radius,
        ));
        let mut alone = Scrollable::new_vertical();
        alone.rect = Rect::new(0.0, 0.0, 300.0, 300.0);
        alone.set_viewport_size(300.0, 300.0);
        alone.set_content_size(300.0, 1200.0);
        draw(&mut alone, &theme);
        let track = alone.vertical_track().expect("bar");
        assert!((track.y1 - (300.0 - radius)).abs() < 1e-9, "{track:?}");
    }

    #[test]
    fn overlay_bar_fades_with_activity_and_damages_its_last_frame() {
        {
            let mut s = vertical(1200.0);
            s.last_activity = None; // as freshly constructed
            s.pointer_near_bar = false;
            assert_eq!(
                s.bar_opacity_at(crate::motion::frame_now(), false),
                0.0,
                "idle overlay is hidden"
            );

            s.wake();
            let t0 = s.last_activity.unwrap();
            assert_eq!(
                s.bar_opacity_at(t0, false),
                1.0,
                "full opacity right after activity"
            );
            assert_eq!(
                s.bar_opacity_at(t0 + Duration::from_millis(VISIBLE_HOLD_MS - 10), false),
                1.0,
                "still fully visible during the hold",
            );
            let mid = s.bar_opacity_at(
                t0 + Duration::from_millis(VISIBLE_HOLD_MS + FADE_MS / 2),
                false,
            );
            assert!(mid > 0.0 && mid < 1.0, "mid-fade opacity is partial: {mid}");
            assert_eq!(
                s.bar_opacity_at(
                    t0 + Duration::from_millis(VISIBLE_HOLD_MS + FADE_MS + 100),
                    false,
                ),
                0.0,
                "fully faded out once idle",
            );
        }

        {
            let mut s = vertical(1200.0);
            s.last_activity = Some(
                crate::time::now()
                    .checked_sub(Duration::from_millis(VISIBLE_HOLD_MS + FADE_MS + 1))
                    .expect("the test duration is far shorter than the monotonic clock"),
            );
            s.bar_visible_last_draw.set(true);
            let mut scene = Scene::new();
            s.draw_scrollbars(&mut scene, Affine::IDENTITY, &Theme::dark());

            assert!(
                s.needs_terminal_fade_frame(),
                "the first transparent frame must be submitted"
            );
            let mut regions = Vec::new();
            s.collect_damage(&mut regions);
            assert!(
                !regions.is_empty(),
                "the transparent terminal frame must repaint the former track"
            );

            s.mark_drawn();
            assert!(
                !s.needs_terminal_fade_frame(),
                "the terminal repaint is a one-frame latch"
            );
        }

        {
            let mut s = vertical(1200.0);
            s.wake();
            let long_after = s.last_activity.unwrap() + Duration::from_secs(30);
            assert_eq!(s.bar_opacity_at(long_after, false), 0.0);
            s.focused = true;
            assert_eq!(
                s.bar_opacity_at(long_after, false),
                1.0,
                "focus pins the bar visible"
            );
            s.focused = false;
            assert_eq!(
                s.bar_opacity_at(long_after, true),
                1.0,
                "reduced motion pins visible"
            );
            let mut persistent = vertical(1200.0).always_visible();
            persistent.last_activity = None;
            assert_eq!(
                persistent.bar_opacity_at(crate::motion::frame_now(), false),
                1.0,
                "persistent bar always visible"
            );
        }

        {
            let s = vertical(1200.0);
            assert!(
                !Widget::needs_animation_frame(&s),
                "idle list does not animate"
            );
            let p = vertical(1200.0).always_visible();
            assert!(
                !Widget::needs_animation_frame(&p),
                "persistent bar is static"
            );
            let mut fading = vertical(1200.0);
            fading.last_activity =
                Some(crate::time::now() - Duration::from_millis(VISIBLE_HOLD_MS + 1));
            assert!(
                Widget::needs_animation_frame(&fading),
                "a fading bar pumps frames"
            );
        }
    }

    #[test]
    fn opaque_hold_sleeps_until_fade_while_preserving_other_deadlines() {
        // Fresh curves are read in the instant they start: a descheduled
        // runner cannot age them.
        crate::time::testing::with_monotonic_time(crate::time::now(), || {
            {
                let mut s = vertical(1200.0);
                s.wake();
                let activity = s.last_activity.unwrap();
                assert!(s.needs_redraw(), "activity must still paint the thumb");
                s.bar_visible_last_draw.set(true);
                s.mark_drawn();
                assert!(
                    !s.needs_animation_frame(),
                    "opaque hold must not pump frames"
                );
                assert!(!s.needs_redraw(), "unchanged opaque thumb needs no damage");
                assert!(
                    !s.needs_terminal_fade_frame(),
                    "hold is not an expired fade"
                );
                assert_eq!(
                    s.next_wake_deadline(),
                    Some(activity + Duration::from_millis(900))
                );
            }

            {
                let mut s = vertical(1200.0);
                let start = crate::time::now();
                s.last_activity = Some(start);
                s.bar_visible_last_draw.set(true);
                let fade_start = start + Duration::from_millis(900);
                let fade_end = start + Duration::from_millis(1220);
                assert_eq!(s.fade_start_deadline_at(start), Some(fade_start));
                assert!(!s.fade_in_progress_at(fade_start - Duration::from_nanos(1)));
                assert!(!s.needs_terminal_fade_frame_at(fade_start - Duration::from_nanos(1)));
                assert!(s.fade_in_progress_at(fade_start));
                assert_eq!(s.fade_start_deadline_at(fade_start), None);
                assert!(s.fade_in_progress_at(fade_end - Duration::from_nanos(1)));
                assert!(!s.needs_terminal_fade_frame_at(fade_end - Duration::from_nanos(1)));
                assert!(!s.fade_in_progress_at(fade_end));
                assert!(s.needs_terminal_fade_frame_at(fade_end));
                s.last_activity = Some(start + Duration::from_millis(50));
                assert_eq!(
                    s.fade_start_deadline_at(start + Duration::from_millis(50)),
                    Some(fade_start + Duration::from_millis(50))
                );
                s.focused = true;
                assert_eq!(s.fade_start_deadline_at(start), None);
                s.focused = false;
                s.reduce_motion_active = true;
                assert_eq!(s.fade_start_deadline_at(start), None);
                s.reduce_motion_active = false;
                s.overlay = false;
                assert_eq!(s.fade_start_deadline_at(start), None);
            }

            {
                let now = crate::time::now();
                let mut child = vertical(1200.0);
                child.last_activity = Some(now - Duration::from_millis(100));
                let mut outer = vertical(1200.0);
                outer.last_activity = Some(now);
                outer.child = Some(Box::new(child));
                assert_eq!(
                    outer.next_wake_deadline(),
                    Some(now + Duration::from_millis(800))
                );
            }

            {
                let mut s = vertical(1200.0);
                s.fling(0.0, 120.0);
                assert!(
                    s.needs_animation_frame(),
                    "momentum must not depend on scrollbar opacity"
                );
                let doc = AutoCommit::new();
                // Match the admitted primary-frame lifecycle: update runs even when
                // the prior frame consumed all ordinary dirty flags. Frames are
                // 1/60 s apart on the clock the glide reads.
                let mut frame = crate::time::now();
                for _ in 0..240 {
                    if !s.needs_animation_frame() {
                        break;
                    }
                    frame += Duration::from_nanos(16_666_667);
                    crate::time::testing::with_monotonic_time(frame, || {
                        s.update(&doc, &automerge::ROOT);
                    });
                    s.sync(&doc, &automerge::ROOT);
                    s.mark_drawn();
                }
                assert!(s.offset_y() > 0.0, "the demanded frames advance content");
                assert_eq!(s.momentum_speed(), 0.0);
                assert!(
                    !s.needs_animation_frame(),
                    "settled momentum must stop the pump"
                );
            }
        });
    }

    #[test]
    fn primary_scroll_shares_inset_geometry_and_native_offset() {
        use crate::widget::PrimaryScrollAction;
        let mut scroll = Scrollable::new_vertical().with_bottom_content_inset(12.0);
        scroll.set_viewport_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
        scroll.set_content_size(400.0, 500.0);
        scroll.apply_primary_vertical_scroll(PrimaryScrollAction::End);
        let m = scroll.primary_vertical_scroll().unwrap().metrics;
        assert_eq!(m.offset, 212.0);
        assert_eq!(m.viewport, 288.0);
        assert_eq!(scroll.offset_y(), 212.0);
        scroll.set_offset_y(48.0);
        assert_eq!(
            scroll.primary_vertical_scroll().unwrap().metrics.offset,
            48.0
        );
        scroll.set_content_size(400.0, 100.0);
        assert_eq!(
            scroll.primary_vertical_scroll().unwrap().metrics.offset,
            0.0
        );
    }
    #[test]
    fn persistent_overflow_bar_paints_before_input_and_after_idle_without_ticks() {
        use crate::headless::FrameHarness;
        use crate::widget::{Column, Text};
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let mut content = Column::new().with_no_padding().with_intrinsic_height();
            for index in 0..71 {
                content = content.push(Box::new(Text::new(&format!("Application {index}"))));
            }
            let mut host = FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(
                    Scrollable::new_vertical()
                        .always_visible()
                        .with_child(Box::new(content)),
                ),
                (400, 300),
                theme.clone(),
            );
            host.with_widget_mut(|scroll: &mut Scrollable| {
                assert!(
                    scroll.max_offset_y() > 0.0,
                    "real laid-out content must overflow"
                );
                let track = scroll.vertical_track().unwrap();
                assert!(
                    scroll.child.as_ref().unwrap().get_rect().x1 <= track.x0,
                    "the persistent bar must not cover content"
                );
                scroll.last_activity = None;
                for activity in [None, Some(crate::time::now() - Duration::from_secs(30))] {
                    scroll.last_activity = activity;
                    let mut scene = Scene::new();
                    scroll.draw_scrollbars(&mut scene, Affine::IDENTITY, &theme);
                    assert!(
                        !scene.encoding().path_tags.is_empty(),
                        "overflow must paint without hover or input"
                    );
                    assert_eq!(scroll.bar_opacity(&theme), 1.0);
                    assert!(
                        !scroll.needs_animation_frame(),
                        "persistent bars must not animate at rest"
                    );
                    assert!(scroll.next_wake_deadline().is_none(), "no idle fade wake");
                }
                scroll.set_content_size(100.0, 100.0);
                let mut scene = Scene::new();
                scroll.draw_scrollbars(&mut scene, Affine::IDENTITY, &theme);
                assert!(
                    scene.encoding().path_tags.is_empty(),
                    "fitting content has no bar"
                );
            })
            .unwrap();
        }
    }

    #[test]
    fn stable_vertical_gutter_keeps_media_columns_across_overflow() {
        use crate::headless::FrameHarness;
        use crate::widget::{MediaAspect, MediaCard, MediaGrid, MediaSource};
        for theme in [
            Theme::dark(),
            Theme::light(),
            Theme::dark().with_text_scale(2.0),
        ] {
            let mount = |count| {
                let cards = (0..count)
                    .map(|i| {
                        MediaCard::new(format!("gutter-card-{i}"), MediaSource::Placeholder)
                            .aspect(MediaAspect::Wide)
                            .title("A media title")
                            .meta("The creator")
                    })
                    .collect();
                FrameHarness::new_with_theme(
                    crate::State::new(),
                    Box::new(
                        Scrollable::new_vertical()
                            .with_stable_vertical_gutter()
                            .with_child(Box::new(MediaGrid::new(cards))),
                    ),
                    (952, 800),
                    theme.clone(),
                )
            };
            let mut short = mount(3);
            let mut long = mount(12);
            let short_root = short.inspection().root;
            let long_root = long.inspection().root;
            let short_grid = &short_root.children[0];
            let long_grid = &long_root.children[0];
            assert_eq!(
                short_grid.rect[2], long_grid.rect[2],
                "the short page keeps the same content width as its overflowing sibling"
            );
            assert!(
                (short_grid.children[0].rect[2] - long_grid.children[0].rect[2]).abs() < 1.0,
                "the real measured media cards retain equal widths"
            );
            let short_bar = short
                .with_widget_mut(|scroll: &mut Scrollable| scroll.vbar_active())
                .unwrap();
            let long_bar = long
                .with_widget_mut(|scroll: &mut Scrollable| scroll.vbar_active())
                .unwrap();
            assert!(
                !short_bar,
                "a stable empty lane does not fabricate a scrollbar"
            );
            assert!(long_bar, "overflow still exposes the real scrollbar");
            assert!(
                (short_grid.rect[2] - (952.0 - f64::from(theme.tokens.metrics.scrollbar_gutter)))
                    .abs()
                    < 0.1,
                "the reserved lane follows the actual theme metric"
            );
        }
    }
}
