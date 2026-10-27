//! A native device that reaches its host over HTTPS only: the relay
//! session client over the same link a page uses, beside a device on the
//! host's iroh endpoint, sees what that device publishes in their workspace.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

use std::sync::Arc;

use tilo_relay_client::{AttributedPresence, Envelope};
use tilo_relay_session::RelayClient;
use tilo_user_identity::{DeviceCapabilities, RevocationList, UserIdentity};

use support::{enrol, start_host, workspace};

async fn subscribed(client: &RelayClient) {
    client
        .send(Envelope::Subscribe {
            workspace_id: workspace(),
        })
        .unwrap();
    tilo_test_env::within("the host subscribes the device", async {
        while !matches!(client.recv().await, Some(Envelope::Ready { .. })) {}
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_native_client_over_https_shares_the_workspace_with_an_iroh_one() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::member());
    let desktop = enrol(&root, "desktop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;

    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let iroh = RelayClient::connect_to_pinned_host(
        endpoint,
        host.relay.node_addr(),
        laptop.cert.clone(),
        laptop.key.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    let https = RelayClient::connect_https(
        host.https() as Arc<dyn tilo_relay_client::HttpClient>,
        host_device.cert.device_public_key,
        desktop.cert.clone(),
        &desktop.key,
        RevocationList::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        https.relay_peer.cert, host_device.cert,
        "the HTTPS session authenticated the pinned host"
    );
    subscribed(&iroh).await;
    subscribed(&https).await;

    let body = b"hello from the laptop".to_vec();
    iroh.send(Envelope::Presence {
        workspace_id: workspace(),
        payload: AttributedPresence {
            author: laptop.cert.device_public_key,
            body: body.clone(),
        }
        .encode()
        .unwrap(),
    })
    .unwrap();
    let seen = tilo_test_env::within("the presence reaches the HTTPS client", async {
        loop {
            if let Some(Envelope::Presence { payload, .. }) = https.recv().await {
                break AttributedPresence::decode(&payload).unwrap();
            }
        }
    })
    .await;
    assert_eq!(seen.author, laptop.cert.device_public_key);
    assert_eq!(seen.body, body);

    // Another host behind the same origin is not the pinned one.
    let elsewhere = RelayClient::connect_https(
        host.https(),
        laptop.cert.device_public_key,
        desktop.cert.clone(),
        &desktop.key,
        RevocationList::new(),
    )
    .await;
    drop(host);
    assert!(matches!(
        elsewhere,
        Err(tilo_relay_session::ClientError::Https(
            tilo_relay_client::LinkError::HostChanged
        ))
    ));
}
