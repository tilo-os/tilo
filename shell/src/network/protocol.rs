//! Peer-sync wire protocol: a single tagged enum (`NeighbourMsg`) carrying every
//! cross-host payload (CRDT sync, presence, surface frames, remote input,
//! capability negotiation). Transport bindings (Iroh gossip, in-process
//! [`crate::network::hub::Hub`]) operate on it without inspecting the payload.

use serde::{Deserialize, Serialize};

/// Peer identifier — opaque numeric handle assigned per session.
pub type PeerId = u64;

/// Continuation is portable metadata, never an application backup. Keep this
/// limit at the wire boundary so every transport (direct or store-forward)
/// applies the same bound before decoding or retaining it.
pub const MAX_PERSONAL_CONTINUITY_CHECKPOINT_BYTES: usize = 4 * 1024 * 1024;

/// Wayland surface identifier — local handle used for streaming/input routing.
pub type SurfaceId = u64;

/// Presence channel key — namespaces presence updates (e.g. cursor, selection).
pub type PresenceKey = String;

/// Tagged envelope for everything that crosses the neighbour boundary.
///
/// `f64` lives inside [`NeighbourMsg::PresenceMoved`] so we cannot derive
/// [`Eq`]; [`PartialEq`] is sufficient for tests and dispatch logic.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub enum NeighbourMsg {
    /// Request: replay every Automerge change to `room` after the included clock.
    ///
    /// `room` is the [`crate::shell::state::RoomId`] uuid string that scopes the
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
    /// there ([`crate::shell::state::local_user_id`]), not something the
    /// receiver infers; empty falls back to showing the session handle.
    PresenceMoved {
        peer: PeerId,
        user: String,
        key: PresenceKey,
        point: (f64, f64),
        ts: u64,
    },
    /// Peer's presence cursor disappeared from `key`'s coordinate space.
    PresenceCleared { peer: PeerId, key: PresenceKey },
    /// Encoded H.264 frame for a shared surface.
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
    },
    /// Explicit start-of-stream announcement for a shared surface.
    ///
    /// Distinct from `SurfaceFrame` so late joiners can size a decoder /
    /// [`crate::shell::peer_view::PeerView`] before the first NALU arrives,
    /// avoiding a dropped frame. Codec is negotiated up front (`"h264"` in v1,
    /// room left for `"av1"`/`"h265"`).
    SurfaceStreamStart {
        surface: SurfaceId,
        /// Hashed Iroh `PeerId` of the owner; distinguishes concurrent shares in the
        /// same workspace.
        source_peer: PeerId,
        width: u32,
        height: u32,
        /// Codec label (`"h264"` in v1); unrecognised codecs are ignored + logged.
        codec: String,
        ts: u64,
        /// Sender's monotonic-clock reading when the message left the encode task
        /// (same origin as [`Self::SurfaceFrame::ts`]).
        ///
        /// The receiver computes `offset = receiver_now_ns - sender_monotonic_ns_at_send`
        /// on decode and applies it to every later `ts` via
        /// [`crate::diagnostics::PeerSurfaceMetrics::end_to_end_latency_ns`] to get
        /// real cross-host motion-to-photon numbers despite unrelated monotonic
        /// origins.
        ///
        /// A value of `0` means the sender cannot provide a clock offset;
        /// [`crate::diagnostics::PeerSurfaceMetrics::set_clock_offset`] then skips
        /// cross-host latency instead of reporting a bogus value.
        sender_monotonic_ns_at_send: u64,
        /// Colorimetry tag; `None` means the receiver assumes SDR BT.709.
        colorimetry: Option<crate::collab::colorimetry::Colorimetry>,
        /// HDR10 static metadata (SMPTE ST 2086 + CTA-861.3) for HDR pipelines;
        /// `None` on SDR streams. Used by the receiver's tone-mapper to size
        /// compression correctly on SDR displays.
        hdr_static_metadata: Option<crate::collab::hdr_metadata::Hdr10Metadata>,
        /// Codec target bit depth; `0` means unspecified — receivers infer 8-bit
        /// from `codec = "h264"`, else 8 or 10 (HEVC Main10).
        bit_depth: u8,
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
    /// `peer` would like permission `capability`.
    PermissionRequest { peer: PeerId, capability: String },
    /// `peer` was granted permission `capability`.
    PermissionGrant { peer: PeerId, capability: String },
    /// Periodic liveness beacon — Actor publishes one per joined workspace every
    /// 5s; direct peers are evicted after 15s without any traffic. `peer` is the hashed Iroh
    /// `PeerId`; `ts` is unix-millis.
    Heartbeat { peer: PeerId, ts: u64 },
    /// Authenticated identity chain for the transport peer on this workspace.
    ///
    /// The certificate binds the Iroh endpoint/device key to a user root. The
    /// additive, root-signed revocations let receivers reject a lost device
    /// before any of its collaboration payload reaches an app or replica.
    IdentityHello {
        cert: Box<qos_user_identity::DeviceCert>,
        #[serde(default)]
        revocations: Vec<qos_user_identity::RevocationEntry>,
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
        /// Record id of the originating document — third routing-key component;
        /// empty means the sender announced no record.
        record_id: String,
        /// The signed-op envelope; the receiving shell's validator decides whether
        /// to fan it to local apps or drop it with an audit-log entry.
        envelope: Box<collab_types::SignedOp>,
    },
    /// App-level presence crossing the host boundary. Presence is cluster scoped,
    /// so a participant remains visible while moving between apps in one room.
    AppPresenceV2 {
        peer: PeerId,
        app_id: String,
        cluster_id: String,
        presence: collab_types::PeerPresence,
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
    /// Root-signed device certificate announced on one room. Appended to the
    /// enum to preserve every existing Postcard discriminant. The receiver
    /// binds it to the transport-authenticated sender endpoint and only the
    /// room governor may promote it into replicated governance.
    IdentityAnnouncement {
        room: String,
        device_cert: Box<qos_user_identity::DeviceCert>,
    },
    /// V2 identity announcement carrying additive root-signed revocations.
    /// Appended rather than changing V1's Postcard payload layout.
    IdentityAnnouncementV2 {
        room: String,
        device_cert: Box<qos_user_identity::DeviceCert>,
        /// Additive root-signed revocations for the certificate's human root.
        /// The room governor verifies and retains them independently of the
        /// announcing device, so omission can never resurrect a device.
        revocations: Vec<qos_user_identity::RevocationEntry>,
    },
    /// Ask another certified device of the same person for its latest
    /// continuation checkpoints. This rides a user-root-derived topic, never
    /// a collaboration room: browser/session state must not become visible to
    /// collaborators merely because they share a workspace.
    ///
    /// Appended to preserve all existing Postcard discriminants.
    PersonalContinuityRequest {
        user_public_key: [u8; 32],
        requester: PeerId,
    },
    /// A bounded, validated aggregate of per-device
    /// [`crate::shell::personal_continuity`] snapshots.
    /// The receiving shell accepts it only when the authenticated transport
    /// certificate has `user_public_key`. Certified devices may relay the
    /// aggregate so an always-on peer retains the latest state for all of them.
    PersonalContinuityCheckpoint {
        user_public_key: [u8; 32],
        publisher: PeerId,
        snapshot: Vec<u8>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Syntactically valid but unsigned; only the postcard framing needs to
    /// round-trip.
    fn dummy_envelope() -> Box<collab_types::SignedOp> {
        use collab_types::{ArtifactId, AttestationId, Ed25519PublicKey, OpClass, SignedOp};
        use std::time::SystemTime;
        Box::new(SignedOp::new_unsigned(
            vec![0xDE, 0xAD, 0xBE, 0xEF],
            ArtifactId::new(),
            Ed25519PublicKey::from_bytes([7u8; 32]),
            AttestationId::new(),
            OpClass::Splice,
            SystemTime::UNIX_EPOCH,
            42,
        ))
    }

    /// Room id shared by every fixture that names one.
    const ROOM: &str = "6b3f9a10-0000-4000-8000-00000000000c";

    /// Every [`NeighbourMsg`] variant, split into thematic groups so no one
    /// function runs long — the round-trip test below exercises the
    /// concatenation.
    fn fixtures() -> Vec<(&'static str, NeighbourMsg)> {
        let mut all = fixtures_sync_presence_and_streams();
        all.extend(fixtures_input_permissions_and_identity());
        all.extend(fixtures_app_crdt_and_scrollback());
        all
    }

    fn fixtures_sync_presence_and_streams() -> Vec<(&'static str, NeighbourMsg)> {
        use crate::collab::colorimetry::Colorimetry;
        use crate::collab::hdr_metadata::Hdr10Metadata;

        vec![
            (
                "sync request",
                NeighbourMsg::SyncReq {
                    since: vec![1, 2, 3],
                    room: ROOM.into(),
                    invite_id: Some([9; 32]),
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
                },
            ),
            (
                "SDR stream start",
                NeighbourMsg::SurfaceStreamStart {
                    surface: 7,
                    source_peer: 0xABCD,
                    width: 1280,
                    height: 720,
                    codec: "h264".into(),
                    ts: 1_700_000_002,
                    sender_monotonic_ns_at_send: 1_700_000_002,
                    colorimetry: None,
                    hdr_static_metadata: None,
                    bit_depth: 0,
                },
            ),
            (
                "HDR stream start",
                NeighbourMsg::SurfaceStreamStart {
                    surface: 8,
                    source_peer: 0xABCD,
                    width: 1920,
                    height: 1080,
                    codec: "hevc-main10".into(),
                    ts: 1_700_000_003,
                    sender_monotonic_ns_at_send: 1_700_000_003,
                    colorimetry: Some(Colorimetry::hdr10()),
                    hdr_static_metadata: Some(Hdr10Metadata::reference_bt2020()),
                    bit_depth: 10,
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
                    cert: Box::new(qos_user_identity::DeviceCert {
                        user_public_key: [1; 32],
                        device_public_key: [2; 32],
                        device_label: "Laptop".into(),
                        capabilities: qos_user_identity::DeviceCapabilities::member(),
                        issued_at_ns: 10,
                        expires_at_ns: None,
                        signature: [3; 64],
                    }),
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
        ]
    }

    fn fixtures_app_crdt_and_scrollback() -> Vec<(&'static str, NeighbourMsg)> {
        use collab_types::{ArtifactId, PeerPresence, PeerPresencePayload};

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
                },
            ),
            (
                "scrollback request",
                NeighbourMsg::ScrollbackRequest {
                    artifact: ArtifactId::new(),
                    before_seq: 42,
                    max: 50,
                    requester: 0x00AB_CDEF,
                },
            ),
            (
                "scrollback response",
                NeighbourMsg::ScrollbackResponse {
                    artifact: ArtifactId::new(),
                    ops: vec![dummy_envelope(), dummy_envelope()],
                    responder: 0x123_456,
                },
            ),
        ]
    }

    #[test]
    fn every_message_round_trips_over_the_production_codec() {
        for (case, message) in fixtures() {
            let bytes = postcard::to_allocvec(&message)
                .unwrap_or_else(|error| panic!("{case} failed to encode: {error}"));
            let decoded: NeighbourMsg = postcard::from_bytes(&bytes)
                .unwrap_or_else(|error| panic!("{case} failed to decode: {error}"));
            assert_eq!(message, decoded, "{case} changed in transit");
        }
    }
}
