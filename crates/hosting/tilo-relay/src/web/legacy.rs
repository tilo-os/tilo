//! Legacy windows for browser members (`tilo_relay_protocol::legacy_window`).
//!
//! A session shares a legacy window with this host by granting the host's
//! device `workspace-control:<surface>`; the grant and the stream
//! (`SurfaceStreamStart`, `SurfaceFrame`, `SurfaceStreamEnd`) reach the host
//! as presence of the session's device. Streams granted to the host are
//! re-published on the live relay in that device's namespace, where the live
//! grant, and so the room's admission, decides who receives them; streams
//! shared only with other devices never leave their presence.
//!
//! A member that may drive the window sends input on
//! [`INPUT_ROUTE`](tilo_relay_protocol::legacy_window::INPUT_ROUTE). The host
//! forwards it to the session alone (never fanned out: it carries keys) as
//! `RemoteInput` of its own device, the viewer the session granted.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::coedit::AttributedPresence;
use tilo_moq::live::LiveRelay;
use tilo_peer_state::NeighbourMsg;
use tilo_relay_protocol::legacy_window as wire;
use tilo_relay_protocol::web::live_room;

/// How often streams are looked after: replays to new subscribers, index
/// re-sends, departures.
pub(crate) const MAINTAIN_EVERY: Duration = Duration::from_millis(250);
/// While any stream is live its index is re-sent this often, so a member that
/// subscribes later learns it.
const INDEX_EVERY: Duration = Duration::from_secs(1);
/// A session away from the room this long has left: its streams end.
const DEPARTURE: Duration = Duration::from_secs(15);
/// The session is reminded who sends it input at least this often: it drops
/// a device's identity when its route goes quiet.
const HELLO_EVERY: Duration = Duration::from_secs(30);
/// Most bytes of one coded sequence kept for a member that subscribes later.
const MAX_SEQUENCE_BYTES: usize = 8 * 1024 * 1024;
/// Most streams the host re-publishes for one room.
const MAX_ROOM_STREAMS: usize = 64;
const MAX_ROOMS: usize = 32;
const MAX_PUBLISHERS: usize = 32;
const MAX_CACHED_BYTES: usize = 64 * 1024 * 1024;

/// One stream a session publishes.
struct Stream {
    listed: wire::Stream,
    record: String,
    seq: u32,
    /// The current coded sequence as frames: its start, keyframe and later
    /// frames. A static window sends nothing more, so a member that
    /// subscribes later is given this instead of waiting for a keyframe.
    sequence: Vec<bytes::Bytes>,
    sequence_bytes: usize,
    /// A relay group is open: new frames join it.
    started: bool,
}

/// What one session device publishes in one room.
#[derive(Default)]
struct Publisher {
    /// The user the device chains to, as its connection authenticated it.
    user: [u8; 32],
    /// Surfaces the session granted this host.
    granted: BTreeSet<u64>,
    /// Streams the session announced, granted or not yet.
    announced: BTreeMap<u64, wire::Stream>,
    /// Granted streams, re-published.
    streams: BTreeMap<u64, Stream>,
    /// The last index sent listed a stream.
    listed: bool,
    absent: Option<Instant>,
    hello: Option<Instant>,
}

#[derive(Default)]
struct Room {
    publishers: BTreeMap<[u8; 32], Publisher>,
    index_sent: Option<Instant>,
    index_dirty: bool,
}

/// Every room's legacy windows on this host's live relay.
pub(crate) struct LegacyWindows {
    live: LiveRelay,
    /// This host's device and its session handle, the viewer sessions grant.
    device: [u8; 32],
    handle: u64,
    rooms: Mutex<HashMap<u64, Room>>,
    closed: AtomicBool,
}

/// What [`LegacyWindows::input`] refuses.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InputRefusal {
    /// The device does not stream that window to this host.
    NoSuchWindow,
}

impl LegacyWindows {
    pub(crate) fn new(live: LiveRelay, device: [u8; 32]) -> Arc<Self> {
        Arc::new(Self {
            live,
            device,
            handle: tilo_peer_state::peer_handle(&device),
            rooms: Mutex::default(),
            closed: AtomicBool::new(false),
        })
    }

    fn rooms(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Room>> {
        self.rooms.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Release media when the browser listener stops. The relay retains its
    /// bridge, so it must no longer accept late native presence or input.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let rooms = std::mem::take(&mut *self.rooms());
        let index = wire::Frame::Index(Vec::new()).encode().ok();
        let record = hex::encode(wire::index_record().digest());
        for (workspace, state) in rooms {
            let room = live_room(workspace);
            for (device, publisher) in state.publishers {
                let publisher_hex = hex::encode(device);
                for stream in publisher.streams.values() {
                    self.live.end_stream(&room, &publisher_hex, &stream.record);
                }
                if publisher.listed
                    && let Some(index) = &index
                {
                    self.live.inject_ops(&room, &publisher_hex, &record, index);
                }
            }
        }
    }

    /// Presence `payload` that the subscribed device `author` of `user` sent
    /// in `workspace_id`. Anything but a legacy-window message of the
    /// device itself is ignored.
    pub(crate) fn observe_presence(
        &self,
        workspace_id: u64,
        author: [u8; 32],
        user: [u8; 32],
        payload: &[u8],
    ) {
        let Some(presence) = AttributedPresence::decode(payload) else {
            return;
        };
        // The relay does not stamp presence: only a device's own word about
        // its own windows counts.
        if presence.author != author {
            return;
        }
        let Ok(message) = postcard::from_bytes::<NeighbourMsg>(&presence.body) else {
            return;
        };
        self.observe(workspace_id, author, user, &message);
    }

    fn observe(&self, workspace_id: u64, author: [u8; 32], user: [u8; 32], message: &NeighbourMsg) {
        let own = tilo_peer_state::peer_handle(&author);
        let room = live_room(workspace_id);
        let publisher_hex = hex::encode(author);
        let mut rooms = self.rooms();
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if !rooms.contains_key(&workspace_id) && rooms.len() >= MAX_ROOMS {
            return;
        }
        if let Some(state) = rooms.get(&workspace_id)
            && !state.publishers.contains_key(&author)
            && state.publishers.len() >= MAX_PUBLISHERS
        {
            return;
        }
        match message {
            NeighbourMsg::PermissionGrant { peer, capability } if *peer == self.handle => {
                self.grant(&mut rooms, workspace_id, author, user, capability);
            }
            NeighbourMsg::SurfaceStreamStart {
                surface,
                source_peer,
                width,
                height,
                codec,
                app_id,
                ..
            } if *source_peer == own => {
                // Keyframes are found in the bitstream; browsers decode
                // H.264, which the session picks for a viewer that never
                // advertised codecs.
                if codec != "h264" || app_id.is_empty() {
                    tracing::debug!(codec, app_id, "legacy window stream not bridged");
                    return;
                }
                let listed = wire::Stream::new(*surface, app_id, codec, *width, *height);
                if !listed.valid() {
                    return;
                }
                self.announce(&mut rooms, workspace_id, author, user, *surface, listed);
            }
            NeighbourMsg::SurfaceFrame {
                surface,
                source_peer,
                encoded_h264,
                ts,
            } if *source_peer == own => {
                let cached = rooms
                    .values()
                    .flat_map(|state| state.publishers.values())
                    .flat_map(|publisher| publisher.streams.values())
                    .map(|stream| stream.sequence_bytes)
                    .sum::<usize>();
                let Some(stream) = rooms
                    .get_mut(&workspace_id)
                    .and_then(|state| state.publishers.get_mut(&author))
                    .and_then(|publisher| publisher.streams.get_mut(surface))
                else {
                    return;
                };
                if cached.saturating_add(encoded_h264.len().saturating_add(2048)) > MAX_CACHED_BYTES
                {
                    self.live.end_stream(&room, &publisher_hex, &stream.record);
                    stream.sequence.clear();
                    stream.sequence_bytes = 0;
                    stream.started = false;
                    return;
                }
                Self::frame(&self.live, &room, &publisher_hex, stream, encoded_h264, *ts);
            }
            NeighbourMsg::SurfaceStreamEnd {
                surface,
                source_peer,
                ..
            } if *source_peer == own => {
                let Some(state) = rooms.get_mut(&workspace_id) else {
                    return;
                };
                let Some(publisher) = state.publishers.get_mut(&author) else {
                    return;
                };
                publisher.announced.remove(surface);
                if let Some(stream) = publisher.streams.remove(surface) {
                    self.live.end_stream(&room, &publisher_hex, &stream.record);
                    state.index_dirty = true;
                }
            }
            _ => {}
        }
        drop(rooms);
    }

    fn grant(
        &self,
        rooms: &mut HashMap<u64, Room>,
        workspace_id: u64,
        author: [u8; 32],
        user: [u8; 32],
        capability: &str,
    ) {
        let room = live_room(workspace_id);
        let publisher_hex = hex::encode(author);
        let Some((surface, granted)) = wire::parse_grant(capability) else {
            return;
        };
        let state = rooms.entry(workspace_id).or_default();
        let publisher = state.publishers.entry(author).or_default();
        publisher.user = user;
        if granted {
            if publisher.granted.len() >= wire::MAX_STREAMS && !publisher.granted.contains(&surface)
            {
                return;
            }
            publisher.granted.insert(surface);
            if !publisher.streams.contains_key(&surface)
                && let Some(listed) = publisher.announced.get(&surface).cloned()
            {
                Self::start(
                    &self.live,
                    &room,
                    &publisher_hex,
                    publisher,
                    surface,
                    listed,
                );
                state.index_dirty = true;
            }
        } else {
            publisher.granted.remove(&surface);
            if let Some(stream) = publisher.streams.remove(&surface) {
                self.live.end_stream(&room, &publisher_hex, &stream.record);
                state.index_dirty = true;
            }
        }
    }

    fn announce(
        &self,
        rooms: &mut HashMap<u64, Room>,
        workspace_id: u64,
        author: [u8; 32],
        user: [u8; 32],
        surface: u64,
        listed: wire::Stream,
    ) {
        let room = live_room(workspace_id);
        let publisher_hex = hex::encode(author);
        let state = rooms.entry(workspace_id).or_default();
        let total: usize = state.publishers.values().map(|p| p.announced.len()).sum();
        let publisher = state.publishers.entry(author).or_default();
        publisher.user = user;
        if !publisher.announced.contains_key(&surface)
            && (total >= MAX_ROOM_STREAMS || publisher.announced.len() >= wire::MAX_STREAMS)
        {
            return;
        }
        publisher.announced.insert(surface, listed.clone());
        if publisher.granted.contains(&surface)
            && publisher
                .streams
                .get(&surface)
                .is_none_or(|stream| stream.listed != listed)
        {
            Self::start(
                &self.live,
                &room,
                &publisher_hex,
                publisher,
                surface,
                listed,
            );
            state.index_dirty = true;
        }
    }

    fn start(
        live: &LiveRelay,
        room: &str,
        publisher_hex: &str,
        publisher: &mut Publisher,
        surface: u64,
        listed: wire::Stream,
    ) {
        let record = hex::encode(wire::stream_record(surface).digest());
        live.end_stream(room, publisher_hex, &record);
        tracing::debug!(app = listed.app, surface, "legacy window stream bridged");
        publisher.streams.insert(
            surface,
            Stream {
                listed,
                record,
                seq: 0,
                sequence: Vec::new(),
                sequence_bytes: 0,
                started: false,
            },
        );
    }

    fn frame(
        live: &LiveRelay,
        room: &str,
        publisher_hex: &str,
        stream: &mut Stream,
        encoded: &[u8],
        ts_ns: u64,
    ) {
        let key = wire::h264_keyframe(encoded);
        if !key && stream.sequence.is_empty() {
            return;
        }
        let Ok(chunks) = wire::chunks(encoded, key, stream.seq, ts_ns / 1_000) else {
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
            stream.sequence_bytes = start.len();
            stream.sequence = vec![bytes::Bytes::from(start)];
        }
        let added = chunks.iter().map(bytes::Bytes::len).sum::<usize>();
        if stream.sequence_bytes.saturating_add(added) > MAX_SEQUENCE_BYTES {
            // Too long to replay: wait for the next keyframe.
            live.end_stream(room, publisher_hex, &stream.record);
            stream.sequence.clear();
            stream.sequence_bytes = 0;
            stream.started = false;
            return;
        }
        stream.sequence.extend(chunks.iter().cloned());
        stream.sequence_bytes = stream.sequence_bytes.saturating_add(added);
        // Without a subscriber nothing is written; the sequence is replayed
        // as a new group once one subscribes.
        stream.started = if key || !stream.started {
            live.inject_stream(room, publisher_hex, &stream.record, true, &stream.sequence)
        } else {
            live.inject_stream(room, publisher_hex, &stream.record, false, &chunks)
        };
    }

    /// The devices publishing streams, per room, for [`Self::present`].
    pub(crate) fn publishers(&self) -> Vec<(u64, [u8; 32])> {
        self.rooms()
            .iter()
            .flat_map(|(workspace_id, room)| {
                room.publishers
                    .iter()
                    .filter(|(_, publisher)| {
                        !publisher.announced.is_empty() || !publisher.granted.is_empty()
                    })
                    .map(|(device, _)| (*workspace_id, *device))
            })
            .collect()
    }

    /// Whether the publishing device is still subscribed to the room. A
    /// static window sends no frames, so its session leaving, not silence,
    /// ends its streams; a reconnect shorter than [`DEPARTURE`] does not.
    pub(crate) fn present(&self, workspace_id: u64, device: [u8; 32], here: bool, now: Instant) {
        let mut rooms = self.rooms();
        let Some(state) = rooms.get_mut(&workspace_id) else {
            return;
        };
        let Some(publisher) = state.publishers.get_mut(&device) else {
            return;
        };
        if here {
            publisher.absent = None;
            return;
        }
        let since = *publisher.absent.get_or_insert(now);
        if now.saturating_duration_since(since) < DEPARTURE {
            return;
        }
        let room = live_room(workspace_id);
        let publisher_hex = hex::encode(device);
        for stream in publisher.streams.values() {
            self.live.end_stream(&room, &publisher_hex, &stream.record);
        }
        let listed = publisher.listed;
        state.publishers.remove(&device);
        if listed {
            // Its index must say the streams ended.
            state.publishers.insert(
                device,
                Publisher {
                    listed: true,
                    ..Publisher::default()
                },
            );
            state.index_dirty = true;
        }
        drop(rooms);
    }

    /// End immediately when membership, write permission or the device certificate is lost.
    pub(crate) fn revoke(&self, workspace_id: u64, device: [u8; 32], now: Instant) {
        if let Some(publisher) = self
            .rooms()
            .get_mut(&workspace_id)
            .and_then(|state| state.publishers.get_mut(&device))
        {
            publisher.absent = now.checked_sub(DEPARTURE);
        }
        self.present(workspace_id, device, false, now);
    }

    /// Replay sequences to new subscribers and keep every index current.
    pub(crate) fn maintain(&self, now: Instant) {
        let mut rooms = self.rooms();
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        for (workspace_id, state) in rooms.iter_mut() {
            let room = live_room(*workspace_id);
            for (device, publisher) in &mut state.publishers {
                let publisher_hex = hex::encode(device);
                for stream in publisher.streams.values_mut() {
                    // A member subscribed since the last frame (or all left
                    // and one came back): give it the sequence.
                    stream.started &= self.live.stream_open(&room, &publisher_hex, &stream.record);
                    if !stream.started && !stream.sequence.is_empty() {
                        stream.started = self.live.inject_stream(
                            &room,
                            &publisher_hex,
                            &stream.record,
                            true,
                            &stream.sequence,
                        );
                    }
                }
            }
            let live = state.publishers.values().any(|p| !p.streams.is_empty());
            let due = state
                .index_sent
                .is_none_or(|sent| now.saturating_duration_since(sent) >= INDEX_EVERY);
            if !(state.index_dirty || due && live) {
                continue;
            }
            let record = hex::encode(wire::index_record().digest());
            for (device, publisher) in &mut state.publishers {
                if publisher.streams.is_empty() && !publisher.listed {
                    continue;
                }
                let mut streams = publisher
                    .streams
                    .values()
                    .map(|stream| stream.listed.clone())
                    .collect::<Vec<_>>();
                streams.truncate(wire::MAX_STREAMS);
                publisher.listed = !streams.is_empty();
                if let Ok(frame) = wire::Frame::Index(streams).encode() {
                    // Also announces the namespace a member subscribes in.
                    self.live
                        .inject_ops(&room, &hex::encode(device), &record, &frame);
                }
            }
            state.publishers.retain(|_, publisher| {
                publisher.listed || !publisher.announced.is_empty() || !publisher.granted.is_empty()
            });
            state.index_sent = Some(now);
            state.index_dirty = false;
        }
        rooms.retain(|_, state| !state.publishers.is_empty());
    }

    /// The user the publishing device chains to, while it streams `surface`
    /// to this host.
    pub(crate) fn publisher_user(
        &self,
        workspace_id: u64,
        device: [u8; 32],
        surface: u64,
    ) -> Option<[u8; 32]> {
        self.rooms()
            .get(&workspace_id)?
            .publishers
            .get(&device)
            .filter(|publisher| publisher.streams.contains_key(&surface))
            .map(|publisher| publisher.user)
    }

    /// The presence payloads that deliver `events` for `surface` to the
    /// publishing device: this host's identity first when it is due, then
    /// one `RemoteInput` per event, authored by this host's device.
    pub(crate) fn input(
        &self,
        workspace_id: u64,
        device: [u8; 32],
        surface: u64,
        events: Vec<wire::InputEvent>,
        identity: &tilo_user_identity::DeviceCert,
        now: Instant,
    ) -> Result<Vec<Vec<u8>>, InputRefusal> {
        let mut rooms = self.rooms();
        if self.closed.load(Ordering::Acquire) {
            return Err(InputRefusal::NoSuchWindow);
        }
        let publisher = rooms
            .get_mut(&workspace_id)
            .and_then(|state| state.publishers.get_mut(&device))
            .filter(|publisher| publisher.streams.contains_key(&surface))
            .ok_or(InputRefusal::NoSuchWindow)?;
        let mut messages = Vec::with_capacity(events.len().saturating_add(1));
        if publisher
            .hello
            .is_none_or(|sent| now.saturating_duration_since(sent) >= HELLO_EVERY)
        {
            publisher.hello = Some(now);
            messages.push(NeighbourMsg::IdentityHello {
                cert: Box::new(identity.clone()),
                revocations: Vec::new(),
            });
        }
        for event in events {
            let Ok(event) = postcard::to_allocvec(&event.for_surface(surface)) else {
                continue;
            };
            messages.push(NeighbourMsg::RemoteInput {
                peer: self.handle,
                surface,
                event,
            });
        }
        drop(rooms);
        Ok(messages
            .iter()
            .filter_map(|message| {
                let body = postcard::to_allocvec(message).ok()?;
                AttributedPresence {
                    author: self.device,
                    body,
                }
                .encode()
                .ok()
            })
            .collect())
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
    use tilo_moq::live::{
        Admission, AuthorizationStatus, LiveAuthority, LiveSession, LoopbackLive,
    };

    const WORKSPACE: u64 = 0x5eed;
    const SESSION: [u8; 32] = [7; 32];
    const USER: [u8; 32] = [8; 32];
    const HOST: [u8; 32] = [5; 32];
    const IDR: [u8; 12] = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x65, 0x88, 0x84];
    const DELTA: [u8; 6] = [0, 0, 0, 1, 0x41, 0x9a];

    struct Viewer(String);
    impl LiveSession for Viewer {
        fn room(&self) -> &str {
            &self.0
        }
        fn publisher(&self) -> &'static str {
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
                Ok(Arc::new(Viewer(live_room(WORKSPACE))))
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

    fn own() -> u64 {
        tilo_peer_state::peer_handle(&SESSION)
    }
    fn grant(surface: u64, peer: u64, state: &str) -> NeighbourMsg {
        NeighbourMsg::PermissionGrant {
            peer,
            capability: format!("workspace-control:{surface}:{state}"),
        }
    }
    fn start(surface: u64) -> NeighbourMsg {
        NeighbourMsg::SurfaceStreamStart {
            surface,
            source_peer: own(),
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
            source_peer: own(),
            encoded_h264: bytes.to_vec(),
            ts: 5_000,
        }
    }
    fn presence(author: [u8; 32], message: &NeighbourMsg) -> Vec<u8> {
        AttributedPresence {
            author,
            body: postcard::to_allocvec(message).unwrap(),
        }
        .encode()
        .unwrap()
    }

    fn observe(bridge: &LegacyWindows, message: &NeighbourMsg) {
        bridge.observe_presence(WORKSPACE, SESSION, USER, &presence(SESSION, message));
    }

    fn track(record: &tilo_relay_protocol::live::LiveRecord) -> String {
        format!("ops/{}", hex::encode(record.digest()))
    }

    async fn connect_viewer(live: &LoopbackLive) -> tilo_moq::live::peer::Peer {
        let trust = tilo_moq::carrier::Trust::Fingerprints(live.fingerprints.clone());
        tilo_moq::live::peer::Peer::connect(
            &live.endpoint.parse().unwrap(),
            &trust,
            b"viewer",
            None,
        )
        .await
        .unwrap()
    }

    /// A granted stream becomes an index and keyframe-headed groups on the
    /// live relay, as a member receives them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn granted_streams_reach_a_live_member_as_an_index_and_keyframe_groups() {
        let live = LoopbackLive::start(Arc::new(Authority)).unwrap();
        let viewer = connect_viewer(&live).await;
        let bridge = LegacyWindows::new(live.relay.clone(), HOST);
        let mut now = Instant::now();
        let host = tilo_peer_state::peer_handle(&HOST);
        observe(&bridge, &grant(7, host, "granted"));
        observe(&bridge, &start(7));
        bridge.maintain(now);
        let path = format!(
            "tilo-live/{}/{}",
            live_room(WORKSPACE),
            hex::encode(SESSION)
        );
        let namespace = tilo_test_env::within("the session namespace", viewer.member(&path))
            .await
            .expect("announced by the index");
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
                bridge.maintain(now);
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
        let expected = wire::Stream::new(7, "org.chromium.Chromium", "h264", 640, 480);
        assert_eq!(listed, std::slice::from_ref(&expected));

        // Before any keyframe there is nothing to decode from.
        observe(&bridge, &frame(7, &DELTA));
        // A static window: one keyframe before anyone watches, then nothing.
        observe(&bridge, &frame(7, &IDR));
        let mut video = namespace
            .track(&track(&wire::stream_record(7)))
            .unwrap()
            .subscribe(subscription)
            .await
            .unwrap();
        let mut group = tilo_test_env::within("the replayed keyframe group", async {
            loop {
                bridge.maintain(now);
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
        observe(&bridge, &frame(7, &DELTA));
        let wire::Frame::Chunk { flags, data, .. } = next().await else {
            panic!("a delta joins the group");
        };
        assert_eq!(
            (flags & wire::CHUNK_KEY, data.as_slice()),
            (0, DELTA.as_slice())
        );

        // Revoking the grant ends the stream for members.
        observe(&bridge, &grant(7, host, "revoked"));
        bridge.maintain(now);
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
    async fn closing_the_bridge_frees_replay_and_refuses_late_presence_and_input() {
        let live = LoopbackLive::start(Arc::new(Authority)).unwrap();
        let bridge = LegacyWindows::new(live.relay.clone(), HOST);
        let host = tilo_peer_state::peer_handle(&HOST);
        let publish = || {
            bridge.observe_presence(
                WORKSPACE,
                SESSION,
                USER,
                &presence(SESSION, &grant(7, host, "granted")),
            );
            bridge.observe_presence(WORKSPACE, SESSION, USER, &presence(SESSION, &start(7)));
            bridge.observe_presence(
                WORKSPACE,
                SESSION,
                USER,
                &presence(SESSION, &frame(7, &IDR)),
            );
        };
        publish();
        assert_eq!(bridge.publisher_user(WORKSPACE, SESSION, 7), Some(USER));
        bridge.close();
        assert!(bridge.rooms().is_empty());
        publish();
        bridge.maintain(Instant::now());
        assert!(
            bridge.rooms().is_empty(),
            "the retained bridge must not refill after listener shutdown"
        );
        let identity = tilo_user_identity::DeviceCert {
            user_public_key: USER,
            device_public_key: HOST,
            device_label: "test host".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::host(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        assert_eq!(
            bridge.input(WORKSPACE, SESSION, 7, vec![], &identity, Instant::now()),
            Err(InputRefusal::NoSuchWindow)
        );
    }

    #[tokio::test]
    async fn repeated_grants_preserve_static_frames_and_all_metadata_is_bounded() {
        let live = LoopbackLive::start(Arc::new(Authority)).unwrap();
        let bridge = LegacyWindows::new(live.relay.clone(), HOST);
        let host = tilo_peer_state::peer_handle(&HOST);
        for surface in 0..wire::MAX_STREAMS as u64 + 10 {
            bridge.observe_presence(
                WORKSPACE,
                SESSION,
                USER,
                &presence(SESSION, &grant(surface, host, "granted")),
            );
            bridge.observe_presence(
                WORKSPACE,
                SESSION,
                USER,
                &presence(SESSION, &start(surface)),
            );
        }
        bridge.observe_presence(
            WORKSPACE,
            SESSION,
            USER,
            &presence(SESSION, &frame(7, &IDR)),
        );
        bridge.observe_presence(
            WORKSPACE,
            SESSION,
            USER,
            &presence(SESSION, &grant(7, host, "granted")),
        );
        {
            let rooms = bridge.rooms();
            let publisher = &rooms[&WORKSPACE].publishers[&SESSION];
            assert_eq!(publisher.granted.len(), wire::MAX_STREAMS);
            assert_eq!(publisher.announced.len(), wire::MAX_STREAMS);
            assert_eq!(publisher.streams.len(), wire::MAX_STREAMS);
            assert!(
                !publisher.streams[&7].sequence.is_empty(),
                "repeat grant must keep the static image replay"
            );
            drop(rooms);
        }
        bridge.revoke(WORKSPACE, SESSION, Instant::now());
        assert!(bridge.publisher_user(WORKSPACE, SESSION, 7).is_none());
        bridge.maintain(Instant::now());
        assert!(
            bridge.rooms().is_empty(),
            "revoked metadata must be reclaimed"
        );
    }

    #[tokio::test]
    async fn only_streams_granted_to_this_host_by_their_own_device_are_bridged() {
        let live = LoopbackLive::start(Arc::new(Authority)).unwrap();
        let bridge = LegacyWindows::new(live.relay.clone(), HOST);
        let host = tilo_peer_state::peer_handle(&HOST);
        // Shared with someone else only.
        bridge.observe_presence(
            WORKSPACE,
            SESSION,
            USER,
            &presence(SESSION, &grant(1, 42, "granted")),
        );
        bridge.observe_presence(WORKSPACE, SESSION, USER, &presence(SESSION, &start(1)));
        // Another device speaking for the session.
        bridge.observe_presence(
            WORKSPACE,
            [9; 32],
            USER,
            &presence(SESSION, &grant(2, host, "granted")),
        );
        bridge.observe_presence(WORKSPACE, [9; 32], USER, &presence(SESSION, &start(2)));
        // A codec browsers are not sent.
        let mut vp9 = start(3);
        if let NeighbourMsg::SurfaceStreamStart { codec, .. } = &mut vp9 {
            *codec = "vp9".into();
        }
        bridge.observe_presence(
            WORKSPACE,
            SESSION,
            USER,
            &presence(SESSION, &grant(3, host, "granted")),
        );
        bridge.observe_presence(WORKSPACE, SESSION, USER, &presence(SESSION, &vp9));
        for surface in [1, 2, 3] {
            assert_eq!(bridge.publisher_user(WORKSPACE, SESSION, surface), None);
        }
        // The stream announced before its grant is bridged once granted.
        bridge.observe_presence(
            WORKSPACE,
            SESSION,
            USER,
            &presence(SESSION, &grant(1, host, "granted")),
        );
        assert_eq!(bridge.publisher_user(WORKSPACE, SESSION, 1), Some(USER));
    }

    #[tokio::test]
    async fn input_reaches_the_session_as_remote_input_of_this_host_and_a_departed_session_ends() {
        let live = LoopbackLive::start(Arc::new(Authority)).unwrap();
        let bridge = LegacyWindows::new(live.relay.clone(), HOST);
        let host = tilo_peer_state::peer_handle(&HOST);
        let identity = tilo_user_identity::DeviceCert {
            user_public_key: USER,
            device_public_key: HOST,
            device_label: "host".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::host(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        let events = vec![
            wire::InputEvent::PointerEnter { surface: 99 },
            wire::InputEvent::PointerButton {
                button: 272,
                pressed: true,
            },
        ];
        let now = Instant::now();
        assert_eq!(
            bridge.input(WORKSPACE, SESSION, 7, events.clone(), &identity, now),
            Err(InputRefusal::NoSuchWindow)
        );
        bridge.observe_presence(
            WORKSPACE,
            SESSION,
            USER,
            &presence(SESSION, &grant(7, host, "granted")),
        );
        bridge.observe_presence(WORKSPACE, SESSION, USER, &presence(SESSION, &start(7)));
        let decode = |payload: &[u8]| {
            let presence = AttributedPresence::decode(payload).unwrap();
            assert_eq!(presence.author, HOST);
            postcard::from_bytes::<NeighbourMsg>(&presence.body).unwrap()
        };
        let sent = bridge
            .input(WORKSPACE, SESSION, 7, events.clone(), &identity, now)
            .unwrap();
        assert_eq!(sent.len(), 3);
        assert!(matches!(
            decode(&sent[0]),
            NeighbourMsg::IdentityHello { .. }
        ));
        let NeighbourMsg::RemoteInput {
            peer,
            surface,
            event,
        } = decode(&sent[1])
        else {
            panic!("not remote input");
        };
        assert_eq!((peer, surface), (host, 7));
        assert_eq!(
            postcard::from_bytes::<wire::InputEvent>(&event).unwrap(),
            wire::InputEvent::PointerEnter { surface: 7 },
            "the member's surface is replaced by the stream's"
        );
        assert_eq!(
            bridge
                .input(WORKSPACE, SESSION, 7, events, &identity, now)
                .unwrap()
                .len(),
            2,
            "identity is not repeated at once"
        );

        assert_eq!(bridge.publishers(), vec![(WORKSPACE, SESSION)]);
        bridge.present(WORKSPACE, SESSION, false, now);
        bridge.present(WORKSPACE, SESSION, true, now + DEPARTURE);
        bridge.present(WORKSPACE, SESSION, false, now + DEPARTURE * 2);
        assert!(
            bridge.publisher_user(WORKSPACE, SESSION, 7).is_some(),
            "a reconnect keeps the stream"
        );
        bridge.present(WORKSPACE, SESSION, false, now + DEPARTURE * 3);
        assert_eq!(bridge.publisher_user(WORKSPACE, SESSION, 7), None);
        assert!(bridge.publishers().is_empty());
    }
}
