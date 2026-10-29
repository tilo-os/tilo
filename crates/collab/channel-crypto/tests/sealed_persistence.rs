use channel_crypto::{ChannelGroup, MemberKey, VaultKey};
use collab_types::ChannelId;

/// Sealed groups survive restart with ratchet state and reject a wrong key, tampering and
/// truncation.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn sealed_group_state_survives_restarts_and_refuses_tampering() {
    sealed_groups_survive_restart_and_keep_ratchet_state();
    sealed_state_rejects_wrong_key_tampering_and_truncation();
}

fn sealed_groups_survive_restart_and_keep_ratchet_state() {
    let dir = tempfile::tempdir().unwrap();
    let alice_path = dir.path().join("alice.mls");
    let bob_path = dir.path().join("bob.mls");
    let channel = ChannelId::new();
    let alice_member = MemberKey::generate("alice").unwrap();
    let bob_member = MemberKey::generate("bob").unwrap();
    let bob_package = bob_member.fresh_key_package().unwrap();
    let mut alice = ChannelGroup::create(channel, alice_member).unwrap();
    let welcome = alice.add_member(bob_package).unwrap();
    let mut bob = ChannelGroup::process_welcome(channel, &welcome, bob_member).unwrap();

    let before_restart = alice.encrypt(b"persist me").unwrap();
    assert_eq!(bob.decrypt(&before_restart).unwrap(), b"persist me");
    alice
        .save_sealed(&alice_path, &VaultKey::new([7; 32]))
        .unwrap();
    bob.save_sealed(&bob_path, &VaultKey::new([8; 32])).unwrap();
    drop(alice);
    drop(bob);

    let mut alice = ChannelGroup::load_sealed(&alice_path, &VaultKey::new([7; 32])).unwrap();
    let mut bob = ChannelGroup::load_sealed(&bob_path, &VaultKey::new([8; 32])).unwrap();
    let after_restart = bob.encrypt(b"ratchet continues").unwrap();
    assert_eq!(alice.decrypt(&after_restart).unwrap(), b"ratchet continues");
}

fn sealed_state_rejects_wrong_key_tampering_and_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("group.mls");
    let group =
        ChannelGroup::create(ChannelId::new(), MemberKey::generate("alice").unwrap()).unwrap();
    group.save_sealed(&path, &VaultKey::new([3; 32])).unwrap();

    assert!(ChannelGroup::load_sealed(&path, &VaultKey::new([4; 32])).is_err());
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&path, &bytes).unwrap();
    assert!(ChannelGroup::load_sealed(&path, &VaultKey::new([3; 32])).is_err());
    std::fs::write(&path, &bytes[..10]).unwrap();
    assert!(ChannelGroup::load_sealed(&path, &VaultKey::new([3; 32])).is_err());
}
