//! `WebView` — a leaf widget that displays the live output of a
//! [`crate::webview::WebViewEngine`] and forwards pointer, scroll and
//! keyboard input into it.
//!
//! The widget is deliberately chrome-free: address bar, navigation
//! buttons, progress and permission prompts belong to the host app / OS
//! chrome. It paints whatever frame the engine last produced (scaled to
//! its layout rect), shows a neutral placeholder until the first frame
//! arrives, and translates toolkit-space input into engine commands
//! (physical pixels relative to the view's origin).
//!
//! Navigation state (URL, title, progress) intentionally does NOT flow
//! through this widget — the host app drains
//! [`crate::webview::WebViewEvent`]s from the engine during `sync` and
//! owns that state in its CRDT.
//!
//! # Device scale
//!
//! The page is rendered at the device scale of the surface it is shown on,
//! so one frame pixel is one physical pixel. That scale has one source at a
//! time and nothing else changes it:
//!
//! - a live host's surface, told on mount and again when the surface's scale
//!   changes ([`crate::widget::NativeViewLifecycle`]); the window moving to
//!   a display of another density re-renders the page once;
//! - without a live host (an offscreen capture, an app embedded in a host's
//!   scene), the scale the view lands on screen with, read through every
//!   cache being recorded around it ([`crate::scroll_copy::on_screen`]): a
//!   container that caches its children draws them at the identity and
//!   scales the cache, so the draw's own transform says 1 on any display.
//!
//! Input never changes it. An event's scale is the scale of the surface
//! that delivered it, and the engine re-renders the whole page at every new
//! scale, briefly showing the old frame stretched or the new one at the old
//! size: a pointer moving over a page whose draws and events disagreed
//! re-rendered it at alternating scales on every move.

use crate::num::f32_of_f64;
use crate::widget::{AppModel, ModelPath};
use std::sync::Arc;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Blob, Fill, ImageData as PenikoImage};
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key as WKey, NamedKey as WNamedKey};

use crate::gesture::NavDirection;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::webview::{KeyInput, KeyModifiers, MouseButton, NamedKey, SharedEngine, WebViewCommand};
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Pixels scrolled per mouse-wheel line tick, in logical units (scaled
/// by the display factor before being sent to the engine).
const WHEEL_LINE_PX: f64 = 40.0;

/// Rounds a physical pixel extent to `u32`, clamping to `[0, u32::MAX]`
/// first so a negative or absurdly large rect (a transient layout glitch)
/// saturates instead of wrapping.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to u32's range immediately above, so the cast itself can never truncate or flip sign"
)]
fn round_to_u32_px(v: f64) -> u32 {
    v.round().clamp(0.0, f64::from(u32::MAX)) as u32
}

/// Backends may render the exact physical request or quantize it through an
/// integer logical viewport. Allow rounding in either direction (an engine may
/// round logical dimensions to nearest; CEF rounds outward). Integer device scales
/// need at most scale - 1 pixels; fractional scales can add a final pixel when
/// converting the logical viewport back to an integer physical buffer.
fn frame_quantization_slack(scale_factor: f32) -> u32 {
    let scale = f64::from(scale_factor);
    if !scale.is_finite() || scale <= 0.0 {
        return 0;
    }
    round_to_u32_px(if scale.fract() == 0.0 {
        scale - 1.0
    } else {
        scale.ceil()
    })
}

/// The largest page frame the engine renders, in device pixels: each side,
/// and in all (its 64 MiB frame packet of four-byte pixels), each a little
/// under the engine's own bound, which its rounding of a fractional scale
/// can pass by a pixel or two. The engine closes on a larger request.
const MAX_FRAME_SIDE: f64 = 8192.0 - 16.0;
const MAX_FRAME_PIXELS: f64 = 16.0 * 1024.0 * 1024.0 * 0.99;

/// See module docs.
pub struct WebView {
    text_input: crate::webview::text_input::Guard,
    history_operation: Option<(
        tilo_actions::InvocationId,
        crate::webview::text_input::Receipt,
    )>,
    rect: Rect,
    engine: SharedEngine,
    /// Physical pixels per logical pixel of the device the page is shown on
    /// (see the module docs, "Device scale"); the engine renders at it unless
    /// that passes its limits ([`Self::engine_scale`]).
    scale_factor: f32,
    /// `scale_factor` is the live host surface's: draws leave it alone.
    host_scaled: bool,
    /// `scale_factor` is the seed [`Self::with_scale_factor`] gave and no
    /// host has replaced yet.
    preseeded: bool,
    focused: bool,
    last_mouse: Point,
    /// The buttons pressed on the page and not yet released: the page keeps
    /// the pointer until they are, wherever it goes, as a drag does.
    held: Vec<MouseButton>,
    /// The pointer was over the page at its last move: leaving it ends the
    /// page's hover, so the page hears where it went.
    pointer_inside: bool,
    /// Physical size last sent as a `Resize`, to avoid spamming the engine.
    last_sent_size: (u32, u32),
    last_sent_scale: f32,
    /// Last expected/observed mismatch for which we requested a repaint.
    /// A matching frame clears it, so a stalled backend cannot cause a
    /// redraw-command loop on every UI frame.
    stale_resize_retry: Option<((u32, u32), (u32, u32))>,
    native_mounted: bool,
    /// Keyed by the frame buffer's allocation pointer + dimensions.
    cached_image: Option<PenikoImage>,
    cached_trace_id: Option<u64>,
    cached_key: (usize, u32, u32),
    /// What the cached frame changed from the frames before it.
    cached_damage: Option<crate::webview::FrameDamage>,
    /// What the last draw put on screen, and what the last frame that
    /// reached the screen showed ([`Widget::mark_drawn`]).
    drawing: Option<Drawn>,
    submitted: Option<Drawn>,
    /// Where the last draw differs from `submitted`, in layout coordinates;
    /// `None` for the whole view.
    damage: Option<Vec<Rect>>,
    /// The frames the view may show ([`Self::accept_frames`]); `None`
    /// shows every frame that fits.
    accepts: Option<fn(&crate::webview::Frame) -> bool>,
    /// Draws the engine's display list when it has one (the paint stream).
    page: crate::webview::paint::PageRenderer,
    test_id: String,
    accessible_name: String,
}

/// What one draw of the view put on screen of a page frame, to tell where
/// the next draw differs from it.
#[derive(Clone)]
struct Drawn {
    rect: Rect,
    transform: Affine,
    /// The colour around and under the frame.
    backing: vello::peniko::Color,
    /// The frame's pixels, held so their allocation is not another frame's.
    frame: Blob<u8>,
    damage: Option<crate::webview::FrameDamage>,
    /// Logical pixels per frame pixel, as bits.
    scale_bits: u64,
    size: (u32, u32),
}

impl Drawn {
    /// Where this draw differs from `before`, in layout coordinates: nothing
    /// when it drew the same frame the same way, the frame's changed rects
    /// when only the frame changed and it knows what changed since
    /// `before`'s, and `None` (all of it) otherwise.
    fn damage_since(&self, before: &Self) -> Option<Vec<Rect>> {
        if self.rect != before.rect
            || self.transform != before.transform
            || self.backing != before.backing
            || self.scale_bits != before.scale_bits
            || self.size != before.size
        {
            return None;
        }
        if self.frame.id() == before.frame.id() {
            return Some(Vec::new());
        }
        let generation = before.damage.as_ref()?.generation;
        let changed = self.damage.as_ref()?.since(generation)?;
        let scale = f64::from_bits(self.scale_bits);
        let painted = self.rect.intersect(Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x0 + f64::from(self.size.0) * scale,
            self.rect.y0 + f64::from(self.size.1) * scale,
        ));
        Some(
            changed
                .iter()
                .filter_map(|rect| {
                    let at = |pixels: u32| f64::from(pixels) * scale;
                    // A pixel's neighbours sample it when the frame is
                    // scaled or lands between device pixels.
                    let rect = Rect::new(
                        self.rect.x0 + at(rect.x),
                        self.rect.y0 + at(rect.y),
                        self.rect.x0 + at(rect.x.saturating_add(rect.width)),
                        self.rect.y0 + at(rect.y.saturating_add(rect.height)),
                    )
                    .inflate(1.0, 1.0)
                    .intersect(painted);
                    (rect.width() > 0.0 && rect.height() > 0.0).then_some(rect)
                })
                .collect(),
        )
    }
}

impl WebView {
    pub fn new(engine: SharedEngine) -> Self {
        Self {
            text_input: Default::default(),
            history_operation: None,
            rect: Rect::ZERO,
            engine,
            scale_factor: 1.0,
            host_scaled: false,
            preseeded: false,
            focused: false,
            last_mouse: Point::ZERO,
            held: Vec::new(),
            pointer_inside: false,
            last_sent_size: (0, 0),
            last_sent_scale: 0.0,
            stale_resize_retry: None,
            native_mounted: false,
            cached_image: None,
            cached_trace_id: None,
            cached_key: (0, 0, 0),
            cached_damage: None,
            drawing: None,
            submitted: None,
            damage: None,
            accepts: None,
            page: crate::webview::paint::PageRenderer::new(),
            test_id: "webview".to_string(),
            accessible_name: "Web content".to_string(),
        }
    }

    /// While the session shares this window's page with someone watching,
    /// hand it the page as a paint stream — the view on screen is the page
    /// the window shows. Payloads the app → shell channel cannot carry are
    /// dropped; a dropped whole frame is asked for again.
    fn share_page(&self) {
        use crate::webview::paint;
        if !paint::share_watched() {
            return;
        }
        let keyframe = paint::take_share_keyframe();
        let page = [
            self.rect.x0,
            self.rect.y0,
            self.rect.width(),
            self.rect.height(),
        ];
        for payload in self.engine.take_paint_stream(keyframe) {
            let whole = matches!(
                payload,
                crate::webview::paint::stream::wire::Payload::Key(_)
            );
            match payload.encode() {
                Ok(bytes) if bytes.len() <= paint::MAX_SHARE_PAYLOAD => {
                    paint::queue_share(paint::SharedFrame {
                        payload: bytes,
                        page,
                        scale: f64::from(self.engine_scale()),
                    });
                }
                _ => {
                    tracing::debug!(whole, "paint payload too large to share; skipped");
                }
            }
        }
    }

    /// Seed the window DPI before the first layout of a rebuilt view.
    #[must_use]
    pub fn with_scale_factor(mut self, scale_factor: f32) -> Self {
        if scale_factor.is_finite() && scale_factor > 0.0 {
            self.scale_factor = scale_factor;
            self.preseeded = true;
        }
        self
    }

    /// Override the e2e test identifier (default `"webview"`), for a tree
    /// hosting several web views.
    #[must_use]
    pub fn with_test_id(mut self, id: &str) -> Self {
        self.test_id = id.to_string();
        self
    }

    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = name.into();
        self
    }

    /// Place the view at an explicit logical rect without going through a
    /// taffy layout pass — for hosts that embed the view inside their own
    /// immediate-mode drawing (e.g. an email reader rendering an HTML body
    /// inline) rather than as a first-class widget-tree node. Sends a
    /// `Resize` when the physical size changes, like [`Widget::apply_layout`].
    /// `scale_factor` is the host surface's device scale, which draws then
    /// leave alone, as a live host's.
    pub fn set_viewport(&mut self, rect: Rect, scale_factor: f32) {
        self.rect = rect;
        if scale_factor.is_finite() && scale_factor > 0.0 {
            self.scale_factor = scale_factor;
            self.host_scaled = true;
        }
        self.send_resize_if_needed();
    }

    /// Place the view at an explicit logical rect, like
    /// [`Self::set_viewport`], rendering the page at the scale it lands on
    /// screen with: the display's, times any zoom of the content around it.
    /// For a page drawn inside a host's own document (an HTML mail), so it
    /// stays sharp on a dense display and when the reader zooms.
    pub fn place(&mut self, rect: Rect) {
        self.rect = rect;
        self.send_resize_if_needed();
    }

    /// Whether the engine's latest frame fits the view as placed: drawn,
    /// it shows the page rather than a placeholder waiting for a resize.
    #[must_use]
    pub fn has_fitting_frame(&self) -> bool {
        let expected = (
            round_to_u32_px(self.rect.width() * f64::from(self.engine_scale())),
            round_to_u32_px(self.rect.height() * f64::from(self.engine_scale())),
        );
        let slack = frame_quantization_slack(self.engine_scale());
        let slot = self.engine.frames();
        let guard = slot.lock();
        guard.ok().as_deref().is_some_and(|frame| {
            frame.as_ref().is_some_and(|frame| {
                frame.width.abs_diff(expected.0) <= slack
                    && frame.height.abs_diff(expected.1) <= slack
            })
        })
    }

    /// The view lands on screen through `transform`. Without a live host,
    /// the page renders at the scale it lands on screen with: through any
    /// cache a container records it into, which draws it at the identity
    /// (module docs, "Device scale"). [`Widget::draw`] says so itself; a
    /// host that holds the view back until its page has painted says so
    /// while it does, so the page paints at that scale before it is shown.
    pub fn lands_at(&mut self, transform: Affine) {
        if !self.host_scaled {
            let coefficients = crate::scroll_copy::on_screen(transform).as_coeffs();
            self.rescale(f32_of_f64(coefficients[0].hypot(coefficients[1])));
        }
    }

    /// Show only the frames `accepts` passes, for a host that knows its
    /// own page's frames (an HTML mail marks its document's end): a frame it
    /// rejects, or one that does not fit the view, is never drawn, and the
    /// view goes on drawing the last frame it showed. Whatever the engine
    /// delivers between a host's look at the view and its draw, the draw
    /// shows a frame the host accepted.
    pub fn accept_frames(&mut self, accepts: fn(&crate::webview::Frame) -> bool) {
        self.accepts = Some(accepts);
    }

    /// Take the engine's latest frame to draw, if the view may show it, and
    /// say whether the view has a frame to draw: what [`Widget::draw`]
    /// would put on screen now, beyond its placeholder.
    pub fn refresh_frame(&mut self) -> bool {
        self.adopt_frame();
        self.cached_image.is_some()
    }

    /// Take the engine's latest frame as the one to draw if it fits the
    /// view (and passes [`Self::accept_frames`]). A view without that
    /// filter drops its frame for a stale one, and returns the stale size.
    fn adopt_frame(&mut self) -> Option<(u32, u32)> {
        let latest = {
            let slot = self.engine.frames();
            let guard = slot.lock();
            guard.ok().and_then(|g| g.clone())
        };
        let frame = latest?;
        if !self.retry_resize_for_stale_frame((frame.width, frame.height)) {
            if self.accepts.is_some() {
                return None;
            }
            self.cached_image = None;
            self.cached_trace_id = None;
            self.cached_key = (0, 0, 0);
            return Some((frame.width, frame.height));
        }
        if self.accepts.is_some_and(|accepts| !accepts(&frame)) {
            return None;
        }
        let key = (Arc::as_ptr(&frame.rgba) as usize, frame.width, frame.height);
        if self.cached_image.is_none() || key != self.cached_key {
            let expected = (frame.width as usize)
                .saturating_mul(frame.height as usize)
                .saturating_mul(4);
            if frame.rgba.len() == expected && expected > 0 {
                let image =
                    crate::raster::rgba8(Blob::new(frame.rgba.clone()), frame.width, frame.height);
                self.note_image_delta(&image, frame.damage.as_ref());
                self.cached_image = Some(image);
                self.cached_key = key;
                self.cached_trace_id = frame.trace_id;
                self.cached_damage = frame.damage;
            }
        }
        None
    }

    /// Draws the page: its frame when the engine has one that fits, its
    /// placeholder otherwise. Returns what it drew of a frame, for
    /// [`Drawn::damage_since`].
    fn draw_page(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<Drawn> {
        // Placeholder before the first frame, letterbox backing afterwards.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        self.engine.presented();
        self.share_page();
        // A display list replaces pixels: draw it like any other widget
        // content, in this toolkit's own text and Vello.
        if let Some(page) = self.engine.paint_frame() {
            let engine = Arc::clone(&self.engine);
            let blobs = move |hash: &[u8; 32]| engine.paint_blob(hash);
            let mut canvas = crate::webview::paint::PageCanvas {
                scene,
                transform: transform * Affine::translate((self.rect.x0, self.rect.y0)),
                text: text_renderer,
                blobs: &blobs,
            };
            self.page.draw(&mut canvas, &page);
            crate::webview::profile::record_draw(self.rect.width(), self.rect.height(), None);
            return None;
        }

        if let Some(stale) = self.adopt_frame() {
            crate::webview::profile::record_draw(
                self.rect.width(),
                self.rect.height(),
                Some(stale),
            );
            return None;
        }

        if let Some(img) = self.cached_image.clone() {
            let (_, iw, ih) = self.cached_key;
            crate::webview::profile::record_draw(
                self.rect.width(),
                self.rect.height(),
                Some((iw, ih)),
            );
            if iw == 0 || ih == 0 {
                return None;
            }
            // The engine renders at physical resolution for our logical rect
            // — scale down so hi-DPI frames map 1:1 to device pixels.
            // Keep the last frame at its physical pixel size while a resize
            // awaits paint. Stretching each axis independently distorts text.
            let scale = 1.0 / f64::from(self.engine_scale());
            let brush_transform = Affine::translate((self.rect.x0, self.rect.y0)).pre_scale(scale);
            let painted = self.rect.intersect(Rect::new(
                self.rect.x0,
                self.rect.y0,
                self.rect.x0 + f64::from(iw) * scale,
                self.rect.y0 + f64::from(ih) * scale,
            ));
            crate::webview::frame_trace::drawn(self.cached_trace_id);
            let frame = img.data.clone();
            let brush = vello::peniko::ImageBrush::new(img);
            scene.fill(
                Fill::NonZero,
                transform,
                &brush,
                Some(brush_transform),
                &painted,
            );
            return Some(Drawn {
                rect: self.rect,
                transform,
                backing: theme.tokens.colors.surfaces.base,
                frame,
                damage: self.cached_damage.clone(),
                scale_bits: scale.to_bits(),
                size: (iw, ih),
            });
        }
        crate::webview::profile::record_draw(self.rect.width(), self.rect.height(), None);
        None
    }

    /// Tell the renderer that `image`, the frame about to replace the cached
    /// one, differs from it only where `damage` says, so the renderer that
    /// holds the cached frame uploads just those rects of the new one.
    fn note_image_delta(&self, image: &PenikoImage, damage: Option<&crate::webview::FrameDamage>) {
        let Some(previous) = self
            .cached_image
            .as_ref()
            .filter(|previous| (previous.width, previous.height) == (image.width, image.height))
        else {
            return;
        };
        let Some(changed) = self
            .cached_damage
            .as_ref()
            .zip(damage)
            .and_then(|(drawn, damage)| damage.since(drawn.generation))
        else {
            return;
        };
        let rects: Vec<vello::ImageRect> = changed
            .iter()
            .map(|rect| [rect.x, rect.y, rect.width, rect.height])
            .collect();
        vello::register_image_delta(previous.data.id(), image.data.id(), &rects);
    }

    /// Forget the page's last frame, for a host loading another document
    /// into the view: until the new page paints, the view shows its
    /// placeholder, never the document before it.
    pub fn forget_frame(&mut self) {
        if let Ok(mut slot) = self.engine.frames().lock() {
            *slot = None;
        }
        self.cached_image = None;
        self.cached_trace_id = None;
        self.cached_key = (0, 0, 0);
    }

    /// The device scale the page is rendered at.
    #[must_use]
    pub fn device_scale(&self) -> f32 {
        self.scale_factor
    }

    /// The density the engine renders the page at: the device's, lowered
    /// only where a frame that dense would pass the engine's limits (a page
    /// filling an 8K display), which would close the engine and every page
    /// in it. Engine pixels, input and frames are all at this scale.
    fn engine_scale(&self) -> f32 {
        let (width, height) = (self.rect.width(), self.rect.height());
        if width <= 0.0 || height <= 0.0 {
            return self.scale_factor;
        }
        let limit =
            (MAX_FRAME_SIDE / width.max(height)).min((MAX_FRAME_PIXELS / (width * height)).sqrt());
        if f64::from(self.scale_factor) <= limit {
            self.scale_factor
        } else {
            f32_of_f64(limit)
        }
    }

    /// Render the page at `scale` from now on: once, and only when it is new.
    fn rescale(&mut self, scale: f32) {
        if !scale.is_finite() || scale <= 0.0 || scale.to_bits() == self.scale_factor.to_bits() {
            return;
        }
        self.scale_factor = scale;
        self.last_sent_size = (0, 0);
        self.send_resize_if_needed();
    }

    /// The engine handle this view drives, so a host can send it commands
    /// (Navigate to an email's sandboxed `data:` document, install a
    /// content filter) directly.
    pub fn engine(&self) -> &SharedEngine {
        &self.engine
    }

    /// Engine-space (physical, view-relative) coordinates for a logical point.
    fn to_engine(&self, p: Point) -> (f32, f32) {
        let scale = f64::from(self.engine_scale());
        (
            f32_of_f64((p.x - self.rect.x0) * scale),
            f32_of_f64((p.y - self.rect.y0) * scale),
        )
    }

    fn send_resize_if_needed(&mut self) {
        let scale = self.engine_scale();
        let w = round_to_u32_px(self.rect.width() * f64::from(scale));
        let h = round_to_u32_px(self.rect.height() * f64::from(scale));
        if w == 0
            || h == 0
            || ((w, h) == self.last_sent_size && scale.to_bits() == self.last_sent_scale.to_bits())
        {
            return;
        }
        self.last_sent_size = (w, h);
        self.last_sent_scale = scale;
        crate::webview::profile::record_resize(w, h, scale);
        self.engine.send(WebViewCommand::Resize {
            width: w,
            height: h,
            scale_factor: scale,
        });
    }

    fn retry_resize_for_stale_frame(&mut self, observed: (u32, u32)) -> bool {
        let scale = self.engine_scale();
        let expected = (
            round_to_u32_px(self.rect.width() * f64::from(scale)),
            round_to_u32_px(self.rect.height() * f64::from(scale)),
        );
        if expected.0 == 0 || expected.1 == 0 {
            self.stale_resize_retry = None;
            return true;
        }
        // This shared check rejects stale density/viewport frames without
        // requiring every engine to use CEF's exact allocation geometry.
        // Chromium additionally enforces its exact paint size in the host.
        let slack = frame_quantization_slack(scale);
        let matches =
            observed.0.abs_diff(expected.0) <= slack && observed.1.abs_diff(expected.1) <= slack;
        if matches {
            self.stale_resize_retry = None;
            return true;
        }
        let retry = (expected, observed);
        if self.stale_resize_retry != Some(retry) {
            self.stale_resize_retry = Some(retry);
            crate::webview::profile::record_resize(expected.0, expected.1, scale);
            self.engine.send(WebViewCommand::Resize {
                width: expected.0,
                height: expected.1,
                scale_factor: scale,
            });
        }
        false
    }

    /// Map a winit logical key to the engine's key model. Pure so tests can
    /// pin the table without synthesising `winit::KeyEvent`s (whose
    /// platform fields aren't portably constructible).
    fn map_key(key: &WKey) -> Option<KeyInput> {
        match key {
            WKey::Character(c) => Some(KeyInput::Character(c.to_string())),
            WKey::Named(WNamedKey::Space) => Some(KeyInput::Character(" ".to_string())),
            WKey::Named(named) => {
                let mapped = match named {
                    WNamedKey::Enter => NamedKey::Enter,
                    WNamedKey::Backspace => NamedKey::Backspace,
                    WNamedKey::Delete => NamedKey::Delete,
                    WNamedKey::Tab => NamedKey::Tab,
                    WNamedKey::Escape => NamedKey::Escape,
                    WNamedKey::ArrowUp => NamedKey::ArrowUp,
                    WNamedKey::ArrowDown => NamedKey::ArrowDown,
                    WNamedKey::ArrowLeft => NamedKey::ArrowLeft,
                    WNamedKey::ArrowRight => NamedKey::ArrowRight,
                    WNamedKey::Home => NamedKey::Home,
                    WNamedKey::End => NamedKey::End,
                    WNamedKey::PageUp => NamedKey::PageUp,
                    WNamedKey::PageDown => NamedKey::PageDown,
                    _ => return None,
                };
                Some(KeyInput::Named(mapped))
            }
            _ => None,
        }
    }
}

impl crate::widget::NativeViewLifecycle for WebView {
    fn mount_native_view(&mut self, host: &crate::widget::NativeViewHost<'_>) {
        let first_mount = !self.native_mounted;
        self.native_mounted = true;
        let host_scale = f32_of_f64(host.scale_factor());
        // A freshly rebuilt Browser WebView is seeded from the device-scale
        // draw that requested the rebuild. Some macOS startup windows still
        // report 1 here until their first visible frame. Preserve the larger
        // preseed only on that first mount; later mounts (including an actual
        // ScaleFactorChanged event, and a mount after an unmount, when the
        // window may have moved to another display) take the host value in
        // either direction.
        let scale_factor = if std::mem::take(&mut self.preseeded) {
            self.scale_factor.max(host_scale)
        } else {
            host_scale
        };
        if scale_factor.is_finite()
            && scale_factor > 0.0
            && (first_mount || scale_factor.to_bits() != self.scale_factor.to_bits())
        {
            self.scale_factor = scale_factor;
            self.last_sent_size = (0, 0);
            self.send_resize_if_needed();
        }
        // The surface's scale is the page's from now on (module docs).
        self.host_scaled = true;
    }

    fn unmount_native_view(&mut self) {
        self.host_scaled = false;
        self.held.clear();
        self.text_input.invalidate(&self.engine);
        if self.focused {
            self.engine.send(WebViewCommand::SetFocused(false));
        }
        self.focused = false;
        self.native_mounted = false;
    }

    fn native_view_frame_start(&mut self) {}
}

impl crate::undo::LocalUndo for WebView {
    fn local_undo_is_deferred(&self) -> bool {
        true
    }
    fn local_undo_status(&self) -> Option<crate::undo::LocalUndoStatus> {
        (self.focused && self.engine.supports_guarded_edit_history()).then(|| {
            self.text_input.history_status().map_or(
                crate::undo::LocalUndoStatus::Pending,
                crate::undo::LocalUndoStatus::Ready,
            )
        })
    }

    fn handle_local_undo_request(&mut self, request: crate::undo::UndoRequest) -> bool {
        if self.focused
            && self.engine.supports_guarded_edit_history()
            && self.text_input.history(request).is_some()
        {
            return true;
        }
        if !self.focused || !self.engine.handle_undo_request(request) {
            return false;
        }
        // Editing the page retires any original-field speech reservation.
        self.text_input.invalidate(&self.engine);
        true
    }
}

impl Widget for WebView {
    crate::provides!(
        dyn crate::widget::NativeViewLifecycle,
        dyn crate::undo::LocalUndo,
        dyn crate::widget::ResourceOperations
    );

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        self.text_input.pump(
            &self.engine,
            self.focused && self.engine.supports_text_input(),
        );
    }

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        (self.focused && self.engine.supports_text_input() && self.text_input.ready())
            .then_some(crate::ime::InputContent::TEXT)
    }

    fn ime_multiline(&self) -> bool {
        self.ime_content().is_some() && self.text_input.multiline()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.ime_content()?;
        Some(Rect::new(
            self.rect.x0,
            self.rect.y1 - 1.0,
            self.rect.x0 + 1.0,
            self.rect.y1,
        ))
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_content()?;
        self.text_input.stamp(key)
    }

    fn ime_requires_deferred_ack(&self) -> bool {
        true
    }

    fn ime_refuses_ordinary_commits(&self) -> bool {
        true
    }

    fn start_deferred_text(&mut self) -> Option<crate::webview::text_input::Receipt> {
        let receipt = self.focused.then(|| self.text_input.start()).flatten();
        Some(receipt.unwrap_or_else(|| {
            crate::webview::text_input::Receipt::failed(
                crate::webview::text_input::Failure::Rejected,
            )
        }))
    }

    fn commit_deferred_text(&mut self, text: &str) -> Option<crate::webview::text_input::Receipt> {
        let receipt = self
            .focused
            .then(|| self.text_input.commit(&self.engine, text))
            .flatten();
        Some(receipt.unwrap_or_else(|| {
            let receipt = crate::webview::text_input::Receipt::pending();
            receipt.complete(Err(crate::webview::text_input::Failure::Rejected));
            receipt
        }))
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        ((self.focused || self.text_input.has_work()) && self.engine.supports_text_input())
            .then(|| self.text_input.deadline())
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size::from_percent(1.0, 1.0),
            flex_grow: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.send_resize_if_needed();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.lands_at(transform);
        let drawn = self.draw_page(scene, transform, text_renderer, theme);
        self.damage = drawn
            .as_ref()
            .zip(self.submitted.as_ref())
            .and_then(|(now, before)| now.damage_since(before));
        self.drawing = drawn;
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        match &self.damage {
            Some(rects) => regions.extend_from_slice(rects),
            None => regions.push(self.rect),
        }
    }

    fn mark_drawn(&mut self) {
        self.submitted.clone_from(&self.drawing);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // Someone is using the page: it is on screen, whether or not a frame
        // has been drawn since it came back.
        if matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::KeyboardInput { .. }
                | WindowEvent::Focused(true)
                | WindowEvent::Occluded(false)
        ) {
            self.engine.presented();
        }
        // The event's scale is its surface's, never the page's: input does
        // not re-render the page (module docs, "Device scale").

        // Modifier releases may never become mapped page keys (and system
        // shortcuts consume their presses). Pointer default actions must use
        // the current host state rather than the last key sent to Chromium.
        if matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
        ) {
            let modifiers = ctx.modifiers.state();
            self.engine.set_modifiers(KeyModifiers {
                ctrl: modifiers.control_key(),
                shift: modifiers.shift_key(),
                alt: modifiers.alt_key(),
                meta: modifiers.super_key(),
            });
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical = ctx.logical(position);
                self.last_mouse = logical;
                // A drag that began on the page goes on outside it: a text
                // selection or a slider follows, and the page scrolls. The
                // first move out of it is the page's too, so what it shows
                // under the pointer (a menu, a highlight) lets go.
                let inside = self.rect.contains(logical);
                if inside || !self.held.is_empty() || self.pointer_inside {
                    let (x, y) = self.to_engine(logical);
                    self.engine.send(WebViewCommand::MouseMove { x, y });
                }
                self.pointer_inside = inside;
            }
            WindowEvent::CursorLeft { .. } => {
                // Out of the window altogether: the page hears it go, past
                // its own corner, unless a drag holds it.
                if self.pointer_inside && self.held.is_empty() {
                    let (x, y) = self.to_engine(Point::new(self.rect.x0 - 1.0, self.rect.y0 - 1.0));
                    self.engine.send(WebViewCommand::MouseMove { x, y });
                }
                self.pointer_inside = false;
            }
            WindowEvent::MouseInput {
                state: element_state,
                button,
                ..
            } => {
                let down = *element_state == ElementState::Pressed;
                let mapped = match button {
                    winit::event::MouseButton::Left => Some(MouseButton::Left),
                    winit::event::MouseButton::Middle => Some(MouseButton::Middle),
                    winit::event::MouseButton::Right => Some(MouseButton::Right),
                    _ => None,
                };
                // A button pressed on the page is released to it wherever
                // the release lands; otherwise the page still holds it down.
                let releases_held =
                    !down && mapped.is_some_and(|mapped| self.held.contains(&mapped));
                if !self.rect.contains(self.last_mouse) && !releases_held {
                    return;
                }
                let (x, y) = self.to_engine(self.last_mouse);
                match button {
                    winit::event::MouseButton::Left
                    | winit::event::MouseButton::Middle
                    | winit::event::MouseButton::Right => {
                        let Some(mapped) = mapped else {
                            return;
                        };
                        if down {
                            if !self.held.contains(&mapped) {
                                self.held.push(mapped);
                            }
                        } else {
                            self.held.retain(|held| *held != mapped);
                        }
                        if down && mapped == MouseButton::Left {
                            *ctx.focus_target = Some(ctx.node_id);
                        }
                        self.text_input.invalidate(&self.engine);
                        crate::webview::profile::record_input("mouse");
                        self.engine.send(WebViewCommand::MouseButton {
                            button: mapped,
                            down,
                            x,
                            y,
                        });
                        *ctx.handled = true;
                    }
                    // Mouse side buttons drive history, as in every desktop browser.
                    winit::event::MouseButton::Back => {
                        if down {
                            self.engine.send(WebViewCommand::GoBack);
                            *ctx.handled = true;
                        }
                    }
                    winit::event::MouseButton::Forward => {
                        if down {
                            self.engine.send(WebViewCommand::GoForward);
                            *ctx.handled = true;
                        }
                    }
                    winit::event::MouseButton::Other(_) => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Ctrl+scroll zooms the window (`crate::content_zoom`); a
                // page with a zoom of its own is the Browser's to offer.
                if !self.rect.contains(self.last_mouse)
                    || crate::content_zoom::is_zoom_scroll(event, ctx.modifiers)
                {
                    return;
                }
                // Engine pixels are the page's: logical pixels at its scale.
                let page = f64::from(self.engine_scale());
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (
                        f32_of_f64(f64::from(*x) * WHEEL_LINE_PX * page),
                        f32_of_f64(f64::from(*y) * WHEEL_LINE_PX * page),
                    ),
                    // PixelDelta is in the delivering surface's physical
                    // pixels.
                    MouseScrollDelta::PixelDelta(p) => {
                        let to_page = if ctx.scale_factor > 0.0 {
                            page / ctx.scale_factor
                        } else {
                            1.0
                        };
                        (f32_of_f64(p.x * to_page), f32_of_f64(p.y * to_page))
                    }
                };
                let (x, y) = self.to_engine(self.last_mouse);
                crate::webview::profile::record_input("scroll");
                self.engine.send(WebViewCommand::Scroll { dx, dy, x, y });
                *ctx.handled = true;
                // The page scrolls in its engine: nothing the app publishes
                // moved. What the page reports back reaches the app as its
                // own events.
                crate::widget::note_view_only_pan();
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                if !self.focused {
                    return;
                }
                if let Some(key) = Self::map_key(&key_event.logical_key) {
                    // A host shortcut consumes its press but its release still
                    // reaches the page. Retiring here canceled queued Undo/Redo
                    // before the next sync could send it to the original editor.
                    // Release handlers remain subject to the renderer's field
                    // and selection checks before deferred history is applied.
                    if key_event.state == ElementState::Pressed {
                        self.text_input.invalidate(&self.engine);
                    }
                    crate::webview::profile::record_input("key");
                    let m = ctx.modifiers.state();
                    self.engine.send(WebViewCommand::Key {
                        key,
                        down: key_event.state == ElementState::Pressed,
                        modifiers: KeyModifiers {
                            ctrl: m.control_key(),
                            shift: m.shift_key(),
                            alt: m.alt_key(),
                            meta: m.super_key(),
                        },
                    });
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "WebView"
    }

    fn wants_pointer_capture(&self) -> bool {
        !self.held.is_empty()
    }

    /// The shape the page asks for over itself: a hand on its links, an
    /// I-beam on its text.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.rect
            .contains(point)
            .then(|| self.engine.cursor())
            .flatten()
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.test_id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Document)
                .with_name(self.accessible_name.clone())
                .with_state(
                    crate::a11y::AccessState::new()
                        .focusable(true)
                        .focused(self.focused),
                ),
        )
    }

    /// A freshly-exported engine frame that the scene hasn't painted yet
    /// keeps the app's ~60 Hz animation pump alive; once the frame is
    /// consumed (cached) the widget stops asking and the loop can idle.
    fn needs_animation_frame(&self) -> bool {
        let slot = self.engine.frames();
        let guard = slot.lock();
        match guard.ok().as_deref() {
            Some(Some(f)) => (Arc::as_ptr(&f.rgba) as usize, f.width, f.height) != self.cached_key,
            _ => false,
        }
    }

    /// Two-finger swipe / edge-pull → browser-style history navigation.
    /// Overriding this here makes every embedder of `WebView`
    /// swipe-navigable, not just the browser app.
    fn on_nav_gesture(&mut self, dir: NavDirection, _state: &AppModel, _path: &ModelPath) -> bool {
        self.engine.send(match dir {
            NavDirection::Back => WebViewCommand::GoBack,
            NavDirection::Forward => WebViewCommand::GoForward,
        });
        true
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.text_input.invalidate(&self.engine);
            self.focused = focused;
            self.engine.send(WebViewCommand::SetFocused(focused));
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn wants_tab_input(&self) -> bool {
        true
    }
}

impl crate::widget::ResourceOperations for WebView {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::undo::EDIT_OPERATION]
    }

    fn start_operation(
        &mut self,
        operation: &crate::widget::ResourceOperation<'_>,
        _: &mut crate::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let Some(request) = operation.unscoped() else {
            return Ok(None);
        };
        if request.type_id != crate::undo::EDIT_OPERATION {
            return Ok(None);
        }
        if !self.focused || !self.engine.supports_guarded_edit_history() {
            return Ok(None);
        }
        if let Some((invocation, _)) = &self.history_operation {
            return if *invocation == request.invocation {
                Ok(Some(crate::widget::ResourceOperationStart::Accepted))
            } else {
                Err("An original-field history request is already pending".into())
            };
        }
        let direction: crate::undo::UndoRequest =
            serde_json::from_slice(request.payload).map_err(|_| "Invalid native Edit request")?;
        if !self
            .text_input
            .history_status()
            .is_some_and(|status| status.can(direction))
        {
            return Err("The original field's history is not available".into());
        }
        let receipt = self
            .text_input
            .history(direction)
            .ok_or("The original field's history reservation retired")?;
        self.history_operation = Some((request.invocation, receipt));
        Ok(Some(crate::widget::ResourceOperationStart::Accepted))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _: &automerge::AutoCommit,
        _: &automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let (original, receipt) = self.history_operation.as_ref()?;
        if *original != invocation {
            return None;
        }
        if cancellation_requested {
            receipt.cancel();
        }
        let result = receipt.poll()?;
        self.history_operation = None;
        Some(match result {
            Ok(None) => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: Default::default(),
            },
            _ => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "native_history_unconfirmed".into(),
                    message: crate::i18n::I18nManager::lookup_plain(
                        "host-native-history-unconfirmed",
                    ),
                    details: None,
                    retryable: false,
                },
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Dispatch;
    use crate::webview::WebViewEngine; // send()/commands() on the concrete TestEngine
    use crate::webview::test_engine::TestEngine;
    use automerge::AutoCommit;
    use std::sync::Arc;
    use winit::event::{ElementState, WindowEvent};

    #[test]
    fn native_history_operation_waits_for_renderer_ack_and_rejects_focus_change() {
        use crate::webview::text_input::Transactions;
        use serde_json::{Value, json};
        struct Acknowledged {
            inner: SharedEngine,
            transactions: std::sync::Mutex<Transactions>,
            edits: std::sync::Mutex<Vec<Value>>,
        }
        impl WebViewEngine for Acknowledged {
            fn wrapped(&self) -> Option<&SharedEngine> {
                Some(&self.inner)
            }
            fn send(&self, command: WebViewCommand) {
                self.inner.send(command);
            }
            fn frames(&self) -> crate::webview::FrameSlot {
                self.inner.frames()
            }
            fn drain_events(&self) -> Vec<crate::webview::WebViewEvent> {
                self.inner.drain_events()
            }
            fn supports_text_input(&self) -> bool {
                true
            }
            fn supports_guarded_edit_history(&self) -> bool {
                true
            }
            fn text_input(
                &self,
                request: crate::webview::text_input::Request,
            ) -> Option<crate::webview::text_input::Receipt> {
                let mut transactions = self.transactions.lock().unwrap();
                let (receipt, command) = transactions.admit(request)?;
                if matches!(
                    command["action"].as_str(),
                    Some("undo_text_input" | "redo_text_input")
                ) {
                    assert_eq!(command["history_scope"], "page");
                    self.edits.lock().unwrap().push(command);
                } else {
                    transactions.reply(&json!({"request_id":command["request_id"],"ok":true,
                        "data":{"lease":"0123456789abcdef0123456789abcdef"},
                        "history":{"can_undo":true,"can_redo":false}}));
                }
                Some(receipt)
            }
        }
        for (changed, acknowledged, key_edge) in [
            (false, true, None),
            (false, false, None),
            (true, true, None),
            (false, true, Some(ElementState::Released)),
            (false, true, Some(ElementState::Pressed)),
        ] {
            let native = TestEngine::new();
            let engine = Arc::new(Acknowledged {
                inner: native.clone(),
                transactions: Default::default(),
                edits: Default::default(),
            });
            let (mut page, tree, node) = laid_out(engine.clone());
            page.focused = true;
            let edit = crate::host_chrome::edit::edit_resource(&crate::undo::UndoStatus {
                can_undo: true,
                ..Default::default()
            });
            let violations = tilo_actions::validate_surface(
                &[edit],
                &tilo_actions::ActionRegistry::new(),
                &crate::widget::resource_operation_handlers(&page),
            );
            assert!(
                violations.is_empty(),
                "native Edit publication: {violations:?}"
            );
            let mut doc = AutoCommit::new();
            page.sync(&doc, &automerge::ROOT);
            page.sync(&doc, &automerge::ROOT);
            let invocation = tilo_actions::InvocationId::new();
            let payload = serde_json::to_vec(&crate::undo::UndoRequest::Undo).unwrap();
            let operation = crate::widget::ResourceOperation::local(
                invocation,
                tilo_actions::OperationKind::Effect,
                crate::host_chrome::edit::EDIT_OPERATION,
                &payload,
            );
            assert_eq!(
                crate::widget::start_operation(
                    &mut page,
                    &operation,
                    &mut crate::widget::Edits::lend(&mut doc),
                    &automerge::ROOT
                )
                .unwrap(),
                Some(crate::widget::ResourceOperationStart::Accepted)
            );
            assert!(
                crate::widget::poll_resource_operation(
                    &mut page,
                    invocation,
                    false,
                    &doc,
                    &automerge::ROOT
                )
                .is_none(),
                "queue admission cannot report success"
            );
            if changed {
                page.focused = false;
            }
            if let Some(edge) = key_edge {
                dispatch(
                    &mut page,
                    &tree,
                    node,
                    &crate::synthetic_input::key_input(
                        &winit::keyboard::Key::Character("z".into()),
                        edge,
                    ),
                );
                assert!(native.commands().iter().any(|command| {
                    matches!(command, WebViewCommand::Key { down, .. }
                        if *down == (edge == ElementState::Pressed))
                }));
            }
            let retired = changed || key_edge == Some(ElementState::Pressed);
            page.sync(&doc, &automerge::ROOT);
            let edits = engine.edits.lock().unwrap();
            if retired {
                assert!(
                    edits.is_empty(),
                    "changed focus or a new press cannot receive the original edit"
                );
            } else {
                assert_eq!(edits.len(), 1);
                assert!(
                    crate::widget::poll_resource_operation(
                        &mut page,
                        invocation,
                        false,
                        &doc,
                        &automerge::ROOT
                    )
                    .is_none()
                );
                engine.transactions.lock().unwrap().reply(&json!({"request_id":edits[0]["request_id"],"ok":acknowledged,"code":"rejected"}));
            }
            drop(edits);
            page.sync(&doc, &automerge::ROOT);
            let terminal = crate::widget::poll_resource_operation(
                &mut page,
                invocation,
                false,
                &doc,
                &automerge::ROOT,
            )
            .unwrap();
            assert_eq!(
                matches!(terminal, tilo_actions::InvocationUpdate::Succeeded { .. }),
                !retired && acknowledged
            );
            assert!(
                crate::widget::poll_resource_operation(
                    &mut page,
                    invocation,
                    false,
                    &doc,
                    &automerge::ROOT
                )
                .is_none(),
                "terminal receipt is one shot"
            );
        }
    }

    struct NativeHistory {
        inner: crate::webview::SharedEngine,
        requests: std::sync::Mutex<Vec<crate::undo::UndoRequest>>,
        modifiers: std::sync::Mutex<Vec<KeyModifiers>>,
        pointer_modifiers: std::sync::Mutex<Vec<KeyModifiers>>,
    }
    impl WebViewEngine for NativeHistory {
        fn send(&self, command: WebViewCommand) {
            if matches!(command, WebViewCommand::MouseButton { .. }) {
                let modifiers = self
                    .modifiers
                    .lock()
                    .unwrap()
                    .last()
                    .copied()
                    .unwrap_or_default();
                self.pointer_modifiers.lock().unwrap().push(modifiers);
            }
            self.inner.send(command);
        }
        fn set_modifiers(&self, modifiers: KeyModifiers) {
            self.modifiers.lock().unwrap().push(modifiers);
        }
        fn frames(&self) -> crate::webview::FrameSlot {
            self.inner.frames()
        }
        fn drain_events(&self) -> Vec<crate::webview::WebViewEvent> {
            self.inner.drain_events()
        }
        fn wrapped(&self) -> Option<&crate::webview::SharedEngine> {
            Some(&self.inner)
        }
        fn handle_undo_request(&self, request: crate::undo::UndoRequest) -> bool {
            self.requests
                .lock()
                .expect("native history fixture")
                .push(request);
            true
        }
    }

    #[test]
    fn system_undo_delegates_only_to_focused_native_page() {
        use crate::undo::{UndoRequest, handle_local_undo_request};
        let engine = Arc::new(NativeHistory {
            inner: TestEngine::new(),
            requests: Default::default(),
            modifiers: Default::default(),
            pointer_modifiers: Default::default(),
        });
        let mut page = WebView::new(engine.clone());
        assert!(!handle_local_undo_request(&mut page, UndoRequest::Undo));
        assert!(engine.requests.lock().expect("fixture").is_empty());
        page.set_focus(true);
        assert!(handle_local_undo_request(&mut page, UndoRequest::Undo));
        assert!(handle_local_undo_request(&mut page, UndoRequest::Redo));
        page.set_focus(false);
        assert!(!handle_local_undo_request(&mut page, UndoRequest::Redo));
        assert_eq!(
            *engine.requests.lock().expect("fixture"),
            [UndoRequest::Undo, UndoRequest::Redo]
        );
        let mut unsupported = WebView::new(TestEngine::new());
        unsupported.set_focus(true);
        assert!(!handle_local_undo_request(
            &mut unsupported,
            UndoRequest::Undo
        ));
    }

    #[test]
    fn keyboard_focus_is_forwarded_to_the_page() {
        {
            let mut page = WebView::new(TestEngine::new());
            page.set_focus(true);
            assert!(page.wants_tab_input());
        }

        {
            let engine = TestEngine::new();
            let (mut w, _tree, _node) = laid_out(engine.clone());
            w.set_focus(true);
            w.set_focus(true);
            w.set_focus(false);
            let focus_cmds: Vec<_> = engine
                .commands()
                .into_iter()
                .filter(|c| matches!(c, WebViewCommand::SetFocused(_)))
                .collect();
            assert_eq!(
                focus_cmds,
                vec![
                    WebViewCommand::SetFocused(true),
                    WebViewCommand::SetFocused(false)
                ]
            );
        }

        {
            assert_eq!(
                WebView::map_key(&WKey::Character("a".into())),
                Some(KeyInput::Character("a".into()))
            );
            assert_eq!(
                WebView::map_key(&WKey::Named(WNamedKey::Space)),
                Some(KeyInput::Character(" ".into()))
            );
            assert_eq!(
                WebView::map_key(&WKey::Named(WNamedKey::Enter)),
                Some(KeyInput::Named(NamedKey::Enter))
            );
            assert_eq!(
                WebView::map_key(&WKey::Named(WNamedKey::PageDown)),
                Some(KeyInput::Named(NamedKey::PageDown))
            );
            // Modifier keys are not forwarded as page keys.
            assert_eq!(WebView::map_key(&WKey::Named(WNamedKey::Shift)), None);
        }
    }

    fn laid_out(engine: SharedEngine) -> (WebView, TaffyTree<()>, NodeId) {
        let mut w = WebView::new(engine);
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = w.build_layout(&mut tree, &theme, &mut node_map);
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(400.0),
                height: taffy::AvailableSpace::Definite(300.0),
            },
        )
        .unwrap();
        w.apply_layout(&tree, node, Point::ZERO);
        (w, tree, node)
    }

    #[test]
    fn a_page_too_dense_for_the_engine_renders_at_the_density_it_can() {
        // A page filling an 8K display at 2x: 7680 x 4320 device pixels,
        // past both the engine's 8192 side and its 64 MiB frame.
        let engine = TestEngine::new();
        let mut view = WebView::new(engine.clone()).with_scale_factor(2.0);
        view.set_viewport(Rect::new(0.0, 0.0, 3840.0, 2160.0), 2.0);
        let Some(WebViewCommand::Resize {
            width,
            height,
            scale_factor,
        }) = engine.commands().last().cloned()
        else {
            panic!("the page is sized");
        };
        assert!(scale_factor < 2.0, "{scale_factor}");
        assert!(
            u64::from(width) * u64::from(height) * 4 + 9 <= 64 * 1024 * 1024,
            "{width} x {height}"
        );
        assert!(width.max(height) <= 8192);
        assert_eq!(view.device_scale(), 2.0, "the device is still 2x");
        // The engine's frame at that size is the page, not a stale one.
        assert!(view.retry_resize_for_stale_frame((width, height)));
        // Input lands where the engine drew: its far corner is the frame's.
        let (x, y) = view.to_engine(Point::new(3840.0, 2160.0));
        assert!((x - width as f32).abs() <= 1.0 && (y - height as f32).abs() <= 1.0);
        view.set_viewport(Rect::new(0.0, 0.0, 1920.0, 1080.0), 2.0);
        assert_eq!(
            engine.commands().last(),
            Some(&WebViewCommand::Resize {
                width: 3840,
                height: 2160,
                scale_factor: 2.0,
            })
        );
    }

    fn dispatch(w: &mut WebView, tree: &TaffyTree<()>, node: NodeId, event: &WindowEvent) {
        Dispatch::new().layout(tree, node).send(w, event);
    }

    #[test]
    fn engine_learns_physical_size_and_retina_scale_before_input() {
        {
            let engine = TestEngine::new();
            let (mut w, tree, node) = laid_out(engine.clone());
            // Re-applying the same layout must not resend.
            w.apply_layout(&tree, node, Point::ZERO);
            let resizes: Vec<_> = engine
                .commands()
                .into_iter()
                .filter(|c| matches!(c, WebViewCommand::Resize { .. }))
                .collect();
            assert_eq!(
                resizes,
                vec![WebViewCommand::Resize {
                    width: 400,
                    height: 300,
                    scale_factor: 1.0
                }]
            );
        }

        {
            let engine = TestEngine::new();
            let (mut view, _, _) = laid_out(engine.clone());
            view.draw(
                &mut Scene::new(),
                Affine::scale(2.0),
                &mut TextRenderer::new(),
                &Theme::dark(),
                &PresenceCtx::default(),
            );
            assert_eq!(
                engine.commands().last(),
                Some(&WebViewCommand::Resize {
                    width: 800,
                    height: 600,
                    scale_factor: 2.0,
                })
            );
            let count = engine.commands().len();
            view.draw(
                &mut Scene::new(),
                Affine::scale(2.0),
                &mut TextRenderer::new(),
                &Theme::dark(),
                &PresenceCtx::default(),
            );
            assert_eq!(engine.commands().len(), count);
        }

        {
            let engine = TestEngine::new();
            let (mut view, _, _) = laid_out(engine.clone());
            crate::widget::mount_native_views(
                &mut view,
                &crate::widget::NativeViewHost::headless(
                    crate::platform::PlatformConventions::default(),
                    2.0,
                ),
            );
            assert_eq!(
                engine.commands().last(),
                Some(&WebViewCommand::Resize {
                    width: 800,
                    height: 600,
                    scale_factor: 2.0,
                })
            );
        }

        {
            let engine = TestEngine::new();
            let mut view = WebView::new(engine.clone()).with_scale_factor(2.0);
            let mut taffy = TaffyTree::new();
            let mut node_map = std::collections::HashMap::new();
            let node = view.build_layout(&mut taffy, &Theme::default(), &mut node_map);
            taffy
                .compute_layout(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(400.0),
                        height: taffy::AvailableSpace::Definite(300.0),
                    },
                )
                .unwrap();
            view.apply_layout(&taffy, node, Point::ORIGIN);
            let before_mount = engine.commands().len();
            crate::widget::mount_native_views(
                &mut view,
                &crate::widget::NativeViewHost::headless(
                    crate::platform::PlatformConventions::default(),
                    2.0,
                ),
            );
            assert_eq!(
                engine.commands().len(),
                before_mount + 1,
                "native mount must reissue a resize that startup could have missed"
            );
            assert_eq!(
                engine.commands().last(),
                Some(&WebViewCommand::Resize {
                    width: 800,
                    height: 600,
                    scale_factor: 2.0,
                })
            );
            crate::widget::mount_native_views(
                &mut view,
                &crate::widget::NativeViewHost::headless(
                    crate::platform::PlatformConventions::default(),
                    2.0,
                ),
            );
            assert_eq!(
                engine.commands().len(),
                before_mount + 1,
                "repeated mount of the same retained view must be idempotent"
            );
        }

        {
            let engine = TestEngine::new();
            let mut view = WebView::new(engine.clone()).with_scale_factor(2.0);
            let mut taffy = TaffyTree::new();
            let mut node_map = std::collections::HashMap::new();
            let node = view.build_layout(&mut taffy, &Theme::default(), &mut node_map);
            taffy
                .compute_layout(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(400.0),
                        height: taffy::AvailableSpace::Definite(300.0),
                    },
                )
                .unwrap();
            view.apply_layout(&taffy, node, Point::ORIGIN);
            crate::widget::mount_native_views(
                &mut view,
                &crate::widget::NativeViewHost::headless(
                    crate::platform::PlatformConventions::default(),
                    1.0,
                ),
            );
            assert_eq!(
                engine.commands().last(),
                Some(&WebViewCommand::Resize {
                    width: 800,
                    height: 600,
                    scale_factor: 2.0,
                })
            );
        }
    }

    fn resizes(engine: &TestEngine) -> Vec<WebViewCommand> {
        engine
            .commands()
            .into_iter()
            .filter(|c| matches!(c, WebViewCommand::Resize { .. }))
            .collect()
    }

    fn mount_at(view: &mut WebView, scale: f64) {
        crate::widget::mount_native_views(
            view,
            &crate::widget::NativeViewHost::headless(
                crate::platform::PlatformConventions::default(),
                scale,
            ),
        );
    }

    fn draw_at(view: &mut WebView, transform: Affine) -> Scene {
        let mut scene = Scene::new();
        view.draw(
            &mut scene,
            transform,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::default(),
        );
        scene
    }

    /// Draw `view` as a `Column` or `Row` does: at the identity into its
    /// subtree cache, which lands on screen through `screen`.
    fn draw_through_cache(view: &mut WebView, screen: Affine) -> Scene {
        crate::scroll_copy::drawing_into_cache(screen, || draw_at(view, Affine::IDENTITY))
    }

    fn hover(view: &mut WebView, tree: &TaffyTree<()>, node: NodeId, event_scale: f64, x: f64) {
        Dispatch::new()
            .layout(tree, node)
            .scale_factor(event_scale)
            .send(
                view,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: (x * event_scale, 120.0 * event_scale).into(),
                },
            );
    }

    #[test]
    fn a_page_keeps_its_surface_scale_through_hover_and_rescales_once_per_display() {
        let engine = TestEngine::new();
        let (mut view, tree, node) = laid_out(engine.clone());
        mount_at(&mut view, 2.0);
        let mounted = resizes(&engine);
        assert_eq!(
            mounted.last(),
            Some(&WebViewCommand::Resize {
                width: 800,
                height: 600,
                scale_factor: 2.0,
            })
        );
        for step in 0..12 {
            // Events from surfaces at other scales, draws through a cache
            // (the identity), under an opening animation's scale, and plain.
            hover(
                &mut view,
                &tree,
                node,
                if step % 2 == 0 { 1.0 } else { 2.0 },
                20.0 + f64::from(step),
            );
            draw_through_cache(&mut view, Affine::scale(2.0));
            draw_at(&mut view, Affine::scale(2.0 * 0.96));
            draw_at(&mut view, Affine::scale(2.0));
            Dispatch::new()
                .layout(&tree, node)
                .scale_factor(1.0)
                .send(&mut view, &WindowEvent::Focused(step % 3 == 0));
        }
        assert_eq!(
            resizes(&engine),
            mounted,
            "hovering, focus and redraws must never re-render the page at another scale"
        );
        assert!((view.device_scale() - 2.0).abs() < f32::EPSILON);

        // Moved to the external monitor: its surface scale arrives once.
        mount_at(&mut view, 1.0);
        for step in 0..6 {
            hover(&mut view, &tree, node, 2.0, 30.0 + f64::from(step));
            draw_through_cache(&mut view, Affine::scale(1.0));
            draw_at(&mut view, Affine::scale(2.0));
        }
        let moved = resizes(&engine);
        assert_eq!(
            &moved[mounted.len()..],
            &[WebViewCommand::Resize {
                width: 400,
                height: 300,
                scale_factor: 1.0,
            }],
            "one re-render at the new display's scale, then none"
        );

        mount_at(&mut view, 1.75);
        for step in 0..6 {
            hover(&mut view, &tree, node, 1.0, 40.0 + f64::from(step));
            draw_through_cache(&mut view, Affine::scale(1.75));
        }
        assert_eq!(
            &resizes(&engine)[moved.len()..],
            &[WebViewCommand::Resize {
                width: 700,
                height: 525,
                scale_factor: 1.75,
            }]
        );
    }

    #[test]
    fn a_page_drawn_through_a_cache_renders_at_the_scale_it_lands_on_screen_with() {
        for (scale, physical) in [(2.0, (800, 600)), (1.5, (600, 450)), (1.75, (700, 525))] {
            let engine = TestEngine::new();
            let (mut view, tree, node) = laid_out(engine.clone());
            draw_through_cache(&mut view, Affine::scale(scale));
            assert_eq!(
                resizes(&engine).last(),
                Some(&WebViewCommand::Resize {
                    width: physical.0,
                    height: physical.1,
                    scale_factor: crate::num::f32_of_f64(scale),
                }),
                "a cached draw at {scale} must not request a 1x page"
            );
            let count = resizes(&engine).len();
            for step in 0..8 {
                hover(&mut view, &tree, node, 1.0, 10.0 + f64::from(step));
                draw_through_cache(&mut view, Affine::scale(scale));
            }
            assert_eq!(
                resizes(&engine).len(),
                count,
                "hover re-rendered the page at {scale}"
            );

            *engine.frames().lock().unwrap() = Some(crate::webview::Frame {
                trace_id: None,
                damage: None,
                rgba: Arc::new(vec![255; physical.0 as usize * physical.1 as usize * 4]),
                width: physical.0,
                height: physical.1,
            });
            let scene = draw_through_cache(&mut view, Affine::scale(scale));
            assert!(
                scene
                    .encoding()
                    .draw_tags
                    .contains(&vello_encoding::DrawTag::IMAGE),
                "the {}x{} frame at {scale} must draw",
                physical.0,
                physical.1
            );
        }
    }

    #[test]
    fn engine_frames_are_accepted_by_size_and_density_with_bounded_repaint_retries() {
        {
            let engine = TestEngine::new();
            let (mut view, _, _) = laid_out(engine.clone());
            let initial = engine.commands().len();
            view.retry_resize_for_stale_frame((400, 292));
            assert_eq!(engine.commands().len(), initial + 1);
            assert_eq!(
                engine.commands().last(),
                Some(&WebViewCommand::Resize {
                    width: 400,
                    height: 300,
                    scale_factor: 1.0,
                })
            );
            view.retry_resize_for_stale_frame((400, 292));
            assert_eq!(
                engine.commands().len(),
                initial + 1,
                "the same stale paint must not create an invalidation loop"
            );
            view.retry_resize_for_stale_frame((399, 300));
            assert_eq!(
                engine.commands().len(),
                initial + 2,
                "a distinct stale frame gets one retry"
            );
            view.retry_resize_for_stale_frame((400, 300));
            view.retry_resize_for_stale_frame((400, 292));
            assert_eq!(
                engine.commands().len(),
                initial + 3,
                "a matching frame resets the bounded retry state"
            );
        }

        {
            let engine = TestEngine::new();
            let (mut view, _, _) = laid_out(engine.clone());
            view.rect = Rect::new(0.0, 0.0, 400.5, 225.5);
            view.scale_factor = 2.0;
            let initial = engine.commands().len();
            assert!(view.retry_resize_for_stale_frame((802, 452)));
            assert_eq!(engine.commands().len(), initial);
        }

        {
            let engine = TestEngine::new();
            let (mut view, _, _) = laid_out(engine.clone());
            view.rect = Rect::new(0.0, 0.0, 802.0 / 1.75, 451.0 / 1.75);
            view.scale_factor = 1.75;
            let initial = engine.commands().len();
            assert!(
                view.retry_resize_for_stale_frame((804, 452)),
                "CEF rounds requested physical pixels to integer DIP and back out"
            );
            assert_eq!(engine.commands().len(), initial);
            assert!(
                view.retry_resize_for_stale_frame((802, 451)),
                "engines that render the exact physical request are also valid"
            );
            assert_eq!(engine.commands().len(), initial);
        }

        {
            let engine = TestEngine::new();
            let mut view = WebView::new(engine.clone());
            view.set_viewport(Rect::new(0.0, 0.0, 400.0, 300.0), 2.0);
            engine.send(WebViewCommand::Navigate("about:frame-regression".into()));
            let current = engine.frames().lock().unwrap().clone().unwrap();
            let stale = crate::webview::Frame {
                trace_id: None,
                damage: None,
                rgba: Arc::new(vec![255; 400 * 300 * 4]),
                width: 400,
                height: 300,
            };
            let initial = engine.commands().len();
            for (frame, should_draw, retries) in [
                (current.clone(), true, 0),
                (stale.clone(), false, 1),
                (stale.clone(), false, 1),
                (current, true, 1),
                (stale, false, 2),
            ] {
                *engine.frames().lock().unwrap() = Some(frame);
                let mut scene = Scene::new();
                view.draw(
                    &mut scene,
                    Affine::scale(2.0),
                    &mut TextRenderer::new(),
                    &Theme::dark(),
                    &PresenceCtx::default(),
                );
                assert_eq!(
                    scene
                        .encoding()
                        .draw_tags
                        .contains(&vello_encoding::DrawTag::IMAGE),
                    should_draw
                );
                assert_eq!(engine.commands().len(), initial + retries);
            }
        }
    }

    #[test]
    fn a_caret_that_blinks_damages_and_uploads_only_the_caret() {
        use crate::webview::{Frame, FrameDamage, PixelRect};
        let engine = TestEngine::new();
        let mut view = WebView::new(engine.clone());
        // 200x100 logical at 2x: a 400x200 frame.
        view.set_viewport(Rect::new(10.0, 20.0, 210.0, 120.0), 2.0);
        let frame = |generation, recent: Vec<(u64, Vec<PixelRect>)>, shade| Frame {
            trace_id: None,
            rgba: Arc::new(vec![shade; 400 * 200 * 4]),
            width: 400,
            height: 200,
            damage: Some(FrameDamage {
                generation,
                recent: recent
                    .into_iter()
                    .map(|(generation, rects)| (generation, Arc::from(rects)))
                    .collect(),
            }),
        };
        let show = |frame| *engine.frames().lock().unwrap() = Some(frame);
        let damage_of = |view: &mut WebView| {
            draw_at(view, Affine::scale(2.0));
            let mut damage = Vec::new();
            view.collect_damage(&mut damage);
            view.mark_drawn();
            damage
        };

        show(frame(1, Vec::new(), 200));
        assert_eq!(
            damage_of(&mut view),
            [view.rect],
            "the first frame is all new"
        );
        assert!(
            damage_of(&mut view).is_empty(),
            "the same frame is no change"
        );

        let caret = PixelRect {
            x: 100,
            y: 40,
            width: 4,
            height: 32,
        };
        let drawn = view.cached_image.as_ref().unwrap().data.id();
        show(frame(2, vec![(2, vec![caret])], 0));
        assert_eq!(
            damage_of(&mut view),
            [Rect::new(59.0, 39.0, 63.0, 57.0)],
            "the caret, in layout coordinates, with a pixel for sampling"
        );
        let shown = view.cached_image.as_ref().unwrap().data.id();
        assert_eq!(
            vello::image_delta(shown).map(|(from, rects)| (from, rects.to_vec())),
            Some((drawn, vec![[100, 40, 4, 32]])),
            "the renderer uploads just the caret"
        );

        // A frame whose rects do not reach back to the one on screen, and a
        // view that moved, repaint all of it.
        show(frame(5, vec![(5, vec![caret])], 1));
        assert_eq!(damage_of(&mut view), [view.rect]);
        show(frame(6, vec![(6, vec![caret]), (5, vec![caret])], 2));
        view.set_viewport(Rect::new(12.0, 20.0, 212.0, 120.0), 2.0);
        assert_eq!(damage_of(&mut view), [view.rect]);
    }

    /// A frame of `width`x`height` opaque pixels, grey `shade` throughout
    /// but for `marked`, which is black, in generation `generation` after the
    /// generations `recent` describe.
    fn shaded_frame(
        (width, height): (u32, u32),
        shade: u8,
        marked: Option<crate::webview::PixelRect>,
        generation: u64,
        recent: &[(u64, crate::webview::PixelRect)],
    ) -> crate::webview::Frame {
        let mut rgba = [shade, shade, shade, 255].repeat(width as usize * height as usize);
        if let Some(rect) = marked {
            for y in rect.y..rect.y + rect.height {
                for x in rect.x..rect.x + rect.width {
                    let at = (y as usize * width as usize + x as usize) * 4;
                    rgba[at..at + 4].copy_from_slice(&[0, 0, 0, 255]);
                }
            }
        }
        crate::webview::Frame {
            trace_id: None,
            rgba: Arc::new(rgba),
            width,
            height,
            damage: Some(crate::webview::FrameDamage {
                generation,
                recent: recent
                    .iter()
                    .map(|(generation, rect)| (*generation, Arc::from(vec![*rect])))
                    .collect(),
            }),
        }
    }

    #[test]
    fn a_caret_frame_uploads_only_the_caret_to_the_gpu() {
        use crate::offscreen::OffscreenRenderer;
        use crate::webview::PixelRect;
        let background = vello::peniko::Color::from_rgba8(0, 0, 255, 255);
        let pixel = |rendered: &[u8], x, y| OffscreenRenderer::sample(rendered, 64, x, y);
        {
            let engine = TestEngine::new();
            let mut view = WebView::new(engine.clone());
            view.set_viewport(Rect::new(0.0, 0.0, 64.0, 48.0), 1.0);
            let mut renderer = OffscreenRenderer::new(64, 48);
            let show = |view: &mut WebView, renderer: &mut OffscreenRenderer, frame| {
                *engine.frames().lock().unwrap() = Some(frame);
                renderer.render(&draw_at(view, Affine::IDENTITY), background)
            };
            let first = show(
                &mut view,
                &mut renderer,
                shaded_frame((64, 48), 255, None, 1, &[]),
            );
            assert_eq!(pixel(&first, 30, 20), (255, 255, 255, 255));

            // The next frame says only its caret changed, though every
            // other pixel of it is grey: a renderer that uploads only the
            // caret shows the caret over the first frame's white.
            let caret = PixelRect {
                x: 20,
                y: 10,
                width: 2,
                height: 16,
            };
            let next = show(
                &mut view,
                &mut renderer,
                shaded_frame((64, 48), 128, Some(caret), 2, &[(2, caret)]),
            );
            assert_eq!(
                pixel(&next, 20, 10),
                (0, 0, 0, 255),
                "the caret is uploaded"
            );
            assert_eq!(pixel(&next, 21, 25), (0, 0, 0, 255));
            for (x, y) in [(19, 10), (22, 10), (20, 9), (20, 26), (0, 0), (63, 47)] {
                assert_eq!(
                    pixel(&next, x, y),
                    (255, 255, 255, 255),
                    "({x}, {y}) is never uploaded again"
                );
            }

            // A frame that says nothing of what changed is uploaded whole.
            let whole = show(
                &mut view,
                &mut renderer,
                shaded_frame((64, 48), 128, None, 9, &[]),
            );
            assert_eq!(pixel(&whole, 0, 0), (128, 128, 128, 255));
            assert_eq!(pixel(&whole, 20, 10), (128, 128, 128, 255));
        }

        // What a caret blink costs the renderer on a 2560x1600 page, drawn
        // small so the upload, not the raster, is what is measured.
        let engine = TestEngine::new();
        let mut view = WebView::new(engine.clone());
        view.set_viewport(Rect::new(0.0, 0.0, 1280.0, 800.0), 2.0);
        let mut renderer = OffscreenRenderer::new(64, 40);
        let caret = PixelRect {
            x: 400,
            y: 300,
            width: 2,
            height: 40,
        };
        let mut generation = 1;
        *engine.frames().lock().unwrap() =
            Some(shaded_frame((2560, 1600), 255, None, generation, &[]));
        renderer.render_gpu_wait(&draw_at(&mut view, Affine::scale(0.05)), background);
        let mut blink = |rects: bool| {
            let mut spent = Vec::new();
            for blink in 0..12 {
                generation += 1;
                let recent = [(generation, caret)];
                *engine.frames().lock().unwrap() = Some(shaded_frame(
                    (2560, 1600),
                    255,
                    (blink % 2 == 0).then_some(caret),
                    generation,
                    if rects { &recent[..] } else { &[] },
                ));
                let scene = draw_at(&mut view, Affine::scale(0.05));
                let started = crate::time::now();
                renderer.render_gpu_wait(&scene, background);
                spent.push(started.elapsed());
            }
            spent.sort();
            spent[spent.len() / 2]
        };
        let whole = blink(false);
        let rects = blink(true);
        println!(
            "perf webview caret blink on a 2560x1600 page: {:.2} ms to render with the frame \
             uploaded whole (16 MB), {:.2} ms with its caret uploaded ({} bytes)",
            whole.as_secs_f64() * 1000.0,
            rects.as_secs_f64() * 1000.0,
            caret.width * caret.height * 4,
        );
    }

    #[test]
    fn engine_frames_draw_at_their_placed_geometry_and_scene_trace() {
        {
            for (width, height, scale) in [(802, 451, 1.75), (801, 451, 2.0), (800, 600, 2.0)] {
                let engine = TestEngine::new();
                let mut view = WebView::new(engine.clone());
                view.set_viewport(
                    Rect::new(
                        0.0,
                        0.0,
                        f64::from(width) / scale,
                        f64::from(height) / scale,
                    ),
                    scale as f32,
                );
                engine.send(WebViewCommand::Navigate("about:frame-regression".into()));
                let initial = engine.commands().len();
                let mut scene = Scene::new();
                view.draw(
                    &mut scene,
                    Affine::scale(scale),
                    &mut TextRenderer::new(),
                    &Theme::dark(),
                    &PresenceCtx::default(),
                );
                assert!(
                    scene
                        .encoding()
                        .draw_tags
                        .contains(&vello_encoding::DrawTag::IMAGE),
                    "exact {width}x{height} frame at {scale} must draw"
                );
                assert_eq!((view.cached_key.1, view.cached_key.2), (width, height));
                assert_eq!(
                    engine.commands().len(),
                    initial,
                    "valid paint must not retry resize"
                );
            }
        }

        {
            // CEF rounds outward twice; an engine rounding the logical size to
            // nearest turns an 802x451 request at 3x into 267x150 logical /
            // 801x450 physical.
            for (requested, painted, scale) in [
                ((802, 451), (804, 452), 1.75),
                ((802, 451), (801, 450), 3.0),
                ((801, 451), (802, 452), 2.0),
            ] {
                let engine = TestEngine::new();
                let mut view = WebView::new(engine.clone());
                view.set_viewport(
                    Rect::new(
                        0.0,
                        0.0,
                        f64::from(requested.0) / scale,
                        f64::from(requested.1) / scale,
                    ),
                    scale as f32,
                );
                *engine.frames().lock().unwrap() = Some(crate::webview::Frame {
                    trace_id: None,
                    damage: None,
                    rgba: Arc::new(vec![255; painted.0 as usize * painted.1 as usize * 4]),
                    width: painted.0,
                    height: painted.1,
                });
                let initial = engine.commands().len();
                let mut scene = Scene::new();
                view.draw(
                    &mut scene,
                    Affine::scale(scale),
                    &mut TextRenderer::new(),
                    &Theme::dark(),
                    &PresenceCtx::default(),
                );
                assert!(
                    scene
                        .encoding()
                        .draw_tags
                        .contains(&vello_encoding::DrawTag::IMAGE),
                    "{painted:?} paint for {requested:?} at {scale} must draw"
                );
                assert_eq!((view.cached_key.1, view.cached_key.2), painted);
                assert_eq!(engine.commands().len(), initial);
            }
        }

        {
            let engine = TestEngine::new();
            let mut view = WebView::new(engine.clone());
            view.set_viewport(Rect::new(0.0, 0.0, 16.0, 16.0), 1.0);
            let frame = crate::webview::Frame {
                trace_id: Some(101),
                damage: None,
                rgba: Arc::new(vec![255; 16 * 16 * 4]),
                width: 16,
                height: 16,
            };
            crate::webview::frame_trace::take_for_test();
            for available in [Some(frame), None] {
                *engine.frames().lock().unwrap() = available;
                let mut scene = Scene::new();
                view.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut TextRenderer::new(),
                    &Theme::dark(),
                    &PresenceCtx::default(),
                );
                assert!(
                    scene
                        .encoding()
                        .draw_tags
                        .contains(&vello_encoding::DrawTag::IMAGE)
                );
                assert_eq!(
                    crate::webview::frame_trace::take_for_test(),
                    (vec![101], false)
                );
            }
            *engine.frames().lock().unwrap() = Some(crate::webview::Frame {
                trace_id: Some(102),
                damage: None,
                rgba: Arc::new(vec![255; 8 * 8 * 4]),
                width: 8,
                height: 8,
            });
            let mut scene = Scene::new();
            view.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                &PresenceCtx::default(),
            );
            assert!(
                !scene
                    .encoding()
                    .draw_tags
                    .contains(&vello_encoding::DrawTag::IMAGE)
            );
            assert_eq!(
                crate::webview::frame_trace::take_for_test(),
                (vec![], false)
            );
        }

        {
            let engine = TestEngine::new();
            let mut view = WebView::new(engine.clone());
            view.set_viewport(Rect::new(0.0, 0.0, 16.0, 16.0), 1.0);
            *engine.frames().lock().unwrap() = Some(crate::webview::Frame {
                trace_id: Some(201),
                damage: None,
                rgba: Arc::new(vec![255; 16 * 16 * 4]),
                width: 16,
                height: 16,
            });
            let mut root = crate::widget::Column::new()
                .push(Box::new(crate::widget::Row::new().push(Box::new(view))));
            crate::webview::frame_trace::take_for_test();
            for pass in 0..3 {
                if pass == 1 {
                    *engine.frames().lock().unwrap() = None;
                } else if pass == 2 {
                    *engine.frames().lock().unwrap() = Some(crate::webview::Frame {
                        trace_id: Some(202),
                        damage: None,
                        rgba: Arc::new(vec![128; 16 * 16 * 4]),
                        width: 16,
                        height: 16,
                    });
                }
                let mut scene = Scene::new();
                root.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut TextRenderer::new(),
                    &Theme::dark(),
                    &PresenceCtx::default(),
                );
                assert!(
                    scene
                        .encoding()
                        .draw_tags
                        .contains(&vello_encoding::DrawTag::IMAGE)
                );
                let expected = if pass == 2 { vec![202] } else { vec![201] };
                assert_eq!(
                    crate::webview::frame_trace::take_for_test(),
                    (expected, false),
                    "pass {pass}"
                );
            }
        }

        {
            let engine = TestEngine::new();
            let (mut w, _tree, _node) = laid_out(engine.clone());
            engine.send(WebViewCommand::Navigate("https://example.org".into()));

            let mut scene = Scene::new();
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            let presence = PresenceCtx::default();
            w.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            assert!(
                w.cached_image.is_some(),
                "draw caches the engine frame as a vello image",
            );
        }
    }

    #[test]
    fn pointer_press_uses_released_modifiers_after_a_native_shortcut() {
        let engine = Arc::new(NativeHistory {
            inner: TestEngine::new(),
            requests: std::sync::Mutex::new(Vec::new()),
            modifiers: std::sync::Mutex::new(Vec::new()),
            pointer_modifiers: std::sync::Mutex::new(Vec::new()),
        });
        engine.set_modifiers(KeyModifiers {
            ctrl: true,
            shift: true,
            ..KeyModifiers::default()
        });
        let (mut page, tree, node) = laid_out(engine.clone());
        let device = winit::event::DeviceId::dummy();
        dispatch(
            &mut page,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: device,
                position: (50.0, 60.0).into(),
            },
        );
        dispatch(
            &mut page,
            &tree,
            node,
            &WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
        );
        assert_eq!(
            &*engine.pointer_modifiers.lock().unwrap(),
            &[KeyModifiers::default()],
            "pointer default actions must see the current released modifiers before delivery"
        );
    }

    #[test]
    fn pointer_input_forwards_inside_only_and_navigates_history() {
        {
            let engine = TestEngine::new();
            let (mut w, tree, node) = laid_out(engine.clone());
            let device = winit::event::DeviceId::dummy();
            dispatch(
                &mut w,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device,
                    position: (50.0, 60.0).into(),
                },
            );
            dispatch(
                &mut w,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device,
                    state: ElementState::Pressed,
                    button: winit::event::MouseButton::Left,
                },
            );
            let cmds = engine.commands();
            assert!(cmds.contains(&WebViewCommand::MouseMove { x: 50.0, y: 60.0 }));
            assert!(cmds.contains(&WebViewCommand::MouseButton {
                button: MouseButton::Left,
                down: true,
                x: 50.0,
                y: 60.0
            }));
        }

        {
            let engine = TestEngine::new();
            let (mut w, tree, node) = laid_out(engine.clone());
            let device = winit::event::DeviceId::dummy();
            dispatch(
                &mut w,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device,
                    position: (900.0, 900.0).into(),
                },
            );
            dispatch(
                &mut w,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device,
                    state: ElementState::Pressed,
                    button: winit::event::MouseButton::Left,
                },
            );
            assert!(
                engine
                    .commands()
                    .iter()
                    .all(|c| matches!(c, WebViewCommand::Resize { .. })),
                "only the layout resize may reach the engine",
            );
        }

        // Case `a_drag_off_the_page_follows_and_releases_there`: a selection
        // dragged out over the toolbar, let go there. The page must hear the
        // release, or it holds the button down until the next click.
        {
            let engine = TestEngine::new();
            let (mut w, tree, node) = laid_out(engine.clone());
            let device = winit::event::DeviceId::dummy();
            let to = |x: f64, y: f64| WindowEvent::CursorMoved {
                device_id: device,
                position: (x, y).into(),
            };
            let button = |state| WindowEvent::MouseInput {
                device_id: device,
                state,
                button: winit::event::MouseButton::Left,
            };
            dispatch(&mut w, &tree, node, &to(50.0, 60.0));
            dispatch(&mut w, &tree, node, &button(ElementState::Pressed));
            assert!(w.wants_pointer_capture(), "the page keeps the drag");
            dispatch(&mut w, &tree, node, &to(50.0, -20.0));
            dispatch(&mut w, &tree, node, &button(ElementState::Released));
            let commands = engine.commands();
            assert!(commands.contains(&WebViewCommand::MouseMove { x: 50.0, y: -20.0 }));
            assert_eq!(
                commands.last(),
                Some(&WebViewCommand::MouseButton {
                    button: MouseButton::Left,
                    down: false,
                    x: 50.0,
                    y: -20.0,
                })
            );
            assert!(!w.wants_pointer_capture());
            let count = engine.commands().len();
            dispatch(&mut w, &tree, node, &to(50.0, -40.0));
            dispatch(&mut w, &tree, node, &button(ElementState::Released));
            assert_eq!(engine.commands().len(), count);
        }

        // The page hears the pointer leave once, past its edge, so its
        // hover ends; then nothing until the pointer comes back.
        {
            let engine = TestEngine::new();
            let (mut w, tree, node) = laid_out(engine.clone());
            let device = winit::event::DeviceId::dummy();
            let to = |x: f64, y: f64| WindowEvent::CursorMoved {
                device_id: device,
                position: (x, y).into(),
            };
            dispatch(&mut w, &tree, node, &to(50.0, 60.0));
            dispatch(&mut w, &tree, node, &to(50.0, -20.0));
            dispatch(&mut w, &tree, node, &to(50.0, -40.0));
            let moves: Vec<_> = engine
                .commands()
                .into_iter()
                .filter(|c| matches!(c, WebViewCommand::MouseMove { .. }))
                .collect();
            assert_eq!(
                moves,
                [
                    WebViewCommand::MouseMove { x: 50.0, y: 60.0 },
                    WebViewCommand::MouseMove { x: 50.0, y: -20.0 },
                ]
            );
            // And out of the window from over the page.
            dispatch(&mut w, &tree, node, &to(50.0, 60.0));
            dispatch(
                &mut w,
                &tree,
                node,
                &WindowEvent::CursorLeft { device_id: device },
            );
            assert_eq!(
                engine.commands().last(),
                Some(&WebViewCommand::MouseMove { x: -1.0, y: -1.0 })
            );
        }

        {
            let engine = TestEngine::new();
            let (mut w, tree, node) = laid_out(engine.clone());
            let device = winit::event::DeviceId::dummy();
            dispatch(
                &mut w,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device,
                    position: (10.0, 10.0).into(),
                },
            );
            dispatch(
                &mut w,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device,
                    state: ElementState::Pressed,
                    button: winit::event::MouseButton::Back,
                },
            );
            assert!(engine.commands().contains(&WebViewCommand::GoBack));
        }

        {
            let engine = TestEngine::new();
            let (mut w, tree, node) = laid_out(engine.clone());
            let device = winit::event::DeviceId::dummy();
            dispatch(
                &mut w,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device,
                    position: (10.0, 10.0).into(),
                },
            );
            dispatch(
                &mut w,
                &tree,
                node,
                &WindowEvent::MouseWheel {
                    device_id: device,
                    delta: MouseScrollDelta::LineDelta(0.0, -2.0),
                    phase: winit::event::TouchPhase::Moved,
                },
            );
            assert!(engine.commands().contains(&WebViewCommand::Scroll {
                dx: 0.0,
                dy: -80.0,
                x: 10.0,
                y: 10.0
            }));
        }

        {
            use crate::gesture::NavDirection;
            let engine = TestEngine::new();
            let mut w = WebView::new(engine.clone());
            let state = AutoCommit::new();
            let root = automerge::ROOT;
            assert!(w.on_nav_gesture(NavDirection::Back, &state, &root));
            assert!(w.on_nav_gesture(NavDirection::Forward, &state, &root));
            let cmds = engine.commands();
            assert!(cmds.contains(&WebViewCommand::GoBack));
            assert!(cmds.contains(&WebViewCommand::GoForward));
        }
    }

    /// A view taller than the engine paints in one frame (an HTML mail
    /// sized to its whole document) asks for the largest frame the engine
    /// takes, never one it refuses, and keeps its device scale otherwise.
    #[test]
    fn a_view_never_asks_for_a_frame_past_the_engines_limits() {
        let sizes = |engine: &TestEngine| -> Vec<(u32, u32)> {
            engine
                .commands()
                .iter()
                .filter_map(|command| match command {
                    WebViewCommand::Resize { width, height, .. } => Some((*width, *height)),
                    _ => None,
                })
                .collect()
        };
        let engine = TestEngine::new();
        let mut view = WebView::new(engine.clone());
        view.set_viewport(Rect::new(0.0, 0.0, 612.0, 900.0), 2.0);
        assert_eq!(sizes(&engine).last(), Some(&(1224, 1800)));
        for (width, height) in [(612.0, 12_000.0), (3000.0, 7000.0)] {
            view.place(Rect::new(0.0, 0.0, width, height));
            let &(w, h) = sizes(&engine).last().expect("a resize");
            assert!(
                f64::from(w) <= MAX_FRAME_SIDE && f64::from(h) <= MAX_FRAME_SIDE,
                "{w}x{h}"
            );
            assert!(f64::from(w) * f64::from(h) <= MAX_FRAME_PIXELS, "{w}x{h}");
        }
        view.place(Rect::new(0.0, 0.0, 612.0, 900.0));
        assert_eq!(sizes(&engine).last(), Some(&(1224, 1800)));
    }
}

/// A web view is a window of its own where a page is all there is to show:
/// a browser tab a host streams to this desktop. It keeps no record here.
impl crate::widget::AppWidget for WebView {
    fn uses_host_record(&self) -> bool {
        false
    }
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Browser
    }
    fn app_title(&self) -> &str {
        &self.accessible_name
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("W")
    }
    fn permission_hint(&self) -> Option<crate::widget::AppKind> {
        Some(crate::widget::AppKind::Browser)
    }
}
