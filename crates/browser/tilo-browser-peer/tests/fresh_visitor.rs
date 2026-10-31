//! Visitors who arrive one after another in a Lounge whose host governs it
//! and takes part in its channels. The first visitor's page opens the room's
//! records, so the host opens their channels; a visitor who comes
//! later is handed the room's whole history under those channels and must
//! find nothing in it written in the clear: the host writes nothing to a
//! room whose records are sealed channels, not even the empty shared
//! desktop a room whose channels it takes no part in starts with.

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

// This binary re-execs as the automerge worker (the host's rooms use it).
tilo_automerge_isolate::test_worker!();

use std::time::Duration;

use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_browser_peer::{Host, PeerEvent, PeerHandle};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_user_identity::DeviceCapabilities;

use support::{TestHost, door, enrol, ready, redeem, start_peer, start_room_host, wait_for};

/// Gather what `page` delivers: the welcome notes it opened and the
/// documents it refused.
fn gather(page: &PeerHandle, notes: &mut Vec<Vec<u8>>, refused: &mut Vec<String>) {
    for event in page.drain() {
        match event {
            PeerEvent::Document(document) if document.route.record == "welcome" => {
                if let Some(DocumentMutation::Snapshot(bytes)) = document.mutation() {
                    notes.push(bytes);
                }
            }
            PeerEvent::DocumentRejected(reason) => refused.push(reason),
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_later_visitor_finds_nothing_in_the_clear_in_a_sealed_lounge() {
    let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
    let host = enrol(
        &alice,
        "lounge-host",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let alice_laptop = enrol(&alice, "alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let hosted = start_room_host(&alice, &host, state.path(), false).await;
    let https = Host::Https(hosted.https());
    let relay = &hosted.relay;

    let lounge = relay.rooms().unwrap().create("Lounge").await.unwrap();
    call::enter(relay, &alice_laptop.cert, &lounge.room).await;
    let lounge_id = tilo_relay::room_workspace_id(&lounge.room).unwrap();
    let welcome = DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: lounge.room.clone(),
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
            &lounge.room,
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
        page.join(lounge_id).unwrap();
        ready(&[&page]).await;
        page.open_route(welcome.clone()).unwrap();
        pages.push(page);
    }
    let [carol, dave] = [&pages[0], &pages[1]];

    // Carol writes the room's welcome note; it goes out sealed once the
    // host admitted her to its channel.
    let note = b"welcome to the lounge".to_vec();
    carol
        .publish(welcome.clone(), &DocumentMutation::Snapshot(note.clone()))
        .unwrap();
    let receipt = carol.barrier().unwrap();
    wait_for("the welcome note to be durable", || async {
        receipt.try_recv().ok()
    })
    .await
    .expect("the host keeps the note");

    // Dave, admitted to the room's channels after they were opened, reads
    // it, and refuses nothing the room's history holds.
    let (mut notes, mut refused) = (Vec::new(), Vec::new());
    let read = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            gather(dave, &mut notes, &mut refused);
            if notes.contains(&note) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(
        read.is_ok(),
        "dave never read the welcome note; channels:\n{}",
        dave.status().channels
    );
    // What the replay still brings arrives in the same breath.
    tokio::time::sleep(Duration::from_secs(2)).await;
    for page in [carol, dave] {
        gather(page, &mut notes, &mut refused);
    }
    assert!(
        refused.is_empty(),
        "a visitor refused documents of the room: {refused:?}"
    );

    for page in pages {
        page.close();
    }
    let TestHost { relay, web } = hosted;
    drop(web);
    relay.shutdown().await;
}
