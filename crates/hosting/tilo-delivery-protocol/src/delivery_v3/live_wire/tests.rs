use super::*;
use ed25519_dalek::SigningKey;

fn record() -> LiveRecord {
    LiveRecord {
        app: "so.tilo.word-processor".into(),
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
    m.audience = "https://127.0.0.1:19343/moq/voice".into();
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

#[test]
fn agent_publisher_is_deterministic_and_domain_separated() {
    let room = "4f1e5c3a-0000-4000-8000-000000000001";
    let host = [7; 32];
    let id = agent_publisher(room, host, "agent-1");
    assert_eq!(id, agent_publisher(room, host, "agent-1"));
    // Exact construction.
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-live-agent-v1\0");
    hash.update(&(room.len() as u64).to_le_bytes());
    hash.update(room.as_bytes());
    hash.update(&host);
    hash.update(b"agent-1");
    assert_eq!(id, *hash.finalize().as_bytes());
    // Every input separates; the length prefix stops room/host splicing.
    assert_ne!(id, agent_publisher(room, host, "agent-2"));
    assert_ne!(id, agent_publisher(room, [8; 32], "agent-1"));
    assert_ne!(id, agent_publisher("other-room", host, "agent-1"));
    assert_ne!(
        agent_publisher("ab", host, "c"),
        agent_publisher("a", host, "bc")
    );
    // Never the room digest or a record digest under another domain.
    assert_ne!(id, room_digest(room));
    assert_ne!(id, host);
    let namespace = agent_namespace(room, host, "agent-1");
    assert_eq!(namespace.room, room_digest(room));
    assert_eq!(namespace.publisher, id);
}

fn activity() -> AgentActivity {
    AgentActivity {
        v: 1,
        agent: "agent-1".into(),
        name: "Scribe".into(),
        state: "writing".into(),
        label: "Writing in Writer".into(),
        tool: Some("append-paragraph".into()),
        at_ms: 1_700_000_000_000,
        status: Some("running".into()),
    }
}

#[test]
fn agent_activity_round_trips_as_one_preview_insert() {
    let value = activity();
    let json = value.to_json().unwrap();
    assert_eq!(
        json,
        r#"{"v":1,"agent":"agent-1","name":"Scribe","state":"writing","label":"Writing in Writer","tool":"append-paragraph","at_ms":1700000000000,"status":"running"}"#
    );
    assert_eq!(AgentActivity::from_json(&json).unwrap(), value);
    let frame = value.to_frame(9).unwrap();
    let bytes = frame.encode().unwrap();
    let decoded = OpsFrame::decode(&bytes).unwrap();
    assert_eq!(
        AgentActivity::from_frame(&decoded).unwrap(),
        (9, value.clone())
    );
    let OpsFrame::Preview { record, ops, .. } = &decoded else {
        panic!()
    };
    assert_eq!(
        (record.app.as_str(), record.record.as_str()),
        (AGENT_ACTIVITY_APP, AGENT_ACTIVITY_RECORD)
    );
    assert!(
        matches!(&ops[..], [PreviewOp::Insert { at, .. }] if at.cursor == AGENT_ACTIVITY_ANCHOR)
    );
    let no_tool = AgentActivity {
        tool: None,
        label: String::new(),
        ..value
    };
    assert!(no_tool.to_json().unwrap().contains(r#""tool":null"#));
    assert_eq!(
        AgentActivity::from_json(&no_tool.to_json().unwrap()).unwrap(),
        no_tool
    );
    // Another record or anchor is not an activity frame.
    let foreign = OpsFrame::Preview {
        seq: 1,
        record: record_fixture(),
        ops: vec![PreviewOp::Insert {
            at: LiveAnchor {
                cursor: AGENT_ACTIVITY_ANCHOR.to_vec(),
            },
            text: json,
        }],
    };
    assert!(AgentActivity::from_frame(&foreign).is_err());
}

fn record_fixture() -> LiveRecord {
    record()
}

#[test]
fn agent_activity_enforces_closed_states_and_bounds() {
    let ok = activity();
    for state in AGENT_ACTIVITY_STATES {
        AgentActivity {
            state: state.into(),
            ..ok.clone()
        }
        .validate()
        .unwrap();
    }
    let bad = [
        AgentActivity { v: 2, ..ok.clone() },
        AgentActivity {
            state: "sleeping".into(),
            ..ok.clone()
        },
        AgentActivity {
            agent: String::new(),
            ..ok.clone()
        },
        AgentActivity {
            agent: "a".repeat(129),
            ..ok.clone()
        },
        AgentActivity {
            name: String::new(),
            ..ok.clone()
        },
        AgentActivity {
            name: "n".repeat(65),
            ..ok.clone()
        },
        AgentActivity {
            label: "l".repeat(121),
            ..ok.clone()
        },
        AgentActivity {
            label: "line\nbreak".into(),
            ..ok.clone()
        },
        AgentActivity {
            tool: Some(String::new()),
            ..ok.clone()
        },
        AgentActivity {
            tool: Some("t".repeat(65)),
            ..ok.clone()
        },
    ];
    for value in bad {
        assert!(value.to_json().is_err(), "{value:?}");
    }
    // Largest valid value still fits one preview insert.
    let max = AgentActivity {
        agent: "a".repeat(128),
        name: "n".repeat(64),
        label: "l".repeat(120),
        tool: Some("t".repeat(64)),
        at_ms: u64::MAX,
        status: Some("completed".into()),
        ..ok.clone()
    };
    assert!(max.to_json().unwrap().len() <= MAX_PREVIEW_TEXT_BYTES);
    assert!(AgentActivity::from_json(r#"{"v":1}"#).is_err());
    let extra = ok
        .to_json()
        .unwrap()
        .replace("\"v\":1", "\"v\":1,\"extra\":true");
    assert!(
        AgentActivity::from_json(&extra).is_err(),
        "unknown fields are rejected"
    );
    assert!(AgentActivity::from_json(&"x".repeat(MAX_PREVIEW_TEXT_BYTES + 1)).is_err());
    assert_eq!(bounded_display_text("a\u{1}béc", 5), "a bé");
    assert_eq!(bounded_display_text("ééé", 5), "éé");
}

#[test]
fn digest_scoped_anchors_are_valid_opaque_selections_within_one_datagram() {
    let mut anchor = vec![ANCHOR_TAG_DIGEST, 0xac, 0x02];
    anchor.extend_from_slice(&[0x9c; ANCHOR_SCOPE_DIGEST_BYTES]);
    let frame = PresenceFrame {
        seq: 1,
        sent_at_ms: 2,
        focus: Some(record()),
        pointer: None,
        selection: Some((
            LiveAnchor {
                cursor: anchor.clone(),
            },
            LiveAnchor {
                cursor: vec![ANCHOR_TAG_SAME_SCOPE, 0xb0, 0x02],
            },
        )),
        drag: None,
    };
    let bytes = frame.encode().unwrap();
    assert!(bytes.len() <= MAX_DATAGRAM_FRAME_BYTES);
    assert_eq!(PresenceFrame::decode(&bytes).unwrap(), frame);
    assert!(anchor.len() <= MAX_OBJECT_ID_BYTES);
    assert_ne!(ANCHOR_TAG_DIGEST, ANCHOR_TAG_SCOPED);
    assert_ne!(ANCHOR_TAG_DIGEST, ANCHOR_TAG_SAME_SCOPE);
}

#[test]
fn agent_activity_status_is_optional_and_closed() {
    let mut value = activity();
    value.status = None;
    let json = value.to_json().unwrap();
    assert!(
        !json.contains("status"),
        "absent when unknown (older hosts)"
    );
    assert_eq!(AgentActivity::from_json(&json).unwrap(), value);
    value.status = Some("waiting".into());
    assert_eq!(
        AgentActivity::from_json(&value.to_json().unwrap()).unwrap(),
        value
    );
    value.status = Some("sleeping".into());
    assert!(value.to_json().is_err());
}
