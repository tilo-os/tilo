//! A page reaches its host over iroh only: its device key never leaves
//! custody, so its session dials from an endpoint of its own, and its device
//! signs that the endpoint speaks for it. The host admits the session by the
//! device's certificate and the workspace's plane by that delegation; the page
//! then says where it is on the plane and the workspace's other device hears
//! it through the host's own relay, opened with the scope's keys.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the host lives until the test ends"
)]

mod support;

// Placing the workspace has the host build its governance in an automerge worker.
tilo_automerge_isolate::test_worker!();

use std::sync::Arc;

use channel_crypto::ChannelContentKeys;
use channel_crypto::{ChannelGroup, MemberKey};
use collab_types::presence::Presence;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_browser_peer::{PeerEvent, PeerHandle, certify};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, UserIdentity};

use support::{
    WORKSPACE, approve_one, enrol, in_workspace, place, start_host, start_page, start_peer,
    wait_for,
};

/// The page asks its host who is here over its session, and every device in
/// `here` answers it: the account the HTTPS route answers with, and no HTTPS
/// session named the asker.
async fn assert_here(page: &PeerHandle, page_cert: &DeviceCert, device_cert: &DeviceCert) {
    let (reply, answer) = tokio::sync::oneshot::channel();
    page.here_then(
        tilo_relay_protocol::web::HereRequest {
            workspace_id: support::workspace(),
            label: "Ada on the page".into(),
            window: Some(7),
            actions: Vec::new(),
            agent: false,
        },
        Box::new(move |result| {
            let _ = reply.send(result);
        }),
    )
    .unwrap();
    let reply = tilo_test_env::within("the page asks who is here", answer)
        .await
        .unwrap()
        .unwrap();
    for device in [page_cert.device_public_key, device_cert.device_public_key] {
        assert!(
            reply.people.iter().any(|person| person.device == device),
            "{device:02x?} is here"
        );
    }
}

/// The workspace scope's record: its channel keys seal the plane's tracks.
fn plane_route() -> DocumentRoute {
    DocumentRoute {
        app_id: collab_types::scope::MEDIA_APP.to_owned(),
        cluster_id: WORKSPACE.to_owned(),
        record: collab_types::scope::WORKSPACE_RECORD.to_owned(),
    }
}

/// The scope's keys, as the workspace's members derive them.
fn keys() -> ChannelContentKeys {
    ChannelGroup::create(
        collab_types::ChannelId::new(),
        MemberKey::generate("ada").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_dials_its_host_from_an_endpoint_its_device_delegated() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let user = root.verifying_key().to_bytes();
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let state = support::scratch_dir();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;

    // The page is certified (over the host's HTTPS for now); the laptop
    // approves. Its key stays in custody from here on.
    let browser_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let (code_tx, code_rx) = tokio::sync::oneshot::channel::<String>();
    let certifying = tokio::spawn({
        let https = host.iroh();
        let key = browser_key.clone();
        async move {
            certify(
                &https,
                user,
                "Browser on Ada's desk",
                [0; 16],
                &key,
                |code| {
                    let _ = code_tx.send(code.to_string());
                },
            )
            .await
        }
    });
    let shown = code_rx.await.unwrap();
    assert_eq!(approve_one(&host, &root, &laptop).await, shown);
    let (browser_cert, host_cert) = certifying.await.unwrap().unwrap();

    place(&host.relay, &browser_cert).await;
    place(&host.relay, &laptop.cert).await;
    let keys = keys();

    // The page's session reaches the host over iroh, from an endpoint whose key
    // is not the device's: its device never handed the key out.
    let page = start_page(
        host.iroh(),
        browser_cert.clone(),
        host_cert.clone(),
        browser_key,
        Arc::new(tilo_browser_peer::MemoryPeerStore::default()),
    );
    page.install_channel_keys(plane_route(), keys.clone())
        .unwrap();
    page.join_placed(WORKSPACE, "Workspace").unwrap();
    in_workspace(&page, support::workspace()).await;

    // A device of the same person, dialling as its own key, joins the same
    // workspace over iroh.
    let device = start_peer(
        host.iroh(),
        laptop.cert.clone(),
        host_cert,
        laptop.key.clone(),
    );
    device
        .install_channel_keys(plane_route(), keys.clone())
        .unwrap();
    device.join_placed(WORKSPACE, "Workspace").unwrap();
    in_workspace(&device, support::workspace()).await;

    // The page says where it is on the workspace's plane. The host admits that
    // plane session only through the delegation the page's device signed: the
    // endpoint it arrived on speaks for the device, not under its key.
    let events = device.events();
    let said = Presence::pointing(WORKSPACE, 7, 0.25, 0.75);
    let (from, track, body) = wait_for("the device to hear the page on its plane", || {
        page.publish_presence(support::workspace(), vec![said.clone()])
            .unwrap();
        let events = events.drain();
        async move {
            for event in events {
                if let PeerEvent::LivePlane {
                    workspace_id,
                    from,
                    track,
                    body,
                    ..
                } = event
                    && workspace_id == support::workspace()
                {
                    return Some((from, track, body));
                }
            }
            None
        }
    })
    .await;
    assert_eq!(from, browser_cert.device_public_key);
    assert_eq!(
        track,
        tilo_live::live::Track::Presence,
        "the page's presence travels the plane's presence track"
    );
    assert_eq!(tilo_live::presence::decode(&body).unwrap(), said);
    assert_eq!(
        page.live_note(),
        None,
        "the page joined its plane over iroh"
    );

    // The page asks its host who is here over the same session: the account the
    // HTTPS route answers with, but with no HTTPS session to name the asker.
    assert_here(&page, &browser_cert, &laptop.cert).await;
}
