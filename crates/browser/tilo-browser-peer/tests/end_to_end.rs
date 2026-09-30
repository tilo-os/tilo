//! A simulated browser — the page's session code built natively, its device
//! key behind the `DeviceSigner` seam — reaches the user's host only over the
//! browser carriers: HTTPS to a local host with a development certificate
//! (pinned by SHA-256, as the page pins it) for certification and documents,
//! WebTransport `moq-lite` for live presence. The root-holding laptop is an
//! iroh peer of the same host.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

use std::time::Duration;

use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_browser_peer::{Host, LiveState, PeerEvent, PeerSnapshot, TransportPhase, certify};
use tilo_relay_protocol::live::{LiveFrame, LiveRecord};
use tilo_user_identity::{
    DeviceCapabilities, RevocationEntry, RevocationReason, UserIdentity, sign_revocation_entry,
};

use support::{
    approve_one, enrol, next_document, ready, route, start_host, start_peer, wait_for, workspace,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario from certification to revocation, in order"
)]
async fn browser_over_https_and_webtransport_is_certified_syncs_and_is_revoked() {
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
    let https = Host::Https(host.https());

    // 1. Certification over HTTPS; the laptop approves.
    let browser_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let (code_tx, code_rx) = tokio::sync::oneshot::channel::<String>();
    let certifying = tokio::spawn({
        let https = https.clone();
        let key = browser_key.clone();
        async move {
            certify(&https, user, "Browser on Cloud", &key, |code| {
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
    let laptop_peer = start_peer(
        host.iroh(),
        laptop.cert.clone(),
        host_cert,
        laptop.key.clone(),
    );
    browser.join(workspace()).unwrap();
    laptop_peer.join(workspace()).unwrap();
    laptop_peer.live_join(workspace()).unwrap();
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

    // 4. Live over WebTransport: the host grants the browser session, the
    // browser's moq-lite participant publishes presence, and a laptop frame
    // sent over iroh is bridged into the browser's live room.
    let (grant_tx, grant_rx) = tokio::sync::oneshot::channel();
    browser
        .live_grant_then(
            workspace(),
            Box::new(move |grant| {
                let _ = grant_tx.send(grant);
            }),
        )
        .unwrap();
    let grant = grant_rx.await.unwrap().unwrap();
    assert_eq!(grant.publisher, hex::encode(browser_cert.device_public_key));
    let namespace = tilo_moq::live::namespace_path(&grant.room, &grant.publisher).unwrap();
    let url: url::Url = format!("https://127.0.0.1:{}/moq/live", host.web.addr.port())
        .parse()
        .unwrap();
    let trust =
        tilo_moq::carrier::Trust::Fingerprints(vec![host.web.certificate_sha256.clone().unwrap()]);
    let mut live =
        tilo_moq::live::peer::Peer::connect(&url, &trust, &grant.token, Some(&namespace))
            .await
            .expect("the live grant admits the WebTransport session");
    let mut browser_live = LiveState::starting_now();
    let frame = browser_live
        .publish_presence(1, |presence| {
            presence.focus = Some(LiveRecord {
                app: "so.tilo.notes".into(),
                record: "note-1".into(),
            });
            presence.pointer = Some([12.0, 34.0]);
        })
        .unwrap();
    assert!(
        live.publish_presence(&frame),
        "valid presence is accepted locally"
    );

    // A second browser device (a tablet, also over HTTPS) receives it over
    // its own WebTransport session.
    let tablet = enrol(&root, "tablet", DeviceCapabilities::member());
    let tablet_peer = start_peer(
        https.clone(),
        tablet.cert.clone(),
        host_device.cert.clone(),
        tablet.key.clone(),
    );
    tablet_peer.join(workspace()).unwrap();
    ready(&[&tablet_peer]).await;
    let (grant_tx, grant_rx) = tokio::sync::oneshot::channel();
    tablet_peer
        .live_grant_then(
            workspace(),
            Box::new(move |grant| {
                let _ = grant_tx.send(grant);
            }),
        )
        .unwrap();
    let tablet_grant = grant_rx.await.unwrap().unwrap();
    let tablet_live = tilo_moq::live::peer::Peer::connect(&url, &trust, &tablet_grant.token, None)
        .await
        .expect("the tablet's grant admits it");
    let from_browser = tokio::time::timeout(Duration::from_secs(20), async {
        let broadcast = tablet_live
            .member(&namespace)
            .await
            .expect("browser announced");
        let mut track = broadcast
            .track(tilo_moq::live::PRESENCE_TRACK)
            .unwrap()
            .subscribe(None)
            .await
            .unwrap();
        let mut seq = 2;
        loop {
            let frame = browser_live
                .publish_presence(seq, |presence| presence.pointer = Some([12.0, 34.0]))
                .unwrap();
            seq += 1;
            live.publish_presence(&frame);
            if let Ok(Ok(Some(datagram))) =
                tokio::time::timeout(Duration::from_millis(50), track.recv_datagram()).await
            {
                return datagram.payload.to_vec();
            }
        }
    })
    .await
    .expect("browser presence reaches the tablet over WebTransport");
    assert!(matches!(
        LiveFrame::decode(&from_browser),
        Ok(LiveFrame::Presence(presence)) if presence.pointer == Some([12.0, 34.0])
    ));
    drop(tablet_live);
    tablet_peer.close();

    // The laptop, a live member over iroh, receives the browser's
    // WebTransport presence through the host's bridge.
    laptop_peer.live_join(workspace()).unwrap();
    let laptop_events = laptop_peer.events();
    let bridged = tokio::time::timeout(Duration::from_secs(20), async {
        let mut seq = 1000;
        loop {
            let frame = browser_live
                .publish_presence(seq, |presence| presence.pointer = Some([21.0, 43.0]))
                .unwrap();
            seq += 1;
            live.publish_presence(&frame);
            tokio::time::sleep(Duration::from_millis(50)).await;
            for event in laptop_events.drain() {
                if let PeerEvent::Live { from, frame, .. } = event
                    && from == browser_cert.device_public_key
                {
                    return frame;
                }
            }
        }
    })
    .await
    .expect("browser presence reaches an iroh live member");
    assert!(matches!(
        LiveFrame::decode(&bridged),
        Ok(LiveFrame::Presence(presence)) if presence.pointer.is_some()
    ));

    let laptop_namespace =
        tilo_moq::live::namespace_path(&grant.room, &hex::encode(laptop.cert.device_public_key))
            .unwrap();
    let mut laptop_state = LiveState::starting_now();
    let received = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let frame = laptop_state
                .publish_presence(2, |presence| presence.pointer = Some([5.0, 6.0]))
                .unwrap();
            laptop_peer.publish_live(workspace(), frame).unwrap();
            let Ok(Some(broadcast)) =
                tokio::time::timeout(Duration::from_millis(200), live.member(&laptop_namespace))
                    .await
            else {
                continue;
            };
            let Ok(mut track) = broadcast
                .track(tilo_moq::live::PRESENCE_TRACK)
                .unwrap()
                .subscribe(None)
                .await
            else {
                continue;
            };
            for _ in 0..20 {
                let frame = laptop_state
                    .publish_presence(3, |presence| presence.pointer = Some([7.0, 8.0]))
                    .unwrap();
                laptop_peer.publish_live(workspace(), frame).unwrap();
                if let Ok(Ok(Some(datagram))) =
                    tokio::time::timeout(Duration::from_millis(100), track.recv_datagram()).await
                {
                    return datagram.payload.to_vec();
                }
            }
        }
    })
    .await
    .expect("a laptop frame reaches the browser's live session");
    assert!(matches!(
        LiveFrame::decode(&received),
        Ok(LiveFrame::Presence(presence))
            if presence.pointer == Some([5.0, 6.0]) || presence.pointer == Some([7.0, 8.0])
    ));
    drop(live);

    // 5. Revocation: the host ends the HTTPS session, refuses the handshake
    // from then on, and the browser stops in `failed`.
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
    assert_eq!(failed.error_code, Some("certificate-rejected"));
    assert_eq!(browser.status().phase, TransportPhase::Failed);
    browser.close();
    laptop_peer.close();
    host.relay.shutdown().await;
}
