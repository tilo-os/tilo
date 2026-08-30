//! Auth-side integration checks:
//!
//! 1. A peer whose cert chains to a different user identity is
//!    rejected during the handshake — `RelayClient::connect`
//!    surfaces the error, no session is opened.
//! 2. A peer whose device was revoked is rejected.
//!
//! Both cases exercise the wire handshake, not just the pure
//! auth-module tests — an implementation bug on the relay side
//! (e.g. skipping the revocation check) would slip past the unit
//! tests but fail here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use qos_relay::RelayClient;
use qos_user_identity::{
    DeviceCapabilities, RevocationEntry, RevocationList, RevocationReason, sign_revocation_entry,
};

use crate::common::{TestUser, client_endpoint, spawn_relay};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_from_different_user_is_rejected() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let alice_relay_dev = alice.enrol("home-server", DeviceCapabilities::host());
    let alice_ws_member = alice.enrol("laptop", DeviceCapabilities::primary());
    let bob_intruder = bob.enrol("stranger-laptop", DeviceCapabilities::primary());

    // Alice's relay, membership Alice's laptop.
    let relay = spawn_relay(
        &alice,
        alice_relay_dev,
        &[1],
        &[alice_ws_member.device_pubkey()],
    )
    .await;
    let relay_addr = relay.relay.node_addr();

    // Bob's device tries to connect. Handshake must fail — the
    // policy is AllowMatchingUser and Bob's cert chains to Bob's
    // root, not Alice's.
    let ep = client_endpoint().await;
    let res = RelayClient::connect(
        ep,
        relay_addr,
        bob.user_pk,
        bob_intruder.cert,
        bob_intruder.sk,
        RevocationList::new(),
    )
    .await;
    assert!(res.is_err(), "cross-user client must be rejected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoked_device_is_rejected() {
    let user = TestUser::fresh("Alice");
    let relay_dev = user.enrol("home-server", DeviceCapabilities::host());
    let good_dev = user.enrol("laptop", DeviceCapabilities::primary());
    let bad_dev = user.enrol("stolen-phone", DeviceCapabilities::primary());

    // Build a relay identity with the stolen-phone device already
    // in the revocation list.
    let mut revs = RevocationList::new();
    let mut entry = RevocationEntry {
        user_public_key: user.user_pk,
        revoked_device_public_key: bad_dev.device_pubkey(),
        revoked_at_ns: 100,
        reason: RevocationReason::Stolen,
        signature: [0u8; 64],
    };
    sign_revocation_entry(&mut entry, &user.user_sk).unwrap();
    revs.insert(entry).unwrap();

    // Custom spawn to inject the revocation list into the relay.
    let tmp = tempfile::tempdir().unwrap();
    let state_root = tmp.path().to_path_buf();
    let cfg = qos_relay::RelayConfig {
        state_root: state_root.clone(),
        ..Default::default()
    };
    let identity = qos_relay::daemon::RelayIdentity {
        user_public_key: user.user_pk,
        device_signing_key: relay_dev.sk.clone(),
        device_cert: relay_dev.cert.clone(),
        revocations: revs,
    };
    let relay = qos_relay::daemon::Relay::start(cfg, identity)
        .await
        .unwrap();
    relay
        .set_workspace_membership(9, true, vec![good_dev.device_pubkey()], state_root)
        .await
        .unwrap();
    let addr = relay.node_addr();

    // The revoked device tries to connect — must fail.
    let ep = client_endpoint().await;
    let res = RelayClient::connect(
        ep,
        addr.clone(),
        user.user_pk,
        bad_dev.cert.clone(),
        bad_dev.sk.clone(),
        RevocationList::new(),
    )
    .await;
    assert!(res.is_err(), "revoked device must be rejected");

    // A non-revoked device from the same user still connects fine.
    let ep2 = client_endpoint().await;
    let res_ok = RelayClient::connect(
        ep2,
        addr,
        user.user_pk,
        good_dev.cert,
        good_dev.sk,
        RevocationList::new(),
    )
    .await;
    assert!(res_ok.is_ok(), "good device should still connect");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn newly_persisted_revocation_closes_an_existing_session() {
    let user = TestUser::fresh("Alice");
    let relay_dev = user.enrol("home-server", DeviceCapabilities::host());
    let device = user.enrol("lost-phone", DeviceCapabilities::member());
    let device_key = device.device_pubkey();
    let state = tempfile::tempdir().unwrap();
    let relay = qos_relay::daemon::Relay::start(
        qos_relay::RelayConfig {
            state_root: state.path().to_path_buf(),
            ..Default::default()
        },
        qos_relay::daemon::RelayIdentity {
            user_public_key: user.user_pk,
            device_signing_key: relay_dev.sk,
            device_cert: relay_dev.cert,
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let client = RelayClient::connect(
        client_endpoint().await,
        relay.node_addr(),
        user.user_pk,
        device.cert,
        device.sk,
        RevocationList::new(),
    )
    .await
    .unwrap();
    client
        .send(qos_relay::Envelope::Subscribe { workspace_id: 33 })
        .unwrap();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv())
        .await
        .unwrap();

    let mut entry = RevocationEntry {
        user_public_key: user.user_pk,
        revoked_device_public_key: device_key,
        revoked_at_ns: 1,
        reason: RevocationReason::Lost,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &user.user_sk).unwrap();
    qos_relay::store::save_revocations(state.path(), &[entry]).unwrap();

    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(4), client.recv()).await,
        Ok(None)
    ));
}
