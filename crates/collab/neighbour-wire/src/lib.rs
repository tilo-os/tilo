//! Peer-sync wire protocol: a single tagged enum ([`NeighbourMsg`]) carrying
//! every cross-host payload (CRDT sync, presence, surface frames, remote
//! input, capability negotiation). Transport bindings (gossip, a durable
//! host, an in-process hub) operate on it without inspecting the payload.
//!
//! The encoding is postcard over serde's default externally tagged enum
//! representation, so the variant order is the wire discriminant: new
//! variants are appended, never inserted, and new fields go last with
//! `#[serde(default)]`. The golden-byte tests in this crate pin every
//! variant so a peer running an older build keeps decoding what this one
//! sends.
//!
//! The crate is pure data plus pure helpers ([`mod@retention`]); it owns no
//! transport, runtime or state machine.

pub mod colorimetry;
pub mod contribution;
pub mod hdr_metadata;
pub mod presence_slot;
pub mod record_sync;
pub mod retention;
pub mod unit_chain;

pub use presence_slot::PresenceSlot;
pub use record_sync::{RecordSync, on_demand_record, record_part, record_sync};
pub use retention::{
    Retention, app_document_channel, audience, retention, standalone, steward_only,
};

use serde::{Deserialize, Serialize};

/// The device ↔ device interface's one version: the certified handshake
/// (`tilo_relay_protocol::auth`), the relay session's frames and the
/// peer-sync messages they carry. Two devices sync only when theirs are
/// equal; there is no negotiation and nothing kept for an older version
/// (docs/architecture/compatibility.md). `interface.toml` beside this crate
/// holds the schema, and the repository policy fails a change to it that
/// keeps the number.
///
/// [`NeighbourMsg::IdentityHello`] carries it first, at a variant index that
/// never changes ([`IDENTITY_HELLO_INDEX`]), so every version reads another
/// device's number before anything else it says.
pub const PEER_PROTOCOL_VERSION: u32 = 2;

/// [`NeighbourMsg::IdentityHello`]'s postcard variant index, frozen: the one
/// message every version decodes far enough to learn the sender's
/// [`PEER_PROTOCOL_VERSION`].
pub const IDENTITY_HELLO_INDEX: u8 = 11;

/// The [`PEER_PROTOCOL_VERSION`] an encoded [`NeighbourMsg`] says it is
/// from, when it is an identity announcement: read from its frozen prefix,
/// whatever the rest of a message from another version looks like.
#[must_use]
pub fn identity_hello_version(bytes: &[u8]) -> Option<u32> {
    let (&index, rest) = bytes.split_first()?;
    if index != IDENTITY_HELLO_INDEX {
        return None;
    }
    let mut version: u32 = 0;
    for (shift, byte) in rest.iter().take(5).enumerate() {
        version |= u32::from(byte & 0x7f) << (7 * shift);
        if byte & 0x80 == 0 {
            return Some(version);
        }
    }
    None
}

/// Peer identifier — opaque numeric handle assigned per session.
pub type PeerId = u64;

/// The [`PeerId`] a device's messages carry as their publisher: FNV-1a over
/// its Ed25519 device key. A receiving shell drops a message whose publisher
/// is not the handle of the device it authenticated the message from, so
/// every sender (shell, browser, a host signing a compaction) derives it here.
pub fn peer_handle(device_public_key: &[u8; 32]) -> PeerId {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    for b in device_public_key {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Continuation is portable metadata, never an application backup. Keep this
/// limit at the wire boundary so every transport (direct or store-forward)
/// applies the same bound before decoding or retaining it.
pub const MAX_PERSONAL_CONTINUITY_CHECKPOINT_BYTES: usize = 4 * 1024 * 1024;

/// Wayland surface identifier — local handle used for streaming/input routing.
pub type SurfaceId = u64;

/// Presence channel key — namespaces presence updates (e.g. cursor, selection).
pub type PresenceKey = String;

/// The colour the person at the publishing session wears for everyone in
/// the room: one of the twelve presence hues by its stored name (`"red"` …
/// `"pink"`, as `identity.colour` spells it) — their choice, or the one their
/// stable identity derives, so every collaborator paints them the same. Self-
/// asserted and cosmetic, like `user`. `None` when the sender did not say; a
/// receiver then derives one from the sender's handle. A name the receiver
/// does not know is treated as unsaid.
pub type PresenceColour = Option<String>;

/// Tagged envelope for everything that crosses the neighbour boundary.
///
/// `f64` lives inside [`NeighbourMsg::PresenceMoved`] so we cannot derive
/// [`Eq`]; [`PartialEq`] is sufficient for tests and dispatch logic.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub enum NeighbourMsg {
    /// Request: replay every Automerge change to `room` after the included clock.
    ///
    /// `room` is the workspace room uuid string that scopes the
    /// answer — an empty/unparseable value names no room (not a wildcard) and gets
    /// no document back. The transport emits one on peer-join, before it knows
    /// what room the peer is in.
    SyncReq {
        since: Vec<u8>,
        room: String,
        /// Hash of the exact signed bearer invite being redeemed. Ordinary
        /// anti-entropy requests omit it; an authority uses it once to bind
        /// the authenticated transport identity to that invite's role.
        #[serde(default)]
        invite_id: Option<[u8; 32]>,
        /// Fresh per request. Gossip transports deduplicate by content, so
        /// without it a repeated anti-entropy request for unchanged heads
        /// is discarded as already seen and a room that missed its first
        /// answer never catches up.
        #[serde(default)]
        nonce: u64,
    },
    /// Response: a batch of opaque Automerge change blobs for `room`. The blobs
    /// carry the room identity too; the receiver checks the two agree before
    /// merging.
    SyncResp { changes: Vec<Vec<u8>>, room: String },
    /// Peer's presence cursor moved within `key`'s coordinate space.
    ///
    /// `peer` identifies the publishing **session** — all windows under one
    /// session share one cursor, and the receiver's own per-connection ids never
    /// leave the host that minted them. `user` is the session's claim of who is
    /// there (the sender's local user id), not something the
    /// receiver infers; empty falls back to showing the session handle.
    /// `colour` is the colour the person there wears ([`PresenceColour`]).
    PresenceMoved {
        peer: PeerId,
        user: String,
        key: PresenceKey,
        point: (f64, f64),
        ts: u64,
        #[serde(default)]
        colour: PresenceColour,
    },
    /// Peer's presence cursor disappeared from `key`'s coordinate space.
    PresenceCleared { peer: PeerId, key: PresenceKey },
    /// One encoded access unit (H.264/HEVC), frame (VP9) or temporal unit
    /// (AV1) for a shared surface, in the codec the preceding
    /// [`Self::SurfaceStreamStart`] named. `encoded_h264` keeps its historical
    /// name; the field carries whichever codec was announced.
    ///
    /// `ts` is the owner's monotonic-clock timestamp in nanoseconds; receivers
    /// diff against their own monotonic-now for motion-to-photon latency, since
    /// wall-clock (`SystemTime`) would drift under NTP on the WAN case.
    SurfaceFrame {
        surface: SurfaceId,
        /// Hashed Iroh endpoint of the stream owner. Bound to the
        /// authenticated publisher before encoded bytes are dispatched.
        #[serde(default)]
        source_peer: PeerId,
        encoded_h264: Vec<u8>,
        ts: u64,
        /// The unit's place in the owner's encoded sequence, one more than
        /// the unit it follows. Delta units reference earlier ones, so a
        /// receiver that sees a gap (a unit lost on the way) must not feed
        /// the next delta to its decoder: it waits for the next keyframe.
        seq: u64,
    },
    /// Explicit start-of-stream announcement for a shared surface.
    ///
    /// Distinct from `SurfaceFrame` so late joiners can size a decoder /
    /// receive-side view before the first NALU arrives,
    /// avoiding a dropped frame. The codec is chosen by the sender from the
    /// receivers' [`Self::MediaCapabilities`]: `"av1"` or `"vp9"` when every
    /// receiver decodes it in hardware, `"h264"` otherwise (and always for a
    /// receiver that never advertised capabilities), `"hevc-main10"` for HDR.
    SurfaceStreamStart {
        surface: SurfaceId,
        /// Hashed Iroh `PeerId` of the owner; distinguishes concurrent shares in the
        /// same workspace.
        source_peer: PeerId,
        width: u32,
        height: u32,
        /// Codec label: `"h264"`, `"hevc-main10"`, `"vp9"` or `"av1"`.
        /// A receiver ignores (and logs) a label it does not know, which is
        /// why a sender only picks a codec every receiver advertised.
        codec: String,
        ts: u64,
        /// Sender's monotonic-clock reading when the message left the encode task
        /// (same origin as [`Self::SurfaceFrame::ts`]).
        ///
        /// The receiver computes `offset = receiver_now_ns - sender_monotonic_ns_at_send`
        /// on decode and applies it to every later `ts` via
        /// the receiver's latency metrics to get
        /// real cross-host motion-to-photon numbers despite unrelated monotonic
        /// origins.
        ///
        /// A value of `0` means the sender cannot provide a clock offset;
        /// the receiver then skips
        /// cross-host latency instead of reporting a bogus value.
        sender_monotonic_ns_at_send: u64,
        /// Colorimetry tag; `None` means the receiver assumes SDR BT.709.
        colorimetry: Option<crate::colorimetry::Colorimetry>,
        /// HDR10 static metadata (SMPTE ST 2086 + CTA-861.3) for HDR pipelines;
        /// `None` on SDR streams. Used by the receiver's tone-mapper to size
        /// compression correctly on SDR displays.
        hdr_static_metadata: Option<crate::hdr_metadata::Hdr10Metadata>,
        /// Codec target bit depth; `0` means unspecified — receivers infer 8-bit
        /// from `codec = "h264"`, else 8 or 10 (HEVC Main10).
        bit_depth: u8,
        /// Desktop app-id of the shared legacy window. Kept last so the
        /// serde default accepts pre-field postcard payloads.
        #[serde(default)]
        app_id: String,
        /// The workspace tab holding the window, the same id on every device
        /// of the room, so a viewer shows the stream in exactly that tab
        /// even when several windows of one app are open. `0` when the
        /// sender does not name one.
        #[serde(default)]
        tab: u64,
    },
    /// Explicit end-of-stream (owner tore down the share, or the surface
    /// unmapped). Receivers drop their `PeerView` + decoder on receipt, instead of
    /// waiting out the 15s heartbeat-eviction path that implicit end-of-stream
    /// (owner vanishes from gossip) still relies on.
    SurfaceStreamEnd {
        surface: SurfaceId,
        source_peer: PeerId,
        ts: u64,
    },
    /// Serialized input event from `peer` targeting `surface`.
    RemoteInput {
        peer: PeerId,
        surface: SurfaceId,
        event: Vec<u8>,
    },
    /// Retired: `peer` would like permission `capability`. No build produces
    /// it and receivers refuse it ([`NeighbourMsg::is_retired`]); it stays
    /// only so every later Postcard discriminant keeps its value.
    PermissionRequest { peer: PeerId, capability: String },
    /// `peer` was granted permission `capability`.
    PermissionGrant { peer: PeerId, capability: String },
    /// Liveness beacon of builds that evict a direct peer after 15 s without
    /// traffic and beat every 5 s on every joined workspace; `ts` is their
    /// unix-millis. Current builds keep liveness on the transport and send
    /// one only to a neighbour of such a build, numbering it with a `ts`
    /// below [`Self::COMPAT_HEARTBEAT_TS_LIMIT`] so that it is not mistaken
    /// for one (see [`Self::is_timed_heartbeat`]). `peer` is the hashed Iroh
    /// `PeerId`.
    Heartbeat { peer: PeerId, ts: u64 },
    /// Authenticated identity chain for the transport peer on this workspace.
    ///
    /// The certificate binds the Iroh endpoint/device key to a user root. The
    /// additive, root-signed revocations let receivers reject a lost device
    /// before any of its collaboration payload reaches an app or replica.
    ///
    /// `protocol` is the sender's [`PEER_PROTOCOL_VERSION`], first, at the
    /// frozen [`IDENTITY_HELLO_INDEX`]: a receiver at another version admits
    /// nothing from the sender and knows which of the two is behind.
    IdentityHello {
        protocol: u32,
        cert: Box<tilo_user_identity::DeviceCert>,
        #[serde(default)]
        revocations: Vec<tilo_user_identity::RevocationEntry>,
    },
    /// One typed, signed document mutation crossing the Iroh boundary.
    /// Recipients authenticate the transport device, validate the envelope and
    /// decode its [`collab_types::DocumentMutation`] before local fan-out.
    /// Boxed so this variant does not bloat `NeighbourMsg`'s stack size.
    AppDocument {
        /// Hashed Iroh `PeerId` of the publisher; used to filter self-published
        /// gossip echoes.
        peer: PeerId,
        /// App-id of the originating app, first routing-key component.
        app_id: String,
        /// Cluster id — second half of the routing key.
        cluster_id: String,
        /// Wire name of the originating document's record — third routing-key
        /// component; empty means the sender announced no record. A local
        /// record id that embeds a filesystem path is never sent: it is
        /// replaced by its sealed, room-scoped name (the compositor's
        /// `network::wire_record`), which receivers map back to their own
        /// local id.
        record_id: String,
        /// The signed-op envelope; the receiving shell's validator decides whether
        /// to fan it to local apps or drop it with an audit-log entry.
        envelope: Box<collab_types::SignedOp>,
    },
    /// App-level presence crossing the host boundary. Presence is cluster scoped,
    /// so a participant remains visible while moving between apps in one room.
    /// `colour` is the colour the person there wears ([`PresenceColour`]).
    AppPresenceV2 {
        peer: PeerId,
        app_id: String,
        cluster_id: String,
        presence: collab_types::PeerPresence,
        #[serde(default)]
        colour: PresenceColour,
    },
    /// Cooperative scrollback request, broadcast over gossip when a peer's
    /// local broker has no in-memory scrollback for a range an app asked for.
    /// Any peer holding the range replies with
    /// [`NeighbourMsg::ScrollbackResponse`].
    ///
    /// `requester` is the local peer hash used to target the response
    /// (`ScrollbackResponse::responder`). v1 doesn't dedupe concurrent identical
    /// requests — the case is rare/single-shot, so amplification isn't a
    /// concern.
    ScrollbackRequest {
        /// The artifact whose log to fetch.
        artifact: collab_types::ArtifactId,
        /// Exclusive upper bound on `seq`, same semantics as
        /// `CollabRequest::FetchScrollback::before_seq`.
        before_seq: u64,
        /// Page size cap, matching `CollabRequest::FetchScrollback::max` so the
        /// cooperative path doesn't re-cap.
        max: u16,
        /// Hashed Iroh peer id of the requester; receivers use it to skip their
        /// own echoed requests.
        requester: PeerId,
    },
    /// Response to a [`NeighbourMsg::ScrollbackRequest`]: the responding broker
    /// forwards `ops` from its local op log; the requester delivers them via
    /// `CollabEvent::ScrollbackBatch`.
    ///
    /// `responder` is for audit/replay diagnostics only — it plays no role in
    /// validation, since every envelope in `ops` is independently signed.
    ScrollbackResponse {
        /// Same as the request's `artifact`, repeated so the receiver can route
        /// without matching on a request-id.
        artifact: collab_types::ArtifactId,
        /// The page of validated ops the responder is sharing; empty means "I don't
        /// have the range either" and is treated as a non-answer.
        ops: Vec<Box<collab_types::SignedOp>>,
        /// Hashed Iroh peer id of the answering broker.
        responder: PeerId,
    },
    /// Retired first generation of [`Self::IdentityAnnouncementV2`]: a
    /// root-signed device certificate announced on one room, without the
    /// revocations that keep a lost device out. No build produces it and
    /// receivers refuse it ([`NeighbourMsg::is_retired`]); it stays only so
    /// every later Postcard discriminant keeps its value.
    IdentityAnnouncement {
        room: String,
        device_cert: Box<tilo_user_identity::DeviceCert>,
    },
    /// V2 identity announcement carrying additive root-signed revocations.
    /// Appended rather than changing V1's Postcard payload layout.
    IdentityAnnouncementV2 {
        room: String,
        device_cert: Box<tilo_user_identity::DeviceCert>,
        /// Additive root-signed revocations for the certificate's human root.
        /// The room governor verifies and retains them independently of the
        /// announcing device, so omission can never resurrect a device.
        revocations: Vec<tilo_user_identity::RevocationEntry>,
    },
    /// Ask another certified device of the same person for its latest
    /// continuation checkpoints. This rides a user-root-derived topic, never
    /// a collaboration room: personal session state must not become visible to
    /// collaborators merely because they share a workspace.
    ///
    /// Appended to preserve all existing Postcard discriminants.
    PersonalContinuityRequest {
        user_public_key: [u8; 32],
        requester: PeerId,
    },
    /// A bounded, validated aggregate of per-device
    /// personal continuity snapshots.
    /// The receiving shell accepts it only when the authenticated transport
    /// certificate has `user_public_key`. Certified devices may relay the
    /// aggregate so an always-on peer retains the latest state for all of them.
    PersonalContinuityCheckpoint {
        user_public_key: [u8; 32],
        publisher: PeerId,
        snapshot: Vec<u8>,
    },
    /// One remote-action frame (`tilo_actions::remote::RemoteActionFrame`
    /// bytes) for exactly one authenticated device: a request to list,
    /// invoke, observe or cancel its actions, or the answer to one.
    ///
    /// It travels only on an exact-target route, never workspace-wide
    /// gossip, so arguments and results are not disclosed to the rest of
    /// the room. The receiver takes the caller from the authenticated
    /// transport; nothing in the frame names it. Appended to preserve all
    /// existing Postcard discriminants.
    RemoteAction { frame: Vec<u8> },
    /// Which video codecs a peer can encode and decode, split by whether
    /// hardware does it. Broadcast with the heartbeat so a screen-share sender
    /// can pick a codec every viewer takes: a peer that never sends this is
    /// assumed to decode H.264 only. Labels are the
    /// [`Self::SurfaceStreamStart::codec`] vocabulary; unknown ones are
    /// ignored. Appended to preserve every existing Postcard discriminant.
    MediaCapabilities {
        peer: PeerId,
        hardware_decode: Vec<String>,
        software_decode: Vec<String>,
        hardware_encode: Vec<String>,
        software_encode: Vec<String>,
    },
    /// One governance frame of a channel in `room`, published by the
    /// channel's steward. Its Welcome, commit and key-history bytes are
    /// opaque MLS material: sealed to their recipients, never content.
    /// Receivers apply it only when the steward's signature and the hash
    /// chain check out, so replaying or reordering it is harmless. Appended
    /// to preserve every existing Postcard discriminant.
    MlsGovernance {
        room: String,
        frame: Box<collab_types::MlsGovernanceFrameV1>,
        /// Chosen by the sender for each delivery attempt. Redelivering the
        /// same content with a new value keeps transports that drop
        /// byte-identical messages from swallowing the retry; receivers
        /// ignore it.
        delivery: u64,
    },
    /// A device asking a channel's steward for membership, with a key
    /// package signed by the device's certified key. Appended to preserve
    /// every existing Postcard discriminant.
    MlsAccessRequest {
        room: String,
        request: Box<collab_types::MlsAccessRequestV1>,
        /// Chosen by the sender for each delivery attempt. Redelivering the
        /// same content with a new value keeps transports that drop
        /// byte-identical messages from swallowing the retry; receivers
        /// ignore it.
        delivery: u64,
    },
    /// A device's signed, cumulative acknowledgement of a channel's
    /// governance frames. Appended to preserve every existing Postcard
    /// discriminant.
    MlsGovernanceAck {
        room: String,
        ack: Box<collab_types::MlsGovernanceAckV1>,
        /// Chosen by the sender for each delivery attempt. Redelivering the
        /// same content with a new value keeps transports that drop
        /// byte-identical messages from swallowing the retry; receivers
        /// ignore it.
        delivery: u64,
    },
    /// The person's workspace directory as one of their devices holds it:
    /// a device-signed Automerge snapshot of which workspaces the person
    /// has and which room carries each. Travels only on the person's own
    /// topic; the receiver merges it only when the authenticated transport
    /// certificate and the update's author are devices of
    /// `user_public_key`. Appended to preserve every existing Postcard
    /// discriminant.
    PersonalWorkspaces {
        user_public_key: [u8; 32],
        update: Box<tilo_user_identity::DirectoryUpdate>,
    },
    /// A device asking the room for one piece of content a record of
    /// `app_id` in room `cluster_id` refers to, by its BLAKE3 `hash`. Rides
    /// the room's topic, so only peers admitted to read the room see it;
    /// any of them holding the content answers with
    /// [`Self::RecordContentPiece`]. Appended to preserve every existing
    /// Postcard discriminant.
    RecordContentWant {
        requester: PeerId,
        app_id: String,
        cluster_id: String,
        hash: String,
    },
    /// One bounded piece of a record's content, answering `requester`'s
    /// [`Self::RecordContentWant`]: `bytes` start at `offset` of the
    /// `total`-byte content named by `hash`. The receiver keeps the content
    /// only once every piece arrived and the bytes hash to `hash`. Appended
    /// to preserve every existing Postcard discriminant.
    RecordContentPiece {
        responder: PeerId,
        requester: PeerId,
        app_id: String,
        cluster_id: String,
        hash: String,
        total: u64,
        offset: u64,
        bytes: Vec<u8>,
    },
    /// [`Self::MlsAccessRequest`] with the route of the record the channel
    /// carries, so a steward can decide Read on that record, a Deny over its
    /// app or any pattern covering it included. The route proves itself: the
    /// steward refuses it unless it hashes, with `room`, to the request's
    /// channel ([`collab_types::ChannelRoute::proves`]). A device sends both
    /// forms while a steward may predate this one. Appended to preserve every
    /// existing Postcard discriminant.
    MlsRoutedAccessRequest {
        room: String,
        request: Box<collab_types::MlsAccessRequestV1>,
        route: Box<collab_types::ChannelRoute>,
        /// Chosen by the sender for each delivery attempt, as for
        /// [`Self::MlsAccessRequest`]; receivers ignore it.
        delivery: u64,
    },
    /// Viewer `peer` cannot continue decoding `surface`: a unit before
    /// [`Self::SurfaceFrame::seq`] `seq` never reached it, or its decoder
    /// rejected one, and the deltas after reference what it lacks. The owner
    /// answers with a keyframe instead of leaving the viewer frozen until its
    /// periodic one. `seq` is the unit the viewer could not use; the owner
    /// ignores a request its last keyframe already answers. Appended to
    /// preserve every existing Postcard discriminant.
    SurfaceKeyframeRequest {
        peer: PeerId,
        surface: SurfaceId,
        seq: u64,
    },
}

impl NeighbourMsg {
    /// Postcard variant index of [`NeighbourMsg::AppDocument`]. Variant order
    /// is wire-frozen (peers and hosts persist and exchange these frames), and
    /// a unit test pins this value to a real encoding.
    pub const APP_DOCUMENT_TAG: u8 = 12;

    /// Whether `bytes` encode an [`NeighbourMsg::AppDocument`], reading only
    /// the postcard variant tag and not the variant's fields.
    ///
    /// Postcard writes a variant index as a varint `u32` before the fields.
    /// An index below `0x80` is that single byte with its continuation bit
    /// clear, so a first byte equal to [`Self::APP_DOCUMENT_TAG`] is exactly
    /// that index and nothing longer. The fields are not checked: a frame
    /// tagged `AppDocument` whose fields do not decode still answers `true`.
    pub fn is_app_document(bytes: &[u8]) -> bool {
        const _: () = assert!(NeighbourMsg::APP_DOCUMENT_TAG < 0x80);
        bytes.first() == Some(&Self::APP_DOCUMENT_TAG)
    }

    /// Whether this is a retired wire generation that no current build
    /// produces. Receivers refuse such a message once, where it enters,
    /// before any handler or authorization sees it; the variant itself stays
    /// only because removing it would shift every later Postcard
    /// discriminant.
    pub fn is_retired(&self) -> bool {
        match self {
            // Superseded by `IdentityAnnouncementV2`, whose revocations a
            // receiver must see before trusting the certificate.
            Self::IdentityAnnouncement { .. }
            // Never acted on; control consent rides `PermissionGrant`.
            | Self::PermissionRequest { .. } => true,
            Self::SyncReq { .. }
            | Self::SyncResp { .. }
            | Self::PresenceMoved { .. }
            | Self::PresenceCleared { .. }
            | Self::SurfaceFrame { .. }
            | Self::SurfaceStreamStart { .. }
            | Self::SurfaceStreamEnd { .. }
            | Self::RemoteInput { .. }
            | Self::PermissionGrant { .. }
            | Self::Heartbeat { .. }
            | Self::IdentityHello { .. }
            | Self::AppDocument { .. }
            | Self::AppPresenceV2 { .. }
            | Self::ScrollbackRequest { .. }
            | Self::ScrollbackResponse { .. }
            | Self::IdentityAnnouncementV2 { .. }
            | Self::PersonalContinuityRequest { .. }
            | Self::PersonalContinuityCheckpoint { .. }
            | Self::RemoteAction { .. }
            | Self::MediaCapabilities { .. }
            | Self::MlsGovernance { .. }
            | Self::MlsAccessRequest { .. }
            | Self::MlsRoutedAccessRequest { .. }
            | Self::MlsGovernanceAck { .. }
            | Self::PersonalWorkspaces { .. }
            | Self::RecordContentWant { .. }
            | Self::RecordContentPiece { .. }
            | Self::SurfaceKeyframeRequest { .. } => false,
        }
    }

    /// A [`Self::Heartbeat`] `ts` below this (September 2001 in unix
    /// milliseconds) is no time: it numbers a heartbeat a transport-liveness
    /// build sends only for an older neighbour's sake. No build ever sent a
    /// real time this early, and none read the field.
    pub const COMPAT_HEARTBEAT_TS_LIMIT: u64 = 1_000_000_000_000;

    /// Whether this is the timed heartbeat of a build that keeps liveness by
    /// heartbeat and evicts a peer that stops beating: the sign that its
    /// neighbours must keep beating too.
    pub fn is_timed_heartbeat(&self) -> bool {
        matches!(self, Self::Heartbeat { ts, .. } if *ts >= Self::COMPAT_HEARTBEAT_TS_LIMIT)
    }

    /// The variant's name, for diagnostics and traffic tallies. Never a
    /// wire value: the postcard discriminant is.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::SyncReq { .. } => "SyncReq",
            Self::SyncResp { .. } => "SyncResp",
            Self::PresenceMoved { .. } => "PresenceMoved",
            Self::PresenceCleared { .. } => "PresenceCleared",
            Self::SurfaceFrame { .. } => "SurfaceFrame",
            Self::SurfaceStreamStart { .. } => "SurfaceStreamStart",
            Self::SurfaceStreamEnd { .. } => "SurfaceStreamEnd",
            Self::RemoteInput { .. } => "RemoteInput",
            Self::PermissionRequest { .. } => "PermissionRequest",
            Self::PermissionGrant { .. } => "PermissionGrant",
            Self::Heartbeat { .. } => "Heartbeat",
            Self::IdentityHello { .. } => "IdentityHello",
            Self::AppDocument { .. } => "AppDocument",
            Self::AppPresenceV2 { .. } => "AppPresenceV2",
            Self::ScrollbackRequest { .. } => "ScrollbackRequest",
            Self::ScrollbackResponse { .. } => "ScrollbackResponse",
            Self::IdentityAnnouncement { .. } => "IdentityAnnouncement",
            Self::IdentityAnnouncementV2 { .. } => "IdentityAnnouncementV2",
            Self::PersonalContinuityRequest { .. } => "PersonalContinuityRequest",
            Self::PersonalContinuityCheckpoint { .. } => "PersonalContinuityCheckpoint",
            Self::RemoteAction { .. } => "RemoteAction",
            Self::MediaCapabilities { .. } => "MediaCapabilities",
            Self::MlsGovernance { .. } => "MlsGovernance",
            Self::MlsAccessRequest { .. } => "MlsAccessRequest",
            Self::MlsGovernanceAck { .. } => "MlsGovernanceAck",
            Self::PersonalWorkspaces { .. } => "PersonalWorkspaces",
            Self::RecordContentWant { .. } => "RecordContentWant",
            Self::RecordContentPiece { .. } => "RecordContentPiece",
            Self::MlsRoutedAccessRequest { .. } => "MlsRoutedAccessRequest",
            Self::SurfaceKeyframeRequest { .. } => "SurfaceKeyframeRequest",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Syntactically valid but unsigned; only the postcard framing needs to
    /// round-trip.
    fn dummy_envelope() -> Box<collab_types::SignedOp> {
        use collab_types::{AttestationId, Ed25519PublicKey, OpKind, SignedOp};
        use std::time::SystemTime;
        Box::new(SignedOp::new_unsigned(
            vec![0xDE, 0xAD, 0xBE, 0xEF],
            artifact(0x11),
            Ed25519PublicKey::from_bytes([7u8; 32]),
            AttestationId(uuid::Uuid::from_bytes([0x22; 16])),
            OpKind::Change,
            SystemTime::UNIX_EPOCH,
            42,
        ))
    }

    /// Shells in the field check publishers against this exact fold, so it
    /// is pinned like the encodings.
    #[test]
    fn peer_handle_is_fnv1a_of_the_device_key() {
        assert_eq!(peer_handle(&[7; 32]), 0xf160_fd08_b6d9_aac5);
    }

    /// Room id shared by every fixture that names one.
    const ROOM: &str = "6b3f9a10-0000-4000-8000-00000000000c";

    /// Fixed artifact ids keep the golden encodings reproducible.
    fn artifact(fill: u8) -> collab_types::ArtifactId {
        collab_types::ArtifactId(uuid::Uuid::from_bytes([fill; 16]))
    }

    fn device_cert() -> Box<tilo_user_identity::DeviceCert> {
        Box::new(tilo_user_identity::DeviceCert {
            user_public_key: [1; 32],
            device_public_key: [2; 32],
            device_label: "Laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 10,
            expires_at_ns: None,
            signature: [3; 64],
        })
    }

    /// Every version reads another device's version from its identity
    /// announcement: the variant index and the leading field never move.
    #[test]
    fn an_identity_hello_says_its_version_first_at_a_frozen_index() {
        for protocol in [PEER_PROTOCOL_VERSION, PEER_PROTOCOL_VERSION + 1, 300] {
            let bytes = postcard::to_allocvec(&NeighbourMsg::IdentityHello {
                protocol,
                cert: device_cert(),
                revocations: Vec::new(),
            })
            .unwrap();
            assert_eq!(bytes[0], IDENTITY_HELLO_INDEX);
            assert_eq!(identity_hello_version(&bytes), Some(protocol));
        }
        let beat = postcard::to_allocvec(&NeighbourMsg::Heartbeat { peer: 1, ts: 2 }).unwrap();
        assert_eq!(identity_hello_version(&beat), None);
        assert_eq!(identity_hello_version(&[]), None);
    }

    /// Every [`NeighbourMsg`] variant, split into thematic groups so no one
    /// function runs long — the round-trip test below exercises the
    /// concatenation.
    fn fixtures() -> Vec<(&'static str, NeighbourMsg)> {
        let mut all = fixtures_sync_presence_and_streams();
        all.extend(fixtures_input_permissions_and_identity());
        all.extend(fixtures_app_crdt_and_scrollback());
        all.extend(fixtures_channel_governance());
        all
    }

    /// Channel governance, with fixed ids and signatures so the golden
    /// bytes stay reproducible (these fixtures are not verified).
    fn fixtures_channel_governance() -> Vec<(&'static str, NeighbourMsg)> {
        use collab_types::{
            ChannelId, Ed25519PublicKey, MlsAccessRequestV1, MlsGovernanceAckV1,
            MlsGovernanceAction, MlsGovernanceFrameV1, Signature,
        };
        let channel = ChannelId(uuid::Uuid::from_bytes([0x55; 16]));
        vec![
            (
                "channel governance frame",
                NeighbourMsg::MlsGovernance {
                    room: ROOM.into(),
                    frame: Box::new(MlsGovernanceFrameV1 {
                        version: 1,
                        transaction_id: [6; 32],
                        channel,
                        membership_revision: 2,
                        previous_frame_digest: [7; 32],
                        epoch: 3,
                        action: MlsGovernanceAction::Remove {
                            removed: Ed25519PublicKey::from_bytes([8; 32]),
                            commit: vec![0xC0, 0x33],
                        },
                        signed_by: Ed25519PublicKey::from_bytes([9; 32]),
                        signature: Signature::from_bytes([0xAB; 64]),
                    }),
                    delivery: 7,
                },
            ),
            (
                "channel access request",
                NeighbourMsg::MlsAccessRequest {
                    room: ROOM.into(),
                    request: Box::new(MlsAccessRequestV1 {
                        version: 1,
                        channel,
                        device: Ed25519PublicKey::from_bytes([2; 32]),
                        key_package: vec![0x4B, 0x50],
                        issued_at: 1_700_000_000,
                        signature: Signature::from_bytes([0xCD; 64]),
                    }),
                    delivery: 8,
                },
            ),
            (
                "channel governance ack",
                NeighbourMsg::MlsGovernanceAck {
                    room: ROOM.into(),
                    ack: Box::new(MlsGovernanceAckV1 {
                        version: 1,
                        channel,
                        device: Ed25519PublicKey::from_bytes([2; 32]),
                        through_digest: [0xEE; 32],
                        signature: Signature::from_bytes([0xEF; 64]),
                    }),
                    delivery: 9,
                },
            ),
            (
                "channel routed access request",
                NeighbourMsg::MlsRoutedAccessRequest {
                    room: ROOM.into(),
                    request: Box::new(MlsAccessRequestV1 {
                        version: 1,
                        channel,
                        device: Ed25519PublicKey::from_bytes([2; 32]),
                        key_package: vec![0x4B, 0x50],
                        issued_at: 1_700_000_000,
                        signature: Signature::from_bytes([0xCD; 64]),
                    }),
                    route: Box::new(collab_types::ChannelRoute::new("so.tilo.sheets", "payroll")),
                    delivery: 10,
                },
            ),
        ]
    }

    fn fixtures_sync_presence_and_streams() -> Vec<(&'static str, NeighbourMsg)> {
        use crate::colorimetry::Colorimetry;
        use crate::hdr_metadata::Hdr10Metadata;

        vec![
            (
                "sync request",
                NeighbourMsg::SyncReq {
                    since: vec![1, 2, 3],
                    room: ROOM.into(),
                    invite_id: Some([9; 32]),
                    nonce: 7,
                },
            ),
            (
                "sync response",
                NeighbourMsg::SyncResp {
                    changes: vec![vec![0xAA, 0xBB], vec![0xCC]],
                    room: ROOM.into(),
                },
            ),
            (
                "presence moved",
                NeighbourMsg::PresenceMoved {
                    peer: 42,
                    user: "bob".into(),
                    key: "cursor".into(),
                    point: (12.5, -3.25),
                    ts: 1_700_000_000,
                    colour: Some("violet".into()),
                },
            ),
            (
                "presence cleared",
                NeighbourMsg::PresenceCleared {
                    peer: 42,
                    key: "cursor".into(),
                },
            ),
            (
                "surface frame",
                NeighbourMsg::SurfaceFrame {
                    surface: 7,
                    source_peer: 0xABCD,
                    encoded_h264: vec![0, 0, 0, 1, 0x67],
                    ts: 1_700_000_001,
                    seq: 3,
                },
            ),
            (
                "SDR stream start",
                NeighbourMsg::SurfaceStreamStart {
                    surface: 7,
                    source_peer: 0xABCD,
                    app_id: "org.example.Editor".into(),
                    width: 1280,
                    height: 720,
                    codec: "h264".into(),
                    ts: 1_700_000_002,
                    sender_monotonic_ns_at_send: 1_700_000_002,
                    colorimetry: None,
                    hdr_static_metadata: None,
                    bit_depth: 0,
                    tab: 0,
                },
            ),
            (
                "HDR stream start",
                NeighbourMsg::SurfaceStreamStart {
                    surface: 8,
                    source_peer: 0xABCD,
                    app_id: "org.example.HdrEditor".into(),
                    width: 1920,
                    height: 1080,
                    codec: "hevc-main10".into(),
                    ts: 1_700_000_003,
                    sender_monotonic_ns_at_send: 1_700_000_003,
                    colorimetry: Some(Colorimetry::hdr10()),
                    hdr_static_metadata: Some(Hdr10Metadata::reference_bt2020()),
                    bit_depth: 10,
                    tab: 0,
                },
            ),
            (
                "stream end",
                NeighbourMsg::SurfaceStreamEnd {
                    surface: 7,
                    source_peer: 0xABCD,
                    ts: 1_700_000_004,
                },
            ),
            (
                "surface keyframe request",
                NeighbourMsg::SurfaceKeyframeRequest {
                    peer: 3,
                    surface: 7,
                    seq: 300,
                },
            ),
        ]
    }

    fn fixtures_input_permissions_and_identity() -> Vec<(&'static str, NeighbourMsg)> {
        vec![
            (
                "remote input",
                NeighbourMsg::RemoteInput {
                    peer: 3,
                    surface: 7,
                    event: vec![0xDE, 0xAD, 0xBE, 0xEF],
                },
            ),
            (
                "permission request",
                NeighbourMsg::PermissionRequest {
                    peer: 5,
                    capability: "share-screen".into(),
                },
            ),
            (
                "permission grant",
                NeighbourMsg::PermissionGrant {
                    peer: 5,
                    capability: "share-screen".into(),
                },
            ),
            (
                "heartbeat",
                NeighbourMsg::Heartbeat {
                    peer: 0xDEAD_BEEF,
                    ts: 1_700_000_005,
                },
            ),
            (
                "identity hello",
                NeighbourMsg::IdentityHello {
                    protocol: crate::PEER_PROTOCOL_VERSION,
                    cert: device_cert(),
                    revocations: Vec::new(),
                },
            ),
            (
                "identity announcement",
                NeighbourMsg::IdentityAnnouncement {
                    room: ROOM.into(),
                    device_cert: device_cert(),
                },
            ),
            (
                "identity announcement v2",
                NeighbourMsg::IdentityAnnouncementV2 {
                    room: ROOM.into(),
                    device_cert: device_cert(),
                    revocations: Vec::new(),
                },
            ),
            (
                "personal continuity request",
                NeighbourMsg::PersonalContinuityRequest {
                    user_public_key: [7; 32],
                    requester: 0x00AB_CDEF,
                },
            ),
            (
                "personal continuity checkpoint",
                NeighbourMsg::PersonalContinuityCheckpoint {
                    user_public_key: [7; 32],
                    publisher: 0x00AB_CDEF,
                    snapshot: vec![1, 2, 3],
                },
            ),
            (
                "personal workspaces",
                NeighbourMsg::PersonalWorkspaces {
                    user_public_key: [7; 32],
                    update: Box::new(tilo_user_identity::DirectoryUpdate {
                        author_device_public_key: [2; 32],
                        authored_at_ns: 10,
                        delta: vec![1, 2, 3],
                        signature: [3; 64],
                    }),
                },
            ),
            (
                "remote action",
                NeighbourMsg::RemoteAction {
                    frame: b"{}".to_vec(),
                },
            ),
            (
                "media capabilities",
                NeighbourMsg::MediaCapabilities {
                    peer: 0xABCD,
                    hardware_decode: vec!["av1".into(), "vp9".into()],
                    software_decode: vec!["h264".into()],
                    hardware_encode: vec!["av1".into()],
                    software_encode: vec!["vp9".into()],
                },
            ),
        ]
    }

    fn fixtures_app_crdt_and_scrollback() -> Vec<(&'static str, NeighbourMsg)> {
        use collab_types::{PeerPresence, PeerPresencePayload};

        vec![
            (
                "signed app document",
                NeighbourMsg::AppDocument {
                    peer: 0x00AB_CDEF,
                    app_id: "notes".into(),
                    cluster_id: ROOM.into(),
                    record_id: "uuid:1c2f".into(),
                    envelope: dummy_envelope(),
                },
            ),
            (
                "app presence",
                NeighbourMsg::AppPresenceV2 {
                    peer: 0x00AB_CDEF,
                    app_id: "notes".into(),
                    cluster_id: ROOM.into(),
                    presence: PeerPresence {
                        key: vec![4, 5],
                        payload: PeerPresencePayload::Selection { start: 3, end: 8 },
                    },
                    colour: Some("teal".into()),
                },
            ),
            (
                "record content want",
                NeighbourMsg::RecordContentWant {
                    requester: 0x00AB_CDEF,
                    app_id: "so.tilo.slides".into(),
                    cluster_id: "room".into(),
                    hash: "ab".into(),
                },
            ),
            (
                "record content piece",
                NeighbourMsg::RecordContentPiece {
                    responder: 0x123_456,
                    requester: 0x00AB_CDEF,
                    app_id: "so.tilo.slides".into(),
                    cluster_id: "room".into(),
                    hash: "ab".into(),
                    total: 3,
                    offset: 0,
                    bytes: vec![1, 2, 3],
                },
            ),
            (
                "scrollback request",
                NeighbourMsg::ScrollbackRequest {
                    artifact: artifact(0x33),
                    before_seq: 42,
                    max: 50,
                    requester: 0x00AB_CDEF,
                },
            ),
            (
                "scrollback response",
                NeighbourMsg::ScrollbackResponse {
                    artifact: artifact(0x44),
                    ops: vec![dummy_envelope(), dummy_envelope()],
                    responder: 0x123_456,
                },
            ),
        ]
    }

    /// Every message round-trips over the production codec and every variant encodes to its
    /// pinned bytes.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn every_message_round_trips_and_encodes_to_its_pinned_bytes() {
        every_message_round_trips_over_the_production_codec();
        every_variant_encodes_to_its_pinned_bytes();
    }

    fn every_message_round_trips_over_the_production_codec() {
        for (case, message) in fixtures() {
            let bytes = postcard::to_allocvec(&message)
                .unwrap_or_else(|error| panic!("{case} failed to encode: {error}"));
            let decoded: NeighbourMsg = postcard::from_bytes(&bytes)
                .unwrap_or_else(|error| panic!("{case} failed to decode: {error}"));
            assert_eq!(message, decoded, "{case} changed in transit");
        }
    }

    /// The tag peek agrees with a full decode on a real encoding of every
    /// variant, and the pinned tag is the varint postcard writes for
    /// `AppDocument`.
    #[test]
    fn app_document_tag_matches_encoding() {
        let mut app_documents = 0;
        for (case, message) in fixtures() {
            let bytes = postcard::to_allocvec(&message).unwrap();
            let is_app_document = matches!(message, NeighbourMsg::AppDocument { .. });
            assert_eq!(
                NeighbourMsg::is_app_document(&bytes),
                is_app_document,
                "{case}"
            );
            if is_app_document {
                app_documents += 1;
                let (tag, _) = postcard::take_from_bytes::<u32>(&bytes).unwrap();
                assert_eq!(tag, u32::from(NeighbourMsg::APP_DOCUMENT_TAG), "{case}");
            }
        }
        assert!(app_documents > 0, "no AppDocument fixture was classified");
        assert!(!NeighbourMsg::is_app_document(&[]));
    }

    /// Exactly the two generations no build produces are retired; every
    /// other variant, including both identity successors, stays live.
    #[test]
    fn only_the_unproduced_generations_are_retired() {
        let retired: Vec<&str> = fixtures()
            .iter()
            .filter(|(_, message)| message.is_retired())
            .map(|(_, message)| variant(message))
            .collect();
        assert_eq!(retired, ["PermissionRequest", "IdentityAnnouncement"]);
    }

    /// Names every variant through an exhaustive match, so adding a variant
    /// fails to compile until it has a fixture and a golden encoding.
    fn variant(message: &NeighbourMsg) -> &'static str {
        message.kind()
    }

    #[test]
    fn only_a_wall_clock_heartbeat_asks_its_neighbours_to_keep_beating() {
        let beat = |ts| NeighbourMsg::Heartbeat { peer: 9, ts };
        // What a heartbeat-keeping build sends: unix milliseconds, October 2026.
        assert!(beat(1_791_500_000_000).is_timed_heartbeat());
        // What a transport-liveness build sends such a neighbour: a number.
        assert!(!beat(1).is_timed_heartbeat());
        assert!(!beat(NeighbourMsg::COMPAT_HEARTBEAT_TS_LIMIT - 1).is_timed_heartbeat());
        assert!(
            !NeighbourMsg::PresenceCleared {
                peer: 9,
                key: String::new()
            }
            .is_timed_heartbeat()
        );
    }

    const VARIANT_COUNT: usize = 30;

    /// Postcard encodings pinned byte for byte. Peers running older builds
    /// decode these frames, so a change here is a wire break, not a refactor.
    const GOLDEN: &[(&str, &str)] = &[
        (
            "sync request",
            concat!(
                "00030102032436623366396131302d303030302d343030302d383030302d3030",
                "3030303030303030306301090909090909090909090909090909090909090909",
                "090909090909090909090907",
            ),
        ),
        (
            "sync response",
            concat!(
                "010202aabb01cc2436623366396131302d303030302d343030302d383030302d",
                "303030303030303030303063",
            ),
        ),
        (
            "presence moved",
            concat!(
                "022a03626f6206637572736f7200000000000029400000000000000ac080e2cf",
                "aa06010676696f6c6574",
            ),
        ),
        ("presence cleared", "032a06637572736f72"),
        ("surface frame", "0407cdd70205000000016781e2cfaa0603"),
        (
            "SDR stream start",
            concat!(
                "0507cdd702800ad005046832363482e2cfaa0682e2cfaa06000000126f72672e",
                "6578616d706c652e456469746f7200",
            ),
        ),
        (
            "HDR stream start",
            concat!(
                "0508cdd702800fb8080b686576632d6d61696e313083e2cfaa0683e2cfaa0601",
                "0101010001c894028872b442aab7029633fc11937ac2800180ade204f4030000",
                "0a156f72672e6578616d706c652e486472456469746f7200",
            ),
        ),
        ("stream end", "0607cdd70284e2cfaa06"),
        ("surface keyframe request", "1d0307ac02"),
        ("remote input", "07030704deadbeef"),
        ("permission request", "08050c73686172652d73637265656e"),
        ("permission grant", "09050c73686172652d73637265656e"),
        ("heartbeat", "0aeffdb6f50d85e2cfaa06"),
        (
            "identity hello",
            concat!(
                // The variant index and the sender's peer protocol version.
                "0b02",
                "01010101010101010101010101010101010101010101010101010101010101",
                "0102020202020202020202020202020202020202020202020202020202020202",
                "02064c6170746f70010000000a00400303030303030303030303030303030303",
                "0303030303030303030303030303030303030303030303030303030303030303",
                "03030303030303030303030303030300",
            ),
        ),
        (
            "identity announcement",
            concat!(
                "102436623366396131302d303030302d343030302d383030302d303030303030",
                "3030303030630101010101010101010101010101010101010101010101010101",
                "0101010101010202020202020202020202020202020202020202020202020202",
                "020202020202064c6170746f70010000000a0040030303030303030303030303",
                "0303030303030303030303030303030303030303030303030303030303030303",
                "0303030303030303030303030303030303030303",
            ),
        ),
        (
            "identity announcement v2",
            concat!(
                "112436623366396131302d303030302d343030302d383030302d303030303030",
                "3030303030630101010101010101010101010101010101010101010101010101",
                "0101010101010202020202020202020202020202020202020202020202020202",
                "020202020202064c6170746f70010000000a0040030303030303030303030303",
                "0303030303030303030303030303030303030303030303030303030303030303",
                "030303030303030303030303030303030303030300",
            ),
        ),
        (
            "personal continuity request",
            concat!(
                "1207070707070707070707070707070707070707070707070707070707070707",
                "07ef9baf05",
            ),
        ),
        (
            "personal continuity checkpoint",
            concat!(
                "1307070707070707070707070707070707070707070707070707070707070707",
                "07ef9baf0503010203",
            ),
        ),
        (
            "personal workspaces",
            concat!(
                "1907070707070707070707070707070707070707070707070707070707070707",
                "0702020202020202020202020202020202020202020202020202020202020202",
                "020a030102034003030303030303030303030303030303030303030303030303",
                "0303030303030303030303030303030303030303030303030303030303030303",
                "03030303030303",
            ),
        ),
        ("remote action", "14027b7d"),
        (
            "media capabilities",
            concat!(
                "15cdd702020361763103767039010468323634010361763101",
                "03767039",
            ),
        ),
        (
            "signed app document",
            concat!(
                "0cef9baf05056e6f7465732436623366396131302d303030302d343030302d38",
                "3030302d30303030303030303030306309757569643a3163326604deadbeef10",
                "1111111111111111111111111111111107070707070707070707070707070707",
                "0707070707070707070707070707070710222222222222222222222222222222",
                "220000002a000000400000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "000000000000000000",
            ),
        ),
        (
            "app presence",
            concat!(
                "0def9baf05056e6f7465732436623366396131302d303030302d343030302d38",
                "3030302d303030303030303030303063020405020308",
                "01047465616c",
            ),
        ),
        (
            "record content want",
            "1aef9baf050e736f2e74696c6f2e736c6964657304726f6f6d026162",
        ),
        (
            "record content piece",
            "1bd6e848ef9baf050e736f2e74696c6f2e736c6964657304726f6f6d026162030003010203",
        ),
        (
            "scrollback request",
            "0e10333333333333333333333333333333332a32ef9baf05",
        ),
        (
            "scrollback response",
            concat!(
                "0f10444444444444444444444444444444440204deadbeef1011111111111111",
                "1111111111111111110707070707070707070707070707070707070707070707",
                "07070707070707070710222222222222222222222222222222220000002a0000",
                "0040000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "000004deadbeef10111111111111111111111111111111110707070707070707",
                "0707070707070707070707070707070707070707070707071022222222222222",
                "2222222222222222220000002a00000040000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000d6e848",
            ),
        ),
        (
            "channel governance frame",
            concat!(
                "162436623366396131302d303030302d343030302d383030302d303030303030",
                "3030303030630106060606060606060606060606060606060606060606060606",
                "0606060606060610555555555555555555555555555555550207070707070707",
                "0707070707070707070707070707070707070707070707070703010808080808",
                "08080808080808080808080808080808080808080808080808080802c0330909",
                "09090909090909090909090909090909090909090909090909090909090940ab",
                "abababababababababababababababababababababababababababababababab",
                "ababababababababababababababababababababababababababababababab07",
            ),
        ),
        (
            "channel access request",
            concat!(
                "172436623366396131302d303030302d343030302d383030302d303030303030",
                "3030303030630110555555555555555555555555555555550202020202020202",
                "020202020202020202020202020202020202020202020202024b5080e2cfaa06",
                "40cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
                "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
                "cd08",
            ),
        ),
        (
            "channel governance ack",
            concat!(
                "182436623366396131302d303030302d343030302d383030302d303030303030",
                "3030303030630110555555555555555555555555555555550202020202020202",
                "020202020202020202020202020202020202020202020202eeeeeeeeeeeeeeee",
                "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee40efefefefefefef",
                "efefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefef",
                "efefefefefefefefefefefefefefefefefefefefefefefefef09",
            ),
        ),
        (
            "channel routed access request",
            concat!(
                "1c2436623366396131302d303030302d343030302d383030302d303030303030",
                "3030303030630110555555555555555555555555555555550202020202020202",
                "020202020202020202020202020202020202020202020202024b5080e2cfaa06",
                "40cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
                "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
                "cd0e736f2e74696c6f2e73686565747307706179726f6c6c0a",
            ),
        ),
    ];

    fn every_variant_encodes_to_its_pinned_bytes() {
        let fixtures = fixtures();
        let covered: std::collections::BTreeSet<_> = fixtures
            .iter()
            .map(|(_, message)| variant(message))
            .collect();
        assert_eq!(
            covered.len(),
            VARIANT_COUNT,
            "every variant needs a fixture"
        );
        let mut actual = Vec::new();
        for (case, message) in &fixtures {
            let bytes = postcard::to_allocvec(message).expect("fixture encodes");
            actual.push((*case, hex::encode(&bytes)));
        }
        let expected: Vec<_> = GOLDEN
            .iter()
            .map(|(case, bytes)| (*case, (*bytes).to_owned()))
            .collect();
        assert_eq!(actual, expected);
    }
}
