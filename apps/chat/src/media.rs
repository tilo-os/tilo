//! Real-time media transport for calls.
//!
//! Voice, camera and screen-share frames travel as a MoQ-style publish/subscribe
//! of timestamped media objects, carried by **iroh QUIC** for direct P2P (a
//! host relay is selected above the mesh threshold — see [`crate::model::CallSession`]).
//! Microphone capture/playback and the Opus audio codec go through **`GStreamer`**
//! elements ([`crate::audio`]); camera + screen video use the shared media stack
//! (`tilo-callstream` over `media`'s `GStreamer` real-time encoder, in the codec
//! the call negotiated — see [`crate::video`]); screen capture is requested
//! from the host's screen-capture service (natively the
//! **xdg-desktop-portal `ScreenCast`** API, see `ui::capture`). None of ALSA /
//! `PulseAudio` / ffmpeg / the `opus` crate are used — only `GStreamer` and
//! the portal, matching the Tilo build env.
//!
//! ## Real vs. gated
//! The *transport contract* ([`MediaTransport`]) and a *simulated*
//! implementation ([`SimulatedTransport`]) are always compiled; the *real* iroh
//! transport ([`live::IrohTransport`]) is gated behind the `live-media` cargo
//! feature. The live transport binds a genuine iroh endpoint, dials peers by the
//! ticket they publish into the shared voice map, and streams
//! length-prefixed [`MediaObject`]s over QUIC — proven by a real two-peer
//! loopback test (see the `tests` module) that moves encoded Opus audio between
//! two in-process endpoints over the network stack, not a mock.
//!
//! Live end-to-end *capture* still needs audio/camera hardware and (for
//! screen-share) a desktop portal — none of which exist headless — so the
//! capture/playback pipelines gate cleanly and yield the honest "no device"
//! state when absent. The transport + codec contract does not: it runs fully
//! headless.
//!
//! [`MediaEngine::new`] picks the live transport when the feature is on and the
//! environment is a real session, otherwise the simulated one — reported via
//! [`MediaEngine::is_live`].

use std::collections::VecDeque;
use std::time::Duration;

/// Hard resource bounds for media objects. These are deliberately shared by
/// the simulator and QUIC transport so a test-only route cannot hide a queue
/// or parser that production would reject.
pub const MAX_MEDIA_PAYLOAD: usize = 2 * 1024 * 1024;
pub const MAX_MEDIA_SENDER_BYTES: usize = 256;
pub const MAX_MEDIA_QUEUE: usize = 128;
pub const MAX_MEDIA_POLL: usize = 64;

/// What kind of media a published track carries.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    /// Camera video.
    Video,
    /// Screen-share video (portal `ScreenCast` / `PipeWire`).
    Screen,
    /// Camera video in a codec other than H.264, in the shared stack's
    /// codec-tagged `SurfaceFrame` framing. H.264 camera video stays on
    /// [`Self::Video`] as raw access units, which is what peers that predate
    /// this track understand; a sender uses this track only when every
    /// member announced it decodes the codec.
    CameraFramed,
}

impl TrackKind {
    /// Compact wire tag.
    fn wire_tag(self) -> u8 {
        match self {
            TrackKind::Audio => 0,
            TrackKind::Video => 1,
            TrackKind::Screen => 2,
            TrackKind::CameraFramed => 3,
        }
    }
    fn from_wire_tag(b: u8) -> Option<Self> {
        match b {
            0 => Some(TrackKind::Audio),
            1 => Some(TrackKind::Video),
            2 => Some(TrackKind::Screen),
            3 => Some(TrackKind::CameraFramed),
            _ => None,
        }
    }
}

/// One MoQ-style media object: a self-describing, timestamped chunk of one
/// track. Objects are the unit of publish/subscribe; a subscriber reassembles
/// them into a playable stream. Kept transport-agnostic so the same object
/// flows over iroh QUIC, a relay, or the in-process simulated bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaObject {
    /// Publisher (user) id.
    pub sender: String,
    pub kind: TrackKind,
    /// Monotonic sequence number within the track.
    pub seq: u64,
    /// Presentation timestamp.
    pub pts: Duration,
    /// Encoded payload (an Opus packet for audio, an H.264 access unit for
    /// video/screen).
    pub payload: Vec<u8>,
}

impl MediaObject {
    /// Verify an object before allocating, queuing, or putting it on the wire.
    /// An invalid sender/payload is rejected rather than truncated by a cast.
    pub fn is_valid(&self) -> bool {
        let sender = self.sender.as_bytes();
        !sender.is_empty()
            && sender.len() <= MAX_MEDIA_SENDER_BYTES
            && self.payload.len() <= MAX_MEDIA_PAYLOAD
            && self.pts.as_millis() <= u128::from(u64::MAX)
    }

    /// Checked serialization. The legacy `to_wire` convenience is retained
    /// for trusted tests; all transports use this fallible form at the trust
    /// boundary.
    pub fn try_to_wire(&self) -> Option<Vec<u8>> {
        if !self.is_valid() {
            return None;
        }
        Some(self.to_wire())
    }

    /// Serialize to the framed wire form used on the QUIC stream:
    /// `[kind:u8][seq:u64 LE][pts_ms:u64 LE][sender_len:u16 LE][sender][payload_len:u32 LE][payload]`.
    /// Self-describing so a receiver reassembles the exact object with no side
    /// channel.
    pub fn to_wire(&self) -> Vec<u8> {
        let sender = self.sender.as_bytes();
        let mut out = Vec::with_capacity(1 + 8 + 8 + 2 + sender.len() + 4 + self.payload.len());
        out.push(self.kind.wire_tag());
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&(self.pts.as_millis() as u64).to_le_bytes());
        out.extend_from_slice(&(sender.len() as u16).to_le_bytes());
        out.extend_from_slice(sender);
        out.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.payload);
        out
    }

    /// Parse an object from its framed wire form. Returns `None` on any
    /// truncation or an unknown track tag, so a corrupt frame is dropped rather
    /// than panicking the receive loop.
    pub fn from_wire(buf: &[u8]) -> Option<Self> {
        let mut p = 0usize;
        let take = |p: &mut usize, n: usize| -> Option<&[u8]> {
            let s = buf.get(*p..*p + n)?;
            *p += n;
            Some(s)
        };
        let kind = TrackKind::from_wire_tag(*take(&mut p, 1)?.first()?)?;
        let seq = u64::from_le_bytes(take(&mut p, 8)?.try_into().ok()?);
        let pts_ms = u64::from_le_bytes(take(&mut p, 8)?.try_into().ok()?);
        let sender_len = u16::from_le_bytes(take(&mut p, 2)?.try_into().ok()?) as usize;
        let sender = String::from_utf8(take(&mut p, sender_len)?.to_vec()).ok()?;
        if sender_len == 0 || sender_len > MAX_MEDIA_SENDER_BYTES {
            return None;
        }
        let payload_len = u32::from_le_bytes(take(&mut p, 4)?.try_into().ok()?) as usize;
        if payload_len > MAX_MEDIA_PAYLOAD {
            return None;
        }
        let payload = take(&mut p, payload_len)?.to_vec();
        // A frame is exact: accepting trailing bytes would silently desync a
        // peer's object stream and makes parser smuggling possible.
        if p != buf.len() {
            return None;
        }
        let object = MediaObject {
            sender,
            kind,
            seq,
            pts: Duration::from_millis(pts_ms),
            payload,
        };
        object.is_valid().then_some(object)
    }
}

/// A subscription handle scoped to one track of one peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subscription {
    pub peer: String,
    pub kind: TrackKind,
}

/// The transport contract the call layer programs against. An implementation
/// connects peers, publishes the local tracks, and delivers remote objects.
/// The call UI never names iroh or `GStreamer` directly — it speaks only this
/// trait, so the simulated and live backends are interchangeable.
pub trait MediaTransport {
    /// Whether this transport actually moves bytes over a network/devices
    /// (`true` for the live iroh engine, `false` for the simulator).
    fn is_live(&self) -> bool;

    /// The local node's dialable address/ticket other peers connect to.
    /// `None` until an endpoint is bound (or always, for the simulator).
    fn local_ticket(&self) -> Option<String>;

    /// Connect to a remote peer by its ticket. Returns whether the dial was
    /// accepted/scheduled (the simulator always accepts; the live engine parses
    /// the ticket and spawns the QUIC dial).
    fn connect(&mut self, ticket: &str) -> bool;

    /// Begin publishing a local track. Idempotent per kind.
    fn publish(&mut self, kind: TrackKind);

    /// Stop publishing a local track.
    fn unpublish(&mut self, kind: TrackKind);

    /// Subscribe to a remote peer's track.
    fn subscribe(&mut self, sub: Subscription);

    /// Push a locally-produced object onto the transport (capture → wire). The
    /// live engine fans it out to every connected peer over QUIC.
    /// Try to submit one object. `false` means it was rejected (malformed,
    /// closed, or backpressured), never silently queued without a bound.
    fn send(&mut self, obj: MediaObject) -> bool;

    /// Drain any media objects that have arrived for our subscriptions.
    fn poll(&mut self) -> Vec<MediaObject>;
}

/// An in-process loopback transport: anything published is immediately
/// available to `poll`, so a single machine (and `--shots`) exercises the
/// full publish/subscribe path with simulated participants and zero network.
#[derive(Default)]
pub struct SimulatedTransport {
    published: Vec<TrackKind>,
    subs: Vec<Subscription>,
    queue: VecDeque<MediaObject>,
}

impl MediaTransport for SimulatedTransport {
    fn is_live(&self) -> bool {
        false
    }
    fn local_ticket(&self) -> Option<String> {
        Some("sim://local".into())
    }
    fn connect(&mut self, _ticket: &str) -> bool {
        true
    }
    fn publish(&mut self, kind: TrackKind) {
        if !self.published.contains(&kind) {
            self.published.push(kind);
        }
    }
    fn unpublish(&mut self, kind: TrackKind) {
        self.published.retain(|k| *k != kind);
    }
    fn subscribe(&mut self, sub: Subscription) {
        if !self.subs.contains(&sub) {
            self.subs.push(sub);
        }
    }
    fn send(&mut self, obj: MediaObject) -> bool {
        // Loopback: deliver to ourselves so the pipeline is exercised end to
        // end on one machine.
        if !obj.is_valid() || self.queue.len() >= MAX_MEDIA_QUEUE {
            return false;
        }
        self.queue.push_back(obj);
        true
    }
    fn poll(&mut self) -> Vec<MediaObject> {
        let take = self.queue.len().min(MAX_MEDIA_POLL);
        self.queue.drain(..take).collect()
    }
}

/// The call layer's media handle. Owns whichever transport was selected and
/// exposes a tiny surface the app drives. The heavy live engine is created
/// lazily and only when it can actually run.
pub struct MediaEngine {
    transport: Box<dyn MediaTransport + Send>,
    // Binding iroh may perform address discovery. Keep that work out of the
    // application's first-frame path and do it only for an explicit call.
    live_start_pending: bool,
}

impl MediaEngine {
    /// Build a non-blocking engine for application startup. When this build
    /// and environment support live media, [`Self::activate_for_call`] upgrades
    /// it at the first explicit call action.
    pub fn new() -> Self {
        Self {
            transport: Box::new(SimulatedTransport::default()),
            live_start_pending: environment_supports_media(),
        }
    }

    /// Force the simulated transport regardless of build/environment. Used by
    /// `--shots`/tests that want the deterministic local bus.
    pub fn simulated() -> Self {
        Self {
            transport: Box::new(SimulatedTransport::default()),
            live_start_pending: false,
        }
    }

    /// Upgrade to the live transport at the call boundary. A failed probe is
    /// final for this engine, avoiding retries on the frame/event thread.
    pub fn activate_for_call(&mut self) {
        if !self.live_start_pending {
            return;
        }
        self.live_start_pending = false;
        #[cfg(feature = "live-media")]
        if let Some(transport) = live::IrohTransport::try_new() {
            self.transport = Box::new(transport);
        }
    }

    pub fn is_live(&self) -> bool {
        self.transport.is_live()
    }

    /// The local node's dialable media ticket, published into the shared voice
    /// membership on join so peers can connect. `Some("sim://local")` for the
    /// simulator; the live engine returns its iroh endpoint ticket.
    pub fn local_ticket(&self) -> Option<String> {
        self.transport.local_ticket()
    }

    pub fn transport_mut(&mut self) -> &mut (dyn MediaTransport + Send) {
        &mut *self.transport
    }

    /// A human description of the active transport for diagnostics / the
    /// call's connection line.
    pub fn description(&self) -> &'static str {
        if self.is_live() {
            "iroh QUIC (P2P)"
        } else {
            "simulated (local)"
        }
    }
}

fn environment_supports_media() -> bool {
    #[cfg(feature = "live-media")]
    {
        live::environment_supports_media()
    }
    #[cfg(not(feature = "live-media"))]
    {
        false
    }
}

impl Default for MediaEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Live engine — compiled only under `live-media`.
// ---------------------------------------------------------------------------
#[cfg(feature = "live-media")]
mod live {
    use super::{
        MAX_MEDIA_PAYLOAD, MAX_MEDIA_POLL, MAX_MEDIA_QUEUE, MAX_MEDIA_SENDER_BYTES, MediaObject,
        MediaTransport, Subscription, TrackKind,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    /// ALPN for the chat call media stream. Versioned so a protocol change is a
    /// clean break rather than a silent mis-parse.
    const MEDIA_ALPN: &[u8] = b"tilo-chat-media/0";

    /// Hard cap on a single wire frame (`DoS` guard). One audio/video access unit
    /// is far smaller than this.
    const MAX_FRAME: usize = 1 + 8 + 8 + 2 + MAX_MEDIA_SENDER_BYTES + 4 + MAX_MEDIA_PAYLOAD;

    /// Runtime probe: should the live media stack be used here? Requires a real
    /// desktop session (a session bus or a Wayland display). Headless `--shots`
    /// / CI have neither, so the app transparently falls back to the simulator —
    /// while the loopback test constructs [`IrohTransport`] directly and does
    /// not depend on this probe (iroh QUIC needs no session).
    pub fn environment_supports_media() -> bool {
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
            || std::env::var_os("WAYLAND_DISPLAY").is_some()
    }

    /// Every media object on the stream: a little-endian `u32` length, then
    /// [`MediaObject::to_wire`]. An empty frame carries nothing and is skipped.
    const MEDIA_FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(MAX_FRAME);

    #[cfg(test)]
    mod wire_tests {
        use super::{MAX_FRAME, MEDIA_FRAMING};

        /// Peers on other builds read these bytes: a little-endian `u32`
        /// length, then the object; an empty frame is legal.
        #[test]
        fn media_wire_bytes_are_pinned() {
            let mut wire = Vec::new();
            MEDIA_FRAMING.write(&mut wire, b"obj").unwrap();
            MEDIA_FRAMING.write(&mut wire, b"").unwrap();
            assert_eq!(wire, [3, 0, 0, 0, b'o', b'b', b'j', 0, 0, 0, 0]);
            let mut reader = wire.as_slice();
            assert_eq!(MEDIA_FRAMING.read(&mut reader).unwrap().unwrap(), b"obj");
            assert_eq!(MEDIA_FRAMING.read(&mut reader).unwrap().unwrap(), b"");
            let oversized = u32::try_from(MAX_FRAME + 1).unwrap().to_le_bytes();
            assert!(MEDIA_FRAMING.read(&mut oversized.as_slice()).is_err());
        }
    }

    /// The receive half: accepts an incoming QUIC connection, reads a stream of
    /// length-prefixed [`MediaObject`]s and forwards each decoded object to the
    /// transport's inbound channel, which [`IrohTransport::poll`] drains.
    #[derive(Clone)]
    struct MediaProtocol {
        inbound: mpsc::Sender<MediaObject>,
    }

    impl std::fmt::Debug for MediaProtocol {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("MediaProtocol")
        }
    }

    impl iroh::protocol::ProtocolHandler for MediaProtocol {
        async fn accept(
            &self,
            connection: iroh::endpoint::Connection,
        ) -> Result<(), iroh::protocol::AcceptError> {
            let recv = connection.accept_uni().await?;
            let mut frames = wire_frame::FrameReader::new(recv, MEDIA_FRAMING);
            loop {
                match frames.read_frame_async().await {
                    Ok(Some([])) => {}
                    Ok(Some(buf)) => {
                        if let Some(obj) = MediaObject::from_wire(buf) {
                            // Bounded, non-blocking admission: never let a
                            // fast peer grow memory or make the receive task
                            // wait indefinitely behind the UI. A realtime
                            // frame dropped under pressure is preferable to a
                            // stale one arriving seconds later.
                            match self.inbound.try_send(obj) {
                                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                                // The receiver belongs to a dropped transport;
                                // continuing to read would only waste a task
                                // and hide that the operation was cancelled.
                                Err(mpsc::error::TrySendError::Closed(_)) => break,
                            }
                        }
                    }
                    // Clean EOF or a torn stream: the peer finished sending.
                    Ok(None) | Err(_) => break,
                }
            }
            Ok(())
        }
    }

    /// Real transport: an iroh endpoint carrying MoQ-style objects over QUIC.
    ///
    /// The synchronous [`MediaTransport`] surface (called from the app's render
    /// thread) is bridged to iroh's async world by an owned multi-thread Tokio
    /// runtime and channels: inbound objects arrive on a channel filled by the
    /// accept loop; outbound objects are fanned to one writer task per connected
    /// peer. Dropping the transport shuts the runtime down, aborting the tasks.
    pub struct IrohTransport {
        // Runtime kept alive for the endpoint + tasks; dropped last.
        rt: Arc<tokio::runtime::Runtime>,
        endpoint: iroh::Endpoint,
        // Router owns the accept loop; kept alive so peers can dial us.
        _router: iroh::protocol::Router,
        ticket: String,
        inbound_rx: mpsc::Receiver<MediaObject>,
        /// One outbound writer per connected peer, keyed by its ticket.
        peers: HashMap<String, mpsc::Sender<Vec<u8>>>,
        published: Vec<TrackKind>,
        subs: Vec<Subscription>,
    }

    impl IrohTransport {
        /// Bind a real iroh endpoint and start accepting media connections.
        /// Returns `None` if the endpoint can't bind (no usable network stack).
        pub fn try_new() -> Option<Self> {
            let rt = Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(2)
                    .build()
                    .ok()?,
            );
            let (inbound_tx, inbound_rx) = mpsc::channel(MAX_MEDIA_QUEUE);
            let (endpoint, router, ticket) = rt.block_on(async move {
                let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
                    .bind()
                    .await
                    .ok()?;
                let router = iroh::protocol::Router::builder(endpoint.clone())
                    .accept(
                        MEDIA_ALPN,
                        MediaProtocol {
                            inbound: inbound_tx,
                        },
                    )
                    .spawn();
                // Local (direct) addresses are available right after bind;
                // relay contact is not required for LAN / loopback peers.
                let addr = endpoint.addr();
                let ticket = serde_json::to_string(&addr).ok()?;
                Some((endpoint, router, ticket))
            })?;
            Some(Self {
                rt,
                endpoint,
                _router: router,
                ticket,
                inbound_rx,
                peers: HashMap::new(),
                published: Vec::new(),
                subs: Vec::new(),
            })
        }
    }

    impl MediaTransport for IrohTransport {
        fn is_live(&self) -> bool {
            true
        }
        fn local_ticket(&self) -> Option<String> {
            Some(self.ticket.clone())
        }
        fn connect(&mut self, ticket: &str) -> bool {
            // Don't dial ourselves, and dedupe repeat dials (this runs every
            // frame while call membership settles).
            if ticket == self.ticket || self.peers.contains_key(ticket) {
                return ticket != self.ticket;
            }
            let Ok(addr) = serde_json::from_str::<iroh::EndpointAddr>(ticket) else {
                return false;
            };
            let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(MAX_MEDIA_QUEUE);
            let endpoint = self.endpoint.clone();
            self.rt.spawn(async move {
                let Ok(conn) = endpoint.connect(addr, MEDIA_ALPN).await else {
                    return;
                };
                let Ok(mut send) = conn.open_uni().await else {
                    return;
                };
                while let Some(bytes) = out_rx.recv().await {
                    if MEDIA_FRAMING.write_async(&mut send, &bytes).await.is_err() {
                        break;
                    }
                }
                let _ = send.finish();
            });
            self.peers.insert(ticket.to_string(), out_tx);
            true
        }
        fn publish(&mut self, kind: TrackKind) {
            if !self.published.contains(&kind) {
                self.published.push(kind);
            }
        }
        fn unpublish(&mut self, kind: TrackKind) {
            self.published.retain(|k| *k != kind);
        }
        fn subscribe(&mut self, sub: Subscription) {
            if !self.subs.contains(&sub) {
                self.subs.push(sub);
            }
        }
        fn send(&mut self, obj: MediaObject) -> bool {
            let Some(bytes) = obj.try_to_wire() else {
                return false;
            };
            // Fan out to every connected peer; drop writers whose task has
            // ended (peer left / connection died). Full per-peer queues are
            // intentionally rejected instead of becoming an unbounded latent
            // playback buffer.
            let mut accepted = false;
            self.peers.retain(|_, tx| match tx.try_send(bytes.clone()) {
                Ok(()) => {
                    accepted = true;
                    true
                }
                Err(mpsc::error::TrySendError::Full(_)) => true,
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            });
            accepted
        }
        fn poll(&mut self) -> Vec<MediaObject> {
            let mut out = Vec::new();
            while out.len() < MAX_MEDIA_POLL {
                let Ok(obj) = self.inbound_rx.try_recv() else {
                    break;
                };
                out.push(obj);
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_object_wire_round_trips() {
        let obj = MediaObject {
            sender: "u_aria".into(),
            kind: TrackKind::Video,
            seq: 42,
            pts: Duration::from_millis(1234),
            payload: vec![9, 8, 7, 6, 5],
        };
        let wire = obj.to_wire();
        let back = MediaObject::from_wire(&wire).expect("decodes");
        assert_eq!(back, obj);
    }

    #[test]
    fn media_object_wire_rejects_truncation() {
        let obj = MediaObject {
            sender: "x".into(),
            kind: TrackKind::Audio,
            seq: 1,
            pts: Duration::ZERO,
            payload: vec![1, 2, 3],
        };
        let mut wire = obj.to_wire();
        wire.truncate(wire.len() - 1); // lose a payload byte
        assert!(
            MediaObject::from_wire(&wire).is_none(),
            "truncated frame is dropped, not panicked"
        );
    }

    #[test]
    fn media_object_wire_rejects_trailing_and_oversized_fields() {
        let obj = MediaObject {
            sender: "x".into(),
            kind: TrackKind::Audio,
            seq: 1,
            pts: Duration::ZERO,
            payload: vec![1],
        };
        let mut trailing = obj.to_wire();
        trailing.push(0);
        assert!(MediaObject::from_wire(&trailing).is_none());

        let too_large = MediaObject {
            sender: "x".into(),
            kind: TrackKind::Video,
            seq: 2,
            pts: Duration::ZERO,
            payload: vec![0; MAX_MEDIA_PAYLOAD + 1],
        };
        assert!(too_large.try_to_wire().is_none());
    }

    #[test]
    fn simulated_transport_round_trips_objects() {
        let mut t = SimulatedTransport::default();
        assert!(!t.is_live());
        t.publish(TrackKind::Audio);
        t.subscribe(Subscription {
            peer: "u_aria".into(),
            kind: TrackKind::Audio,
        });
        t.send(MediaObject {
            sender: "u_me".into(),
            kind: TrackKind::Audio,
            seq: 0,
            pts: Duration::from_millis(0),
            payload: vec![1, 2, 3],
        });
        let got = t.poll();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].payload, vec![1, 2, 3]);
        assert!(t.poll().is_empty(), "poll drains the queue");
    }

    #[test]
    fn simulated_transport_is_bounded_and_backpressures() {
        let mut t = SimulatedTransport::default();
        for seq in 0..MAX_MEDIA_QUEUE as u64 {
            assert!(t.send(MediaObject {
                sender: "sender".into(),
                kind: TrackKind::Audio,
                seq,
                pts: Duration::ZERO,
                payload: vec![1],
            }));
        }
        assert!(
            !t.send(MediaObject {
                sender: "sender".into(),
                kind: TrackKind::Audio,
                seq: MAX_MEDIA_QUEUE as u64,
                pts: Duration::ZERO,
                payload: vec![1],
            }),
            "a full realtime queue rejects rather than growing"
        );
        assert_eq!(t.poll().len(), MAX_MEDIA_POLL);
        assert_eq!(t.poll().len(), MAX_MEDIA_QUEUE - MAX_MEDIA_POLL);
    }

    #[test]
    fn engine_defaults_to_simulated_without_live_feature() {
        let e = MediaEngine::new();
        // Without `live-media`, or without a supporting environment, the
        // engine must be the simulator and never the live transport.
        if !cfg!(feature = "live-media") {
            assert!(!e.is_live());
            assert_eq!(e.description(), "simulated (local)");
        }
    }

    #[test]
    fn engine_construction_never_binds_live_transport() {
        // The app constructor runs before the window's first frame. Even when
        // a desktop environment is advertised it must remain a pure local
        // allocation; endpoint setup belongs to `activate_for_call`.
        let e = MediaEngine::new();
        assert!(!e.is_live());
        assert_eq!(e.description(), "simulated (local)");
    }

    #[test]
    fn publish_unpublish_is_idempotent() {
        let mut t = SimulatedTransport::default();
        t.publish(TrackKind::Video);
        t.publish(TrackKind::Video);
        t.unpublish(TrackKind::Video);
        t.unpublish(TrackKind::Video);
        // No panic, no duplicate state leakage — round-trips cleanly.
        t.send(MediaObject {
            sender: "x".into(),
            kind: TrackKind::Video,
            seq: 1,
            pts: Duration::ZERO,
            payload: vec![],
        });
        assert_eq!(t.poll().len(), 1);
    }

    // -- Real transport + codec loopback (only compiled with `live-media`) ----
    #[cfg(feature = "live-media")]
    mod live_loopback {
        use super::super::live::IrohTransport;
        use super::*;
        use crate::audio::{OpusDecoder, OpusEncoder};
        use std::time::{Duration, Instant};

        /// Spin the transport's poll (non-blocking) until `want` objects have
        /// arrived or the deadline passes. The endpoints do their real QUIC
        /// handshake + stream I/O on the transport's own runtime threads, so a
        /// short sleep between polls lets them make progress.
        fn drain_until(t: &mut IrohTransport, want: usize, timeout: Duration) -> Vec<MediaObject> {
            let deadline = Instant::now() + timeout;
            let mut got = Vec::new();
            while got.len() < want && Instant::now() < deadline {
                got.extend(t.poll());
                if got.len() < want {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            got
        }

        /// The headline contract: two real iroh endpoints, bound in-process,
        /// exchange genuinely *encoded* audio over QUIC. Peer A synthesises a
        /// PCM tone, Opus-encodes it through `GStreamer`, publishes the packets
        /// over the network transport; peer B receives them off the wire and
        /// Opus-decodes them back to PCM. No mock, no loopback shortcut — the
        /// bytes cross two endpoints and both codecs run.
        #[test]
        #[ignore = "requires live network access and GStreamer Opus elements; see docs/testing-live.md"]
        fn two_peers_exchange_encoded_audio_over_quic() {
            // This explicit live lane must fail honestly when its required
            // network stack is unavailable; an ignored test should never turn
            // into a reported pass through another runtime self-skip.
            let mut a = IrohTransport::try_new().expect("first live iroh endpoint binds");
            let mut b = IrohTransport::try_new().expect("second endpoint binds once the first did");

            let ticket_b = b.local_ticket().expect("B publishes a real ticket");
            // A real serialized iroh EndpointAddr (id + dialable addresses),
            // never the simulator's placeholder.
            assert!(
                ticket_b != "sim://local",
                "the live engine must not hand out the simulator ticket"
            );
            assert!(
                ticket_b.starts_with('{')
                    && ticket_b.contains("\"id\"")
                    && ticket_b.contains("\"addrs\""),
                "ticket is a serialized EndpointAddr, got: {ticket_b}"
            );

            // A dials B so A→B media can flow, and publishes its audio track.
            assert!(a.connect(&ticket_b), "A dials B's real ticket");
            a.publish(TrackKind::Audio);
            b.subscribe(Subscription {
                peer: "u_a".into(),
                kind: TrackKind::Audio,
            });

            // Real encode: a 40 ms 48 kHz mono tone → two 20 ms Opus packets.
            let mut enc = OpusEncoder::new().expect("opusenc pipeline builds");
            let pcm = crate::audio::test_tone(48_000, 1, 40);
            let packets = enc.encode(&pcm);
            assert!(
                !packets.is_empty(),
                "GStreamer opusenc produced encoded audio"
            );
            assert!(
                packets.iter().all(|p| !p.is_empty()),
                "every Opus packet carries bytes"
            );

            // Publish each encoded packet as a MediaObject over the QUIC link.
            for (i, pkt) in packets.iter().enumerate() {
                a.send(MediaObject {
                    sender: "u_a".into(),
                    kind: TrackKind::Audio,
                    seq: i as u64,
                    pts: Duration::from_millis(i as u64 * 20),
                    payload: pkt.clone(),
                });
            }

            // B receives the encoded audio off the wire.
            let received = drain_until(&mut b, packets.len(), Duration::from_secs(15));
            assert_eq!(
                received.len(),
                packets.len(),
                "B received every Opus packet A sent over QUIC"
            );
            assert!(
                received
                    .iter()
                    .all(|o| o.kind == TrackKind::Audio && o.sender == "u_a")
            );
            // The bytes that crossed the wire are exactly the encoded packets.
            for (got, sent) in received.iter().zip(packets.iter()) {
                assert_eq!(
                    &got.payload, sent,
                    "encoded Opus bytes survived the QUIC round-trip"
                );
            }

            // Real decode on B: the received Opus packets play back as PCM.
            let mut dec = OpusDecoder::new().expect("opusdec pipeline builds");
            let mut decoded_samples = 0usize;
            for o in &received {
                decoded_samples += dec.decode(&o.payload).len();
            }
            assert!(
                decoded_samples > 0,
                "B decoded the received Opus back into audible PCM samples"
            );
        }
    }
}
