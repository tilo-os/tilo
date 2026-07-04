//! Peer-sync wire protocol — message envelope shared by all neighbour transports.
//!
//! Phase 1 Task 1.1 of the peer-sync end-to-end plan: a single tagged enum
//! carrying every kind of cross-host payload (CRDT sync, presence, surface
//! frames, remote input, capability negotiation). Concrete transport bindings
//! (libp2p gossipsub, in-process [`crate::network::hub::Hub`]) operate purely
//! on `NeighbourMsg`; they never inspect the payload.

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
    /// "Catch me up": deliver every Automerge change after the included clock.
    SyncReq { since: Vec<u8> },
    /// Response carrying a batch of opaque Automerge change blobs.
    SyncResp { changes: Vec<Vec<u8>> },
    /// Peer's presence cursor moved within `key`'s coordinate space.
    PresenceMoved {
        peer: PeerId,
        key: PresenceKey,
        point: (f64, f64),
        ts: u64,
    },
    /// Peer's presence cursor disappeared from `key`'s coordinate space.
    PresenceCleared { peer: PeerId, key: PresenceKey },
    /// Encoded H.264 frame for a shared surface.
    SurfaceFrame {
        surface: SurfaceId,
        encoded_h264: Vec<u8>,
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
    /// Periodic liveness beacon — Actor publishes one per joined workspace
    /// every 5 s; receivers track arrival time and evict peers silent for
    /// 15 s. Per-message `peer` is the publisher's libp2p PeerId hashed to
    /// u64; `ts` is unix-millis.
    Heartbeat { peer: PeerId, ts: u64 },
    /// App-level CRDT incremental — a peer-to-peer bridge for the
    /// app-side Automerge incremental change blobs that flow through
    /// the local `QUICK_COLLAB_SOCK` broker. Distinct from
    /// [`NeighbourMsg::SyncResp`], which carries shell-side
    /// `WorkspaceTree` changes; this variant ships the bytes the
    /// `quick-ui` toolkit's `ui::collaboration::CollabRequest::SyncIncremental`
    /// produces, opaque to the shell.
    ///
    /// Two laptops with the broker wired up exchange these so two
    /// terminals (or any pair of apps) running on different hosts
    /// share their CRDT state via libp2p. The remote-receive path
    /// fans the payload into every locally-connected app as a
    /// `CollabEvent::CRDTIncremental` — same translation the broker's
    /// local fan-out does.
    AppCrdtIncremental {
        peer: PeerId,
        /// App-id from the originating `CollabRequest::Hello`. v1
        /// doesn't route on this — every app on a peer sees every
        /// app's update — but the field lets future routers filter.
        app_id: String,
        /// Cluster id from the originating Hello. Same caveat —
        /// v1 doesn't filter on it.
        cluster_id: String,
        /// Opaque Automerge incremental change blob, passed through
        /// untouched.
        data: Vec<u8>,
    },
    /// Phase 3 of the spaces+channels spec: a signed CRDT envelope
    /// crossing the libp2p boundary. Recipients route the envelope
    /// into their local broker, which runs the validation pipeline
    /// (`qos_collab_types::validate_inbound_op`) before fanning the
    /// payload to any locally-connected app.
    ///
    /// The legacy `AppCrdtIncremental` variant is kept alongside this
    /// one so legacy peers on the same mesh continue to publish + see
    /// unsigned updates while their owners migrate. A peer that has
    /// upgraded keeps emitting `AppCrdtIncrementalSigned`; receiving
    /// shells fall back to the legacy fan-out for `AppCrdtIncremental`
    /// (no validation, advisory only).
    ///
    /// The envelope is boxed so the `NeighbourMsg` enum's stack size
    /// stays in the same ballpark as the other variants. Postcard's
    /// on-wire shape ignores the Box — it sees only the underlying
    /// `SignedOp` bytes.
    AppCrdtIncrementalSigned {
        /// Hashed libp2p PeerId of the publisher. Mirrors the `peer`
        /// field on the legacy variant; used by receiving shells to
        /// filter out self-publishes when they observe their own
        /// gossipsub echoes.
        peer: PeerId,
        /// The signed-op envelope. The validator on the receiving
        /// shell consults its channel state to decide whether to
        /// fan the payload out to local apps or to drop it with an
        /// audit-log entry.
        envelope: Box<qos_collab_types::SignedOp>,
    },
    /// Phase 6: cooperative scrollback request. Broadcast over
    /// gossipsub when a peer's local broker has no in-memory
    /// scrollback for a given artifact range and the requesting app
    /// asked for it. Any peer that *does* hold the range replies
    /// with [`NeighbourMsg::ScrollbackResponse`].
    ///
    /// `requester` is the local peer hash of the broker that issued
    /// the request — receiving brokers use it to target the response
    /// (`ScrollbackResponse::responder` becomes the source). v1 does
    /// not deduplicate concurrent identical requests; the typical
    /// case is "rare, single-shot" so amplification isn't a concern.
    ScrollbackRequest {
        /// The artifact whose log to fetch.
        artifact: qos_collab_types::ArtifactId,
        /// Exclusive upper bound on `seq`. Same semantics as
        /// `CollabRequest::FetchScrollback::before_seq`.
        before_seq: u64,
        /// Page size cap. Matches `CollabRequest::FetchScrollback::max`
        /// so the cooperative path doesn't have to re-cap.
        max: u16,
        /// Hashed libp2p peer id of the requesting broker. Receivers
        /// use it to skip themselves when they echo their own
        /// requests back over gossipsub.
        requester: PeerId,
    },
    /// Phase 6: response to a [`NeighbourMsg::ScrollbackRequest`].
    /// The responding broker fetched `ops` from its own local op
    /// log and forwarded them; the requesting broker delivers them
    /// to the original requesting local app via
    /// `CollabEvent::ScrollbackBatch`.
    ///
    /// `responder` is the local peer hash of the answering broker
    /// (useful for "who served me the page" audit / replay
    /// diagnostics). It does NOT play a role in the validation path
    /// — every envelope inside `ops` is independently signed.
    ScrollbackResponse {
        /// Same as the request's `artifact`. Repeated on the
        /// response so the receiving broker can route without
        /// matching on a request-id.
        artifact: qos_collab_types::ArtifactId,
        /// The page of validated ops the responder is sharing. Empty
        /// is allowed (means "I don't have the range either"); the
        /// requester treats an empty response as a non-answer.
        ops: Vec<Box<qos_collab_types::SignedOp>>,
        /// Hashed libp2p peer id of the answering broker.
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
        });
    }

    #[test]
    fn sync_resp_round_trips() {
        round_trip(NeighbourMsg::SyncResp {
            changes: vec![vec![0xAA, 0xBB], vec![0xCC]],
        });
    }

    #[test]
    fn presence_moved_round_trips() {
        round_trip(NeighbourMsg::PresenceMoved {
            peer: 42,
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
            data: vec![0x01, 0x02, 0x03, 0x04],
        });
    }

    /// Build a syntactically-correct (but cryptographically empty)
    /// envelope for postcard round-trip exercising. The bytes don't
    /// need to verify — only the framing has to survive a
    /// serialise/deserialise cycle.
    fn dummy_envelope() -> Box<qos_collab_types::SignedOp> {
        use qos_collab_types::{
            ArtifactId, AttestationId, Ed25519PublicKey, OpClass, SignedOp,
        };
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
        // The actor still encodes `NeighbourMsg` with `serde_json` for
        // historical reasons; the new variant must survive the same
        // pipeline. (postcard round-trip is exercised separately so
        // that future wire-format experiments can swap the codec
        // without touching this test.)
        round_trip(NeighbourMsg::AppCrdtIncrementalSigned {
            peer: 0xABCDEF,
            envelope: dummy_envelope(),
        });
    }

    #[test]
    fn app_crdt_incremental_signed_round_trips_postcard() {
        // Postcard is the canonical-bytes format used by
        // `qos_collab_types` for signing; verifying that
        // `NeighbourMsg::AppCrdtIncrementalSigned` round-trips through
        // postcard guards against a future wire-format experiment
        // accidentally dropping the new variant.
        let msg = NeighbourMsg::AppCrdtIncrementalSigned {
            peer: 0x1234,
            envelope: dummy_envelope(),
        };
        let bytes = postcard::to_allocvec(&msg).expect("postcard serialize");
        let back: NeighbourMsg = postcard::from_bytes(&bytes).expect("postcard deserialize");
        assert_eq!(msg, back);
    }

    #[test]
    fn scrollback_request_round_trips_serde_json() {
        // Phase 6 — the cooperative-fetch request must survive the
        // actor's serde_json pipeline the same way every other
        // gossipsub message does.
        use qos_collab_types::ArtifactId;
        round_trip(NeighbourMsg::ScrollbackRequest {
            artifact: ArtifactId::new(),
            before_seq: 42,
            max: 50,
            requester: 0xABCDEF,
        });
    }

    #[test]
    fn scrollback_request_round_trips_postcard() {
        use qos_collab_types::ArtifactId;
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
        use qos_collab_types::ArtifactId;
        round_trip(NeighbourMsg::ScrollbackResponse {
            artifact: ArtifactId::new(),
            ops: vec![dummy_envelope(), dummy_envelope()],
            responder: 0x123_456,
        });
    }

    #[test]
    fn scrollback_response_round_trips_postcard() {
        use qos_collab_types::ArtifactId;
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
