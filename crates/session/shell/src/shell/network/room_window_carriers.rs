//! A room's windows through its relay, for members with no direct route to
//! each other.
//!
//! A member's device that shows a window another member hosts asks the host
//! for it directly when it can ([`super::room_window_views`]). When it
//! reaches the host only through the room's relay, both use the room's
//! window carrier instead: the governor grants every member device of a
//! room that shares its layout a participation in it
//! ([`tilo_relay_protocol::voice::windows_call`]), and on it each window is
//! a `tilo-video/1` track of its own ([`tilo_moq::video::window_track`]).
//!
//! * The viewer shows the window's track of the host's publication. The
//!   relay asks the host for the track while anyone shows it.
//! * The host counts its carrier as one more viewer of each window whose
//!   track is asked for ([`carrier_peer`]), read only, so the window's one
//!   share encodes it (damage-driven, hardware first, only while someone
//!   views it) and its frames reach the carrier as they reach direct
//!   viewers ([`ShellNetwork::publish_legacy_to`]).
//! * Each picture is sealed with the content keys of the room's window
//!   channel ([`windows_channel`]), which only the room's member devices
//!   read: a relay host holds no key, forwards bytes it cannot open and
//!   never learns what a window shows. The seal binds the host, the tab and
//!   the picture's place in the stream ([`picture_artifact`]).
//! * The viewer opens each picture into the stream start and frames a
//!   direct stream carries, so the window's tile decodes it the same way.
//!
//! Nothing streams in a room whose layout is personal: the governor grants
//! no carrier there, and the host's shares name no member of it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use channel_crypto::ChannelContentKeys;
use collab_types::ChannelId;
use media::StreamCodec;
use tilo_lock::MutexRecover as _;
use tilo_moq::client::{Client, ReceivedPicture};
use tilo_moq::video::{Codec, Layer, VideoObject};

use super::room_window_views::RoomWindowViewer;
use super::{ShellNetwork, room_topic_id};
use crate::network::native_media::WindowCarrier;
use crate::network::protocol::NeighbourMsg;
use crate::shell::state::RoomId;

/// Record id of the channel a room's window pictures are sealed in.
const WINDOWS_RECORD: &str = "@tilo:windows";
/// Kind of a window picture among the artifacts bound to a record digest.
const PICTURE_KIND: u8 = 0x57;

/// The peer handle that stands for `room`'s carrier among the viewers of a
/// window this device hosts. Never a device's own handle: those hash an
/// endpoint key, this the room under its own domain.
pub fn carrier_peer(room: RoomId) -> u64 {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-room-window-carrier-v1\0");
    hash.update(room.to_string().as_bytes());
    let digest = hash.finalize();
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_le_bytes(bytes)
}

/// The channel whose content keys seal `room`'s window pictures.
pub fn windows_channel(room: RoomId) -> ChannelId {
    collab_types::document_channel_id(
        tilo_relay_protocol::legacy_window::APP,
        &room.to_string(),
        WINDOWS_RECORD,
    )
}

/// What sealed picture `sequence` of window `tab`, hosted on the device
/// whose key is `host`, is bound to.
pub fn picture_artifact(host: &[u8; 32], tab: u64, sequence: u64) -> collab_types::ArtifactId {
    crate::network::sealed_picture::artifact(
        b"tilo-room-window-picture-v2\0",
        &[host, &tab.to_le_bytes()],
        PICTURE_KIND,
        sequence,
    )
}

/// The carrier's codec for a stream's, when it carries that codec.
fn carried(codec: &str) -> Option<Codec> {
    match StreamCodec::from_wire_label(codec)? {
        StreamCodec::H264 => Some(Codec::H264),
        StreamCodec::Vp9 => Some(Codec::Vp9),
        StreamCodec::Av1 => Some(Codec::Av1),
        StreamCodec::HevcMain10 => None,
    }
}

fn stream_codec(codec: Codec) -> Option<StreamCodec> {
    match codec {
        Codec::H264 => Some(StreamCodec::H264),
        Codec::Vp9 => Some(StreamCodec::Vp9),
        Codec::Av1 => Some(StreamCodec::Av1),
        Codec::Vp8 | Codec::HevcMain10 => None,
    }
}

/// What the carrier's viewer decodes: what every member device decodes in
/// software, whatever its hardware.
fn carrier_capabilities() -> media::realtime::CodecCapabilities {
    media::realtime::CodecCapabilities {
        software_decode: [StreamCodec::H264, StreamCodec::Vp9].into(),
        ..media::realtime::CodecCapabilities::default()
    }
}

fn seal(
    keys: &ChannelContentKeys,
    host: &[u8; 32],
    tab: u64,
    sequence: u64,
    plaintext: &[u8],
) -> Option<Vec<u8>> {
    crate::network::sealed_picture::seal(keys, picture_artifact(host, tab, sequence), plaintext)
}

fn open(
    keys: &ChannelContentKeys,
    channel: ChannelId,
    host: &[u8; 32],
    tab: u64,
    sequence: u64,
    payload: &[u8],
) -> Option<Vec<u8>> {
    crate::network::sealed_picture::open(
        keys,
        channel,
        picture_artifact(host, tab, sequence),
        payload,
    )
}

/// One window's stream on its way onto the carrier.
struct OutStream {
    tab: u64,
    codec: Codec,
    width: u16,
    height: u16,
}

/// A host's window streams onto its carrier: the frames its shares send the
/// carrier's viewer, as sealed pictures on each window's track.
#[derive(Default)]
pub struct Outbound {
    streams: HashMap<u64, OutStream>,
}

impl Outbound {
    /// The streams of the windows `tabs` that a new reader started on (or
    /// whose picture could not join its group): each needs its next unit to
    /// be a keyframe. Returns their surfaces; a window with no stream yet
    /// starts with one anyway.
    pub fn readers_started(&self, tabs: &[u64]) -> Vec<u64> {
        self.streams
            .iter()
            .filter(|(_, stream)| tabs.contains(&stream.tab))
            .map(|(surface, _)| *surface)
            .collect()
    }

    /// Carry `message`, a window stream's traffic, on `client`, sealed with
    /// `keys` as the window of the device whose key is `host`. Returns
    /// whether it was carried: a frame of a stream that never started, in a
    /// codec the carrier does not carry, or that nobody shows is not.
    pub fn send(
        &mut self,
        client: &Client,
        host: &[u8; 32],
        keys: &ChannelContentKeys,
        message: &NeighbourMsg,
    ) -> bool {
        match message {
            NeighbourMsg::SurfaceStreamStart {
                surface,
                width,
                height,
                codec,
                tab,
                ..
            } => {
                let Some(codec) = carried(codec) else {
                    tracing::debug!(codec, "the window carrier does not carry this codec");
                    self.streams.remove(surface);
                    return false;
                };
                self.streams.insert(
                    *surface,
                    OutStream {
                        tab: *tab,
                        codec,
                        width: u16::try_from(*width).unwrap_or(u16::MAX),
                        height: u16::try_from(*height).unwrap_or(u16::MAX),
                    },
                );
                true
            }
            NeighbourMsg::SurfaceFrame {
                surface,
                encoded_h264,
                ts,
                seq,
                ..
            } => {
                let Some(stream) = self.streams.get(surface) else {
                    return false;
                };
                let keyframe = stream_codec(stream.codec)
                    .is_some_and(|codec| media::realtime::is_keyframe(codec, encoded_h264));
                let Some(payload) = seal(keys, host, stream.tab, *seq, encoded_h264) else {
                    return false;
                };
                let object = VideoObject {
                    keyframe,
                    codec: stream.codec,
                    layer: Layer::High,
                    sequence: *seq,
                    timestamp_us: ts / 1_000,
                    width: stream.width,
                    height: stream.height,
                    payload,
                };
                client.send_window(stream.tab, &object).is_ok()
            }
            NeighbourMsg::SurfaceStreamEnd { surface, .. } => {
                self.streams.remove(surface);
                true
            }
            _ => false,
        }
    }
}

/// One window's stream as received from the carrier.
#[derive(Default)]
struct InStream {
    /// The codec and size its decoder was started for.
    started: Option<(Codec, u16, u16)>,
}

/// The window streams a viewer receives from its carrier, as the stream
/// messages a direct stream carries.
#[derive(Default)]
pub struct Inbound {
    streams: HashMap<([u8; 32], u64), InStream>,
}

impl Inbound {
    /// The messages `picture`, a picture of window `tab` hosted on the
    /// device whose key is `host` (its peer handle `source_peer`), stands
    /// for: a stream start before the first keyframe and whenever one
    /// changes codec or size, then the frame. Nothing for a picture that
    /// does not open with `keys` or that precedes the stream's first
    /// keyframe.
    pub fn receive(
        &mut self,
        (host, source_peer): (&[u8; 32], u64),
        tab: u64,
        app_id: &str,
        keys: &ChannelContentKeys,
        channel: ChannelId,
        picture: &ReceivedPicture,
    ) -> Vec<NeighbourMsg> {
        let object = &picture.object;
        let Some(frame) = open(keys, channel, host, tab, object.sequence, &object.payload) else {
            tracing::debug!(tab, "a window picture did not open");
            return Vec::new();
        };
        let Some(codec) = stream_codec(object.codec) else {
            return Vec::new();
        };
        let stream = self.streams.entry((*host, tab)).or_default();
        let shape = (object.codec, object.width, object.height);
        let mut messages = Vec::new();
        if object.keyframe && stream.started != Some(shape) {
            stream.started = Some(shape);
            messages.push(NeighbourMsg::SurfaceStreamStart {
                surface: tab,
                source_peer,
                width: u32::from(object.width),
                height: u32::from(object.height),
                codec: codec.wire_label().to_owned(),
                ts: object.timestamp_us.saturating_mul(1_000),
                sender_monotonic_ns_at_send: 0,
                colorimetry: None,
                hdr_static_metadata: None,
                bit_depth: 8,
                app_id: app_id.to_owned(),
                tab,
            });
        }
        if stream.started.is_none() {
            return Vec::new();
        }
        messages.push(NeighbourMsg::SurfaceFrame {
            surface: tab,
            source_peer,
            encoded_h264: frame,
            ts: object.timestamp_us.saturating_mul(1_000),
            seq: object.sequence,
        });
        messages
    }

    /// The end of the stream of window `tab` hosted on `host`, when one
    /// started.
    pub fn end(&mut self, host: &[u8; 32], source_peer: u64, tab: u64) -> Option<NeighbourMsg> {
        let stream = self.streams.remove(&(*host, tab))?;
        stream.started.map(|_| NeighbourMsg::SurfaceStreamEnd {
            surface: tab,
            source_peer,
            ts: 0,
        })
    }
}

enum Carrier {
    Idle,
    Opening(Arc<Mutex<Option<Result<WindowCarrier, String>>>>),
    Open(WindowCarrier),
    /// Could not be joined, or its participation ended: the room's relay
    /// was not connected, or its governor did not grant it. Joined again
    /// once one of those may have changed: the room's document was written
    /// (membership, layout, governance), or a relay of a room became ready.
    Failed {
        tree: u64,
        relays: u64,
    },
}

struct Room {
    carrier: Carrier,
    /// Whether this device hosts windows in the room.
    hosting: bool,
    /// The windows this device shows through the carrier: by host device,
    /// each tab with its app id.
    shown: BTreeMap<[u8; 32], BTreeMap<u64, String>>,
    /// What the carrier was last asked to show, by host device.
    showing: BTreeMap<[u8; 32], Vec<u64>>,
    outbound: Outbound,
    /// The streams whose shares owe the carrier a keyframe, by surface,
    /// until the compositor takes them
    /// ([`ShellNetwork::take_room_window_keyframes`]).
    keyframes: BTreeSet<u64>,
    /// The carrier connection that wakes the loop, by address.
    woken: usize,
    inbound: Inbound,
}

impl Room {
    fn new() -> Self {
        Self {
            carrier: Carrier::Idle,
            hosting: false,
            shown: BTreeMap::new(),
            showing: BTreeMap::new(),
            outbound: Outbound::default(),
            keyframes: BTreeSet::new(),
            woken: 0,
            inbound: Inbound {
                streams: HashMap::new(),
            },
        }
    }

    fn wanted(&self) -> bool {
        self.hosting || !self.shown.is_empty()
    }

    fn client(&self) -> Option<Arc<Client>> {
        match &self.carrier {
            Carrier::Open(carrier) => carrier.client(),
            _ => None,
        }
    }
}

/// Every room's window carrier on this device.
#[derive(Default)]
pub(super) struct State {
    rooms: Mutex<HashMap<RoomId, Room>>,
    /// The generation of the workspace tree the rooms were last given at.
    tree: AtomicU64,
    /// Advances whenever a room's relay session becomes ready.
    relays: AtomicU64,
}

impl State {
    fn inputs(&self) -> (u64, u64) {
        (
            self.tree.load(Ordering::Acquire),
            self.relays.load(Ordering::Acquire),
        )
    }
}

fn endpoint_of(device: &[u8; 32]) -> Option<iroh::EndpointId> {
    iroh::EndpointId::from_bytes(device).ok()
}

fn source_peer_of(device: &[u8; 32]) -> Option<u64> {
    endpoint_of(device).map(|endpoint| crate::network::behaviour::peer_liveness_handle(&endpoint))
}

impl ShellNetwork {
    /// The rooms this device hosts windows in, as of the workspace tree's
    /// `tree_generation`; called as the windows change.
    /// Their carriers are joined while their layouts are shared, so members
    /// that reach this device only through the relay can view them.
    pub fn set_room_window_hosting(&self, rooms: &BTreeSet<RoomId>, tree_generation: u64) {
        self.room_window_carriers
            .tree
            .store(tree_generation, Ordering::Release);
        let mut all = self.room_window_carriers.rooms.lock_recover();
        for room in rooms {
            all.entry(*room).or_insert_with(Room::new).hosting = true;
        }
        for (room, state) in all.iter_mut() {
            if !rooms.contains(room) {
                state.hosting = false;
            }
        }
    }

    /// Show `windows`, given as (room, host device, tab, app id), through
    /// their rooms' carriers: the windows this device shows whose hosts it
    /// reaches only through a relay.
    pub(super) fn show_room_windows_through_carriers(
        &self,
        windows: Vec<(RoomId, [u8; 32], u64, String)>,
    ) {
        let mut shown: HashMap<RoomId, BTreeMap<[u8; 32], BTreeMap<u64, String>>> = HashMap::new();
        for (room, host, tab, app_id) in windows {
            shown
                .entry(room)
                .or_default()
                .entry(host)
                .or_default()
                .insert(tab, app_id);
        }
        let mut all = self.room_window_carriers.rooms.lock_recover();
        for (room, windows) in &shown {
            all.entry(*room).or_insert_with(Room::new).shown = windows.clone();
        }
        for (room, state) in all.iter_mut() {
            if !shown.contains_key(room) {
                state.shown.clear();
            }
        }
    }

    /// The members viewing this device's windows through each room's
    /// carrier, by tab: the carrier stands for them all
    /// ([`carrier_peer`]), at the window's own size.
    pub(super) fn room_carrier_viewers(&self) -> BTreeMap<u64, Vec<RoomWindowViewer>> {
        let all = self.room_window_carriers.rooms.lock_recover();
        let mut viewers: BTreeMap<u64, Vec<RoomWindowViewer>> = BTreeMap::new();
        for (room, state) in all.iter() {
            if !state.hosting {
                continue;
            }
            let Some(client) = state.client() else {
                continue;
            };
            for tab in client.wanted_windows() {
                viewers.entry(tab).or_default().push(RoomWindowViewer {
                    peer: carrier_peer(*room),
                    room: *room,
                    width: 0,
                    height: 0,
                    decode_limits: BTreeMap::new(),
                });
            }
        }
        viewers
    }

    /// A room's relay session became ready: carriers that could not be
    /// joined for want of one are joined again.
    pub(super) fn room_relay_ready(&self) {
        self.room_window_carriers
            .relays
            .fetch_add(1, Ordering::AcqRel);
    }

    /// The window shares that owe a room's carrier a keyframe now, as (the
    /// carrier's viewer handle, surface): a member started reading the
    /// window through the relay, and can start at nothing else. Taken once;
    /// the share answers from the window's last committed buffer at once.
    pub fn take_room_window_keyframes(&self) -> Vec<(u64, u64)> {
        let mut all = self.room_window_carriers.rooms.lock_recover();
        all.iter_mut()
            .flat_map(|(room, state)| {
                let peer = carrier_peer(*room);
                std::mem::take(&mut state.keyframes)
                    .into_iter()
                    .map(move |surface| (peer, surface))
            })
            .collect()
    }

    /// Carry `message` to the carriers among `viewers` of a window of the
    /// room whose topic is `workspace_topic`, returning the viewers left
    /// for the direct route.
    pub(super) fn carry_room_window(
        &self,
        workspace_topic: u64,
        message: &NeighbourMsg,
        viewers: Vec<u64>,
    ) -> Vec<u64> {
        let mut all = self.room_window_carriers.rooms.lock_recover();
        let Some((room, state)) = all
            .iter_mut()
            .find(|(room, state)| state.hosting && room_topic_id(**room) == workspace_topic)
        else {
            return viewers;
        };
        let carrier = carrier_peer(*room);
        if !viewers.contains(&carrier) {
            return viewers;
        }
        let room = *room;
        if let Some(client) = state.client()
            && let Some(keys) =
                crate::shell::live_presence::session_keys(room, windows_channel(room))
        {
            let host = *self.local_peer_id.as_bytes();
            state.outbound.send(&client, &host, &keys, message);
        }
        drop(all);
        viewers
            .into_iter()
            .filter(|peer| *peer != carrier)
            .collect()
    }

    /// Keep each room's carrier joined while wanted and left once not, show
    /// what this device shows through it, and queue the pictures it brought
    /// as the window streams they are. Cheap when nothing changed; called
    /// with every drain of the legacy inbox.
    pub(super) fn pump_room_window_carriers(&self) {
        let mut queued = Vec::new();
        let mut all = self.room_window_carriers.rooms.lock_recover();
        let inputs = self.room_window_carriers.inputs();
        let mut opening = Vec::new();
        for (room, state) in all.iter_mut() {
            if !state.wanted() {
                state.carrier = Carrier::Idle;
                for (host, tabs) in std::mem::take(&mut state.showing) {
                    end_streams(&mut state.inbound, *room, &host, &tabs, &mut queued);
                }
                continue;
            }
            advance(&mut state.carrier, inputs);
            if matches!(state.carrier, Carrier::Idle) {
                opening.push(*room);
            }
            let carrier = std::mem::replace(&mut state.carrier, Carrier::Idle);
            if let Carrier::Open(open) = &carrier
                && let Some(client) = open.client()
            {
                // The loop hears of a reader starting on a window, or a
                // picture arriving, as it happens.
                let installed = Arc::as_ptr(&client) as usize;
                if state.woken != installed {
                    client.set_wake(self.loop_waker());
                    state.woken = installed;
                }
                show(state, open, &client, *room, &mut queued);
                receive(state, open, &client, *room, &mut queued);
                if state.hosting {
                    let started = client.take_window_keyframes();
                    if !started.is_empty() {
                        let surfaces = state.outbound.readers_started(&started);
                        state.keyframes.extend(surfaces);
                    }
                }
            }
            state.carrier = carrier;
        }
        // Without the native owner nothing is joined yet; its open wakes the
        // loop, and they are joined then.
        if !opening.is_empty()
            && let (Some(owner), Ok(runtime)) =
                (self.native_owner_status().open(), self.runtime_handle())
        {
            for room in opening {
                let Some(state) = all.get_mut(&room) else {
                    continue;
                };
                let slot = Arc::new(Mutex::new(None));
                let result = Arc::clone(&slot);
                let (commands, owner) = (self.cmd_tx.clone(), owner.clone());
                let wake = self.loop_waker();
                runtime.spawn(async move {
                    let opened = WindowCarrier::open(commands, owner, room).await;
                    *result.lock_recover() = Some(opened);
                    wake();
                });
                state.carrier = Carrier::Opening(slot);
            }
        }
        all.retain(|_, state| state.wanted() || !matches!(state.carrier, Carrier::Idle));
        drop(all);
        for (topic, host, message) in queued {
            self.queue_legacy_message_from(topic, endpoint_of(&host), message);
        }
    }
}

/// What an open of a carrier ended with, once it ended.
fn take_opened(
    slot: &Mutex<Option<Result<WindowCarrier, String>>>,
) -> Option<Result<WindowCarrier, String>> {
    slot.lock_recover().take()
}

/// Move `carrier` on: an open that ended becomes the carrier or a failure;
/// a carrier that ended fails; a failure becomes idle (joined again) once
/// `inputs`, the tree's generation and the relays' readiness, moved past
/// what it failed at.
fn advance(carrier: &mut Carrier, inputs: (u64, u64)) {
    let (tree, relays) = inputs;
    let next = match carrier {
        Carrier::Opening(slot) => match take_opened(slot) {
            Some(Ok(opened)) => {
                crate::network::media_caps::record_caps(
                    carrier_peer(opened.room()),
                    carrier_capabilities(),
                );
                Some(Carrier::Open(opened))
            }
            Some(Err(error)) => {
                tracing::debug!(%error, "the room's window carrier could not be joined");
                Some(Carrier::Failed { tree, relays })
            }
            None => None,
        },
        Carrier::Open(opened) if opened.ended() => Some(Carrier::Failed { tree, relays }),
        Carrier::Failed {
            tree: failed_tree,
            relays: failed_relays,
        } if (*failed_tree, *failed_relays) != inputs => Some(Carrier::Idle),
        _ => None,
    };
    if let Some(next) = next {
        *carrier = next;
    }
}

type Queued = Vec<(u64, [u8; 32], NeighbourMsg)>;

fn end_streams(
    inbound: &mut Inbound,
    room: RoomId,
    host: &[u8; 32],
    tabs: &[u64],
    queued: &mut Queued,
) {
    let Some(source_peer) = source_peer_of(host) else {
        return;
    };
    for tab in tabs {
        if let Some(end) = inbound.end(host, source_peer, *tab) {
            queued.push((room_topic_id(room), *host, end));
        }
    }
}

/// Ask the carrier for what `state` shows, ending the streams of windows no
/// longer shown.
fn show(
    state: &mut Room,
    carrier: &WindowCarrier,
    client: &Client,
    room: RoomId,
    queued: &mut Queued,
) {
    let wanted: BTreeMap<[u8; 32], Vec<u64>> = state
        .shown
        .iter()
        .map(|(host, tabs)| (*host, tabs.keys().copied().collect()))
        .collect();
    if wanted == state.showing {
        return;
    }
    for (host, tabs) in &state.showing {
        let kept = wanted.get(host);
        let gone: Vec<u64> = tabs
            .iter()
            .copied()
            .filter(|tab| kept.is_none_or(|kept| !kept.contains(tab)))
            .collect();
        if kept.is_none()
            && let Some(namespace) = carrier.namespace_of(*host)
        {
            client.show_windows(&namespace, &[]);
        }
        end_streams(&mut state.inbound, room, host, &gone, queued);
    }
    let mut showing = BTreeMap::new();
    for (host, tabs) in wanted {
        // A host not yet on the carrier is asked once it is.
        if let Some(namespace) = carrier.namespace_of(host) {
            client.show_windows(&namespace, &tabs);
            showing.insert(host, tabs);
        }
    }
    state.showing = showing;
}

/// Queue the pictures the carrier brought of the windows `state` shows.
fn receive(
    state: &mut Room,
    carrier: &WindowCarrier,
    client: &Client,
    room: RoomId,
    queued: &mut Queued,
) {
    let channel = windows_channel(room);
    let mut keys = None;
    let mut hosts: HashMap<String, Option<[u8; 32]>> = HashMap::new();
    while let Some(picture) = client.try_recv_picture() {
        let Some(tab) = picture.window else {
            continue;
        };
        let namespace = &picture.publisher.namespace;
        let host = *hosts.entry(namespace.clone()).or_insert_with(|| {
            state
                .shown
                .keys()
                .copied()
                .find(|host| carrier.namespace_of(*host).as_deref() == Some(namespace.as_str()))
        });
        let Some(host) = host else {
            continue;
        };
        let Some(app_id) = state.shown.get(&host).and_then(|tabs| tabs.get(&tab)) else {
            continue;
        };
        let Some(source_peer) = source_peer_of(&host) else {
            continue;
        };
        if keys.is_none() {
            keys = Some(crate::shell::live_presence::session_keys(room, channel));
        }
        let Some(Some(keys)) = &keys else {
            continue;
        };
        for message in
            state
                .inbound
                .receive((&host, source_peer), tab, app_id, keys, channel, &picture)
        {
            queued.push((room_topic_id(room), host, message));
        }
    }
}

#[cfg(test)]
#[path = "room_window_carriers_tests.rs"]
mod tests;
