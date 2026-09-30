//! Legacy windows for browser viewers. The session publishes each shared
//! legacy window as `SurfaceStreamStart`/`SurfaceFrame`/`SurfaceStreamEnd`,
//! which reach this host as relay presence of the session's device. They are
//! never owner state: they are re-published on the `LIVE_V1` relay in that
//! device's namespace (`live_wire::legacy_window`), where the live grant, and
//! so the room roster, decides who receives them.
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};
use tilo_delivery_protocol::delivery_v3::live_wire::{legacy_window as wire, room_digest};
use tilo_moq::live::LiveRelay;
use tilo_peer_state::NeighbourMsg;

/// While any stream is live its index is re-sent this often, so a viewer that
/// subscribes later learns it.
const INDEX_EVERY: Duration = Duration::from_secs(1);
/// A session away from the relay this long has left: its streams end.
const DEPARTURE: Duration = Duration::from_secs(15);
/// The session is reminded who sends it input at least this often: it drops a
/// device's identity when its route goes quiet.
const HELLO_EVERY: Duration = Duration::from_secs(30);
/// Input messages waiting for the owner loop to forward.
const MAX_PENDING_INPUT: usize = 256;
/// Most bytes of one coded sequence kept for a viewer that subscribes later.
const MAX_SEQUENCE_BYTES: usize = 8 * 1024 * 1024;

struct Stream {
    listed: wire::Stream,
    record: String,
    seq: u32,
    /// The current coded sequence as frames: its start, keyframe and later
    /// frames. A static window sends nothing more, so a viewer that subscribes
    /// later is given this instead of waiting for the next keyframe.
    sequence: Vec<bytes::Bytes>,
    sequence_bytes: usize,
    /// A relay group is open: new frames join it.
    started: bool,
}

/// The session streams this host re-publishes.
pub(crate) struct LegacyWindows {
    room: String,
    streams: BTreeMap<([u8; 32], u64), Stream>,
    /// Since when each publishing session has been away from the relay.
    absent: BTreeMap<[u8; 32], Instant>,
    /// Publishers whose last sent index listed a stream.
    listed: BTreeSet<[u8; 32]>,
    index_sent: Option<Instant>,
    index_dirty: bool,
    /// `RemoteInput` for sessions, sent as this host's governor device.
    input: Vec<NeighbourMsg>,
    hello_sent: Option<Instant>,
}

/// The session's peer handle for a device key: 64-bit FNV-1a, as the session
/// folds an authenticated endpoint into `RemoteInput::peer`.
pub(crate) fn peer_handle(device: &[u8; 32]) -> u64 {
    device
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash: u64, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

impl LegacyWindows {
    pub(crate) fn new(room: &str) -> Self {
        Self {
            room: hex::encode(room_digest(room)),
            streams: BTreeMap::new(),
            absent: BTreeMap::new(),
            listed: BTreeSet::new(),
            index_sent: None,
            index_dirty: false,
            input: Vec::new(),
            hello_sent: None,
        }
    }

    /// Input a controlling viewer sent for one stream, to be forwarded to its
    /// session as `RemoteInput` of `governor` (this host's device, which the
    /// session shares its legacy windows with).
    pub(crate) fn queue_input(
        &mut self,
        request: wire::InputRequest,
        governor: &[u8; 32],
    ) -> Result<(), String> {
        let surface = self
            .streams
            .iter()
            .find(|(_, stream)| stream.listed.record == request.record)
            .map(|((_, surface), _)| *surface)
            .ok_or("no such legacy window")?;
        if self.input.len() + request.events.len() > MAX_PENDING_INPUT {
            return Err("legacy window input is backed up".into());
        }
        let peer = peer_handle(governor);
        for event in request.events {
            let event = postcard::to_allocvec(&event.for_surface(surface))
                .map_err(|error| error.to_string())?;
            self.input.push(NeighbourMsg::RemoteInput {
                peer,
                surface,
                event,
            });
        }
        Ok(())
    }

    /// The input to forward, and whether the governor's identity must go
    /// first.
    pub(crate) fn take_input(&mut self, now: Instant) -> (bool, Vec<NeighbourMsg>) {
        if self.input.is_empty() {
            return (false, Vec::new());
        }
        let hello = self
            .hello_sent
            .is_none_or(|sent| now.saturating_duration_since(sent) >= HELLO_EVERY);
        if hello {
            self.hello_sent = Some(now);
        }
        (hello, std::mem::take(&mut self.input))
    }

    /// The session devices with a live stream.
    pub(crate) fn publishers(&self) -> BTreeSet<[u8; 32]> {
        self.streams.keys().map(|(author, _)| *author).collect()
    }

    /// Whether the session device `author` is subscribed to the room's relay.
    /// A static window sends no frames, so its session leaving, not silence,
    /// ends its streams; a reconnect shorter than [`DEPARTURE`] does not
    /// (the session does not announce its streams again).
    pub(crate) fn subscribed(
        &mut self,
        relay: &LiveRelay,
        author: [u8; 32],
        subscribed: bool,
        now: Instant,
    ) {
        if subscribed {
            self.absent.remove(&author);
            return;
        }
        let since = *self.absent.entry(author).or_insert(now);
        if now.saturating_duration_since(since) >= DEPARTURE {
            self.absent.remove(&author);
            self.departed(relay, author);
        }
    }

    fn departed(&mut self, relay: &LiveRelay, author: [u8; 32]) {
        let gone = self
            .streams
            .keys()
            .filter(|(publisher, _)| *publisher == author)
            .copied()
            .collect::<Vec<_>>();
        for key in gone {
            if let Some(stream) = self.streams.remove(&key) {
                relay.end_stream(&self.room, &hex::encode(author), &stream.record);
                self.index_dirty = true;
            }
        }
    }

    /// Whether `message` is legacy-window video, which this bridge consumes.
    pub(crate) const fn carries(message: &NeighbourMsg) -> bool {
        matches!(
            message,
            NeighbourMsg::SurfaceStreamStart { .. }
                | NeighbourMsg::SurfaceFrame { .. }
                | NeighbourMsg::SurfaceStreamEnd { .. }
        )
    }

    /// One message the session device `author` published.
    pub(crate) fn observe(&mut self, relay: &LiveRelay, author: [u8; 32], message: &NeighbourMsg) {
        let publisher = hex::encode(author);
        match message {
            NeighbourMsg::SurfaceStreamStart {
                surface,
                source_peer,
                width,
                height,
                codec,
                app_id,
                ..
            } => {
                // Keyframes are found in the bitstream; browsers get only
                // H.264, which the session picks for a receiver that never
                // advertised capabilities.
                if codec != "h264" || app_id.is_empty() {
                    tracing::debug!(codec, app_id, "legacy window stream not bridged");
                    return;
                }
                let listed =
                    wire::Stream::new(*source_peer, *surface, app_id, codec, *width, *height);
                let record = hex::encode(wire::stream_record(*source_peer, *surface).digest());
                if let Some(previous) = self.streams.get(&(author, *surface))
                    && previous.listed == listed
                {
                    return;
                }
                relay.end_stream(&self.room, &publisher, &record);
                tracing::debug!(
                    app_id,
                    surface,
                    width,
                    height,
                    "legacy window stream bridged"
                );
                self.streams.insert(
                    (author, *surface),
                    Stream {
                        listed,
                        record,
                        seq: 0,
                        sequence: Vec::new(),
                        sequence_bytes: 0,
                        started: false,
                    },
                );
                self.index_dirty = true;
            }
            NeighbourMsg::SurfaceFrame {
                surface,
                encoded_h264,
                ts,
                ..
            } => {
                let Some(stream) = self.streams.get_mut(&(author, *surface)) else {
                    return;
                };
                let key = wire::h264_keyframe(encoded_h264);
                if !key && stream.sequence.is_empty() {
                    return;
                }
                let Ok(chunks) = wire::chunks(encoded_h264, key, stream.seq, ts / 1_000) else {
                    return;
                };
                stream.seq = stream.seq.wrapping_add(1);
                let chunks = chunks
                    .into_iter()
                    .map(bytes::Bytes::from)
                    .collect::<Vec<_>>();
                if key {
                    let Ok(start) = wire::Frame::Start(stream.listed.clone()).encode() else {
                        return;
                    };
                    stream.sequence = vec![bytes::Bytes::from(start)];
                    stream.sequence_bytes = 0;
                }
                let added = chunks.iter().map(bytes::Bytes::len).sum::<usize>();
                if stream.sequence_bytes + added > MAX_SEQUENCE_BYTES {
                    // Too long to replay: wait for the next keyframe.
                    stream.sequence.clear();
                    stream.sequence_bytes = 0;
                    stream.started = false;
                    return;
                }
                stream.sequence.extend(chunks.iter().cloned());
                stream.sequence_bytes += added;
                // Without a subscriber nothing is written; the sequence is
                // replayed as a new group once one subscribes.
                stream.started = if key || !stream.started {
                    relay.inject_stream(
                        &self.room,
                        &publisher,
                        &stream.record,
                        true,
                        &stream.sequence,
                    )
                } else {
                    relay.inject_stream(&self.room, &publisher, &stream.record, false, &chunks)
                };
            }
            NeighbourMsg::SurfaceStreamEnd { surface, .. } => {
                if let Some(stream) = self.streams.remove(&(author, *surface)) {
                    relay.end_stream(&self.room, &publisher, &stream.record);
                    self.index_dirty = true;
                }
            }
            _ => {}
        }
    }

    /// Replay sequences to new subscribers and keep every publisher's index
    /// current.
    pub(crate) fn maintain(&mut self, relay: &LiveRelay, now: Instant) {
        // A viewer subscribed since the last frame (or all left and one came
        // back): give it the sequence.
        for ((author, _), stream) in &mut self.streams {
            let publisher = hex::encode(author);
            stream.started &= relay.stream_open(&self.room, &publisher, &stream.record);
            if !stream.started && !stream.sequence.is_empty() {
                stream.started = relay.inject_stream(
                    &self.room,
                    &publisher,
                    &stream.record,
                    true,
                    &stream.sequence,
                );
            }
        }
        let due = self
            .index_sent
            .is_none_or(|sent| now.saturating_duration_since(sent) >= INDEX_EVERY);
        if !self.index_dirty && !(due && !self.streams.is_empty()) {
            return;
        }
        let mut indexes: BTreeMap<[u8; 32], Vec<wire::Stream>> = self
            .listed
            .iter()
            .map(|author| (*author, Vec::new()))
            .collect();
        for ((author, _), stream) in &self.streams {
            indexes
                .entry(*author)
                .or_default()
                .push(stream.listed.clone());
        }
        let record = hex::encode(wire::index_record().digest());
        self.listed.clear();
        for (author, mut streams) in indexes {
            streams.truncate(wire::MAX_STREAMS);
            if !streams.is_empty() {
                self.listed.insert(author);
            }
            let count = streams.len();
            if let Ok(frame) = wire::Frame::Index(streams).encode() {
                // Also announces the namespace a viewer then subscribes in.
                let delivered = relay.inject_ops(&self.room, &hex::encode(author), &record, &frame);
                tracing::trace!(streams = count, delivered, "legacy window index");
            }
        }
        self.index_sent = Some(now);
        self.index_dirty = false;
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion"
)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tilo_moq::live::{
        Admission, AuthorizationStatus, LiveAuthority, LiveSession, LoopbackLive,
    };

    const ROOM: &str = "6b3f9a10-0000-4000-8000-00000000000c";
    const SESSION: [u8; 32] = [7; 32];
    const IDR: [u8; 12] = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x65, 0x88, 0x84];
    const DELTA: [u8; 6] = [0, 0, 0, 1, 0x41, 0x9a];

    struct Viewer(String);
    impl LiveSession for Viewer {
        fn room(&self) -> &str {
            &self.0
        }
        fn publisher(&self) -> &str {
            "abababababababababababababababababababababababababababababababab"
        }
        fn publish(&self) -> bool {
            false
        }
        fn subscribe(&self) -> bool {
            true
        }
        fn status(&self) -> AuthorizationStatus {
            AuthorizationStatus::Current
        }
    }
    struct Authority;
    impl LiveAuthority for Authority {
        fn try_admit(&self, token: &[u8]) -> Result<Arc<dyn LiveSession>, Admission> {
            if token == b"viewer" {
                Ok(Arc::new(Viewer(hex::encode(room_digest(ROOM)))))
            } else {
                Err(Admission::Denied)
            }
        }
        fn presence_seq(&self, _: &[u8]) -> Option<u64> {
            None
        }
        fn ops_valid(&self, _: &str, _: &[u8]) -> bool {
            false
        }
    }

    fn start(surface: u64) -> NeighbourMsg {
        NeighbourMsg::SurfaceStreamStart {
            surface,
            source_peer: 0xABCD,
            width: 640,
            height: 480,
            codec: "h264".into(),
            ts: 1,
            sender_monotonic_ns_at_send: 0,
            colorimetry: None,
            hdr_static_metadata: None,
            bit_depth: 0,
            app_id: "org.chromium.Chromium".into(),
        }
    }
    fn frame(surface: u64, bytes: &[u8]) -> NeighbourMsg {
        NeighbourMsg::SurfaceFrame {
            surface,
            source_peer: 0xABCD,
            encoded_h264: bytes.to_vec(),
            ts: 5_000,
        }
    }

    /// The session's `SurfaceStream*` presence becomes an index and a
    /// keyframe-headed group on the live relay, as a viewer receives them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_streams_reach_a_live_viewer_as_an_index_and_keyframe_groups() {
        let live = LoopbackLive::start(Arc::new(Authority)).unwrap();
        let trust = tilo_moq::carrier::Trust::Fingerprints(live.fingerprints.clone());
        let viewer = tilo_moq::live::peer::Peer::connect(
            &live.endpoint.parse().unwrap(),
            &trust,
            b"viewer",
            None,
        )
        .await
        .unwrap();
        let relay = &live.relay;
        let mut bridge = LegacyWindows::new(ROOM);
        let mut now = Instant::now();
        bridge.observe(relay, SESSION, &start(7));
        bridge.maintain(relay, now);
        let path = format!(
            "tilo-live/{}/{}",
            hex::encode(room_digest(ROOM)),
            hex::encode(SESSION)
        );
        let namespace = tilo_test_env::within("the session namespace", viewer.member(&path))
            .await
            .expect("announced by the index");
        let track = |record: &tilo_delivery_protocol::delivery_v3::live_wire::LiveRecord| {
            format!("ops/{}", hex::encode(record.digest()))
        };
        let subscription =
            moq_net::track::Subscription::default().with_max_age(Duration::from_secs(2));
        let mut index = namespace
            .track(&track(&wire::index_record()))
            .unwrap()
            .subscribe(subscription.clone())
            .await
            .unwrap();
        let listed = tilo_test_env::within("the index", async {
            loop {
                now += INDEX_EVERY;
                bridge.maintain(relay, now);
                if let Ok(Ok(Some(mut group))) =
                    tokio::time::timeout(Duration::from_millis(50), index.recv_group()).await
                    && let Ok(Some(frame)) = group.read_frame().await
                    && let Ok(wire::Frame::Index(streams)) = wire::Frame::decode(&frame.payload)
                {
                    return streams;
                }
            }
        })
        .await;
        let expected = wire::Stream::new(0xABCD, 7, "org.chromium.Chromium", "h264", 640, 480);
        assert_eq!(listed, std::slice::from_ref(&expected));

        // Before any keyframe there is nothing to decode from.
        bridge.observe(relay, SESSION, &frame(7, &DELTA));
        // A static window: one keyframe before anyone watches, then nothing.
        bridge.observe(relay, SESSION, &frame(7, &IDR));
        let mut video = namespace
            .track(&track(&wire::stream_record(0xABCD, 7)))
            .unwrap()
            .subscribe(subscription)
            .await
            .unwrap();
        let mut group = tilo_test_env::within("the replayed keyframe group", async {
            loop {
                bridge.maintain(relay, now);
                if let Ok(Ok(Some(group))) =
                    tokio::time::timeout(Duration::from_millis(50), video.recv_group()).await
                {
                    return group;
                }
            }
        })
        .await;
        let mut next = async || {
            let frame = tilo_test_env::within("a frame", group.read_frame())
                .await
                .unwrap()
                .unwrap();
            wire::Frame::decode(&frame.payload).unwrap()
        };
        assert_eq!(next().await, wire::Frame::Start(expected));
        let wire::Frame::Chunk { flags, data, .. } = next().await else {
            panic!("the keyframe follows the start");
        };
        assert_eq!(flags & wire::CHUNK_KEY, wire::CHUNK_KEY);
        assert_eq!(data, IDR);
        bridge.observe(relay, SESSION, &frame(7, &DELTA));
        let wire::Frame::Chunk { flags, data, .. } = next().await else {
            panic!("a delta joins the group");
        };
        assert_eq!(
            (flags & wire::CHUNK_KEY, data.as_slice()),
            (0, DELTA.as_slice())
        );

        bridge.observe(
            relay,
            SESSION,
            &NeighbourMsg::SurfaceStreamEnd {
                surface: 7,
                source_peer: 0xABCD,
                ts: 9,
            },
        );
        bridge.maintain(relay, now);
        let emptied = tilo_test_env::within("the emptied index", async {
            loop {
                if let Some(mut group) = index.recv_group().await.unwrap()
                    && let Ok(Some(frame)) = group.read_frame().await
                    && let Ok(wire::Frame::Index(streams)) = wire::Frame::decode(&frame.payload)
                    && streams.is_empty()
                {
                    return true;
                }
            }
        })
        .await;
        assert!(emptied);
    }

    #[tokio::test]
    async fn viewer_input_becomes_remote_input_of_the_governor_for_that_surface() {
        let live = LoopbackLive::start(Arc::new(Authority)).unwrap();
        let mut bridge = LegacyWindows::new(ROOM);
        bridge.observe(&live.relay, SESSION, &start(7));
        let record = wire::stream_record(0xABCD, 7).record;
        let governor = [5; 32];
        let request = |record: &str| wire::InputRequest {
            record: record.into(),
            events: vec![
                wire::InputEvent::PointerEnter { surface: 1 },
                wire::InputEvent::PointerButton {
                    button: 272,
                    pressed: true,
                },
            ],
        };
        assert!(bridge.queue_input(request("other"), &governor).is_err());
        bridge.queue_input(request(&record), &governor).unwrap();
        let now = Instant::now();
        let (hello, messages) = bridge.take_input(now);
        assert!(hello, "the session learns the governor's identity first");
        assert_eq!(messages.len(), 2);
        let NeighbourMsg::RemoteInput {
            peer,
            surface,
            event,
        } = &messages[0]
        else {
            panic!("not remote input");
        };
        assert_eq!((*peer, *surface), (peer_handle(&governor), 7));
        assert_eq!(
            postcard::from_bytes::<wire::InputEvent>(event).unwrap(),
            wire::InputEvent::PointerEnter { surface: 7 },
            "the viewer's surface is replaced by the stream's"
        );
        bridge.queue_input(request(&record), &governor).unwrap();
        assert!(
            !bridge.take_input(now).0,
            "identity is not repeated at once"
        );
        assert_eq!(bridge.take_input(now), (false, Vec::new()));
    }

    /// The public key of the Ed25519 seed `[1; 32]`; the session's
    /// `peer_liveness_handle` of it pins the same value.
    const SEED_ONE_PUBLIC: [u8; 32] = [
        138, 136, 227, 221, 116, 9, 241, 149, 253, 82, 219, 45, 60, 186, 93, 114, 202, 103, 9, 191,
        29, 148, 18, 27, 243, 116, 136, 1, 180, 15, 111, 92,
    ];

    #[test]
    fn the_peer_handle_is_the_sessions_handle_of_the_device_key() {
        assert_eq!(peer_handle(&SEED_ONE_PUBLIC), 0x90b3_65ba_582f_856f);
    }

    #[tokio::test]
    async fn only_h264_streams_with_an_app_are_bridged_and_a_departed_session_ends_them() {
        let live = LoopbackLive::start(Arc::new(Authority)).unwrap();
        let mut bridge = LegacyWindows::new(ROOM);
        let mut vp9 = start(1);
        if let NeighbourMsg::SurfaceStreamStart { codec, .. } = &mut vp9 {
            *codec = "vp9".into();
        }
        bridge.observe(&live.relay, SESSION, &vp9);
        assert!(bridge.streams.is_empty());
        bridge.observe(&live.relay, SESSION, &start(2));
        bridge.observe(&live.relay, [9; 32], &start(3));
        assert_eq!(bridge.publishers(), BTreeSet::from([SESSION, [9; 32]]));
        let now = Instant::now();
        bridge.subscribed(&live.relay, SESSION, false, now);
        bridge.subscribed(&live.relay, SESSION, true, now + DEPARTURE);
        bridge.subscribed(&live.relay, SESSION, false, now + DEPARTURE * 2);
        assert_eq!(
            bridge.publishers().len(),
            2,
            "a reconnect keeps the streams"
        );
        bridge.subscribed(&live.relay, SESSION, false, now + DEPARTURE * 3);
        assert_eq!(bridge.publishers(), BTreeSet::from([[9; 32]]));
        assert!(bridge.index_dirty, "viewers learn the stream ended");
    }
}
