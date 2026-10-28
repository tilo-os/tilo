//! A page learns from the room's governance whether the room shares its
//! members' layout: in a room whose layout is personal it keeps its strips
//! and windows to itself and sends the room no desktop, which the room
//! would refuse; in a shared room it shares them as before.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the test holds its host for its length"
)]

mod support;

// This binary re-execs as the automerge worker (support is shared with crates that lack the dep).
tilo_automerge_isolate::test_worker!();

use tilo_browser_peer::{Host, PeerEvent, PeerHandle};
use tilo_desktop_model::projection::RoomLayout;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{enrol, ready, start_peer, start_room_host, wait_for};

/// Wait until `page` says `room`'s layout is `personal`, by its status and
/// by the event that tells its desktop.
async fn heard(page: &PeerHandle, room: &str, personal: bool) {
    let mut told = false;
    wait_for("the page to hear the room's layout scope", || {
        told |= page.drain().into_iter().any(|event| {
            matches!(
                event,
                PeerEvent::RoomLayout { room: r, personal: p } if r == room && p == personal
            )
        });
        let known = page.status().personal_rooms.contains(room) == personal;
        async move { (told && known).then_some(()) }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_keeps_to_its_rooms_layout_scope() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let laptop = enrol(&alice, "alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let host = start_room_host(&alice, &host_device, state.path(), false).await;
    let room = host.relay.rooms().unwrap().create("Team").await.unwrap();
    host.relay
        .rooms()
        .unwrap()
        .enter(&room.room, &laptop.cert)
        .await
        .unwrap();
    let page = start_peer(
        Host::Https(host.https()),
        laptop.cert.clone(),
        host_device.cert.clone(),
        laptop.key.clone(),
    );
    page.join(room.workspace_id).unwrap();
    ready(&[&page]).await;
    wait_for("the room to let the page in", || async {
        (page.status().rooms.get(&room.workspace_id) == Some(&true)).then_some(())
    })
    .await;
    assert!(
        !page.status().personal_rooms.contains(&room.room),
        "a room shares its members' layout until its governance says otherwise"
    );

    host.relay
        .rooms()
        .unwrap()
        .set_layout(&room.room, RoomLayout::Personal)
        .await
        .unwrap();
    heard(&page, &room.room, true).await;

    host.relay
        .rooms()
        .unwrap()
        .set_layout(&room.room, RoomLayout::Shared)
        .await
        .unwrap();
    heard(&page, &room.room, false).await;
}
