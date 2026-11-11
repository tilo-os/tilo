//! A person's root revokes the stand-in approver a host holds for them, the
//! way a page can: with the entry its root signed, announced beside the
//! device's own certificate. The host takes the entry from the session that
//! carries it and refuses what the stand-in signs from then on, while every
//! device it certified before stays a device of the person's.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "the host, its sessions and the test's scratch directory live until the test ends"
)]

mod support;

tilo_automerge_isolate::test_worker!();

use ed25519_dalek::SigningKey;
use tilo_relay_client::issue_stand_in;
use tilo_session_client::{Cmd, Outbox};
use tilo_user_identity::{
    CERTIFICATION_SCHEMA_VERSION, CertificationRequest, DeviceCapabilities, RevocationList,
    RevocationReason, UserIdentity, issue_device_cert_as_stand_in,
};

use support::{enrol, place, ready, start_host, start_peer, wait_for};

/// A person whose root lives in one browser tab has a stand-in approver on
/// their host. Once they keep the root with a passkey, the page revokes the
/// stand-in: the host learns it from the page's next greeting, and from then
/// on approves nothing with a key it was told to forget — the devices the
/// stand-in approved while it was trusted stay theirs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario: the stand-in approves, the page revokes it, and the host refuses it after"
)]
async fn a_stand_in_revoked_beside_a_session_refuses_what_it_signs_after() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser = enrol(&root, "Browser in a tab", DeviceCapabilities::member());
    let stand_in_key = SigningKey::from_bytes(&[0x5a; 32]);
    let day = 24 * 60 * 60 * 1_000_000_000;
    let stand_in = issue_stand_in(
        &root,
        stand_in_key.verifying_key().to_bytes(),
        "Stand-in on the host",
        30 * day,
    )
    .await
    .unwrap();
    // A browser the stand-in approved while the root was still in one tab.
    let request = |key: u8, at: u64| {
        let device = SigningKey::from_bytes(&[key; 32]);
        CertificationRequest {
            version: CERTIFICATION_SCHEMA_VERSION,
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Browser".into(),
            requested: DeviceCapabilities::member(),
            nonce: [key; 16],
            invite: [0; 16],
            issued_at_ns: at,
            expires_at_ns: at + 60_000_000_000,
            signature: [0; 64],
        }
        .sign(&device)
        .unwrap()
    };
    let approved_request = request(7, stand_in.issued_at_ns + 1);
    let approved = issue_device_cert_as_stand_in(
        &approved_request,
        &stand_in_key,
        &stand_in,
        &RevocationList::new(),
        approved_request.issued_at_ns,
    )
    .unwrap();

    let state = support::scratch_dir();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    place(&host.relay, &browser.cert).await;
    let peer = start_peer(
        host.iroh(),
        browser.cert.clone(),
        host_device.cert.clone(),
        browser.key.clone(),
    );
    peer.join_placed(support::WORKSPACE, "Workspace").unwrap();
    ready(&[&peer]).await;

    // The person keeps their root with a passkey: the page has the root
    // revoke the stand-in, and the entry reaches the host with the session
    // it already has.
    let revoked_at = stand_in.issued_at_ns + 2;
    let entry = tilo_relay_client::revoke_device(
        &root,
        stand_in.device_public_key,
        revoked_at,
        RevocationReason::Retired,
    )
    .await
    .unwrap();
    peer.send(Cmd::Revoke(entry.clone())).unwrap();
    let learned = wait_for("the host to record the revocation", || {
        let entry = entry.clone();
        let workspaces = host.relay.workspaces().unwrap();
        async move {
            let held = workspaces.state(support::WORKSPACE).await?;
            held.revocations().into_iter().find(|held| held == &entry)
        }
    })
    .await;
    assert_eq!(
        learned.revoked_device_public_key,
        stand_in.device_public_key
    );
    assert_eq!(learned.reason, RevocationReason::Retired);

    // What the host holds now refuses every certificate the stand-in signs
    // from the revocation on, and none it signed before.
    let mut revocations = RevocationList::new();
    for entry in host
        .relay
        .workspaces()
        .unwrap()
        .state(support::WORKSPACE)
        .await
        .unwrap()
        .revocations()
    {
        revocations.insert(entry).unwrap();
    }
    assert!(
        !revocations.refuses(&approved),
        "a device the stand-in approved before the revocation stays a device of the person's"
    );
    // A stand-in that answers anyway, as a host still holding its key would.
    let later_request = request(8, revoked_at);
    let later = issue_device_cert_as_stand_in(
        &later_request,
        &stand_in_key,
        &stand_in,
        &RevocationList::new(),
        revoked_at,
    )
    .unwrap();
    assert!(
        revocations.refuses(&later),
        "the host refuses a device the revoked stand-in signed after the revocation"
    );
    assert!(revocations.refuses(&stand_in), "and the stand-in itself");

    peer.close();
    host.relay.shutdown().await;
}
