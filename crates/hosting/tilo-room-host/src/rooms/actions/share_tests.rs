//! A member's share of one record or item with a room this host governs,
//! as a page asks it (`crate::rooms::shares`): the same rule every receiver
//! holds a device's own share to (`tilo_peer_state::room::share`).

#![expect(
    clippy::significant_drop_tightening,
    reason = "the relay and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the host it drives and releases no lock"
)]

use serde_json::{Value, json};
use tilo_user_identity::DeviceCert;

use super::tests::{admit, governed};
use crate::rooms::Rooms;
use crate::rooms::tests::{governor, test_host};

const SHARE_RESOURCE: &str = "share";
const UNSHARE_RESOURCE: &str = "unshare";
const RESOURCE_PERMITS: &str = "permits";

/// A member's ask of `room`: share, end a share, or what they may do at a
/// node, with its arguments.
struct Ask {
    room: String,
    action: &'static str,
    argument: Value,
}

fn member_request(room: &str, action: &'static str, argument: Value) -> Ask {
    Ask {
        room: room.to_owned(),
        action,
        argument,
    }
}

/// What the room answers `ask` from `device`.
async fn invoke(rooms: &Rooms, device: &DeviceCert, ask: Ask) -> Result<Value, String> {
    let text = |key: &str| ask.argument[key].as_str().unwrap_or_default().to_owned();
    let node = || -> Result<tilo_action_policy::TreePath, String> {
        text("node").parse().map_err(|_| "not a node".to_owned())
    };
    match ask.action {
        SHARE_RESOURCE => {
            let access = text("access").parse().map_err(|_| "no access".to_owned())?;
            rooms
                .share_resource(&ask.room, device, &text("share"), &node()?, access)
                .await
                .map(|grants| json!({ "grants": grants }))
        }
        UNSHARE_RESOURCE => rooms
            .unshare_resource(&ask.room, device, &text("share"))
            .await
            .map(|revoked| json!({ "revoked": revoked })),
        _ => {
            let node = node()?;
            let now = web_time::SystemTime::now();
            let state = rooms
                .room(&ask.room)
                .await
                .map_err(|error| error.to_string())?;
            let held = state.lock().await;
            let member = held.peer().permissions(device.device_public_key, now)?;
            drop(held);
            return Ok(json!({
                "read": member.allows(&node, tilo_action_policy::ActionKind::Read, now),
                "undoable": member.allows(&node, tilo_action_policy::ActionKind::Undoable, now),
            }));
        }
    }
    .map_err(|error| error.to_string())
}

/// An item of Notes' own record of `room`, by its id.
fn note(room: &str, id: &str) -> String {
    tilo_action_policy::TreePath::room(room)
        .app("so.tilo.notes")
        .kind("notes.notebook")
        .any(tilo_action_policy::Level::Resource)
        .item(id)
        .to_string()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one share told whole, through both doors, in order"
)]
async fn a_share_grants_the_room_one_item_until_its_sharer_ends_it() {
    use tilo_relay_protocol::rooms::{RoomsReply, RoomsRequest};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let rooms = relay.rooms();
    let (room, _, _) = governed(rooms).await;
    let ada = admit(rooms, &room, &owner, 1).await;
    let bo = admit(rooms, &room, &owner, 2).await;
    rooms
        .set_member_role(
            &room,
            &owner,
            crate::rooms::SetMemberRoleRequest {
                request_id: [40; 32],
                user_public_key: bo.user_public_key,
                role: tilo_peer_state::join::MemberRole::Viewer,
            },
        )
        .await
        .unwrap();
    let (p1, p2) = (note(&room, "p1"), note(&room, "p2"));
    let permits = |node: &str| member_request(&room, RESOURCE_PERMITS, json!({ "node": node }));
    let share = |access: &str| {
        member_request(
            &room,
            SHARE_RESOURCE,
            json!({ "share": "m1", "node": p1, "access": access }),
        )
    };
    let unshare = || member_request(&room, UNSHARE_RESOURCE, json!({ "share": "m1" }));
    let bo_writes = |node: &str| {
        let request = permits(node);
        let bo = &bo;
        async move {
            invoke(rooms, bo, request).await.unwrap()["undoable"]
                .as_bool()
                .unwrap()
        }
    };

    // Bo, a Viewer, reads the note and writes nothing.
    let before = invoke(rooms, &bo, permits(&p1)).await.unwrap();
    assert_eq!(before, json!({ "read": true, "undoable": false }));

    // Bo cannot hand out more than he holds.
    assert!(invoke(rooms, &bo, share("edit")).await.is_err());

    // A view share gives no writes.
    let viewed = invoke(rooms, &ada, share("view")).await.unwrap();
    assert_eq!(viewed, json!({ "grants": 4 }));
    assert!(!bo_writes(&p1).await);

    // Ada, an Editor, shares it to edit: Bo writes that note, not its sibling.
    invoke(rooms, &ada, share("edit")).await.unwrap();
    assert!(bo_writes(&p1).await);
    assert!(
        !bo_writes(&p2).await,
        "a sibling note stays as Bo's role has it"
    );
    assert_eq!(
        invoke(rooms, &ada, share("edit")).await.unwrap(),
        json!({ "grants": 4 }),
        "a repeat is the same share"
    );
    // The page door decides the same.
    assert_eq!(
        rooms
            .page_rooms(
                &bo,
                RoomsRequest::Permits {
                    room: room.clone(),
                    node: p1.clone(),
                },
            )
            .await,
        RoomsReply::Permits(true)
    );

    // Only its sharer ends it.
    assert_eq!(
        invoke(rooms, &bo, unshare()).await.unwrap(),
        json!({ "revoked": 0 })
    );
    assert!(bo_writes(&p1).await);
    assert_eq!(
        invoke(rooms, &ada, unshare()).await.unwrap(),
        json!({ "revoked": 4 })
    );
    assert!(!bo_writes(&p1).await);

    // Shared again through a page's door, and ended there.
    let shared = rooms
        .page_rooms(
            &ada,
            RoomsRequest::Share {
                room: room.clone(),
                share: "m1".into(),
                node: p1.clone(),
                access: "edit".into(),
            },
        )
        .await;
    assert_eq!(shared, RoomsReply::Shared(4));
    assert!(bo_writes(&p1).await);
    let every = tilo_action_policy::TreePath::room(&room)
        .app("so.tilo.notes")
        .kind("notes.notebook")
        .any(tilo_action_policy::Level::Resource)
        .to_string();
    assert!(matches!(
        rooms
            .page_rooms(
                &ada,
                RoomsRequest::Share {
                    room: room.clone(),
                    share: "m2".into(),
                    node: every,
                    access: "view".into(),
                },
            )
            .await,
        RoomsReply::Refused(_)
    ));
    assert_eq!(
        rooms
            .page_rooms(
                &ada,
                RoomsRequest::Unshare {
                    room: room.clone(),
                    share: "m1".into(),
                },
            )
            .await,
        RoomsReply::Shared(4)
    );
    assert!(!bo_writes(&p1).await);
    relay.shutdown().await;
}

#[tokio::test]
async fn a_share_to_edit_lifts_no_restriction() {
    use tilo_relay_protocol::rooms::{RoomsReply, RoomsRequest};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let rooms = relay.rooms();
    let (room, _, _) = governed(rooms).await;
    let ada = admit(rooms, &room, &owner, 1).await;
    let bo = admit(rooms, &room, &owner, 2).await;
    rooms
        .set_member_role(
            &room,
            &owner,
            crate::rooms::SetMemberRoleRequest {
                request_id: [40; 32],
                user_public_key: bo.user_public_key,
                role: tilo_peer_state::join::MemberRole::Viewer,
            },
        )
        .await
        .unwrap();
    let p1 = note(&room, "p1");
    let shared = json!({ "share": "m1", "node": p1, "access": "edit" });
    invoke(rooms, &ada, member_request(&room, SHARE_RESOURCE, shared))
        .await
        .unwrap();
    let permits = || {
        invoke(
            rooms,
            &bo,
            member_request(&room, RESOURCE_PERMITS, json!({ "node": p1 })),
        )
    };
    assert_eq!(
        permits().await.unwrap(),
        json!({ "read": true, "undoable": true })
    );

    // Restricted, Bo writes nothing, the note shared to edit included, and
    // still reads it.
    let until = tilo_clock::unix_now_ms() + 30 * 60_000;
    rooms
        .restrict_member(
            &room,
            &owner,
            crate::rooms::RemoveMemberRequest {
                request_id: [41; 32],
                user_public_key: bo.user_public_key,
            },
            until,
        )
        .await
        .unwrap();
    assert_eq!(
        permits().await.unwrap(),
        json!({ "read": true, "undoable": false })
    );
    assert_eq!(
        rooms
            .page_rooms(
                &bo,
                RoomsRequest::Permits {
                    room: room.clone(),
                    node: p1.clone(),
                },
            )
            .await,
        RoomsReply::Permits(false),
        "the page door decides the same"
    );
    // Nor may he hand out what his restriction keeps from him.
    let reshare = json!({ "share": "m2", "node": p1, "access": "view" });
    assert!(
        invoke(rooms, &bo, member_request(&room, SHARE_RESOURCE, reshare))
            .await
            .is_err()
    );

    // Lifted, the share lets him write again.
    rooms
        .unrestrict_member(
            &room,
            &owner,
            crate::rooms::RemoveMemberRequest {
                request_id: [42; 32],
                user_public_key: bo.user_public_key,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        permits().await.unwrap(),
        json!({ "read": true, "undoable": true })
    );
    relay.shutdown().await;
}

/// A window is shared under its own share id by the device it runs on, the
/// one its tab's topology namespace is registered to; nobody shares another
/// device's window, nor a window in a namespace no device holds. The share
/// is the room's record of who shared which window, read back by every
/// member ([`tilo_desktop_model::window_share::window_shares`]).
#[tokio::test]
async fn only_the_device_a_window_runs_on_shares_it() {
    use tilo_desktop_model::window_share::{window_node, window_share_id};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let rooms = relay.rooms();
    let (room, _, _) = governed(rooms).await;
    let ada = admit(rooms, &room, &owner, 1).await;
    let bo = admit(rooms, &room, &owner, 2).await;
    // Ada's device mints its windows under the salt its key derives.
    let salt = tilo_desktop_model::ids::device_salt_of_key(&ada.device_public_key);
    let notes = (u64::from(salt) << 48) | 4;
    let share = |tab: u64, id: &str| {
        member_request(
            &room,
            SHARE_RESOURCE,
            json!({
                "share": id,
                "node": window_node(&room, "so.tilo.notes", tab).to_string(),
                "access": "view",
            }),
        )
    };
    let refused = |answer: Result<Value, String>| answer.is_err();
    assert!(
        refused(invoke(rooms, &bo, share(notes, &window_share_id(notes))).await),
        "Bo can't share a window of Ada's device"
    );
    assert!(
        refused(invoke(rooms, &ada, share(notes, "m1")).await),
        "a window is shared under its own share id"
    );
    let held_salts = {
        let state = rooms.room(&room).await.unwrap();
        let held = state.lock().await;
        tilo_desktop_model::window_share::device_salts(held.peer().admitted_room().unwrap())
    };
    assert_eq!(
        held_salts.get(&salt),
        Some(&hex::encode(ada.device_public_key)),
        "the room knows which device mints under Ada's salt"
    );
    let unheld = (1..u16::MAX)
        .find(|salt| !held_salts.contains_key(salt))
        .expect("a salt none of the room's devices mints under");
    let elsewhere = (u64::from(unheld) << 48) | 4;
    assert!(
        refused(invoke(rooms, &ada, share(elsewhere, &window_share_id(elsewhere))).await),
        "nor a window in a namespace no device holds"
    );
    assert!(shared_windows(rooms, &room).await.is_empty());

    invoke(rooms, &ada, share(notes, &window_share_id(notes)))
        .await
        .unwrap();
    let now_shared = shared_windows(rooms, &room).await;
    let shared = now_shared.get(&notes).expect("Ada's Notes is shared");
    assert_eq!(shared.host, hex::encode(ada.device_public_key));
    assert_eq!(shared.sharer, hex::encode(ada.user_public_key));
    assert_eq!(shared.app_id, "so.tilo.notes");

    invoke(
        rooms,
        &ada,
        member_request(
            &room,
            UNSHARE_RESOURCE,
            json!({ "share": window_share_id(notes) }),
        ),
    )
    .await
    .unwrap();
    assert!(
        shared_windows(rooms, &room).await.is_empty(),
        "the share ended"
    );
    relay.shutdown().await;
}

/// What a Tilo app's shared window shows is shared beside the window, under
/// its share id, by the device the window runs on alone; ending the
/// window's share ends it too.
#[tokio::test]
async fn what_a_shared_window_shows_is_shared_by_its_device_and_ends_with_it() {
    use tilo_desktop_model::window_share::{
        WindowShows, shows_node, window_node, window_share_id, window_shows_share_id,
    };

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let rooms = relay.rooms();
    let (room, _, _) = governed(rooms).await;
    let ada = admit(rooms, &room, &owner, 1).await;
    let bo = admit(rooms, &room, &owner, 2).await;
    // Ada's device mints its windows under the salt its key derives.
    let salt = tilo_desktop_model::ids::device_salt_of_key(&ada.device_public_key);
    let notes = (u64::from(salt) << 48) | 4;
    let at = |page: &str| WindowShows {
        record: String::new(),
        item: Some(format!("notes.page/pages/{page}")),
    };
    let note = at("7");
    let request = |id: String, node: tilo_action_policy::TreePath| share_request(&room, &id, &node);
    let showing =
        |shown: &WindowShows| shows_node(&room, "so.tilo.notes", notes, "notes.notebook", shown);
    let shows = |shown: &WindowShows| request(window_shows_share_id(notes), showing(shown));
    let by_ada = |request: Ask| invoke(rooms, &ada, request);
    assert!(
        refused(&invoke(rooms, &bo, shows(&note)).await),
        "Bo can't say what a window of Ada's device shows"
    );
    assert!(
        refused(&by_ada(request(window_share_id(notes), showing(&note))).await),
        "the window's own share id is the window's node alone"
    );
    by_ada(shows(&note)).await.unwrap();
    let window = window_node(&room, "so.tilo.notes", notes);
    by_ada(request(window_share_id(notes), window))
        .await
        .unwrap();
    let shown_now = || shown_in(rooms, &room, notes);
    assert_eq!(
        shown_now().await,
        Some(note),
        "the room's record says which note Ada's Notes window is at"
    );

    // Ada's window moves to another note: sharing what it shows again, under
    // the same share id, is the room's record of where it is now, in place
    // of where it was.
    let moved = at("8");
    let before = grants_of_share(rooms, &room, &window_shows_share_id(notes)).await;
    by_ada(shows(&moved)).await.unwrap();
    assert_eq!(
        shown_now().await,
        Some(moved),
        "the room's record follows Ada's window to the other note"
    );
    assert_eq!(
        grants_of_share(rooms, &room, &window_shows_share_id(notes)).await,
        before,
        "the same share, written over: never two notes at once"
    );

    let unshare = json!({ "share": window_share_id(notes) });
    by_ada(member_request(&room, UNSHARE_RESOURCE, unshare))
        .await
        .unwrap();
    assert_eq!(
        grants_of_share(rooms, &room, &window_share_id(notes)).await,
        0,
        "ending the window's share ended what it shows"
    );
    relay.shutdown().await;
}

/// The windows shared in `room` now, by the room's own record.
async fn shared_windows(
    rooms: &crate::rooms::Rooms,
    room: &str,
) -> std::collections::BTreeMap<u64, tilo_desktop_model::window_share::WindowShare> {
    let state = rooms.room(room).await.unwrap();
    let held = state.lock().await;
    let doc = held.peer().admitted_room().unwrap();
    let shared = tilo_desktop_model::window_share::window_shares(
        &tilo_desktop_model::projection::room_policy_of(doc, room),
        &tilo_desktop_model::window_share::device_salts(doc),
        tilo_clock::unix_now_ms(),
    );
    drop(held);
    shared
}

/// How many of `room`'s grants are of the share `share`.
async fn grants_of_share(rooms: &crate::rooms::Rooms, room: &str, share: &str) -> usize {
    let state = rooms.room(room).await.unwrap();
    let held = state.lock().await;
    let grants =
        tilo_desktop_model::projection::room_policy_of(held.peer().admitted_room().unwrap(), room)
            .grants
            .iter()
            .filter(|grant| tilo_action_policy::share::is_of_share(grant, share))
            .count();
    drop(held);
    grants
}

/// Sharing `node` in `room`, viewable, under the share id `id`.
fn share_request(room: &str, id: &str, node: &tilo_action_policy::TreePath) -> Ask {
    member_request(
        room,
        SHARE_RESOURCE,
        json!({ "share": id, "node": node.to_string(), "access": "view" }),
    )
}

/// Whether the room refused `answer`.
const fn refused(answer: &Result<Value, String>) -> bool {
    answer.is_err()
}

/// What the window `tab` shows, by `room`'s record of its shares.
async fn shown_in(
    rooms: &crate::rooms::Rooms,
    room: &str,
    tab: u64,
) -> Option<tilo_desktop_model::window_share::WindowShows> {
    shared_windows(rooms, room)
        .await
        .get(&tab)
        .and_then(|share| share.shows.clone())
}
