//! Legacy custom-Iroh media protocol and simulator for replay/diagnostic tests.
//!
//! `tilo-chat-media/0` is not MOQT and cannot provide governed-call readiness.
//! Native Chat calls use the shell's typed media service; this module is never
//! selected as a compatibility fallback for denied or unavailable native calls.

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
    /// The live endpoint while it binds on the app runtime. It stands in for
    /// `transport` until the bound endpoint replaces it.
    #[cfg(feature = "live-media")]
    binding: Option<live::PendingBind>,
    /// Set when the bound live transport replaced the stand-in, until
    /// [`Self::take_bound`] observes it.
    bound: bool,
}

impl MediaEngine {
    /// Build a non-blocking engine for application startup. When this build
    /// and environment support live media, [`Self::activate_for_call`] upgrades
    /// it at the first explicit call action.
    pub fn new() -> Self {
        Self {
            live_start_pending: environment_supports_media(),
            ..Self::simulated()
        }
    }

    #[cfg(test)]
    pub(crate) fn latch_bound_for_test(&mut self) {
        self.bound = true;
    }

    /// Force the simulated transport regardless of build/environment. Used by
    /// `--shots`/tests that want the deterministic local bus.
    pub fn simulated() -> Self {
        Self {
            transport: Box::new(SimulatedTransport::default()),
            live_start_pending: false,
            #[cfg(feature = "live-media")]
            binding: None,
            bound: false,
        }
    }

    /// Upgrade to the live transport at the call boundary. The endpoint binds
    /// on the app runtime, never on the caller's thread: until it is ready the
    /// engine is live but has no ticket, and dials/sends are refused;
    /// publications and subscriptions made meanwhile carry over. If binding
    /// fails the engine stays on the simulator; a failed probe is final for
    /// this engine. Call it under the app's wake scope so the bound endpoint
    /// wakes the event loop.
    pub fn activate_for_call(&mut self) {
        if !self.live_start_pending {
            return;
        }
        self.live_start_pending = false;
        #[cfg(feature = "live-media")]
        {
            self.binding = Some(live::PendingBind::start());
        }
    }

    fn active(&self) -> &dyn MediaTransport {
        #[cfg(feature = "live-media")]
        if let Some(binding) = &self.binding {
            return binding;
        }
        &*self.transport
    }

    pub fn is_live(&self) -> bool {
        self.active().is_live()
    }

    /// The local node's dialable media ticket, published into the shared voice
    /// membership on join so peers can connect. `Some("sim://local")` for the
    /// simulator; the live engine returns its iroh endpoint ticket, or `None`
    /// while the endpoint is still binding.
    pub fn local_ticket(&self) -> Option<String> {
        self.active().local_ticket()
    }

    /// Adopt the live endpoint if it finished binding. Returns whether a
    /// newly bound transport is waiting for [`Self::take_bound`].
    pub fn poll_binding(&mut self) -> bool {
        #[cfg(feature = "live-media")]
        if let Some(outcome) = self.binding.as_mut().and_then(live::PendingBind::try_take) {
            self.binding = None;
            match outcome {
                live::BindOutcome::Bound(live) => {
                    self.transport = live;
                    self.bound = true;
                    return true;
                }
                // The simulator stays in place.
                live::BindOutcome::Failed => {}
            }
        }
        false
    }

    /// Whether the live transport became ready since the last call. A call
    /// joined while it was binding announces its ticket then.
    pub fn take_bound(&mut self) -> bool {
        self.poll_binding();
        std::mem::take(&mut self.bound)
    }

    pub fn transport_mut(&mut self) -> &mut (dyn MediaTransport + Send) {
        self.poll_binding();
        #[cfg(feature = "live-media")]
        if let Some(binding) = self.binding.as_mut() {
            return binding;
        }
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
// Live engine — compiled only under `live-media`: a peer-to-peer endpoint
// (iroh QUIC on the desktop). A browser page cannot bind one and carries calls
// over its own MoQ media instead.
// ---------------------------------------------------------------------------
#[cfg(feature = "live-media")]
mod live {
    use super::{
        MAX_MEDIA_PAYLOAD, MAX_MEDIA_POLL, MAX_MEDIA_QUEUE, MAX_MEDIA_SENDER_BYTES, MediaObject,
        MediaTransport, Subscription, TrackKind,
    };
    use std::collections::HashMap;
    use tilo_net::p2p;

    /// ALPN for the chat call media stream. Versioned so a protocol change is a
    /// clean break rather than a silent mis-parse.
    const MEDIA_ALPN: &[u8] = b"tilo-chat-media/0";

    /// Hard cap on a single wire frame (`DoS` guard). One audio/video access unit
    /// is far smaller than this. Every media object on the stream is one
    /// frame: a little-endian `u32` length, then [`MediaObject::to_wire`].
    const MAX_FRAME: usize = 1 + 8 + 8 + 2 + MAX_MEDIA_SENDER_BYTES + 4 + MAX_MEDIA_PAYLOAD;

    /// Runtime probe: should the live media stack be used here? Requires a real
    /// desktop session (a session bus or a Wayland display). Headless `--shots`
    /// / CI have neither, so the app transparently falls back to the simulator —
    /// while the loopback test constructs [`IrohTransport`] directly and does
    /// not depend on this probe (iroh QUIC needs no session).
    pub fn environment_supports_media() -> bool {
        tilo_paths::session::has_desktop_session()
    }

    /// Real transport: a peer-to-peer endpoint carrying MoQ-style objects.
    ///
    /// The synchronous [`MediaTransport`] surface (called from the app's UI
    /// thread) is bridged to the endpoint's async world on the runtime it was
    /// bound on: inbound objects arrive in the endpoint's bounded queue;
    /// outbound objects are fanned to one connection per peer. Dropping the
    /// transport closes the endpoint and its connections.
    pub struct IrohTransport {
        endpoint: p2p::Endpoint<MediaObject>,
        /// One outbound connection per peer, keyed by its ticket.
        peers: HashMap<String, p2p::Peer>,
        published: Vec<TrackKind>,
        subs: Vec<Subscription>,
    }

    impl IrohTransport {
        /// Bind a real endpoint and start accepting media connections.
        /// Returns `None` if the endpoint can't bind (no usable network stack,
        /// or a host that cannot accept peers).
        ///
        /// Must run on the app runtime: the endpoint's tasks run on the
        /// runtime it is bound on.
        pub async fn bind() -> Option<Self> {
            let endpoint = p2p::Endpoint::bind(
                MEDIA_ALPN,
                MAX_FRAME,
                MAX_MEDIA_QUEUE,
                MediaObject::from_wire,
            )
            .await
            .ok()?;
            Some(Self {
                endpoint,
                peers: HashMap::new(),
                published: Vec::new(),
                subs: Vec::new(),
            })
        }

        /// [`Self::bind`], waiting for the result, for the loopback test; the
        /// app binds through [`PendingBind`] and never waits.
        #[cfg(test)]
        pub fn try_new() -> Option<Self> {
            ui::rt::app().block_on(Self::bind()).ok().flatten()
        }
    }

    /// The live endpoint while it binds on the app runtime.
    ///
    /// It stands in for the transport meanwhile: live, but with no ticket to
    /// hand out, refusing dials and sends. Publications and subscriptions are
    /// recorded and carried over to the bound endpoint. The bind's completion
    /// wakes the event loop of the wake scope it was started under.
    pub struct PendingBind {
        request: ui::LatestRequest<Option<IrohTransport>>,
        published: Vec<TrackKind>,
        subs: Vec<Subscription>,
    }

    impl PendingBind {
        pub fn start() -> Self {
            Self::spawn(IrohTransport::bind())
        }

        pub(super) fn spawn(
            bind: impl std::future::Future<Output = Option<IrohTransport>> + Send + 'static,
        ) -> Self {
            let mut request = ui::LatestRequest::new();
            request.spawn(bind);
            Self {
                request,
                published: Vec::new(),
                subs: Vec::new(),
            }
        }

        /// The bind's outcome once it finished; `None` while it is running.
        /// A bound transport carries the recorded publications and
        /// subscriptions over.
        pub fn try_take(&mut self) -> Option<BindOutcome> {
            let outcome = match self.request.try_take() {
                Some(outcome) => outcome,
                // No result and nothing running: the bind was refused or
                // its task stopped without reporting.
                None if !self.request.is_pending() => None,
                None => return None,
            };
            let Some(mut live) = outcome else {
                return Some(BindOutcome::Failed);
            };
            for kind in self.published.drain(..) {
                live.publish(kind);
            }
            for sub in self.subs.drain(..) {
                live.subscribe(sub);
            }
            Some(BindOutcome::Bound(Box::new(live)))
        }
    }

    /// How a [`PendingBind`] ended.
    pub enum BindOutcome {
        Bound(Box<IrohTransport>),
        /// The endpoint could not bind (no usable network stack).
        Failed,
    }

    impl MediaTransport for PendingBind {
        fn is_live(&self) -> bool {
            true
        }
        fn local_ticket(&self) -> Option<String> {
            None
        }
        fn connect(&mut self, _ticket: &str) -> bool {
            false
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
        fn send(&mut self, _obj: MediaObject) -> bool {
            false
        }
        fn poll(&mut self) -> Vec<MediaObject> {
            Vec::new()
        }
    }

    impl MediaTransport for IrohTransport {
        fn is_live(&self) -> bool {
            true
        }
        fn local_ticket(&self) -> Option<String> {
            Some(self.endpoint.ticket().to_owned())
        }
        fn connect(&mut self, ticket: &str) -> bool {
            // Don't dial ourselves, and dedupe repeat dials (this runs every
            // frame while call membership settles).
            let own = ticket == self.endpoint.ticket();
            if own || self.peers.contains_key(ticket) {
                return !own;
            }
            let Ok(peer) = self.endpoint.connect(ticket, MAX_MEDIA_QUEUE) else {
                return false;
            };
            self.peers.insert(ticket.to_string(), peer);
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
            // Fan out to every connected peer; drop connections that have
            // ended (peer left / connection died). Full per-peer queues are
            // intentionally rejected instead of becoming an unbounded latent
            // playback buffer.
            let mut accepted = false;
            self.peers
                .retain(|_, peer| match peer.try_send(bytes.clone()) {
                    p2p::Sent::Queued => {
                        accepted = true;
                        true
                    }
                    p2p::Sent::Full => true,
                    p2p::Sent::Closed => false,
                });
            accepted
        }
        fn poll(&mut self) -> Vec<MediaObject> {
            let mut out = Vec::new();
            while out.len() < MAX_MEDIA_POLL {
                let Some(obj) = self.endpoint.try_recv() else {
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
    fn media_object_wire_round_trips_and_rejects_malformed_frames() {
        // Phase: media_object_wire_round_trips.
        {
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
        // Phase: media_object_wire_rejects_truncation.
        {
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
        // Phase: media_object_wire_rejects_trailing_and_oversized_fields.
        {
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
    }

    #[test]
    fn simulated_media_path_is_the_default_and_is_bounded_and_idempotent() {
        // Phase: engine_construction_never_binds_live_transport.
        {
            // The app constructor runs before the window's first frame. Even when
            // a desktop environment is advertised it must remain a pure local
            // allocation; endpoint setup belongs to `activate_for_call`.
            let e = MediaEngine::new();
            assert!(!e.is_live());
            assert_eq!(e.description(), "simulated (local)");
        }
        // Phase: simulated_transport_round_trips_objects.
        {
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
        // Phase: simulated_transport_is_bounded_and_backpressures.
        {
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
        // Phase: publish_unpublish_is_idempotent.
        {
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
    }

    /// Binding runs on the app runtime and wakes the event loop; until then
    /// the engine is live without a ticket, and a failed bind falls back to
    /// the simulator instead of leaving calls on a dead stand-in.
    #[cfg(feature = "live-media")]
    #[test]
    fn live_engine_binds_in_the_background_and_falls_back_when_binding_fails() {
        let mut scope = ui::WakeScope::new();
        let (woke, wakes) = std::sync::mpsc::channel();
        scope
            .bind(move |_| {
                let _ = woke.send(());
            })
            .unwrap();
        let _entry = scope.enter();
        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        let mut engine = MediaEngine {
            binding: Some(live::PendingBind::spawn(async move {
                let _ = gate.await;
                None
            })),
            ..MediaEngine::simulated()
        };
        assert!(engine.is_live());
        assert_eq!(engine.local_ticket(), None);
        assert!(!engine.transport_mut().connect("peer-ticket"));
        engine.transport_mut().publish(TrackKind::Audio);
        assert!(!engine.poll_binding(), "still binding");

        release.send(()).unwrap();
        wakes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        assert!(!engine.take_bound(), "a failed bind announces nothing");
        assert!(!engine.is_live());
        assert_eq!(engine.local_ticket().as_deref(), Some("sim://local"));
    }

    /// `poll_binding` used to return the latched `bound` flag forever, so
    /// Chat's `update` stayed true after the endpoint came up and the
    /// native runner committed a buffer at 144 Hz (~90% of a core).
    #[test]
    fn poll_binding_is_edge_triggered_after_the_endpoint_is_ready() {
        let mut engine = MediaEngine::simulated();
        engine.bound = true;
        assert!(
            !engine.poll_binding(),
            "an already-bound endpoint is not a change"
        );
        assert!(engine.take_bound(), "the latch is still delivered once");
        assert!(!engine.take_bound());
        assert!(!engine.poll_binding());
    }

    // -- Real transport + codec loopback (only compiled with `live-media`) ----
    #[cfg(all(feature = "live-media", unix))]
    mod live_loopback {
        use super::super::live::IrohTransport;
        use super::*;
        use media::voice::gst::{OpusDecoder, OpusEncoder, test_tone};
        use ui::time::Duration;

        /// Spin the transport's poll (non-blocking) until `want` objects have
        /// arrived or the deadline passes. The endpoints do their real QUIC
        /// handshake + stream I/O on the app runtime's thread, so a
        /// short sleep between polls lets them make progress.
        fn drain_until(t: &mut IrohTransport, want: usize, timeout: Duration) -> Vec<MediaObject> {
            let deadline = ui::time::now() + timeout;
            let mut got = Vec::new();
            while got.len() < want && ui::time::now() < deadline {
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
            let pcm = test_tone(48_000, 1, 40);
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
            let received = drain_until(&mut b, packets.len(), tilo_test_env::hang_bound());
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
