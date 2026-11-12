//! A page keeps the content its records reference: another member asking the workspace for it
//! is answered by the page's session, to that member alone, in pieces that make up the bytes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Arc;
use tilo_session_client::{Cmd, Outbox};

use neighbour_wire::content::ContentWants;
use tilo_session_client::Event;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{enrol, ready, start_host, start_peer, wait_for};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_asking_for_content_a_page_keeps_is_answered_by_its_session() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let keeper = enrol(&root, "browser", DeviceCapabilities::member());
    let asker = enrol(&root, "phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    crate::support::place(&host.relay, &asker.cert).await;
    let start = |device: &crate::support::Device| {
        start_peer(
            host.iroh(),
            device.cert.clone(),
            host_device.cert.clone(),
            device.key.clone(),
        )
    };
    let (keeper_peer, asker_peer) = (start(&keeper), start(&asker));
    let heard = asker_peer.events();
    for peer in [&keeper_peer, &asker_peer] {
        peer.join_placed(crate::support::WORKSPACE, "Workspace")
            .unwrap();
    }
    ready(&[&keeper_peer, &asker_peer]).await;

    // More than one piece's worth, so the answer is several.
    let picture: Vec<u8> = (0..=u8::MAX).cycle().take(700_000).collect();
    let hash = blake3::hash(&picture).to_hex().to_string();
    let kept = picture.clone();
    let workspace = crate::support::WORKSPACE.to_owned();
    keeper_peer
        .send(Cmd::KeepContent(
            crate::support::workspace(),
            Arc::new(move |_app: &str, cluster: &str, _hash: &str| {
                (cluster == workspace).then(|| kept.clone())
            }),
        ))
        .unwrap();

    let device = asker.cert.device_public_key;
    let mut wants = ContentWants::default();
    let request = wants
        .ask(
            &device,
            (),
            ("so.tilo.notes", crate::support::WORKSPACE),
            &hash,
            1,
        )
        .unwrap();
    asker_peer
        .send(Cmd::WantContent(crate::support::workspace(), request))
        .unwrap();
    let arrived = wait_for("the page's pieces", || {
        let whole = heard.drain().into_iter().find_map(|event| match event {
            Event::RecordContent { from, frame, .. } if from == keeper.cert.device_public_key => {
                wants.piece(&device, &frame, tilo_clock::unix_now_ms())
            }
            _ => None,
        });
        async move { whole }
    })
    .await;
    assert_eq!(arrived.hash, hash);
    assert_eq!(arrived.bytes, picture, "the bytes the page keeps");
}
