//! Two-member end-to-end: alice creates a group, invites bob via
//! Welcome, both encrypt+decrypt application messages.
//!
//! The headline test: exercises every step of the minimum-viable
//! surface — create → key-package → `add_member` → `process_welcome` →
//! encrypt → decrypt — without touching the openmls 3+-member surface.
//! A failure here means the crate is fundamentally broken.

use channel_crypto::{ChannelGroup, MemberKey};
use collab_types::ChannelId;

/// Spin up alice + bob + a fresh channel id so every test starts from a
/// clean slate.
fn fixture() -> (ChannelId, MemberKey, MemberKey) {
    (
        ChannelId::new(),
        MemberKey::generate("alice").unwrap(),
        MemberKey::generate("bob").unwrap(),
    )
}

#[test]
fn owner_alone_is_a_one_member_group() {
    let (cid, alice, _bob) = fixture();
    let g = ChannelGroup::create(cid, alice).unwrap();
    assert_eq!(g.member_count(), 1);
    assert_eq!(g.channel_id(), cid);
    // Initial epoch is 0 (openmls convention), mirrored in
    // EncryptionMetadata so receivers can compare equal-to-zero.
    assert_eq!(g.epoch(), 0);
}

#[test]
fn add_member_increments_epoch_and_member_count() {
    let (cid, alice, bob) = fixture();
    let bob_kp = bob.fresh_key_package().unwrap();

    let mut alice_g = ChannelGroup::create(cid, alice).unwrap();
    let pre_epoch = alice_g.epoch();
    let pre_count = alice_g.member_count();

    let _welcome = alice_g.add_member(bob_kp).unwrap();

    assert_eq!(alice_g.epoch(), pre_epoch + 1);
    assert_eq!(alice_g.member_count(), pre_count + 1);
}

#[test]
fn bob_can_join_via_welcome_alice_minted() {
    let (cid, alice, bob) = fixture();
    let bob_kp = bob.fresh_key_package().unwrap();

    let mut alice_g = ChannelGroup::create(cid, alice).unwrap();
    let welcome = alice_g.add_member(bob_kp).unwrap();

    let bob_g = ChannelGroup::process_welcome(cid, &welcome, bob).unwrap();
    assert_eq!(bob_g.channel_id(), cid);
    assert_eq!(bob_g.member_count(), 2);
    // After processing the Welcome, bob is at the same epoch alice
    // advanced to.
    assert_eq!(bob_g.epoch(), alice_g.epoch());
}

#[test]
fn alice_encrypts_bob_decrypts() {
    let (cid, alice, bob) = fixture();
    let bob_kp = bob.fresh_key_package().unwrap();

    let mut alice_g = ChannelGroup::create(cid, alice).unwrap();
    let welcome = alice_g.add_member(bob_kp).unwrap();
    let mut bob_g = ChannelGroup::process_welcome(cid, &welcome, bob).unwrap();

    let ct = alice_g.encrypt(b"hello bob").unwrap();
    let pt = bob_g.decrypt(&ct).unwrap();
    assert_eq!(pt, b"hello bob");
}

#[test]
fn bob_encrypts_alice_decrypts() {
    // Direction-reverse of the previous test — proves the group key
    // schedule is symmetric.
    let (cid, alice, bob) = fixture();
    let bob_kp = bob.fresh_key_package().unwrap();

    let mut alice_g = ChannelGroup::create(cid, alice).unwrap();
    let welcome = alice_g.add_member(bob_kp).unwrap();
    let mut bob_g = ChannelGroup::process_welcome(cid, &welcome, bob).unwrap();

    let ct = bob_g.encrypt(b"hello alice").unwrap();
    let pt = alice_g.decrypt(&ct).unwrap();
    assert_eq!(pt, b"hello alice");
}

#[test]
fn multiple_messages_round_trip_in_sequence() {
    // Sanity check that the key schedule advances correctly across
    // many messages within one epoch.
    let (cid, alice, bob) = fixture();
    let bob_kp = bob.fresh_key_package().unwrap();
    let mut alice_g = ChannelGroup::create(cid, alice).unwrap();
    let welcome = alice_g.add_member(bob_kp).unwrap();
    let mut bob_g = ChannelGroup::process_welcome(cid, &welcome, bob).unwrap();

    for i in 0..5u32 {
        let msg = format!("alice-{i}");
        let ct = alice_g.encrypt(msg.as_bytes()).unwrap();
        let pt = bob_g.decrypt(&ct).unwrap();
        assert_eq!(pt, msg.as_bytes());
    }
    for i in 0..5u32 {
        let msg = format!("bob-{i}");
        let ct = bob_g.encrypt(msg.as_bytes()).unwrap();
        let pt = alice_g.decrypt(&ct).unwrap();
        assert_eq!(pt, msg.as_bytes());
    }
}

#[test]
fn channel_id_is_preserved_across_create_and_join() {
    let cid = ChannelId::new();
    let alice = MemberKey::generate("alice").unwrap();
    let bob = MemberKey::generate("bob").unwrap();
    let bob_kp = bob.fresh_key_package().unwrap();

    let mut alice_g = ChannelGroup::create(cid, alice).unwrap();
    let welcome = alice_g.add_member(bob_kp).unwrap();
    let bob_g = ChannelGroup::process_welcome(cid, &welcome, bob).unwrap();
    assert_eq!(alice_g.channel_id(), cid);
    assert_eq!(bob_g.channel_id(), cid);
}

#[test]
fn fresh_key_packages_are_distinct() {
    // MLS rejects a re-used KeyPackage: two back-to-back
    // `fresh_key_package` calls must produce structurally distinct
    // artefacts (different HPKE init keys).
    let alice = MemberKey::generate("alice").unwrap();
    let kp1 = alice.fresh_key_package().unwrap();
    let kp2 = alice.fresh_key_package().unwrap();
    assert_ne!(
        kp1.hpke_init_key().as_slice(),
        kp2.hpke_init_key().as_slice(),
        "two fresh key packages must have distinct HPKE init keys"
    );
}

#[test]
fn member_key_does_not_expose_secret_in_debug() {
    // Defence-in-depth: a developer accidentally `{:?}`-printing a
    // MemberKey must not leak signature secret bytes into logs.
    let alice = MemberKey::generate("alice-secret-name").unwrap();
    let dbg = format!("{alice:?}");
    assert!(dbg.contains("alice-secret-name"));
    // Crude guard: signing-key private bytes shouldn't be in the debug
    // output. No direct API access to them, so assert the output is
    // short — a full SignatureKeyPair debug would dump 32 secret bytes.
    assert!(
        dbg.len() < 200,
        "debug output unexpectedly long; possibly leaking secret material: {dbg}"
    );
}
