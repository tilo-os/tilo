//! A simulated browser — the page's session code built natively, its device
//! key behind the `DeviceSigner` seam — reaches the user's host over iroh, as
//! a page does, after the HTTPS calls it makes before it has a session
//! (certification, against a local host with a development certificate that
//! it pins by SHA-256, as the page pins it). The root-holding laptop is an
//! iroh peer of the same host.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "support/participant.rs"]
mod participant;
mod support;

// Placing the workspace has the host build its governance in an automerge worker.
tilo_automerge_isolate::test_worker!();

use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_browser_peer::{PeerSnapshot, TransportPhase, certify};
use tilo_user_identity::{
    DeviceCapabilities, RevocationEntry, RevocationReason, UserIdentity, sign_revocation_entry,
};

use support::{approve_one, enrol, next_document, ready, route, start_host, start_peer, wait_for};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario from certification to revocation, in order"
)]
async fn browser_over_https_is_certified_syncs_and_is_revoked() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let user = root.verifying_key().to_bytes();
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _laptop = participant::Participant::start(&host.relay, &root).await;
    let https = host.iroh();

    // 1. Certification over HTTPS; the laptop approves.
    let browser_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let (code_tx, code_rx) = tokio::sync::oneshot::channel::<String>();
    let certifying = tokio::spawn({
        let https = https.clone();
        let key = browser_key.clone();
        async move {
            certify(&https, user, "Browser on Cloud", [0; 16], &key, |code| {
                let _ = code_tx.send(code.to_string());
            })
            .await
        }
    });
    let shown = code_rx.await.unwrap();
    let approved = approve_one(&host, &root, &laptop).await;
    assert_eq!(approved, shown, "the person compares codes");
    let (browser_cert, host_cert) = certifying.await.unwrap().unwrap();
    assert_eq!(
        host_cert.device_public_key,
        host_device.cert.device_public_key
    );
    assert!(!browser_cert.capabilities.can_admit_devices);

    // 2. Sessions: the browser over HTTPS, the laptop over iroh.
    let browser = start_peer(
        https.clone(),
        browser_cert.clone(),
        host_cert.clone(),
        browser_key.clone(),
    );
    support::place(&host.relay, &laptop.cert).await;
    let laptop_peer = start_peer(
        host.iroh(),
        laptop.cert.clone(),
        host_cert,
        laptop.key.clone(),
    );
    browser
        .join_placed(support::WORKSPACE, "Workspace")
        .unwrap();
    laptop_peer
        .join_placed(support::WORKSPACE, "Workspace")
        .unwrap();
    ready(&[&browser, &laptop_peer]).await;

    // 3. Durable both ways: commit over HTTPS, delivery over iroh and back.
    let snapshot = DocumentMutation::Snapshot(b"automerge snapshot".to_vec());
    browser.publish(route(), &snapshot).unwrap();
    let receipt = browser.barrier().unwrap();
    wait_for("the commit to be durable", || async {
        receipt.try_recv().ok()
    })
    .await
    .unwrap();
    let mut laptop_events = Vec::new();
    assert_eq!(
        next_document(&laptop_peer, &mut laptop_events).await,
        snapshot
    );
    let reply = DocumentMutation::Incremental(b"typed on the laptop".to_vec());
    laptop_peer.publish(route(), &reply).unwrap();
    let mut browser_events = Vec::new();
    assert_eq!(next_document(&browser, &mut browser_events).await, reply);
    assert_eq!(
        PeerSnapshot::from_status(&browser.status()).synchronization,
        "acknowledged"
    );

    // 5. Revocation: the host ends the session and refuses the handshake
    // from then on. It closes the handshake without saying why, so the page
    // cannot tell a revoked device from one it no longer admits: after a few
    // refusals in a row it stops in `failed` with `not-admitted`.
    let mut entry = RevocationEntry {
        user_public_key: user,
        revoked_device_public_key: browser_cert.device_public_key,
        revoked_at_ns: 1,
        reason: RevocationReason::Stolen,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &root).unwrap();
    tilo_relay::store::save_revocations(state.path(), &[entry]).unwrap();
    let failed = wait_for("the browser to be refused", || {
        let snapshot = PeerSnapshot::from_status(&browser.status());
        async move { (snapshot.phase == "failed").then_some(snapshot) }
    })
    .await;
    assert_eq!(failed.error_code, Some("not-admitted"));
    assert_eq!(browser.status().phase, TransportPhase::Failed);
    browser.close();
    laptop_peer.close();
    host.relay.shutdown().await;
}
