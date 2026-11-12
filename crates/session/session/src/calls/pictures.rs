//! A call's pictures on this device: the camera and screen it encodes once
//! per layer a device in the call shows, and the pictures that arrive for its
//! app to draw.
//!
//! **Sending.** A source has one [`Sending`]: its capture publishes the
//! newest frame into a [`Feed`], and a thread of its own encodes each frame
//! for every layer someone shows, as the [`Sink`] it was started with says
//! ([`super::plane_pictures`] publishes them). A layer nobody shows is not
//! encoded, and a layer that starts being shown starts at a keyframe: the
//! standing one while the picture has not changed since, so a still screen
//! costs no encode, or one encoded from the last frame at once.
//!
//! **Receiving.** The workspace's live plane seals a picture per layer and
//! opens the ones the app shows; each waits in the call's [`Inbox`] until the
//! app collects it, and a stream that falls behind starts over at its next
//! keyframe, so the app's decoder never sees a picture whose reference was
//! dropped. A page's pictures arrive the same way, over its own plane.
//!
//! Nothing here waits on a device: the capture thread only ever replaces the
//! newest frame, and a picture is published the moment it is encoded.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Instant;

use media::realtime::{EncoderConfig, InputFormat, InputFrame, RealtimeEncoder, StreamCodec};
use tilo_live::video::{self, Codec};
use tilo_lock::MutexRecover as _;

pub use tilo_live::video::{Layer, Source, VideoObject};

/// Camera frames a second.
pub const CAMERA_FPS: u32 = 30;
/// The camera's capture size: the largest camera layer.
pub const CAMERA_SIZE: (u32, u32) = (1280, 720);
/// Screen pictures a second.
pub const SCREEN_FPS: u32 = 15;
/// Bytes of received pictures held for the app: a few seconds of every shown layer. Past it the
/// oldest stream starts over at a keyframe.
pub const INBOX_BYTES: usize = 8 * 1024 * 1024;

/// One frame to encode.
#[derive(Clone)]
pub struct Frame {
    pub pixels: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    /// [`InputFormat::Nv12`] (the camera) or [`InputFormat::Rgba`] (the screen), tightly packed.
    pub format: InputFormat,
    pub captured_at: Instant,
}

/// The newest frame of one source, handed from its capture to its encoder.
#[derive(Default)]
pub struct Feed {
    state: Mutex<FeedState>,
    ready: Condvar,
}

#[derive(Default)]
struct FeedState {
    frame: Option<Frame>,
    fresh: bool,
    /// A new reader asks for a keyframe of the last frame.
    asked: bool,
    stopped: bool,
}

enum Next {
    Fresh(Frame),
    /// No new frame, and a new reader asks for a keyframe: the last frame.
    Asked(Frame),
    Stopped,
}

impl Feed {
    /// Hand `frame` to the encoder as the newest one.
    pub fn publish(&self, frame: Frame) {
        let mut state = self.state.lock_recover();
        state.frame = Some(frame);
        state.fresh = true;
        drop(state);
        self.ready.notify_one();
    }

    /// Forget the last frame: nothing is sent until the next one. What a locked screen shows is
    /// never shared, and its last picture is not repeated.
    pub fn clear(&self) {
        let mut state = self.state.lock_recover();
        state.frame = None;
        state.fresh = false;
    }

    /// A layer gained a reader: the encoder answers with a keyframe even while no new frame comes.
    pub fn ask(&self) {
        self.state.lock_recover().asked = true;
        self.ready.notify_one();
    }

    fn stop(&self) {
        self.state.lock_recover().stopped = true;
        self.ready.notify_one();
    }

    fn next(&self) -> Next {
        let mut state = self.state.lock_recover();
        loop {
            if state.stopped {
                return Next::Stopped;
            }
            if let Some(frame) = state.frame.clone() {
                if state.fresh {
                    state.fresh = false;
                    state.asked = false;
                    return Next::Fresh(frame);
                }
                if state.asked {
                    state.asked = false;
                    return Next::Asked(frame);
                }
            }
            state = match self.ready.wait(state) {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }
}

/// `rgba` cropped to even dimensions, which 4:2:0 encoders need.
#[must_use]
pub fn even_rgba(width: u32, height: u32, rgba: Arc<[u8]>) -> Option<Frame> {
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

/// The largest size within `layer` that keeps `source` 's shape, even, and never larger than the
/// source itself.
#[must_use]
pub fn fitted((width, height): (u32, u32), (box_width, box_height): (u16, u16)) -> (u32, u32) {
    let (box_width, box_height) = (u32::from(box_width), u32::from(box_height));
    let (w, h) = if width <= box_width && height <= box_height {
        (width, height)
    } else if u64::from(width).saturating_mul(u64::from(box_height))
        >= u64::from(height).saturating_mul(u64::from(box_width))
    {
        let scaled = u64::from(height)
            .saturating_mul(u64::from(box_width))
            .checked_div(u64::from(width.max(1)))
            .unwrap_or(0);
        (box_width, u32::try_from(scaled).unwrap_or(box_height))
    } else {
        let scaled = u64::from(width)
            .saturating_mul(u64::from(box_height))
            .checked_div(u64::from(height.max(1)))
            .unwrap_or(0);
        (u32::try_from(scaled).unwrap_or(box_width), box_height)
    };
    ((w & !1).max(2), (h & !1).max(2))
}

/// What this device must encode now: the layers the plane's devices show of each source, the
/// sources to start over at a keyframe, and this person's own camera tile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Asking {
    /// The layers someone in the call shows of each source.
    pub wanted: BTreeMap<Source, BTreeSet<Layer>>,
    /// Sources to start over at a keyframe, as a device whose stream fell behind asks.
    pub restart: BTreeSet<Source>,
    /// This device's own camera, shown here, for this person's own tile.
    pub own: Option<Layer>,
}

/// What encodes a device's pictures: read by the encoder thread of each source, written by the
/// call.
pub trait Sink: Send + Sync {
    /// What this device must encode of `source` now.
    fn asking(&self, source: Source) -> Asking;
    /// One encoded picture, to be published on the plane.
    fn sent(&self, source: Source, object: &VideoObject);
    /// This person's own picture, for their own tile: encoded here, never sent.
    fn own(&self, object: &VideoObject);
}

/// Start an encoder for each shown layer that has none yet, and end the ones nobody shows any more
/// or whose picture changed shape.
fn start_encoders(
    source: Source,
    fps: u32,
    shown: &BTreeSet<Layer>,
    frame: &Frame,
    encoders: &mut BTreeMap<Layer, LayerEncoder>,
) {
    encoders.retain(|layer, encoder| {
        shown.contains(layer) && encoder.input == (frame.width, frame.height)
    });
    for layer in shown {
        if encoders.contains_key(layer) {
            continue;
        }
        match LayerEncoder::new(source, *layer, frame, fps) {
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
            }
        }
    }
}

/// One layer's encoder.
struct LayerEncoder {
    encoder: RealtimeEncoder,
    /// The frame size it takes.
    input: (u32, u32),
    /// The picture size it sends.
    output: (u16, u16),
    sequence: u64,
    /// The newest unit while it is a keyframe: a new reader starts at it.
    standing: Option<VideoObject>,
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
            standing: None,
        })
    }
}

/// One source this device publishes: its feed, and the thread encoding it for the layers someone
/// shows.
/// Dropping it ends the encoder thread at its next frame wait.
pub struct Sending {
    feed: Arc<Feed>,
}

impl Sending {
    /// Start encoding `source` for whatever `sink` says is shown, at most
    /// `fps` pictures a second.
    ///
    /// # Errors
    /// The encoder thread could not be started.
    pub fn start(source: Source, fps: u32, sink: &Arc<dyn Sink>) -> Result<Self, String> {
        let feed = Arc::new(Feed::default());
        let encoding = Arc::clone(&feed);
        let sink: Weak<dyn Sink> = Arc::downgrade(sink);
        std::thread::Builder::new()
            .name(match source {
                Source::Camera => "call-camera".to_owned(),
                Source::Screen => "call-screen".to_owned(),
            })
            .spawn(move || encode(source, fps, &encoding, &sink))
            .map_err(|error| error.to_string())?;
        Ok(Self { feed })
    }

    /// Hand the encoder the newest captured frame.
    pub fn publish(&self, frame: Frame) {
        self.feed.publish(frame);
    }

    /// Forget the last frame: nothing is sent until the next one.
    pub fn clear(&self) {
        self.feed.clear();
    }

    /// A layer gained a reader: answer with a keyframe.
    pub fn ask_keyframe(&self) {
        self.feed.ask();
    }
}

impl Drop for Sending {
    fn drop(&mut self) {
        self.feed.stop();
    }
}

/// The encoder thread of one source: encode each frame for every layer someone shows, and hand each
/// picture to the sink.
fn encode(source: Source, fps: u32, feed: &Feed, sink: &Weak<dyn Sink>) {
    let started = Instant::now();
    let mut encoders: BTreeMap<Layer, LayerEncoder> = BTreeMap::new();
    let mut wanted: BTreeSet<Layer> = BTreeSet::new();
    loop {
        let (frame, fresh) = match feed.next() {
            Next::Stopped => return,
            Next::Fresh(frame) => (frame, true),
            Next::Asked(frame) => (frame, false),
        };
        let Some(sink) = sink.upgrade() else {
            return;
        };
        let asking = sink.asking(source);
        let shown = asking.wanted.get(&source).cloned().unwrap_or_default();
        let own = match (source, asking.own) {
            (Source::Camera, own) => own,
            (Source::Screen, _) => None,
        };
        // Taken before the layers are read, so a layer that just gained a reader is encoded as a
        // keyframe in this pass.
        let mut asked: BTreeSet<Layer> = shown.difference(&wanted).copied().collect();
        if asking.restart.contains(&source) {
            asked.extend(shown.iter().copied());
        }
        wanted.clone_from(&shown);
        start_encoders(source, fps, &shown, &frame, &mut encoders);
        let at = if fresh {
            frame.captured_at
        } else {
            Instant::now()
        };
        let pts = at.saturating_duration_since(started);
        let timestamp_us = u64::try_from(pts.as_micros()).unwrap_or(u64::MAX);
        let mut unwanted = Vec::new();
        for (layer, encoder) in &mut encoders {
            let shown_here = shown.contains(layer);
            let own_here = own == Some(*layer);
            if !shown_here && !own_here {
                continue;
            }
            if !fresh {
                if !asked.contains(layer) {
                    continue;
                }
                if let Some(standing) = &encoder.standing {
                    if shown_here {
                        sink.sent(source, standing);
                    }
                    if own_here {
                        sink.own(standing);
                    }
                    continue;
                }
            }
            if asked.contains(layer) {
                encoder.encoder.force_keyframe();
            }
            let units = match encoder
                .encoder
                .encode(InputFrame::Packed(&frame.pixels), pts)
            {
                Ok(units) => units,
                Err(error) => {
                    tracing::warn!(
                        ?source,
                        ?layer,
                        %error,
                        "a call picture failed to encode: its layer stops"
                    );
                    unwanted.push(*layer);
                    continue;
                }
            };
            for unit in units {
                let object = VideoObject {
                    keyframe: unit.keyframe,
                    hdr: false,
                    codec: Codec::H264,
                    layer: *layer,
                    sequence: encoder.sequence,
                    timestamp_us,
                    width: encoder.output.0,
                    height: encoder.output.1,
                    payload: unit.data,
                };
                encoder.sequence = encoder.sequence.saturating_add(1);
                encoder.standing = object.keyframe.then(|| object.clone());
                if shown_here {
                    sink.sent(source, &object);
                }
                if own_here {
                    sink.own(&object);
                }
            }
        }
        for layer in unwanted {
            encoders.remove(&layer);
        }
    }
}

/// One received picture: an encoded tilo-video/1 object of one device's source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// The device that sent it: the plane names it, and the keys prove it.
    pub from: [u8; 32],
    pub source: Source,
    pub keyframe: bool,
    /// The tilo-video/1 object, header and access unit.
    pub object: Vec<u8>,
}

/// One stream of pictures: a device's source.
pub type PictureStream = ([u8; 32], Source);

/// Received pictures waiting for the app.
#[derive(Default)]
pub struct Inbox {
    queue: VecDeque<Queued>,
    next: u64,
    bytes: usize,
    /// Streams that dropped a picture: nothing of them until a keyframe.
    behind: BTreeSet<PictureStream>,
    signal: Arc<tokio::sync::Notify>,
}

struct Queued {
    cursor: u64,
    picture: Picture,
}

impl Inbox {
    /// Woken whenever a picture arrives.
    #[must_use]
    pub fn signal(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.signal)
    }

    /// Queue `picture` for the app. A stream dropped to make room is left behind: it starts over at
    /// its next keyframe.
    pub fn admit(&mut self, picture: Picture) {
        let key = (picture.from, picture.source);
        if picture.keyframe {
            self.behind.remove(&key);
        } else if self.behind.contains(&key) {
            return;
        }
        let size = picture.object.len();
        while self.bytes.saturating_add(size) > INBOX_BYTES {
            let Some(oldest) = self.queue.front() else {
                break;
            };
            // The oldest stream starts over: none of its queued pictures decode without the one
            // dropped.
            let stream = (oldest.picture.from, oldest.picture.source);
            self.drop_stream(stream);
            self.behind.insert(stream);
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

    fn drop_stream(&mut self, stream: PictureStream) {
        let mut bytes = self.bytes;
        self.queue.retain(|queued| {
            let keep = (queued.picture.from, queued.picture.source) != stream;
            if !keep {
                bytes = bytes.saturating_sub(queued.picture.object.len());
            }
            keep
        });
        self.bytes = bytes;
    }

    /// Forget what the app has collected (up to `after` ), and hand it the next pictures, oldest
    /// first, up to `budget` bytes but at least one. The cursor returned is where the next
    /// collection continues.
    pub fn take(&mut self, after: u64, budget: usize) -> (Vec<Picture>, u64) {
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

    /// Some of `stream` 's pictures were dropped before they got here: nothing of it until its next
    /// keyframe.
    pub fn behind(&mut self, from: [u8; 32], source: Source) {
        let key = (from, source);
        self.drop_stream(key);
        self.behind.insert(key);
    }

    /// The call ended: every stream starts over.
    pub fn clear(&mut self) {
        self.queue.clear();
        self.bytes = 0;
        self.behind.clear();
    }
}

/// This call's pictures: the inbox the app collects, and this person's own camera picture for their
/// own tile.
pub struct Video {
    inner: Arc<Inner>,
}

struct Inner {
    own: [u8; 32],
    inbox: Mutex<Inbox>,
    /// The layer this person's own camera tile shows.
    own_layer: Mutex<Option<Layer>>,
}

impl Video {
    /// The pictures of this device's call: `own` is this device's key, which its own tile's
    /// pictures name it by.
    #[must_use]
    pub fn new(own: [u8; 32]) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(Inner {
                own,
                inbox: Mutex::new(Inbox::default()),
                own_layer: Mutex::new(None),
            }),
        })
    }

    /// This device's own camera tile shows `layer` , or nothing.
    pub fn own_tile(&self, layer: Option<Layer>) {
        *self.inner.own_layer.lock_recover() = layer;
    }

    /// The layer this person's own camera shows here.
    #[must_use]
    pub fn own_layer(&self) -> Option<Layer> {
        *self.inner.own_layer.lock_recover()
    }

    /// This person's own picture, for their own tile: kept here, never sent.
    pub fn own_picture(&self, object: &VideoObject) {
        if self.own_layer() != Some(object.layer) {
            return;
        }
        let Ok(bytes) = object.encode() else {
            return;
        };
        self.inner.inbox.lock_recover().admit(Picture {
            from: self.inner.own,
            source: Source::Camera,
            keyframe: object.keyframe,
            object: bytes,
        });
    }

    /// A stream lost a picture before the app saw it: nothing of it is drawn until its next
    /// keyframe.
    pub fn behind(&self, from: [u8; 32], source: Source) {
        self.inner.inbox.lock_recover().behind(from, source);
    }

    /// One picture of `source` a device sent over the plane: queued for the app.
    pub fn heard(&self, from: [u8; 32], source: Source, body: Vec<u8>) {
        let Ok(header) = video::validate(&body) else {
            return;
        };
        self.inner.inbox.lock_recover().admit(Picture {
            from,
            source,
            keyframe: header.keyframe,
            object: body,
        });
    }

    /// The pictures the app has not collected, oldest first, up to `budget` bytes.
    pub fn take(&self, after: u64, budget: usize) -> (Vec<Picture>, u64) {
        self.inner.inbox.lock_recover().take(after, budget)
    }

    /// Woken whenever a picture arrives.
    #[must_use]
    pub fn signal(&self) -> Arc<tokio::sync::Notify> {
        self.inner.inbox.lock_recover().signal()
    }

    /// This call ended: nothing more is drawn.
    pub fn close(&self) {
        self.inner.inbox.lock_recover().clear();
    }
}

/// The camera a call's pictures come from: the device's own, or the moving test picture for a
/// device without one (a virtual machine, a nested development session).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Camera {
    /// The device's own camera.
    #[default]
    Device,
    /// The moving test picture: `TILO_CALL_CAMERA=test-pattern` .
    TestPattern,
}

impl Camera {
    /// The camera the environment asks for.
    #[must_use]
    pub fn from_env() -> Self {
        if std::env::var("TILO_CALL_CAMERA").is_ok_and(|value| value == "test-pattern") {
            Self::TestPattern
        } else {
            Self::Device
        }
    }

    /// What this camera captures from, or `None` when the device has none.
    #[must_use]
    pub fn device(self) -> Option<media::realtime::capture::CaptureDevice> {
        use media::realtime::capture::CaptureDevice;
        match self {
            Self::Device => {
                media::realtime::capture::camera_node().map(|node| CaptureDevice::Camera { node })
            }
            Self::TestPattern => Some(CaptureDevice::TestPattern),
        }
    }
}
