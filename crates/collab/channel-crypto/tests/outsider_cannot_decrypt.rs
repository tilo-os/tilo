//! Confidentiality: a party who is not a member of the group cannot
//! read its ciphertexts.
//!
//! The core promise is "only group members can decrypt"; this file
//! pins that from several angles — a fresh standalone `ChannelGroup`, a
//! different-channel group, and a tampered ciphertext.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use channel_crypto::{ChannelGroup, MemberKey};
use collab_types::ChannelId;

fn alice_bob_group() -> (ChannelId, ChannelGroup, ChannelGroup) {
    let cid = ChannelId::new();
    let alice = MemberKey::generate("alice").unwrap();
    let bob = MemberKey::generate("bob").unwrap();
    let bob_kp = bob.fresh_key_package().unwrap();
    let mut alice_g = ChannelGroup::create(cid, alice).unwrap();
    let welcome = alice_g.add_member(bob_kp).unwrap();
    let bob_g = ChannelGroup::process_welcome(cid, &welcome, bob).unwrap();
    (cid, alice_g, bob_g)
}

/// An outsider in a brand-new group, or one who never received a welcome for the same channel
/// id, cannot decrypt.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn an_outsider_without_a_welcome_cannot_decrypt() {
    outsider_in_a_brand_new_group_cannot_decrypt();
    outsider_who_never_received_a_welcome_cannot_decrypt_from_same_channel_id();
}

fn outsider_in_a_brand_new_group_cannot_decrypt() {
    // Dave never joins. He has his own ChannelGroup (different identity,
    // different MLS group state) and tries to decrypt alice's ciphertext
    // — openmls rejects (no key schedule for the sender's group).
    let (_cid, mut alice_g, _bob_g) = alice_bob_group();
    let ct = alice_g.encrypt(b"members only").unwrap();

    let dave_cid = ChannelId::new();
    let dave = MemberKey::generate("dave").unwrap();
    let mut dave_g = ChannelGroup::create(dave_cid, dave).unwrap();

    let err = dave_g.decrypt(&ct);
    assert!(
        err.is_err(),
        "dave's fresh group must reject alice's ciphertext"
    );
}

fn outsider_who_never_received_a_welcome_cannot_decrypt_from_same_channel_id() {
    // Subtle case: the *channel id* in EncryptionMetadata is not the
    // security boundary, the MLS group is. Even if dave creates a fresh
    // ChannelGroup with the *same* ChannelId as alice's, his MLS group
    // state has no overlap with hers, so decryption fails.
    let (cid, mut alice_g, _bob_g) = alice_bob_group();
    let ct = alice_g.encrypt(b"colliding-channel-id payload").unwrap();

    let dave = MemberKey::generate("dave").unwrap();
    let mut dave_g = ChannelGroup::create(cid, dave).unwrap();
    let err = dave_g.decrypt(&ct);
    assert!(
        err.is_err(),
        "matching channel_id must not bypass MLS membership"
    );
}

/// Tampered, truncated and garbage ciphertext are all rejected without panicking.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn malformed_ciphertext_is_rejected_gracefully() {
    tampered_ciphertext_is_rejected();
    truncated_ciphertext_is_rejected();
    garbage_bytes_decrypt_fails_gracefully();
}

fn tampered_ciphertext_is_rejected() {
    // Flip a byte in the ciphertext — the AEAD tag check fails inside
    // openmls, giving Error::OpenMls rather than a successful decrypt to garbage.
    let (_cid, mut alice_g, mut bob_g) = alice_bob_group();
    let mut ct = alice_g.encrypt(b"untampered").unwrap();
    // Flip a byte deep enough to be inside the AEAD-protected region.
    let last = ct.len() - 4;
    ct[last] ^= 0xff;
    let err = bob_g.decrypt(&ct);
    assert!(err.is_err(), "tampered ciphertext must be rejected");
}

fn truncated_ciphertext_is_rejected() {
    // A 1-byte truncation breaks the TLS-codec framing; the error
    // surfaces as Error::TlsCodec or Error::OpenMls depending on where
    // it lands. Either way: not Ok.
    let (_cid, mut alice_g, mut bob_g) = alice_bob_group();
    let mut ct = alice_g.encrypt(b"untruncated").unwrap();
    ct.pop();
    let err = bob_g.decrypt(&ct);
    assert!(err.is_err(), "truncated ciphertext must be rejected");
}

fn garbage_bytes_decrypt_fails_gracefully() {
    // 32 bytes of nonsense — should error, not panic.
    let (_cid, _alice_g, mut bob_g) = alice_bob_group();
    let garbage = vec![0xa5u8; 32];
    let err = bob_g.decrypt(&garbage);
    assert!(err.is_err(), "garbage must fail");
}
