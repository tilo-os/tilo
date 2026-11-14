//! A page and the other person's desktop talking in their conversation's
//! record: the record's id is the two people's, derived on both sides, so the
//! page opens it like any other record of the workspace. The host admits the
//! page to the workspace's group; the desktop (a native device of the other
//! person) opens what the page wrote, live, and the page opens what the
//! desktop writes back.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::items_after_statements,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_session_client::{Cmd, Outbox};

use collab_types::{
    DocumentFrame, Ed25519PublicKey, OpKind, SignedOp, document_artifact_id,
    document_attestation_id,
};
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_client::invite::guest_certificate;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, UserIdentity};

use crate::native::NativeDevice;
use crate::support::{door, enrol, enter, hears, redeem, say, start_workspace_host};

/// A device certificate for `root`'s page, as the person's own root signs it.
async fn page_cert(root: &SigningKey, label: &str) -> (SigningKey, DeviceCert) {
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let cert = guest_certificate(
        root,
        key.verifying_key().to_bytes(),
        label,
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    (key, cert)
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "async Relay::shutdown consumes the relay; the suggested later drop is a use-after-move"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_and_the_other_persons_desktop_talk_in_their_conversations_record() {
    // Alice runs the host her workspace lives on, and her desktop is another
    // device of hers; Bob takes part through the workspace's door.
    let (alice_root, _) = UserIdentity::generate("Alice", 1);
    let alice_root_key = alice_root.verifying_key().to_bytes();
    let bob_root = SigningKey::generate(&mut UnwrapErr(SysRng));
    let bob_root_key = bob_root.verifying_key().to_bytes();
    let host_device = enrol(&alice_root, "alice-home", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice_root, &host_device, state.path(), false).await;
    let workspace = host
        .relay
        .workspaces()
        .unwrap()
        .create("Pages talking")
        .await
        .unwrap();

    // Bob's page, let in through the workspace's door: a browser of bob's.
    let (bob_key, bob_cert) = page_cert(&bob_root, "Bob's page").await;
    let bob = enter(
        &host,
        door(
            &host,
            &alice_root,
            &host_device,
            &workspace.workspace,
            bob_root_key,
        )
        .await,
        &bob_cert,
        &bob_key,
        &host_device.cert,
        workspace.workspace_id,
    )
    .await;

    // Alice's desktop: a native device of hers, let in the same way.
    let alice = enrol(&alice_root, "alice-desktop", DeviceCapabilities::primary());
    redeem(
        &host,
        door(
            &host,
            &alice_root,
            &host_device,
            &workspace.workspace,
            alice_root_key,
        )
        .await,
        &alice.cert,
        &alice.key,
    )
    .await;

    // The conversation's record is named by the two people it is between, so
    // the page derives its id without asking anyone: it opens the record like
    // any other record of the workspace.
    let mut people = [hex::encode(bob_root_key), hex::encode(alice_root_key)];
    people.sort_unstable();
    let pair = collab_types::direct_pair_id(&workspace.workspace, &people[0], &people[1]).unwrap();

    // Alice's desktop joins the workspace's group, which the host founded:
    // the conversation's record is sealed under it like every record of the
    // workspace. The page opens the record and writes its message, which
    // waits for the keys the host hands out.
    let route = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: workspace.workspace.clone(),
        record: collab_types::direct_record(&pair),
    };
    let artifact = document_artifact_id(&route.app_id, &route.cluster_id, &route.record);
    let alice_device = alice.key.verifying_key().to_bytes();
    let mut desktop =
        NativeDevice::start(&host.relay, alice_root_key, alice, workspace.workspace_id).await;
    desktop.join().await;
    let bob_device = bob_cert.device_public_key;

    bob.send(Cmd::OpenRecordAt(workspace.workspace_id, route.clone()))
        .unwrap();
    bob.send(Cmd::OpenRoute(route.clone())).unwrap();
    let message = say(
        &mut automerge::AutoCommit::new(),
        "a message from Bob's page",
    );
    bob.write(route.clone(), &message).unwrap();

    // The host admits the page when it asks, and the desktop opens what the
    // page sealed: the message arrives live.
    assert_eq!(
        desktop.written(bob_device, &route).await,
        DocumentFrame::Whole(message),
        "the desktop reads what the page wrote, live"
    );

    // The desktop writes back, sealed the same way; the page reads it live.
    let frame = say(
        &mut automerge::AutoCommit::new(),
        "and one back from Alice's desktop",
    )
    .encode()
    .unwrap();
    let (metadata, sealed) = desktop.seal(&route, &frame);
    let op = SignedOp::new_unsigned_with_encryption(
        sealed,
        artifact,
        Ed25519PublicKey::from_bytes(alice_device),
        document_attestation_id(&route.app_id, &route.cluster_id, &route.record),
        OpKind::Change,
        std::time::SystemTime::now(),
        2,
        metadata,
    );
    desktop.write(&route, op);
    // The page reads what the desktop wrote.
    hears(&bob, &route, "and one back from Alice's desktop").await;

    desktop.session.close().await;
    bob.close();
    drop(host.web);
    host.relay.shutdown().await;
}
