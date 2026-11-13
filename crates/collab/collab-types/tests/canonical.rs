//! Integration tests for canonical-bytes round-trip + tamper detection.
//!
//! Every wire type must round-trip through `canonical_bytes` /
//! `from_canonical_bytes` byte-for-byte. We also assert that one-byte
//! tampering of an *encoded* envelope makes signature verification
//! fail — this is the property that makes signed ops trustworthy.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, SystemTime};

use collab_types::canonical::from_canonical_bytes;
use collab_types::identity::AvatarBytes;
use collab_types::presence::PresenceHue;
use collab_types::{
    ArtifactId, ArtifactKind, AttestationId, Ed25519PublicKey, OpKind, Person, PersonId, Role,
    SignedOp, canonical_bytes, generate_keypair, sign_op, verify_op,
};

fn fake_pk(byte: u8) -> Ed25519PublicKey {
    Ed25519PublicKey::from_bytes([byte; 32])
}

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap()
}

/// Encodes `value`, decodes it back and asserts equality, naming the case.
fn assert_round_trips<T>(case: &str, value: &T)
where
    T: serde::Serialize + for<'de> serde::Deserialize<'de> + PartialEq + std::fmt::Debug,
{
    let bytes = canonical_bytes(value).unwrap();
    let back: T = from_canonical_bytes(&bytes).unwrap();
    assert_eq!(value, &back, "case {case}");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one literal fixture per wire type; splitting them apart would only scatter the list"
)]
fn every_wire_type_round_trips_deterministically() {
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
    assert_round_trips("avatar bytes", &AvatarBytes(vec![0x89, b'P', b'N', b'G']));
    assert_round_trips(
        "person with avatar",
        &Person {
            id: PersonId::new("ada"),
            name: "Ada".into(),
            avatar: Some(AvatarBytes(vec![0x89, b'P', b'N', b'G'])),
            colour: PresenceHue::Violet,
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
    let op = SignedOp::new_unsigned(
        b"x".to_vec(),
        ArtifactId::new(),
        fake_pk(1),
        AttestationId::new(),
        OpKind::Change,
        t(1),
        1,
    );
    let mut bytes = canonical_bytes(&op).unwrap();
    bytes.push(0xff);
    assert!(
        from_canonical_bytes::<SignedOp>(&bytes).is_err(),
        "trailing byte should fail exact-length decode"
    );
    bytes.push(0xff);
    bytes.push(0xff);
    assert!(from_canonical_bytes::<SignedOp>(&bytes).is_err());

    // Case: truncated op.
    let op = SignedOp::new_unsigned(
        b"payload".to_vec(),
        ArtifactId::new(),
        fake_pk(1),
        AttestationId::new(),
        OpKind::Change,
        t(1),
        1,
    );
    let bytes = canonical_bytes(&op).unwrap();
    for n in 1..=bytes.len() {
        let truncated = &bytes[..bytes.len() - n];
        assert!(
            from_canonical_bytes::<SignedOp>(truncated).is_err(),
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
    // Case: one-byte flip in a signed op. After we encode + sign + ship an
    // op, an adversary who flips a single byte either breaks the postcard
    // decode OR the signature check — never neither.
    let (sk, pk) = generate_keypair();
    let mut op = SignedOp::new_unsigned(
        b"automerge change blob".to_vec(),
        ArtifactId::new(),
        pk,
        AttestationId::new(),
        OpKind::Change,
        t(1),
        1,
    );
    sign_op(&mut op, &sk).unwrap();
    assert!(verify_op(&op).is_ok());

    let bytes = canonical_bytes(&op).unwrap();
    let mut hit_decode_fail = false;
    let mut hit_signature_fail = false;
    for i in 0..bytes.len() {
        let mut tampered = bytes.clone();
        tampered[i] ^= 0x01;
        match from_canonical_bytes::<SignedOp>(&tampered) {
            Err(_) => hit_decode_fail = true,
            Ok(decoded) => {
                if verify_op(&decoded).is_err() {
                    hit_signature_fail = true;
                } else {
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
        assert!(verify_op(&op).is_ok(), "case {case}");
        tamper(&mut op);
        assert!(verify_op(&op).is_err(), "case {case}");
    }
}
