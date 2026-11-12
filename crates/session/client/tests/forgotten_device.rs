//! A host that forgets a guest: a host whose state was reset (its identity
//! and address kept) turns the guest away without a word, and after a few
//! refusals in a row the page stops with `not-admitted` for the person to
//! join again.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the test holds its host for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_session_client::TransportPhase;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{HostSettings, enrol, restart_host, start_host_serving, start_peer, wait_for};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reset_host_refuses_its_forgotten_guest() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let (guest_root, _) = UserIdentity::generate("Guest", 1);
    let guest = enrol(&guest_root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host_serving(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        HostSettings {
            guests: &[guest.cert.user_public_key],
            ..HostSettings::default()
        },
    )
    .await;
    let peer = start_peer(
        host.iroh(),
        guest.cert.clone(),
        host_device.cert.clone(),
        guest.key.clone(),
    );
    wait_for("the guest to get in", || {
        let status = peer.status();
        async move { (status.phase == TransportPhase::Connected).then_some(()) }
    })
    .await;

    // The host's state is reset: the same identity and web certificate, its
    // ports the kernel's to pick again, and nothing of the guest.
    drop(host.web);
    host.relay.shutdown().await;
    let reset = tempfile::tempdir().unwrap();
    std::fs::copy(
        state.path().join("web-dev-identity"),
        reset.path().join("web-dev-identity"),
    )
    .unwrap();
    let host = restart_host(&root, &host_device, reset.path()).await;
    let failed = wait_for("the reset host to refuse the guest", || {
        let snapshot = peer.snapshot();
        async move { (snapshot.phase == "failed").then_some(snapshot) }
    })
    .await;
    assert_eq!(failed.error_code, Some("not-admitted"));
    peer.close();
    host.relay.shutdown().await;
}
