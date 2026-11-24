//! The carrier over one byte stream: a desktop session's link to its own device daemon.
//!
//! The daemon holds the device's one endpoint, and with it the relay of every workspace this
//! device serves and the connections to the relays of the others. The session that captures,
//! encodes and draws a call stays a `moq-lite` client like any other device: its live session
//! runs on [`Session`], which multiplexes the streams and datagrams `moq-lite` opens over one
//! stream of the daemon's socket. Everything above the carrier, the admission preface, `moq-lite`
//! and the relay's judgement, is the same code as over iroh.
//!
//! Before the carrier starts, the session names where the daemon takes it ([`Toward`]): into the
//! relay the daemon runs, for a workspace this device serves, or on to the relay of another
//! device, which the daemon dials from its endpoint and splices to the link stream by stream
//! ([`splice`]). The relay judges the session's grant either way.
//!
//! # Frames
//!
//! | Bytes | Field |
//! | --- | --- |
//! | 1 | kind ([`Kind`]) |
//! | 8 | stream id, little-endian; 0 for a datagram or a close |
//! | 4 | payload length, little-endian, at most [`MAX_PAYLOAD`] |
//! | length | payload |
//!
//! A stream id's lowest bit says which end opened it and the next whether it carries one way
//! only, as QUIC's do. Each stream is flow-controlled on its own ([`WINDOW`]): what one stream's
//! reader leaves unread never holds up another. The writer sends what was asked of the link in
//! this order: control frames, then datagrams (a call's voice), then stream data by the stream's
//! priority, in pieces of at most [`CHUNK`] bytes, so a picture's keyframe never keeps a voice
//! frame waiting longer than one piece takes. Datagrams are dropped oldest first when more wait
//! than [`DATAGRAMS_KEPT`], as a network drops them.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use bytes::{Buf as _, BufMut as _, Bytes, BytesMut};
use moq_net::web_transport_trait as wt;
use moq_net::web_transport_trait::poll;
use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::Task;
use crate::admit::{AdmissionError, Dial, Dialling, start};
use crate::preface::PROTOCOL;

/// The device's own resource its daemon publishes the endpoints its relay turned away from each
/// workspace's live session as, and has not let in since, read in the person's own workspace with
/// `record` empty: a JSON object of lists by workspace, each endpoint with its key (`device`), its
/// person's name for it, why (`why`, with the epochs `presented` and `newest` for keys behind)
/// and how often (`times`). A relay tells the device it turns away nothing but that it was not
/// let in, so this, read on the relaying device beside the refused device's own account, says why.
pub const TURNED_AWAY: &str = "tilo.live.turned-away";

/// The device's own resource its daemon publishes how far this device's place in each workspace's
/// live session came as, read in the person's own workspace with `record` empty: a JSON object by
/// workspace, each with the state (`"open"`, `"waiting-for-keys"`, `"keyless"`, `"unread"`,
/// `"no-relay"`, `"unreached"`, `"busy"` or `"refused"`), the device that relays it (its key in
/// `relay`, its person's name in `relay_name`, and `relay_here` when it is this computer), and the
/// epoch the session seals under.
pub const SESSIONS: &str = "tilo.live.sessions";

/// Bytes one stream may have in flight unread before its reader takes some.
pub const WINDOW: u64 = 1 << 20;
/// The largest piece of one stream the writer sends at once.
pub const CHUNK: usize = 16 * 1024;
/// The largest payload a frame carries: a piece of a stream, or a close's reason.
pub const MAX_PAYLOAD: u32 = 64 * 1024;
/// Datagrams waiting to be written, or to be read, before the oldest is dropped.
pub const DATAGRAMS_KEPT: usize = 128;
/// The largest datagram the link carries: what a QUIC path carries, so a session's datagrams
/// fit the relay's connection onwards too.
pub const MAX_DATAGRAM: usize = 1200;
/// Streams the other end may hold open on one link at once.
pub const MAX_STREAMS: usize = 2048;
/// The longest a route may take to read, in bytes.
const MAX_ROUTE: usize = 4096;

/// One frame's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Kind {
    Open = 1,
    Data = 2,
    Fin = 3,
    Reset = 4,
    Stop = 5,
    Credit = 6,
    Priority = 7,
    Datagram = 8,
    Close = 9,
}

impl Kind {
    const fn of(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Open,
            2 => Self::Data,
            3 => Self::Fin,
            4 => Self::Reset,
            5 => Self::Stop,
            6 => Self::Credit,
            7 => Self::Priority,
            8 => Self::Datagram,
            9 => Self::Close,
            _ => return None,
        })
    }
}

/// Where the daemon takes a session's link: written once, before the carrier starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Toward {
    /// Into the relay the daemon runs: a workspace this device serves.
    Here,
    /// On to the relay of another device, at this address.
    Relay(iroh::EndpointAddr),
    /// Into the device's own live session, which the daemon holds for `workspace`: what a
    /// person's desktop captures and plays out through.
    Call { workspace: String },
}

/// Write `toward` to `writer`: the opening of a link.
///
/// # Errors
/// The stream did not take it.
pub async fn write_route<W: AsyncWrite + Unpin>(
    writer: &mut W,
    toward: &Toward,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(toward).map_err(std::io::Error::other)?;
    let length = u16::try_from(bytes.len())
        .ok()
        .filter(|length| usize::from(*length) <= MAX_ROUTE)
        .ok_or_else(|| std::io::Error::other("a route too long to say"))?;
    writer.write_all(&length.to_le_bytes()).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await
}

/// Read the route a link opens with.
///
/// # Errors
/// The stream ended, or what it said is no route.
pub async fn read_route<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<Toward> {
    let mut length = [0_u8; 2];
    reader.read_exact(&mut length).await?;
    let length = usize::from(u16::from_le_bytes(length));
    if length > MAX_ROUTE {
        return Err(std::io::Error::other("a route too long to read"));
    }
    let mut bytes = vec![0_u8; length];
    reader.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}

/// Why an operation on a link failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// The other end closed the session with `code`.
    #[error("closed by the other end ({code}): {reason}")]
    Closed { code: u32, reason: String },
    /// This end closed the session.
    #[error("closed here ({code}): {reason}")]
    ClosedHere { code: u32, reason: String },
    /// The byte stream under the link ended.
    #[error("the link ended")]
    Ended,
    /// The other end reset the stream.
    #[error("stream reset ({0})")]
    Reset(u32),
    /// The other end stopped reading the stream.
    #[error("stream stopped ({0})")]
    Stopped(u32),
    /// The stream was finished or reset here: it takes no more bytes.
    #[error("stream finished")]
    Finished,
    /// The datagram is longer than the link carries.
    #[error("datagram too long")]
    TooLong,
}

impl wt::Error for LinkError {
    fn session_error(&self) -> Option<(u32, String)> {
        match self {
            Self::Closed { code, reason } => Some((*code, reason.clone())),
            _ => None,
        }
    }

    fn stream_error(&self) -> Option<u32> {
        match self {
            Self::Reset(code) | Self::Stopped(code) => Some(*code),
            _ => None,
        }
    }
}

/// Which end of the link this is: the end that dialled opens even streams, the other odd.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// The session, which opened the link.
    Dialler,
    /// The daemon, which accepted it.
    Listener,
}

/// One frame to write.
enum Frame {
    Open(u64),
    Data(u64, Bytes),
    Fin(u64),
    Reset(u64, u32),
    Stop(u64, u32),
    Credit(u64, u64),
    Priority(u64, u8),
    Datagram(Bytes),
    Close(u32, String),
}

impl Frame {
    fn encode(&self, out: &mut BytesMut) {
        let (kind, id, payload): (Kind, u64, &[u8]) = match self {
            Self::Open(id) => (Kind::Open, *id, &[]),
            Self::Data(id, bytes) => (Kind::Data, *id, bytes),
            Self::Fin(id) => (Kind::Fin, *id, &[]),
            Self::Reset(id, code) => return head(out, Kind::Reset, *id, &code.to_le_bytes()),
            Self::Stop(id, code) => return head(out, Kind::Stop, *id, &code.to_le_bytes()),
            Self::Credit(id, bytes) => return head(out, Kind::Credit, *id, &bytes.to_le_bytes()),
            Self::Priority(id, order) => (Kind::Priority, *id, std::slice::from_ref(order)),
            Self::Datagram(bytes) => (Kind::Datagram, 0, bytes),
            Self::Close(code, reason) => {
                let mut payload = code.to_le_bytes().to_vec();
                let reason = reason.as_bytes();
                let room = usize::try_from(MAX_PAYLOAD)
                    .unwrap_or(usize::MAX)
                    .saturating_sub(payload.len());
                payload.extend_from_slice(reason.get(..reason.len().min(room)).unwrap_or(&[]));
                return head(out, Kind::Close, 0, &payload);
            }
        };
        head(out, kind, id, payload);
    }
}

fn head(out: &mut BytesMut, kind: Kind, id: u64, payload: &[u8]) {
    out.put_u8(kind as u8);
    out.put_u64_le(id);
    out.put_u32_le(u32::try_from(payload.len()).unwrap_or(u32::MAX));
    out.put_slice(payload);
}

/// The receiving half of one stream.
#[derive(Default)]
struct Receiving {
    buffered: VecDeque<Bytes>,
    held: u64,
    fin: bool,
    reset: Option<u32>,
    /// Stopped here, or its handle let go: what arrives is dropped.
    stopped: bool,
    /// Read since the last credit this end granted.
    read: u64,
    /// The order the other end sends this stream in.
    priority: u8,
    waker: Option<Waker>,
}

impl Receiving {
    fn done(&self) -> bool {
        self.stopped || self.reset.is_some() || (self.fin && self.buffered.is_empty())
    }
}

/// The sending half of one stream.
struct Sending {
    queued: VecDeque<Bytes>,
    credit: u64,
    priority: u8,
    finishing: bool,
    fin_sent: bool,
    reset: bool,
    stopped: Option<u32>,
    writer: Option<Waker>,
    closed: Option<Waker>,
}

impl Sending {
    const fn new() -> Self {
        Self {
            queued: VecDeque::new(),
            credit: WINDOW,
            priority: 0,
            finishing: false,
            fin_sent: false,
            reset: false,
            stopped: None,
            writer: None,
            closed: None,
        }
    }

    fn done(&self) -> bool {
        self.reset || self.stopped.is_some() || self.fin_sent
    }

    fn wants_writing(&self) -> bool {
        !self.done() && (!self.queued.is_empty() || self.finishing)
    }

    fn wake(&mut self) {
        if let Some(waker) = self.writer.take() {
            waker.wake();
        }
        if let Some(waker) = self.closed.take() {
            waker.wake();
        }
    }
}

/// One stream of the link.
struct Stream {
    receiving: Option<Receiving>,
    sending: Option<Sending>,
    /// Whether a handle still holds each half.
    held: (bool, bool),
}

impl Stream {
    fn removable(&self) -> bool {
        let receiving = !self.held.0 && self.receiving.as_ref().is_none_or(Receiving::done);
        let sending = !self.held.1
            && self
                .sending
                .as_ref()
                .is_none_or(|sending| sending.done() || !sending.wants_writing());
        receiving && sending
    }
}

#[derive(Default)]
struct Counters {
    sent_bytes: u64,
    received_bytes: u64,
    sent_frames: u64,
    received_frames: u64,
}

/// One of each for the streams that carry both ways and those that carry one way.
#[derive(Default)]
struct Ways<T> {
    bi: T,
    uni: T,
}

impl<T> Ways<T> {
    const fn of(&mut self, uni: bool) -> &mut T {
        if uni { &mut self.uni } else { &mut self.bi }
    }
}

/// The link's state, shared by every handle of it and its reader and writer.
struct State {
    end: End,
    closed: Option<LinkError>,
    opened: Ways<u64>,
    streams: BTreeMap<u64, Stream>,
    accept: Ways<VecDeque<u64>>,
    accepting: Ways<Vec<Waker>>,
    heard: VecDeque<Bytes>,
    hearing: Vec<Waker>,
    control: VecDeque<Frame>,
    datagrams: VecDeque<Bytes>,
    writer: Option<Waker>,
    closing: Vec<Waker>,
    counters: Counters,
}

impl State {
    fn new(end: End) -> Self {
        Self {
            end,
            closed: None,
            opened: Ways::default(),
            streams: BTreeMap::new(),
            accept: Ways::default(),
            accepting: Ways::default(),
            heard: VecDeque::new(),
            hearing: Vec::new(),
            control: VecDeque::new(),
            datagrams: VecDeque::new(),
            writer: None,
            closing: Vec::new(),
            counters: Counters::default(),
        }
    }

    fn wake_writer(&mut self) {
        if let Some(waker) = self.writer.take() {
            waker.wake();
        }
    }

    fn send(&mut self, frame: Frame) {
        self.control.push_back(frame);
        self.wake_writer();
    }

    /// End the link with `why`: every wait it holds is woken to see it.
    fn end(&mut self, why: LinkError) {
        if self.closed.is_some() {
            return;
        }
        self.closed = Some(why);
        for waker in std::mem::take(&mut self.accepting.bi)
            .into_iter()
            .chain(std::mem::take(&mut self.accepting.uni))
            .chain(std::mem::take(&mut self.hearing))
            .chain(std::mem::take(&mut self.closing))
        {
            waker.wake();
        }
        for stream in self.streams.values_mut() {
            if let Some(receiving) = &mut stream.receiving
                && let Some(waker) = receiving.waker.take()
            {
                waker.wake();
            }
            if let Some(sending) = &mut stream.sending {
                sending.wake();
            }
        }
        self.wake_writer();
    }

    fn open(&mut self, uni: bool) -> u64 {
        let next = self.opened.of(uni);
        let counter = *next;
        *next = counter.wrapping_add(1);
        let opener = u64::from(self.end == End::Listener);
        let id = (counter << 2) | (u64::from(uni) << 1) | opener;
        self.streams.insert(
            id,
            Stream {
                receiving: (!uni).then(Receiving::default),
                sending: Some(Sending::new()),
                held: (!uni, true),
            },
        );
        self.send(Frame::Open(id));
        id
    }

    fn tidy(&mut self, id: u64) {
        if self.streams.get(&id).is_some_and(Stream::removable) {
            self.streams.remove(&id);
        }
    }

    /// The next frame to write, by the link's order: control, datagrams, then the stream with
    /// the highest priority that has something to send.
    fn next_frame(&mut self) -> Option<Frame> {
        if let Some(frame) = self.control.pop_front() {
            return Some(frame);
        }
        if self.closed.is_some() {
            return None;
        }
        if let Some(datagram) = self.datagrams.pop_front() {
            return Some(Frame::Datagram(datagram));
        }
        let id = self
            .streams
            .iter()
            .filter_map(|(id, stream)| {
                let sending = stream.sending.as_ref()?;
                sending.wants_writing().then_some((sending.priority, *id))
            })
            // The highest priority, and among equals the oldest stream.
            .max_by(|(a, a_id), (b, b_id)| a.cmp(b).then(b_id.cmp(a_id)))
            .map(|(_, id)| id)?;
        let stream = self.streams.get_mut(&id)?;
        let sending = stream.sending.as_mut()?;
        let frame = if let Some(front) = sending.queued.front_mut() {
            let piece = if front.len() > CHUNK {
                front.split_to(CHUNK)
            } else {
                sending.queued.pop_front().unwrap_or_default()
            };
            Frame::Data(id, piece)
        } else {
            sending.fin_sent = true;
            sending.wake();
            Frame::Fin(id)
        };
        self.tidy(id);
        Some(frame)
    }

    /// The other end opened stream `id`.
    fn heard_open(&mut self, id: u64) -> Result<(), LinkError> {
        let opener = u64::from(self.end == End::Dialler);
        if id & 1 != opener || self.streams.contains_key(&id) {
            return Err(LinkError::Ended);
        }
        if self.streams.len() >= MAX_STREAMS {
            return Err(LinkError::Ended);
        }
        let uni = id & 2 != 0;
        self.streams.insert(
            id,
            Stream {
                receiving: Some(Receiving::default()),
                sending: (!uni).then(Sending::new),
                held: (true, !uni),
            },
        );
        self.accept.of(uni).push_back(id);
        for waker in std::mem::take(self.accepting.of(uni)) {
            waker.wake();
        }
        Ok(())
    }

    /// The other end sent `payload` on stream `id`.
    fn heard_data(&mut self, id: u64, payload: Bytes) -> Result<(), LinkError> {
        let Some(receiving) = self
            .streams
            .get_mut(&id)
            .and_then(|stream| stream.receiving.as_mut())
        else {
            return Ok(());
        };
        if receiving.stopped || receiving.reset.is_some() {
            return Ok(());
        }
        receiving.held = receiving.held.saturating_add(payload.len() as u64);
        if receiving.held > WINDOW {
            // The other end sent past what this end granted.
            return Err(LinkError::Ended);
        }
        receiving.buffered.push_back(payload);
        if let Some(waker) = receiving.waker.take() {
            waker.wake();
        }
        Ok(())
    }

    /// Take in one frame the other end wrote.
    fn heard_frame(&mut self, kind: Kind, id: u64, payload: Bytes) -> Result<(), LinkError> {
        self.counters.received_frames = self.counters.received_frames.wrapping_add(1);
        self.counters.received_bytes = self
            .counters
            .received_bytes
            .wrapping_add(13)
            .wrapping_add(payload.len() as u64);
        match kind {
            Kind::Open => self.heard_open(id)?,
            Kind::Data => self.heard_data(id, payload)?,
            Kind::Fin | Kind::Reset => {
                let code = (kind == Kind::Reset).then(|| code_of(&payload));
                if let Some(receiving) = self
                    .streams
                    .get_mut(&id)
                    .and_then(|stream| stream.receiving.as_mut())
                {
                    match code {
                        Some(code) => {
                            receiving.reset = Some(code);
                            receiving.buffered.clear();
                        }
                        None => receiving.fin = true,
                    }
                    if let Some(waker) = receiving.waker.take() {
                        waker.wake();
                    }
                }
                self.tidy(id);
            }
            Kind::Stop => {
                if let Some(sending) = self
                    .streams
                    .get_mut(&id)
                    .and_then(|stream| stream.sending.as_mut())
                {
                    sending.stopped = Some(code_of(&payload));
                    sending.queued.clear();
                    sending.wake();
                }
                self.tidy(id);
            }
            Kind::Credit => {
                let granted = payload
                    .get(..8)
                    .and_then(|bytes| bytes.try_into().ok())
                    .map_or(0, u64::from_le_bytes);
                if let Some(sending) = self
                    .streams
                    .get_mut(&id)
                    .and_then(|stream| stream.sending.as_mut())
                {
                    sending.credit = sending.credit.saturating_add(granted);
                    if let Some(waker) = sending.writer.take() {
                        waker.wake();
                    }
                }
            }
            Kind::Priority => {
                if let Some(receiving) = self
                    .streams
                    .get_mut(&id)
                    .and_then(|stream| stream.receiving.as_mut())
                {
                    receiving.priority = payload.first().copied().unwrap_or(0);
                }
            }
            Kind::Datagram => {
                if self.heard.len() >= DATAGRAMS_KEPT {
                    self.heard.pop_front();
                }
                self.heard.push_back(payload);
                for waker in std::mem::take(&mut self.hearing) {
                    waker.wake();
                }
            }
            Kind::Close => {
                let code = code_of(&payload);
                let reason = String::from_utf8_lossy(payload.get(4..).unwrap_or(&[])).into_owned();
                self.end(LinkError::Closed { code, reason });
            }
        }
        Ok(())
    }
}

fn code_of(payload: &[u8]) -> u32 {
    payload
        .get(..4)
        .and_then(|bytes| bytes.try_into().ok())
        .map_or(0, u32::from_le_bytes)
}

struct Inner {
    state: Mutex<State>,
    /// The reader and writer of the byte stream, aborted when the last handle goes.
    io: Mutex<Vec<Task>>,
}

/// One link as a `moq-lite` transport. Cheap to clone: every clone drives the same link.
#[derive(Clone)]
pub struct Session {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("link::Session").finish_non_exhaustive()
    }
}

impl Session {
    /// The carrier over `reader` and `writer`, as `end`: its reader and writer run on the
    /// ambient executor for as long as a handle of it lives.
    #[must_use]
    pub fn new<R, W>(reader: R, writer: W, end: End) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let inner = Arc::new(Inner {
            state: Mutex::new(State::new(end)),
            io: Mutex::new(Vec::new()),
        });
        let reading = Task::spawn(read_loop(Arc::downgrade(&inner), reader));
        let writing = Task::spawn(write_loop(Arc::downgrade(&inner), writer));
        inner.io.lock_recover().extend([reading, writing]);
        Self { inner }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.state.lock_recover()
    }

    /// Close the link with `code` and `reason`, which the other end reads as the session's end.
    pub fn close(&self, code: u32, reason: &str) {
        poll::Session::close(&mut self.clone(), code, reason);
    }

    fn accept(&self, cx: &Context<'_>, uni: bool) -> Poll<Result<u64, LinkError>> {
        let mut state = self.state();
        if let Some(id) = state.accept.of(uni).pop_front() {
            return Poll::Ready(Ok(id));
        }
        if let Some(closed) = &state.closed {
            return Poll::Ready(Err(closed.clone()));
        }
        state.accepting.of(uni).push(cx.waker().clone());
        Poll::Pending
    }

    fn opened(&self, uni: bool) -> Result<u64, LinkError> {
        let mut state = self.state();
        if let Some(closed) = &state.closed {
            return Err(closed.clone());
        }
        Ok(state.open(uni))
    }

    fn pair(&self, id: u64) -> poll::BiStreams<Self> {
        (
            Outgoing {
                inner: Arc::clone(&self.inner),
                id,
            },
            Incoming {
                inner: Arc::clone(&self.inner),
                id,
            },
        )
    }
}

async fn read_loop<R: AsyncRead + Unpin>(inner: std::sync::Weak<Inner>, mut reader: R) {
    let ended = loop {
        let mut header = [0_u8; 13];
        if reader.read_exact(&mut header).await.is_err() {
            break LinkError::Ended;
        }
        let kind = Kind::of(header[0]);
        let id = u64::from_le_bytes(header[1..9].try_into().unwrap_or_default());
        let length = u32::from_le_bytes(header[9..13].try_into().unwrap_or_default());
        let Some(kind) = kind.filter(|_| length <= MAX_PAYLOAD) else {
            break LinkError::Ended;
        };
        let mut payload = vec![0_u8; usize::try_from(length).unwrap_or(0)];
        if reader.read_exact(&mut payload).await.is_err() {
            break LinkError::Ended;
        }
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let heard = inner
            .state
            .lock_recover()
            .heard_frame(kind, id, Bytes::from(payload));
        if let Err(error) = heard {
            break error;
        }
    };
    if let Some(inner) = inner.upgrade() {
        inner.state.lock_recover().end(ended);
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(inner: std::sync::Weak<Inner>, mut writer: W) {
    let mut out = BytesMut::new();
    loop {
        let more = std::future::poll_fn(|cx| {
            let Some(inner) = inner.upgrade() else {
                return Poll::Ready(false);
            };
            let mut state = inner.state.lock_recover();
            // Gather what waits, up to a few pieces, into one write.
            while out.len() < 4 * CHUNK {
                let Some(frame) = state.next_frame() else {
                    break;
                };
                frame.encode(&mut out);
                state.counters.sent_frames = state.counters.sent_frames.wrapping_add(1);
            }
            if !out.is_empty() {
                state.counters.sent_bytes =
                    state.counters.sent_bytes.wrapping_add(out.len() as u64);
                return Poll::Ready(true);
            }
            if state.closed.is_some() {
                return Poll::Ready(false);
            }
            state.writer = Some(cx.waker().clone());
            Poll::Pending
        })
        .await;
        if !out.is_empty() {
            let written = writer.write_all(&out).await.and(writer.flush().await);
            out.clear();
            if written.is_err() {
                if let Some(inner) = inner.upgrade() {
                    inner.state.lock_recover().end(LinkError::Ended);
                }
                return;
            }
        }
        if !more {
            let _ = writer.shutdown().await;
            return;
        }
    }
}

impl poll::Session for Session {
    type SendStream = Outgoing;
    type RecvStream = Incoming;
    type Error = LinkError;

    fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Incoming, LinkError>> {
        self.accept(cx, true).map(|accepted| {
            accepted.map(|id| Incoming {
                inner: Arc::clone(&self.inner),
                id,
            })
        })
    }

    fn poll_accept_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<poll::BiStreams<Self>, LinkError>> {
        self.accept(cx, false)
            .map(|accepted| accepted.map(|id| self.pair(id)))
    }

    fn poll_open_uni(&mut self, _cx: &mut Context<'_>) -> Poll<Result<Outgoing, LinkError>> {
        Poll::Ready(self.opened(true).map(|id| Outgoing {
            inner: Arc::clone(&self.inner),
            id,
        }))
    }

    fn poll_open_bi(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<poll::BiStreams<Self>, LinkError>> {
        Poll::Ready(self.opened(false).map(|id| self.pair(id)))
    }

    /// A datagram is queued and never waited for: the oldest waiting is dropped instead.
    fn poll_send_datagram(
        &mut self,
        _cx: &mut Context<'_>,
        payload: &[u8],
    ) -> Poll<Result<(), LinkError>> {
        if payload.len() > MAX_DATAGRAM {
            return Poll::Ready(Err(LinkError::TooLong));
        }
        let mut state = self.state();
        if let Some(closed) = &state.closed {
            return Poll::Ready(Err(closed.clone()));
        }
        if state.datagrams.len() >= DATAGRAMS_KEPT {
            state.datagrams.pop_front();
        }
        state.datagrams.push_back(Bytes::copy_from_slice(payload));
        state.wake_writer();
        drop(state);
        Poll::Ready(Ok(()))
    }

    fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, LinkError>> {
        let mut state = self.state();
        if let Some(datagram) = state.heard.pop_front() {
            return Poll::Ready(Ok(datagram));
        }
        if let Some(closed) = &state.closed {
            return Poll::Ready(Err(closed.clone()));
        }
        state.hearing.push(cx.waker().clone());
        Poll::Pending
    }

    fn max_datagram_size(&self) -> usize {
        MAX_DATAGRAM
    }

    fn protocol(&self) -> Option<&str> {
        Some(PROTOCOL)
    }

    fn close(&mut self, code: u32, reason: &str) {
        let mut state = self.state();
        if state.closed.is_some() {
            return;
        }
        state
            .control
            .push_back(Frame::Close(code, reason.to_owned()));
        state.end(LinkError::ClosedHere {
            code,
            reason: reason.to_owned(),
        });
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<LinkError> {
        let mut state = self.state();
        if let Some(closed) = &state.closed {
            return Poll::Ready(closed.clone());
        }
        state.closing.push(cx.waker().clone());
        Poll::Pending
    }

    fn stats(&self) -> impl wt::Stats {
        let state = self.state();
        Stats {
            sent_bytes: state.counters.sent_bytes,
            received_bytes: state.counters.received_bytes,
            sent_frames: state.counters.sent_frames,
            received_frames: state.counters.received_frames,
        }
    }
}

/// A link's counters at one moment. A link loses nothing: what it sent arrives, or the link
/// ended.
struct Stats {
    sent_bytes: u64,
    received_bytes: u64,
    sent_frames: u64,
    received_frames: u64,
}

impl wt::Stats for Stats {
    fn bytes_sent(&self) -> Option<u64> {
        Some(self.sent_bytes)
    }

    fn bytes_received(&self) -> Option<u64> {
        Some(self.received_bytes)
    }

    fn bytes_lost(&self) -> Option<u64> {
        Some(0)
    }

    fn packets_sent(&self) -> Option<u64> {
        Some(self.sent_frames)
    }

    fn packets_received(&self) -> Option<u64> {
        Some(self.received_frames)
    }

    fn packets_lost(&self) -> Option<u64> {
        Some(0)
    }
}

/// One stream of a link, as this end writes it.
pub struct Outgoing {
    inner: Arc<Inner>,
    id: u64,
}

impl Outgoing {
    fn with<T>(&self, apply: impl FnOnce(&mut State, &mut Sending) -> T) -> Option<T> {
        let mut state = self.inner.state.lock_recover();
        let mut sending = state
            .streams
            .get_mut(&self.id)
            .and_then(|stream| stream.sending.take())?;
        let answer = apply(&mut state, &mut sending);
        if let Some(stream) = state.streams.get_mut(&self.id) {
            stream.sending = Some(sending);
        }
        drop(state);
        Some(answer)
    }
}

impl poll::SendStream for Outgoing {
    type Error = LinkError;

    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, LinkError>> {
        self.with(|state, sending| {
            if let Some(closed) = &state.closed {
                return Poll::Ready(Err(closed.clone()));
            }
            if let Some(code) = sending.stopped {
                return Poll::Ready(Err(LinkError::Stopped(code)));
            }
            if sending.finishing || sending.reset {
                return Poll::Ready(Err(LinkError::Finished));
            }
            if buf.is_empty() {
                return Poll::Ready(Ok(0));
            }
            if sending.credit == 0 {
                sending.writer = Some(cx.waker().clone());
                return Poll::Pending;
            }
            let taken = buf
                .len()
                .min(usize::try_from(sending.credit).unwrap_or(usize::MAX))
                .min(usize::try_from(MAX_PAYLOAD).unwrap_or(usize::MAX));
            sending.credit = sending.credit.saturating_sub(taken as u64);
            sending
                .queued
                .push_back(Bytes::copy_from_slice(buf.get(..taken).unwrap_or_default()));
            state.wake_writer();
            Poll::Ready(Ok(taken))
        })
        .unwrap_or(Poll::Ready(Err(LinkError::Finished)))
    }

    fn set_priority(&mut self, order: u8) {
        let id = self.id;
        self.with(|state, sending| {
            if sending.priority != order {
                sending.priority = order;
                state.send(Frame::Priority(id, order));
            }
        });
    }

    fn finish(&mut self) -> Result<(), LinkError> {
        self.with(|state, sending| {
            if sending.reset || sending.finishing {
                return Err(LinkError::Finished);
            }
            sending.finishing = true;
            state.wake_writer();
            Ok(())
        })
        .unwrap_or(Err(LinkError::Finished))
    }

    fn reset(&mut self, code: u32) {
        let id = self.id;
        self.with(|state, sending| {
            if sending.done() {
                return;
            }
            sending.reset = true;
            sending.queued.clear();
            sending.wake();
            state.send(Frame::Reset(id, code));
        });
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), LinkError>> {
        self.with(|state, sending| {
            if let Some(code) = sending.stopped {
                return Poll::Ready(Err(LinkError::Stopped(code)));
            }
            if sending.done() {
                return Poll::Ready(Ok(()));
            }
            if let Some(closed) = &state.closed {
                return Poll::Ready(Err(closed.clone()));
            }
            sending.closed = Some(cx.waker().clone());
            Poll::Pending
        })
        .unwrap_or(Poll::Ready(Ok(())))
    }
}

impl Drop for Outgoing {
    /// A stream let go unfinished is reset, as the trait asks: half a message is never read as
    /// a whole one.
    fn drop(&mut self) {
        poll::SendStream::reset(self, 0);
        let mut state = self.inner.state.lock_recover();
        if let Some(stream) = state.streams.get_mut(&self.id) {
            stream.held.1 = false;
        }
        state.tidy(self.id);
    }
}

/// One stream of a link, as this end reads it.
pub struct Incoming {
    inner: Arc<Inner>,
    id: u64,
}

impl Incoming {
    /// The order the other end sends this stream in: what a daemon passing the stream on to
    /// another relay sends it in there.
    #[must_use]
    pub fn priority(&self) -> u8 {
        self.inner
            .state
            .lock_recover()
            .streams
            .get(&self.id)
            .and_then(|stream| stream.receiving.as_ref())
            .map_or(0, |receiving| receiving.priority)
    }

    fn with<T>(&self, apply: impl FnOnce(&mut State, &mut Receiving) -> T) -> Option<T> {
        let mut state = self.inner.state.lock_recover();
        let mut receiving = state
            .streams
            .get_mut(&self.id)
            .and_then(|stream| stream.receiving.take())?;
        let answer = apply(&mut state, &mut receiving);
        if let Some(stream) = state.streams.get_mut(&self.id) {
            stream.receiving = Some(receiving);
        }
        drop(state);
        Some(answer)
    }
}

impl poll::RecvStream for Incoming {
    type Error = LinkError;

    fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, LinkError>> {
        let id = self.id;
        self.with(|state, receiving| {
            if let Some(code) = receiving.reset {
                return Poll::Ready(Err(LinkError::Reset(code)));
            }
            if receiving.stopped {
                return Poll::Ready(Err(LinkError::Finished));
            }
            if dst.is_empty() {
                return Poll::Ready(Ok(Some(0)));
            }
            let mut read = 0;
            while read < dst.len() {
                let Some(front) = receiving.buffered.front_mut() else {
                    break;
                };
                let taken = front.len().min(dst.len().saturating_sub(read));
                if let Some(into) = dst.get_mut(read..read.saturating_add(taken)) {
                    into.copy_from_slice(front.get(..taken).unwrap_or_default());
                }
                front.advance(taken);
                if front.is_empty() {
                    receiving.buffered.pop_front();
                }
                read = read.saturating_add(taken);
            }
            if read > 0 {
                receiving.held = receiving.held.saturating_sub(read as u64);
                receiving.read = receiving.read.saturating_add(read as u64);
                if receiving.read >= WINDOW / 4 {
                    let granted = std::mem::take(&mut receiving.read);
                    state.send(Frame::Credit(id, granted));
                }
                return Poll::Ready(Ok(Some(read)));
            }
            if receiving.fin {
                return Poll::Ready(Ok(None));
            }
            if let Some(closed) = &state.closed {
                return Poll::Ready(Err(closed.clone()));
            }
            receiving.waker = Some(cx.waker().clone());
            Poll::Pending
        })
        .unwrap_or(Poll::Ready(Err(LinkError::Finished)))
    }

    fn stop(&mut self, code: u32) {
        let id = self.id;
        self.with(|state, receiving| {
            if receiving.done() {
                return;
            }
            receiving.stopped = true;
            receiving.buffered.clear();
            state.send(Frame::Stop(id, code));
        });
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), LinkError>> {
        self.with(|state, receiving| {
            if let Some(code) = receiving.reset {
                return Poll::Ready(Err(LinkError::Reset(code)));
            }
            if receiving.done() {
                return Poll::Ready(Ok(()));
            }
            if let Some(closed) = &state.closed {
                return Poll::Ready(Err(closed.clone()));
            }
            receiving.waker = Some(cx.waker().clone());
            Poll::Pending
        })
        .unwrap_or(Poll::Ready(Ok(())))
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        poll::RecvStream::stop(self, 0);
        let mut state = self.inner.state.lock_recover();
        if let Some(stream) = state.streams.get_mut(&self.id) {
            stream.held.0 = false;
        }
        state.tidy(self.id);
    }
}

/// What opens one stream of the daemon's socket for a link: the session's own step, made once
/// per dial.
pub type Opening = Pin<
    Box<
        dyn Future<
                Output = std::io::Result<(
                    Box<dyn AsyncRead + Unpin + Send>,
                    Box<dyn AsyncWrite + Unpin + Send>,
                )>,
            > + Send,
    >,
>;

/// The session's carrier to a relay through its own daemon: each dial opens a stream of the
/// daemon's socket ([`Opening`]), names where the daemon takes it, and runs the admission and
/// `moq-lite` over it. A link that ends is dialled again, as a connection between two devices is.
pub struct Daemon {
    open: Box<dyn Fn() -> Opening + Send + Sync>,
    toward: Toward,
}

impl Daemon {
    /// The carrier that opens streams with `open` and asks the daemon to take them `toward`.
    #[must_use]
    pub fn new(open: impl Fn() -> Opening + Send + Sync + 'static, toward: Toward) -> Self {
        Self {
            open: Box::new(open),
            toward,
        }
    }
}

impl Dial for Daemon {
    fn dial<'a>(
        &'a self,
        grant: &'a [u8],
        publish: Option<moq_net::origin::Consumer>,
        subscribe: Option<moq_net::origin::Producer>,
        timeout: Duration,
    ) -> Dialling<'a> {
        Box::pin(async move {
            n0_future::time::timeout(timeout, async {
                let (reader, mut writer) = (self.open)()
                    .await
                    .map_err(|_| AdmissionError::Unavailable)?;
                write_route(&mut writer, &self.toward)
                    .await
                    .map_err(|_| AdmissionError::Unavailable)?;
                let session = Session::new(reader, writer, End::Dialler);
                start(session, grant, publish, subscribe).await
            })
            .await
            .map_err(|_| AdmissionError::Unavailable)?
        })
    }

    fn redials(&self) -> bool {
        true
    }
}

/// How long the daemon waits to reach another device's relay for a link before the session is
/// told it was not reached, and dials again.
pub const REACH: Duration = Duration::from_secs(4);

/// What takes a desktop's link into the device's own live session for a workspace: the daemon,
/// which holds that live session.
pub trait Calls: Send + Sync {
    /// Carry `session`, a link the desktop opened for `workspace`, until it ends.
    fn carry<'a>(
        &'a self,
        workspace: String,
        session: Session,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

/// Carry one link a session opened on its daemon: read where it goes ([`Toward`]), then serve it
/// in `relay` as `own`'s session, or dial the relay it names from `endpoint`, this device's own,
/// and [`splice`] the two until either ends.
///
/// # Errors
/// The link named no route, or the relay it names was not reached within [`REACH`]: the link is
/// closed, which the session's dial reads as unavailable, and dials again.
pub async fn carry<R, W>(
    mut reader: R,
    writer: W,
    relay: &crate::relay::Relay,
    endpoint: &iroh::Endpoint,
    own: [u8; 32],
    calls: Option<&dyn Calls>,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let toward = read_route(&mut reader).await?;
    let session = Session::new(reader, writer, End::Listener);
    match toward {
        Toward::Call { workspace } => match calls {
            Some(calls) => calls.carry(workspace, session).await,
            None => session.close(0, "this device holds no live session"),
        },
        Toward::Relay(addr) if *addr.id.as_bytes() != own => {
            let reached =
                n0_future::time::timeout(REACH, endpoint.connect(addr, crate::native::ALPN)).await;
            let connection = match reached {
                Ok(Ok(connection)) => connection,
                Ok(Err(error)) => {
                    session.close(0, "the relay was not reached");
                    return Err(std::io::Error::other(format!(
                        "the relay the link names was not reached: {error}"
                    )));
                }
                Err(_) => {
                    session.close(0, "the relay was not reached");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "the relay the link names did not answer in time",
                    ));
                }
            };
            let onward = crate::native::Session::new(connection);
            // The relay this link reaches may open what it serves the session.
            onward.admitted();
            splice(session, onward).await;
        }
        _ => relay.serve_session(session, crate::relay::Peer(own)).await,
    }
    Ok(())
}

/// Carry `link`, a session's link, over `onward`, the daemon's connection to another device's
/// relay, until either ends: every stream one end opens is opened at the other and copied both
/// ways, each datagram is passed on, and a close is passed on with its code, so the relay's
/// answer reaches the session as the relay gave it. A stream the session sends in an order is
/// sent onward in the same one.
pub async fn splice(link: Session, onward: crate::native::Session) {
    use moq_net::transport::poll::Session as _;

    let tasks: Arc<Mutex<Vec<Task>>> = Arc::default();
    let streams = {
        let (link, onward, tasks) = (link.clone(), onward.clone(), Arc::clone(&tasks));
        async move {
            let (mut link_bi, mut link_uni) = (link.clone(), link.clone());
            let (mut onward_bi, mut onward_uni) = (onward.clone(), onward.clone());
            loop {
                tokio::select! {
                    accepted = link_bi.accept_bi() => {
                        let Ok((send, recv)) = accepted else { break };
                        let Ok((onward_send, onward_recv)) = onward.clone().open_bi().await else { break };
                        let mut held = tasks.lock_recover();
                        held.push(Task::spawn(copy_out(recv, onward_send)));
                        held.push(Task::spawn(copy(onward_recv, send, |_| 0)));
                    }
                    accepted = link_uni.accept_uni() => {
                        let Ok(recv) = accepted else { break };
                        let Ok(onward_send) = onward.clone().open_uni().await else { break };
                        tasks.lock_recover().push(Task::spawn(copy_out(recv, onward_send)));
                    }
                    accepted = onward_bi.accept_bi() => {
                        let Ok((send, recv)) = accepted else { break };
                        let Ok((link_send, link_recv)) = link.clone().open_bi().await else { break };
                        let mut held = tasks.lock_recover();
                        held.push(Task::spawn(copy(recv, link_send, |_| 0)));
                        held.push(Task::spawn(copy_out(link_recv, send)));
                    }
                    accepted = onward_uni.accept_uni() => {
                        let Ok(recv) = accepted else { break };
                        let Ok(link_send) = link.clone().open_uni().await else { break };
                        tasks.lock_recover().push(Task::spawn(copy(recv, link_send, |_| 0)));
                    }
                }
                tasks.lock_recover().retain(|task| !task.is_finished());
            }
        }
    };
    let datagrams_out = {
        let (mut link, mut onward) = (link.clone(), onward.clone());
        async move {
            while let Ok(datagram) = link.recv_datagram().await {
                let _ = onward.send_datagram(&datagram);
            }
        }
    };
    let datagrams_in = {
        let (mut link, mut onward) = (link.clone(), onward.clone());
        async move {
            while let Ok(datagram) = onward.recv_datagram().await {
                let _ = link.send_datagram(&datagram);
            }
        }
    };
    let (mut link_closed, mut onward_closed) = (link.clone(), onward.clone());
    tokio::select! {
        () = streams => {}
        () = datagrams_out => {}
        () = datagrams_in => {}
        why = onward_closed.closed() => {
            let (code, reason) = wt::Error::session_error(&why).unwrap_or((0, String::new()));
            poll::Session::close(&mut link.clone(), code, &reason);
        }
        why = link_closed.closed() => {
            let (code, reason) = wt::Error::session_error(&why).unwrap_or((0, String::new()));
            poll::Session::close(&mut onward.clone(), code, &reason);
        }
    }
    poll::Session::close(&mut link.clone(), 0, "");
    poll::Session::close(&mut onward.clone(), 0, "");
    drop(tasks);
}

/// Copy one stream from the session's link onward, in the order the session sends it in.
async fn copy_out<S>(from: Incoming, to: S)
where
    S: moq_net::transport::poll::SendStream,
{
    copy(from, to, Incoming::priority).await;
}

/// Copy one stream from `from` to `to` until it ends, passing a reset or a stop on, and sending
/// it in the order `order` reads off `from`.
async fn copy<R, S>(mut from: R, mut to: S, order: impl Fn(&R) -> u8)
where
    R: moq_net::transport::poll::RecvStream,
    S: moq_net::transport::poll::SendStream,
{
    let mut sent_in = None;
    loop {
        let priority = order(&from);
        if sent_in != Some(priority) {
            poll::SendStream::set_priority(&mut to, priority);
            sent_in = Some(priority);
        }
        tokio::select! {
            read = from.read_chunk(CHUNK) => match read {
                Ok(Some(chunk)) => {
                    if to.write_chunk(chunk).await.is_err() {
                        poll::RecvStream::stop(&mut from, 0);
                        return;
                    }
                }
                Ok(None) => {
                    let _ = poll::SendStream::finish(&mut to);
                    let _ = to.closed().await;
                    return;
                }
                Err(error) => {
                    poll::SendStream::reset(&mut to, wt::Error::stream_error(&error).unwrap_or(0));
                    return;
                }
            },
            stopped = to.closed() => {
                let code = stopped.err().and_then(|error| wt::Error::stream_error(&error)).unwrap_or(0);
                poll::RecvStream::stop(&mut from, code);
                return;
            }
        }
    }
}
