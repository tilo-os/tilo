//! What a member that takes work answers, and where it keeps what it runs on.
#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;

#[test]
fn the_relay_is_named_by_its_device_key_and_where_it_is_reached() {
    let device = hex::encode([0x11; 32]);
    let relay = Relay::new(&device, "127.0.0.1:7703").unwrap();
    assert_eq!(relay.device(), [0x11; 32]);
    assert_eq!(relay.address(), "127.0.0.1:7703");
    let address = relay.endpoint_addr().unwrap();
    assert_eq!(address.id.as_bytes(), &[0x11; 32]);
    assert!(
        address.ip_addrs().any(|socket| socket.port() == 7703),
        "the relay is reached where its address says"
    );
}

#[test]
fn a_relay_named_by_something_that_is_no_device_key_is_refused() {
    assert!(Relay::new("not hex", "relay.example:7703").is_err());
    assert!(
        Relay::new(&hex::encode([0x11; 16]), "relay.example:7703").is_err(),
        "half a key names no device"
    );
    assert!(Relay::new(&hex::encode([0x11; 32]), " ").is_err());
}

#[test]
fn a_member_is_configured_by_the_relay_it_serves() {
    let device = hex::encode([0x22; 32]);
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove(RELAY_ENV);
    assert_eq!(
        Relay::from_env().unwrap(),
        None,
        "a profile that is no member takes no work"
    );
    env.set(RELAY_ENV, "relay.example:7703");
    env.remove(RELAY_DEVICE_ENV);
    assert!(
        Relay::from_env().is_err(),
        "the relay's address alone does not say which device it is"
    );
    env.set(RELAY_DEVICE_ENV, &device);
    env.remove(RELAY_ENDPOINT_ENV);
    let relay = Relay::from_env().unwrap().unwrap();
    assert_eq!(relay.device(), [0x22; 32]);
    assert_eq!(relay.address(), "relay.example:7703");
    env.set(RELAY_ENV, "127.0.0.1:7704");
    let endpoint = public(3);
    env.set(RELAY_ENDPOINT_ENV, hex::encode(endpoint));
    let relay = Relay::from_env().unwrap().unwrap();
    assert_eq!(
        relay.device(),
        [0x22; 32],
        "requests are still the device's"
    );
    assert_eq!(relay.endpoint_addr().unwrap().id.as_bytes(), &endpoint);
    env.set(RELAY_ENDPOINT_ENV, "not hex");
    assert!(Relay::from_env().is_err(), "an endpoint id that is no key");
}

/// An endpoint id: the public half of a key seeded `seed`.
fn public(seed: u8) -> [u8; 32] {
    SigningKey::from_bytes(&[seed; 32])
        .verifying_key()
        .to_bytes()
}

#[test]
fn a_tilo_relay_is_dialed_at_its_endpoint_and_its_requests_are_its_devices() {
    // A Tilo relay keys its endpoint apart from its device: the member dials
    // the endpoint, and takes requests signed by the device.
    let relay = Relay::new(&hex::encode([0x11; 32]), "127.0.0.1:7704")
        .unwrap()
        .with_endpoint(&hex::encode(public(4)))
        .unwrap();
    assert_eq!(relay.device(), [0x11; 32]);
    let address = relay.endpoint_addr().unwrap();
    assert_eq!(address.id.as_bytes(), &public(4));
    assert!(address.ip_addrs().any(|socket| socket.port() == 7704));
}

#[test]
fn each_workspaces_files_are_a_directory_of_their_own_under_the_profile() {
    let profile = Path::new("/var/lib/tilo/runtime");
    let workspace = "1f8f5b1c-6f2a-4c53-9c1a-2f0b3b0d4e11";
    let held = files(profile, workspace);
    assert_eq!(held, profile.join("state/take-work/files").join(workspace));
    assert!(
        held.starts_with(profile),
        "a member writes nothing outside its profile"
    );
    assert_eq!(
        files(profile, "../../etc"),
        profile.join("state/take-work/files").join("______etc"),
        "a name that is no workspace stays one directory"
    );
}

/// A profile takes work when its relay is named, and an admission session,
/// which is a session of an existing owner's device, never does.
#[test]
fn only_a_profile_whose_relay_is_named_takes_work() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove(RELAY_ENV);
    env.remove(RELAY_DEVICE_ENV);
    assert!(
        crate::standalone::taking_work(crate::standalone::Serving::Profile)
            .unwrap()
            .is_none()
    );
    env.set(RELAY_ENV, "127.0.0.1:7703");
    assert!(
        crate::standalone::taking_work(crate::standalone::Serving::Profile).is_err(),
        "a relay is named, so its device key must be"
    );
    env.set(RELAY_DEVICE_ENV, hex::encode([0x33; 32]));
    assert!(
        crate::standalone::taking_work(crate::standalone::Serving::Profile)
            .unwrap()
            .is_some()
    );
    assert!(
        crate::standalone::taking_work(crate::standalone::Serving::Admission { owner_uid: 1000 })
            .unwrap()
            .is_none(),
        "an admission session serves an existing owner, and takes no work"
    );
}

/// The endpoint a member's relay reaches it at: keyed by the member's device
/// key, listening on the port the relay's members file names.
#[test]
fn a_members_endpoint_listens_where_its_relay_reaches_it() {
    let key = ed25519_dalek::SigningKey::from_bytes(&[0x44; 32]);
    let endpoint = crate::standalone::listen_endpoint(&key, Some(0)).unwrap();
    assert_eq!(
        endpoint.id().as_bytes(),
        &key.verifying_key().to_bytes(),
        "the endpoint is this device"
    );
    assert_eq!(
        endpoint
            .bound_sockets()
            .into_iter()
            .filter(std::net::SocketAddr::is_ipv4)
            .count(),
        1,
        "a member listens on one IPv4 socket, on the port it was given"
    );
}
