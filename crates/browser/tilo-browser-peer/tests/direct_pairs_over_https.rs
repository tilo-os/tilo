//! Page DMs on the pair's record: two people's pages announce the pair they
//! share through the host's workspace door, learn it from the workspace's
//! governance, and then read and change the record it names. The host records
//! the proof and carries the record, and the conversation never goes to the
//! host's old private store.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::items_after_statements,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

// Placing the workspace has the host build its governance in an automerge worker.
tilo_automerge_isolate::test_worker!();

use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_browser_peer::PeerHandle;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_user_identity::{DeviceCapabilities, guest_certificate};

use support::{door, enrol, enter, next_document_on, start_workspace_host, wait_for};

/// A device certificate for `root`'s page, as the person's own root signs it.
async fn page_cert(root: &SigningKey, label: &str) -> (SigningKey, tilo_user_identity::DeviceCert) {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pages_announce_their_pair_and_talk_in_its_record() {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Owner", 1);
    let host_device = enrol(&owner, "owner-home", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&owner, &host_device, state.path(), false).await;
    let workspace = host
        .relay
        .workspaces()
        .unwrap()
        .create("Pages talking")
        .await
        .unwrap();

    // Two people's pages, each let in through the workspace's door.
    let alice_root = SigningKey::generate(&mut UnwrapErr(SysRng));
    let alice_root_key = alice_root.verifying_key().to_bytes();
    let (alice_key, alice_cert) = page_cert(&alice_root, "Alice's page").await;
    let alice = enter(
        &host,
        door(
            &host,
            &owner,
            &host_device,
            &workspace.workspace,
            alice_root_key,
        )
        .await,
        &alice_cert,
        &alice_key,
        &host_device.cert,
        workspace.workspace_id,
    )
    .await;
    let bob_root = SigningKey::generate(&mut UnwrapErr(SysRng));
    let bob_root_key = bob_root.verifying_key().to_bytes();
    let (bob_key, bob_cert) = page_cert(&bob_root, "Bob's page").await;
    let bob = enter(
        &host,
        door(
            &host,
            &owner,
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

    // Each page announces the pair its person shares; the workspace holds it
    // once, and both pages learn it from the governance they follow.
    let announce = |peer: &PeerHandle, other: [u8; 32]| {
        let (done, answer) = tokio::sync::oneshot::channel();
        peer.announce_pair_then(
            workspace.workspace_id,
            other,
            Box::new(move |result| {
                let _ = done.send(result);
            }),
        )
        .unwrap();
        async move { answer.await.unwrap() }
    };
    announce(&alice, bob_root_key).await.unwrap();
    announce(&bob, alice_root_key)
        .await
        .expect("the workspace already holds the pair, so the second is answered");
    let pairs = |peer: &PeerHandle| {
        let (done, answer) = tokio::sync::oneshot::channel();
        peer.pairs_then(
            workspace.workspace_id,
            Box::new(move |pairs| {
                let _ = done.send(pairs);
            }),
        )
        .unwrap();
        async move { answer.await.unwrap() }
    };
    let mut people = [hex::encode(alice_root_key), hex::encode(bob_root_key)];
    people.sort_unstable();
    let pair = collab_types::direct_pair_id(&workspace.workspace, &people[0], &people[1]).unwrap();
    for peer in [&alice, &bob] {
        let held = wait_for("the workspace to prove the pair", || {
            let pairs = pairs(peer);
            async move { pairs.await.ok().and_then(|pairs| pairs.get(&pair).cloned()) }
        })
        .await;
        assert_eq!(held, people, "both pages learn the pair they are in");
    }

    // The conversation is that record: one page writes, the other reads it
    // without asking again.
    let route = DocumentRoute {
        record: collab_types::direct_record(&pair),
        ..support::route()
    };
    alice
        .open_record(workspace.workspace_id, route.clone())
        .unwrap();
    bob.open_record(workspace.workspace_id, route.clone())
        .unwrap();
    let written = DocumentMutation::Incremental(b"a message from Alice's page".to_vec());
    alice.publish(route.clone(), &written).unwrap();
    let mut pending = Vec::new();
    assert_eq!(
        next_document_on(&bob, &mut pending, &route).await,
        written,
        "Bob's page reads what Alice's page wrote in the pair's record"
    );
    let reply = DocumentMutation::Incremental(b"and one back from Bob's page".to_vec());
    bob.publish(route.clone(), &reply).unwrap();
    assert_eq!(
        next_document_on(&alice, &mut pending, &route).await,
        reply,
        "and Alice's page reads his"
    );

    for peer in [&alice, &bob] {
        peer.close();
    }
    drop(host.web);
    host.relay.shutdown().await;
}
