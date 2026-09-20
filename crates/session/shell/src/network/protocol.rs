//! Native consumer of the shared, unchanged neighbour wire format.
pub use qos_peer_state::protocol::*;

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
                    app_id: "org.example.Editor".into(),
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
                    app_id: "org.example.HdrEditor".into(),
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
                    cert: Box::new(tilo_user_identity::DeviceCert {
                        user_public_key: [1; 32],
                        device_public_key: [2; 32],
                        device_label: "Laptop".into(),
                        capabilities: tilo_user_identity::DeviceCapabilities::member(),
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
