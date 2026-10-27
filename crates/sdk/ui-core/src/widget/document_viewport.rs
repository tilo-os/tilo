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
//!
//! A toolbar outside the viewport reaches its zoom through
//! [`DocumentViewport::with_zoom`]: the buttons write what they want, the
//! viewport writes back the factor it shows, and the level a person reads is
//! never a frame behind the canvas it names.

use crate::widget::{AppModel, ModelPath};

use std::cell::Cell;
use std::collections::HashMap;
use taffy::{NodeId, Size, TaffyTree};
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
/// How far the zoom may be from the factor a toolbar asked for and still be
/// the same zoom. Guards the publish-back loop against float noise.
const ZOOM_EPSILON: f64 = 1e-9;
/// Thickness (screen px) of a scrollbar track/thumb.
const SCROLLBAR_THICKNESS: f64 = 8.0;
/// Inset (screen px) of the scrollbar from the viewport edge.
const SCROLLBAR_INSET: f64 = 3.0;

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

/// What a control outside a [`DocumentViewport`] — a toolbar's Zoom out, its
/// level, its Zoom in — asks the content's zoom to be.
///
/// The viewport applies the request and writes back the
/// [`Self::Factor`] it now shows, so the level a person reads follows a
/// gesture (Ctrl+wheel, a pinch, the keyboard) as well as a button.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewportZoom {
    /// Fit the whole content into the viewport.
    Fit,
    /// A factor of the content's own size.
    Factor(f64),
}

impl ViewportZoom {
    /// The level as a whole percentage, for the label beside the buttons
    /// (`crate::format::percent`), or `None` for [`Self::Fit`].
    #[must_use]
    pub fn percent(self) -> Option<f64> {
        match self {
            Self::Fit => None,
            Self::Factor(factor) =>
            {
                #[allow(
                    clippy::arithmetic_side_effects,
                    reason = "a zoom factor is a small positive float and 100 is a constant"
                )]
                Some((factor * 100.0).round())
            }
        }
    }
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
            vbar: Scrollbar::vertical(),
            hbar: Scrollbar::horizontal(),
        }
    }

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

    /// Where the scroll bars paint over the content without moving with
    /// it: each bar's lane (screen space).
    pub fn scrollbar_lanes(&self) -> Vec<Rect> {
        [self.v_track(), self.h_track()]
            .into_iter()
            .flatten()
            .map(|track| track.inflate(SCROLLBAR_INSET, SCROLLBAR_INSET))
            .collect()
    }

    /// What the scroll bars paint: the vertical and the horizontal thumb,
    /// each with whether it is lit.
    pub fn scrollbar_thumbs(&self) -> [(Option<Rect>, bool); 2] {
        [
            (self.v_scrollbar_thumb(), self.vbar.is_active()),
            (self.h_scrollbar_thumb(), self.hbar.is_active()),
        ]
    }

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
    /// What the frame being drawn shows of the content, and what the last
    /// frame the host presented showed: a pan between the two that changed
    /// nothing else is offered to the host as a copy
    /// ([`crate::scroll_copy::offer`]).
    shown: crate::scroll_copy::ShownFrames<ViewShown>,
    /// Something besides a pan changed what the frame being drawn paints
    /// (`dirty` is cleared by the draw itself, before the host collects
    /// damage).
    repaint: bool,
    /// What a toolbar outside the viewport asks its zoom to be, and where the
    /// viewport writes back what it shows.
    zoom_state: Option<crate::LocalState<ViewportZoom>>,
    /// Where the scroll stands, kept across the rebuilds an app's view takes
    /// when something else on screen changes (a selection, an inspector
    /// opening): a canvas a person panned must stay panned.
    scroll_cell: Option<std::rc::Rc<Cell<(f32, f32)>>>,
}

/// Where a frame shows the content: where its origin lands and at what
/// zoom, the viewport, the content's size, the transform it is drawn with,
/// and the scroll bars' tracks, which stay put over it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ViewShown {
    origin: Point,
    zoom: f64,
    rect: Rect,
    content: (f64, f64),
    transform: Affine,
    tracks: [Option<Rect>; 2],
}

impl crate::scroll_copy::Shown for ViewShown {
    fn scroll(&self) -> Vec2 {
        -self.origin.to_vec2()
    }

    fn clip(&self) -> Rect {
        self.rect
    }

    fn transform(&self) -> Affine {
        self.transform
    }

    fn same_view(&self, other: &Self) -> bool {
        self.zoom.to_bits() == other.zoom.to_bits() && self.content == other.content
    }

    fn overlays(&self, out: &mut Vec<Rect>) {
        out.extend(
            self.tracks
                .iter()
                .flatten()
                .map(|track| track.inflate(SCROLLBAR_INSET, SCROLLBAR_INSET)),
        );
    }
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
            shown: crate::scroll_copy::ShownFrames::default(),
            repaint: true,
            zoom_state: None,
            scroll_cell: None,
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
            shown: crate::scroll_copy::ShownFrames::default(),
            repaint: true,
            zoom_state: None,
            scroll_cell: None,
        }
    }

    /// Share this viewport's zoom with controls outside it: a toolbar's Zoom
    /// out, its level and its Zoom in write [`ViewportZoom`]s here, and the
    /// viewport writes back the [`ViewportZoom::Factor`] it shows. A gesture
    /// in the viewport itself (Ctrl+wheel, a pinch, Ctrl+`+`) is published
    /// the same way, so the level a person reads never disagrees with the
    /// canvas.
    #[must_use]
    pub fn with_zoom(mut self, zoom: &crate::LocalState<ViewportZoom>) -> Self {
        self.zoom_state = Some(zoom.clone());
        self
    }

    /// Keep the scroll in `cell`, so a rebuilt viewport (the app's view is
    /// built again when the selection or the inspector changes) opens where
    /// the last one was left.
    #[must_use]
    pub fn with_scroll(mut self, cell: &std::rc::Rc<Cell<(f32, f32)>>) -> Self {
        let (x, y) = cell.get();
        self.state.set_scroll(f64::from(x), f64::from(y));
        self.scroll_cell = Some(std::rc::Rc::clone(cell));
        self
    }

    /// Tell `cell` where the scroll stands now.
    fn publish_scroll(&self) {
        let Some(cell) = &self.scroll_cell else {
            return;
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "scroll offsets are logical pixels, far inside f32"
        )]
        let shown = (self.state.scroll_x() as f32, self.state.scroll_y() as f32);
        if cell.get() != shown {
            cell.set(shown);
        }
    }

    /// Take the zoom `state` asks for, and answer with the factor the
    /// viewport shows now.
    fn apply_requested_zoom(&mut self) -> f64 {
        if let Some(state) = &self.zoom_state {
            match state.with(|wanted| *wanted) {
                ViewportZoom::Fit => self.state.reset_to_fit(),
                ViewportZoom::Factor(factor)
                    if factor.is_finite() && (factor - self.state.zoom()).abs() > ZOOM_EPSILON =>
                {
                    self.state.set_zoom(factor);
                }
                ViewportZoom::Factor(_) => {}
            }
        }
        self.state.zoom()
    }

    /// Whether a toolbar has asked for a zoom the viewport has not taken
    /// yet: the frame that repaints takes it, so a button outside the
    /// viewport needs no rebuild of its own to be obeyed.
    fn zoom_requested(&self) -> bool {
        let Some(state) = &self.zoom_state else {
            return false;
        };
        match state.with(|wanted| *wanted) {
            ViewportZoom::Fit => true,
            ViewportZoom::Factor(factor) => {
                !factor.is_finite() || (factor - self.state.zoom()).abs() > ZOOM_EPSILON
            }
        }
    }

    /// Tell `state` the factor the viewport shows now, when it says something
    /// else: a gesture or a fit resolved to a factor the toolbar is showing.
    fn publish_zoom(&self) {
        let Some(state) = &self.zoom_state else {
            return;
        };
        let shown = ViewportZoom::Factor(self.state.zoom());
        if state.with(|known| *known) != shown {
            state.set(shown);
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

    /// Where a point of the content is drawn: the transform a host that must
    /// name a place on screen (a test clicking a control on a canvas, a
    /// pointer a person sees) needs, since the content's own widgets are laid
    /// out in content coordinates.
    #[must_use]
    pub fn content_point_on_screen(&self, point: Point) -> Point {
        self.state.content_point_to_screen(point)
    }

    /// The first viewport in `root`'s tree, if any: for a host that has to
    /// say where a control inside a canvas is on screen.
    #[must_use]
    pub fn first_in(root: &dyn Widget) -> Option<&Self> {
        if let Some(viewport) = root.as_any().and_then(|it| it.downcast_ref::<Self>()) {
            return Some(viewport);
        }
        root.children().into_iter().find_map(Self::first_in)
    }
    /// Mutable access to the underlying [`ViewportState`].
    pub fn state_mut(&mut self) -> &mut ViewportState {
        self.dirty = true;
        &mut self.state
    }

    /// Lay the wrapped child widget out at its own (content-space) size, so
    /// it has rects for hit-testing. Cheap; only runs for the widget form.
    ///
    /// The child may place its own children (a graph canvas does), so this
    /// runs the host's solver — manual policies included — rather than a
    /// plain Taffy measure: a canvas laid out without them stacks its nodes
    /// in a row instead of drawing its graph.
    fn place_child(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        let Content::Widget(child) = &mut self.content else {
            return;
        };
        let (cw, ch) = self.state.content_size();
        let mut taffy = TaffyTree::<()>::new();
        let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = child.build_layout(&mut taffy, theme, &mut nodes);
        if cw > 0.0 && ch > 0.0 {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "narrowing content-space layout dimensions for taffy; never near f32::MAX"
            )]
            let (w, h) = (cw as f32, ch as f32);
            let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
            // SAFETY: `child`, `taffy` and `nodes` come from the build above
            // and stay exclusively borrowed until layout has been applied.
            unsafe {
                bindings.rebuild(child.as_ref(), &taffy, node, &nodes);
                let _ = super::layout::compute_widget_layout(
                    &mut taffy,
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(w),
                        height: taffy::AvailableSpace::Definite(h),
                    },
                    &nodes,
                    &bindings,
                    text_renderer,
                );
            }
        }
        // Lay the child out at content-space origin (0,0). Its draw happens
        // under the content→screen transform, so this gives content-space
        // rects directly.
        child.apply_layout(&taffy, node, Point::ZERO);
        // The child knows how big it really is (a graph's nodes reach past
        // the size it was given); the scroll extent follows it, so the last
        // node is reachable rather than cut off by a guess.
        let (right, bottom) = content_extent(child.as_ref());
        if (right > cw || bottom > ch) && right.is_finite() && bottom.is_finite() {
            self.state.set_content_size(cw.max(right), ch.max(bottom));
        }
        self.inner_taffy = Some(taffy);
        self.content_node = Some(node);
    }
}

/// How far `w` and everything under it reach, in content space: the union of
/// the placed rects, except under a widget that clips its children.
fn content_extent(w: &dyn Widget) -> (f64, f64) {
    let rect = w.get_rect();
    let mut right = rect.x1;
    let mut bottom = rect.y1;
    if w.child_clip_rect().is_none() {
        for child in w.children() {
            let (child_right, child_bottom) = content_extent(child);
            right = right.max(child_right);
            bottom = bottom.max(child_bottom);
        }
    }
    (right, bottom)
}

impl Widget for DocumentViewport {
    crate::provides!(dyn crate::content_zoom::ContentZoom);

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

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
        taffy.fill_leaf(node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.state.set_rect(self.rect);
        self.dirty = true;
        let theme = self.layout_theme.clone();
        crate::text_render::with_layout_renderer(theme.text_scale, |tr| {
            self.place_child(&theme, tr)
        });
        // A toolbar's request is solved once the rect it fits into, and how
        // big the content really is, are both known.
        self.apply_requested_zoom();
        self.publish_zoom();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // A request from a toolbar outside the viewport is taken here: the
        // frame that draws is the frame that answers it, so the level a
        // person reads and the canvas agree at once.
        if self.zoom_requested() {
            self.apply_requested_zoom();
            self.publish_zoom();
        }
        // Reconcile with the live renderer before paint so custom font
        // metrics cannot diverge from the pre-paint layout/hit targets.
        if self.dirty {
            self.place_child(theme, text_renderer);
        }
        let content_xf = compose(transform, self.state.content_to_screen());
        self.repaint |= self.state.is_panning() || self.state.scrollbar_dragging();
        self.shown.note(ViewShown {
            origin: self.state.content_origin(),
            zoom: self.state.zoom(),
            rect: self.rect,
            content: self.state.content_size(),
            transform: crate::scroll_copy::on_screen(transform),
            tracks: [self.state.v_track(), self.state.h_track()],
        });
        match &mut self.content {
            Content::Widget(c) => c.draw(scene, content_xf, text_renderer, theme, presence),
            Content::Draw(draw) => draw(scene, content_xf, text_renderer, theme, self.rect),
        }
        self.state.draw_scrollbars(scene, transform, theme);
        self.publish_scroll();
        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
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
                    self.repaint = true;
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
                        self.repaint = true;
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
                    self.repaint = true;
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
                    self.publish_zoom();
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
                self.publish_zoom();
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
        self.dirty
            || self.state.is_panning()
            || self.state.scrollbar_dragging()
            || self.zoom_requested()
    }

    /// A pan offers the host a copy of what stays on screen: the bars'
    /// tracks are what does not move with the content, and what changed in
    /// the wrapped widget besides moving is its own damage, placed on
    /// screen. Anything else repaints the viewport.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        let shown_changed = self
            .shown
            .frames()
            .is_none_or(|(drawing, presented)| *drawing != *presented);
        if !shown_changed && !self.repaint && !self.needs_redraw() {
            return;
        }
        // A draw closure's content says nothing of what changed in it, so
        // only a wrapped widget offers its pans.
        if let Content::Widget(child) = &self.content {
            let mut overlays = Vec::new();
            if self.shown.compare(self, &mut overlays) == crate::scroll_copy::Since::Copied {
                regions.extend(overlays);
                let mut changed = Vec::new();
                child.collect_damage(&mut changed);
                let placed = self.state.content_to_screen();
                regions.extend(
                    changed
                        .into_iter()
                        .map(|rect| placed.transform_rect_bbox(rect).intersect(self.rect))
                        .filter(|rect| rect.width() > 0.0 && rect.height() > 0.0),
                );
                return;
            }
        }
        regions.push(self.rect);
    }

    fn mark_drawn(&mut self) {
        // Not drawn this frame, what the screen shows where it lies is not
        // known to be its content: the next pan repaints the viewport.
        self.shown.present();
        self.repaint = false;
        for child in self.children_mut() {
            child.mark_drawn();
        }
    }

    /// Hold the pointer while panning or dragging a scrollbar thumb so the
    /// gesture keeps receiving moves even if the cursor leaves the rect.
    fn wants_pointer_capture(&self) -> bool {
        self.state.is_panning() || self.state.scrollbar_dragging()
    }
}

/// A canvas viewport is a widget whose content zooms by itself: the window's
/// Zoom in, Zoom out and Actual size reach the content, and the window bar
/// says the factor, instead of the whole window's text growing.
impl crate::content_zoom::ContentZoom for DocumentViewport {
    fn zoom_content(&mut self, step: crate::content_zoom::ZoomStep) -> Option<f32> {
        use crate::content_zoom::ZoomStep;
        match step {
            ZoomStep::In => self.state.zoom_by(1.15),
            ZoomStep::Out => self.state.zoom_by(1.0 / 1.15),
            // Actual size for content with a frame of its own is the frame's
            // fit, exactly as a picture fitted to its window.
            ZoomStep::Reset => self.state.reset_to_fit(),
        }
        self.dirty = true;
        self.repaint = true;
        self.publish_zoom();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a zoom factor is a small positive float"
        )]
        Some(self.state.zoom() as f32)
    }
}

impl DocumentViewport {
    /// Route an event to the wrapped child, if any.
    ///
    /// The child's widgets were laid out in *content* space, while a pointer
    /// event names a place in the window: a pointer event is mapped through
    /// the viewport's own transform first, so a control on a zoomed or
    /// panned canvas is pressed where a person sees it. The child's inner
    /// taffy node is passed through so child containers can recurse.
    fn route_to_child(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let (Content::Widget(c), Some(taffy), Some(node)) = (
            &mut self.content,
            self.inner_taffy.as_ref(),
            self.content_node,
        ) else {
            return;
        };
        let mapped;
        let event =
            match event {
                WindowEvent::CursorMoved {
                    device_id,
                    position,
                    ..
                } => {
                    let content = self.state.screen_to_content(ctx.logical(position));
                    mapped = WindowEvent::CursorMoved {
                        device_id: *device_id,
                        position: winit::dpi::PhysicalPosition::new(
                            content.x * ctx.scale_factor,
                            content.y * ctx.scale_factor,
                        ),
                    };
                    &mapped
                }
                // A touch names its place the same way, in physical pixels.
                WindowEvent::Touch(touch) => {
                    let content = self.state.screen_to_content(ctx.logical(
                        winit::dpi::PhysicalPosition::new(touch.location.x, touch.location.y),
                    ));
                    mapped = WindowEvent::Touch(winit::event::Touch {
                        location: winit::dpi::PhysicalPosition::new(
                            content.x * ctx.scale_factor,
                            content.y * ctx.scale_factor,
                        ),
                        ..*touch
                    });
                    &mapped
                }
                other => other,
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

        retained_layout_keeps_the_active_theme_for_child_layout();
        default_gutter_and_scrollbar_thickness_equal_the_spacing_tokens();
        gutter_follows_the_theme_unless_a_host_pinned_it();
    }

    /// Build a kernel with a 1000×1400 content in an 800×600 viewport at the
    /// top-left of the window.
    fn kernel() -> ViewportState {
        let mut v = ViewportState::new();
        v.set_rect(Rect::new(0.0, 0.0, 800.0, 600.0));
        v.set_content_size(1000.0, 1400.0);
        v
    }

    /// A button of `label` inside a canvas at content point (`x`, `y`), the
    /// content 600×600.
    fn canvas_with_button(label: &str, x: f64, y: f64) -> (Element, f64, f64) {
        let button = crate::widget::Button::new("canvas-button", label);
        let manual = crate::widget::Manual::new(move |cx| {
            let size = cx
                .measure_child(
                    0,
                    Size {
                        width: crate::widget::MeasureConstraint::Intrinsic,
                        height: crate::widget::MeasureConstraint::Intrinsic,
                    },
                )?
                .size;
            cx.place(
                0,
                Rect::from_origin_size((x, y), (f64::from(size.width), f64::from(size.height))),
            )?;
            Ok(Size {
                width: 600.0,
                height: 600.0,
            })
        })
        .child(Box::new(button));
        (Box::new(manual), x, y)
    }

    fn canvas_rect(harness: &crate::headless::FrameHarness, id: &str) -> Rect {
        fn find(node: &crate::headless::InspectionNode, id: &str) -> Option<Rect> {
            if node.test_id.as_deref() == Some(id) {
                let [x, y, w, h] = node.rect;
                return Some(Rect::new(x, y, x + w, y + h));
            }
            node.children.iter().find_map(|child| find(child, id))
        }
        find(&harness.inspection().root, id).expect("the canvas button is laid out")
    }

    /// A control on a zoomed canvas is pressed where a person sees it, and
    /// not where its content-space rect would put it.
    #[test]
    fn a_zoomed_canvas_control_is_pressed_where_it_is_drawn() {
        use std::cell::Cell;
        use std::rc::Rc;

        let presses = Rc::new(Cell::new(0_u32));
        let counter = Rc::clone(&presses);
        let button = crate::widget::Button::new("canvas-button", "Inspect")
            .on_effect(move || counter.set(counter.get() + 1));
        let manual = crate::widget::Manual::new(|cx| {
            let size = cx
                .measure_child(
                    0,
                    Size {
                        width: crate::widget::MeasureConstraint::Intrinsic,
                        height: crate::widget::MeasureConstraint::Intrinsic,
                    },
                )?
                .size;
            cx.place(
                0,
                Rect::from_origin_size(
                    (300.0, 300.0),
                    (f64::from(size.width), f64::from(size.height)),
                ),
            )?;
            Ok(Size {
                width: 600.0,
                height: 600.0,
            })
        })
        .child(Box::new(button));
        let mut viewport = DocumentViewport::new(Box::new(manual));
        viewport.set_content_size(600.0, 600.0);
        viewport.state_mut().set_zoom(2.0);
        let mut harness =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(viewport), (800, 900));
        let content = canvas_rect(&harness, "canvas-button");
        // The transform the canvas draws with: content × 2, its own gutter in.
        let pad = f64::from(crate::tokens::Spacing::default().s6);
        let drawn = Point::new(
            pad + content.center().x * 2.0,
            pad + content.center().y * 2.0,
        );
        assert!(
            !content.contains(drawn),
            "the drawn place and the content rect must differ for this test to say anything"
        );
        harness.click_at(drawn.x, drawn.y);
        assert_eq!(presses.get(), 1, "the press lands on the drawn control");
        harness.click_at(content.center().x, content.center().y);
        assert_eq!(
            presses.get(),
            1,
            "a press at the control's content-space rect hits nothing on screen"
        );
    }

    /// A toolbar's Zoom out, Fit and Zoom in reach the viewport, and the
    /// viewport answers with the factor it shows.
    #[test]
    fn a_toolbar_sets_the_zoom_and_the_viewport_answers_with_the_factor_it_shows() {
        let (content, _, _) = canvas_with_button("Inspect", 0.0, 0.0);
        let zoom = crate::LocalState::new(ViewportZoom::Fit);
        let mut viewport = DocumentViewport::new(content).with_zoom(&zoom);
        viewport.set_content_size(600.0, 600.0);
        let mut harness =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(viewport), (800, 600));
        let _ = harness.recompose();
        let fit = match zoom.with(|shown| *shown) {
            ViewportZoom::Factor(factor) => factor,
            ViewportZoom::Fit => panic!("the viewport answers Fit with the factor it used"),
        };
        assert!(
            fit < 1.0,
            "a 600×600 content in an 800×600 frame fits at {fit}"
        );
        zoom.set(ViewportZoom::Factor(1.0));
        let _ = harness.recompose();
        assert_eq!(
            zoom.with(|shown| *shown),
            ViewportZoom::Factor(1.0),
            "the factor a toolbar asks for is kept"
        );
        zoom.set(ViewportZoom::Fit);
        let _ = harness.recompose();
        assert_eq!(
            zoom.with(|shown| *shown),
            ViewportZoom::Factor(fit),
            "Fit is answered with the factor it resolved to"
        );
        assert_eq!(ViewportZoom::Factor(1.25).percent(), Some(125.0));
        assert_eq!(ViewportZoom::Fit.percent(), None);
    }

    /// The window's own Zoom in / Zoom out / Actual size reach a canvas that
    /// has a scale of its own, and the level the toolbar shows follows.
    #[test]
    fn the_windows_zoom_reaches_a_canvas_that_has_one_of_its_own() {
        use crate::content_zoom::{ContentZoom, ZoomStep};
        let (content, _, _) = canvas_with_button("Inspect", 0.0, 0.0);
        let zoom = crate::LocalState::new(ViewportZoom::Fit);
        let mut viewport = DocumentViewport::new(content).with_zoom(&zoom);
        viewport.set_content_size(600.0, 600.0);
        let mut text = TextRenderer::new();
        crate::widget::layout::lay_out_detached(
            &mut viewport,
            Rect::new(0.0, 0.0, 800.0, 900.0),
            &Theme::dark(),
            &mut text,
        )
        .expect("the canvas lays out");
        let fitted = viewport.state().zoom();
        let factor = viewport
            .zoom_content(ZoomStep::In)
            .expect("a canvas with a scale of its own takes the window's zoom");
        assert!(
            f64::from(factor) > fitted,
            "Zoom in draws the canvas larger: {factor} from {fitted}"
        );
        let shown = zoom.with(|shown| *shown);
        assert!(
            matches!(shown, ViewportZoom::Factor(level) if (level - f64::from(factor)).abs() < 1e-6),
            "and the toolbar is told what the canvas shows: {shown:?} for {factor}"
        );
        let reset = viewport.zoom_content(ZoomStep::Reset).expect("actual size");
        assert!(
            (f64::from(reset) - fitted).abs() < 1e-6,
            "actual size is the whole canvas fitted: {reset} vs {fitted}"
        );
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
            v.pan_by(f64::MAX, 0.0);
            let expected = (v.scaled_content_width() - v.rect().width()).max(0.0);
            assert!(
                (v.scroll_x() - expected).abs() < 1.0,
                "pan should reach the full scaled width: {} vs {}",
                v.scroll_x(),
                expected
            );
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
            v.pan_by(0.0, -10_000.0);
            assert_eq!(v.scroll_y(), 0.0, "cannot scroll above the top");
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

#[cfg(test)]
#[path = "document_viewport_copy_tests.rs"]
mod copy_tests;
