//! A call's pictures.
//!
//! **Sending.** This device's camera and shared screen each have a
//! [`Publisher`]: a thread that takes the newest captured frame and encodes
//! it once for every layer someone in the call shows (the relay asks the
//! carrier for exactly those, `tilo_moq::client::Client::wanted_pictures`),
//! hardware first, in H.264, which every participant (native and browser)
//! decodes. A layer nobody shows is not encoded. A picture that stops
//! changing (a still screen) is sent again as a keyframe every
//! [`REPEAT`], so someone who starts showing it later sees it at once.
//!
//! **Receiving.** The pictures of others the app shows arrive on the carrier
//! and wait in the call's [`Inbox`] until the app collects them; a stream
//! that falls behind starts over at its next keyframe, so the app's decoder
//! never sees a picture whose reference was dropped.
//!
//! **The screen** comes from the compositor ([`screen_share`]): while a call
//! shares it, the compositor renders the composed desktop for it at most
//! [`SCREEN_FPS`] times a second, and never while the screen is locked.

use media::realtime::capture::{CaptureConfig, CaptureDevice, VideoCapture};
use media::realtime::{EncoderConfig, InputFormat, InputFrame, RealtimeEncoder, StreamCodec};
use runtime_collab::native_media::{Picture, PictureLayer, PictureSource};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tilo_lock::MutexRecover as _;
use tilo_moq::client::{Client, ClientError};
use tilo_moq::video::{Codec, Layer, Source, VideoObject};

/// A still picture is sent again, as a keyframe, this often.
pub(crate) const REPEAT: Duration = Duration::from_secs(2);
/// Camera frames a second.
pub(crate) const CAMERA_FPS: u32 = 30;
/// The camera's capture size: the largest camera layer.
pub(crate) const CAMERA_SIZE: (u32, u32) = (1280, 720);
/// Screen pictures a second.
pub(crate) const SCREEN_FPS: u32 = 15;
/// The time between two screen pictures.
const SCREEN_INTERVAL: Duration = Duration::from_millis(1000 / SCREEN_FPS as u64);
/// Bytes of received pictures held for the app: a few seconds of every
/// shown layer. Past it the oldest stream starts over at a keyframe.
const INBOX_BYTES: usize = 8 * 1024 * 1024;
/// Pictures read off the carrier in one call tick.
pub(crate) const RECEIVE_PER_TICK: usize = 64;

/// One frame to encode.
#[derive(Clone)]
pub(crate) struct Frame {
    pub(crate) pixels: Arc<[u8]>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// [`InputFormat::Nv12`] (the camera) or [`InputFormat::Rgba`] (the
    /// screen), tightly packed.
    pub(crate) format: InputFormat,
    pub(crate) captured_at: Instant,
}

/// The newest frame of one source, handed from its capture to its encoder.
#[derive(Default)]
pub(crate) struct Feed {
    state: Mutex<FeedState>,
    ready: Condvar,
}
#[derive(Default)]
struct FeedState {
    frame: Option<Frame>,
    fresh: bool,
    stopped: bool,
}
enum Next {
    Fresh(Frame),
    /// Nothing new for [`REPEAT`]: the last frame again.
    Repeat(Frame),
    Stopped,
}
impl Feed {
    pub(crate) fn publish(&self, frame: Frame) {
        let mut state = self.state.lock_recover();
        state.frame = Some(frame);
        state.fresh = true;
        drop(state);
        self.ready.notify_one();
    }
    /// Forget the last frame: nothing is sent until the next one.
    pub(crate) fn clear(&self) {
        let mut state = self.state.lock_recover();
        state.frame = None;
        state.fresh = false;
    }
    fn stop(&self) {
        self.state.lock_recover().stopped = true;
        self.ready.notify_one();
    }
    fn next(&self) -> Next {
        let mut state = self.state.lock_recover();
        let deadline = Instant::now().checked_add(REPEAT);
        loop {
            if state.stopped {
                return Next::Stopped;
            }
            if state.fresh
                && let Some(frame) = state.frame.clone()
            {
                state.fresh = false;
                return Next::Fresh(frame);
            }
            let now = Instant::now();
            let remaining =
                deadline.map_or(REPEAT, |deadline| deadline.saturating_duration_since(now));
            if remaining.is_zero() {
                if let Some(frame) = state.frame.clone() {
                    return Next::Repeat(frame);
                }
                return self.wait_fresh(state);
            }
            state = match self.ready.wait_timeout(state, remaining) {
                Ok((state, _)) => state,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }
    /// No frame at all yet: wait for one without a deadline.
    fn wait_fresh(&self, mut state: std::sync::MutexGuard<'_, FeedState>) -> Next {
        loop {
            if state.stopped {
                return Next::Stopped;
            }
            if state.fresh
                && let Some(frame) = state.frame.clone()
            {
                state.fresh = false;
                return Next::Fresh(frame);
            }
            state = match self.ready.wait(state) {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }
}

/// The screen this device shares in its calls, fed by the compositor.
pub(crate) struct ScreenShare {
    /// The feed of the call sharing the screen; replaced by each new share.
    feed: Mutex<Option<Arc<Feed>>>,
    sharing: AtomicUsize,
    next: Mutex<Option<Instant>>,
}

/// This device's screen, as its calls share it.
pub(crate) fn screen_share() -> &'static ScreenShare {
    static SCREEN: OnceLock<ScreenShare> = OnceLock::new();
    SCREEN.get_or_init(|| ScreenShare {
        feed: Mutex::new(None),
        sharing: AtomicUsize::new(0),
        next: Mutex::new(None),
    })
}

impl ScreenShare {
    /// Whether the compositor should render the desktop for a call now:
    /// a call shares the screen and its last picture is at least a
    /// [`SCREEN_FPS`] interval old.
    pub(crate) fn wants_frame(&self, now: Instant) -> bool {
        if self.sharing.load(Ordering::Acquire) == 0 {
            return false;
        }
        let mut next = self.next.lock_recover();
        if next.is_some_and(|next| now < next) {
            return false;
        }
        *next = now.checked_add(SCREEN_INTERVAL);
        true
    }
    /// The composed desktop, tightly packed RGBA.
    pub(crate) fn publish_rgba(&self, width: u32, height: u32, rgba: Arc<[u8]>) {
        let Some(frame) = even_rgba(width, height, rgba) else {
            return;
        };
        if let Some(feed) = self.feed.lock_recover().as_ref() {
            feed.publish(frame);
        }
    }
    /// The screen is locked: what it shows is never shared, and the last
    /// picture is not repeated.
    pub(crate) fn withhold(&self) {
        if let Some(feed) = self.feed.lock_recover().as_ref() {
            feed.clear();
        }
    }
    fn start(&self) -> Arc<Feed> {
        let feed = Arc::new(Feed::default());
        *self.feed.lock_recover() = Some(feed.clone());
        self.sharing.fetch_add(1, Ordering::AcqRel);
        feed
    }
    fn stop(&self, feed: &Arc<Feed>) {
        let mut current = self.feed.lock_recover();
        if current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, feed))
        {
            *current = None;
        }
        drop(current);
        let _ = self
            .sharing
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_sub(1)
            });
    }
}

/// `rgba` cropped to even dimensions, which 4:2:0 encoders need.
fn even_rgba(width: u32, height: u32, rgba: Arc<[u8]>) -> Option<Frame> {
    let (even_width, even_height) = (width & !1, height & !1);
    if even_width == 0 || even_height == 0 {
        return None;
    }
    let row = usize::try_from(width).ok()?.checked_mul(4)?;
    if rgba.len() != row.checked_mul(usize::try_from(height).ok()?)? {
        return None;
    }
    let pixels = if (even_width, even_height) == (width, height) {
        rgba
    } else {
        let even_row = usize::try_from(even_width).ok()?.checked_mul(4)?;
        let rows = usize::try_from(even_height).ok()?;
        let mut cropped = Vec::with_capacity(even_row.checked_mul(rows)?);
        for line in rgba.chunks_exact(row).take(rows) {
            cropped.extend_from_slice(line.get(..even_row)?);
        }
        Arc::from(cropped)
    };
    Some(Frame {
        pixels,
        width: even_width,
        height: even_height,
        format: InputFormat::Rgba,
        captured_at: Instant::now(),
    })
}

/// The carrier a call sends on now: it changes when the call reconnects.
pub(crate) type CurrentClient = Arc<dyn Fn() -> Option<Arc<Client>> + Send + Sync>;

/// Where a call's camera pictures come from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum CameraChoice {
    /// The device's own camera.
    #[default]
    Device,
    /// The moving test picture, for a device without a camera (a virtual
    /// machine, a nested development session): `TILO_CALL_CAMERA=test-pattern`.
    TestPattern,
}
impl CameraChoice {
    pub(crate) fn from_env() -> Self {
        if std::env::var("TILO_CALL_CAMERA").is_ok_and(|value| value == "test-pattern") {
            Self::TestPattern
        } else {
            Self::Device
        }
    }
    fn device(&self) -> Option<CaptureDevice> {
        match self {
            Self::Device => {
                media::realtime::capture::camera_node().map(|node| CaptureDevice::Camera { node })
            }
            Self::TestPattern => Some(CaptureDevice::TestPattern),
        }
    }
}

/// This person's own camera, for their own tile: encoded like everyone
/// else's at the layer that tile shows, and handed to the app without
/// leaving the device.
#[derive(Default)]
pub(crate) struct Preview {
    layer: Mutex<Option<Layer>>,
    queue: Mutex<PreviewQueue>,
}
#[derive(Default)]
struct PreviewQueue {
    objects: VecDeque<VideoObject>,
    /// Pictures were dropped: the app's decoder waits for a keyframe.
    gap: bool,
}
/// Own pictures waiting for the call's tick at most.
const PREVIEW_QUEUED: usize = 8;
impl Preview {
    /// Show this person's own picture at `layer`, or not at all.
    pub(crate) fn set(&self, layer: Option<Layer>) {
        *self.layer.lock_recover() = layer;
    }
    fn layer(&self) -> Option<Layer> {
        *self.layer.lock_recover()
    }
    fn push(&self, object: VideoObject) {
        let mut queue = self.queue.lock_recover();
        if queue.objects.len() >= PREVIEW_QUEUED {
            queue.objects.clear();
            queue.gap = true;
        }
        queue.objects.push_back(object);
    }
    /// The own pictures encoded since last asked, and whether some were
    /// dropped before them.
    pub(crate) fn take(&self) -> (Vec<VideoObject>, bool) {
        let mut queue = self.queue.lock_recover();
        let gap = std::mem::take(&mut queue.gap);
        (queue.objects.drain(..).collect(), gap)
    }
}

/// One source this device publishes in a call; see the [module docs](self).
pub(crate) struct Publisher {
    feed: Arc<Feed>,
    capture: Option<VideoCapture>,
    screen: bool,
    preview: Arc<Preview>,
}
impl Publisher {
    /// Start the camera and its encoder thread.
    pub(crate) fn camera(choice: &CameraChoice, current: CurrentClient) -> Result<Self, String> {
        let device = choice.device().ok_or("no camera")?;
        let feed = Arc::new(Feed::default());
        let into = feed.clone();
        let capture = VideoCapture::start(
            &CaptureConfig {
                device,
                width: CAMERA_SIZE.0,
                height: CAMERA_SIZE.1,
                fps: CAMERA_FPS,
            },
            Box::new(move |frame| {
                into.publish(Frame {
                    pixels: Arc::from(frame.nv12),
                    width: frame.width,
                    height: frame.height,
                    format: InputFormat::Nv12,
                    captured_at: frame.captured_at,
                });
            }),
        )
        .map_err(|error| error.to_string())?;
        let preview = Arc::new(Preview::default());
        spawn_encoder(
            Source::Camera,
            CAMERA_FPS,
            feed.clone(),
            current,
            preview.clone(),
        )?;
        Ok(Self {
            feed,
            capture: Some(capture),
            screen: false,
            preview,
        })
    }
    /// Share the screen: the compositor feeds it from now on.
    pub(crate) fn screen(current: CurrentClient) -> Result<Self, String> {
        let feed = screen_share().start();
        let preview = Arc::new(Preview::default());
        if let Err(error) = spawn_encoder(
            Source::Screen,
            SCREEN_FPS,
            feed.clone(),
            current,
            preview.clone(),
        ) {
            screen_share().stop(&feed);
            return Err(error);
        }
        Ok(Self {
            feed,
            capture: None,
            screen: true,
            preview,
        })
    }
    /// The capture ended on its own (the camera went away).
    pub(crate) fn failed(&self) -> bool {
        self.capture.as_ref().is_some_and(VideoCapture::failed)
    }
    /// This source's own picture for this person's own tile.
    pub(crate) fn preview(&self) -> &Preview {
        &self.preview
    }
}
impl Drop for Publisher {
    fn drop(&mut self) {
        // The capture stops first, then the encoder thread, which ends at
        // its next frame wait without anyone waiting for it.
        self.capture = None;
        if self.screen {
            screen_share().stop(&self.feed);
        }
        self.feed.stop();
    }
}

fn spawn_encoder(
    source: Source,
    fps: u32,
    feed: Arc<Feed>,
    current: CurrentClient,
    preview: Arc<Preview>,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name(match source {
            Source::Camera => "call-camera".into(),
            Source::Screen => "call-screen".into(),
        })
        .spawn(move || encode(source, fps, &feed, &current, &preview))
        .map(drop)
        .map_err(|error| error.to_string())
}

/// One layer's encoder.
struct LayerEncoder {
    encoder: RealtimeEncoder,
    /// The frame size it takes.
    input: (u32, u32),
    /// The picture size it sends.
    output: (u16, u16),
    sequence: u64,
}

/// The largest size within `layer` that keeps `source`'s shape, even, and
/// never larger than the source itself.
pub(crate) fn fitted(
    (width, height): (u32, u32),
    (box_width, box_height): (u16, u16),
) -> (u32, u32) {
    let (box_width, box_height) = (u32::from(box_width), u32::from(box_height));
    let (w, h) = if width <= box_width && height <= box_height {
        (width, height)
    } else if u64::from(width).saturating_mul(u64::from(box_height))
        >= u64::from(height).saturating_mul(u64::from(box_width))
    {
        let scaled =
            u64::from(height).saturating_mul(u64::from(box_width)) / u64::from(width.max(1));
        (box_width, u32::try_from(scaled).unwrap_or(box_height))
    } else {
        let scaled =
            u64::from(width).saturating_mul(u64::from(box_height)) / u64::from(height.max(1));
        (u32::try_from(scaled).unwrap_or(box_width), box_height)
    };
    ((w & !1).max(2), (h & !1).max(2))
}

impl LayerEncoder {
    fn new(source: Source, layer: Layer, frame: &Frame, fps: u32) -> Result<Self, String> {
        let (width, height) = fitted((frame.width, frame.height), layer.size(source));
        let mut config = EncoderConfig::new(StreamCodec::H264, frame.width, frame.height, fps);
        config.input = frame.format;
        config.bitrate_bps = u64::from(layer.bitrate(source));
        config.keyframe_interval = fps.saturating_mul(2);
        config.screen_content = source == Source::Screen;
        config.output_size = Some((width, height));
        let encoder = RealtimeEncoder::new(&config).map_err(|error| error.to_string())?;
        Ok(Self {
            encoder,
            input: (frame.width, frame.height),
            output: (
                u16::try_from(width).map_err(|_| "picture too wide")?,
                u16::try_from(height).map_err(|_| "picture too tall")?,
            ),
            sequence: 0,
        })
    }
}

/// The encoder thread of one source: encode each frame for every layer
/// someone shows, on whichever carrier the call has now, and for this
/// person's own tile.
#[allow(
    clippy::too_many_lines,
    reason = "one frame's path: carrier, wanted layers, encoders, then each unit's destinations"
)]
fn encode(source: Source, fps: u32, feed: &Feed, current: &CurrentClient, preview: &Preview) {
    let started = Instant::now();
    let mut encoders: BTreeMap<Layer, LayerEncoder> = BTreeMap::new();
    let mut carrier: Option<usize> = None;
    let mut refused: BTreeSet<Layer> = BTreeSet::new();
    loop {
        let (frame, repeat) = match feed.next() {
            Next::Stopped => return,
            Next::Fresh(frame) => (frame, false),
            Next::Repeat(frame) => (frame, true),
        };
        let client = current();
        // A new carrier starts every layer over with a keyframe.
        let id = client.as_ref().map(|client| Arc::as_ptr(client) as usize);
        if carrier != id {
            encoders.clear();
            refused.clear();
            carrier = id;
        }
        let remote: BTreeSet<Layer> = client
            .as_ref()
            .map(|client| {
                client
                    .wanted_pictures()
                    .into_iter()
                    .filter(|(wanted, _)| *wanted == source)
                    .map(|(_, layer)| layer)
                    .collect()
            })
            .unwrap_or_default();
        let own = preview.layer();
        let mut wanted = remote.clone();
        wanted.extend(own);
        encoders.retain(|layer, encoder| {
            wanted.contains(layer) && encoder.input == (frame.width, frame.height)
        });
        refused.retain(|layer| wanted.contains(layer));
        for layer in &wanted {
            if encoders.contains_key(layer) || refused.contains(layer) {
                continue;
            }
            match LayerEncoder::new(source, *layer, &frame, fps) {
                Ok(encoder) => {
                    tracing::info!(
                        ?source,
                        ?layer,
                        element = encoder.encoder.element().element,
                        "call picture layer encoding"
                    );
                    encoders.insert(*layer, encoder);
                }
                Err(error) => {
                    tracing::warn!(?source, ?layer, %error, "call picture layer has no encoder");
                    refused.insert(*layer);
                }
            }
        }
        let at = if repeat {
            Instant::now()
        } else {
            frame.captured_at
        };
        let pts = at.saturating_duration_since(started);
        let timestamp_us = u64::try_from(pts.as_micros()).unwrap_or(u64::MAX);
        let mut unwanted = Vec::new();
        for (layer, encoder) in &mut encoders {
            if repeat {
                encoder.encoder.force_keyframe();
            }
            let units = match encoder
                .encoder
                .encode(InputFrame::Packed(&frame.pixels), pts)
            {
                Ok(units) => units,
                Err(error) => {
                    tracing::debug!(?source, ?layer, %error, "call picture encode failed");
                    unwanted.push(*layer);
                    continue;
                }
            };
            for unit in units {
                let object = VideoObject {
                    keyframe: unit.keyframe,
                    codec: Codec::H264,
                    layer: *layer,
                    sequence: encoder.sequence,
                    timestamp_us,
                    width: encoder.output.0,
                    height: encoder.output.1,
                    payload: unit.data,
                };
                encoder.sequence = encoder.sequence.saturating_add(1);
                if own == Some(*layer) {
                    preview.push(object.clone());
                }
                let Some(client) = client.as_ref().filter(|_| remote.contains(layer)) else {
                    continue;
                };
                match client.send_picture(source, &object) {
                    Ok(()) => {}
                    // Nobody shows this layer any more.
                    Err(ClientError::Muted) if own != Some(*layer) => unwanted.push(*layer),
                    Err(ClientError::Muted) => {}
                    // The layer starts (again) at a keyframe.
                    Err(ClientError::Expired | ClientError::Capacity) => {
                        encoder.encoder.force_keyframe();
                    }
                    Err(error) => {
                        tracing::debug!(?source, ?layer, ?error, "call picture not sent");
                    }
                }
            }
        }
        for layer in unwanted {
            encoders.remove(&layer);
        }
    }
}

/// The wire names of a picture's source and layer.
pub(crate) const fn source_of(source: PictureSource) -> Source {
    match source {
        PictureSource::Camera => Source::Camera,
        PictureSource::Screen => Source::Screen,
    }
}
pub(crate) const fn picture_source(source: Source) -> PictureSource {
    match source {
        Source::Camera => PictureSource::Camera,
        Source::Screen => PictureSource::Screen,
    }
}
pub(crate) const fn layer_of(layer: PictureLayer) -> Layer {
    match layer {
        PictureLayer::Low => Layer::Low,
        PictureLayer::Mid => Layer::Mid,
        PictureLayer::High => Layer::High,
    }
}

/// Received pictures waiting for the app; see the [module docs](self).
pub(crate) struct Inbox {
    queue: VecDeque<Queued>,
    next: u64,
    bytes: usize,
    /// Streams that dropped a picture: nothing of them until a keyframe.
    behind: BTreeSet<(String, PictureSource)>,
    signal: Arc<tokio::sync::Notify>,
}
struct Queued {
    cursor: u64,
    picture: Picture,
}
impl Default for Inbox {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            next: 0,
            bytes: 0,
            behind: BTreeSet::new(),
            signal: Arc::new(tokio::sync::Notify::new()),
        }
    }
}
impl Inbox {
    /// Woken whenever a picture arrives.
    pub(crate) fn signal(&self) -> Arc<tokio::sync::Notify> {
        self.signal.clone()
    }
    /// Queue `picture`, a keyframe or not.
    pub(crate) fn admit(&mut self, picture: Picture, keyframe: bool) {
        let key = (picture.stream.clone(), picture.source);
        if keyframe {
            self.behind.remove(&key);
        } else if self.behind.contains(&key) {
            return;
        }
        let size = picture.object.len();
        while self.bytes.saturating_add(size) > INBOX_BYTES {
            let Some(oldest) = self.queue.front() else {
                break;
            };
            // The oldest stream starts over: none of its queued pictures
            // decode without the one dropped.
            let stream = (oldest.picture.stream.clone(), oldest.picture.source);
            self.drop_stream(&stream);
            self.behind.insert(stream.clone());
            if stream == key {
                return;
            }
        }
        self.next = self.next.saturating_add(1);
        self.bytes = self.bytes.saturating_add(size);
        self.queue.push_back(Queued {
            cursor: self.next,
            picture,
        });
        self.signal.notify_waiters();
    }
    fn drop_stream(&mut self, stream: &(String, PictureSource)) {
        let mut bytes = self.bytes;
        self.queue.retain(|queued| {
            let keep = (&queued.picture.stream, queued.picture.source) != (&stream.0, stream.1);
            if !keep {
                bytes = bytes.saturating_sub(queued.picture.object.len());
            }
            keep
        });
        self.bytes = bytes;
    }
    /// Forget what the app has collected (up to `after`), and hand it the
    /// next pictures, oldest first, up to `budget` bytes but at least one.
    /// The cursor returned is where the next collection continues.
    pub(crate) fn take(&mut self, after: u64, budget: usize) -> (Vec<Picture>, u64) {
        while self
            .queue
            .front()
            .is_some_and(|queued| queued.cursor <= after)
        {
            if let Some(queued) = self.queue.pop_front() {
                self.bytes = self.bytes.saturating_sub(queued.picture.object.len());
            }
        }
        let mut taken = Vec::new();
        let mut cursor = after;
        let mut bytes = 0_usize;
        for queued in &self.queue {
            let size = queued.picture.object.len();
            if !taken.is_empty() && bytes.saturating_add(size) > budget {
                break;
            }
            bytes = bytes.saturating_add(size);
            cursor = queued.cursor;
            taken.push(queued.picture.clone());
        }
        (taken, cursor)
    }
    /// Some of `stream`'s pictures were dropped before they got here:
    /// nothing of it until its next keyframe.
    pub(crate) fn behind(&mut self, stream: &str, source: PictureSource) {
        let key = (stream.to_owned(), source);
        self.drop_stream(&key);
        self.behind.insert(key);
    }
    /// The call ended or reconnected: every stream starts over.
    pub(crate) fn clear(&mut self) {
        self.queue.clear();
        self.bytes = 0;
        self.behind.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(stream: &str, size: usize) -> Picture {
        Picture {
            person: "ab".repeat(32),
            stream: stream.into(),
            source: PictureSource::Camera,
            object: vec![1; size],
        }
    }

    #[test]
    fn a_layer_keeps_its_sources_shape_within_its_box_and_never_grows_it() {
        assert_eq!(fitted((1280, 720), (320, 180)), (320, 180));
        assert_eq!(fitted((1920, 1200), (1920, 1080)), (1728, 1080));
        assert_eq!(fitted((2560, 1080), (640, 360)), (640, 270));
        assert_eq!(fitted((800, 600), (1920, 1080)), (800, 600));
        assert_eq!(fitted((1366, 768), (320, 180)), (320, 178));
    }

    #[test]
    fn the_app_collects_pictures_in_order_and_acknowledges_with_its_cursor() {
        let mut inbox = Inbox::default();
        for _ in 0..3 {
            inbox.admit(picture("a", 10), true);
        }
        let (first, cursor) = inbox.take(0, 15);
        assert_eq!(
            first.len(),
            1,
            "a budget smaller than two pictures takes one"
        );
        let (again, same) = inbox.take(0, 15);
        assert_eq!(
            (again.len(), same),
            (1, cursor),
            "a lost answer is answered again"
        );
        let (rest, end) = inbox.take(cursor, 1024);
        assert_eq!(rest.len(), 2);
        assert_eq!(inbox.take(end, 1024).0.len(), 0);
    }

    #[test]
    fn a_stream_that_falls_behind_starts_over_at_its_next_keyframe() {
        let mut inbox = Inbox::default();
        let big = INBOX_BYTES / 4;
        inbox.admit(picture("slow", big), true);
        inbox.admit(picture("slow", big), false);
        inbox.admit(picture("fast", big), true);
        inbox.admit(picture("fast", big), false);
        // One more: the oldest stream gives way whole.
        inbox.admit(picture("fast", big), false);
        let (queued, cursor) = inbox.take(0, usize::MAX);
        assert!(queued.iter().all(|picture| picture.stream == "fast"));
        inbox.take(cursor, usize::MAX);
        inbox.admit(picture("slow", 10), false);
        assert!(
            inbox.take(cursor, usize::MAX).0.is_empty(),
            "not without its keyframe"
        );
        inbox.admit(picture("slow", 10), true);
        inbox.admit(picture("slow", 10), false);
        assert_eq!(inbox.take(cursor, usize::MAX).0.len(), 2);
    }

    #[test]
    fn an_odd_screen_is_cropped_to_even_rows_and_columns() {
        let rgba: Arc<[u8]> = Arc::from(vec![9; 5 * 3 * 4]);
        let frame = even_rgba(5, 3, rgba).unwrap();
        assert_eq!((frame.width, frame.height), (4, 2));
        assert_eq!(frame.pixels.len(), 4 * 2 * 4);
        assert!(even_rgba(1, 1, Arc::from(vec![0; 4])).is_none());
        assert!(even_rgba(4, 2, Arc::from(vec![0; 3])).is_none());
    }
}
