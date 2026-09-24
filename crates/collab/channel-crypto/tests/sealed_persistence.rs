use channel_crypto::{ChannelGroup, MemberKey, VaultKey};
use collab_types::ChannelId;

#[test]
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

#[test]
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

/// Sealed states written before a crypto-stack upgrade must still open:
/// the file is XChaCha20-Poly1305 under the vault key, so this pins the
/// on-disk format, not just a round trip through the current build.
#[test]
fn sealed_state_fixtures_from_an_earlier_build_still_open() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut alice = ChannelGroup::load_sealed(
        &fixtures.join("sealed_alice_v1.mls"),
        &VaultKey::new([0x42; 32]),
    )
    .unwrap();
    let mut bob = ChannelGroup::load_sealed(
        &fixtures.join("sealed_bob_v1.mls"),
        &VaultKey::new([0x43; 32]),
    )
    .unwrap();
    for group in [&alice, &bob] {
        assert_eq!(
            group.channel_id().0.to_string(),
            "767db0e8-d9f8-4031-a17a-142e2cdab689"
        );
        assert_eq!(group.epoch(), 1);
        assert_eq!(group.member_count(), 2);
    }
    let ciphertext = bob.encrypt(b"fixture ratchet").unwrap();
    assert_eq!(alice.decrypt(&ciphertext).unwrap(), b"fixture ratchet");
    assert!(
        ChannelGroup::load_sealed(
            &fixtures.join("sealed_alice_v1.mls"),
            &VaultKey::new([0x43; 32]),
        )
        .is_err()
    );
}
