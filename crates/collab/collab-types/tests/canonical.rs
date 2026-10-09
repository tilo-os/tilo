//! Integration tests for canonical-bytes round-trip + tamper detection.
//!
//! Every wire type must round-trip through `canonical_bytes` /
//! `from_canonical_bytes` byte-for-byte. We also assert that one-byte
//! tampering of an *encoded* envelope makes signature verification
//! fail — this is the property that makes signed ops trustworthy.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, SystemTime};

use collab_types::canonical::from_canonical_bytes;
use collab_types::channel::DocumentHandle;
use collab_types::identity::{AvatarBytes, Identity};
use collab_types::{
    Artifact, ArtifactId, ArtifactKind, Attestation, AttestationId, Channel, ChannelEvent,
    ChannelId, ChannelSettingsDelta, Claim, Clearance, Ed25519PublicKey, MemberRef, OpKind, Role,
    RoleAttestation, Signature, SignedOp, Space, SpaceId, canonical_bytes, generate_keypair,
    sign_event, sign_op, verify_event, verify_op,
};

// ---------- helpers ----------

fn fake_pk(byte: u8) -> Ed25519PublicKey {
    Ed25519PublicKey::from_bytes([byte; 32])
}

fn fake_sig(byte: u8) -> Signature {
    Signature::from_bytes([byte; 64])
}

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap()
}

// ---------- round-trips for every wire type ----------

/// Encodes `value`, decodes it back and asserts equality, naming the case.
fn assert_round_trips<T>(case: &str, value: &T)
where
    T: serde::Serialize + for<'de> serde::Deserialize<'de> + PartialEq + std::fmt::Debug,
{
    let bytes = canonical_bytes(value).unwrap();
    let back: T = from_canonical_bytes(&bytes).unwrap();
    assert_eq!(value, &back, "case {case}");
}

/// Every wire type round-trips through `canonical_bytes` /
/// `from_canonical_bytes`, and encoding is deterministic.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one literal fixture per wire type; splitting them apart would only scatter the list"
)]
fn every_wire_type_round_trips_deterministically() {
    assert_round_trips("clearance", &Clearance::CONFIDENTIAL);
    for r in [Role::Owner, Role::Editor, Role::Commenter, Role::Viewer] {
        assert_round_trips(&format!("role {r:?}"), &r);
    }
    for op in [OpKind::Change, OpKind::Compaction, OpKind::Redaction] {
        assert_round_trips(&format!("op kind {op:?}"), &op);
    }
    for k in [
        ArtifactKind::Note,
        ArtifactKind::File,
        ArtifactKind::Cluster,
        ArtifactKind::Kanban,
        ArtifactKind::ChatLog,
    ] {
        assert_round_trips(&format!("artifact kind {k:?}"), &k);
    }
    assert_round_trips(
        "claim member-of",
        &Claim::MemberOf {
            space: SpaceId::new(),
        },
    );
    assert_round_trips(
        "claim custom",
        &Claim::Custom {
            kind: "com.example.badge".into(),
            value: vec![1, 2, 3, 4, 5],
        },
    );
    assert_round_trips(
        "attestation",
        &Attestation {
            id: AttestationId::new(),
            issued_by: fake_pk(1),
            subject: fake_pk(2),
            claims: vec![Claim::ClearedAt(Clearance::INTERNAL)],
            valid_from: t(0),
            valid_until: t(86_400),
            signature: fake_sig(0xab),
        },
    );
    assert_round_trips(
        "identity with avatar",
        &Identity {
            public_key: fake_pk(7),
            label: "Quinten — Personal".into(),
            avatar: Some(AvatarBytes(vec![0x89, b'P', b'N', b'G'])),
            attestations: vec![],
        },
    );
    assert_round_trips(
        "space",
        &Space {
            id: SpaceId::new(),
            name: "ACME".into(),
            trust_root: fake_pk(9),
            default_identity_label: Some("Quinten".into()),
            channels: vec![],
            member_directory: vec![],
        },
    );
    assert_round_trips(
        "channel",
        &Channel {
            id: ChannelId::new(),
            space: SpaceId::new(),
            name: "#engineering".into(),
            owner: fake_pk(11),
            default_role: Role::Commenter,
            members: vec![MemberRef {
                identity: fake_pk(12),
                role: Role::Editor,
                attestation: AttestationId::new(),
            }],
            observe_default: true,
            min_clearance: Some(Clearance::CONFIDENTIAL),
            artifacts: vec![],
            presence_visible_for_viewers: true,
        },
    );
    assert_round_trips(
        "artifact",
        &Artifact {
            id: ArtifactId::new(),
            kind: ArtifactKind::Note,
            channel: ChannelId::new(),
            classification: Some(Clearance::INTERNAL),
            crdt_doc: Some(DocumentHandle(vec![9, 9, 9])),
        },
    );
    assert_round_trips(
        "signed op",
        &SignedOp::new_unsigned(
            b"automerge change blob".to_vec(),
            ArtifactId::new(),
            fake_pk(3),
            AttestationId::new(),
            OpKind::Change,
            t(100_000),
            42,
        ),
    );

    // Every channel event variant.
    let pk = fake_pk(5);
    let events = [
        ChannelEvent::Genesis {
            channel_id: ChannelId::new(),
            space: SpaceId::new(),
            name: "g".into(),
            owner: pk,
            created_at: t(1),
            signature: fake_sig(0x01),
        },
        ChannelEvent::AttestationIssued(RoleAttestation {
            channel: ChannelId::new(),
            role: Role::Editor,
            attestation: Attestation {
                id: AttestationId::new(),
                issued_by: pk,
                subject: fake_pk(6),
                claims: vec![],
                valid_from: t(0),
                valid_until: t(86_400),
                signature: fake_sig(0x02),
            },
        }),
        ChannelEvent::AttestationRevoked {
            attestation: AttestationId::new(),
            signed_by: pk,
            signature: fake_sig(0x03),
        },
        ChannelEvent::Rename {
            new_name: "renamed".into(),
            signed_by: pk,
            signature: fake_sig(0x05),
        },
        ChannelEvent::SettingsUpdate {
            delta: ChannelSettingsDelta {
                default_role: Some(Role::Editor),
                observe_default: Some(true),
                min_clearance: Some(Clearance::CONFIDENTIAL),
                presence_visible_for_viewers: Some(false),
            },
            signed_by: pk,
            signature: fake_sig(0x06),
        },
    ];
    for (i, e) in events.iter().enumerate() {
        assert_round_trips(&format!("channel event #{i}"), e);
    }

    // Determinism: two encodes of an identical value must produce identical
    // bytes. The signing protocol relies on this property.
    let op = SignedOp::new_unsigned(
        b"x".to_vec(),
        ArtifactId(uuid::Uuid::from_u128(0xdead_beef)),
        fake_pk(2),
        AttestationId(uuid::Uuid::from_u128(0xcafe_f00d)),
        OpKind::Change,
        t(100),
        7,
    );
    let a = canonical_bytes(&op).unwrap();
    let b = canonical_bytes(&op).unwrap();
    assert_eq!(a, b);
}

// ---------- tamper detection: decode failure ----------

/// Decoding demands exactly the encoded bytes: trailing bytes and any
/// truncation are rejected.
#[test]
fn decode_rejects_trailing_or_truncated_bytes() {
    // Case: trailing bytes after a struct. postcard is structural — there
    // are no field names, so an "extra field" surfaces as trailing bytes on
    // the wire. Our `from_canonical_bytes` enforces exact-length input
    // precisely so trailing data can't slip past undetected.
    // (`deny_unknown_fields` on the struct only takes effect for name-based
    // deserialisers like JSON; the exact-length check is the
    // postcard-equivalent protection.)
    let s = Space {
        id: SpaceId::new(),
        name: "x".into(),
        trust_root: fake_pk(1),
        default_identity_label: None,
        channels: vec![],
        member_directory: vec![],
    };
    let mut bytes = canonical_bytes(&s).unwrap();
    bytes.push(0xff);
    assert!(
        from_canonical_bytes::<Space>(&bytes).is_err(),
        "trailing byte should fail exact-length decode"
    );
    bytes.push(0xff);
    bytes.push(0xff);
    assert!(from_canonical_bytes::<Space>(&bytes).is_err());

    // Case: truncated attestation.
    let a = Attestation {
        id: AttestationId::new(),
        issued_by: fake_pk(1),
        subject: fake_pk(2),
        claims: vec![Claim::EmployeeOf("ACME".into())],
        valid_from: t(0),
        valid_until: t(86_400),
        signature: fake_sig(0),
    };
    let bytes = canonical_bytes(&a).unwrap();
    for n in 1..=bytes.len() {
        let truncated = &bytes[..bytes.len() - n];
        assert!(
            from_canonical_bytes::<Attestation>(truncated).is_err(),
            "truncation by {n} bytes should fail decode"
        );
        if n > 3 {
            break;
        }
    }
}

/// Tampering with signed bytes breaks decode or signature — never neither.
#[test]
fn tampering_breaks_decode_or_signature() {
    // Case: one-byte flip in an attestation. After we encode + sign + ship
    // an attestation, an adversary who flips a single byte either breaks the
    // postcard decode OR the signature check — never neither. We test both
    // pathways.
    let (sk, pk) = generate_keypair();
    let mut att = Attestation {
        id: AttestationId::new(),
        issued_by: pk,
        subject: pk,
        claims: vec![Claim::EmployeeOf("ACME".into())],
        valid_from: t(0),
        valid_until: t(86_400),
        signature: fake_sig(0),
    };
    let signing_bytes = att.signing_bytes().unwrap();
    let sig = sign_event(&signing_bytes, &sk).unwrap();
    att.signature = sig;

    // Sanity check: untampered attestation verifies.
    let sb = att.signing_bytes().unwrap();
    assert!(verify_event(&sb, &att.signature, &pk).is_ok());

    let bytes = canonical_bytes(&att).unwrap();
    // Try every single-byte flip on the encoded bytes; either decode
    // fails or, if it succeeds, the recovered attestation's signature
    // no longer verifies.
    let mut hit_decode_fail = false;
    let mut hit_signature_fail = false;
    for i in 0..bytes.len() {
        let mut tampered = bytes.clone();
        tampered[i] ^= 0x01;
        match from_canonical_bytes::<Attestation>(&tampered) {
            Err(_) => hit_decode_fail = true,
            Ok(decoded) => {
                let sb2 = decoded.signing_bytes().unwrap();
                if verify_event(&sb2, &decoded.signature, &decoded.issued_by).is_err() {
                    hit_signature_fail = true;
                } else {
                    // Decoded *and* signature still verifies → the
                    // tampered byte was in a "free" position. There
                    // are no such positions in a well-formed signed
                    // attestation because every byte is either part
                    // of the signed view or part of the signature
                    // itself. If this fires, something's wrong.
                    panic!("byte {i} flip survived both decode and signature");
                }
            }
        }
    }
    assert!(hit_decode_fail);
    assert!(hit_signature_fail);

    // Cases: tampering a signed op's payload or seq breaks verification.
    let flip_payload: fn(&mut SignedOp) = |op| op.payload[0] ^= 0x80;
    // Bumping seq invalidates the signature.
    let bump_seq: fn(&mut SignedOp) = |op| op.seq = 8;
    for (case, payload, seq, tamper) in [
        ("payload", &b"original payload"[..], 1, flip_payload),
        ("seq", &b"payload"[..], 7, bump_seq),
    ] {
        let (sk, pk) = generate_keypair();
        let mut op = SignedOp::new_unsigned(
            payload.to_vec(),
            ArtifactId::new(),
            pk,
            AttestationId::new(),
            OpKind::Change,
            t(1),
            seq,
        );
        sign_op(&mut op, &sk).unwrap();
        // Untampered verifies.
        assert!(verify_op(&op).is_ok(), "case {case}");
        tamper(&mut op);
        assert!(verify_op(&op).is_err(), "case {case}");
    }
}
