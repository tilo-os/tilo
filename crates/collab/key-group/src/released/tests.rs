use super::*;

/// The peer state a v0.1.5 relay kept for the Lounge's one channel, as the
/// release's own relay wrote it (`tools/relay-upgrade/v0.1.5`): one per
/// channel, sealed under the vault key that relay derives from its device.
const KEPT: &str = include_str!("../../golden/released-channel-peer/v3.hex");
/// That relay's device key, from the same state: a test key, which seals
/// nothing but this fixture.
const RELAY_DEVICE: &str = "e34a375993373996c1af58575606c80c302788f08f35d1d58be45815f7c1d54b";
/// The channel the release kept that state for.
const CHANNEL: &str = "792bcbb6-9a42-800b-845c-d088a8b6500e";

fn kept() -> Vec<u8> {
    hex::decode(KEPT.split_whitespace().collect::<String>()).unwrap()
}

/// The vault key the relay seals its channel state under.
fn relay_vault() -> VaultKey {
    let device: [u8; 32] = hex::decode(RELAY_DEVICE).unwrap().try_into().unwrap();
    VaultKey::new(blake3::derive_key(
        "tilo-relay governed room channel vault v1",
        &device,
    ))
}

/// A channel's state a release kept gives the channel and every content
/// key it held.
#[test]
fn a_kept_channel_gives_its_keys() {
    let (channel, keys) = released_channel_keys(&kept(), &relay_vault()).unwrap();
    assert_eq!(channel.to_string(), CHANNEL);
    // Its group went through epochs 0 to 3 as people came in, and a
    // key of each seals what was written then.
    let released = crate::content::RELEASED_EPOCHS;
    assert_eq!(
        keys.epoch_range(),
        Some((released, released | 3)),
        "{keys:?}"
    );
    assert_eq!(keys.len(), 4);
}

/// State another vault sealed, or a workspace peer this build sealed, is
/// not a release's channel.
#[test]
fn another_vault_or_this_builds_state_is_refused() {
    {
        assert!(matches!(
            released_channel_keys(&kept(), &VaultKey::new([7; 32])),
            Err(Error::StateCrypto(_))
        ));
        // A workspace's peer state as this build seals it is not a channel's.
        let vault = relay_vault();
        let peer = crate::GroupPeer::waiting(
            None,
            collab_types::WorkspaceId::new(),
            Box::new(ed25519_dalek::SigningKey::from_bytes(&[3; 32])),
            &vault,
        )
        .unwrap();
        assert!(matches!(
            released_channel_keys(&peer.export_sealed(&vault).unwrap(), &vault),
            Err(Error::StateCrypto(_))
        ));
    }
}

/// Device `n`'s signing key.
fn key(n: u8) -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[n; 32])
}

fn devices(ns: &[u8]) -> std::collections::BTreeSet<collab_types::Ed25519PublicKey> {
    ns.iter()
        .map(|n| collab_types::Ed25519PublicKey::from_bytes(key(*n).verifying_key().to_bytes()))
        .collect()
}

/// Device `n` asking `host` for `workspace`.
fn asking(
    host: &mut crate::GroupPeer,
    workspace: collab_types::WorkspaceId,
    n: u8,
    vault: &VaultKey,
) -> crate::GroupPeer {
    let mut peer = crate::GroupPeer::waiting(None, workspace, Box::new(key(n)), vault).unwrap();
    let asked = peer.access_request_now(1, false, vault).unwrap().unwrap();
    host.learn_request(asked).unwrap();
    peer
}

/// What `host` queued, as the device it admits takes it (with its join
/// material, when it carries one), and then published.
fn sent(host: &mut crate::GroupPeer) -> collab_types::MlsGovernanceFrame {
    let outbox = host.outbox();
    let frame = outbox
        .iter()
        .find(|frame| frame.join_material().is_some())
        .unwrap_or_else(|| outbox.last().expect("a frame"))
        .clone();
    host.published();
    frame
}

/// The keys a release kept, adopted by the workspace's group, reach a
/// member admitted before and one admitted after, and never seal: the
/// group's own key seals every epoch it went through.
#[test]
fn members_get_the_keys_a_release_kept() {
    let vault = relay_vault();
    let (_, released) = released_channel_keys(&kept(), &vault).unwrap();
    let workspace = collab_types::WorkspaceId::new();
    let mut host = crate::GroupPeer::create_now(None, workspace, Box::new(key(1)), &vault).unwrap();
    host.published();
    let mut early = asking(&mut host, workspace, 2, &vault);
    let allowed = devices(&[1, 2]);
    host.reconcile_now(&allowed, &vault).unwrap();
    let frame = sent(&mut host);
    early.receive(frame, &allowed, &vault).unwrap();

    assert!(host.adopt(&released, &vault).unwrap());
    assert!(!host.adopt(&released, &vault).unwrap(), "adopted once");
    assert_eq!(host.turn(&allowed), Some(0), "handed over at once");
    assert!(host.reconcile_now(&allowed, &vault).unwrap().history);
    let frame = sent(&mut host);
    early.receive(frame, &allowed, &vault).unwrap();
    assert!(
        !host.adopt(&released, &vault).unwrap(),
        "the group holds them"
    );
    assert!(!host.holds_unshared_keys());
    assert!(released.missing_from(early.keyring().unwrap()).is_empty());

    let mut later = asking(&mut host, workspace, 3, &vault);
    let allowed = devices(&[1, 2, 3]);
    host.reconcile_now(&allowed, &vault).unwrap();
    let frame = sent(&mut host);
    later.receive(frame, &allowed, &vault).unwrap();
    let keyring = later.keyring().expect("admitted");
    assert!(released.missing_from(keyring).is_empty());
    let crate::Access::Ready { epoch } = later.access() else {
        panic!("{:?}", later.access());
    };
    // The group's own epochs are its own: none is a release's.
    for lived in 0..=epoch {
        let own = collab_types::EncryptionMetadata::content_key(
            lived,
            collab_types::ContentRetention::Delta,
        );
        assert!(keyring.holds_key_for(&own), "epoch {lived}");
    }
    assert_eq!(
        keyring.len(),
        usize::try_from(epoch).unwrap() + 1 + released.len()
    );
}
