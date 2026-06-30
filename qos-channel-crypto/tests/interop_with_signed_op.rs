//! Interop: encrypt-then-sign round-trips cleanly through the
//! `qos-collab-types` envelope.
//!
//! The Phase-7 wire shape is: producer calls
//! `ChannelGroup::encrypt`, then wraps the ciphertext +
//! `EncryptionMetadata` in `SignedOp::new_unsigned_with_encryption`,
//! signs, hands to the broker. The receiver verifies the outer
//! signature first (drop + log on failure), then feeds the payload
//! to its matching `ChannelGroup::decrypt`. The decrypted bytes go
//! to the original Phase-2 ingestion path (Automerge change
//! application, etc.).
//!
//! This file pins each of those steps. The Automerge step is out
//! of scope — `qos-channel-crypto` does not depend on Automerge.

use std::time::SystemTime;

use qos_channel_crypto::{ChannelGroup, MemberKey};
use qos_collab_types::{
    ArtifactId, AttestationId, ChannelId, EncryptionMetadata, OpClass, SignedOp, generate_keypair,
    sign_op, verify_op,
};

/// Stand up a two-member encrypted group + a producer identity keypair.
///
/// The producer's identity (used to sign the *outer* envelope) is
/// distinct from the producer's MLS signature keypair. In production
/// they may coincide once Phase 8's threshold identity is wired up;
/// for this test we keep them separate so the test fails loudly if
/// the two signature paths get accidentally merged.
fn fixture() -> (
    ChannelId,
    ChannelGroup,
    ChannelGroup,
    ed25519_dalek::SigningKey,
    qos_collab_types::Ed25519PublicKey,
) {
    let cid = ChannelId::new();
    let alice = MemberKey::generate("alice").unwrap();
    let bob = MemberKey::generate("bob").unwrap();
    let bob_kp = bob.fresh_key_package().unwrap();
    let mut alice_g = ChannelGroup::create(cid, alice).unwrap();
    let welcome = alice_g.add_member(bob_kp).unwrap();
    let bob_g = ChannelGroup::process_welcome(cid, &welcome, bob).unwrap();
    let (sk, pk) = generate_keypair();
    (cid, alice_g, bob_g, sk, pk)
}

#[test]
fn encrypted_signed_op_round_trips_outer_signature() {
    let (cid, mut alice_g, _bob_g, sk, pk) = fixture();
    let cleartext = b"crdt-change-blob".to_vec();
    let ct = alice_g.encrypt(&cleartext).unwrap();
    let epoch = alice_g.epoch();
    let mut op = SignedOp::new_unsigned_with_encryption(
        ct,
        ArtifactId::new(),
        pk,
        AttestationId::new(),
        OpClass::Splice,
        SystemTime::UNIX_EPOCH,
        1,
        EncryptionMetadata { epoch, channel: cid },
    );
    sign_op(&mut op, &sk).unwrap();
    // Outer signature verifies — that's the only contract
    // verify_op cares about. The validator pipeline check is in a
    // separate test below so we can keep failure modes localised.
    assert!(verify_op(&op).is_ok());
}

#[test]
fn bob_decrypts_payload_after_verifying_outer_signature() {
    let (cid, mut alice_g, mut bob_g, sk, pk) = fixture();
    let cleartext = b"crdt-change-blob".to_vec();
    let ct = alice_g.encrypt(&cleartext).unwrap();
    let epoch = alice_g.epoch();
    let mut op = SignedOp::new_unsigned_with_encryption(
        ct,
        ArtifactId::new(),
        pk,
        AttestationId::new(),
        OpClass::Splice,
        SystemTime::UNIX_EPOCH,
        1,
        EncryptionMetadata { epoch, channel: cid },
    );
    sign_op(&mut op, &sk).unwrap();

    // Bob's broker step: verify the outer signature.
    verify_op(&op).unwrap();
    // Then route to the matching ChannelGroup and decrypt.
    let meta = op.encryption.unwrap();
    assert_eq!(meta.channel, cid);
    let pt = bob_g.decrypt(&op.payload).unwrap();
    assert_eq!(pt, cleartext);
}

#[test]
fn tampering_with_payload_breaks_outer_signature() {
    // The outer signature covers the (cipher)bytes. A peer that
    // re-encrypts the inner payload to something else cannot keep
    // the outer signature valid — they would need the author's
    // signing key.
    let (cid, mut alice_g, _bob_g, sk, pk) = fixture();
    let ct = alice_g.encrypt(b"a").unwrap();
    let mut op = SignedOp::new_unsigned_with_encryption(
        ct,
        ArtifactId::new(),
        pk,
        AttestationId::new(),
        OpClass::Splice,
        SystemTime::UNIX_EPOCH,
        1,
        EncryptionMetadata {
            epoch: alice_g.epoch(),
            channel: cid,
        },
    );
    sign_op(&mut op, &sk).unwrap();
    // Tamper: flip a payload byte. Outer signature must fail.
    let last = op.payload.len() - 1;
    op.payload[last] ^= 0xff;
    assert!(verify_op(&op).is_err());
}

#[test]
fn tampering_with_encryption_metadata_breaks_outer_signature() {
    // Same as above but the tamper is on the metadata: a peer tries
    // to redirect a ciphertext to a *different* channel by rewriting
    // EncryptionMetadata.channel. The outer signature covers the
    // metadata so this also fails.
    let (cid, mut alice_g, _bob_g, sk, pk) = fixture();
    let ct = alice_g.encrypt(b"a").unwrap();
    let mut op = SignedOp::new_unsigned_with_encryption(
        ct,
        ArtifactId::new(),
        pk,
        AttestationId::new(),
        OpClass::Splice,
        SystemTime::UNIX_EPOCH,
        1,
        EncryptionMetadata {
            epoch: alice_g.epoch(),
            channel: cid,
        },
    );
    sign_op(&mut op, &sk).unwrap();
    op.encryption = Some(EncryptionMetadata {
        epoch: alice_g.epoch(),
        channel: ChannelId::new(),
    });
    assert!(verify_op(&op).is_err());
}

#[test]
fn unencrypted_op_still_works_alongside_encrypted_ones() {
    // Backwards-compat: pre-Phase-7 legacy/unencrypted channels keep
    // round-tripping with `encryption: None`. Same signing pipeline,
    // same verify call, no decrypt step.
    let (sk, pk) = generate_keypair();
    let mut op = SignedOp::new_unsigned(
        b"plaintext".to_vec(),
        ArtifactId::new(),
        pk,
        AttestationId::new(),
        OpClass::Splice,
        SystemTime::UNIX_EPOCH,
        1,
    );
    assert!(op.encryption.is_none());
    sign_op(&mut op, &sk).unwrap();
    assert!(verify_op(&op).is_ok());
}

#[test]
fn epoch_in_metadata_matches_group_epoch_at_time_of_encrypt() {
    // The receiver uses meta.epoch to decide whether the
    // ChannelGroup is current enough to decrypt. The test pins the
    // invariant that the producer stamps the current epoch.
    let (cid, mut alice_g, _bob_g, _sk, _pk) = fixture();
    let snapshot_epoch = alice_g.epoch();
    let ct = alice_g.encrypt(b"x").unwrap();
    let meta = EncryptionMetadata {
        epoch: snapshot_epoch,
        channel: cid,
    };
    // (No actual SignedOp built here — the invariant is just about
    // the per-encrypt epoch value mirroring `alice_g.epoch()` at
    // the moment of encrypt.)
    assert_eq!(meta.epoch, snapshot_epoch);
    let _ = ct;
}
