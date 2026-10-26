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

use automerge::AutoCommit;
use tilo_automerge::ApplyIncoming as _;
use tilo_browser_peer::{Host, PeerEvent, PeerHandle, TransportPhase};
use tilo_desktop_model::projection::RoomLayout;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{enrol, ready, start_peer, start_room_host, start_room_host_at, wait_for};

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

/// Take every room-document change `page` heard for `room` into `doc`.
fn hear_room(page: &PeerHandle, room: &str, workspace: u64, doc: &mut AutoCommit) -> bool {
    let mut heard = false;
    for event in page.drain() {
        if let PeerEvent::RoomChanged {
            workspace_id,
            room: named,
            changes,
        } = event
            && workspace_id == workspace
            && named == room
        {
            for change in changes {
                doc.load_incoming(&change).expect("the room's change reads");
            }
            heard = true;
        }
    }
    heard
}

/// The room's admitted document, as the host's `StatePeer` holds it.
async fn host_room(relay: &tilo_relay::Relay, room: &str) -> AutoCommit {
    let bytes = relay
        .rooms()
        .unwrap()
        .admitted_room_document(room)
        .await
        .unwrap();
    AutoCommit::load(&bytes).unwrap()
}

fn browser_layout(
    room: tilo_desktop_model::RoomId,
    salt: u16,
) -> tilo_desktop_model::ownership::SharedDesktop {
    use tilo_desktop_model::layout::{Strip, Tab, TileContent, WindowGroup};
    use tilo_desktop_model::{
        DesktopTree, StripId, WindowWidth, Workspace, WorkspaceId, compose_id,
    };
    let workspace = Workspace::in_room(WorkspaceId::of_room(room), room, "Team");
    let tab = compose_id(salt, 20);
    let strip = Strip {
        id: StripId(compose_id(salt, 1)),
        name: "Browser".into(),
        window_groups: vec![WindowGroup {
            id: compose_id(salt, 10),
            tabs: vec![Tab {
                id: tab,
                title: "Notes".into(),
                tile: TileContent::LocalApp {
                    app_id: "so.tilo.notes".into(),
                    label: "Notes".into(),
                    note: String::new(),
                },
                app_title: None,
            }],
            active_tab: tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    };
    let mut workspace = workspace;
    workspace.strips = vec![strip];
    tilo_desktop_model::ownership::SharedDesktop {
        tree: DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        },
        ..Default::default()
    }
}

/// Add a strip named `name` to the room document `doc`, as `salt`'s owner,
/// and return the changes it made.
fn add_browser_strip(
    doc: &mut AutoCommit,
    salt: u16,
    counter: u64,
    name: &str,
    actor: [u8; 32],
) -> Vec<Vec<u8>> {
    use tilo_desktop_model::layout::Strip;
    use tilo_desktop_model::{StripId, compose_id};
    let heads = doc.get_heads();
    let mut layout = tilo_desktop_model::room_layout::read(doc, None).expect("the room's layout");
    layout
        .tree
        .workspaces
        .first_mut()
        .expect("the room's workspace")
        .strips
        .push(Strip::new(StripId(compose_id(salt, counter)), name));
    doc.set_actor(automerge::ActorId::from(actor.to_vec()));
    tilo_desktop_model::room_layout::write(doc, &layout, salt, Some("alice")).unwrap();
    doc.commit();
    doc.get_changes(&heads)
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// A page publishes its windows into the room's own document; the host's
/// `StatePeer` admits them within the namespace it claimed and republishes
/// them, so the page reads its own admitted layout back from the room.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_publishes_its_layout_into_the_admitted_room_document() {
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
    let salt =
        tilo_desktop_model::ids::namespace_of_device(&hex::encode(laptop.cert.device_public_key));
    host.relay
        .rooms()
        .unwrap()
        .claim_topology_namespace(&room.room, &laptop.cert, salt)
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
    let mut room_doc = AutoCommit::new();
    wait_for("the claim and governance to reach the page", || {
        hear_room(&page, &room.room, room.workspace_id, &mut room_doc);
        let claimed = tilo_desktop_model::projection::contents_owners_of(&room_doc)
            .get(&salt)
            .is_some_and(|key| {
                key.eq_ignore_ascii_case(&hex::encode(laptop.cert.device_public_key))
            });
        async move { claimed.then_some(()) }
    })
    .await;

    let heads = room_doc.get_heads();
    let layout = browser_layout(tilo_desktop_model::RoomId::parse(&room.room).unwrap(), salt);
    room_doc.set_actor(automerge::ActorId::from(
        laptop.cert.device_public_key.to_vec(),
    ));
    tilo_desktop_model::room_layout::write(&mut room_doc, &layout, salt, Some("alice")).unwrap();
    room_doc.commit();
    let changes: Vec<Vec<u8>> = room_doc
        .get_changes(&heads)
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    assert!(!changes.is_empty(), "the page made a room change");
    page.publish_room_changes(room.workspace_id, &room.room, changes)
        .unwrap();

    let mut admitted = wait_for("the host to admit the page's layout", || async {
        let doc = host_room(&host.relay, &room.room).await;
        let shown = tilo_desktop_model::room_layout::read(&doc, None).is_some_and(|layout| {
            layout
                .tree
                .workspaces
                .iter()
                .any(|workspace| workspace.strips.iter().any(|strip| strip.name == "Browser"))
        });
        shown.then_some(doc)
    })
    .await;
    assert!(
        !admitted.get_changes(&[]).is_empty(),
        "the room's admitted document holds the page's own change"
    );
    page.close();
    host.relay.shutdown().await;
}

/// A page that arranges windows while its host is down keeps the changes in
/// its outbox; reconnecting to the same host replays them, the room's
/// `StatePeer` admits them, and the page reads the admitted layout back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_reconnects_and_its_retained_layout_reaches_the_room() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let laptop = enrol(&alice, "alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let listen: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
    let host = start_room_host_at(&alice, &host_device, state.path(), true, listen).await;
    let listen = host.web.addr;
    let room = host.relay.rooms().unwrap().create("Team").await.unwrap();
    host.relay
        .rooms()
        .unwrap()
        .enter(&room.room, &laptop.cert)
        .await
        .unwrap();
    let salt =
        tilo_desktop_model::ids::namespace_of_device(&hex::encode(laptop.cert.device_public_key));
    host.relay
        .rooms()
        .unwrap()
        .claim_topology_namespace(&room.room, &laptop.cert, salt)
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
    let mut room_doc = AutoCommit::new();
    wait_for("the claim to reach the page", || {
        hear_room(&page, &room.room, room.workspace_id, &mut room_doc);
        let claimed = tilo_desktop_model::projection::contents_owners_of(&room_doc)
            .get(&salt)
            .is_some_and(|key| {
                key.eq_ignore_ascii_case(&hex::encode(laptop.cert.device_public_key))
            });
        async move { claimed.then_some(()) }
    })
    .await;

    drop(host.web);
    host.relay.shutdown().await;
    wait_for("the page to see its host gone", || {
        let gone = page.status().phase != TransportPhase::Connected;
        async move { gone.then_some(()) }
    })
    .await;

    let first = add_browser_strip(
        &mut room_doc,
        salt,
        1,
        "Arranged offline",
        laptop.cert.device_public_key,
    );
    page.publish_room_changes(room.workspace_id, &room.room, first)
        .unwrap();
    // A second arrangement builds on the first, sent while still offline:
    // the outbox keeps them in the order they were made.
    let second = add_browser_strip(
        &mut room_doc,
        salt,
        4,
        "Arranged after",
        laptop.cert.device_public_key,
    );
    page.publish_room_changes(room.workspace_id, &room.room, second)
        .unwrap();
    wait_for("the offline layout to wait in the outbox", || {
        let waiting = page.status().outgoing_pending >= 2;
        async move { waiting.then_some(()) }
    })
    .await;

    let host = start_room_host_at(&alice, &host_device, state.path(), true, listen).await;
    page.reconnect().unwrap();
    wait_for("the replayed layout to be admitted", || async {
        let doc = host_room(&host.relay, &room.room).await;
        let names: Vec<String> = tilo_desktop_model::room_layout::read(&doc, None)
            .map(|layout| {
                layout
                    .tree
                    .workspaces
                    .into_iter()
                    .flat_map(|workspace| workspace.strips)
                    .map(|strip| strip.name)
                    .collect()
            })
            .unwrap_or_default();
        (names == ["Arranged offline", "Arranged after"]).then_some(())
    })
    .await;
    assert!(
        page.status().outgoing_pending == 0,
        "the room took the retained layout"
    );
    page.close();
    host.relay.shutdown().await;
}
