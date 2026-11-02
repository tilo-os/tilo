//! A call's pictures: the camera and screen one device sends, encoded once
//! per layer a device in the call shows, and the pictures that arrive from
//! the others.
//!
//! **Sending.** A source has one [`Sending`]: its capture publishes the
//! newest frame into a [`Feed`], and a thread of its own encodes each frame
//! for every layer someone shows — [`PicturePeer`] carries what each device
//! said it shows, on the lane's control stream. A layer nobody shows is not
//! encoded. A layer that starts being shown starts at a keyframe: the
//! standing one while the picture has not changed since, so a still screen
//! costs no encode, or one encoded from the last frame at once. Each
//! tilo-video/1 object ([`tilo_moq::video`]) is sealed under the stream's
//! media key ([`crate::frame`]) and written to a unidirectional stream per
//! source and layer, which starts at a keyframe.
//!
//! **Receiving.** Each picture stream of a sending device arrives on a
//! unidirectional stream of the lane's connection, is opened with the
//! channel's keys and waits in the call's [`Inbox`] until the app collects
//! it; a stream that falls behind starts over at its next keyframe, so the
//! app's decoder never sees a picture whose reference was dropped.
//!
//! Nothing here waits on a device: the capture thread only ever replaces the
//! newest frame, and the lane's writer drops a picture rather than queueing
//! it past the one before.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use channel_crypto::ChannelContentKeys;
use iroh::endpoint::{ReadExactError, RecvStream, SendStream};
use media::realtime::{EncoderConfig, InputFormat, InputFrame, RealtimeEncoder, StreamCodec};
use tilo_lock::MutexRecover as _;
use tilo_moq::video::{self, Codec, VideoObject};
pub use tilo_moq::video::{Layer, Source};
use tokio::sync::mpsc;

use crate::carrier::Peer;
use crate::frame::{OVERHEAD, Opener, Sealer, Stream};
use crate::roster::Roster;

/// Camera frames a second.
pub const CAMERA_FPS: u32 = 30;
/// The camera's capture size: the largest camera layer.
pub const CAMERA_SIZE: (u32, u32) = (1280, 720);
/// Screen pictures a second.
pub const SCREEN_FPS: u32 = 15;
/// The time between two screen pictures.
pub const SCREEN_INTERVAL: Duration = Duration::from_millis(1000 / SCREEN_FPS as u64);
/// Bytes of received pictures held for the app: a few seconds of every
/// shown layer. Past it the oldest stream starts over at a keyframe.
pub const INBOX_BYTES: usize = 8 * 1024 * 1024;
/// Pictures waiting in the lane's writer: a picture that cannot go out now
/// is dropped rather than held behind the one before it.
const WRITE_QUEUE: usize = 32;
/// How many picture streams one lane carries at once: the layers of two
/// sources, with room for the ones that start over.
pub const MAX_PICTURE_STREAMS: u64 = 64;
/// A control message is a few bytes and a handful of layers; anything larger
/// is not one.
const MAX_CONTROL_BYTES: usize = 512;
/// The largest frame a picture stream carries: the largest object, sealed.
const MAX_FRAME_BYTES: usize = video::MAX_OBJECT_BYTES.saturating_add(OVERHEAD);

/// One frame to encode.
#[derive(Clone)]
pub struct Frame {
    pub pixels: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    /// [`InputFormat::Nv12`] (the camera) or [`InputFormat::Rgba`] (the
    /// screen), tightly packed.
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

    /// Forget the last frame: nothing is sent until the next one. What a
    /// locked screen shows is never shared, and its last picture is not
    /// repeated.
    pub fn clear(&self) {
        let mut state = self.state.lock_recover();
        state.frame = None;
        state.fresh = false;
    }

    /// A layer gained a reader: the encoder answers with a keyframe even
    /// while no new frame comes.
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

/// The largest size within `layer` that keeps `source`'s shape, even, and
/// never larger than the source itself.
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

/// The byte a control message names a source with.
#[must_use]
pub const fn source_wire(source: Source) -> u8 {
    match source {
        Source::Camera => 0,
        Source::Screen => 1,
    }
}

/// The source a control message names, or `None` for any other byte.
#[must_use]
pub const fn source_of_wire(byte: u8) -> Option<Source> {
    match byte {
        0 => Some(Source::Camera),
        1 => Some(Source::Screen),
        _ => None,
    }
}

/// The byte a control message names a layer with.
#[must_use]
pub const fn layer_wire(layer: Layer) -> u8 {
    match layer {
        Layer::Low => 0,
        Layer::Mid => 1,
        Layer::High => 2,
    }
}

/// The layer a control message names, or `None` for any other byte.
#[must_use]
pub const fn layer_of_wire(byte: u8) -> Option<Layer> {
    match byte {
        0 => Some(Layer::Low),
        1 => Some(Layer::Mid),
        2 => Some(Layer::High),
        _ => None,
    }
}

/// The stream of a call's frames a picture source travels on.
#[must_use]
pub const fn stream_of(source: Source) -> Stream {
    match source {
        Source::Camera => Stream::Camera,
        Source::Screen => Stream::Screen,
    }
}

/// A picture stream's share of the connection: the larger layer first, so a
/// grid never waits behind a stage.
const fn priority(layer: Layer) -> i32 {
    match layer {
        Layer::Low => 0,
        Layer::Mid => 1,
        Layer::High => 2,
    }
}

/// What this device must encode now, as the lanes of its call said.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Asking {
    /// The layers someone in the call shows of each source.
    pub wanted: BTreeMap<Source, BTreeSet<Layer>>,
    /// Sources to start over at a keyframe, as a device whose stream fell
    /// behind asks.
    pub restart: BTreeSet<Source>,
    /// This device's own camera, shown here, for this person's own tile.
    pub own: Option<Layer>,
}

/// What encodes a device's pictures: read by the encoder thread of each
/// source, written by the call.
pub trait Sink: Send + Sync {
    /// What this device must encode of `source` now.
    fn asking(&self, source: Source) -> Asking;
    /// One encoded picture, to be sealed and sent to the devices that show
    /// its layer.
    fn sent(&self, source: Source, object: &VideoObject);
    /// This person's own picture, for their own tile: encoded here, never
    /// sent.
    fn own(&self, object: &VideoObject);
}

/// Start an encoder for each shown layer that has none yet, and end the ones
/// nobody shows any more or whose picture changed shape.
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

/// One source this device publishes: its feed, and the thread encoding it
/// for the layers someone shows.
///
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

/// The encoder thread of one source: encode each frame for every layer
/// someone shows, and hand each picture to the sink.
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
        // Taken before the layers are read, so a layer that just gained a
        // reader is encoded as a keyframe in this pass.
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

/// One received picture: an encoded tilo-video/1 object of one device's
/// source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// The device that sent it: the lane's connection names it, and the keys
    /// prove it.
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

    /// Queue `picture` for the app. The streams whose queue was dropped to
    /// make room are returned: each starts over at its next keyframe, and
    /// its sending device is asked for one.
    pub fn admit(&mut self, picture: Picture) -> Vec<PictureStream> {
        let key = (picture.from, picture.source);
        if picture.keyframe {
            self.behind.remove(&key);
        } else if self.behind.contains(&key) {
            return Vec::new();
        }
        let size = picture.object.len();
        let mut dropped = Vec::new();
        while self.bytes.saturating_add(size) > INBOX_BYTES {
            let Some(oldest) = self.queue.front() else {
                break;
            };
            // The oldest stream starts over: none of its queued pictures
            // decode without the one dropped.
            let stream = (oldest.picture.from, oldest.picture.source);
            self.drop_stream(stream);
            self.behind.insert(stream);
            dropped.push(stream);
            if stream == key {
                return dropped;
            }
        }
        self.next = self.next.saturating_add(1);
        self.bytes = self.bytes.saturating_add(size);
        self.queue.push_back(Queued {
            cursor: self.next,
            picture,
        });
        self.signal.notify_waiters();
        dropped
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

    /// Forget what the app has collected (up to `after`), and hand it the
    /// next pictures, oldest first, up to `budget` bytes but at least one.
    /// The cursor returned is where the next collection continues.
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

    /// Some of `stream`'s pictures were dropped before they got here:
    /// nothing of it until its next keyframe.
    pub fn behind(&mut self, from: [u8; 32], source: Source) {
        let key = (from, source);
        self.drop_stream(key);
        self.behind.insert(key);
    }

    /// The call ended or reconnected: every stream starts over.
    pub fn clear(&mut self) {
        self.queue.clear();
        self.bytes = 0;
        self.behind.clear();
    }
}

/// One picture on its way out: sealed, with the layer it belongs to.
struct Outgoing {
    source: Source,
    layer: Layer,
    keyframe: bool,
    sealed: Vec<u8>,
}

/// One device in the call: what it shows, and the picture streams this
/// device sends it.
#[derive(Clone)]
pub struct PicturePeer {
    peer: Peer,
    link: Arc<Link>,
}

struct Link {
    /// What the device says it shows, as its control stream last said.
    showing: Mutex<BTreeMap<Source, BTreeSet<Layer>>>,
    /// The picture stream of each source and layer this device sends it.
    sending: tokio::sync::Mutex<BTreeMap<(Source, Layer), SendStream>>,
    /// What this device tells the other, in the order it said it: a device
    /// whose shows arrive out of order would send a layer nobody shows, or
    /// encode nothing for one that is.
    controls: mpsc::UnboundedSender<Control>,
    /// The receiving end of `controls`, taken once by [`PicturePeer::write_controls`].
    unsent: Mutex<Option<mpsc::UnboundedReceiver<Control>>>,
}

impl std::fmt::Debug for PicturePeer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PicturePeer")
            .field("remote", &self.peer.remote())
            .finish_non_exhaustive()
    }
}

impl PicturePeer {
    /// A device of this device's call, which its pictures are sent to on the
    /// lane `peer` carries.
    #[must_use]
    pub fn new(peer: Peer) -> Self {
        let (controls, unsent) = mpsc::unbounded_channel();
        Self {
            peer,
            link: Arc::new(Link {
                showing: Mutex::new(BTreeMap::new()),
                sending: tokio::sync::Mutex::new(BTreeMap::new()),
                controls,
                unsent: Mutex::new(Some(unsent)),
            }),
        }
    }

    /// The control messages this device has to write, in the order it said
    /// them, for [`Self::write_controls`]. `None` when they are already
    /// being written.
    fn take_controls(&self) -> Option<mpsc::UnboundedReceiver<Control>> {
        self.link.unsent.lock_recover().take()
    }

    /// Write what this device says on the lane, one message after the
    /// other, until the lane ends or nothing more is said of it.
    async fn write_controls(self, mut controls: mpsc::UnboundedReceiver<Control>) {
        while let Some(message) = controls.recv().await {
            let Ok(bytes) = message.encode() else {
                continue;
            };
            let Some(mut send) = self.peer.open_control().await else {
                return;
            };
            if send.write_all(&bytes).await.is_err() {
                return;
            }
            let _ = send.finish();
        }
    }

    /// The remote device's public key.
    #[must_use]
    pub fn remote(&self) -> [u8; 32] {
        self.peer.remote()
    }

    /// The layers of `source` the remote device shows.
    #[must_use]
    pub fn layers(&self, source: Source) -> BTreeSet<Layer> {
        self.link
            .showing
            .lock_recover()
            .get(&source)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether the remote device shows `source` at `layer`.
    #[must_use]
    pub fn shows(&self, source: Source, layer: Layer) -> bool {
        self.layers(source).contains(&layer)
    }

    /// Replace what the remote device says it shows.
    fn set_showing(&self, camera: &[Layer], screen: &[Layer]) {
        let mut showing = self.link.showing.lock_recover();
        showing.clear();
        if !camera.is_empty() {
            showing.insert(Source::Camera, camera.iter().copied().collect());
        }
        if !screen.is_empty() {
            showing.insert(Source::Screen, screen.iter().copied().collect());
        }
    }

    /// Tell the remote device what this device shows of it.
    pub fn send_shows(&self, camera: &[Layer], screen: &[Layer]) {
        self.write_control(&Control::Shows {
            camera: camera.iter().copied().map(layer_wire).collect(),
            screen: screen.iter().copied().map(layer_wire).collect(),
        });
    }

    /// Ask the remote device to start `source` over at a keyframe.
    pub fn ask_keyframe(&self, source: Source) {
        self.write_control(&Control::Keyframe {
            source: source_wire(source),
        });
    }

    fn write_control(&self, message: &Control) {
        // The writer keeps the order the app said things in: a message
        // written on its own task could overtake the one before it.
        let _ = self.link.controls.send(message.clone());
    }

    /// Write one sealed picture to the remote device, on the stream of its
    /// source and layer: a keyframe starts a new one.
    pub async fn send_picture(&self, source: Source, layer: Layer, keyframe: bool, sealed: &[u8]) {
        if !self.shows(source, layer) {
            return;
        }
        let mut sending = self.link.sending.lock().await;
        let key = (source, layer);
        if keyframe {
            match self.peer.open_pictures(priority(layer)).await {
                Some(mut stream) => {
                    if stream.write_all(&[source_wire(source)]).await.is_err() {
                        return;
                    }
                    sending.insert(key, stream);
                }
                None => return,
            }
        }
        let Some(stream) = sending.get_mut(&key) else {
            // No stream of this layer is running: the device starts over at
            // the next keyframe.
            return;
        };
        let Ok(length) = u32::try_from(sealed.len()) else {
            sending.remove(&key);
            return;
        };
        if stream.write_all(&length.to_le_bytes()).await.is_err()
            || stream.write_all(sealed).await.is_err()
        {
            sending.remove(&key);
        }
    }

    /// Follow what the remote device says it shows, until the lane ends.
    async fn follow_control(self, video: Arc<Inner>) {
        let connection = self.peer.connection();
        loop {
            let Ok((_, mut recv)) = connection.clone().accept_bi().await else {
                return;
            };
            let Ok(bytes): Result<Vec<u8>, _> = recv.read_to_end(MAX_CONTROL_BYTES).await else {
                continue;
            };
            let Ok(message) = Control::decode(&bytes) else {
                continue;
            };
            match message {
                Control::Shows { camera, screen } => {
                    let layers = |codes: &[u8]| -> Vec<Layer> {
                        codes
                            .iter()
                            .filter_map(|byte| layer_of_wire(*byte))
                            .collect()
                    };
                    self.set_showing(&layers(&camera), &layers(&screen));
                    video.showed();
                }
                Control::Keyframe { source } => {
                    if let Some(source) = source_of_wire(source) {
                        video.asked(source);
                    }
                }
            }
        }
    }

    /// Read the remote device's picture streams until the lane ends: each is
    /// one source at one layer, from a keyframe to the next.
    async fn read_pictures(self, video: Arc<Inner>) {
        let connection = self.peer.connection();
        loop {
            let Ok(mut stream) = connection.clone().accept_uni().await else {
                return;
            };
            let mut source = [0_u8; 1];
            if stream.read_exact(&mut source).await.is_err() {
                continue;
            }
            let Some(source) = source_of_wire(source[0]) else {
                continue;
            };
            let peer = self.clone();
            let video = Arc::clone(&video);
            tokio::spawn(async move {
                loop {
                    match next_frame(&mut stream).await {
                        Ok(Some(sealed)) => video.arrived(&peer, source, &sealed),
                        // The stream ended between two pictures.
                        Ok(None) => return,
                        // A picture was cut off: nothing of this stream until
                        // the device starts it over.
                        Err(()) => {
                            video.behind(peer.remote(), source);
                            peer.ask_keyframe(source);
                            return;
                        }
                    }
                }
            });
        }
    }
}

/// The next picture of a stream: `Ok(None)` when it ended between two, and
/// [`Err`] when a picture was cut off.
async fn next_frame(stream: &mut RecvStream) -> Result<Option<Vec<u8>>, ()> {
    let mut length = [0_u8; 4];
    match stream.read_exact(&mut length).await {
        Ok(()) => {}
        Err(ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(_) => return Err(()),
    }
    let length = usize::try_from(u32::from_le_bytes(length)).map_err(|_| ())?;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(());
    }
    let mut sealed = vec![0_u8; length];
    stream.read_exact(&mut sealed).await.map_err(|_| ())?;
    Ok(Some(sealed))
}

/// What a device says on its lane's control stream.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Control {
    /// What this device shows of the other: the layers of its camera, and of
    /// its screen.
    Shows { camera: Vec<u8>, screen: Vec<u8> },
    /// Start this source over at a keyframe: the device's stream of it fell
    /// behind.
    Keyframe { source: u8 },
}

impl Control {
    fn encode(&self) -> Result<Vec<u8>, String> {
        postcard::to_stdvec(self).map_err(|error| format!("not a control message: {error}"))
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        postcard::from_bytes(bytes).map_err(|error| format!("not a control message: {error}"))
    }
}

/// One call's pictures, on the session's side: the sources it sends, the
/// lanes it sends them on, and what arrived from them.
pub struct Video {
    inner: Arc<Inner>,
}

struct Inner {
    own: [u8; 32],
    /// The lanes this device sends pictures on, and what each device shows.
    peers: Mutex<Vec<PicturePeer>>,
    /// What seals this device's pictures and opens the ones that arrive:
    /// `None` until it holds the channel's keys, when nothing leaves and
    /// nothing is opened.
    current: Mutex<Option<Together>>,
    /// Pictures on their way out, written by a task of their own: a picture
    /// that cannot go out now is dropped.
    outbox: mpsc::Sender<Outgoing>,
    inbox: Mutex<Inbox>,
    /// The sources this device publishes, as its capture started them.
    sources: Mutex<BTreeMap<Source, Arc<Sending>>>,
    showing: Mutex<Showing>,
}

struct Together {
    opener: Opener,
    sealing: Sealing,
}

/// What seals this device's pictures at the epoch the channel is on.
struct Sealing {
    camera: Sealer,
    screen: Sealer,
    /// The next sealed frame of each source. One counter per source: its
    /// layers are sealed under the one key.
    camera_next: AtomicU32,
    screen_next: AtomicU32,
}

impl Sealing {
    fn of(keys: &ChannelContentKeys, own: [u8; 32]) -> Option<Self> {
        Some(Self {
            camera: Sealer::new(keys, own, Stream::Camera).ok()?,
            screen: Sealer::new(keys, own, Stream::Screen).ok()?,
            camera_next: AtomicU32::new(0),
            screen_next: AtomicU32::new(0),
        })
    }

    fn sealer(&self, source: Source) -> (&Sealer, &AtomicU32) {
        match source {
            Source::Camera => (&self.camera, &self.camera_next),
            Source::Screen => (&self.screen, &self.screen_next),
        }
    }
}

/// What the app shows, and the sources a device asked to start over.
#[derive(Default)]
struct Showing {
    /// Sources a device asked to start over at a keyframe.
    restart: BTreeSet<Source>,
    /// This device's own camera tile, shown here.
    own: Option<Layer>,
    /// What this device shows of each device, to tell a lane that joins
    /// after the app said it.
    mine: BTreeMap<[u8; 32], (Vec<Layer>, Vec<Layer>)>,
}

impl std::fmt::Debug for Video {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Video")
            .field("peers", &self.inner.peers.lock_recover().len())
            .finish_non_exhaustive()
    }
}

impl Video {
    /// The pictures of this device's call: `own` is this device's key, which
    /// its sealed pictures name it by.
    ///
    /// # Panics
    /// Called outside a tokio runtime, which the lane's writer runs on.
    #[must_use]
    pub fn new(own: [u8; 32]) -> Arc<Self> {
        let (outbox, mut outgoing) = mpsc::channel::<Outgoing>(WRITE_QUEUE);
        let inner = Arc::new(Inner {
            own,
            peers: Mutex::new(Vec::new()),
            current: Mutex::new(None),
            outbox,
            inbox: Mutex::new(Inbox::default()),
            sources: Mutex::new(BTreeMap::new()),
            showing: Mutex::new(Showing::default()),
        });
        let weak = Arc::downgrade(&inner);
        tokio::spawn(async move {
            while let Some(picture) = outgoing.recv().await {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                inner.write(picture).await;
            }
        });
        Arc::new(Self { inner })
    }

    /// The channel's keys from now on: what seals the pictures this device
    /// sends, and what opens the ones that arrive.
    pub fn set_keys(&self, keys: &ChannelContentKeys, roster: Roster) {
        let sealing = Sealing::of(keys, self.inner.own);
        let mut current = self.inner.current.lock_recover();
        *current = sealing.map(|sealing| Together {
            opener: Opener::new(keys.clone(), roster),
            sealing,
        });
    }

    /// Open a lane to a device of this call: its pictures are read from now
    /// on, and what it shows is what this device encodes.
    pub fn add(self: &Arc<Self>, peer: Peer) {
        let placed = PicturePeer::new(peer);
        let remote = placed.remote();
        {
            let mut peers = self.inner.peers.lock_recover();
            if let Some(index) = peers.iter().position(|held| held.remote() == remote) {
                let old = peers.remove(index);
                old.peer.close();
            }
            peers.push(placed.clone());
        }
        tokio::spawn(placed.clone().follow_control(Arc::clone(&self.inner)));
        tokio::spawn(placed.clone().read_pictures(Arc::clone(&self.inner)));
        if let Some(controls) = placed.take_controls() {
            tokio::spawn(placed.clone().write_controls(controls));
        }
        // A lane that joined after the app said what it shows hears it now.
        let (camera, screen) = self
            .inner
            .showing
            .lock_recover()
            .mine
            .get(&remote)
            .cloned()
            .unwrap_or_default();
        placed.send_shows(&camera, &screen);
        self.inner.wake();
    }

    /// Shut the call's pictures: every lane stops, and nothing more is
    /// encoded or kept.
    pub fn close(&self) {
        self.inner.sources.lock_recover().clear();
        for peer in self.inner.peers.lock_recover().drain(..) {
            peer.peer.close();
        }
        self.inner.inbox.lock_recover().clear();
    }

    /// The devices this device has a picture lane with.
    #[must_use]
    pub fn devices(&self) -> Vec<[u8; 32]> {
        self.inner
            .peers
            .lock_recover()
            .iter()
            .map(PicturePeer::remote)
            .collect()
    }

    /// What this device shows of `device`: its camera and screen layers.
    pub fn show(&self, device: [u8; 32], camera: &[Layer], screen: &[Layer]) {
        {
            let mut showing = self.inner.showing.lock_recover();
            if camera.is_empty() && screen.is_empty() {
                showing.mine.remove(&device);
            } else {
                showing
                    .mine
                    .insert(device, (camera.to_vec(), screen.to_vec()));
            }
        }
        for peer in self.inner.peers.lock_recover().iter() {
            if peer.remote() == device {
                peer.send_shows(camera, screen);
            }
        }
    }

    /// This device's own camera tile shows `layer`, or nothing.
    pub fn own_tile(&self, layer: Option<Layer>) {
        self.inner.showing.lock_recover().own = layer;
        self.inner.wake();
    }

    /// Start publishing `source`, encoding it for the layers someone shows.
    ///
    /// # Errors
    /// The encoder thread could not be started.
    pub fn start(self: &Arc<Self>, source: Source, fps: u32) -> Result<Arc<Sending>, String> {
        let sink: Arc<dyn Sink> = self.inner.clone();
        let sending = Arc::new(Sending::start(source, fps, &sink)?);
        self.inner
            .sources
            .lock_recover()
            .insert(source, Arc::clone(&sending));
        Ok(sending)
    }

    /// Stop publishing `source`: its encoder stops, and nothing more of it
    /// is sent.
    pub fn stop(&self, source: Source) {
        self.inner.sources.lock_recover().remove(&source);
    }

    /// The pictures the app has not collected, oldest first, up to `budget`
    /// bytes.
    pub fn take(&self, after: u64, budget: usize) -> (Vec<Picture>, u64) {
        self.inner.inbox.lock_recover().take(after, budget)
    }

    /// Woken whenever a picture arrives.
    #[must_use]
    pub fn signal(&self) -> Arc<tokio::sync::Notify> {
        self.inner.inbox.lock_recover().signal()
    }
}

impl Inner {
    /// Something a device shows changed: every source encodes for it from
    /// now.
    fn wake(&self) {
        let sources = self.sources.lock_recover();
        for sending in sources.values() {
            sending.ask_keyframe();
        }
    }

    /// What the devices of the call show, and what a device asked to start
    /// over.
    fn asking(&self, source: Source) -> Asking {
        let mut wanted = BTreeSet::new();
        for peer in self.peers.lock_recover().iter() {
            wanted.extend(peer.layers(source));
        }
        let mut showing = self.showing.lock_recover();
        let restart = showing.restart.remove(&source);
        let own = showing.own;
        drop(showing);
        let mut asking = Asking {
            restart: BTreeSet::new(),
            own,
            wanted: BTreeMap::new(),
        };
        if restart {
            asking.restart.insert(source);
        }
        if !wanted.is_empty() {
            asking.wanted.insert(source, wanted);
        }
        asking
    }

    /// A device's picture stream fell behind or it asked for `source` afresh:
    /// start it over at a keyframe.
    fn asked(&self, source: Source) {
        self.showing.lock_recover().restart.insert(source);
        if let Some(sending) = self.sources.lock_recover().get(&source) {
            sending.ask_keyframe();
        }
    }

    /// A device's picture stream was cut off before a picture ended.
    fn behind(&self, from: [u8; 32], source: Source) {
        self.inbox.lock_recover().behind(from, source);
    }

    /// A device's shows changed.
    fn showed(&self) {
        self.wake();
    }

    /// One picture arrived on `peer`'s lane: opened with the call's keys,
    /// and queued for the app.
    fn arrived(&self, peer: &PicturePeer, source: Source, sealed: &[u8]) {
        let Some(body) = self.opened(peer, source, sealed) else {
            return;
        };
        let Ok(header) = video::validate(&body) else {
            return;
        };
        let dropped = self.inbox.lock_recover().admit(Picture {
            from: peer.remote(),
            source,
            keyframe: header.keyframe,
            object: body,
        });
        for (device, source) in dropped {
            self.ask(device, source);
        }
    }

    /// `sealed` as the picture it carries, unless this device holds no keys
    /// for the call or the picture is not for it.
    fn opened(&self, peer: &PicturePeer, source: Source, sealed: &[u8]) -> Option<Vec<u8>> {
        let current = self.current.lock_recover();
        let opened = current
            .as_ref()?
            .opener
            .open(peer.remote(), stream_of(source), sealed)
            .ok();
        drop(current);
        opened
    }

    /// Ask the device `device` to start `source` over at a keyframe.
    fn ask(&self, device: [u8; 32], source: Source) {
        for peer in self.peers.lock_recover().iter() {
            if peer.remote() == device {
                peer.ask_keyframe(source);
            }
        }
    }

    /// Seal `object` and hand it to the writer, unless this device holds no
    /// keys for the call.
    fn seal_and_send(&self, source: Source, object: &VideoObject) {
        let current = self.current.lock_recover();
        let Some(together) = current.as_ref() else {
            return;
        };
        let (sealer, counter) = together.sealing.sealer(source);
        let Ok(body) = object.encode() else {
            return;
        };
        let sequence = counter.fetch_add(1, Ordering::Relaxed);
        let Ok(packet) = sealer.seal(sequence, &body) else {
            return;
        };
        drop(current);
        // A writer that cannot keep up loses this picture, not the ones
        // behind it.
        let _ = self.outbox.try_send(Outgoing {
            source,
            layer: object.layer,
            keyframe: object.keyframe,
            sealed: packet,
        });
    }

    /// This person's own camera picture, for their own tile.
    fn keep_own(&self, object: &VideoObject) {
        let layer = self.showing.lock_recover().own;
        if layer != Some(object.layer) {
            return;
        }
        let Ok(bytes) = object.encode() else {
            return;
        };
        let own = self.own;
        self.inbox.lock_recover().admit(Picture {
            from: own,
            source: Source::Camera,
            keyframe: object.keyframe,
            object: bytes,
        });
    }

    /// Write one sealed picture to the devices that show its layer.
    async fn write(&self, picture: Outgoing) {
        let peers = self.peers.lock_recover().clone();
        for peer in peers {
            peer.send_picture(
                picture.source,
                picture.layer,
                picture.keyframe,
                &picture.sealed,
            )
            .await;
        }
    }
}

impl Sink for Inner {
    fn asking(&self, source: Source) -> Asking {
        self.asking(source)
    }

    fn sent(&self, source: Source, object: &VideoObject) {
        self.seal_and_send(source, object);
    }

    fn own(&self, object: &VideoObject) {
        self.keep_own(object);
    }
}

/// The camera a call's pictures come from: the device's own, or the moving
/// test picture for a device without one (a virtual machine, a nested
/// development session).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Camera {
    /// The device's own camera.
    #[default]
    Device,
    /// The moving test picture: `TILO_CALL_CAMERA=test-pattern`.
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
