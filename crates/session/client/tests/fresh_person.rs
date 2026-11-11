//! People who arrive one after another in a Lounge whose host governs it and
//! takes part in its channels. The first person's page opens the workspace's
//! records, so the host opens their channels; someone who comes later is
//! handed the workspace's whole history under those channels and must find
//! nothing in it written in the clear: the host writes nothing to a
//! workspace whose records are sealed channels, not even the empty shared
//! desktop a workspace whose channels it takes no part in starts with.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "support/call.rs"]
mod call;
mod support;

// This binary re-execs as the automerge worker (the host's workspaces use it).
tilo_automerge_isolate::test_worker!();

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_session_client::{Cmd, Outbox};
use tilo_user_identity::DeviceCapabilities;

use support::{
    TestHost, door, enrol, hears, ready, redeem, refused, say, start_peer, start_workspace_host,
    wait_for,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_later_person_finds_nothing_in_the_clear_in_a_sealed_lounge() {
    let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
    let host = enrol(
        &alice,
        "lounge-host",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let alice_laptop = enrol(&alice, "alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let hosted = start_workspace_host(&alice, &host, state.path(), false).await;
    let https = hosted.iroh();
    let relay = &hosted.relay;

    let lounge = relay.workspaces().unwrap().create("Lounge").await.unwrap();
    call::enter(relay, &alice_laptop.cert, &lounge.workspace).await;
    let lounge_id = tilo_relay::room_workspace_id(&lounge.workspace).unwrap();
    let welcome = DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: lounge.workspace.clone(),
        record: "welcome".into(),
    };

    let mut pages = Vec::new();
    for name in ["carol", "dave"] {
        let person = SigningKey::generate(&mut UnwrapErr(SysRng));
        let device = enrol(
            &person,
            &format!("{name}-browser"),
            DeviceCapabilities::member(),
        );
        let invitation = door(
            &hosted,
            &alice,
            &host,
            &lounge.workspace,
            person.verifying_key().to_bytes(),
        )
        .await;
        redeem(&hosted, invitation, &device.cert, &device.key).await;
        let page = start_peer(
            https.clone(),
            device.cert.clone(),
            host.cert.clone(),
            device.key.clone(),
        );
        page.send(Cmd::Join(lounge_id)).unwrap();
        ready(&[&page]).await;
        page.send(Cmd::OpenRoute(welcome.clone())).unwrap();
        pages.push(page);
    }
    let [carol, dave] = [&pages[0], &pages[1]];

    // Carol writes the workspace's welcome note; it goes out sealed once the
    // host admitted her to its channel.
    carol
        .write(
            welcome.clone(),
            &say(&mut automerge::AutoCommit::new(), "welcome to the lounge"),
        )
        .unwrap();
    let receipt = carol.barrier().unwrap();
    wait_for("the welcome note to be durable", || async {
        receipt.try_recv().ok()
    })
    .await
    .expect("the host keeps the note");

    // Dave, admitted to the workspace's channels after they were opened, reads
    // it, and refuses nothing the workspace's history holds.
    hears(dave, &welcome, "welcome to the lounge").await;
    for page in [carol, dave] {
        assert_eq!(
            refused(page),
            None,
            "a person was refused writes of the workspace"
        );
    }

    for page in pages {
        page.close();
    }
    let TestHost { relay, web, .. } = hosted;
    drop(web);
    relay.shutdown().await;
}
