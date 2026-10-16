//! Three-member group: alice creates, adds bob, then adds carol.
//!
//! Going past two members exercises the openmls commit-fan-out path:
//! when a third member joins, existing members must process the commit
//! so their epoch advances in lockstep — handled by the
//! [`ChannelGroup::add_member_with_commit`] /
//! [`ChannelGroup::process_commit`] pair.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use channel_crypto::{ChannelGroup, MemberKey};
use collab_types::ChannelId;

/// Build a fully-wired (alice, bob) two-member group + a fresh
/// `carol` `MemberKey` ready to join.
fn two_member_group_plus_carol() -> (ChannelId, ChannelGroup, ChannelGroup, MemberKey) {
    let cid = ChannelId::new();
    let alice = MemberKey::generate("alice").unwrap();
    let bob = MemberKey::generate("bob").unwrap();
    let bob_kp = bob.fresh_key_package().unwrap();

    let mut alice_g = ChannelGroup::create(cid, alice).unwrap();
    let welcome = alice_g.add_member(bob_kp).unwrap();
    let bob_g = ChannelGroup::process_welcome(cid, &welcome, bob).unwrap();
    let carol = MemberKey::generate("carol").unwrap();

    (cid, alice_g, bob_g, carol)
}

/// Carol joins via welcome after a two-member setup, can encrypt for Alice and Bob, Alice can
/// still encrypt, the epoch increments once per add, and a multi-party add cannot drop the commit.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn a_third_member_joins_and_everyone_can_talk() {
    carol_joins_via_welcome_after_two_member_setup();
    carol_can_encrypt_for_alice_and_bob();
    alice_can_encrypt_after_third_member_joins();
    epoch_increments_once_per_add();
    multi_party_add_cannot_drop_the_commit();
}

fn carol_joins_via_welcome_after_two_member_setup() {
    let (cid, mut alice_g, mut bob_g, carol) = two_member_group_plus_carol();
    let carol_kp = carol.fresh_key_package().unwrap();

    let (welcome, commit) = alice_g.add_member_with_commit(carol_kp).unwrap();
    bob_g.process_commit(&commit).unwrap();
    let carol_g = ChannelGroup::process_welcome(cid, &welcome, carol).unwrap();

    assert_eq!(alice_g.member_count(), 3);
    assert_eq!(bob_g.member_count(), 3);
    assert_eq!(carol_g.member_count(), 3);
    // All three share the same epoch.
    assert_eq!(alice_g.epoch(), bob_g.epoch());
    assert_eq!(bob_g.epoch(), carol_g.epoch());
}

fn carol_can_encrypt_for_alice_and_bob() {
    let (cid, mut alice_g, mut bob_g, carol) = two_member_group_plus_carol();
    let carol_kp = carol.fresh_key_package().unwrap();
    let (welcome, commit) = alice_g.add_member_with_commit(carol_kp).unwrap();
    bob_g.process_commit(&commit).unwrap();
    let mut carol_g = ChannelGroup::process_welcome(cid, &welcome, carol).unwrap();

    let ct = carol_g.encrypt(b"hi from carol").unwrap();
    let pt_a = alice_g.decrypt(&ct).unwrap();
    // A given ciphertext can only be decrypted once per receiver
    // (openmls advances its inbound-key-schedule cursor on
    // process_message), so send a second ciphertext to bob.
    let ct2 = carol_g.encrypt(b"hi from carol").unwrap();
    let pt_b = bob_g.decrypt(&ct2).unwrap();
    assert_eq!(pt_a, b"hi from carol");
    assert_eq!(pt_b, b"hi from carol");
}

fn alice_can_encrypt_after_third_member_joins() {
    // Verifies the key schedule advances cleanly: alice keeps her
    // group state usable after the commit-merge.
    let (cid, mut alice_g, mut bob_g, carol) = two_member_group_plus_carol();
    let carol_kp = carol.fresh_key_package().unwrap();
    let (welcome, commit) = alice_g.add_member_with_commit(carol_kp).unwrap();
    bob_g.process_commit(&commit).unwrap();
    let mut carol_g = ChannelGroup::process_welcome(cid, &welcome, carol).unwrap();

    let ct = alice_g.encrypt(b"alice still works").unwrap();
    let pt_b = bob_g.decrypt(&ct).unwrap();
    assert_eq!(pt_b, b"alice still works");

    let ct = alice_g.encrypt(b"alice for carol").unwrap();
    let pt_c = carol_g.decrypt(&ct).unwrap();
    assert_eq!(pt_c, b"alice for carol");
}

/// Bob, who missed the commit, cannot decrypt post-join ciphertexts; a removal commit advances
/// survivors and excludes the removed member.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn members_who_miss_or_are_removed_by_a_commit_cannot_decrypt_after_it() {
    bob_who_missed_the_commit_cannot_decrypt_post_join_ciphertexts();
    removal_commit_advances_survivors_and_excludes_removed_member();
}

fn bob_who_missed_the_commit_cannot_decrypt_post_join_ciphertexts() {
    // The failure mode: if a peer misses the commit (e.g. broker offline
    // at the wrong moment), it stays at the old epoch and reading
    // post-join ciphertexts fails — the *correct* MLS behaviour, pinned
    // here so a future "helpful" auto-catchup doesn't silently weaken it.
    let (_cid, mut alice_g, mut bob_g_stuck, carol) = two_member_group_plus_carol();
    let bob_pre_epoch = bob_g_stuck.epoch();
    let carol_kp = carol.fresh_key_package().unwrap();
    let (_welcome, _commit) = alice_g.add_member_with_commit(carol_kp).unwrap();
    // Deliberately DO NOT feed the commit into bob_g_stuck.
    assert_eq!(bob_g_stuck.epoch(), bob_pre_epoch);
    assert_eq!(alice_g.epoch(), bob_pre_epoch + 1);

    let ct = alice_g.encrypt(b"new-epoch payload").unwrap();
    // bob is one epoch behind; openmls rejects the message.
    let err = bob_g_stuck.decrypt(&ct);
    assert!(
        err.is_err(),
        "stuck bob should not decrypt a future-epoch ciphertext"
    );
}

fn epoch_increments_once_per_add() {
    let (_cid, mut alice_g, _bob_g, carol) = two_member_group_plus_carol();
    let epoch_after_bob = alice_g.epoch();
    let carol_kp = carol.fresh_key_package().unwrap();
    let _ = alice_g.add_member_with_commit(carol_kp).unwrap();
    assert_eq!(alice_g.epoch(), epoch_after_bob + 1);
}

fn multi_party_add_cannot_drop_the_commit() {
    let (_cid, mut alice_g, _bob_g, carol) = two_member_group_plus_carol();
    let error = alice_g
        .add_member(carol.fresh_key_package().unwrap())
        .unwrap_err();
    assert!(matches!(
        error,
        channel_crypto::Error::CommitPropagationRequired { .. }
    ));
    assert_eq!(alice_g.member_count(), 2);
}

fn removal_commit_advances_survivors_and_excludes_removed_member() {
    let (cid, mut alice_g, mut bob_g, carol) = two_member_group_plus_carol();
    let (welcome, add_commit) = alice_g
        .add_member_with_commit(carol.fresh_key_package().unwrap())
        .unwrap();
    bob_g.process_commit(&add_commit).unwrap();
    let mut carol_g = ChannelGroup::process_welcome(cid, &welcome, carol).unwrap();

    // Alice is leaf 0 and Bob is the first admitted member at leaf 1.
    let remove_commit = alice_g.remove_member(1).unwrap();
    carol_g.process_commit(&remove_commit).unwrap();
    bob_g.process_commit(&remove_commit).unwrap();

    assert_eq!(alice_g.member_count(), 2);
    assert_eq!(carol_g.member_count(), 2);
    assert_eq!(alice_g.epoch(), carol_g.epoch());
    assert!(bob_g.encrypt(b"removed member cannot send").is_err());
}
