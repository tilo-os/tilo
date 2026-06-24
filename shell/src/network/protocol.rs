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
}
