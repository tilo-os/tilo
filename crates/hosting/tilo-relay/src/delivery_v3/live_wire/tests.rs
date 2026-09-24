use super::*;
use ed25519_dalek::SigningKey;

fn record() -> LiveRecord {
    LiveRecord {
        app: "dev.qos.word-processor".into(),
        record: "doc-1".into(),
    }
}
fn request() -> LiveGrantRequest {
    LiveGrantRequest {
        version: 1,
        device: [7; 32],
        room: "4f1e5c3a-0000-4000-8000-000000000001".into(),
        participation_id: [9; 32],
        directions: LiveDirections {
            publish: true,
            subscribe: true,
        },
    }
}
fn metadata(host: &SigningKey) -> LiveGrantMetadata {
    let request = request();
    LiveGrantMetadata {
        root: [3; 32],
        namespace: LiveNamespace::new(&request.room, request.device),
        request,
        membership_generation: 4,
        audience: format!("https://127.0.0.1:19343{LIVE_WEBTRANSPORT_PATH}"),
        host: host.verifying_key().to_bytes(),
        host_boot: [5; 32],
        issued_at_ns: 1_000,
        expires_at_ns: 1_000 + GRANT_LIFETIME_NS,
    }
}

#[test]
fn live_grant_round_trips_signs_and_verifies_within_lifetime() {
    let host = SigningKey::from_bytes(&[1; 32]);
    let grant = SignedLiveGrant::sign(metadata(&host), &host).unwrap();
    let bytes = grant.encode().unwrap();
    assert!(bytes.len() <= MAX_GRANT_BYTES);
    let decoded = SignedLiveGrant::decode(&bytes).unwrap();
    assert_eq!(decoded, grant);
    decoded.verify(&host.verifying_key(), 2_000).unwrap();
    assert!(
        decoded
            .verify(&host.verifying_key(), 1_000 + GRANT_LIFETIME_NS)
            .is_err(),
        "expired"
    );
    assert!(
        decoded.verify(&host.verifying_key(), 999).is_err(),
        "not yet valid"
    );
    let other = SigningKey::from_bytes(&[2; 32]);
    assert!(
        decoded.verify(&other.verifying_key(), 2_000).is_err(),
        "foreign host"
    );
}

#[test]
fn live_grant_rejects_scope_substitution_and_overlong_lifetime() {
    let host = SigningKey::from_bytes(&[1; 32]);
    let mut m = metadata(&host);
    m.namespace.publisher = [8; 32];
    assert!(
        SignedLiveGrant::sign(m, &host).is_err(),
        "publisher must equal requesting device"
    );
    let mut m = metadata(&host);
    m.namespace.room = [0xAA; 32];
    assert!(
        SignedLiveGrant::sign(m, &host).is_err(),
        "room digest must match the request room"
    );
    let mut m = metadata(&host);
    m.expires_at_ns = m.issued_at_ns + GRANT_LIFETIME_NS + 1;
    assert!(SignedLiveGrant::sign(m, &host).is_err());
    let mut m = metadata(&host);
    m.audience = "https://127.0.0.1:19343/moq/v21/voice".into();
    assert!(
        SignedLiveGrant::sign(m, &host).is_err(),
        "grant is only for the live relay"
    );
    let mut tampered = SignedLiveGrant::sign(metadata(&host), &host).unwrap();
    tampered.metadata.membership_generation += 1;
    assert!(tampered.verify(&host.verifying_key(), 2_000).is_err());
}

#[test]
fn presence_frame_fits_one_datagram_with_a_full_drag() {
    let frame = PresenceFrame {
        seq: u64::MAX,
        sent_at_ms: 1_790_000_000_000,
        focus: Some(LiveRecord {
            app: "a".repeat(128),
            record: "r".repeat(128),
        }),
        pointer: Some([12.5, 99.0]),
        selection: Some((
            LiveAnchor {
                cursor: vec![1; 64],
            },
            LiveAnchor {
                cursor: vec![2; 64],
            },
        )),
        drag: Some((
            LiveObject {
                record: Some(LiveRecord {
                    app: "a".repeat(128),
                    record: "r".repeat(128),
                }),
                id: vec![3; 64],
            },
            DragPhase::Move,
            [400.0, 300.0],
            Some([640.0, 480.0]),
        )),
    };
    let bytes = frame.encode().unwrap();
    assert!(
        bytes.len() <= MAX_DATAGRAM_FRAME_BYTES,
        "worst-case presence is {} bytes",
        bytes.len()
    );
    assert_eq!(PresenceFrame::decode(&bytes).unwrap(), frame);
}

#[test]
fn presence_frame_rejects_non_finite_and_unbounded_fields() {
    let base = PresenceFrame {
        seq: 1,
        sent_at_ms: 1,
        focus: None,
        pointer: Some([f32::NAN, 0.0]),
        selection: None,
        drag: None,
    };
    assert!(base.encode().is_err());
    let oversized = PresenceFrame {
        pointer: None,
        drag: Some((
            LiveObject {
                record: None,
                id: vec![0; MAX_OBJECT_ID_BYTES + 1],
            },
            DragPhase::Start,
            [0.0, 0.0],
            None,
        )),
        ..base.clone()
    };
    assert!(oversized.encode().is_err());
    let negative = PresenceFrame {
        pointer: None,
        drag: Some((
            LiveObject {
                record: None,
                id: vec![1],
            },
            DragPhase::End,
            [0.0, 0.0],
            Some([-1.0, 5.0]),
        )),
        ..base
    };
    assert!(negative.encode().is_err());
}

#[test]
fn ops_frames_bind_signed_change_digest_and_bound_previews() {
    let canonical = b"original canonical delivery item".to_vec();
    let change = OpsFrame::SignedChange {
        record: record(),
        digest: *blake3::hash(&canonical).as_bytes(),
        canonical: canonical.clone(),
    };
    assert_eq!(OpsFrame::decode(&change.encode().unwrap()).unwrap(), change);
    let forged = OpsFrame::SignedChange {
        record: record(),
        digest: [0; 32],
        canonical,
    };
    assert!(
        forged.encode().is_err(),
        "digest must name the forwarded bytes"
    );
    let preview = OpsFrame::Preview {
        seq: 3,
        record: record(),
        ops: vec![PreviewOp::Insert {
            at: LiveAnchor {
                cursor: vec![4; 16],
            },
            text: "hello".into(),
        }],
    };
    assert_eq!(
        OpsFrame::decode(&preview.encode().unwrap()).unwrap(),
        preview
    );
    let too_many = OpsFrame::Preview {
        seq: 4,
        record: record(),
        ops: vec![
            PreviewOp::Delete {
                at: LiveAnchor { cursor: vec![1] },
                len: 1
            };
            MAX_PREVIEW_OPS + 1
        ],
    };
    assert!(too_many.encode().is_err());
    let empty_delete = OpsFrame::Preview {
        seq: 5,
        record: record(),
        ops: vec![PreviewOp::Delete {
            at: LiveAnchor { cursor: vec![1] },
            len: 0,
        }],
    };
    assert!(empty_delete.encode().is_err());
}

#[test]
fn decode_rejects_noncanonical_tails_and_oversize() {
    let frame = PresenceFrame {
        seq: 1,
        sent_at_ms: 2,
        focus: None,
        pointer: None,
        selection: None,
        drag: None,
    };
    let mut bytes = frame.encode().unwrap();
    bytes.push(0);
    assert_eq!(PresenceFrame::decode(&bytes), Err(LiveWireError::Codec));
    assert_eq!(
        PresenceFrame::decode(&vec![0; MAX_DATAGRAM_FRAME_BYTES + 1]),
        Err(LiveWireError::Codec)
    );
    assert_eq!(OpsFrame::decode(&[]), Err(LiveWireError::Codec));
}

#[test]
fn namespaces_and_tracks_are_stable_and_room_is_hashed() {
    let ns = LiveNamespace::new("room-a", [7; 32]);
    let parts = ns.components();
    assert_eq!(parts[0], LIVE_NAMESPACE_ROOT);
    assert!(!parts[1].contains("room-a"));
    assert_eq!(parts[2], hex::encode([7; 32]));
    assert_ne!(LiveNamespace::new("room-b", [7; 32]).room, ns.room);
    assert!(ops_track(&record()).starts_with(OPS_TRACK_PREFIX));
    assert_eq!(ops_track(&record()), ops_track(&record()));
}

#[test]
fn descriptors_may_advertise_live_beside_voice_but_nothing_else() {
    use super::super::descriptor::{MEDIA_CAPABILITIES, MEDIA_VOICE_V1};
    assert_eq!(MEDIA_CAPABILITIES, MEDIA_VOICE_V1 | MEDIA_LIVE_V1);
    assert_eq!(MEDIA_VOICE_V1 & MEDIA_LIVE_V1, 0);
}
