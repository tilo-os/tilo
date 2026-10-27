//! A device of the person that proves its root-signed certificate is
//! trusted and dialed by every other device of theirs, and kept in the
//! fleet; a revocation from the root wins over it; another person's device
//! is not admitted this way.

use super::tests::personal_continuity_network_for_test;
use super::{NeighbourMsg, ShellNetwork, personal_continuity_topic_id, personal_device_peers};
use crate::device_fleet::DeviceFleetStore;
use ed25519_dalek::SigningKey;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, IdentityPaths, RevocationEntry, RevocationReason,
    sign_device_cert, sign_revocation_entry,
};

fn cert(
    root: &SigningKey,
    device: &iroh::SecretKey,
    capabilities: DeviceCapabilities,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: *device.public().as_bytes(),
        device_label: "Personal device".into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

fn revocation(root: &SigningKey, device: &iroh::SecretKey) -> RevocationEntry {
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: *device.public().as_bytes(),
        revoked_at_ns: 1,
        reason: RevocationReason::Lost,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, root).unwrap();
    entry
}

/// `device` says hello on the person's continuity topic, presenting `cert`
/// and carrying `revocations`.
fn hello(
    net: &ShellNetwork,
    person: &[u8; 32],
    device: &iroh::SecretKey,
    cert: DeviceCert,
    revocations: Vec<RevocationEntry>,
) {
    net.dispatch_neighbour_msg(
        personal_continuity_topic_id(person),
        device.public(),
        Some(crate::network::actor::MembershipEpoch::for_test(1)),
        false,
        NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert),
            revocations,
            successions: Vec::new(),
        },
    );
}

fn own_devices(net: &ShellNetwork) -> Vec<[u8; 32]> {
    net.recheck_personal_device_peers();
    net.enrolled_personal_devices(None)
        .iter()
        .map(|addr| *addr.id.as_bytes())
        .collect()
}

/// A device of `root`'s person with its fleet kept under `dir`.
fn device_of(root: &SigningKey, dir: &std::path::Path) -> (ShellNetwork, DeviceFleetStore) {
    let person = root.verifying_key().to_bytes();
    let (mut net, _commands) = personal_continuity_network_for_test(person);
    let store = DeviceFleetStore::at(dir.join("identity"), IdentityPaths::under(dir));
    net.own_device_fleet = Some(store.clone());
    (net, store)
}

/// What a session started later over the same files counts as the person's.
fn after_restart(
    person: [u8; 32],
    store: &DeviceFleetStore,
    dir: &std::path::Path,
) -> Vec<[u8; 32]> {
    crate::shell::durable_writer::global().flush();
    let mut fresh = personal_device_peers::PersonalDevicePeers::default();
    fresh
        .devices_at(|| personal_device_peers::Sources {
            peers: dir.join("personal-device-peers.json"),
            fleet: Some((person, store.clone())),
        })
        .to_vec()
}

#[test]
#[serial_test::serial(process_env)]
fn a_device_enrolled_elsewhere_is_trusted_dialed_and_kept_once_it_proves_its_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_HOME_PEERS_PATH", dir.path().join("home-peers.json"));
    let root = SigningKey::from_bytes(&[0x51; 32]);
    let person = root.verifying_key().to_bytes();
    let (net, store) = device_of(&root, dir.path());
    // Enrolled by another of the person's devices, after this one: no
    // enrolment list here names it.
    let newer = iroh::SecretKey::from_bytes(&[0x52; 32]);
    assert!(own_devices(&net).is_empty());

    hello(
        &net,
        &person,
        &newer,
        cert(&root, &newer, DeviceCapabilities::member()),
        Vec::new(),
    );
    assert_eq!(own_devices(&net), vec![*newer.public().as_bytes()]);
    assert_eq!(
        after_restart(person, &store, dir.path()),
        vec![*newer.public().as_bytes()],
        "the fleet keeps it for the next session"
    );
}

#[test]
#[serial_test::serial(process_env)]
fn another_persons_device_and_the_persons_hosts_are_not_admitted_by_a_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_HOME_PEERS_PATH", dir.path().join("home-peers.json"));
    let root = SigningKey::from_bytes(&[0x51; 32]);
    let person = root.verifying_key().to_bytes();
    let (net, store) = device_of(&root, dir.path());

    let stranger_root = SigningKey::from_bytes(&[0x61; 32]);
    let stranger = iroh::SecretKey::from_bytes(&[0x62; 32]);
    hello(
        &net,
        &person,
        &stranger,
        cert(&stranger_root, &stranger, DeviceCapabilities::primary()),
        Vec::new(),
    );
    // A host the person runs is reached as a home peer, not a direct one.
    let host = iroh::SecretKey::from_bytes(&[0x53; 32]);
    hello(
        &net,
        &person,
        &host,
        cert(&root, &host, DeviceCapabilities::host()),
        Vec::new(),
    );
    assert!(own_devices(&net).is_empty());
    assert!(after_restart(person, &store, dir.path()).is_empty());
}

#[test]
#[serial_test::serial(process_env)]
fn a_revocation_from_the_root_wins_over_a_kept_device_now_and_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_HOME_PEERS_PATH", dir.path().join("home-peers.json"));
    let root = SigningKey::from_bytes(&[0x51; 32]);
    let person = root.verifying_key().to_bytes();
    let (net, store) = device_of(&root, dir.path());
    let lost = iroh::SecretKey::from_bytes(&[0x52; 32]);
    let other = iroh::SecretKey::from_bytes(&[0x54; 32]);
    // Enrolled here too: revocation wins over enrolment as well.
    std::fs::write(
        dir.path().join("personal-device-peers.json"),
        serde_json::to_vec(&[hex::encode(lost.public().as_bytes())]).unwrap(),
    )
    .unwrap();
    hello(
        &net,
        &person,
        &lost,
        cert(&root, &lost, DeviceCapabilities::member()),
        Vec::new(),
    );
    assert_eq!(own_devices(&net), vec![*lost.public().as_bytes()]);

    // Another device of the person brings the root's revocation of it.
    hello(
        &net,
        &person,
        &other,
        cert(&root, &other, DeviceCapabilities::member()),
        vec![revocation(&root, &lost)],
    );
    assert_eq!(own_devices(&net), vec![*other.public().as_bytes()]);
    assert_eq!(
        after_restart(person, &store, dir.path()),
        vec![*other.public().as_bytes()],
        "the fleet keeps the revocation beside the revoked device's certificate"
    );
    // The revoked device proving its certificate again does not bring it back.
    hello(
        &net,
        &person,
        &lost,
        cert(&root, &lost, DeviceCapabilities::member()),
        Vec::new(),
    );
    assert_eq!(own_devices(&net), vec![*other.public().as_bytes()]);
}
