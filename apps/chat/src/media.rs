//! Real-time media transport for calls.
//!
//! Voice, camera and screen-share frames travel as a MoQ-style publish/subscribe
//! of timestamped media objects, carried by **iroh QUIC** for direct P2P (a
//! host relay is selected above the mesh threshold — see [`crate::model::CallSession`]).
//! Microphone capture/playback and the Opus audio codec go through **GStreamer**
//! elements ([`crate::audio`]); camera + screen video use the in-tree **openh264**
//! codec ([`crate::video`]); screen capture is requested from the
//! **xdg-desktop-portal ScreenCast** API via ashpd. None of ALSA / PulseAudio /
//! ffmpeg / the `opus` crate are used — only GStreamer, the portal, and openh264,
//! matching the qOS build env.
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

use std::time::Duration;

/// What kind of media a published track carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    /// Camera video.
    Video,
    /// Screen-share video (portal ScreenCast / PipeWire).
    Screen,
}

impl TrackKind {
    /// Compact wire tag.
    fn wire_tag(self) -> u8 {
        match self {
            TrackKind::Audio => 0,
            TrackKind::Video => 1,
            TrackKind::Screen => 2,
        }
    }
    fn from_wire_tag(b: u8) -> Option<Self> {
        match b {
            0 => Some(TrackKind::Audio),
            1 => Some(TrackKind::Video),
            2 => Some(TrackKind::Screen),
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
        let payload_len = u32::from_le_bytes(take(&mut p, 4)?.try_into().ok()?) as usize;
        let payload = take(&mut p, payload_len)?.to_vec();
        Some(MediaObject {
            sender,
            kind,
            seq,
            pts: Duration::from_millis(pts_ms),
            payload,
        })
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
/// The call UI never names iroh or GStreamer directly — it speaks only this
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
    fn send(&mut self, obj: MediaObject);

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
    queue: Vec<MediaObject>,
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
    fn send(&mut self, obj: MediaObject) {
        // Loopback: deliver to ourselves so the pipeline is exercised end to
        // end on one machine.
        self.queue.push(obj);
    }
    fn poll(&mut self) -> Vec<MediaObject> {
        std::mem::take(&mut self.queue)
    }
}

/// The call layer's media handle. Owns whichever transport was selected and
/// exposes a tiny surface the app drives. The heavy live engine is created
/// lazily and only when it can actually run.
pub struct MediaEngine {
    transport: Box<dyn MediaTransport + Send>,
}

impl MediaEngine {
    /// Build the best transport for this build + environment: the live iroh
    /// engine when `live-media` is enabled and the runtime probe passes,
    /// otherwise the simulator.
    pub fn new() -> Self {
        #[cfg(feature = "live-media")]
        {
            if live::environment_supports_media() {
                if let Some(t) = live::IrohTransport::try_new() {
                    return Self { transport: Box::new(t) };
                }
            }
        }
        Self { transport: Box::new(SimulatedTransport::default()) }
    }

    /// Force the simulated transport regardless of build/environment. Used by
    /// `--shots`/tests that want the deterministic local bus.
    pub fn simulated() -> Self {
        Self { transport: Box::new(SimulatedTransport::default()) }
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

impl Default for MediaEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Request a screen-share source via the xdg-desktop-portal ScreenCast API.
/// Returns the PipeWire node id to consume, or `None` when the portal is
/// unavailable (always, headless). Gated: the real ashpd call only compiles
/// under `live-media`.
pub fn request_screencast() -> Option<u32> {
    #[cfg(feature = "live-media")]
    {
        live::request_screencast()
    }
    #[cfg(not(feature = "live-media"))]
    {
        None
    }
}

// ---------------------------------------------------------------------------
// Live engine — compiled only under `live-media`.
// ---------------------------------------------------------------------------
#[cfg(feature = "live-media")]
mod live {
    use super::{MediaObject, MediaTransport, Subscription, TrackKind};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    /// ALPN for the chat call media stream. Versioned so a protocol change is a
    /// clean break rather than a silent mis-parse.
    const MEDIA_ALPN: &[u8] = b"qos-chat-media/0";

    /// Hard cap on a single wire frame (DoS guard). One audio/video access unit
    /// is far smaller than this.
    const MAX_FRAME: usize = 8 * 1024 * 1024;

    /// Runtime probe: should the live media stack be used here? Requires a real
    /// desktop session (a session bus or a Wayland display). Headless `--shots`
    /// / CI have neither, so the app transparently falls back to the simulator —
    /// while the loopback test constructs [`IrohTransport`] directly and does
    /// not depend on this probe (iroh QUIC needs no session).
    pub fn environment_supports_media() -> bool {
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
            || std::env::var_os("WAYLAND_DISPLAY").is_some()
    }

    async fn write_frame(send: &mut iroh::endpoint::SendStream, data: &[u8]) -> std::io::Result<()> {
        send.write_all(&(data.len() as u32).to_le_bytes())
            .await
            .map_err(std::io::Error::other)?;
        if !data.is_empty() {
            send.write_all(data).await.map_err(std::io::Error::other)?;
        }
        Ok(())
    }

    async fn read_frame(recv: &mut iroh::endpoint::RecvStream) -> std::io::Result<Vec<u8>> {
        let mut len = [0u8; 4];
        recv.read_exact(&mut len).await.map_err(std::io::Error::other)?;
        let n = u32::from_le_bytes(len) as usize;
        if n == 0 {
            return Ok(Vec::new());
        }
        if n > MAX_FRAME {
            return Err(std::io::Error::other(format!("media frame too large: {n}")));
        }
        let mut buf = vec![0u8; n];
        recv.read_exact(&mut buf).await.map_err(std::io::Error::other)?;
        Ok(buf)
    }

    /// The receive half: accepts an incoming QUIC connection, reads a stream of
    /// length-prefixed [`MediaObject`]s and forwards each decoded object to the
    /// transport's inbound channel, which [`IrohTransport::poll`] drains.
    #[derive(Clone)]
    struct MediaProtocol {
        inbound: mpsc::UnboundedSender<MediaObject>,
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
            let mut recv = connection.accept_uni().await?;
            loop {
                match read_frame(&mut recv).await {
                    Ok(buf) if buf.is_empty() => continue,
                    Ok(buf) => {
                        if let Some(obj) = MediaObject::from_wire(&buf) {
                            // Receiver gone (transport dropped) → stop reading.
                            if self.inbound.send(obj).is_err() {
                                break;
                            }
                        }
                    }
                    // Clean EOF or a torn stream: the peer finished sending.
                    Err(_) => break,
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
        inbound_rx: mpsc::UnboundedReceiver<MediaObject>,
        /// One outbound writer per connected peer, keyed by its ticket.
        peers: HashMap<String, mpsc::UnboundedSender<Vec<u8>>>,
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
            let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
            let (endpoint, router, ticket) = rt.block_on(async move {
                let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0).bind().await.ok()?;
                let router = iroh::protocol::Router::builder(endpoint.clone())
                    .accept(MEDIA_ALPN, MediaProtocol { inbound: inbound_tx })
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
            let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
            let endpoint = self.endpoint.clone();
            self.rt.spawn(async move {
                let conn = match endpoint.connect(addr, MEDIA_ALPN).await {
                    Ok(c) => c,
                    Err(_) => return,
                };
                let mut send = match conn.open_uni().await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                while let Some(bytes) = out_rx.recv().await {
                    if write_frame(&mut send, &bytes).await.is_err() {
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
        fn send(&mut self, obj: MediaObject) {
            let bytes = obj.to_wire();
            // Fan out to every connected peer; drop writers whose task has
            // ended (peer left / connection died).
            self.peers.retain(|_, tx| tx.send(bytes.clone()).is_ok());
        }
        fn poll(&mut self) -> Vec<MediaObject> {
            let mut out = Vec::new();
            while let Ok(obj) = self.inbound_rx.try_recv() {
                out.push(obj);
            }
            out
        }
    }

    /// Real ScreenCast request via the xdg-desktop-portal, using ashpd. Opens a
    /// ScreenCast session, asks the portal to let the user pick a monitor or
    /// window (embedded cursor), starts the cast and returns the first stream's
    /// PipeWire node id — which [`crate::video::ScreenCapture`] then consumes
    /// via `pipewiresrc`. Runs on a short-lived current-thread runtime so it can
    /// be called from the synchronous app loop. Returns `None` when there is no
    /// portal (headless), when the user cancels, or on any portal error — never
    /// panics.
    pub fn request_screencast() -> Option<u32> {
        use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
        use ashpd::desktop::PersistMode;
        use ashpd::WindowIdentifier;

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(async {
            // Bound the whole handshake so an unresponsive portal (or a user who
            // never answers the picker) can't wedge the caller forever.
            let flow = async {
                let proxy = Screencast::new().await.ok()?;
                let session = proxy.create_session().await.ok()?;
                proxy
                    .select_sources(
                        &session,
                        CursorMode::Embedded,
                        SourceType::Monitor | SourceType::Window,
                        false,
                        None,
                        PersistMode::DoNot,
                    )
                    .await
                    .ok()?;
                // No parent window handle from this surface: pass the default
                // (empty) identifier, letting the portal parent the chooser.
                let response = proxy
                    .start(&session, &WindowIdentifier::default())
                    .await
                    .ok()?
                    .response()
                    .ok()?;
                response.streams().first().map(|s| s.pipe_wire_node_id())
            };
            tokio::time::timeout(std::time::Duration::from_secs(120), flow)
                .await
                .ok()
                .flatten()
        })
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
        assert!(MediaObject::from_wire(&wire).is_none(), "truncated frame is dropped, not panicked");
    }

    #[test]
    fn simulated_transport_round_trips_objects() {
        let mut t = SimulatedTransport::default();
        assert!(!t.is_live());
        t.publish(TrackKind::Audio);
        t.subscribe(Subscription { peer: "u_aria".into(), kind: TrackKind::Audio });
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
    fn screencast_is_none_without_portal() {
        // Without the `live-media` build there is no portal support compiled
        // in, so a share request always yields no node id and never panics.
        #[cfg(not(feature = "live-media"))]
        {
            assert!(request_screencast().is_none());
        }
        // Under `live-media` the request performs a *real* xdg-desktop-portal
        // ScreenCast handshake: it pops the system source picker and blocks on
        // the user's choice. That must never run in a test / CI (it would open a
        // dialog on the developer's desktop and hang), so the live path is
        // exercised only by an actual share on a live session — not here.
    }

    #[test]
    fn publish_unpublish_is_idempotent() {
        let mut t = SimulatedTransport::default();
        t.publish(TrackKind::Video);
        t.publish(TrackKind::Video);
        t.unpublish(TrackKind::Video);
        t.unpublish(TrackKind::Video);
        // No panic, no duplicate state leakage — round-trips cleanly.
        t.send(MediaObject { sender: "x".into(), kind: TrackKind::Video, seq: 1, pts: Duration::ZERO, payload: vec![] });
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
        /// PCM tone, Opus-encodes it through GStreamer, publishes the packets
        /// over the network transport; peer B receives them off the wire and
        /// Opus-decodes them back to PCM. No mock, no loopback shortcut — the
        /// bytes cross two endpoints and both codecs run.
        #[test]
        fn two_peers_exchange_encoded_audio_over_quic() {
            let mut a = match IrohTransport::try_new() {
                Some(t) => t,
                // No usable network stack in this environment: skip cleanly
                // rather than fail (the transport itself is still real).
                None => {
                    eprintln!("no iroh endpoint could bind; skipping live loopback");
                    return;
                }
            };
            let mut b = IrohTransport::try_new().expect("second endpoint binds once the first did");

            let ticket_b = b.local_ticket().expect("B publishes a real ticket");
            // A real serialized iroh EndpointAddr (id + dialable addresses),
            // never the simulator's placeholder.
            assert!(ticket_b != "sim://local", "the live engine must not hand out the simulator ticket");
            assert!(
                ticket_b.starts_with('{') && ticket_b.contains("\"id\"") && ticket_b.contains("\"addrs\""),
                "ticket is a serialized EndpointAddr, got: {ticket_b}"
            );

            // A dials B so A→B media can flow, and publishes its audio track.
            assert!(a.connect(&ticket_b), "A dials B's real ticket");
            a.publish(TrackKind::Audio);
            b.subscribe(Subscription { peer: "u_a".into(), kind: TrackKind::Audio });

            // Real encode: a 40 ms 48 kHz mono tone → two 20 ms Opus packets.
            let mut enc = OpusEncoder::new().expect("opusenc pipeline builds");
            let pcm = crate::audio::test_tone(48_000, 1, 40);
            let packets = enc.encode(&pcm);
            assert!(!packets.is_empty(), "GStreamer opusenc produced encoded audio");
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
            assert!(received.iter().all(|o| o.kind == TrackKind::Audio && o.sender == "u_a"));
            // The bytes that crossed the wire are exactly the encoded packets.
            for (got, sent) in received.iter().zip(packets.iter()) {
                assert_eq!(&got.payload, sent, "encoded Opus bytes survived the QUIC round-trip");
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
