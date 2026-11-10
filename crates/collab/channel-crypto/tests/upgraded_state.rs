//! Channel state an earlier build saved loads in this build with every key it
//! held, and the device goes on as a member: a host is often the only
//! always-online holder of a workspace's keys, so an upgrade must keep them.
//!
//! The fixture is state exactly as the build before the format change saved
//! it: three devices took a channel through epochs 0 to 2, content was sealed
//! at each, and the first and third devices' peer states were exported.

use std::collections::BTreeSet;
use std::sync::Arc;

use channel_crypto::{Access, ChannelPeer, PeerStates, VaultKey};
use collab_types::{
    ArtifactId, ChannelId, ContentRetention, Ed25519PublicKey, EncryptionMetadata,
    EncryptionScheme, MlsGovernanceFrame,
};
use ed25519_dalek::SigningKey;

#[derive(serde::Deserialize)]
struct Fixture {
    channel: ChannelId,
    /// Each device's seed and its peer state as the earlier build saved it.
    states: Vec<(u8, Vec<u8>)>,
    /// Content sealed at each epoch: text, artifact, content-key epoch and
    /// ciphertext.
    sealed: Vec<(String, ArtifactId, u64, Vec<u8>)>,
}

fn fixture() -> Fixture {
    let hex = include_str!("fixtures/peer-state-v2.postcard.hex");
    postcard::from_bytes(&hex::decode(hex.trim()).unwrap()).unwrap()
}

fn vault() -> VaultKey {
    VaultKey::new([7; 32])
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn public(seed: u8) -> Ed25519PublicKey {
    Ed25519PublicKey::from_bytes(key(seed).verifying_key().to_bytes())
}

/// The device `seed` of the fixture, opened from its saved state kept in a
/// host's channels directory at `dir`.
fn open(fixture: &Fixture, seed: u8, dir: &std::path::Path) -> ChannelPeer {
    let states: Arc<dyn PeerStates> = Arc::new(channel_crypto::Files::new(dir));
    ChannelPeer::open(Some(states), fixture.channel, Box::new(key(seed)), &vault())
        .unwrap()
        .unwrap()
}

#[test]
fn state_an_earlier_build_saved_keeps_every_key_once_upgraded() {
    let fixture = fixture();
    assert_eq!(fixture.states.len(), 2);
    for (seed, saved) in &fixture.states {
        let dir = tempfile::tempdir().unwrap();
        channel_crypto::Files::new(dir.path())
            .write(fixture.channel, saved)
            .unwrap();
        let peer = open(&fixture, *seed, dir.path());
        assert_eq!(peer.upgraded_from(), Some(2), "device {seed} was upgraded");
        assert_eq!(peer.access(), Access::Ready { epoch: 2 });
        let keys = peer.content_keys().unwrap();
        for (text, artifact, epoch, ciphertext) in &fixture.sealed {
            let metadata = EncryptionMetadata {
                epoch: *epoch,
                channel: fixture.channel,
                scheme: EncryptionScheme::ContentKey {
                    retention: ContentRetention::Delta,
                },
            };
            assert_eq!(
                keys.open_content(*artifact, &metadata, ciphertext)
                    .unwrap()
                    .as_slice(),
                text.as_bytes(),
                "device {seed} opens what was sealed at epoch {epoch}"
            );
        }
        drop(peer);
        let again = open(&fixture, *seed, dir.path());
        assert_eq!(
            again.upgraded_from(),
            None,
            "the upgraded state was kept in the current format"
        );
        assert_eq!(again.access(), Access::Ready { epoch: 2 });
    }
}

#[test]
fn devices_upgraded_from_an_earlier_build_go_on_as_members() {
    let fixture = fixture();
    let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let mut devices: Vec<(u8, ChannelPeer)> = fixture
        .states
        .iter()
        .zip(&dirs)
        .map(|((seed, saved), dir)| {
            channel_crypto::Files::new(dir.path())
                .write(fixture.channel, saved)
                .unwrap();
            (*seed, open(&fixture, *seed, dir.path()))
        })
        .collect();
    let [(_, first), (_, third)] = devices.as_mut_slice() else {
        panic!("two devices");
    };

    // The first device admits a new one, and the third follows the commit.
    let mut newcomer =
        ChannelPeer::waiting(None, fixture.channel, Box::new(key(4)), &vault()).unwrap();
    let request = newcomer
        .access_request_now(1, false, &vault())
        .unwrap()
        .unwrap();
    first.learn_request(request, &vault()).unwrap();
    let members: BTreeSet<Ed25519PublicKey> = [1, 2, 3, 4].into_iter().map(public).collect();
    assert!(first.reconcile_now(&members, &vault()).unwrap().changed());
    let frames: Vec<MlsGovernanceFrame> = first.outbox().to_vec();
    first.published(&vault()).unwrap();
    // As a host fans a commit out: the copy carrying join material reaches
    // only the devices it adds.
    for frame in &frames {
        if frame.join_material().is_some() {
            if frame.added().contains(&public(4)) {
                newcomer.receive(frame.clone(), &members, &vault()).unwrap();
            }
        } else {
            third.receive(frame.clone(), &members, &vault()).unwrap();
        }
    }
    assert_eq!(first.access(), Access::Ready { epoch: 3 });
    assert_eq!(third.access(), Access::Ready { epoch: 3 });
    assert_eq!(newcomer.access(), Access::Ready { epoch: 3 });

    // What one seals now, the others open, and the newcomer reads what was
    // sealed before the upgrade through the key history.
    let artifact = ArtifactId::new();
    let (metadata, ciphertext) = third
        .content_keys()
        .unwrap()
        .seal_content(artifact, ContentRetention::Delta, b"after the upgrade")
        .unwrap();
    for reader in [&*first, &newcomer] {
        assert_eq!(
            reader
                .content_keys()
                .unwrap()
                .open_content(artifact, &metadata, &ciphertext)
                .unwrap()
                .as_slice(),
            b"after the upgrade"
        );
    }
    let (text, artifact, epoch, ciphertext) = &fixture.sealed[0];
    let metadata = EncryptionMetadata {
        epoch: *epoch,
        channel: fixture.channel,
        scheme: EncryptionScheme::ContentKey {
            retention: ContentRetention::Delta,
        },
    };
    assert_eq!(
        newcomer
            .content_keys()
            .unwrap()
            .open_content(*artifact, &metadata, ciphertext)
            .unwrap()
            .as_slice(),
        text.as_bytes()
    );
}
