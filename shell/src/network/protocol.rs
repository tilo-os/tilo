//! Peer-sync wire protocol: a single tagged enum (`NeighbourMsg`) carrying every
//! cross-host payload (CRDT sync, presence, surface frames, remote input,
//! capability negotiation). Transport bindings (Iroh gossip, in-process
//! [`crate::network::hub::Hub`]) operate on it without inspecting the payload.

use serde::{Deserialize, Serialize};

/// Peer identifier — opaque numeric handle assigned per session.
pub type PeerId = u64;

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
    SyncReq { since: Vec<u8>, room: String },
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
        #[serde(default)]
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
        /// `#[serde(default)]` keeps this wire-compatible with pre-handshake peers,
        /// who send `0`; [`crate::diagnostics::PeerSurfaceMetrics::set_clock_offset`]
        /// treats that as "skip cross-host latency" rather than a bogus offset — the
        /// OSD shows "—" instead.
        #[serde(default)]
        sender_monotonic_ns_at_send: u64,
        /// Colorimetry tag; `None` on pre-HDR peers means the receiver assumes SDR
        /// BT.709 (matches v1 behaviour, so the field stays wire-compatible).
        #[serde(default)]
        colorimetry: Option<crate::collab::colorimetry::Colorimetry>,
        /// HDR10 static metadata (SMPTE ST 2086 + CTA-861.3) for HDR pipelines;
        /// `None` on SDR streams and pre-HDR peers. Used by the receiver's
        /// tone-mapper to size compression correctly on SDR displays.
        #[serde(default)]
        hdr_static_metadata: Option<crate::collab::hdr_metadata::Hdr10Metadata>,
        /// Codec target bit depth; `0` (serde-default) means unspecified — receivers
        /// infer 8-bit from `codec = "h264"`, else 8 or 10 (HEVC Main10).
        #[serde(default)]
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
    /// App-level CRDT incremental — bridges app-side Automerge change blobs from
    /// the local collaboration service across hosts, opaque to the shell.
    /// Distinct from [`NeighbourMsg::SyncResp`] (shell-side `WorkspaceTree`
    /// changes). The remote-receive path fans this into local apps as
    /// `CollabEvent::CRDTIncremental`, mirroring the broker's local fan-out.
    AppCrdtIncremental {
        peer: PeerId,
        /// App-id from the originating `CollabRequest::Hello`; the receiving broker
        /// routes only to local apps announcing the same `(app_id, cluster_id)` —
        /// empty addresses nobody.
        app_id: String,
        /// Cluster id from the originating Hello — second half of the routing key.
        cluster_id: String,
        /// Record id from the originating Hello — which document these changes
        /// belong to (empty = single document per `(app, cluster)`). Carried so the
        /// receiving shell can route per-document; `#[serde(default)]` keeps frames
        /// from pre-record peers parsing.
        #[serde(default)]
        record_id: String,
        /// Opaque Automerge incremental change blob, passed through
        /// untouched.
        data: Vec<u8>,
    },
    /// A signed CRDT envelope crossing the Iroh boundary; recipients route it
    /// into the local broker's validation pipeline
    /// (`collab_types::validate_inbound_op`) before fan-out.
    ///
    /// Kept alongside unsigned `AppCrdtIncremental` for documents that have
    /// not adopted signed governance. Boxed so this variant doesn't
    /// bloat `NeighbourMsg`'s stack size — postcard's wire shape is unaffected.
    AppCrdtIncrementalSigned {
        /// Hashed Iroh `PeerId` of the publisher; used to filter self-published
        /// gossip echoes.
        peer: PeerId,
        /// App-id of the originating app, same routing key as
        /// [`NeighbourMsg::AppCrdtIncremental`]. `#[serde(default)]` so envelopes
        /// from peers that predate routing arrive unaddressed and get dropped
        /// rather than shown to every local app.
        #[serde(default)]
        app_id: String,
        /// Cluster id — second half of the routing key, same backward-compat default.
        #[serde(default)]
        cluster_id: String,
        /// Record id of the originating document — third routing-key component;
        /// empty means the sender announced no record.
        #[serde(default)]
        record_id: String,
        /// The signed-op envelope; the receiving shell's validator decides whether
        /// to fan it to local apps or drop it with an audit-log entry.
        envelope: Box<collab_types::SignedOp>,
    },
    /// App-level presence crossing the host boundary. Presence is cluster scoped,
    /// so a participant remains visible while moving between apps in one room.
    AppPresenceV2 {
        peer: PeerId,
        #[serde(default)]
        app_id: String,
        #[serde(default)]
        cluster_id: String,
        presence: collab_types::PeerPresence,
    },
    /// App-level CRDT **snapshot** — a document's whole state, for a peer with no
    /// history for it to adopt.
    ///
    /// Needed because [`NeighbourMsg::AppCrdtIncremental`] deltas name
    /// dependencies by hash: two peers that each *created* a document rather
    /// than opening a shared one otherwise exchange deltas forever without
    /// converging.
    ///
    /// Sent on demand, not per-commit: a peer's `PresencePayload::Arrived`
    /// (noted by [`crate::shell::network::ShellNetwork::note_snapshot_request`])
    /// triggers one snapshot per arriving peer, not addressed specifically to
    /// them — merging is idempotent/commutative, so anyone converges from it.
    ///
    /// Bounded by [`crate::shell::network::MAX_APP_SNAPSHOT_BYTES`] to limit
    /// serialization and fan-out memory. A document has no upper bound, so
    /// publishing above the application cap fails loudly rather than making
    /// the collaboration actor unresponsive.
    AppCrdtSnapshot {
        /// Hashed Iroh `PeerId` of the publisher; receiving shells drop their own
        /// gossip echoes on it, as on every App* variant.
        peer: PeerId,
        /// App-id of the originating app — first routing-key component.
        #[serde(default)]
        app_id: String,
        /// Cluster id (room, as uuid string); the receiving shell refuses a room it
        /// isn't in before the frame reaches any app.
        #[serde(default)]
        cluster_id: String,
        /// Record id — which document this is the state of. Getting this wrong is
        /// unrecoverable (a merged snapshot can't be un-merged), so an unaddressed
        /// snapshot is dropped rather than guessed at.
        #[serde(default)]
        record_id: String,
        /// `automerge::Automerge::save()` output for that record, opaque to every hop.
        data: Vec<u8>,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(msg: NeighbourMsg) {
        let json = serde_json::to_string(&msg).expect("serialize");
        let parsed: NeighbourMsg = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(msg, parsed);
    }

    #[test]
    fn sync_req_round_trips() {
        round_trip(NeighbourMsg::SyncReq {
            since: vec![1, 2, 3],
            room: "6b3f9a10-0000-4000-8000-00000000000c".to_string(),
        });
    }

    #[test]
    fn sync_resp_round_trips() {
        round_trip(NeighbourMsg::SyncResp {
            changes: vec![vec![0xAA, 0xBB], vec![0xCC]],
            room: "6b3f9a10-0000-4000-8000-00000000000c".to_string(),
        });
    }

    #[test]
    fn presence_moved_round_trips() {
        round_trip(NeighbourMsg::PresenceMoved {
            peer: 42,
            user: "bob".to_string(),
            key: "cursor".to_string(),
            point: (12.5, -3.25),
            ts: 1_700_000_000,
        });
    }

    #[test]
    fn presence_cleared_round_trips() {
        round_trip(NeighbourMsg::PresenceCleared {
            peer: 42,
            key: "cursor".to_string(),
        });
    }

    #[test]
    fn surface_frame_round_trips() {
        round_trip(NeighbourMsg::SurfaceFrame {
            surface: 7,
            encoded_h264: vec![0x00, 0x00, 0x00, 0x01, 0x67],
            ts: 1_700_000_001,
        });
    }

    #[test]
    fn surface_stream_start_round_trips() {
        round_trip(NeighbourMsg::SurfaceStreamStart {
            surface: 7,
            source_peer: 0xABCD,
            width: 1280,
            height: 720,
            codec: "h264".to_string(),
            ts: 1_700_000_002,
            sender_monotonic_ns_at_send: 1_700_000_002,
            colorimetry: None,
            hdr_static_metadata: None,
            bit_depth: 0,
        });
    }

    #[test]
    fn surface_stream_start_backfill_when_field_missing() {
        // Legacy JSON lacks the extended fields; `#[serde(default)]` backfills them
        // for old peers.
        let legacy = r#"{"SurfaceStreamStart":{"surface":9,"source_peer":123,"width":320,"height":240,"codec":"h264","ts":1234567}}"#;
        let parsed: NeighbourMsg = serde_json::from_str(legacy).expect("legacy parses");
        match parsed {
            NeighbourMsg::SurfaceStreamStart {
                sender_monotonic_ns_at_send,
                colorimetry,
                hdr_static_metadata,
                bit_depth,
                ..
            } => {
                assert_eq!(sender_monotonic_ns_at_send, 0);
                assert!(colorimetry.is_none(), "legacy has no colorimetry");
                assert!(hdr_static_metadata.is_none(), "legacy has no HDR metadata");
                assert_eq!(bit_depth, 0, "legacy defaults bit_depth to 0 (unspecified)");
            }
            other => panic!("expected SurfaceStreamStart, got {other:?}"),
        }
    }

    #[test]
    fn surface_stream_start_hdr_round_trips() {
        use crate::collab::colorimetry::Colorimetry;
        use crate::collab::hdr_metadata::Hdr10Metadata;
        round_trip(NeighbourMsg::SurfaceStreamStart {
            surface: 7,
            source_peer: 0xABCD,
            width: 1920,
            height: 1080,
            codec: "hevc-main10".to_string(),
            ts: 1_700_000_002,
            sender_monotonic_ns_at_send: 1_700_000_002,
            colorimetry: Some(Colorimetry::hdr10()),
            hdr_static_metadata: Some(Hdr10Metadata::reference_bt2020()),
            bit_depth: 10,
        });
    }

    #[test]
    fn surface_stream_end_round_trips() {
        round_trip(NeighbourMsg::SurfaceStreamEnd {
            surface: 7,
            source_peer: 0xABCD,
            ts: 1_700_000_003,
        });
    }

    #[test]
    fn surface_stream_start_round_trips_postcard() {
        let msg = NeighbourMsg::SurfaceStreamStart {
            surface: 42,
            source_peer: 0x1234,
            width: 1920,
            height: 1080,
            codec: "h264".to_string(),
            ts: 1_700_000_004,
            sender_monotonic_ns_at_send: 1_700_000_004,
            colorimetry: None,
            hdr_static_metadata: None,
            bit_depth: 0,
        };
        let bytes = postcard::to_allocvec(&msg).expect("postcard serialize");
        let back: NeighbourMsg = postcard::from_bytes(&bytes).expect("postcard deserialize");
        assert_eq!(msg, back);
    }

    #[test]
    fn remote_input_round_trips() {
        round_trip(NeighbourMsg::RemoteInput {
            peer: 3,
            surface: 7,
            event: vec![0xDE, 0xAD, 0xBE, 0xEF],
        });
    }

    #[test]
    fn permission_request_round_trips() {
        round_trip(NeighbourMsg::PermissionRequest {
            peer: 5,
            capability: "share-screen".to_string(),
        });
    }

    #[test]
    fn permission_grant_round_trips() {
        round_trip(NeighbourMsg::PermissionGrant {
            peer: 5,
            capability: "share-screen".to_string(),
        });
    }

    #[test]
    fn heartbeat_round_trips() {
        round_trip(NeighbourMsg::Heartbeat {
            peer: 0xDEADBEEF,
            ts: 1_700_000_002,
        });
    }

    #[test]
    fn app_crdt_incremental_round_trips() {
        round_trip(NeighbourMsg::AppCrdtIncremental {
            peer: 0xABCDEF,
            app_id: "terminal".into(),
            cluster_id: "00000000-0000-0000-0000-000000000000".into(),
            record_id: "file:/work/report.odt".into(),
            data: vec![0x01, 0x02, 0x03, 0x04],
        });
    }

    #[test]
    fn app_crdt_snapshot_round_trips() {
        round_trip(NeighbourMsg::AppCrdtSnapshot {
            peer: 0xABCDEF,
            app_id: "dev.qos.word-processor".into(),
            cluster_id: "00000000-0000-0000-0000-000000000000".into(),
            record_id: "file:/work/report.odt".into(),
            data: vec![0x85, 0x6F, 0x4A, 0x83],
        });
    }

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

    #[test]
    fn app_crdt_incremental_signed_round_trips_serde_json() {
        // The actor still encodes `NeighbourMsg` with `serde_json`; postcard is
        // covered separately below.
        round_trip(NeighbourMsg::AppCrdtIncrementalSigned {
            peer: 0xABCDEF,
            app_id: "notes".to_string(),
            cluster_id: "11111111-2222-3333-4444-555555555555".to_string(),
            record_id: "uuid:1c2f".to_string(),
            envelope: dummy_envelope(),
        });
    }

    #[test]
    fn app_crdt_incremental_signed_round_trips_postcard() {
        // Postcard is the canonical signing format; this guards against a
        // wire-format change silently dropping the new variant.
        let msg = NeighbourMsg::AppCrdtIncrementalSigned {
            peer: 0x1234,
            app_id: "notes".to_string(),
            cluster_id: "11111111-2222-3333-4444-555555555555".to_string(),
            record_id: "uuid:1c2f".to_string(),
            envelope: dummy_envelope(),
        };
        let bytes = postcard::to_allocvec(&msg).expect("postcard serialize");
        let back: NeighbourMsg = postcard::from_bytes(&bytes).expect("postcard deserialize");
        assert_eq!(msg, back);
    }

    #[test]
    fn scrollback_request_round_trips_serde_json() {
        use collab_types::ArtifactId;
        round_trip(NeighbourMsg::ScrollbackRequest {
            artifact: ArtifactId::new(),
            before_seq: 42,
            max: 50,
            requester: 0xABCDEF,
        });
    }

    #[test]
    fn scrollback_request_round_trips_postcard() {
        use collab_types::ArtifactId;
        let msg = NeighbourMsg::ScrollbackRequest {
            artifact: ArtifactId::new(),
            before_seq: 42,
            max: 50,
            requester: 0xABCDEF,
        };
        let bytes = postcard::to_allocvec(&msg).expect("postcard serialize");
        let back: NeighbourMsg = postcard::from_bytes(&bytes).expect("postcard deserialize");
        assert_eq!(msg, back);
    }

    #[test]
    fn scrollback_response_round_trips_serde_json() {
        use collab_types::ArtifactId;
        round_trip(NeighbourMsg::ScrollbackResponse {
            artifact: ArtifactId::new(),
            ops: vec![dummy_envelope(), dummy_envelope()],
            responder: 0x123_456,
        });
    }

    #[test]
    fn scrollback_response_round_trips_postcard() {
        use collab_types::ArtifactId;
        let msg = NeighbourMsg::ScrollbackResponse {
            artifact: ArtifactId::new(),
            ops: vec![dummy_envelope()],
            responder: 0x123_456,
        };
        let bytes = postcard::to_allocvec(&msg).expect("postcard serialize");
        let back: NeighbourMsg = postcard::from_bytes(&bytes).expect("postcard deserialize");
        assert_eq!(msg, back);
    }
}
