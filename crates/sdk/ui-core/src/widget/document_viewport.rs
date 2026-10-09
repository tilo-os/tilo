//! `DocumentViewport` — a reusable zoom/pan viewport for document & canvas
//! content: cursor-focal Ctrl+wheel zoom, centre zoom for +/- buttons,
//! fit-reset, drag-to-pan, arrow-key pan, Shift+wheel horizontal pan, and
//! scrollbars that appear only on overflow.
//!
//! Two entry points share the same [`ViewportState`] math: the kernel
//! itself, for a canvas-root app that owns its own draw + event handling;
//! or [`DocumentViewport`], the composing `Widget` that wraps a child
//! [`Element`] (or draw closure).
//!
//! `screen = content * zoom + viewport_top_left + pad - scroll`, recovered
//! exactly by [`ViewportState::content_to_screen`] /
//! [`ViewportState::screen_to_content`].

use crate::time::Instant;
use crate::widget::{AppModel, ModelPath};

use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::scrollbar::{PressOutcome, ScrollMetrics, Scrollbar};
use crate::widget::{Element, Widget};

/// Default lower zoom bound (25 %).
pub const DEFAULT_MIN_ZOOM: f64 = 0.25;
/// Default upper zoom bound (600 %).
pub const DEFAULT_MAX_ZOOM: f64 = 6.0;
/// Default padding (screen px) reserved around the content so it doesn't
/// sit flush against the viewport edges. Matches the PDF reader's feel.
pub const DEFAULT_PADDING: f64 = 24.0;
/// Arrow-key pan step in screen px.
pub const DEFAULT_PAN_STEP: f64 = 80.0;
/// A drag shorter than this (screen px) is a click, not a pan — lets a host
/// distinguish a selection tap from a drag-to-pan gesture.
pub const PAN_SLOP: f64 = 4.0;
/// Thickness (screen px) of a scrollbar track/thumb.
const SCROLLBAR_THICKNESS: f64 = 8.0;
/// Inset (screen px) of the scrollbar from the viewport edge.
const SCROLLBAR_INSET: f64 = 3.0;
/// Largest per-frame `dt` (seconds) honoured by drag inertia bookkeeping;
/// the viewport itself has no easing, but the field is clamped for parity
/// with the animated widgets in case a future smooth-zoom is added.
const MAX_FRAME_DT: f64 = 0.1;

/// Composes two affine transforms. `Affine`'s `Mul` impl is pure `f64`
/// matrix multiply-add with no overflow trap, so it cannot panic regardless
/// of operand magnitude — centralised here rather than repeating the
/// exemption at every composed transform in this module.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix math and cannot panic"
)]
fn compose(a: Affine, b: Affine) -> Affine {
    a * b
}

/// How the content is positioned along an axis when it is *smaller* than
/// the viewport along that axis (i.e. nothing to scroll).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmallContentAlign {
    /// Pin the content to the start (top / left) of the viewport.
    Start,
    /// Centre the content in the viewport (the PDF reader's behaviour for
    /// the horizontal axis when the page fits).
    Center,
}

/// The pure zoom/pan state kernel. Owns the zoom factor and the scroll
/// offsets, derives the content↔screen transform, and reduces input events
/// (wheel, drag, arrow keys) into new state. Render-agnostic and cheap to
/// clone — a host stores one of these and drives it from its own event +
/// draw paths. See the module docs for the two usage shapes.
#[derive(Clone, Debug)]
pub struct ViewportState {
    /// The viewport's on-screen rect (the window region the content is
    /// shown in). Set every frame by the host (or by [`DocumentViewport`]'s
    /// `apply_layout`).
    rect: Rect,
    /// Intrinsic content size in content units.
    content_w: f64,
    content_h: f64,
    /// Current uniform zoom (content units → screen px).
    zoom: f64,
    /// Scroll offset in screen px along each axis (how far the content's
    /// top-left has been pushed up / left past the viewport's padded
    /// origin). Always within `0..=max_scroll`.
    scroll_x: f64,
    scroll_y: f64,
    /// Inclusive zoom bounds.
    min_zoom: f64,
    max_zoom: f64,
    /// Screen-px padding reserved around the content. Follows the theme's
    /// `spacing.s6` from `build_layout` unless a host pinned it with
    /// [`Self::with_padding`]; [`DEFAULT_PADDING`] is the theme-less value
    /// a viewport has before its first layout.
    pad: f64,
    /// `with_padding` was called: keep the host's value over the theme's.
    pad_overridden: bool,
    /// Per-axis small-content alignment.
    align_x: SmallContentAlign,
    align_y: SmallContentAlign,
    /// In-progress drag-to-pan, if any. Records the screen point + scroll
    /// offsets captured at press so motion maps 1:1.
    pan: Option<PanAnchor>,
    /// Wall-clock anchor of the last reduced frame; clamped delta is
    /// available to hosts that layer their own motion on top. Optional so
    /// the first frame doesn't see a huge delta.
    last_tick: Option<Instant>,
    /// Shared vertical-bar interaction model (thumb drag + track paging +
    /// hover). See [`crate::widget::scrollbar`]. Pressing the thumb drags
    /// it directly rather than pan-grabbing the content beneath it.
    vbar: Scrollbar,
    /// Shared horizontal-bar interaction model.
    hbar: Scrollbar,
}

/// Snapshot captured at pan-press so cursor motion maps 1:1 to scroll.
#[derive(Clone, Copy, Debug)]
struct PanAnchor {
    start: Point,
    scroll_x0: f64,
    scroll_y0: f64,
    /// Set once the pointer travels past [`PAN_SLOP`] — lets the host treat
    /// a no-move press as a click.
    moved: bool,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewportState {
    /// A fresh viewport at 100 % zoom, no scroll, default bounds + padding,
    /// content centred horizontally and top-aligned vertically (the common
    /// document feel). Set the content size with
    /// [`Self::set_content_size`] and the rect with [`Self::set_rect`]
    /// before drawing.
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            content_w: 0.0,
            content_h: 0.0,
            zoom: 1.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            min_zoom: DEFAULT_MIN_ZOOM,
            max_zoom: DEFAULT_MAX_ZOOM,
            pad: DEFAULT_PADDING,
            pad_overridden: false,
            align_x: SmallContentAlign::Center,
            align_y: SmallContentAlign::Start,
            pan: None,
            last_tick: None,
            vbar: Scrollbar::vertical(),
            hbar: Scrollbar::horizontal(),
        }
    }

    // ---- builder-style configuration --------------------------------

    /// Override the inclusive zoom bounds. `min` is clamped `> 0` and `max`
    /// is raised to at least `min`.
    #[must_use]
    pub fn with_zoom_bounds(mut self, min: f64, max: f64) -> Self {
        self.min_zoom = min.max(1e-3);
        self.max_zoom = max.max(self.min_zoom);
        self.zoom = self.zoom.clamp(self.min_zoom, self.max_zoom);
        self
    }

    /// Override the screen-px padding reserved around the content. A pinned
    /// value stays put across layouts; without it the padding is the
    /// theme's `spacing.s6` (24 px at the default density).
    #[must_use]
    pub fn with_padding(mut self, pad: f64) -> Self {
        self.pad = pad.max(0.0);
        self.pad_overridden = true;
        self
    }

    /// Override how content is aligned along each axis when it is smaller
    /// than the viewport on that axis.
    #[must_use]
    pub fn with_alignment(
        mut self,
        align_x: SmallContentAlign,
        align_y: SmallContentAlign,
    ) -> Self {
        self.align_x = align_x;
        self.align_y = align_y;
        self
    }

    // ---- host setters -----------------------------------------------

    /// Set the on-screen viewport rect. Re-clamps the scroll so the content
    /// stays reachable after a resize. Call once per layout pass.
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
        self.clamp_scroll();
    }

    /// Declare the content's intrinsic size (content units). Re-clamps the
    /// scroll. Call whenever the document size changes (page added, image
    /// swapped, …).
    pub fn set_content_size(&mut self, width: f64, height: f64) {
        self.content_w = width.max(0.0);
        self.content_h = height.max(0.0);
        self.clamp_scroll();
    }

    // ---- read-only accessors ----------------------------------------

    pub fn rect(&self) -> Rect {
        self.rect
    }
    /// Current uniform zoom (content units → screen px).
    pub fn zoom(&self) -> f64 {
        self.zoom
    }
    /// Current scroll offset (screen px) along x.
    pub fn scroll_x(&self) -> f64 {
        self.scroll_x
    }
    /// Current scroll offset (screen px) along y.
    pub fn scroll_y(&self) -> f64 {
        self.scroll_y
    }
    /// Inclusive zoom bounds `(min, max)`.
    pub fn zoom_bounds(&self) -> (f64, f64) {
        (self.min_zoom, self.max_zoom)
    }
    /// Declared content size `(width, height)` in content units.
    pub fn content_size(&self) -> (f64, f64) {
        (self.content_w, self.content_h)
    }

    // ---- geometry ---------------------------------------------------

    /// Total screen width the content occupies at the current zoom, plus
    /// both side pads.
    pub fn scaled_content_width(&self) -> f64 {
        self.content_w * self.zoom + 2.0 * self.pad
    }
    /// Total screen height the content occupies at the current zoom, plus
    /// both pads.
    pub fn scaled_content_height(&self) -> f64 {
        self.content_h * self.zoom + 2.0 * self.pad
    }

    /// Maximum horizontal scroll (screen px). Zero when the content fits.
    pub fn max_scroll_x(&self) -> f64 {
        (self.scaled_content_width() - self.rect.width()).max(0.0)
    }
    /// Maximum vertical scroll (screen px). Zero when the content fits.
    pub fn max_scroll_y(&self) -> f64 {
        (self.scaled_content_height() - self.rect.height()).max(0.0)
    }

    /// Whether the content overflows the viewport horizontally (scrollbar +
    /// horizontal pan become live).
    pub fn overflows_x(&self) -> bool {
        self.max_scroll_x() > 1.0
    }
    /// Whether the content overflows the viewport vertically.
    pub fn overflows_y(&self) -> bool {
        self.max_scroll_y() > 1.0
    }

    /// Clamp both scroll offsets into their valid ranges. Idempotent; called
    /// internally after every mutation.
    pub fn clamp_scroll(&mut self) {
        self.scroll_x = self.scroll_x.clamp(0.0, self.max_scroll_x());
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll_y());
    }

    /// Screen-space position of the content origin `(0, 0)` along x, taking
    /// alignment-when-small into account.
    fn origin_x(&self) -> f64 {
        if self.overflows_x() {
            self.rect.x0 + self.pad - self.scroll_x
        } else {
            match self.align_x {
                SmallContentAlign::Start => self.rect.x0 + self.pad,
                SmallContentAlign::Center => {
                    let scaled = self.content_w * self.zoom;
                    self.rect.x0 + ((self.rect.width() - scaled) / 2.0).max(self.pad)
                }
            }
        }
    }
    /// Screen-space position of the content origin `(0, 0)` along y.
    fn origin_y(&self) -> f64 {
        if self.overflows_y() {
            self.rect.y0 + self.pad - self.scroll_y
        } else {
            match self.align_y {
                SmallContentAlign::Start => self.rect.y0 + self.pad,
                SmallContentAlign::Center => {
                    let scaled = self.content_h * self.zoom;
                    self.rect.y0 + ((self.rect.height() - scaled) / 2.0).max(self.pad)
                }
            }
        }
    }

    /// Screen-space top-left of the content (`content (0,0)` mapped to
    /// screen). Useful for hosts that want the origin directly.
    pub fn content_origin(&self) -> Point {
        Point::new(self.origin_x(), self.origin_y())
    }

    /// The affine that maps **content space → screen space**: scale by
    /// `zoom` then translate to the content origin. A host draws its content
    /// under `transform * content_to_screen()` and everything zooms/pans
    /// correctly. Pure function of the current state.
    pub fn content_to_screen(&self) -> Affine {
        let o = self.content_origin();
        compose(
            Affine::translate(Vec2::new(o.x, o.y)),
            Affine::scale(self.zoom),
        )
    }

    /// Map a single content-space point to screen space.
    pub fn content_point_to_screen(&self, p: Point) -> Point {
        let o = self.content_origin();
        Point::new(o.x + p.x * self.zoom, o.y + p.y * self.zoom)
    }

    /// Map a screen-space point back into content space — the inverse of
    /// [`Self::content_to_screen`]. Use this to hit-test a pointer event
    /// against content. Robust even at extreme zooms (zoom is always
    /// `>= min_zoom > 0`).
    pub fn screen_to_content(&self, p: Point) -> Point {
        let o = self.content_origin();
        Point::new((p.x - o.x) / self.zoom, (p.y - o.y) / self.zoom)
    }

    /// Whether a screen point lands within the content's drawn area (its
    /// scaled rect). Hosts can use it to decide whether a click hits the
    /// document or empty viewport margin.
    pub fn content_contains_screen(&self, p: Point) -> bool {
        let o = self.content_origin();
        let r = Rect::new(
            o.x,
            o.y,
            o.x + self.content_w * self.zoom,
            o.y + self.content_h * self.zoom,
        );
        r.contains(p)
    }

    // ---- zoom -------------------------------------------------------

    /// Zoom by `factor` (e.g. `1.1` to zoom in 10 %) holding the content
    /// point currently under `focal` (a **screen** point) stationary, so a
    /// Ctrl+wheel zoom tracks the cursor and the content under it doesn't
    /// drift. Clamped to the zoom bounds; a no-op at the bound.
    pub fn zoom_at(&mut self, factor: f64, focal: Point) {
        let old_zoom = self.zoom;
        let new_zoom = (old_zoom * factor).clamp(self.min_zoom, self.max_zoom);
        if (new_zoom - old_zoom).abs() < 1e-9 {
            return;
        }
        // The content point under the focal screen point right now.
        let doc = self.screen_to_content(focal);
        self.zoom = new_zoom;
        // After re-scaling, re-derive the scroll so `doc` maps back onto
        // `focal`. Solve focal = origin + doc*zoom with the *overflow*
        // origin form (origin = rect_edge + pad - scroll), per axis. When an
        // axis doesn't overflow at the new zoom the scroll is just zeroed —
        // the alignment math then re-centres / pins it.
        if self.scaled_content_width() - self.rect.width() > 1.0 {
            // focal.x = rect.x0 + pad - scroll_x + doc.x*zoom
            self.scroll_x = self.rect.x0 + self.pad + doc.x * self.zoom - focal.x;
        } else {
            self.scroll_x = 0.0;
        }
        if self.scaled_content_height() - self.rect.height() > 1.0 {
            self.scroll_y = self.rect.y0 + self.pad + doc.y * self.zoom - focal.y;
        } else {
            self.scroll_y = 0.0;
        }
        self.clamp_scroll();
    }

    /// Zoom by `factor` holding the **viewport centre** fixed — the +/-
    /// buttons and Ctrl+Plus / Ctrl+Minus path.
    pub fn zoom_by(&mut self, factor: f64) {
        let c = self.rect.center();
        self.zoom_at(factor, Point::new(c.x, c.y));
    }

    /// Set the zoom to an absolute value, holding the viewport centre fixed.
    pub fn set_zoom(&mut self, zoom: f64) {
        let target = zoom.clamp(self.min_zoom, self.max_zoom);
        if self.zoom <= 0.0 {
            self.zoom = target;
            self.clamp_scroll();
            return;
        }
        self.zoom_by(target / self.zoom);
    }

    /// The zoom that fits the whole content inside the viewport (both axes
    /// visible), honouring padding. Clamped to the zoom bounds. Returns
    /// `1.0` if the viewport or content is degenerate.
    pub fn fit_zoom(&self) -> f64 {
        let avail_w = self.rect.width() - 2.0 * self.pad;
        let avail_h = self.rect.height() - 2.0 * self.pad;
        if self.content_w <= 0.0 || self.content_h <= 0.0 || avail_w <= 0.0 || avail_h <= 0.0 {
            return 1.0;
        }
        (avail_w / self.content_w)
            .min(avail_h / self.content_h)
            .clamp(self.min_zoom, self.max_zoom)
    }

    /// The zoom that fits the content's *width* to the viewport.
    pub fn fit_width_zoom(&self) -> f64 {
        let avail_w = self.rect.width() - 2.0 * self.pad;
        if self.content_w <= 0.0 || avail_w <= 0.0 {
            return 1.0;
        }
        (avail_w / self.content_w).clamp(self.min_zoom, self.max_zoom)
    }

    /// Reset to fit-the-whole-content zoom, scrolled to the top-left. The
    /// Ctrl+0 path.
    pub fn reset_to_fit(&mut self) {
        self.zoom = self.fit_zoom();
        self.scroll_x = 0.0;
        self.scroll_y = 0.0;
        self.clamp_scroll();
    }

    /// Reset to 100 % zoom, scrolled to the top-left. An alternative Ctrl+0
    /// behaviour for hosts that prefer true 1:1 over fit.
    pub fn reset_to_actual_size(&mut self) {
        self.zoom = (1.0_f64).clamp(self.min_zoom, self.max_zoom);
        self.scroll_x = 0.0;
        self.scroll_y = 0.0;
        self.clamp_scroll();
    }

    /// Zoom so `area` (content units) fills the viewport, honouring
    /// padding, and centre it as far as the scroll range allows: the "zoom to
    /// selection" path. A degenerate area keeps the zoom and only centres.
    pub fn zoom_to_rect(&mut self, area: Rect) {
        let avail_w = self.rect.width() - 2.0 * self.pad;
        let avail_h = self.rect.height() - 2.0 * self.pad;
        if area.width() > 0.0 && area.height() > 0.0 && avail_w > 0.0 && avail_h > 0.0 {
            self.zoom = (avail_w / area.width())
                .min(avail_h / area.height())
                .clamp(self.min_zoom, self.max_zoom);
        }
        let centre = area.center();
        let view = self.rect.center();
        self.scroll_x = self.rect.x0 + self.pad + centre.x * self.zoom - view.x;
        self.scroll_y = self.rect.y0 + self.pad + centre.y * self.zoom - view.y;
        self.clamp_scroll();
    }

    // ---- pan --------------------------------------------------------

    /// Pan by a screen-space delta (positive `dx`/`dy` reveal content to the
    /// right / below). Clamped so the content stays reachable.
    pub fn pan_by(&mut self, dx: f64, dy: f64) {
        self.scroll_x += dx;
        self.scroll_y += dy;
        self.clamp_scroll();
    }

    /// Set the scroll offsets directly (screen px), clamped.
    pub fn set_scroll(&mut self, scroll_x: f64, scroll_y: f64) {
        self.scroll_x = scroll_x;
        self.scroll_y = scroll_y;
        self.clamp_scroll();
    }

    /// Begin a drag-to-pan at `start` (screen point). Captures the current
    /// scroll so motion maps 1:1.
    pub fn begin_pan(&mut self, start: Point) {
        self.pan = Some(PanAnchor {
            start,
            scroll_x0: self.scroll_x,
            scroll_y0: self.scroll_y,
            moved: false,
        });
    }

    /// Continue a drag-to-pan: move so the content tracks the cursor. Past
    /// [`PAN_SLOP`] the drag is flagged as a real pan. Returns `true` while a
    /// pan is in progress. No-op when no pan is active.
    pub fn update_pan(&mut self, cursor: Point) -> bool {
        let Some(anchor) = self.pan else { return false };
        let dx = cursor.x - anchor.start.x;
        let dy = cursor.y - anchor.start.y;
        if dx.hypot(dy) > PAN_SLOP {
            self.scroll_x = anchor.scroll_x0 - dx;
            self.scroll_y = anchor.scroll_y0 - dy;
            self.clamp_scroll();
            if let Some(a) = self.pan.as_mut() {
                a.moved = true;
            }
        }
        true
    }

    /// End a drag-to-pan. Returns `true` if the gesture actually moved the
    /// content (so the host can treat a no-move release as a click).
    pub fn end_pan(&mut self) -> bool {
        self.pan.take().is_some_and(|a| a.moved)
    }

    /// Whether a drag-to-pan is currently in progress.
    pub fn is_panning(&self) -> bool {
        self.pan.is_some()
    }

    // ---- raw event reducers (used by the composing widget; reusable by
    //      canvas-root hosts that want the whole gesture set wired) -----

    /// Reduce a mouse-wheel event into zoom/pan, mirroring the PDF reader's
    /// mapping:
    ///   * **Ctrl/Cmd + wheel** → zoom centred on `cursor` (focal).
    ///   * **Shift + wheel** (or a horizontal wheel) → horizontal pan.
    ///   * otherwise → vertical pan (with any horizontal wheel component).
    ///
    /// `cursor` is the current pointer position in screen space. Returns
    /// `true` if the viewport consumed the event (it always does when it can
    /// scroll/zoom in the requested direction).
    pub fn on_wheel(
        &mut self,
        delta: &MouseScrollDelta,
        modifiers: &winit::keyboard::ModifiersState,
        cursor: Point,
        scale_factor: f64,
    ) -> bool {
        let (raw_x, raw_y) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (f64::from(*x) * 56.0, f64::from(*y) * 56.0),
            MouseScrollDelta::PixelDelta(pos) => (pos.x / scale_factor, pos.y / scale_factor),
        };
        let ctrl = modifiers.control_key() || modifiers.super_key();
        if ctrl {
            let factor = if raw_y > 0.0 {
                1.0 + raw_y / 240.0
            } else {
                1.0 / (1.0 - raw_y / 240.0)
            };
            self.zoom_at(factor, cursor);
            return true;
        }
        if modifiers.shift_key() && self.overflows_x() {
            self.scroll_x -= raw_y;
        } else {
            self.scroll_x -= raw_x;
            self.scroll_y -= raw_y;
        }
        self.clamp_scroll();
        true
    }

    /// Reduce an arrow-key press into a pan / zoom action. Returns `true` if
    /// the key was consumed. Mapping:
    ///   * arrows → pan by [`DEFAULT_PAN_STEP`] in the corresponding direction;
    ///   * Ctrl+`+`/`=` → zoom in (centre); Ctrl+`-` → zoom out (centre);
    ///   * Ctrl+`0` → [`Self::reset_to_fit`].
    ///
    /// A host that wants the arrow keys to *also* page-step when the content
    /// doesn't overflow can check [`Self::overflows_x`] / [`Self::overflows_y`]
    /// before delegating here.
    pub fn on_key(&mut self, key: &Key, modifiers: &winit::keyboard::ModifiersState) -> bool {
        let ctrl = modifiers.control_key() || modifiers.super_key();
        match key {
            Key::Named(NamedKey::ArrowLeft) => {
                self.pan_by(-DEFAULT_PAN_STEP, 0.0);
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.pan_by(DEFAULT_PAN_STEP, 0.0);
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.pan_by(0.0, -DEFAULT_PAN_STEP);
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.pan_by(0.0, DEFAULT_PAN_STEP);
                true
            }
            Key::Character(c) if ctrl => match c.as_str() {
                "+" | "=" => {
                    self.zoom_by(1.15);
                    true
                }
                "-" | "_" => {
                    self.zoom_by(1.0 / 1.15);
                    true
                }
                "0" => {
                    self.reset_to_fit();
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Record a frame tick and return the clamped seconds since the last
    /// call. The viewport has no easing of its own, but exposing the clamped
    /// delta lets a host layer smooth-scroll / kinetic motion on top without
    /// re-deriving the clock. The first call returns `0.0`.
    pub fn frame_dt(&mut self) -> f64 {
        let now = crate::time::now();
        let dt = self.last_tick.map_or(0.0, |t| {
            now.duration_since(t).as_secs_f64().min(MAX_FRAME_DT)
        });
        self.last_tick = Some(now);
        dt
    }

    // ---- scrollbar geometry -----------------------------------------

    /// The horizontal scrollbar track rect (screen space), or `None` when
    /// the content fits horizontally. Leaves room for the vertical bar so
    /// the two don't overlap in the corner.
    fn h_track(&self) -> Option<Rect> {
        if !self.overflows_x() {
            return None;
        }
        let x0 = self.rect.x0 + SCROLLBAR_INSET;
        let mut x1 = self.rect.x1 - SCROLLBAR_INSET;
        if self.overflows_y() {
            x1 -= SCROLLBAR_THICKNESS + SCROLLBAR_INSET;
        }
        let y1 = self.rect.y1 - SCROLLBAR_INSET;
        let y0 = y1 - SCROLLBAR_THICKNESS;
        Some(Rect::new(x0, y0, x1, y1))
    }

    /// The vertical scrollbar track rect (screen space), or `None` when the
    /// content fits vertically.
    fn v_track(&self) -> Option<Rect> {
        if !self.overflows_y() {
            return None;
        }
        let x1 = self.rect.x1 - SCROLLBAR_INSET;
        let x0 = x1 - SCROLLBAR_THICKNESS;
        let y0 = self.rect.y0 + SCROLLBAR_INSET;
        let mut y1 = self.rect.y1 - SCROLLBAR_INSET;
        if self.overflows_x() {
            y1 -= SCROLLBAR_THICKNESS + SCROLLBAR_INSET;
        }
        Some(Rect::new(x0, y0, x1, y1))
    }

    fn h_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            self.scaled_content_width(),
            self.rect.width(),
            self.scroll_x,
        )
    }
    fn v_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            self.scaled_content_height(),
            self.rect.height(),
            self.scroll_y,
        )
    }

    /// The horizontal scrollbar thumb rect (screen space), or `None` when
    /// the content fits horizontally. A host can paint it itself, or rely on
    /// [`DocumentViewport`] to do so.
    pub fn h_scrollbar_thumb(&self) -> Option<Rect> {
        let track = self.h_track()?;
        self.hbar.thumb_rect(track, self.h_metrics())
    }

    /// The vertical scrollbar thumb rect (screen space), or `None` when the
    /// content fits vertically.
    pub fn v_scrollbar_thumb(&self) -> Option<Rect> {
        let track = self.v_track()?;
        self.vbar.thumb_rect(track, self.v_metrics())
    }

    // ---- scrollbar interaction --------------------------------------

    /// Hit-test a left-press against either scrollbar. A press on a thumb
    /// begins a direct thumb drag; a press on the track pages the view
    /// toward the click. Returns `true` when the press landed on a bar, so
    /// the caller suppresses drag-to-pan / child routing beneath it.
    pub fn press_scrollbar(&mut self, cursor: Point) -> bool {
        if let Some(track) = self.v_track() {
            let m = self.v_metrics();
            match self.vbar.press(track, m, cursor) {
                PressOutcome::Thumb => return true,
                PressOutcome::Page(target) => {
                    self.scroll_y = target;
                    self.clamp_scroll();
                    return true;
                }
                PressOutcome::Miss => {}
            }
        }
        if let Some(track) = self.h_track() {
            let m = self.h_metrics();
            match self.hbar.press(track, m, cursor) {
                PressOutcome::Thumb => return true,
                PressOutcome::Page(target) => {
                    self.scroll_x = target;
                    self.clamp_scroll();
                    return true;
                }
                PressOutcome::Miss => {}
            }
        }
        false
    }

    /// Continue an active scrollbar thumb drag, mapping the cursor to a new
    /// scroll offset. Returns `true` while a bar is being dragged.
    pub fn drag_scrollbar(&mut self, cursor: Point) -> bool {
        if self.vbar.is_dragging() {
            if let Some(track) = self.v_track() {
                let m = self.v_metrics();
                if let Some(target) = self.vbar.drag_to(track, m, cursor) {
                    self.scroll_y = target;
                    self.clamp_scroll();
                }
            }
            return true;
        }
        if self.hbar.is_dragging() {
            if let Some(track) = self.h_track() {
                let m = self.h_metrics();
                if let Some(target) = self.hbar.drag_to(track, m, cursor) {
                    self.scroll_x = target;
                    self.clamp_scroll();
                }
            }
            return true;
        }
        false
    }

    /// Whether a scrollbar thumb drag is in progress.
    pub fn scrollbar_dragging(&self) -> bool {
        self.vbar.is_dragging() || self.hbar.is_dragging()
    }

    /// End any active scrollbar drag. Returns `true` when one was in
    /// progress.
    pub fn release_scrollbar(&mut self) -> bool {
        let v = self.vbar.release();
        let h = self.hbar.release();
        v || h
    }

    /// Recompute thumb hover from the cursor; returns `true` when a hover
    /// edge changed (so the caller can schedule a redraw).
    pub fn update_scrollbar_hover(&mut self, cursor: Point) -> bool {
        let mut changed = false;
        if let Some(track) = self.v_track() {
            let m = self.v_metrics();
            changed |= self.vbar.update_hover(track, m, cursor);
        } else {
            changed |= self.vbar.set_hover(false);
        }
        if let Some(track) = self.h_track() {
            let m = self.h_metrics();
            changed |= self.hbar.update_hover(track, m, cursor);
        } else {
            changed |= self.hbar.set_hover(false);
        }
        changed
    }

    /// Paint the scrollbars (when present) into `scene`. Reads the
    /// scrollbar/thumb color from the theme so it matches OS chrome. Both
    /// [`DocumentViewport`] and a canvas-root host call this after drawing
    /// content. The thumb brightens while hovered or dragged.
    pub fn draw_scrollbars(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let radius = SCROLLBAR_THICKNESS / 2.0;
        let idle = theme.tokens.colors.text_dim.with_alpha_factor(0.55);
        let active = theme.tokens.colors.text_dim.with_alpha_factor(0.85);
        if let Some(r) = self.h_scrollbar_thumb() {
            let c = if self.hbar.is_active() { active } else { idle };
            scene.fill(
                Fill::NonZero,
                transform,
                c,
                None,
                &RoundedRect::from_rect(r, radius),
            );
        }
        if let Some(r) = self.v_scrollbar_thumb() {
            let c = if self.vbar.is_active() { active } else { idle };
            scene.fill(
                Fill::NonZero,
                transform,
                c,
                None,
                &RoundedRect::from_rect(r, radius),
            );
        }
    }
}

/// A draw callback supplied to [`DocumentViewport::from_draw`]. It receives
/// the content→screen `Affine` already composed with the host transform, so
/// the closure paints in **content space** and the viewport handles the
/// zoom + pan. The viewport rect is supplied for clipping decisions.
pub type ContentDraw = Box<dyn FnMut(&mut Scene, Affine, &mut TextRenderer, &Theme, Rect)>;

/// Source of the viewport's content: either a child [`Element`] (drawn under
/// the transform) or a host draw closure.
enum Content {
    Widget(Element),
    Draw(ContentDraw),
}

/// The composing viewport widget: wraps content (a child widget or a draw
/// closure), owns the zoom/pan input, and paints scrollbars on top. See the
/// module docs for when to prefer the standalone [`ViewportState`] kernel
/// instead.
pub struct DocumentViewport {
    state: ViewportState,
    content: Content,
    /// Last cursor position in screen space (`MouseInput` carries none).
    last_cursor: Point,
    /// Inner taffy tree for a wrapped child widget (so it lays out + can be
    /// asked for its measured/intrinsic size). `None` for the draw-closure
    /// form.
    inner_taffy: Option<TaffyTree<()>>,
    content_node: Option<NodeId>,
    rect: Rect,
    dirty: bool,
    layout_theme: Theme,
}

impl DocumentViewport {
    /// Wrap a child widget. The child is drawn under the content→screen
    /// transform, so its own coordinate space *is* the content space. The
    /// host should call [`Self::set_content_size`] to declare how big that
    /// space is (the child's intrinsic size in its own units).
    pub fn new(content: Element) -> Self {
        Self {
            state: ViewportState::new(),
            content: Content::Widget(content),
            last_cursor: Point::ZERO,
            inner_taffy: None,
            content_node: None,
            rect: Rect::ZERO,
            dirty: true,
            layout_theme: Theme::dark(),
        }
    }

    /// Wrap a host draw closure instead of a child widget. The closure is
    /// invoked each frame with the content→screen transform already composed
    /// (it paints in content space). Best for canvas content that isn't a
    /// widget but where you still want the viewport to own input.
    pub fn from_draw(content_w: f64, content_h: f64, draw: ContentDraw) -> Self {
        let mut state = ViewportState::new();
        state.set_content_size(content_w, content_h);
        Self {
            state,
            content: Content::Draw(draw),
            last_cursor: Point::ZERO,
            inner_taffy: None,
            content_node: None,
            rect: Rect::ZERO,
            dirty: true,
            layout_theme: Theme::dark(),
        }
    }

    /// Override the zoom bounds (forwards to the kernel builder).
    #[must_use]
    pub fn with_zoom_bounds(mut self, min: f64, max: f64) -> Self {
        self.state = self.state.with_zoom_bounds(min, max);
        self
    }

    /// Override the content padding (forwards to the kernel builder).
    #[must_use]
    pub fn with_padding(mut self, pad: f64) -> Self {
        self.state = self.state.with_padding(pad);
        self
    }

    /// Override small-content alignment (forwards to the kernel builder).
    #[must_use]
    pub fn with_alignment(
        mut self,
        align_x: SmallContentAlign,
        align_y: SmallContentAlign,
    ) -> Self {
        self.state = self.state.with_alignment(align_x, align_y);
        self
    }

    /// Declare the content's intrinsic size (content units).
    pub fn set_content_size(&mut self, width: f64, height: f64) {
        self.state.set_content_size(width, height);
        self.dirty = true;
    }

    /// Borrow the underlying [`ViewportState`] (zoom/scroll/transform).
    pub fn state(&self) -> &ViewportState {
        &self.state
    }
    /// Mutable access to the underlying [`ViewportState`].
    pub fn state_mut(&mut self) -> &mut ViewportState {
        self.dirty = true;
        &mut self.state
    }

    /// Lay the wrapped child widget out at its own (content-space) size, so
    /// it has rects for hit-testing. Cheap; only runs for the widget form.
    fn place_child(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        let Content::Widget(child) = &mut self.content else {
            return;
        };
        let (cw, ch) = self.state.content_size();
        let mut taffy = TaffyTree::<()>::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = child.build_layout(&mut taffy, theme, &mut map);
        if cw > 0.0 && ch > 0.0 {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "narrowing content-space layout dimensions for taffy; never near f32::MAX"
            )]
            let (w, h) = (cw as f32, ch as f32);
            let _ = taffy.compute_layout_with_measure(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(w),
                    height: taffy::AvailableSpace::Definite(h),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, text_renderer) }
                    } else {
                        Size::ZERO
                    }
                },
            );
        }
        // Lay the child out at content-space origin (0,0). Its draw happens
        // under the content→screen transform, so this gives content-space
        // rects directly.
        child.apply_layout(&taffy, node, Point::ZERO);
        self.inner_taffy = Some(taffy);
        self.content_node = Some(node);
    }
}

impl Widget for DocumentViewport {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if let Content::Widget(c) = &mut self.content {
            c.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The gutter is a spacing token, not a constant: the viewport has
        // no theme when it is constructed, so it takes the token here, the
        // first place the theme is handed to it, and follows the theme's
        // density from then on unless a host pinned its own value.
        if !self.state.pad_overridden {
            self.state.pad = f64::from(theme.tokens.spacing.s6);
        }
        self.layout_theme = theme.clone();
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.state.set_rect(self.rect);
        self.dirty = true;
        let theme = self.layout_theme.clone();
        crate::text_render::with_layout_renderer(theme.text_scale, |tr| {
            self.place_child(&theme, tr)
        });
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Reconcile with the live renderer before paint so custom font
        // metrics cannot diverge from the pre-paint layout/hit targets.
        if self.dirty {
            self.place_child(theme, text_renderer);
        }
        let content_xf = compose(transform, self.state.content_to_screen());
        match &mut self.content {
            Content::Widget(c) => c.draw(scene, content_xf, text_renderer, theme, presence),
            Content::Draw(draw) => draw(scene, content_xf, text_renderer, theme, self.rect),
        }
        self.state.draw_scrollbars(scene, transform, theme);
        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                // An active scrollbar thumb drag owns the move.
                if self.state.drag_scrollbar(self.last_cursor) {
                    self.dirty = true;
                    *ctx.handled = true;
                    return;
                }
                if self.state.update_pan(self.last_cursor) {
                    self.dirty = true;
                    *ctx.handled = true;
                    return;
                }
                // Otherwise brighten the hovered thumb.
                if self.state.update_scrollbar_hover(self.last_cursor) {
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.rect.contains(self.last_cursor) {
                    // A press on a scrollbar (thumb or track) wins over both
                    // the child and drag-to-pan beneath it.
                    if self.state.press_scrollbar(self.last_cursor) {
                        self.dirty = true;
                        *ctx.handled = true;
                        return;
                    }
                    // Let the wrapped child take the press next (e.g. a
                    // button on the canvas); only start a pan if it didn't.
                    if let Content::Widget(_) = &self.content {
                        self.route_to_child(event, ctx);
                        if *ctx.handled {
                            return;
                        }
                    }
                    self.state.begin_pan(self.last_cursor);
                    self.dirty = true;
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.state.release_scrollbar() {
                    self.dirty = true;
                    *ctx.handled = true;
                    return;
                }
                if self.state.is_panning() {
                    self.state.end_pan();
                    self.dirty = true;
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.rect.contains(self.last_cursor) {
                    self.state.on_wheel(
                        delta,
                        &ctx.modifiers.state(),
                        self.last_cursor,
                        ctx.scale_factor,
                    );
                    self.dirty = true;
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed
                && self
                    .state
                    .on_key(&key_event.logical_key, &ctx.modifiers.state()) =>
            {
                self.dirty = true;
                *ctx.handled = true;
                return;
            }
            _ => {}
        }
        if !*ctx.handled {
            self.route_to_child(event, ctx);
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        match &self.content {
            Content::Widget(c) => vec![c.as_ref()],
            Content::Draw(_) => vec![],
        }
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if let Content::Widget(child) = &self.content {
            visitor(child.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        match &mut self.content {
            Content::Widget(c) => vec![c.as_mut()],
            Content::Draw(_) => vec![],
        }
    }

    fn name(&self) -> &'static str {
        "DocumentViewport"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.state.is_panning() || self.state.scrollbar_dragging()
    }

    /// Hold the pointer while panning or dragging a scrollbar thumb so the
    /// gesture keeps receiving moves even if the cursor leaves the rect.
    fn wants_pointer_capture(&self) -> bool {
        self.state.is_panning() || self.state.scrollbar_dragging()
    }
}

impl DocumentViewport {
    /// Route an event to the wrapped child, if any. Events reach the child
    /// in window/screen coordinates; its inner taffy node is passed through
    /// so child containers can recurse.
    fn route_to_child(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let (Content::Widget(c), Some(taffy), Some(node)) = (
            &mut self.content,
            self.inner_taffy.as_ref(),
            self.content_node,
        ) else {
            return;
        };
        c.handle_event(
            event,
            &mut crate::widget::EventCtx {
                taffy,
                ..ctx.reborrow(ctx.path, node)
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_gutter_and_theme_follow_spacing_tokens() {
        fn retained_layout_keeps_the_active_theme_for_child_layout() {
            let mut viewport =
                DocumentViewport::from_draw(100.0, 100.0, Box::new(|_, _, _, _, _| {}));
            let mut taffy = TaffyTree::new();
            let mut map = HashMap::new();
            viewport.build_layout(&mut taffy, &Theme::dark().with_text_scale(2.0), &mut map);
            assert_eq!(viewport.layout_theme.text_scale, 2.0);
        }

        /// The theme-less default is the spacing token it stands in for, and
        /// the scrollbar's thickness is the step below it.
        fn default_gutter_and_scrollbar_thickness_equal_the_spacing_tokens() {
            let spacing = crate::tokens::Spacing::default();
            assert_eq!(DEFAULT_PADDING, f64::from(spacing.s6));
            assert_eq!(SCROLLBAR_THICKNESS, f64::from(spacing.s2));
        }

        /// Once laid out, the gutter is the theme's `spacing.s6` — so a denser
        /// or larger UI scale moves it — unless the host pinned its own value,
        /// which then survives every layout.
        fn gutter_follows_the_theme_unless_a_host_pinned_it() {
            let theme = Theme::dark().with_ui_scale(2.0);
            let expected = f64::from(theme.tokens.spacing.s6);
            assert!(
                (expected - DEFAULT_PADDING).abs() > 1.0,
                "the scaled theme must move the token for this test to observe anything"
            );

            let noop = || -> ContentDraw { Box::new(|_, _, _, _, _| {}) };
            let mut v = DocumentViewport::from_draw(100.0, 100.0, noop());
            let mut taffy = TaffyTree::<()>::new();
            let mut map = HashMap::new();
            v.build_layout(&mut taffy, &theme, &mut map);
            assert!(
                (v.state.pad - expected).abs() < 1e-9,
                "gutter {} did not follow spacing.s6 {expected}",
                v.state.pad
            );

            let mut pinned = DocumentViewport::from_draw(100.0, 100.0, noop()).with_padding(10.0);
            let mut taffy = TaffyTree::<()>::new();
            let mut map = HashMap::new();
            pinned.build_layout(&mut taffy, &theme, &mut map);
            assert!(
                (pinned.state.pad - 10.0).abs() < 1e-9,
                "a pinned gutter was overwritten by the theme"
            );
        }

        fn frame_dt_first_call_is_zero_then_positive() {
            let mut v = kernel();
            assert_eq!(v.frame_dt(), 0.0, "first frame has no delta");
            std::thread::sleep(crate::time::Duration::from_millis(2));
            assert!(v.frame_dt() > 0.0, "subsequent frame has a positive delta");
        }

        retained_layout_keeps_the_active_theme_for_child_layout();
        default_gutter_and_scrollbar_thickness_equal_the_spacing_tokens();
        gutter_follows_the_theme_unless_a_host_pinned_it();
        frame_dt_first_call_is_zero_then_positive();
    }

    /// Build a kernel with a 1000×1400 content in an 800×600 viewport at the
    /// top-left of the window.
    fn kernel() -> ViewportState {
        let mut v = ViewportState::new();
        v.set_rect(Rect::new(0.0, 0.0, 800.0, 600.0));
        v.set_content_size(1000.0, 1400.0);
        v
    }

    #[test]
    fn viewport_transforms_round_trip_and_hit_test() {
        fn transform_and_inverse_round_trip() {
            let mut v = kernel();
            v.set_zoom(1.5);
            v.set_scroll(120.0, 240.0);
            for p in [
                Point::new(0.0, 0.0),
                Point::new(500.0, 700.0),
                Point::new(999.0, 1399.0),
            ] {
                let screen = v.content_point_to_screen(p);
                let back = v.screen_to_content(screen);
                assert!(
                    (back.x - p.x).abs() < 1e-6,
                    "x round-trip: {} vs {}",
                    back.x,
                    p.x
                );
                assert!(
                    (back.y - p.y).abs() < 1e-6,
                    "y round-trip: {} vs {}",
                    back.y,
                    p.y
                );
            }
        }

        fn content_to_screen_affine_matches_point_mapping() {
            let mut v = kernel();
            v.set_zoom(2.3);
            v.set_scroll(80.0, 60.0);
            let xf = v.content_to_screen();
            for p in [Point::new(10.0, 20.0), Point::new(640.0, 880.0)] {
                let via_affine = xf * p;
                let via_point = v.content_point_to_screen(p);
                assert!((via_affine.x - via_point.x).abs() < 1e-6);
                assert!((via_affine.y - via_point.y).abs() < 1e-6);
            }
        }

        fn screen_to_content_hit_test_inside_and_outside() {
            let mut v = kernel();
            v.set_zoom(1.0);
            // A point at the content origin maps to (0,0) content.
            let origin = v.content_origin();
            let c = v.screen_to_content(origin);
            assert!(c.x.abs() < 1e-6 && c.y.abs() < 1e-6);
            assert!(
                v.content_contains_screen(origin),
                "origin is inside content"
            );
            // Far above the content is outside.
            assert!(!v.content_contains_screen(Point::new(origin.x, origin.y - 50.0)));
        }

        transform_and_inverse_round_trip();
        content_to_screen_affine_matches_point_mapping();
        screen_to_content_hit_test_inside_and_outside();
    }

    #[test]
    fn zoom_keeps_focus_respects_bounds_and_fits() {
        fn zoom_at_keeps_focal_point_stationary() {
            let mut v = kernel();
            // Zoom in so both axes overflow, then mid-scroll so neither clamps.
            v.set_zoom(2.0);
            v.set_scroll(v.max_scroll_x() / 2.0, v.max_scroll_y() / 2.0);
            let focal = Point::new(400.0, 300.0);
            let before = v.screen_to_content(focal);
            v.zoom_at(1.3, focal);
            let after = v.screen_to_content(focal);
            assert!(
                (before.x - after.x).abs() < 1.0,
                "x focal drift {} too large",
                (before.x - after.x).abs()
            );
            assert!(
                (before.y - after.y).abs() < 1.0,
                "y focal drift {} too large",
                (before.y - after.y).abs()
            );
        }

        fn zoom_by_holds_viewport_centre() {
            let mut v = kernel();
            v.set_zoom(2.0);
            v.set_scroll(v.max_scroll_x() / 2.0, v.max_scroll_y() / 2.0);
            let centre = v.rect().center();
            let before = v.screen_to_content(centre);
            v.zoom_by(1.25);
            let after = v.screen_to_content(centre);
            assert!((before.x - after.x).abs() < 1.0);
            assert!((before.y - after.y).abs() < 1.0);
        }

        fn zoom_respects_bounds() {
            let mut v = kernel().with_zoom_bounds(0.25, 6.0);
            for _ in 0..50 {
                v.zoom_by(1.5);
            }
            assert!(v.zoom() <= 6.0 + 1e-6, "zoom must not exceed max");
            for _ in 0..50 {
                v.zoom_by(0.5);
            }
            assert!(v.zoom() >= 0.25 - 1e-6, "zoom must not fall below min");
        }

        // Exact equality is intentional: `reset_to_fit` either sets `zoom` to a
        // computed value or clamps it to `zoom_bounds().0` via `f64::clamp`,
        // which returns the bound verbatim — not an independent float
        // computation prone to precision drift.
        #[allow(
            clippy::float_cmp,
            reason = "checks whether zoom was clamped to the exact min-zoom bound, not a fuzzy tolerance"
        )]
        fn reset_to_fit_shows_whole_content() {
            let mut v = kernel();
            v.set_zoom(4.0);
            v.set_scroll(100.0, 100.0);
            v.reset_to_fit();
            // At fit zoom the content fits inside the viewport on both axes.
            assert!(!v.overflows_x() || v.zoom() == v.zoom_bounds().0);
            assert!(
                v.scaled_content_height() <= v.rect().height() + 1.0
                    || v.zoom() == v.zoom_bounds().0
            );
            assert_eq!(v.scroll_x(), 0.0);
            assert_eq!(v.scroll_y(), 0.0);
        }

        fn fit_width_matches_viewport_width() {
            let mut v = kernel();
            let z = v.fit_width_zoom();
            v.set_zoom(z);
            // content_w * zoom + 2*pad ≈ viewport width.
            assert!(
                (v.scaled_content_width() - v.rect().width()).abs() < 1.0,
                "fit-width content spans the viewport width"
            );
        }

        zoom_at_keeps_focal_point_stationary();
        zoom_by_holds_viewport_centre();
        zoom_respects_bounds();
        reset_to_fit_shows_whole_content();
        fit_width_matches_viewport_width();
    }

    #[test]
    fn pan_clamps_and_centres_content() {
        fn horizontal_pan_reaches_full_width_when_zoomed() {
            let mut v = kernel();
            v.set_zoom(3.0);
            assert!(v.overflows_x(), "3x zoom must overflow horizontally");
            // Pan to the far right.
            v.pan_by(f64::MAX, 0.0);
            let expected = (v.scaled_content_width() - v.rect().width()).max(0.0);
            assert!(
                (v.scroll_x() - expected).abs() < 1.0,
                "pan should reach the full scaled width: {} vs {}",
                v.scroll_x(),
                expected
            );
            // The right content edge is now reachable inside the viewport.
            let right_edge = v.content_point_to_screen(Point::new(1000.0, 0.0));
            assert!(
                right_edge.x <= v.rect().x1 + 1.0,
                "right content edge {} should be within the viewport right {}",
                right_edge.x,
                v.rect().x1
            );
        }

        fn vertical_pan_clamps_at_both_ends() {
            let mut v = kernel();
            v.set_zoom(2.0);
            // Clamp at the top.
            v.pan_by(0.0, -10_000.0);
            assert_eq!(v.scroll_y(), 0.0, "cannot scroll above the top");
            // Clamp at the bottom.
            v.pan_by(0.0, 10_000.0);
            assert!(
                (v.scroll_y() - v.max_scroll_y()).abs() < 1e-6,
                "cannot scroll past the bottom"
            );
        }

        fn no_overflow_means_no_scroll_and_centered_x() {
            let mut v = kernel();
            // Shrink so the content fits inside the viewport on both axes.
            v.set_zoom(0.25);
            assert!(!v.overflows_x(), "small content must not overflow x");
            assert!(!v.overflows_y(), "small content must not overflow y");
            assert_eq!(v.max_scroll_x(), 0.0);
            assert_eq!(v.max_scroll_y(), 0.0);
            // Centre alignment: content origin should be > pad and the content
            // should be horizontally centred in the viewport.
            let scaled_w = 1000.0 * v.zoom();
            let expected_x0 = v.rect().x0 + (v.rect().width() - scaled_w) / 2.0;
            let origin = v.content_origin();
            assert!(
                (origin.x - expected_x0).abs() < 1.0,
                "small content is centred horizontally: {} vs {}",
                origin.x,
                expected_x0
            );
        }

        fn drag_pan_maps_one_to_one_and_reports_movement() {
            let mut v = kernel();
            v.set_zoom(2.0);
            v.set_scroll(100.0, 100.0);
            v.begin_pan(Point::new(400.0, 300.0));
            // Tiny move under slop: not flagged moved.
            v.update_pan(Point::new(401.0, 300.0));
            // Big move: content tracks the cursor (scroll decreases by drag delta).
            v.update_pan(Point::new(450.0, 360.0));
            assert!(
                (v.scroll_x() - (100.0 - 50.0)).abs() < 1e-6,
                "x tracks drag"
            );
            assert!(
                (v.scroll_y() - (100.0 - 60.0)).abs() < 1e-6,
                "y tracks drag"
            );
            assert!(v.end_pan(), "a real drag reports movement");
        }

        fn no_move_press_is_a_click_not_a_pan() {
            let mut v = kernel();
            v.set_zoom(2.0);
            v.begin_pan(Point::new(400.0, 300.0));
            v.update_pan(Point::new(401.0, 301.0)); // under slop
            assert!(!v.end_pan(), "a no-move press is a click, not a pan");
        }

        fn arrow_keys_pan() {
            let mut v = kernel();
            v.set_zoom(2.0);
            v.set_scroll(100.0, 100.0);
            let mods = winit::keyboard::ModifiersState::empty();
            assert!(v.on_key(&Key::Named(NamedKey::ArrowRight), &mods));
            assert!((v.scroll_x() - (100.0 + DEFAULT_PAN_STEP)).abs() < 1e-6);
            assert!(v.on_key(&Key::Named(NamedKey::ArrowUp), &mods));
            assert!((v.scroll_y() - (100.0 - DEFAULT_PAN_STEP)).abs() < 1e-6);
        }

        horizontal_pan_reaches_full_width_when_zoomed();
        vertical_pan_clamps_at_both_ends();
        no_overflow_means_no_scroll_and_centered_x();
        drag_pan_maps_one_to_one_and_reports_movement();
        no_move_press_is_a_click_not_a_pan();
        arrow_keys_pan();
    }

    #[test]
    fn viewport_scrollbars_appear_track_and_drag() {
        fn scrollbars_present_only_when_overflowing() {
            let mut v = kernel();
            // Fits: no bars.
            v.set_zoom(0.25);
            assert!(v.h_scrollbar_thumb().is_none());
            assert!(v.v_scrollbar_thumb().is_none());
            // Overflows both: both bars.
            v.set_zoom(3.0);
            assert!(v.h_scrollbar_thumb().is_some(), "h-bar appears when wide");
            assert!(v.v_scrollbar_thumb().is_some(), "v-bar appears when tall");
        }

        fn scrollbar_thumb_tracks_scroll_progress() {
            let mut v = kernel();
            v.set_zoom(3.0);
            // Top-left: thumb sits at the track start.
            v.set_scroll(0.0, 0.0);
            let top = v.v_scrollbar_thumb().unwrap();
            // Bottom: thumb sits at the track end.
            v.set_scroll(0.0, v.max_scroll_y());
            let bottom = v.v_scrollbar_thumb().unwrap();
            assert!(
                bottom.y0 > top.y0,
                "v-thumb moves down as content scrolls down"
            );
        }

        fn press_thumb_begins_drag_then_move_scrolls() {
            let mut v = kernel();
            v.set_zoom(3.0);
            // set_zoom anchors on the viewport centre, so scroll is non-zero
            // after it — normalise to the top so the thumb sits at the track
            // start (same reset the geometry test above relies on).
            v.set_scroll(0.0, 0.0);
            let thumb = v.v_scrollbar_thumb().unwrap();
            let mid = Point::new(
                f64::midpoint(thumb.x0, thumb.x1),
                f64::midpoint(thumb.y0, thumb.y1),
            );
            assert!(v.press_scrollbar(mid), "press on the v-thumb is consumed");
            assert!(v.scrollbar_dragging(), "thumb press begins a drag");
            assert_eq!(v.scroll_y(), 0.0, "press alone does not scroll");
            // Drag the thumb down 100 px → content scrolls down proportionally.
            assert!(v.drag_scrollbar(Point::new(mid.x, mid.y + 100.0)));
            assert!(v.scroll_y() > 0.0, "thumb drag scrolls: {}", v.scroll_y());
            assert!(v.scroll_y() <= v.max_scroll_y(), "stays clamped");
            // A huge drag clamps at the bottom.
            v.drag_scrollbar(Point::new(mid.x, mid.y + 100_000.0));
            assert!(
                (v.scroll_y() - v.max_scroll_y()).abs() < 1.0,
                "clamped to bottom"
            );
            assert!(v.release_scrollbar(), "release ends the drag");
            assert!(!v.scrollbar_dragging());
        }

        fn press_track_pages_toward_click() {
            let mut v = kernel();
            v.set_zoom(3.0);
            v.set_scroll(0.0, 0.0); // normalise to the top (see above)
            let thumb = v.v_scrollbar_thumb().unwrap();
            let below = Point::new(f64::midpoint(thumb.x0, thumb.x1), thumb.y1 + 5.0);
            assert!(v.press_scrollbar(below), "track click consumed");
            assert!(!v.scrollbar_dragging(), "track click is not a drag");
            assert!(
                v.scroll_y() > 0.0,
                "paged down toward the click: {}",
                v.scroll_y()
            );
        }

        fn press_in_content_area_is_not_a_bar() {
            let mut v = kernel();
            v.set_zoom(3.0);
            // Middle of the viewport, far from either edge bar → pan is free.
            assert!(
                !v.press_scrollbar(Point::new(400.0, 300.0)),
                "content press leaves pan free"
            );
            assert!(!v.scrollbar_dragging());
        }

        fn horizontal_thumb_drag_scrolls_x() {
            let mut v = kernel();
            v.set_zoom(3.0);
            v.set_scroll(0.0, 0.0); // normalise to the left
            let thumb = v.h_scrollbar_thumb().unwrap();
            let mid = Point::new(
                f64::midpoint(thumb.x0, thumb.x1),
                f64::midpoint(thumb.y0, thumb.y1),
            );
            assert!(v.press_scrollbar(mid), "press on the h-thumb is consumed");
            v.drag_scrollbar(Point::new(mid.x + 80.0, mid.y));
            assert!(
                v.scroll_x() > 0.0,
                "h-thumb drag scrolls x: {}",
                v.scroll_x()
            );
            assert!(v.release_scrollbar());
        }

        scrollbars_present_only_when_overflowing();
        scrollbar_thumb_tracks_scroll_progress();
        press_thumb_begins_drag_then_move_scrolls();
        press_track_pages_toward_click();
        press_in_content_area_is_not_a_bar();
        horizontal_thumb_drag_scrolls_x();
    }

    /// Zooming to an area fills the viewport with it and centres it.
    #[test]
    fn zoom_to_rect_fills_and_centres_the_area() {
        let mut v = kernel();
        let area = Rect::new(400.0, 600.0, 600.0, 750.0);
        v.zoom_to_rect(area);
        let fit = ((800.0 - 2.0 * v.pad) / 200.0).min((600.0 - 2.0 * v.pad) / 150.0);
        assert!((v.zoom() - fit).abs() < 1e-9, "{}", v.zoom());
        let centre = v.content_point_to_screen(area.center());
        assert!(
            centre.distance(Point::new(400.0, 300.0)) < 1e-6,
            "{centre:?}"
        );
    }
}
