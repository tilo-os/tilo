//! `WebView` — a leaf widget that displays the live output of a [`tilo_webview::WebViewEngine`]
//! and forwards pointer, scroll and keyboard input into it.
use std::sync::Arc;
use tilo_ui_base::num::f32_of_f64;
use tilo_ui_base::{widget::AppModel, widget::ModelPath};

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Blob, Fill, ImageData as PenikoImage};
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key as WKey, NamedKey as WNamedKey};

use crate::widget::control_size::ControlSize;
use tilo_ui_base::gesture::NavDirection;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::widget::Widget;
use tilo_webview::{
    KeyInput, KeyModifiers, MouseButton, NamedKey, SharedEngine, WebViewCommand, WebViewEvent,
};

/// Pixels scrolled per mouse-wheel line tick, in logical units (scaled
/// by the display factor before being sent to the engine).
const WHEEL_LINE_PX: f64 = 40.0;

/// How often a floating picture asks the page where its video stands now, in milliseconds: the
/// old browser's own refresh.
const PIP_REFRESH_MS: u64 = 250;

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

pub struct WebView {
    text_input: tilo_webview::text_input::Guard,
    history_operation: Option<(
        tilo_actions::InvocationId,
        tilo_webview::text_input::Receipt,
    )>,
    rect: Rect,
    engine: SharedEngine,
    /// Physical pixels per logical pixel of the device the page is shown on
    /// (see the module docs, "Device scale"); the engine renders at it unless
    /// that passes its limits ([`Self::engine_scale`]).
    scale_factor: f32,
    host_scaled: bool,
    preseeded: bool,
    focused: bool,
    composing: bool,
    composition_emptied: bool,
    /// How much of a focused field's top the input method is told the caret
    /// takes, in logical pixels, when the engine knows the field but not its
    /// caret: a line of text with padding around it, the theme's regular control
    /// height as of the last layout. Before any layout it is the whole field.
    field_line_height: Option<f64>,
    last_mouse: Point,
    /// The buttons pressed on the page and not yet released: the page keeps
    /// the pointer until they are, wherever it goes, as a drag does.
    held: Vec<MouseButton>,
    pointer_inside: bool,
    last_sent_size: (u32, u32),
    last_sent_scale: f32,
    /// Last expected/observed mismatch for which we requested a repaint.
    /// A matching frame clears it, so a stalled backend cannot cause a
    /// redraw-command loop on every UI frame.
    stale_resize_retry: Option<((u32, u32), (u32, u32))>,
    native_mounted: bool,
    cached_image: Option<PenikoImage>,
    cached_trace_id: Option<u64>,
    cached_key: (usize, u32, u32),
    cached_damage: Option<tilo_webview::FrameDamage>,
    drawing: Option<Drawn>,
    submitted: Option<Drawn>,
    damage: Option<Vec<Rect>>,
    accepts: Option<fn(&tilo_webview::Frame) -> bool>,
    page: crate::webview::paint::PageRenderer,
    painted_seq: u64,
    test_id: String,
    accessible_name: String,
    /// What this view reports the page's navigation and failure through, when its window holds
    /// the engine (`crate::webview::window_engine`).
    navigate: tilo_ui_base::component::Event,
    fail: tilo_ui_base::component::Event,
    /// What this view reports the shell returning its floated picture through: the person closed
    /// the picture's window, or the shell handed it back, and the app stops asking for it.
    pip_closed: tilo_ui_base::component::Event,
    /// Whether this view is the one that reads the engine's events: a window's page, not an
    /// app's view whose own code polls the engine it passed in.
    reports_events: bool,
    /// Which of the window's pages it shows: the one the person browsed, or the window's second.
    surface: crate::webview::Surface,
    /// The window's page, when this view is the window's: the one session that reads the
    /// engine's events, shared with the app's page calls, so the engine has one reader.
    session: Option<crate::webview::PageHandle>,
    /// The address this view last told the engine to open.
    opened_at: Option<String>,
    /// Whether the page's video floats above the app's window as picture-in-picture: the
    /// view's own state, set from the `pip` prop the app's record drives.
    pip: bool,
    /// A float the person asked for and the view has not handed to its window yet.
    pip_ask: std::cell::Cell<bool>,
    /// Whether the view already asked the engine what video the page shows, for this request.
    pip_media_asked: std::cell::Cell<bool>,
    /// The view's number for the next ask about the page's video.
    pip_media_next: std::cell::Cell<u64>,
    /// When the view next asks the page what video it shows while it floats: scrolling and a
    /// resize move the picture, and the surface follows it.
    pip_media_at: std::cell::Cell<Option<crate::time::Instant>>,
}

#[derive(Clone)]
struct Drawn {
    rect: Rect,
    transform: Affine,
    backing: vello::peniko::Color,
    /// The frame's pixels, held so their allocation is not another frame's.
    frame: Blob<u8>,
    damage: Option<tilo_webview::FrameDamage>,
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
            composing: false,
            composition_emptied: false,
            field_line_height: None,
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
            painted_seq: 0,
            test_id: "webview".to_string(),
            accessible_name: "Web content".to_string(),
            navigate: tilo_ui_base::component::Event::None,
            fail: tilo_ui_base::component::Event::None,
            pip_closed: tilo_ui_base::component::Event::None,
            reports_events: false,
            surface: crate::webview::Surface::Page,
            session: None,
            opened_at: None,
            pip: false,
            pip_ask: std::cell::Cell::new(false),
            pip_media_asked: std::cell::Cell::new(false),
            pip_media_next: std::cell::Cell::new(0),
            pip_media_at: std::cell::Cell::new(None),
        }
    }

    /// Show the page's video floating above the app's window, as the app's `pip` prop asks: a
    /// view built with the request hands it to its window as the player's does, and one whose
    /// request goes away gives the video back to the page.
    #[must_use]
    pub fn with_pip(mut self, pip: bool) -> Self {
        self.pip = pip;
        self.pip_ask.set(pip);
        self
    }

    /// The app's `pip` prop now: a rising edge is one float to hand the window, and a falling
    /// one puts the video back in the page.
    pub fn set_pip(&mut self, pip: bool) {
        if self.pip == pip {
            return;
        }
        self.pip = pip;
        self.pip_ask.set(pip);
        self.pip_media_asked.set(false);
    }

    /// The video the page shows, as the window's page last reported it. The session is the
    /// engine's one reader, so the view polls it before reading.
    fn page_video(&self) -> Option<tilo_webview::MediaElementInfo> {
        let page = self.session.as_ref()?;
        let mut held = page.borrow_mut();
        let session = held.as_mut()?;
        session.poll();
        session.asks().media.clone()
    }

    /// Where the page's video stands in this view, as the media element the engine last reported
    /// it: the region the floated surface mirrors.
    fn pip_area(&self, media: &tilo_webview::MediaElementInfo) -> Option<Rect> {
        let region = self.rect;
        if region.width() < 1.0 || region.height() < 1.0 {
            return None;
        }
        let [x, y, width, height] = media.viewport_rect;
        let area = Rect::new(
            region.x0 + x * region.width(),
            region.y0 + y * region.height(),
            region.x0 + (x + width).min(1.0) * region.width(),
            region.y0 + (y + height).min(1.0) * region.height(),
        );
        (area.width() >= 1.0 && area.height() >= 1.0).then_some(area)
    }

    /// What the person's request floats: the page's video where it stands in this view, the
    /// picture's shape and what it is called. `None` while the page reports no video.
    fn float_picture(&self) -> Option<tilo_ui_base::widget::PictureInPictureAsk> {
        let media = self.page_video()?;
        let area = self.pip_area(&media)?;
        let [intrinsic_width, intrinsic_height] = media.intrinsic_size;
        let intrinsic = f64::from(intrinsic_width) / f64::from(intrinsic_height.max(1));
        let aspect = if intrinsic.is_finite() && intrinsic > 0.0 {
            intrinsic
        } else {
            area.width() / area.height()
        }
        .clamp(0.4, 2.8);
        let title = if media.label.trim().is_empty() {
            self.accessible_name.clone()
        } else {
            media.label
        };
        Some(tilo_ui_base::widget::PictureInPictureAsk {
            region: area,
            aspect,
            title,
        })
    }

    /// Ask the page what video it shows while the picture floats, now and then: a page that
    /// scrolled, or a window that resized, moved the picture, and the surface follows it.
    fn recheck_page_video(&self) {
        let now = crate::time::now();
        if self.pip_media_at.get().is_some_and(|at| now < at) {
            return;
        }
        self.pip_media_at
            .set(Some(now + std::time::Duration::from_millis(PIP_REFRESH_MS)));
        let request_id = self.pip_media_next.get().wrapping_add(1);
        self.pip_media_next.set(request_id);
        self.engine
            .send(WebViewCommand::RequestMediaElement { request_id });
    }

    /// Ask the engine what video the page shows, once per float: its answer reaches the
    /// window's session as a page event and is kept in its asks, where the next take reads it.
    fn ask_page_video(&self) {
        if self.pip_media_asked.replace(true) {
            return;
        }
        let request_id = self.pip_media_next.get().wrapping_add(1);
        self.pip_media_next.set(request_id);
        self.engine
            .send(WebViewCommand::RequestMediaElement { request_id });
    }

    /// Open the address the view names, once: a page already at it stays where it is.
    pub fn open_at(&mut self, uri: Option<String>) {
        let Some(uri) = uri.filter(|uri| !uri.is_empty()) else {
            return;
        };
        if self.opened_at.as_deref() == Some(uri.as_str()) {
            return;
        }
        self.engine.send(WebViewCommand::Navigate(uri.clone()));
        self.opened_at = Some(uri);
    }

    /// Take the events a host attached to this view, without reading the engine's: an app that
    /// passed its own engine polls that engine itself.
    #[must_use]
    pub fn keeping_events(
        mut self,
        navigate: tilo_ui_base::component::Event,
        fail: tilo_ui_base::component::Event,
    ) -> Self {
        self.navigate = navigate;
        self.fail = fail;
        self
    }

    /// Read the page's events from the window's session, when the window keeps one: it is the
    /// engine's one reader, and the app's page calls read their answers from it too.
    #[must_use]
    pub fn with_page(mut self, session: Option<crate::webview::PageHandle>) -> Self {
        self.session = session;
        self
    }

    /// The events the host attaches to the view now.
    pub fn update_events(
        &mut self,
        navigate: tilo_ui_base::component::Event,
        fail: tilo_ui_base::component::Event,
    ) {
        self.navigate = navigate;
        self.fail = fail;
    }

    /// Report the shell returning this view's floated picture through `closed`: the person closed
    /// the picture's window, and the app stops asking for one.
    pub fn set_pip_closed(&mut self, closed: tilo_ui_base::component::Event) {
        self.pip_closed = closed;
    }

    /// Report the page's navigation and failure through these events, and read the engine's
    /// events here: the window that holds the engine hands them to its page, and nothing else
    /// takes them.
    #[must_use]
    pub fn reporting(
        mut self,
        navigate: tilo_ui_base::component::Event,
        fail: tilo_ui_base::component::Event,
    ) -> Self {
        self.navigate = navigate;
        self.fail = fail;
        self.reports_events = true;
        self
    }

    /// Tell the events what became of the page.
    fn report_page_events(&self) {
        if !self.reports_events {
            return;
        }
        let fire = |event: &tilo_ui_base::component::Event,
                    value: tilo_ui_base::component::Value| {
            if let tilo_ui_base::component::Event::View(intent) = event {
                intent.fire(Some(value));
            }
        };
        // The window's session reads the engine once a frame, for this view and for the app's
        // page calls alike: draining the engine here as well would give each of them half the
        // page's events.
        let reported = self.session.as_ref().and_then(|page| {
            let mut held = page.borrow_mut();
            let session = held.as_mut()?;
            session.poll();
            let events: Vec<tilo_webview::WebViewEvent> =
                std::iter::from_fn(|| session.next_event()).collect();
            Some(events)
        });
        let events = reported.unwrap_or_else(|| self.engine.drain_events());
        for event in events {
            match event {
                WebViewEvent::LoadFinishedAt { uri } | WebViewEvent::UriChanged(uri) => {
                    fire(&self.navigate, tilo_ui_base::component::Value::Text(uri));
                }
                WebViewEvent::LoadFailed { error, .. } => {
                    fire(&self.fail, tilo_ui_base::component::Value::Text(error));
                }
                _ => {}
            }
        }
    }

    /// While the session shares this window's page with someone watching,
    /// hand it the page as a paint stream — the view on screen is the page
    /// the window shows. It is taken where the engine's events are taken and
    /// where the page is drawn, so a change reaches the share with no frame
    /// drawn for it. A payload larger than a share hands over
    /// ([`paint::MAX_SHARE_PAYLOAD`](crate::webview::paint::MAX_SHARE_PAYLOAD))
    /// is not dropped: its size and its start are queued, which is how the
    /// session learns that the page is too large and says so.
    fn share_page(&self) {
        use crate::webview::paint;
        // Only the page the person browsed is the window's page to share: a side panel or a
        // sign-in page beside it is not what someone watching the window came for.
        if self.surface != crate::webview::Surface::Page || !paint::share_watched() {
            return;
        }
        let keyframe = paint::take_share_keyframe();
        let page = [
            self.rect.x0,
            self.rect.y0,
            self.rect.width(),
            self.rect.height(),
        ];
        let scale = f64::from(self.engine_scale());
        for payload in self
            .engine
            .take_paint_stream_within(keyframe, paint::share_room())
        {
            // Encoded past what the paint stream's wire takes too: the size is what is
            // said of a payload too large.
            let mut bytes = match paint::stream::encode(&payload) {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "a payload of the shared page could not be encoded");
                    continue;
                }
            };
            let of = bytes.len();
            if of > paint::MAX_SHARE_PAYLOAD {
                tracing::warn!(
                    bytes = of,
                    largest = paint::MAX_SHARE_PAYLOAD,
                    "a payload of the shared page is too large to hand over"
                );
                bytes.truncate(paint::MAX_SHARE_PART);
            }
            paint::queue_share(paint::SharedFrame {
                payload: bytes,
                of,
                page,
                scale,
            });
        }
    }

    #[must_use]
    pub fn with_scale_factor(mut self, scale_factor: f32) -> Self {
        if scale_factor.is_finite() && scale_factor > 0.0 {
            self.scale_factor = scale_factor;
            self.preseeded = true;
        }
        self
    }

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

    pub fn set_accessible_name(&mut self, name: impl Into<String>) {
        self.accessible_name = name.into();
    }

    /// Say which of the window's pages this view shows: the one the person browsed, or the
    /// window's second page beside or over it.
    pub fn set_surface(&mut self, surface: crate::webview::Surface) {
        self.surface = surface;
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

    pub fn lands_at(&mut self, transform: Affine) {
        if !self.host_scaled {
            let coefficients = tilo_ui_base::scroll_copy::on_screen(transform).as_coeffs();
            self.rescale(f32_of_f64(coefficients[0].hypot(coefficients[1])));
        }
    }

    pub fn refresh_frame(&mut self) -> bool {
        self.adopt_frame();
        self.cached_image.is_some()
    }

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
            self.painted_seq = page.seq;
            self.page.draw(&mut canvas, &page);
            tilo_webview::profile::record_draw(self.rect.width(), self.rect.height(), None);
            return None;
        }

        if let Some(stale) = self.adopt_frame() {
            tilo_webview::profile::record_draw(self.rect.width(), self.rect.height(), Some(stale));
            return None;
        }

        if let Some(img) = self.cached_image.clone() {
            let (_, iw, ih) = self.cached_key;
            tilo_webview::profile::record_draw(
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
            tilo_webview::frame_trace::drawn(self.cached_trace_id);
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
        tilo_webview::profile::record_draw(self.rect.width(), self.rect.height(), None);
        None
    }

    fn note_image_delta(&self, image: &PenikoImage, damage: Option<&tilo_webview::FrameDamage>) {
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

    fn rescale(&mut self, scale: f32) {
        if !scale.is_finite() || scale <= 0.0 || scale.to_bits() == self.scale_factor.to_bits() {
            return;
        }
        self.scale_factor = scale;
        self.last_sent_size = (0, 0);
        self.send_resize_if_needed();
    }

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
        tilo_webview::profile::record_resize(w, h, scale);
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
            tilo_webview::profile::record_resize(expected.0, expected.1, scale);
            self.engine.send(WebViewCommand::Resize {
                width: expected.0,
                height: expected.1,
                scale_factor: scale,
            });
        }
        false
    }

    fn deferred_ready(&self) -> bool {
        self.focused && self.engine.supports_text_input() && self.text_input.ready()
    }

    fn page_field(&self) -> Option<tilo_webview::TextField> {
        (self.focused && self.engine.supports_composition())
            .then(|| self.engine.text_field())
            .flatten()
    }

    /// A page box the engine reported, in physical view pixels, in this
    /// view's logical coordinates and inside it; `None` once it is out of
    /// sight.
    fn engine_to_view(&self, page: Rect) -> Option<Rect> {
        let scale = f64::from(self.engine_scale());
        let placed = Rect::new(
            self.rect.x0 + page.x0 / scale,
            self.rect.y0 + page.y0 / scale,
            self.rect.x0 + page.x1 / scale,
            self.rect.y0 + page.y1 / scale,
        )
        .intersect(self.rect);
        // A box with no width (a caret) stays one; any other must show.
        (placed.height() > 0.0 && (placed.width() > 0.0 || page.width() <= 0.0)).then_some(placed)
    }

    fn settle_composition(&mut self) {
        if std::mem::take(&mut self.composition_emptied) && self.composing {
            self.composing = false;
            self.engine.send(WebViewCommand::ImeCancel);
        }
    }

    fn input_method(&mut self, ime: &winit::event::Ime) {
        match ime {
            winit::event::Ime::Preedit(text, _) if text.is_empty() => {
                self.composition_emptied = self.composing;
            }
            winit::event::Ime::Preedit(text, cursor) => {
                self.text_input.invalidate(&self.engine);
                self.composing = true;
                self.composition_emptied = false;
                self.engine.send(WebViewCommand::ImeComposition {
                    text: text.clone(),
                    cursor: *cursor,
                });
            }
            winit::event::Ime::Commit(text) => {
                self.text_input.invalidate(&self.engine);
                self.composing = false;
                self.composition_emptied = false;
                if !text.is_empty() {
                    self.engine.send(WebViewCommand::ImeCommit(text.clone()));
                }
            }
            winit::event::Ime::Disabled => {
                self.composition_emptied = false;
                if std::mem::take(&mut self.composing) {
                    self.engine.send(WebViewCommand::ImeFinish);
                }
            }
            winit::event::Ime::Enabled => {}
        }
    }

    /// Map a winit logical key to the engine's key model. Pure so tests can
    /// pin the table without synthesising `winit::KeyEvent`s (whose
    /// platform fields aren't portably constructible).
    /// The key a page is forwarded for `key`, if a page takes it: a
    /// character, or one of the named keys it knows.
    #[must_use]
    pub fn map_key(key: &WKey) -> Option<KeyInput> {
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

impl tilo_ui_base::widget::NativeViewLifecycle for WebView {
    fn mount_native_view(&mut self, host: &tilo_ui_base::widget::NativeViewHost<'_>) {
        let first_mount = !self.native_mounted;
        self.native_mounted = true;
        let host_scale = f32_of_f64(host.scale_factor());
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

impl tilo_ui_base::widget::undo_access::LocalUndo for WebView {
    fn local_undo_is_deferred(&self) -> bool {
        true
    }
    fn local_undo_status(&self) -> Option<tilo_ui_base::widget::undo_access::LocalUndoStatus> {
        (self.focused && self.engine.supports_guarded_edit_history()).then(|| {
            self.text_input.history_status().map_or(
                tilo_ui_base::widget::undo_access::LocalUndoStatus::Pending,
                tilo_ui_base::widget::undo_access::LocalUndoStatus::Ready,
            )
        })
    }

    fn handle_local_undo_request(
        &mut self,
        request: tilo_ui_base::widget::undo_access::UndoRequest,
    ) -> bool {
        if self.focused
            && self.engine.supports_guarded_edit_history()
            && self.text_input.history(request).is_some()
        {
            return true;
        }
        if !self.focused || !self.engine.handle_undo_request(request) {
            return false;
        }
        self.text_input.invalidate(&self.engine);
        true
    }
}

/// The page's video, floated above the app's window as a player's picture is: the view hands
/// its window the picture where the page draws it, so the surface mirrors the live video, and
/// takes the person's request once. A page that reports no video yet keeps the request until
/// the engine says what it shows.
impl tilo_ui_base::widget::PictureInPictureProvider for WebView {
    fn wants_picture_in_picture(&self) -> bool {
        self.pip
    }

    fn take_picture_in_picture(&self) -> Option<tilo_ui_base::widget::PictureInPictureAsk> {
        if !self.pip {
            return None;
        }
        if !self.pip_ask.replace(false) {
            return None;
        }
        let Some(ask) = self.float_picture() else {
            self.pip_ask.set(true);
            self.ask_page_video();
            return None;
        };
        Some(ask)
    }

    fn picture_in_picture_floating(&self) -> bool {
        self.pip
    }

    /// The shell returned the picture: the person closed its window, or handed it back to the
    /// page. The app hears it and stops asking for one, so its control offers the float again.
    fn picture_in_picture_returned(&mut self) {
        if let Some(mut closed) = self.pip_closed.clone().handler() {
            closed();
        }
    }

    /// Where the video stands now, once the page moved it: the view asks the page again now and
    /// then while it floats, so scrolling and a resize carry the picture with the surface.
    fn floating_picture_in_picture_region(&self) -> Option<Rect> {
        if !self.pip {
            return None;
        }
        self.recheck_page_video();
        let media = self.page_video()?;
        self.pip_area(&media)
    }

    /// The page's frame, cropped to the video and drawn over the floating window's own overlay.
    fn draw_picture_in_picture(
        &mut self,
        source: Rect,
        target: Rect,
        ctx: &mut tilo_ui_base::draw::DrawCtx<'_>,
    ) -> bool {
        #[allow(unused_variables, reason = "a draw binds only the parts it uses")]
        let (scene, transform, _text, _theme, _env) = ctx.parts();
        let Some(image) = self.cached_image.clone() else {
            return false;
        };
        if self.rect.width() < 1.0
            || self.rect.height() < 1.0
            || source.width() < 1.0
            || source.height() < 1.0
            || target.width() < 1.0
            || target.height() < 1.0
        {
            return false;
        }
        // The frame's pixels are the whole viewport at the device's scale: map them so the
        // video's part of the page fills `target`, where the shell stands the floating window.
        let scale = 1.0 / f64::from(self.engine_scale());
        let onto = Affine::translate(target.origin().to_vec2())
            .pre_scale_non_uniform(
                target.width() / source.width(),
                target.height() / source.height(),
            )
            .pre_translate(-source.origin().to_vec2());
        let brush_transform = onto
            .pre_translate(self.rect.origin().to_vec2())
            .pre_scale(scale);
        let brush = vello::peniko::ImageBrush::new(image);
        scene.fill(
            Fill::NonZero,
            transform,
            &brush,
            Some(brush_transform),
            &target,
        );
        true
    }
}

impl Widget for WebView {
    tilo_ui_base::described!();

    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::NativeViewLifecycle,
        dyn tilo_ui_base::widget::undo_access::LocalUndo,
        dyn tilo_ui_base::widget::ResourceOperations,
        dyn tilo_ui_base::widget::PictureInPictureProvider
    );

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        // The page's navigation and failure, which the window's page reports as its events.
        self.report_page_events();
        // The engine's events were just taken: what they changed of a watched page goes to
        // its share now, whether or not a frame is drawn for it.
        self.share_page();
        self.settle_composition();
        self.text_input.pump(
            &self.engine,
            self.focused && self.engine.supports_text_input(),
        );
    }

    fn ime_content(&self) -> Option<tilo_ui_base::ime::InputContent> {
        if let Some(field) = self.page_field() {
            // A password never reaches the input method: its keys go to
            // the page as they are, as in a native password field.
            return (field.purpose != tilo_ui_base::ime::InputPurpose::Password).then(|| {
                tilo_ui_base::ime::InputContent {
                    purpose: field.purpose,
                    hints: tilo_ui_base::ime::InputHints {
                        multiline: field.multiline,
                        ..tilo_ui_base::ime::InputHints::default()
                    },
                }
            });
        }
        self.deferred_ready()
            .then_some(tilo_ui_base::ime::InputContent::TEXT)
    }

    fn ime_multiline(&self) -> bool {
        self.ime_content().is_some()
            && (self.page_field().is_some_and(|field| field.multiline)
                || self.text_input.multiline())
    }

    /// Where the person types: the text being composed while an input
    /// method composes, else the top line of the focused field, so the
    /// input method's candidates open beside it. An engine that cannot
    /// place the field leaves the view's bottom-left corner.
    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.ime_content()?;
        let field = self.page_field();
        let composition = field
            .and_then(|field| field.composition)
            .filter(|_| self.composing)
            .and_then(|page| self.engine_to_view(page));
        let line = field.and_then(|field| field.bounds).and_then(|page| {
            let placed = self.engine_to_view(page)?;
            Some(Rect::new(
                placed.x0,
                placed.y0,
                placed.x1,
                self.field_line_height
                    .map_or(placed.y1, |line| placed.y1.min(placed.y0 + line)),
            ))
        });
        Some(composition.or(line).unwrap_or(Rect::new(
            self.rect.x0,
            self.rect.y1 - 1.0,
            self.rect.x0 + 1.0,
            self.rect.y1,
        )))
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_content()?;
        self.deferred_ready()
            .then(|| self.text_input.stamp(key))
            .flatten()
    }

    fn ime_requires_deferred_ack(&self) -> bool {
        true
    }

    fn ime_refuses_ordinary_commits(&self) -> bool {
        !self.engine.supports_composition()
    }

    fn start_deferred_text(&mut self) -> Option<tilo_webview::text_input::Receipt> {
        let receipt = self.focused.then(|| self.text_input.start()).flatten();
        Some(receipt.unwrap_or_else(|| {
            tilo_webview::text_input::Receipt::failed(tilo_webview::text_input::Failure::Rejected)
        }))
    }

    fn commit_deferred_text(&mut self, text: &str) -> Option<tilo_webview::text_input::Receipt> {
        let receipt = self
            .focused
            .then(|| self.text_input.commit(&self.engine, text))
            .flatten();
        Some(receipt.unwrap_or_else(|| {
            let receipt = tilo_webview::text_input::Receipt::pending();
            receipt.complete(Err(tilo_webview::text_input::Failure::Rejected));
            receipt
        }))
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        // A watched page's engine has work it wakes nobody for: the loop comes back then,
        // takes its events and its stream, and draws nothing unless the page did.
        ((self.focused || self.text_input.has_work()) && self.engine.supports_text_input())
            .then(|| self.text_input.deadline())
            .into_iter()
            .chain(self.engine.paint_due())
            .min()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.field_line_height = Some(ControlSize::Regular.height(theme));
        taffy.widget_leaf(
            tilo_ui_base::widget::layout::natural_filling(),
            node_map,
            self,
        )
    }

    /// Where nothing gives it a height (a column that grows with what it holds), the page stands
    /// at a laptop screen's 16:10 across the width it has, so the column grows to hold it.
    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        tilo_ui_base::widget::layout::natural_shape(known, available, 16.0 / 10.0)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        self.send_resize_if_needed();
    }

    /// A page's shape across the width it is given: placed once, it asks for no layout.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        if let WindowEvent::Ime(ime) = event {
            if self.focused && self.engine.supports_composition() {
                self.input_method(ime);
                *ctx.handled = true;
            }
            return;
        }
        self.settle_composition();
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
                        tilo_webview::profile::record_input("mouse");
                        self.engine.send(WebViewCommand::MouseButton {
                            button: mapped,
                            down,
                            x,
                            y,
                        });
                        *ctx.handled = true;
                    }
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
                tilo_webview::profile::record_input("scroll");
                self.engine.send(WebViewCommand::Scroll { dx, dy, x, y });
                *ctx.handled = true;
                tilo_ui_base::widget::note_view_only_pan();
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                if !self.focused {
                    return;
                }
                if let Some(key) = Self::map_key(&key_event.logical_key) {
                    if key_event.state == ElementState::Pressed {
                        self.text_input.invalidate(&self.engine);
                    }
                    tilo_webview::profile::record_input("key");
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
                        code: tilo_webview::key_code::dom_code(key_event.physical_key),
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

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        Some(
            tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Document)
                .with_name(self.accessible_name.clone())
                .with_state(
                    tilo_ui_base::a11y::AccessState::new()
                        .focusable(true)
                        .focused(self.focused),
                ),
        )
    }

    fn needs_redraw(&self) -> bool {
        self.needs_animation_frame() || self.painted_seq != self.engine.paint_seq()
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
            self.settle_composition();
            self.composing = false;
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

impl tilo_ui_base::widget::ResourceOperations for WebView {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[tilo_ui_base::widget::undo_access::EDIT_OPERATION]
    }

    fn start_operation(
        &mut self,
        operation: &tilo_ui_base::widget::ResourceOperation<'_>,
        _: &mut tilo_ui_base::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<tilo_ui_base::widget::ResourceOperationStart>, String> {
        let request = operation.request();
        if request.type_id != tilo_ui_base::widget::undo_access::EDIT_OPERATION {
            return Ok(None);
        }
        if !self.focused || !self.engine.supports_guarded_edit_history() {
            return Ok(None);
        }
        if let Some((invocation, _)) = &self.history_operation {
            return if *invocation == request.invocation {
                Ok(Some(tilo_ui_base::widget::ResourceOperationStart::Accepted))
            } else {
                Err("An original-field history request is already pending".into())
            };
        }
        let direction: tilo_ui_base::widget::undo_access::UndoRequest =
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
        Ok(Some(tilo_ui_base::widget::ResourceOperationStart::Accepted))
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
                    message: tilo_ui_base::i18n::I18nManager::lookup_plain(
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
    use automerge::AutoCommit;
    use std::sync::Arc;
    use tilo_ui_base::presence::PresenceCtx;
    use tilo_webview::WebViewEngine; // send()/commands() on the concrete TestEngine
    use tilo_webview::test_engine::TestEngine;
    use winit::event::{ElementState, WindowEvent};

    tilo_test_env::scenario! {
        fn native_history_goes_only_to_the_focused_page_that_was_asked;

        phase native_history_operation_waits_for_renderer_ack_and_rejects_focus_change;
        {
            use tilo_webview::text_input::Transactions;
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
                fn frames(&self) -> tilo_webview::FrameSlot {
                    self.inner.frames()
                }
                fn drain_events(&self) -> Vec<tilo_webview::WebViewEvent> {
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
                    request: tilo_webview::text_input::Request,
                ) -> Option<tilo_webview::text_input::Receipt> {
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
                let edit = crate::host_chrome::edit::edit_resource(&tilo_ui_base::widget::undo_access::UndoStatus {
                    can_undo: true,
                    ..Default::default()
                });
                let violations = tilo_actions::validate_surface(
                    &[edit],
                    &tilo_actions::ActionRegistry::new(),
                    &tilo_ui_base::widget::resource_operation_handlers(&page),
                );
                assert!(
                    violations.is_empty(),
                    "native Edit publication: {violations:?}"
                );
                let mut doc = AutoCommit::new();
                page.sync(&doc, &automerge::ROOT);
                page.sync(&doc, &automerge::ROOT);
                let invocation = tilo_actions::InvocationId::new();
                let payload = serde_json::to_vec(&tilo_ui_base::widget::undo_access::UndoRequest::Undo).unwrap();
                let operation = tilo_ui_base::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    crate::host_chrome::edit::EDIT_OPERATION,
                    &payload,
                );
                assert_eq!(
                    tilo_ui_base::widget::start_operation(
                        &mut page,
                        &operation,
                        &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                        &automerge::ROOT
                    )
                    .unwrap(),
                    Some(tilo_ui_base::widget::ResourceOperationStart::Accepted)
                );
                assert!(
                    tilo_ui_base::widget::poll_resource_operation(
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
                        &tilo_ui_base::synthetic_input::key_input(
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
                        tilo_ui_base::widget::poll_resource_operation(
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
                let terminal = tilo_ui_base::widget::poll_resource_operation(
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
                    tilo_ui_base::widget::poll_resource_operation(
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

        phase system_undo_delegates_only_to_focused_native_page;
        {
            use tilo_ui_base::{widget::undo_access::UndoRequest, widget::undo_access::handle_local_undo_request};
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
    }

    struct NativeHistory {
        inner: tilo_webview::SharedEngine,
        requests: std::sync::Mutex<Vec<tilo_ui_base::widget::undo_access::UndoRequest>>,
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
        fn frames(&self) -> tilo_webview::FrameSlot {
            self.inner.frames()
        }
        fn drain_events(&self) -> Vec<tilo_webview::WebViewEvent> {
            self.inner.drain_events()
        }
        fn wrapped(&self) -> Option<&tilo_webview::SharedEngine> {
            Some(&self.inner)
        }
        fn handle_undo_request(
            &self,
            request: tilo_ui_base::widget::undo_access::UndoRequest,
        ) -> bool {
            self.requests
                .lock()
                .expect("native history fixture")
                .push(request);
            true
        }
    }

    tilo_test_env::scenario! {
        fn input_is_forwarded_to_the_page_it_is_meant_for;

        phase keyboard_focus_is_forwarded_to_the_page;
        {
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

        phase pointer_press_uses_released_modifiers_after_a_native_shortcut;
        {
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

        phase pointer_input_forwards_inside_only_and_navigates_history;
        {
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
                use tilo_ui_base::gesture::NavDirection;
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
    }

    pub(super) fn laid_out(engine: SharedEngine) -> (WebView, TaffyTree<()>, NodeId) {
        let mut w = WebView::new(engine);
        let (tree, node) = place_in_column(&mut w);
        (w, tree, node)
    }

    /// Lay `view` out filling the 400 x 300 a window's column gives it.
    fn place_in_column(view: &mut WebView) -> (TaffyTree<()>, NodeId) {
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let node = view.build_layout(&mut tree, &Theme::dark(), &mut node_map);
        let column = tree
            .new_with_children(
                taffy::Style {
                    flex_direction: taffy::FlexDirection::Column,
                    size: taffy::Size::from_lengths(400.0, 300.0),
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        tree.compute_layout(
            column,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(400.0),
                height: taffy::AvailableSpace::Definite(300.0),
            },
        )
        .unwrap();
        view.apply_layout(&tree, node, Point::ZERO);
        (tree, node)
    }

    tilo_test_env::scenario! {
        fn a_view_never_asks_for_a_frame_past_the_engines_limits;

        phase a_view_never_asks_for_a_frame_past_the_engines_limits;
        // A view taller than the engine paints in one frame (an HTML mail
        // sized to its whole document) asks for the largest frame the engine
        // takes, never one it refuses, and keeps its device scale otherwise.
        {
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

    pub(super) fn dispatch(
        w: &mut WebView,
        tree: &TaffyTree<()>,
        node: NodeId,
        event: &WindowEvent,
    ) {
        Dispatch::new().layout(tree, node).send(w, event);
    }

    fn draw_at(view: &mut WebView, transform: Affine) -> Scene {
        let mut scene = Scene::new();
        view.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            &mut scene,
            transform,
            &mut TextRenderer::new(),
            &Theme::dark(),
            PresenceCtx::default().draw_env(),
        ));
        scene
    }

    tilo_test_env::scenario! {
        fn engine_frames_are_drawn_where_they_are_placed_and_repainted_only_where_they_changed;

        phase a_caret_that_blinks_damages_and_uploads_only_the_caret;
        {
            use tilo_webview::{Frame, FrameDamage, PixelRect};
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

    }
}

impl crate::widget::AppWidget for WebView {
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

#[cfg(test)]
#[path = "webview_ime_tests.rs"]
mod ime_tests;

#[path = "webview_component.rs"]
mod component;
pub use component::WebViewProps;
