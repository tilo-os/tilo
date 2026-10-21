//! The paint stream: a page as a display list instead of pixels.
//!
//! [`PaintSource`] runs where the engine runs. It asks the engine for its
//! layout whenever the page may have changed, converts it into a
//! [`Frame`](tilo_paint_stream::Frame), resolves image URLs to content hashes
//! (reading the bytes from the engine's own cache, never fetching again) and
//! emits the frame with the [`Patch`] from the previous one. [`PaintEngine`]
//! wraps any [`WebViewEngine`] with one, so a WebView draws the list.
//!
//! [`PageRenderer`] draws a frame with this toolkit's Vello and text stack —
//! the same code natively and in the WASM client, which receives the frames
//! and patches instead of pixels and never sees a cookie or a page URL behind
//! an image.

/// The format, for engines and wrappers outside this crate.
pub use tilo_paint_stream as stream;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tilo_paint_stream::wire::Payload;
use tilo_paint_stream::{BlobRef, Color as PaintColor, Frame, Item, LayerId, Patch, Point, ROOT};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill, ImageBrush, ImageData};

use super::{FrameSlot, KeyModifiers, SharedEngine, WebViewCommand, WebViewEngine, WebViewEvent};
use crate::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use crate::time::{Duration, Instant};
use tilo_lock::MutexRecover as _;

/// Never capture more often than this: a capture walks the whole layout.
const MIN_CAPTURE_INTERVAL: Duration = Duration::from_millis(50);
/// An unanswered capture or resource request is given up after this long.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
/// Resource reads in flight at once.
const MAX_RESOURCE_REQUESTS: usize = 8;
/// Largest encoded image the source keeps.
const MAX_BLOB_BYTES: usize = 16 << 20;
/// Patches queued for a stream consumer before it must take a whole frame.
const MAX_QUEUED_PATCHES: usize = 64;
/// Largest encoded payload the app → shell channel carries once base64
/// encoded (its frames are at most 1 MiB).
pub const MAX_SHARE_PAYLOAD: usize = 700 * 1024;
/// Largest image shared whole; larger ones stay unrendered for viewers.
const MAX_SHARE_BLOB: usize = MAX_SHARE_PAYLOAD - 1024;
/// Shared frames waiting for the app to send them before it must start over.
const MAX_SHARE_QUEUE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Resource {
    Pending(u32),
    Ready([u8; 32]),
    Failed,
}

/// A new frame, and the patch from the frame before it (`None` for the first).
pub struct PaintUpdate {
    pub frame: Arc<Frame>,
    pub patch: Option<Patch>,
}

/// Host-side producer of frames and patches for one page.
pub struct PaintSource {
    viewport: Point,
    min_interval: Duration,
    dirty: bool,
    next_request: u32,
    capture: Option<(u32, Instant)>,
    last_capture: Option<Instant>,
    /// The last converted frame before image resolution, so a resource that
    /// arrives later re-resolves it without another capture.
    raw: Option<Frame>,
    frame: Option<Arc<Frame>>,
    resources: HashMap<String, Resource>,
    resource_requests: HashMap<u32, (String, Instant)>,
    blobs: HashMap<[u8; 32], Arc<Vec<u8>>>,
    /// SVG images as vector drawings; `None` for blobs that are not SVG.
    vectors: HashMap<[u8; 32], Option<Arc<tilo_paint_stream::svg::Drawing>>>,
}

impl PaintSource {
    #[must_use]
    pub fn new(viewport: Point) -> Self {
        Self {
            viewport,
            min_interval: MIN_CAPTURE_INTERVAL,
            dirty: true,
            next_request: 0,
            capture: None,
            last_capture: None,
            raw: None,
            frame: None,
            resources: HashMap::new(),
            resource_requests: HashMap::new(),
            blobs: HashMap::new(),
            vectors: HashMap::new(),
        }
    }

    /// Shortest time between two captures.
    pub const fn set_min_interval(&mut self, interval: Duration) {
        self.min_interval = interval;
    }

    /// The page may have changed: capture again at the next opportunity.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn set_viewport(&mut self, viewport: Point) {
        if viewport != self.viewport {
            self.viewport = viewport;
            self.dirty = true;
        }
    }

    #[must_use]
    pub fn frame(&self) -> Option<Arc<Frame>> {
        self.frame.clone()
    }

    #[must_use]
    pub fn blob(&self, hash: &[u8; 32]) -> Option<Arc<Vec<u8>>> {
        self.blobs.get(hash).cloned()
    }

    fn request_id(&mut self) -> u32 {
        // Engine ids are positive i32s.
        self.next_request = self.next_request % 0x7fff_fffe + 1;
        self.next_request
    }

    /// Commands the engine should run now.
    pub fn poll(&mut self, now: Instant) -> Vec<WebViewCommand> {
        let mut out = Vec::new();
        if self
            .capture
            .is_some_and(|(_, at)| now.saturating_duration_since(at) > REQUEST_TIMEOUT)
        {
            self.capture = None;
            self.dirty = true;
        }
        let expired: Vec<u32> = self
            .resource_requests
            .iter()
            .filter(|(_, (_, at))| now.saturating_duration_since(*at) > REQUEST_TIMEOUT)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            if let Some((url, _)) = self.resource_requests.remove(&id) {
                self.resources.insert(url, Resource::Failed);
            }
        }
        let rested = self
            .last_capture
            .is_none_or(|at| now.saturating_duration_since(at) >= self.min_interval);
        if self.dirty && self.capture.is_none() && rested {
            let request_id = self.request_id();
            self.capture = Some((request_id, now));
            self.last_capture = Some(now);
            self.dirty = false;
            out.push(WebViewCommand::PaintCapture { request_id });
        }
        out.extend(self.request_resources(now));
        out
    }

    fn request_resources(&mut self, now: Instant) -> Vec<WebViewCommand> {
        let wanted: Vec<String> = self
            .raw
            .iter()
            .flat_map(Frame::blobs)
            .filter_map(|b| match b {
                BlobRef::Url(u) if !self.resources.contains_key(u) => Some(u.clone()),
                _ => None,
            })
            .collect();
        let mut out = Vec::new();
        for url in wanted {
            if self.resources.contains_key(&url) {
                continue;
            }
            if self.resource_requests.len() >= MAX_RESOURCE_REQUESTS {
                break;
            }
            let request_id = self.request_id();
            self.resources
                .insert(url.clone(), Resource::Pending(request_id));
            self.resource_requests
                .insert(request_id, (url.clone(), now));
            out.push(WebViewCommand::PaintResource { request_id, url });
        }
        out
    }

    /// Images inlined as `data:` URLs need no engine round trip.
    fn resolve_inline(&mut self) {
        let inline: Vec<String> = self
            .raw
            .iter()
            .flat_map(Frame::blobs)
            .filter_map(|b| match b {
                BlobRef::Url(u) if u.starts_with("data:") && !self.resources.contains_key(u) => {
                    Some(u.clone())
                }
                _ => None,
            })
            .collect();
        for url in inline {
            let state = data_url_bytes(&url)
                .and_then(|bytes| self.store(bytes))
                .map_or(Resource::Failed, Resource::Ready);
            self.resources.insert(url, state);
        }
    }

    fn store(&mut self, bytes: Vec<u8>) -> Option<[u8; 32]> {
        if bytes.is_empty() || bytes.len() > MAX_BLOB_BYTES {
            return None;
        }
        let hash = *blake3::hash(&bytes).as_bytes();
        self.blobs.entry(hash).or_insert_with(|| Arc::new(bytes));
        Some(hash)
    }

    /// Feed an engine event. Returns an update when it produced a new frame;
    /// `false` in the second slot means the event was not the source's.
    pub fn on_event(&mut self, event: &WebViewEvent) -> (Option<PaintUpdate>, bool) {
        match event {
            WebViewEvent::PaintSnapshot {
                request_id,
                snapshot,
            } => {
                if self.capture.map(|(id, _)| id) != Some(*request_id) {
                    return (None, true);
                }
                self.capture = None;
                let Some(raw) = snapshot
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                    .and_then(|v| tilo_paint_stream::snapshot::convert(&v, self.viewport, 0).ok())
                else {
                    return (None, true);
                };
                self.raw = Some(raw);
                self.resolve_inline();
                (self.publish(), true)
            }
            WebViewEvent::PaintResource { request_id, bytes } => {
                let Some((url, _)) = self.resource_requests.remove(request_id) else {
                    return (None, true);
                };
                let state = bytes
                    .clone()
                    .and_then(|b| self.store(b))
                    .map_or(Resource::Failed, Resource::Ready);
                let ready = matches!(state, Resource::Ready(_));
                self.resources.insert(url, state);
                (if ready { self.publish() } else { None }, true)
            }
            WebViewEvent::UriChanged(_) | WebViewEvent::LoadFinishedAt { .. } => {
                self.dirty = true;
                (None, false)
            }
            _ => (None, false),
        }
    }

    /// Resolve the raw frame's images and emit it if it differs from the last.
    fn publish(&mut self) -> Option<PaintUpdate> {
        let mut next = self.raw.clone()?;
        for layer in next.layers.values_mut() {
            let items = std::mem::take(&mut layer.items);
            layer.items = items
                .into_iter()
                .filter_map(|item| self.resolve(item))
                .collect();
        }
        let previous = self.frame.clone();
        next.seq = previous.as_ref().map_or(1, |p| p.seq.saturating_add(1));
        let patch = previous.as_ref().map(|p| p.diff(&next));
        if patch.as_ref().is_some_and(Patch::is_empty) {
            return None;
        }
        let frame = Arc::new(next);
        self.frame = Some(Arc::clone(&frame));
        Some(PaintUpdate { frame, patch })
    }
}

impl PaintSource {
    /// An item as a client may receive it: images by content hash, SVG
    /// images as vectors, and nothing that names a page URL.
    fn resolve(&mut self, item: Item) -> Option<Item> {
        let Item::Image { key, rect, blob } = item else {
            return Some(item);
        };
        let hash = match &blob {
            BlobRef::Hash(hash) => *hash,
            BlobRef::Url(url) => match self.resources.get(url.as_str()) {
                Some(Resource::Ready(hash)) => *hash,
                _ => return None,
            },
        };
        let blobs = &self.blobs;
        let drawing = self.vectors.entry(hash).or_insert_with(|| {
            blobs
                .get(&hash)
                .and_then(|b| tilo_paint_stream::svg::drawing(b))
                .map(Arc::new)
        });
        Some(match drawing {
            Some(drawing) => Item::Vector {
                key,
                rect,
                view_box: drawing.0,
                shapes: drawing.1.clone(),
            },
            None => Item::Image {
                key,
                rect,
                blob: BlobRef::Hash(hash),
            },
        })
    }
}

/// One encoded frame of the page this app shares, for the session.
pub struct SharedFrame {
    pub payload: Vec<u8>,
    /// Where the page sits in the window, logical pixels: x, y, w, h.
    pub page: [f64; 4],
    /// The window's buffer scale.
    pub scale: f64,
}

/// Whether this process made an engine that can stream its page.
static PAINT_CAPABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether this app can stream a page, for it to tell the session.
#[must_use]
pub fn share_capable() -> bool {
    PAINT_CAPABLE.load(std::sync::atomic::Ordering::Relaxed)
}

struct ShareOutbox {
    watched: bool,
    keyframe: bool,
    frames: VecDeque<SharedFrame>,
    bytes: usize,
}

/// The app's one share channel: a window has one page the session shares.
static SHARE: Mutex<ShareOutbox> = Mutex::new(ShareOutbox {
    watched: false,
    keyframe: false,
    frames: VecDeque::new(),
    bytes: 0,
});

fn share() -> std::sync::MutexGuard<'static, ShareOutbox> {
    SHARE.lock_recover()
}

/// Whether the session shares this app's page with someone watching.
#[must_use]
pub fn share_watched() -> bool {
    share().watched
}

/// The session started (`true`) or stopped (`false`) sharing the page. A
/// start asks for a whole frame; a stop discards what was not sent.
pub fn set_share_watched(watched: bool) {
    let mut outbox = share();
    outbox.watched = watched;
    outbox.keyframe = watched;
    if !watched {
        outbox.frames.clear();
        outbox.bytes = 0;
    }
}

/// Whether the next shared frame must be a whole one; asking clears it.
#[must_use]
pub fn take_share_keyframe() -> bool {
    std::mem::take(&mut share().keyframe)
}

/// Queue one frame for the session. A consumer that fell this far behind
/// starts over from a whole frame.
pub fn queue_share(frame: SharedFrame) {
    let mut outbox = share();
    if !outbox.watched {
        return;
    }
    let bytes = outbox.bytes.saturating_add(frame.payload.len());
    if bytes > MAX_SHARE_QUEUE_BYTES {
        outbox.frames.clear();
        outbox.bytes = 0;
        outbox.keyframe = true;
        return;
    }
    outbox.bytes = bytes;
    outbox.frames.push_back(frame);
}

/// The frames to send the session, oldest first.
pub fn take_share_frames() -> Vec<SharedFrame> {
    let mut outbox = share();
    outbox.bytes = 0;
    outbox.frames.drain(..).collect()
}

fn data_url_bytes(url: &str) -> Option<Vec<u8>> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if meta.ends_with(";base64") {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.decode(data).ok()
    } else {
        Some(data.as_bytes().to_vec())
    }
}

struct PaintState {
    source: PaintSource,
    last_pixels: usize,
    patches: VecDeque<Patch>,
    /// A consumer fell behind or just joined: it needs a whole frame.
    keyframe_needed: bool,
    /// Blobs the stream consumer was sent since its last whole frame.
    sent_blobs: std::collections::HashSet<[u8; 32]>,
    /// Whether the app wants the page shown at all.
    wanted_visible: bool,
    /// The frame a view last drew (its pixels' address).
    presented_pixels: usize,
    /// Since when a newer frame waits for a view to draw it.
    undrawn_since: Option<Instant>,
    /// What the engine was last told: painting, or hidden.
    painting: bool,
    /// How long an undrawn page keeps painting.
    hide_after: Duration,
}

/// A page whose newest frame no view drew for this long stops painting: the
/// engine is told it is hidden, which stops its compositing and rendering
/// until a view draws it again. A window showing a still page draws nothing
/// and is not "undrawn"; one the shell keeps from drawing (a hidden tab)
/// holds the frame back. Agents keep reading and operating a hidden page.
const HIDE_UNPRESENTED: Duration = Duration::from_secs(2);

/// Any engine, drawn as a display list. Commands and events pass through;
/// the source's own requests and answers are consumed here.
pub struct PaintEngine {
    inner: SharedEngine,
    state: Mutex<PaintState>,
    /// Draw this view from the display list. Otherwise the list exists only
    /// for a session that shares the page, and is captured only while one
    /// watches ([`share_watched`]).
    draws: bool,
}

impl PaintEngine {
    #[must_use]
    pub fn new(inner: SharedEngine) -> Arc<Self> {
        Self::build(inner, MIN_CAPTURE_INTERVAL, true)
    }

    /// As [`Self::new`], capturing at most once per `interval`.
    #[must_use]
    pub fn with_interval(inner: SharedEngine, interval: Duration) -> Arc<Self> {
        Self::build(inner, interval, true)
    }

    /// An engine drawn from its own pixels, whose page is captured as a
    /// display list only while a session shares it with someone watching.
    #[must_use]
    pub fn streaming(inner: SharedEngine) -> Arc<Self> {
        Self::build(inner, MIN_CAPTURE_INTERVAL, false)
    }

    fn build(inner: SharedEngine, interval: Duration, draws: bool) -> Arc<Self> {
        PAINT_CAPABLE.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut source = PaintSource::new(Point { x: 800.0, y: 600.0 });
        source.set_min_interval(interval);
        Arc::new(Self {
            inner,
            state: Mutex::new(PaintState {
                source,
                last_pixels: 0,
                patches: VecDeque::new(),
                keyframe_needed: true,
                sent_blobs: std::collections::HashSet::new(),
                wanted_visible: true,
                presented_pixels: 0,
                undrawn_since: None,
                painting: true,
                hide_after: HIDE_UNPRESENTED,
            }),
            draws,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PaintState> {
        self.state.lock_recover()
    }

    /// The patches since the last call, oldest first, or `Err` with the
    /// current frame when the consumer must start over from a whole frame.
    pub fn take_stream(&self) -> Result<Vec<Patch>, Option<Arc<Frame>>> {
        Self::take_stream_locked(&mut self.lock())
    }

    fn take_stream_locked(state: &mut PaintState) -> Result<Vec<Patch>, Option<Arc<Frame>>> {
        if state.keyframe_needed {
            state.patches.clear();
            let frame = state.source.frame();
            // Until there is a frame, the consumer still needs a whole one.
            state.keyframe_needed = frame.is_none();
            return Err(frame);
        }
        Ok(state.patches.drain(..).collect())
    }

    /// Run the source: notice new pixels (the page changed) and issue its
    /// requests. Called on every read, so whoever watches the page drives it.
    fn pump(&self, state: &mut PaintState) {
        if !self.draws && !share_watched() {
            return;
        }
        let pixels = self
            .inner
            .frames()
            .lock_recover()
            .as_ref()
            .map_or(0, |f| Arc::as_ptr(&f.rgba) as usize);
        if pixels != state.last_pixels {
            state.last_pixels = pixels;
            state.source.invalidate();
        }
        for command in state.source.poll(crate::time::now()) {
            self.inner.send(command);
        }
    }

    /// The address of the engine's current frame's pixels; 0 for none.
    fn current_pixels(&self) -> usize {
        self.inner
            .frames()
            .lock_recover()
            .as_ref()
            .map_or(0, |frame| Arc::as_ptr(&frame.rgba) as usize)
    }

    /// Paint while the app wants the page and someone sees it: no frame waits
    /// undrawn longer than [`HIDE_UNPRESENTED`], or a session shares it with
    /// someone watching.
    fn update_visibility(&self, state: &mut PaintState) {
        let now = crate::time::now();
        if self.current_pixels() == state.presented_pixels {
            state.undrawn_since = None;
        } else if state.undrawn_since.is_none() {
            state.undrawn_since = Some(now);
        }
        let seen = state
            .undrawn_since
            .is_none_or(|since| now.saturating_duration_since(since) < state.hide_after);
        let paint = state.wanted_visible && (seen || share_watched());
        if paint != state.painting {
            state.painting = paint;
            self.inner.set_visible(paint);
        }
    }

    fn absorb(state: &mut PaintState, update: PaintUpdate) {
        if let Some(patch) = update.patch {
            if state.patches.len() >= MAX_QUEUED_PATCHES {
                state.keyframe_needed = true;
                state.patches.clear();
            } else {
                state.patches.push_back(patch);
            }
        }
    }
}

impl WebViewEngine for PaintEngine {
    fn create_tab(&self) -> Result<SharedEngine, String> {
        let tab: SharedEngine =
            Self::build(self.inner.create_tab()?, MIN_CAPTURE_INTERVAL, self.draws);
        Ok(tab)
    }
    fn set_visible(&self, visible: bool) {
        let mut state = self.lock();
        state.wanted_visible = visible;
        self.update_visibility(&mut state);
    }
    fn presented(&self) {
        let pixels = self.current_pixels();
        let mut state = self.lock();
        state.presented_pixels = pixels;
        state.undrawn_since = None;
        self.update_visibility(&mut state);
    }
    fn set_modifiers(&self, modifiers: KeyModifiers) {
        self.inner.set_modifiers(modifiers);
    }
    fn take_new_tab_requests(&self) -> Vec<(String, bool)> {
        self.inner.take_new_tab_requests()
    }
    fn supports_native_filtering(&self) -> bool {
        self.inner.supports_native_filtering()
    }
    fn send(&self, cmd: WebViewCommand) {
        if let WebViewCommand::Resize {
            width,
            height,
            scale_factor,
        } = &cmd
        {
            let scale = if *scale_factor > 0.0 {
                *scale_factor
            } else {
                1.0
            };
            let viewport = Point {
                x: *width as f32 / scale,
                y: *height as f32 / scale,
            };
            self.lock().source.set_viewport(viewport);
        }
        self.inner.send(cmd);
    }
    fn frames(&self) -> FrameSlot {
        self.inner.frames()
    }
    fn drain_events(&self) -> Vec<WebViewEvent> {
        let events = self.inner.drain_events();
        let mut state = self.lock();
        let mut passed = Vec::with_capacity(events.len());
        for event in events {
            let (update, consumed) = state.source.on_event(&event);
            if let Some(update) = update {
                Self::absorb(&mut state, update);
            }
            if !consumed {
                passed.push(event);
            }
        }
        self.pump(&mut state);
        self.update_visibility(&mut state);
        drop(state);
        passed
    }
    fn request_close(&self) -> bool {
        self.inner.request_close()
    }
    fn drain_download_events(&self) -> Vec<WebViewEvent> {
        self.inner.drain_download_events()
    }
    fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        self.inner.set_waker(waker);
    }
    fn wrapped(&self) -> Option<&SharedEngine> {
        Some(&self.inner)
    }
    fn paint_frame(&self) -> Option<Arc<Frame>> {
        let mut state = self.lock();
        self.pump(&mut state);
        if !self.draws {
            return None;
        }
        state.source.frame()
    }
    fn take_paint_stream(&self, keyframe: bool) -> Vec<Payload> {
        let mut state = self.lock();
        // A watched share keeps the page painting.
        self.update_visibility(&mut state);
        self.pump(&mut state);
        if keyframe {
            state.keyframe_needed = true;
        }
        let mut out = Vec::new();
        match Self::take_stream_locked(&mut state) {
            Err(Some(frame)) => {
                state.sent_blobs.clear();
                out.push(Payload::Key((*frame).clone()));
            }
            Err(None) => return out,
            Ok(patches) if patches.is_empty() => {}
            Ok(patches) => out.push(Payload::Patches(patches)),
        }
        // Then the images the frame references that the consumer lacks.
        let Some(frame) = state.source.frame() else {
            return out;
        };
        for hash in frame.blobs().filter_map(|b| match b {
            BlobRef::Hash(h) => Some(*h),
            BlobRef::Url(_) => None,
        }) {
            if state.sent_blobs.insert(hash)
                && let Some(bytes) = state.source.blob(&hash)
                && bytes.len() <= MAX_SHARE_BLOB
            {
                out.push(Payload::Blob {
                    hash,
                    bytes: bytes.to_vec(),
                });
            }
        }
        out
    }
    fn paint_blob(&self, hash: &[u8; 32]) -> Option<Arc<Vec<u8>>> {
        self.lock().source.blob(hash)
    }
}

fn color(c: PaintColor, opacity: f32) -> Color {
    let [r, g, b, a] = c.0;
    Color::from_rgba8(r, g, b, a).multiply_alpha(opacity)
}

fn rect(r: tilo_paint_stream::Rect) -> Rect {
    Rect::new(
        f64::from(r.x),
        f64::from(r.y),
        f64::from(r.x + r.w),
        f64::from(r.y + r.h),
    )
}

fn weight(w: u16) -> TextWeight {
    match w {
        0..=449 => TextWeight::Regular,
        450..=549 => TextWeight::Medium,
        550..=649 => TextWeight::Semibold,
        _ => TextWeight::Bold,
    }
}

/// Draws [`Frame`]s. Keeps font resolution and decoded images between frames.
#[derive(Default)]
pub struct PageRenderer {
    installed: Option<Vec<String>>,
    families: HashMap<String, FontChoice>,
    images: HashMap<[u8; 32], Option<ImageData>>,
    /// Parsed SVG path data; `None` for data that does not parse.
    paths: HashMap<String, Option<BezPath>>,
}

/// Where a renderer gets a blob's encoded bytes.
pub trait BlobSource {
    fn blob(&self, hash: &[u8; 32]) -> Option<Arc<Vec<u8>>>;
}

impl<F: Fn(&[u8; 32]) -> Option<Arc<Vec<u8>>>> BlobSource for F {
    fn blob(&self, hash: &[u8; 32]) -> Option<Arc<Vec<u8>>> {
        self(hash)
    }
}

/// Everything a draw needs besides the frame.
pub struct PageCanvas<'a> {
    pub scene: &'a mut Scene,
    /// Maps CSS pixels of the page's viewport to the scene.
    pub transform: Affine,
    pub text: &'a mut TextRenderer,
    pub blobs: &'a dyn BlobSource,
}

impl PageRenderer {
    /// A renderer for a page about to be shown. Where the image decoder is
    /// fetched on first use, this starts fetching it, so it is in by the time
    /// the page's first frame arrives rather than after its first image.
    #[must_use]
    pub fn new() -> Self {
        let _ = crate::image_decoders::decoders();
        Self::default()
    }

    /// Draw `frame`, clipped to its viewport.
    pub fn draw(&mut self, canvas: &mut PageCanvas<'_>, frame: &Frame) {
        let viewport = Rect::new(
            0.0,
            0.0,
            f64::from(frame.viewport.x),
            f64::from(frame.viewport.y),
        );
        canvas.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            canvas.transform,
            &viewport,
        );
        canvas.scene.fill(
            Fill::NonZero,
            canvas.transform,
            color(frame.background, 1.0),
            None,
            &viewport,
        );
        self.draw_layer(canvas, frame, ROOT, Point::default(), 0);
        canvas.scene.pop_layer();
    }

    fn draw_layer(
        &mut self,
        canvas: &mut PageCanvas<'_>,
        frame: &Frame,
        id: LayerId,
        parent_content: Point,
        depth: usize,
    ) {
        // A malformed frame cannot recurse without bound.
        if depth > frame.layers.len() {
            return;
        }
        let Some(layer) = frame.layers.get(&id) else {
            return;
        };
        let base = match (id, layer.anchor) {
            (ROOT, _) | (_, tilo_paint_stream::Anchor::Viewport) => Point::default(),
            (_, tilo_paint_stream::Anchor::Content) => parent_content,
        };
        let origin = Point {
            x: base.x + layer.origin.x,
            y: base.y + layer.origin.y,
        };
        let content = Point {
            x: origin.x - layer.scroll.x,
            y: origin.y - layer.scroll.y,
        };
        let layered = layer.clips || layer.opacity < 1.0;
        if layered {
            let clip = if layer.clips {
                Rect::new(
                    f64::from(origin.x),
                    f64::from(origin.y),
                    f64::from(origin.x + layer.size.x),
                    f64::from(origin.y + layer.size.y),
                )
            } else {
                Rect::new(-1e7, -1e7, 1e7, 1e7)
            };
            canvas.scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                layer.opacity.clamp(0.0, 1.0),
                canvas.transform,
                &clip,
            );
        }
        let at = canvas.transform * Affine::translate((f64::from(content.x), f64::from(content.y)));
        for item in &layer.items {
            match item {
                Item::Layer(child) => {
                    self.draw_layer(canvas, frame, *child, content, depth.saturating_add(1));
                }
                other => self.draw_item(canvas, at, other),
            }
        }
        if layered {
            canvas.scene.pop_layer();
        }
    }

    fn draw_item(&mut self, canvas: &mut PageCanvas<'_>, at: Affine, item: &Item) {
        match item {
            Item::Fill {
                rect: r,
                color: c,
                radius,
                ..
            } => {
                let shape = RoundedRect::from_rect(rect(*r), f64::from(*radius));
                canvas
                    .scene
                    .fill(Fill::NonZero, at, color(*c, 1.0), None, &shape);
            }
            Item::Border {
                rect: r,
                widths,
                colors,
                radius,
                ..
            } => draw_border(canvas.scene, at, rect(*r), widths, colors, *radius),
            Item::Text {
                rect: r,
                text,
                font,
                color: c,
                ..
            } => self.draw_text(canvas, at, *r, text, font, *c),
            Item::Image { rect: r, blob, .. } => {
                if let BlobRef::Hash(hash) = blob
                    && let Some(image) = self.image(canvas.blobs, hash)
                {
                    let target = rect(*r);
                    let sx = target.width() / f64::from(image.width.max(1));
                    let sy = target.height() / f64::from(image.height.max(1));
                    let brush_at = Affine::translate((target.x0, target.y0))
                        * Affine::scale_non_uniform(sx, sy);
                    canvas.scene.fill(
                        Fill::NonZero,
                        at,
                        &ImageBrush::new(image),
                        Some(brush_at),
                        &target,
                    );
                }
            }
            Item::Vector {
                rect: r,
                view_box,
                shapes,
                ..
            } => self.draw_vector(canvas.scene, at, *r, *view_box, shapes),
            Item::Layer(_) => {}
        }
    }

    fn draw_vector(
        &mut self,
        scene: &mut Scene,
        at: Affine,
        r: tilo_paint_stream::Rect,
        view_box: tilo_paint_stream::Rect,
        shapes: &[tilo_paint_stream::Shape],
    ) {
        if view_box.w <= 0.0 || view_box.h <= 0.0 {
            return;
        }
        // `preserveAspectRatio` default: uniform scale, centred.
        let scale = f64::from((r.w / view_box.w).min(r.h / view_box.h));
        let dx = f64::from(r.x) + (f64::from(r.w) - f64::from(view_box.w) * scale) / 2.0;
        let dy = f64::from(r.y) + (f64::from(r.h) - f64::from(view_box.h) * scale) / 2.0;
        let place = at
            * Affine::translate((dx, dy))
            * Affine::scale(scale)
            * Affine::translate((-f64::from(view_box.x), -f64::from(view_box.y)));
        for shape in shapes {
            let Some(path) = self
                .paths
                .entry(shape.path.clone())
                .or_insert_with(|| BezPath::from_svg(&shape.path).ok())
                .as_ref()
            else {
                continue;
            };
            if let Some(fill) = shape.fill {
                let rule = if shape.even_odd {
                    Fill::EvenOdd
                } else {
                    Fill::NonZero
                };
                scene.fill(rule, place, color(fill, 1.0), None, path);
            }
            if let Some(stroke) = shape.stroke
                && shape.stroke_width > 0.0
            {
                let style = Stroke::new(f64::from(shape.stroke_width));
                scene.stroke(&style, place, color(stroke, 1.0), None, path);
            }
        }
    }

    fn draw_text(
        &mut self,
        canvas: &mut PageCanvas<'_>,
        at: Affine,
        r: tilo_paint_stream::Rect,
        text: &str,
        font: &tilo_paint_stream::FontSpec,
        c: PaintColor,
    ) {
        let family = self.family(canvas.text, &font.family);
        let style = TextStyle::new(font.size)
            .with_weight(weight(font.weight))
            .with_italic(font.italic)
            .with_family(family);
        let (w, h) = canvas.text.measure_style(text, None, &style);
        // Fit our shaping to the engine's line box, so a run of different
        // fonts still starts and ends where the host laid it out.
        let sx = if w > 0.0 && r.w > 0.0 {
            f64::from((r.w / w).clamp(0.8, 1.25))
        } else {
            1.0
        };
        let dy = f64::from((r.h - h) / 2.0);
        let place = at
            * Affine::translate((f64::from(r.x), f64::from(r.y) + dy))
            * Affine::scale_non_uniform(sx, 1.0);
        canvas
            .text
            .draw_style(canvas.scene, text, color(c, 1.0), place, None, &style);
    }

    /// Resolve a CSS family list against the installed fonts, once per list.
    fn family(&mut self, text: &TextRenderer, css: &str) -> FontChoice {
        if let Some(choice) = self.families.get(css) {
            return choice.clone();
        }
        let installed = self.installed.get_or_insert_with(|| {
            text.system_families()
                .into_iter()
                .map(|f| f.to_ascii_lowercase())
                .collect()
        });
        let mut choice = FontChoice::SansSerif;
        for name in css.split(',').map(|f| f.trim().trim_matches(['"', '\''])) {
            let lower = name.to_ascii_lowercase();
            let generic = match lower.as_str() {
                "monospace" | "ui-monospace" => Some(FontChoice::Monospace),
                "serif" | "ui-serif" => Some(FontChoice::Serif),
                "sans-serif" | "system-ui" | "-apple-system" | "ui-sans-serif" => {
                    Some(FontChoice::SansSerif)
                }
                _ => None,
            };
            if let Some(g) = generic {
                choice = g;
                break;
            }
            if installed.contains(&lower) {
                choice = FontChoice::Named(name.to_string());
                break;
            }
        }
        self.families.insert(css.to_string(), choice.clone());
        choice
    }

    fn image(&mut self, blobs: &dyn BlobSource, hash: &[u8; 32]) -> Option<ImageData> {
        if let Some(cached) = self.images.get(hash) {
            return cached.clone();
        }
        // Not cached while the decoder is still to come: the page is drawn
        // again once it is in.
        let decoders = crate::image_decoders::decoders()?;
        let bytes = blobs.blob(hash)?;
        let decoded = (decoders.page)(&bytes);
        self.images.insert(*hash, decoded.clone());
        decoded
    }
}

fn draw_border(
    scene: &mut Scene,
    at: Affine,
    b: Rect,
    widths: &[f32; 4],
    colors: &[PaintColor; 4],
    radius: f32,
) {
    let [top, right, bottom, left] = widths.map(f64::from);
    let [first, ..] = *colors;
    let uniform = [right, bottom, left].iter().all(|w| (w - top).abs() < 0.01)
        && colors.iter().all(|c| *c == first);
    if uniform && top > 0.0 {
        let inset = b.inset(-top / 2.0);
        let shape = RoundedRect::from_rect(inset, (f64::from(radius) - top / 2.0).max(0.0));
        scene.stroke(&Stroke::new(top), at, color(first, 1.0), None, &shape);
        return;
    }
    let sides = [
        Rect::new(b.x0, b.y0, b.x1, b.y0 + top),
        Rect::new(b.x1 - right, b.y0, b.x1, b.y1),
        Rect::new(b.x0, b.y1 - bottom, b.x1, b.y1),
        Rect::new(b.x0, b.y0, b.x0 + left, b.y1),
    ];
    for (side, c) in sides.iter().zip(colors) {
        if side.width() > 0.0 && side.height() > 0.0 && c.0[3] > 0 {
            scene.fill(Fill::NonZero, at, color(*c, 1.0), None, side);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webview::test_engine::TestEngine;

    const TOP: &str =
        include_str!("../../../../browser/paint-stream/tests/fixtures/layers.top.json");
    const SCROLLED: &str =
        include_str!("../../../../browser/paint-stream/tests/fixtures/layers.scrolled.json");
    const TOP_PNG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/layers.top.png"));

    fn paint_engine(snapshot: &str) -> (Arc<TestEngine>, Arc<PaintEngine>) {
        let test = TestEngine::new();
        test.set_paint_snapshot(snapshot);
        let engine = PaintEngine::with_interval(test.clone(), Duration::ZERO);
        engine.send(WebViewCommand::Resize {
            width: 800,
            height: 600,
            scale_factor: 1.0,
        });
        (test, engine)
    }

    /// Drive the engine as a WebView does: read the frame, drain events.
    fn settle(engine: &PaintEngine) -> Vec<WebViewEvent> {
        let mut passed = Vec::new();
        for _ in 0..4 {
            drop(engine.paint_frame());
            passed.extend(engine.drain_events());
        }
        passed
    }

    #[test]
    fn a_snapshot_becomes_a_frame_and_the_source_keeps_its_own_traffic() {
        let (test, engine) = paint_engine(TOP);
        let passed = settle(&engine);
        let frame = engine
            .paint_frame()
            .expect("a frame from the engine's snapshot");
        assert!(
            frame.layers.len() >= 5,
            "root plus header, sidebar, ghost, button"
        );
        assert!(
            passed.iter().all(|e| !matches!(
                e,
                WebViewEvent::PaintSnapshot { .. } | WebViewEvent::PaintResource { .. }
            )),
            "paint traffic never reaches the app: {passed:?}"
        );
        assert!(
            test.commands()
                .iter()
                .any(|c| matches!(c, WebViewCommand::PaintCapture { .. })),
            "the wrapper asked the engine for its layout"
        );
        // The page's data: image is resolved to a content hash, served by hash.
        let hash = frame
            .blobs()
            .find_map(|b| match b {
                BlobRef::Hash(h) => Some(*h),
                BlobRef::Url(_) => None,
            })
            .expect("image resolved");
        let bytes = engine.paint_blob(&hash).expect("blob kept");
        assert_eq!(*blake3::hash(&bytes).as_bytes(), hash);
        assert!(
            frame.blobs().all(|b| matches!(b, BlobRef::Hash(_))),
            "no URL leaves the host"
        );
    }

    /// A new renderer starts fetching the decoder; before it is in, an image
    /// is left out and not remembered as undecodable; once it is in, the same
    /// image decodes.
    #[cfg(feature = "lazy-image-decode")]
    #[test]
    fn images_wait_for_a_deferred_decoder() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static LOADS: AtomicUsize = AtomicUsize::new(0);
        fn load() {
            LOADS.fetch_add(1, Ordering::SeqCst);
        }

        let hash = [7_u8; 32];
        let blobs = |_: &[u8; 32]| Some(Arc::new(TOP_PNG.to_vec()));
        crate::image_decoders::defer_image_decoders(load);
        let mut renderer = PageRenderer::new();
        assert_eq!(
            LOADS.load(Ordering::SeqCst),
            1,
            "a page about to be shown starts fetching it"
        );
        assert!(renderer.image(&blobs, &hash).is_none());
        assert_eq!(LOADS.load(Ordering::SeqCst), 1, "one fetch at a time");
        crate::image_decoders::install_image_decoders(
            crate::image_decoders::builtin_image_decoders,
        );
        let image = renderer
            .image(&blobs, &hash)
            .expect("decoded once it is in");
        assert_eq!((image.width, image.height), (800, 600));
    }

    #[test]
    fn svg_images_reach_clients_as_vectors() {
        let (_, engine) = paint_engine(TOP);
        settle(&engine);
        let frame = engine.paint_frame().expect("frame");
        let fills: Vec<[u8; 4]> = frame
            .layers
            .values()
            .flat_map(|l| &l.items)
            .filter_map(|i| match i {
                Item::Vector { shapes, .. } => Some(shapes),
                _ => None,
            })
            .flatten()
            .filter_map(|s| s.fill.map(|c| c.0))
            .collect();
        assert!(
            fills.contains(&[30, 90, 200, 255]),
            "the SVG <img> as a vector: {fills:?}"
        );
    }

    #[test]
    fn a_consumer_starts_from_a_whole_frame_then_follows_patches() {
        let (test, engine) = paint_engine(TOP);
        settle(&engine);
        let Err(Some(keyframe)) = engine.take_stream() else {
            panic!("a new consumer starts from a whole frame");
        };
        let mut client = (*keyframe).clone();
        assert_eq!(
            engine.take_stream().ok().map(|p| p.len()),
            Some(0),
            "nothing changed yet"
        );

        // The page scrolls: the engine paints, its next snapshot differs.
        test.set_paint_snapshot(SCROLLED);
        test.send(WebViewCommand::Navigate("https://example.test/".into()));
        settle(&engine);
        let patches = engine.take_stream().expect("following");
        assert_eq!(patches.len(), 1, "{patches:?}");
        for patch in &patches {
            let wire: Patch =
                tilo_paint_stream::decode(&tilo_paint_stream::encode(patch).expect("encode"))
                    .expect("decode");
            client.apply(&wire).expect("apply");
        }
        assert_eq!(&client, &*engine.paint_frame().expect("frame"));
    }

    #[test]
    #[serial_test::serial(paint_share)]
    fn a_streaming_engine_captures_only_while_a_session_watches() {
        let test = TestEngine::new();
        test.set_paint_snapshot(TOP);
        let engine = PaintEngine::streaming(test.clone());
        engine.send(WebViewCommand::Resize {
            width: 800,
            height: 600,
            scale_factor: 1.0,
        });
        set_share_watched(false);
        settle(&engine);
        assert!(
            engine.take_paint_stream(true).is_empty(),
            "nobody watches: nothing captured"
        );
        assert!(
            !test
                .commands()
                .iter()
                .any(|c| matches!(c, WebViewCommand::PaintCapture { .. })),
            "an unwatched page costs the engine nothing"
        );
        assert!(share_capable(), "the app can tell the session it streams");

        set_share_watched(true);
        assert!(
            take_share_keyframe(),
            "a new watcher starts from a whole frame"
        );
        settle(&engine);
        let payloads = engine.take_paint_stream(true);
        assert!(
            matches!(payloads.first(), Some(Payload::Key(_))),
            "{} payloads",
            payloads.len()
        );
        assert!(
            payloads.iter().any(|p| matches!(p, Payload::Blob { .. })),
            "with the images the page shows"
        );
        assert!(
            engine.paint_frame().is_none(),
            "the view itself still draws pixels"
        );
        assert!(
            engine.take_paint_stream(false).is_empty(),
            "nothing changed since"
        );
        set_share_watched(false);
    }

    #[test]
    #[serial_test::serial(paint_share)]
    fn a_page_no_view_draws_stops_painting_until_one_does() {
        use crate::time::testing::with_monotonic_time;
        let test = TestEngine::new();
        let engine = PaintEngine::streaming(test.clone());
        engine.lock().hide_after = Duration::from_millis(30);
        set_share_watched(false);
        let start = crate::time::now();
        let at = |ms| start + Duration::from_millis(ms);
        with_monotonic_time(at(0), || {
            engine.presented();
            drop(engine.drain_events());
        });
        assert!(
            test.visibility_changes().is_empty(),
            "drawn just now: still painting"
        );

        // A window showing a still page draws nothing, and is still seen.
        with_monotonic_time(at(60), || drop(engine.drain_events()));
        assert!(
            test.visibility_changes().is_empty(),
            "a still page on screen keeps painting"
        );

        // A new frame no view draws (a hidden tab's) stops it.
        with_monotonic_time(at(60), || {
            engine.send(WebViewCommand::Navigate("https://example.test/next".into()));
            drop(engine.drain_events());
        });
        with_monotonic_time(at(120), || drop(engine.drain_events()));
        assert_eq!(
            test.visibility_changes(),
            vec![false],
            "undrawn: told it is hidden"
        );
        with_monotonic_time(at(120), || drop(engine.drain_events()));
        assert_eq!(test.visibility_changes(), vec![false], "and told once");

        with_monotonic_time(at(120), || engine.presented());
        assert_eq!(
            test.visibility_changes(),
            vec![false, true],
            "drawn again: painting"
        );

        // A page the app hid stays hidden however recently it was drawn.
        with_monotonic_time(at(120), || {
            engine.set_visible(false);
            engine.presented();
        });
        assert_eq!(test.visibility_changes(), vec![false, true, false]);
        with_monotonic_time(at(120), || engine.set_visible(true));

        // A watched share keeps an undrawn page painting.
        set_share_watched(true);
        with_monotonic_time(at(180), || drop(engine.take_paint_stream(false)));
        assert_eq!(test.visibility_changes(), vec![false, true, false, true]);
        set_share_watched(false);
    }

    #[test]
    fn an_engine_without_layout_capture_falls_back_to_pixels() {
        let test = TestEngine::new();
        let engine = PaintEngine::with_interval(test, Duration::ZERO);
        settle(&engine);
        assert!(engine.paint_frame().is_none());
    }

    /// The toolkit draws the recorded page close to the engine's own pixels.
    #[test]
    fn the_renderer_reproduces_the_engines_pixels() {
        let (_, engine) = paint_engine(TOP);
        settle(&engine);
        let frame = engine.paint_frame().expect("frame");
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let source = Arc::clone(&engine);
        let blobs = move |hash: &[u8; 32]| source.paint_blob(hash);
        PageRenderer::new().draw(
            &mut PageCanvas {
                scene: &mut scene,
                transform: Affine::IDENTITY,
                text: &mut text,
                blobs: &blobs,
            },
            &frame,
        );
        let mut renderer = crate::offscreen::OffscreenRenderer::new(800, 600);
        let ours = renderer.render_resilient(&scene, Color::WHITE);
        let reference = image::load_from_memory(TOP_PNG).expect("png").to_rgba8();
        let (mut close, mut total) = (0_usize, 0_usize);
        for (a, b) in ours.chunks_exact(4).zip(reference.as_raw().chunks_exact(4)) {
            total += 1;
            if a.iter().zip(b).take(3).all(|(x, y)| x.abs_diff(*y) <= 24) {
                close += 1;
            }
        }
        let share = close as f64 / total as f64;
        assert!(
            share > 0.95,
            "only {:.1}% of pixels match the engine",
            share * 100.0
        );
    }
}
