//! Signing + verification round-trips for events and signed ops.
//!
//! Covers the cryptographic-only contract: signing produces a sig
//! that verifies under the same public key, and any mismatch
//! (different key, tampered payload, tampered metadata) is rejected.
//! Validator-level concerns (replay, role lookup, etc.) live in
//! `validator.rs`.

#![allow(
    clippy::unwrap_used,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, SystemTime};

use collab_types::crypto::VerifyError;
use collab_types::{
    ArtifactId, AttestationId, ChannelEvent, ChannelId, Ed25519PublicKey, OpKind, Signature,
    SignedOp, SpaceId, generate_keypair, sign_event, sign_op, verify_event, verify_op,
};

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap()
}

fn unsigned_op(author: Ed25519PublicKey, payload: &[u8], kind: OpKind) -> SignedOp {
    SignedOp::new_unsigned(
        payload.to_vec(),
        ArtifactId::new(),
        author,
        AttestationId::new(),
        kind,
        t(10),
        1,
    )
}

/// An event signature verifies under the signer's key — for raw payloads and
/// for the genesis and rename channel events' signing views — and not under
/// another key.
#[test]
fn an_event_signature_verifies_under_its_signer_only() {
    // Case: sign_event round trip.
    let (sk, pk) = generate_keypair();
    let payload: Vec<u8> = b"hello".to_vec();
    let sig = sign_event(&payload, &sk).unwrap();
    assert!(verify_event(&payload, &sig, &pk).is_ok());

    // Case: signing with the wrong key fails.
    let (sk_a, _) = generate_keypair();
    let (_, pk_b) = generate_keypair();
    let sig = sign_event(&payload, &sk_a).unwrap();
    assert_eq!(
        verify_event(&payload, &sig, &pk_b).unwrap_err(),
        VerifyError::BadSignature
    );

    // Cases: channel events (genesis, rename) sign and verify over their
    // signing view, with the signature slot filled in afterwards.
    let (sk, pk) = generate_keypair();
    let events = [
        (
            "genesis",
            ChannelEvent::Genesis {
                channel_id: ChannelId::new(),
                space: SpaceId::new(),
                name: "g".into(),
                owner: pk,
                created_at: t(0),
                signature: Signature::from_bytes([0u8; 64]),
            },
        ),
        (
            "rename",
            ChannelEvent::Rename {
                new_name: "renamed".into(),
                signed_by: pk,
                signature: Signature::from_bytes([0u8; 64]),
            },
        ),
    ];
    for (case, mut e) in events {
        let view = e.signing_bytes().unwrap();
        let sig = sign_event(&view, &sk).unwrap();
        match &mut e {
            ChannelEvent::Genesis { signature, .. } | ChannelEvent::Rename { signature, .. } => {
                *signature = sig;
            }
            _ => unreachable!(),
        }
        let view2 = e.signing_bytes().unwrap();
        assert!(verify_event(&view2, &sig, &pk).is_ok(), "case {case}");
    }
}

/// `sign_op` produces a deterministic signature that verifies only under the
/// op's stated author and only while every signed field is untouched.
#[test]
fn a_signed_op_verifies_only_under_its_author_and_while_untampered() {
    // Case: sign op then verify; the placeholder signature does not verify.
    let (sk, pk) = generate_keypair();
    let mut op = unsigned_op(pk, b"automerge change", OpKind::Change);
    assert!(verify_op(&op).is_err());
    sign_op(&mut op, &sk).unwrap();
    assert!(verify_op(&op).is_ok());

    // Case: sign_op is idempotent. Calling it twice on the same op produces
    // the same signature because Ed25519 is deterministic and the signed view
    // omits the signature field.
    let (sk, pk) = generate_keypair();
    let mut op = unsigned_op(pk, b"x", OpKind::Change);
    sign_op(&mut op, &sk).unwrap();
    let first = op.signature;
    sign_op(&mut op, &sk).unwrap();
    let second = op.signature;
    assert_eq!(first, second);

    // Case: an op signed by A does not verify with B. The author says it's
    // pk_b, but we sign with sk_a — the signature will not verify against
    // pk_b.
    let (sk_a, _pk_a) = generate_keypair();
    let (_sk_b, pk_b) = generate_keypair();
    let mut op = unsigned_op(pk_b, b"x", OpKind::Change);
    sign_op(&mut op, &sk_a).unwrap();
    assert!(verify_op(&op).is_err());

    // Cases: tampering with a signed field after signing breaks verification.
    let tamper_author: fn(&mut SignedOp) = |op| op.author = generate_keypair().1;
    // Lying about op class — the kind field is covered by the signature.
    let tamper_op_class: fn(&mut SignedOp) = |op| op.kind = OpKind::Compaction;
    let tamper_attestation: fn(&mut SignedOp) = |op| op.attestation_id = AttestationId::new();
    for (case, kind, tamper) in [
        ("author", OpKind::Change, tamper_author),
        ("op kind", OpKind::Change, tamper_op_class),
        ("attestation id", OpKind::Change, tamper_attestation),
    ] {
        let (sk, pk) = generate_keypair();
        let mut op = unsigned_op(pk, b"x", kind);
        sign_op(&mut op, &sk).unwrap();
        assert!(verify_op(&op).is_ok(), "case {case}");
        tamper(&mut op);
        assert!(verify_op(&op).is_err(), "case {case}");
    }
}
