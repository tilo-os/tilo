//! A room's layout through a real relay: a page publishes its windows into
//! the room's own document, the host's `StatePeer` admits them within the
//! namespace the room registered to that device, and a retained command for
//! the host's operator strip is applied by the host, its actual owner.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the test holds its host for its length"
)]

use std::time::Duration;

use tilo_automerge::ApplyIncoming as _;
use tilo_desktop_model::commands::{BoundCommand, CommandSource, DesktopCommand};
use tilo_desktop_model::ids::{namespace_of_device, peer_salt_of};
use tilo_desktop_model::layout::{Strip, Tab, TileContent, WindowGroup};
use tilo_desktop_model::projection::LayoutRequest;
use tilo_desktop_model::{
    DesktopTree, RoomId, StripId, WindowWidth, Workspace, WorkspaceId, compose_id, room_layout,
};
use tilo_relay::{CoeditPayload, RelayClient};

use super::*;

/// The layout a room starts with: one workspace and no windows.
fn empty_layout(room: &str, shown: &str) -> tilo_desktop_model::ownership::SharedDesktop {
    let room = RoomId::parse(room).unwrap();
    let workspace = Workspace::in_room(WorkspaceId::of_room(room), room, shown);
    tilo_desktop_model::ownership::SharedDesktop {
        tree: DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        },
        ..Default::default()
    }
}

fn salt(device: &TestDevice) -> u16 {
    namespace_of_device(&hex::encode(device.device_pubkey()))
}

fn person(device: &TestDevice) -> String {
    hex::encode(device.cert.user_public_key)
}

/// A window of `device`'s: a strip, group and tab minted in its namespace.
fn window(device: &TestDevice, counter: u64, title: &str) -> Strip {
    let salt = salt(device);
    let tab = compose_id(salt, counter + 2);
    Strip {
        id: StripId(compose_id(salt, counter)),
        name: format!("{title} strip"),
        window_groups: vec![WindowGroup {
            id: compose_id(salt, counter + 1),
            tabs: vec![Tab {
                id: tab,
                title: title.into(),
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
    }
}

/// Connect `device` to `workspace_id` and take in the room's retained
/// document before it answers `Ready`.
async fn connect_room(
    relay: &Relay,
    device: &TestDevice,
    workspace_id: u64,
    room: &str,
) -> (RelayClient, AutoCommit) {
    let client = RelayClient::connect_to_pinned_host(
        device_endpoint(device).await,
        relay.node_addr(),
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    let mut doc = AutoCommit::new();
    tilo_test_env::within("the page's subscription is ready", async {
        loop {
            match client.recv().await {
                Some(Envelope::Delta { delta, .. }) => {
                    take_room_changes(&delta, room, &mut doc);
                }
                Some(Envelope::Ready { .. }) => break,
                Some(_) => {}
                None => panic!("the session closed before it was ready"),
            }
        }
    })
    .await;
    (client, doc)
}

/// Merge the room-document changes `delta` carries for `room` into `doc`.
fn take_room_changes(delta: &[u8], room: &str, doc: &mut AutoCommit) {
    let Some(CoeditPayload::Checkpoint { body, .. } | CoeditPayload::RetainedDelta { body, .. }) =
        CoeditPayload::decode(delta)
    else {
        return;
    };
    let Ok(NeighbourMsg::SyncResp {
        changes,
        room: named,
    }) = postcard::from_bytes::<NeighbourMsg>(&body)
    else {
        return;
    };
    if named != room {
        return;
    }
    for change in changes {
        doc.load_incoming(&change).expect("a room change reads");
    }
}

/// Read the next room-document message for `room` into `doc`.
async fn next_room(client: &RelayClient, room: &str, doc: &mut AutoCommit) {
    tilo_test_env::within("the room's next document change", async {
        loop {
            match client.recv().await {
                Some(Envelope::Delta { delta, .. }) => take_room_changes(&delta, room, doc),
                Some(_) => {}
                None => panic!("the session closed while waiting for the room"),
            }
        }
    })
    .await;
}

/// Take every room change that arrives for `window`, then stop.
async fn drain_room(client: &RelayClient, room: &str, doc: &mut AutoCommit, window: Duration) {
    let deadline = tokio::time::Instant::now() + window;
    while let Ok(Some(envelope)) = tokio::time::timeout_at(deadline, client.recv()).await {
        if let Envelope::Delta { delta, .. } = envelope {
            take_room_changes(&delta, room, doc);
        }
    }
}

/// Commit `message` as a page does, and the host's answer.
pub(super) async fn commit(
    client: &RelayClient,
    device: &TestDevice,
    workspace_id: u64,
    room: &str,
    message: &NeighbourMsg,
    doc: &mut AutoCommit,
) -> Result<u64, String> {
    let (channel, _) = neighbour_wire::retention(message, false).unwrap();
    let delta = CoeditPayload::RetainedDelta {
        channel,
        body: postcard::to_stdvec(message).unwrap(),
    }
    .encode()
    .unwrap();
    let op = *blake3::hash(&delta).as_bytes();
    let op: [u8; 16] = op[..16].try_into().unwrap();
    client
        .send(Envelope::Commit {
            workspace_id,
            op,
            author_device_public_key: device.device_pubkey(),
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta,
        })
        .unwrap();
    tilo_test_env::within("the host answers the commit", async {
        loop {
            match client.recv().await {
                Some(Envelope::Committed { op: acked, seq, .. }) if acked == op => return Ok(seq),
                Some(Envelope::CommitRefused {
                    op: acked, reason, ..
                }) if acked == op => return Err(reason),
                Some(Envelope::Delta { delta, .. }) => take_room_changes(&delta, room, doc),
                Some(_) => {}
                None => panic!("the session closed before the commit was answered"),
            }
        }
    })
    .await
}

/// The room's layout from `doc`, if it has one.
fn layout_of(doc: &AutoCommit) -> Option<tilo_desktop_model::ownership::SharedDesktop> {
    room_layout::read(doc, None)
}

fn titles(doc: &AutoCommit) -> Vec<String> {
    layout_of(doc)
        .into_iter()
        .flat_map(|layout| {
            layout
                .tree
                .workspaces
                .into_iter()
                .flat_map(|workspace| workspace.strips)
        })
        .flat_map(|strip| {
            std::iter::once(strip.name).chain(
                strip
                    .window_groups
                    .into_iter()
                    .flat_map(|group| group.tabs)
                    .map(|tab| tab.title),
            )
        })
        .collect()
}

/// A page writes its window into the room's own document; the host admits
/// it in the namespace the room registered to the device and republishes
/// the admitted layout to the page.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_publishes_its_windows_into_the_admitted_room_document() {
    let alice = TestUser::fresh("Alice");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let room = relay.rooms().unwrap().create("Team").await.unwrap();
    relay
        .rooms()
        .unwrap()
        .enter(&room.room, &laptop.cert)
        .await
        .unwrap();
    relay
        .rooms()
        .unwrap()
        .claim_topology_namespace(&room.room, &laptop.cert, salt(&laptop))
        .await
        .unwrap();
    let (client, mut doc) = connect_room(&relay, &laptop, room.workspace_id, &room.room).await;
    assert_eq!(
        tilo_desktop_model::projection::contents_owners_of(&doc)
            .get(&salt(&laptop))
            .map(String::as_str),
        Some(hex::encode(laptop.device_pubkey()).as_str()),
        "the room registered this page's namespace before it writes"
    );

    let heads = doc.get_heads();
    let mut layout = layout_of(&doc).unwrap_or_else(|| empty_layout(&room.room, "Team"));
    layout
        .tree
        .workspaces
        .first_mut()
        .unwrap()
        .strips
        .push(window(&laptop, 10, "Notes"));
    doc.set_actor(automerge::ActorId::from(laptop.device_pubkey().to_vec()));
    room_layout::write(&mut doc, &layout, salt(&laptop), Some(&person(&laptop))).unwrap();
    doc.commit();
    let changes = doc
        .get_changes(&heads)
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    let message = NeighbourMsg::SyncResp {
        changes,
        room: room.room.clone(),
    };
    commit(
        &client,
        &laptop,
        room.workspace_id,
        &room.room,
        &message,
        &mut doc,
    )
    .await
    .expect("the host admits the page's own windows");
    tilo_test_env::within("the host admits the page's own windows", async {
        loop {
            let bytes = relay
                .rooms()
                .unwrap()
                .admitted_room_document(&room.room)
                .await
                .unwrap();
            let host_doc = AutoCommit::load(&bytes).unwrap();
            if titles(&host_doc).iter().any(|title| title == "Notes") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;

    assert!(
        titles(&doc).iter().any(|title| title == "Notes"),
        "the room republishes the page's admitted windows"
    );
    let tab = compose_id(salt(&laptop), 12);
    assert_eq!(
        room_layout::read(&doc, None).unwrap().records,
        Vec::new(),
        "a window without a record is still a window"
    );
    assert!(
        room_layout::read(&doc, None)
            .unwrap()
            .tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .any(|window| window.id == tab),
        "the admitted window is the one the page minted"
    );
    relay.shutdown().await;
}

/// A moderator asks the host to rename the operator strip the host itself
/// made. The host applies the retained command once, records that it did in
/// its own namespace, and republishes the result.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_host_applies_a_retained_command_for_its_operator_strip() {
    let alice = TestUser::fresh("Alice");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let room = relay.rooms().unwrap().create("Team").await.unwrap();
    relay
        .rooms()
        .unwrap()
        .enter(&room.room, &laptop.cert)
        .await
        .unwrap();
    relay
        .rooms()
        .unwrap()
        .claim_topology_namespace(&room.room, &laptop.cert, salt(&laptop))
        .await
        .unwrap();

    let host_salt = salt(&host);
    let operator_id = compose_id(host_salt, 1);
    relay
        .rooms()
        .unwrap()
        .author_room_document(&room.room, &host.cert, |doc| {
            let mut layout =
                room_layout::read(doc, None).unwrap_or_else(|| empty_layout(&room.room, "Team"));
            layout
                .tree
                .workspaces
                .first_mut()
                .unwrap()
                .strips
                .push(Strip::new(StripId(operator_id), "Lobby"));
            room_layout::write(doc, &layout, host_salt, Some(&person(&host)))
                .map_err(|error| error.to_string())
        })
        .await
        .expect("the host writes its operator strip");
    let bytes = relay
        .rooms()
        .unwrap()
        .admitted_room_document(&room.room)
        .await
        .unwrap();
    let host_doc = AutoCommit::load(&bytes).unwrap();
    assert!(
        titles(&host_doc).iter().any(|title| title == "Lobby"),
        "the host's operator strip is in its admitted room document"
    );

    let (client, mut doc) = connect_room(&relay, &laptop, room.workspace_id, &room.room).await;
    tilo_test_env::within("the member reads the host's operator strip", async {
        while !titles(&doc).iter().any(|title| title == "Lobby") {
            next_room(&client, &room.room, &mut doc).await;
        }
    })
    .await;
    let heads = doc.get_heads();
    let command = BoundCommand {
        source: CommandSource {
            room: room.room.clone(),
            heads: heads.iter().map(|head| head.0).collect(),
        },
        command: DesktopCommand::RenameStrip {
            strip: operator_id,
            name: "Reception".into(),
        },
    };
    let command = serde_json::to_vec(&command).unwrap();
    let request = LayoutRequest {
        id: LayoutRequest::id(&person(&laptop), host_salt, 1, &command),
        from: person(&laptop),
        to_salt: host_salt,
        at: 1,
        command,
    };
    room_layout::request(&mut doc, &request).unwrap();
    doc.set_actor(automerge::ActorId::from(laptop.device_pubkey().to_vec()));
    doc.commit();
    let changes = doc
        .get_changes(&heads)
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    let message = NeighbourMsg::SyncResp {
        changes,
        room: room.room.clone(),
    };
    commit(
        &client,
        &laptop,
        room.workspace_id,
        &room.room,
        &message,
        &mut doc,
    )
    .await
    .expect("the room retains the moderator's request");

    let bytes = relay
        .rooms()
        .unwrap()
        .admitted_room_document(&room.room)
        .await
        .unwrap();
    let host_doc = AutoCommit::load(&bytes).unwrap();
    assert!(
        room_layout::requests(&host_doc)
            .iter()
            .any(|asked| asked.id == request.id),
        "the room retained the request"
    );
    assert!(
        titles(&host_doc).iter().any(|title| title == "Reception"),
        "the host applied the operator strip rename"
    );
    assert!(
        room_layout::applied(&host_doc, host_salt).contains(&request.id),
        "the host recorded that it applied the command"
    );
    assert_eq!(
        room_layout::pending(&host_doc, host_salt),
        Vec::new(),
        "the host applied it once"
    );
    relay.shutdown().await;
}

/// A command another device files in someone else's name is not applied:
/// the host judges the retained request against the room's policy before
/// committing it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_in_another_persons_name_is_not_admitted() {
    let alice = TestUser::fresh("Alice");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let room = relay.rooms().unwrap().create("Team").await.unwrap();
    relay
        .rooms()
        .unwrap()
        .enter(&room.room, &laptop.cert)
        .await
        .unwrap();
    let host_salt = salt(&host);
    let operator_id = compose_id(host_salt, 1);
    relay
        .rooms()
        .unwrap()
        .author_room_document(&room.room, &host.cert, |doc| {
            let mut layout =
                room_layout::read(doc, None).unwrap_or_else(|| empty_layout(&room.room, "Team"));
            layout
                .tree
                .workspaces
                .first_mut()
                .unwrap()
                .strips
                .push(Strip::new(StripId(operator_id), "Lobby"));
            room_layout::write(doc, &layout, host_salt, Some(&person(&host)))
                .map_err(|error| error.to_string())
        })
        .await
        .unwrap();
    let (client, mut doc) = connect_room(&relay, &laptop, room.workspace_id, &room.room).await;
    tilo_test_env::within("the member reads the host's operator strip", async {
        while !titles(&doc).iter().any(|title| title == "Lobby") {
            next_room(&client, &room.room, &mut doc).await;
        }
    })
    .await;
    let heads = doc.get_heads();
    let command = BoundCommand {
        source: CommandSource {
            room: room.room.clone(),
            heads: heads.iter().map(|head| head.0).collect(),
        },
        command: DesktopCommand::RenameStrip {
            strip: operator_id,
            name: "Stolen".into(),
        },
    };
    let command = serde_json::to_vec(&command).unwrap();
    let request = LayoutRequest {
        // Filed as though somebody else asked it.
        id: LayoutRequest::id(&"11".repeat(32), host_salt, 1, &command),
        from: "11".repeat(32),
        to_salt: host_salt,
        at: 1,
        command,
    };
    room_layout::request(&mut doc, &request).unwrap();
    doc.set_actor(automerge::ActorId::from(laptop.device_pubkey().to_vec()));
    doc.commit();
    let changes = doc
        .get_changes(&heads)
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    let message = NeighbourMsg::SyncResp {
        changes,
        room: room.room.clone(),
    };
    // The relay acknowledges the retained body; the host's governor refuses
    // to commit the request, so the room never shows it applied.
    let _ = commit(
        &client,
        &laptop,
        room.workspace_id,
        &room.room,
        &message,
        &mut doc,
    )
    .await;
    drain_room(&client, &room.room, &mut doc, Duration::from_millis(300)).await;
    assert!(
        !titles(&doc).iter().any(|title| title == "Stolen"),
        "another person's name in a request changes nothing"
    );
    assert!(
        room_layout::applied(&doc, host_salt).is_empty(),
        "the host never recorded it as applied"
    );
    assert!(
        peer_salt_of(operator_id) == host_salt,
        "the operator strip belongs to the host's namespace"
    );
    relay.shutdown().await;
}
