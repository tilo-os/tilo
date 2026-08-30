//! Signing + verification round-trips for events and signed ops.
//!
//! Covers the cryptographic-only contract: signing produces a sig
//! that verifies under the same public key, and any mismatch
//! (different key, tampered payload, tampered metadata) is rejected.
//! Validator-level concerns (replay, role lookup, etc.) live in
//! `validator.rs`.

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

use collab_types::crypto::VerifyError;
use collab_types::{
    ArtifactId, AttestationId, ChannelEvent, ChannelId, OpClass, Signature, SignedOp, SpaceId,
    generate_keypair, sign_event, sign_op, verify_event, verify_op,
};

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

#[test]
fn sign_event_round_trip() {
    let (sk, pk) = generate_keypair();
    let payload: Vec<u8> = b"hello".to_vec();
    let sig = sign_event(&payload, &sk).unwrap();
    assert!(verify_event(&payload, &sig, &pk).is_ok());
}

#[test]
fn sign_with_wrong_key_fails() {
    let (sk_a, _) = generate_keypair();
    let (_, pk_b) = generate_keypair();
    let payload: Vec<u8> = b"hello".to_vec();
    let sig = sign_event(&payload, &sk_a).unwrap();
    assert_eq!(
        verify_event(&payload, &sig, &pk_b).unwrap_err(),
        VerifyError::BadSignature
    );
}

#[test]
fn sign_op_then_verify() {
    let (sk, pk) = generate_keypair();
    let mut op = SignedOp::new_unsigned(
        b"automerge change".to_vec(),
        ArtifactId::new(),
        pk,
        AttestationId::new(),
        OpClass::Splice,
        t(10),
        1,
    );
    // Placeholder signature does not verify.
    assert!(verify_op(&op).is_err());

    sign_op(&mut op, &sk).unwrap();
    assert!(verify_op(&op).is_ok());
}

#[test]
fn sign_op_idempotent() {
    // Calling sign_op twice on the same op produces the same
    // signature because Ed25519 is deterministic and the signed view
    // omits the signature field.
    let (sk, pk) = generate_keypair();
    let mut op = SignedOp::new_unsigned(
        b"x".to_vec(),
        ArtifactId::new(),
        pk,
        AttestationId::new(),
        OpClass::Splice,
        t(10),
        1,
    );
    sign_op(&mut op, &sk).unwrap();
    let first = op.signature;
    sign_op(&mut op, &sk).unwrap();
    let second = op.signature;
    assert_eq!(first, second);
}

#[test]
fn op_signed_by_a_does_not_verify_with_b() {
    let (sk_a, _pk_a) = generate_keypair();
    let (_sk_b, pk_b) = generate_keypair();
    let mut op = SignedOp::new_unsigned(
        b"x".to_vec(),
        ArtifactId::new(),
        // Author says it's pk_b, but we sign with sk_a — signature
        // will not verify against pk_b.
        pk_b,
        AttestationId::new(),
        OpClass::Splice,
        t(10),
        1,
    );
    sign_op(&mut op, &sk_a).unwrap();
    assert!(verify_op(&op).is_err());
}

#[test]
fn tampering_op_author_breaks_signature() {
    let (sk, pk) = generate_keypair();
    let mut op = SignedOp::new_unsigned(
        b"x".to_vec(),
        ArtifactId::new(),
        pk,
        AttestationId::new(),
        OpClass::Splice,
        t(10),
        1,
    );
    sign_op(&mut op, &sk).unwrap();
    assert!(verify_op(&op).is_ok());
    // Changing author after signing breaks verification.
    let (_, other_pk) = generate_keypair();
    op.author = other_pk;
    assert!(verify_op(&op).is_err());
}

#[test]
fn tampering_op_op_class_breaks_signature() {
    let (sk, pk) = generate_keypair();
    let mut op = SignedOp::new_unsigned(
        b"x".to_vec(),
        ArtifactId::new(),
        pk,
        AttestationId::new(),
        OpClass::Comment,
        t(10),
        1,
    );
    sign_op(&mut op, &sk).unwrap();
    assert!(verify_op(&op).is_ok());
    // Lying about op class breaks verification — the op_class field
    // is covered by the signature.
    op.op_class = OpClass::Splice;
    assert!(verify_op(&op).is_err());
}

#[test]
fn tampering_op_attestation_id_breaks_signature() {
    let (sk, pk) = generate_keypair();
    let mut op = SignedOp::new_unsigned(
        b"x".to_vec(),
        ArtifactId::new(),
        pk,
        AttestationId::new(),
        OpClass::Splice,
        t(10),
        1,
    );
    sign_op(&mut op, &sk).unwrap();
    assert!(verify_op(&op).is_ok());
    op.attestation_id = AttestationId::new();
    assert!(verify_op(&op).is_err());
}

#[test]
fn channel_event_genesis_signs_and_verifies() {
    let (sk, pk) = generate_keypair();
    let mut g = ChannelEvent::Genesis {
        channel_id: ChannelId::new(),
        space: SpaceId::new(),
        name: "g".into(),
        owner: pk,
        created_at: t(0),
        signature: Signature::from_bytes([0u8; 64]),
    };
    let view = g.signing_bytes().unwrap();
    let sig = sign_event(&view, &sk).unwrap();
    if let ChannelEvent::Genesis { signature, .. } = &mut g {
        *signature = sig;
    }
    let view2 = g.signing_bytes().unwrap();
    assert!(verify_event(&view2, &sig, &pk).is_ok());
}

#[test]
fn channel_event_rename_signs_and_verifies() {
    let (sk, pk) = generate_keypair();
    let mut e = ChannelEvent::Rename {
        new_name: "renamed".into(),
        signed_by: pk,
        signature: Signature::from_bytes([0u8; 64]),
    };
    let view = e.signing_bytes().unwrap();
    let sig = sign_event(&view, &sk).unwrap();
    if let ChannelEvent::Rename { signature, .. } = &mut e {
        *signature = sig;
    }
    let view2 = e.signing_bytes().unwrap();
    assert!(verify_event(&view2, &sig, &pk).is_ok());
}
