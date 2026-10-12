//! Ignoring an app prevents channel admission as well as document delivery.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup and assertions over the real peer and steward"
)]

#[path = "support/steward.rs"]
mod steward;
mod support;

use collab_types::DocumentMutation;
use tilo_browser_peer::{Host, PeerEvent};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{enrol, ready, route, start_host, start_peer, wait_for, workspace};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ignored_app_is_not_admitted_even_when_its_route_is_explicitly_opened() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let author_device = enrol(&root, "author", DeviceCapabilities::member());
    let reader_device = enrol(&root, "reader", DeviceCapabilities::member());
    let reader_key = reader_device.key.verifying_key().to_bytes();
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let steward = steward::Steward::start(&host.relay, &root).await;
    let ignored = route();
    let mut wanted = route();
    wanted.app_id = "so.tilo.sheets".into();

    let author = start_peer(
        Host::Https(host.https()),
        author_device.cert,
        host_device.cert.clone(),
        author_device.key,
    );
    author.join(workspace()).unwrap();
    ready(&[&author]).await;
    for document in [&ignored, &wanted] {
        author
            .publish(
                document.clone(),
                &DocumentMutation::Snapshot(b"retained".to_vec()),
            )
            .unwrap();
    }
    let receipt = author.barrier().unwrap();
    wait_for("both records to be durable", || async {
        receipt.try_recv().ok()
    })
    .await
    .unwrap();

    let reader = start_peer(
        Host::Https(host.https()),
        reader_device.cert,
        host_device.cert.clone(),
        reader_device.key,
    );
    reader.ignore_apps(vec![ignored.app_id.clone()]).unwrap();
    reader.open_route(ignored.clone()).unwrap();
    reader.open_route(wanted.clone()).unwrap();
    reader.join(workspace()).unwrap();
    let mut documents = Vec::new();
    wait_for("the wanted record to arrive", || {
        documents.extend(reader.drain().into_iter().filter_map(|event| match event {
            PeerEvent::Document(document) => Some(document.route.clone()),
            _ => None,
        }));
        let arrived = documents.contains(&wanted);
        async move { arrived.then_some(()) }
    })
    .await;
    assert!(documents.iter().all(|document| document == &wanted));
    assert!(steward.is_member(&wanted, reader_key));
    assert!(!steward.is_member(&ignored, reader_key));
    reader.close();
    author.close();
    drop(host);
}
