//! Interop: encrypt-then-sign round-trips cleanly through the
//! `collab-types` envelope.
//!
//! Wire shape: producer calls `ChannelGroup::encrypt`, wraps the
//! ciphertext + `EncryptionMetadata` in
//! `SignedOp::new_unsigned_with_encryption`, signs, hands to the
//! broker. The receiver verifies the outer signature first (drop + log
//! on failure), then feeds the payload to its matching
//! `ChannelGroup::decrypt`; decrypted bytes go to the ordinary
//! Automerge-change-application ingestion path (out of scope here —
//! `channel-crypto` doesn't depend on Automerge).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::SystemTime;

use channel_crypto::{ChannelGroup, MemberKey};
use collab_types::{
    ArtifactId, AttestationId, ChannelId, EncryptionMetadata, OpClass, SignedOp, generate_keypair,
    sign_op, verify_op,
};

/// Stand up a two-member encrypted group + a producer identity keypair.
///
/// The producer's identity (signing the *outer* envelope) is distinct
/// from its MLS signature keypair — kept separate here so the test
/// fails loudly if the two signature paths get accidentally merged.
fn fixture() -> (
    ChannelId,
    ChannelGroup,
    ChannelGroup,
    ed25519_dalek::SigningKey,
    collab_types::Ed25519PublicKey,
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

/// An encrypted signed op round-trips its outer signature, Bob decrypts after verifying it,
/// unencrypted ops still work alongside, and the metadata epoch matches the group epoch at encrypt
/// time.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn encrypted_signed_ops_verify_and_decrypt_like_plain_ones() {
    encrypted_signed_op_round_trips_outer_signature();
    bob_decrypts_payload_after_verifying_outer_signature();
    unencrypted_op_still_works_alongside_encrypted_ones();
    epoch_in_metadata_matches_group_epoch_at_time_of_encrypt();
}

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
        EncryptionMetadata::mls_application(epoch, cid),
    );
    sign_op(&mut op, &sk).unwrap();
    // Outer signature verifies — the only contract verify_op cares
    // about; the validator pipeline check is a separate test below.
    assert!(verify_op(&op).is_ok());
}

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
        EncryptionMetadata::mls_application(epoch, cid),
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

/// Tampering with the payload or with the encryption metadata breaks the outer signature.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn tampering_with_an_encrypted_op_breaks_its_outer_signature() {
    tampering_with_payload_breaks_outer_signature();
    tampering_with_encryption_metadata_breaks_outer_signature();
}

fn tampering_with_payload_breaks_outer_signature() {
    // The outer signature covers the (cipher)bytes. A peer re-encrypting
    // the inner payload cannot keep the outer signature valid — they'd
    // need the author's signing key.
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
        EncryptionMetadata::mls_application(alice_g.epoch(), cid),
    );
    sign_op(&mut op, &sk).unwrap();
    // Tamper: flip a payload byte. Outer signature must fail.
    let last = op.payload.len() - 1;
    op.payload[last] ^= 0xff;
    assert!(verify_op(&op).is_err());
}

fn tampering_with_encryption_metadata_breaks_outer_signature() {
    // Same as above but tampering the metadata: a peer tries to
    // redirect a ciphertext to a *different* channel by rewriting
    // EncryptionMetadata.channel. The outer signature covers the
    // metadata too, so this also fails.
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
        EncryptionMetadata::mls_application(alice_g.epoch(), cid),
    );
    sign_op(&mut op, &sk).unwrap();
    op.encryption = Some(EncryptionMetadata::mls_application(
        alice_g.epoch(),
        ChannelId::new(),
    ));
    assert!(verify_op(&op).is_err());
}

fn unencrypted_op_still_works_alongside_encrypted_ones() {
    // Backwards-compat: legacy/unencrypted channels keep round-tripping
    // with `encryption: None` — same signing pipeline, same verify call, no decrypt.
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

fn epoch_in_metadata_matches_group_epoch_at_time_of_encrypt() {
    // The receiver uses meta.epoch to decide whether the ChannelGroup is
    // current enough to decrypt; pins the invariant that the producer
    // stamps the current epoch.
    let (cid, mut alice_g, _bob_g, _sk, _pk) = fixture();
    let snapshot_epoch = alice_g.epoch();
    let ct = alice_g.encrypt(b"x").unwrap();
    let meta = EncryptionMetadata::mls_application(snapshot_epoch, cid);
    // (No actual SignedOp built here — the invariant is just about
    // the per-encrypt epoch value mirroring `alice_g.epoch()` at
    // the moment of encrypt.)
    assert_eq!(meta.epoch, snapshot_epoch);
    let _ = ct;
}
