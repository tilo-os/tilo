//! A member's own business as `tilo.member` actions: offered to every
//! person the room admitted, and decided by the room methods the wire door
//! ran.

use serde_json::{Value, json};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    ActionHost as _, ActionHostError, Caller, CatalogueQuery, InvokeRequest, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_relay_session::rooms::{
    MEMBER_ACTIONS_PROVIDER, PREPARE_APPLICATION_RECORD, RECORD_EFFECT_RECEIPT, RESOURCE_PERMITS,
    SAVE_SHORTCUT, SHARE_RESOURCE, UNSHARE_RESOURCE, member_resource,
};
use tilo_user_identity::DeviceCert;

use super::super::HostRoomActions;
use super::super::tests::{admit, admit_device, caller, governed, settled};
use crate::rooms::tests::{governor, guest_device, test_host};

fn member_request(room: &str, action: &str, argument: Value) -> InvokeRequest {
    let mut request = super::super::tests::request(room, action, argument);
    request.target = RemoteActionTarget {
        provider: MEMBER_ACTIONS_PROVIDER.into(),
        address: ActionAddress::new(member_resource(room), action),
    };
    request
}

async fn offered(host: &HostRoomActions, workspace_id: u64, caller: &Caller) -> Vec<String> {
    let query = CatalogueQuery {
        provider: Some(MEMBER_ACTIONS_PROVIDER.into()),
        max_results: 100,
        ..CatalogueQuery::default()
    };
    settled(host.in_workspace(workspace_id).query(caller, query))
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter_map(|row| row.action.map(|action| action.address.action))
        .collect()
}

async fn invoke(
    host: &HostRoomActions,
    workspace_id: u64,
    device: &DeviceCert,
    request: InvokeRequest,
) -> Result<Value, ActionHostError> {
    let receipt = settled(
        host.in_workspace(workspace_id)
            .invoke(&caller(device), request),
    )
    .await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("a member's own business completes at once: {other:?}"),
    }
}

/// `person`'s own terminal receipt of `invocation`, redacted as the broker
/// redacts it.
fn terminal_audit(person: &[u8; 32], invocation: uuid::Uuid) -> String {
    audit_by(&hex::encode(person), invocation)
}

/// The terminal receipt of `invocation` by `actor`, redacted as the broker
/// redacts it.
fn audit_by(actor: &str, invocation: uuid::Uuid) -> String {
    let mut context =
        tilo_actions::ActionContext::for_resource("workspace", "workspace", "Private fixture");
    context.facets = vec![
        tilo_actions::ActionFacet::new("tilo.receipt", "effect"),
        tilo_actions::ActionFacet::new("tilo.invocation", invocation.to_string()),
    ];
    format!(
        "{{\"invocation\":{},\"actor\":{},\"context\":{},\"status\":{},\"completed_at\":1}}",
        serde_json::to_string(&invocation).unwrap(),
        serde_json::to_string(actor).unwrap(),
        serde_json::to_string(&context).unwrap(),
        serde_json::to_string(&tilo_actions::EffectReceiptStatus::Succeeded).unwrap(),
    )
}

#[tokio::test]
async fn a_member_claims_their_namespace_and_records_their_receipt_as_actions() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let rooms = relay.rooms();
    let (room, workspace, host) = governed(rooms).await;
    let ada = admit(rooms, &room, &owner, 1).await;

    assert_eq!(
        offered(&host, workspace, &caller(&ada)).await,
        [
            RECORD_EFFECT_RECEIPT,
            PREPARE_APPLICATION_RECORD,
            SAVE_SHORTCUT,
            SHARE_RESOURCE,
            UNSHARE_RESOURCE,
            RESOURCE_PERMITS
        ]
        .map(str::to_owned)
    );

    let permits = || {
        member_request(
            &room,
            RESOURCE_PERMITS,
            json!({ "node": tilo_action_policy::TreePath::room(&room).to_string() }),
        )
    };

    // Her own receipt is recorded; one claiming another actor is refused.
    let invocation = uuid::Uuid::new_v4();
    let receipt = member_request(
        &room,
        RECORD_EFFECT_RECEIPT,
        json!({
            "invocation": invocation.to_string(),
            "redacted": terminal_audit(&ada.user_public_key, invocation),
        }),
    );
    let recorded = invoke(&host, workspace, &ada, receipt).await.unwrap();
    assert!(recorded["generation"].as_u64().is_some(), "{recorded}");
    let spoof = uuid::Uuid::new_v4();
    let impersonating = member_request(
        &room,
        RECORD_EFFECT_RECEIPT,
        json!({
            "invocation": spoof.to_string(),
            "redacted": terminal_audit(&owner.user_public_key, spoof),
        }),
    );
    assert!(matches!(
        invoke(&host, workspace, &ada, impersonating).await,
        Err(ActionHostError::Refused { .. })
    ));
    // An agent's receipt: the host's own device, the governor's, records
    // what acts on it; a member records only what its own person did.
    let by_agent = |invocation: uuid::Uuid| {
        member_request(
            &room,
            RECORD_EFFECT_RECEIPT,
            json!({
                "invocation": invocation.to_string(),
                "redacted": audit_by("agent:writer", invocation),
            }),
        )
    };
    let recorded = invoke(&host, workspace, &owner, by_agent(uuid::Uuid::new_v4()))
        .await
        .unwrap();
    assert!(recorded["generation"].as_u64().is_some(), "{recorded}");
    assert!(matches!(
        invoke(&host, workspace, &ada, by_agent(uuid::Uuid::new_v4())).await,
        Err(ActionHostError::Refused { .. })
    ));

    // Someone the room never admitted is offered nothing and runs nothing,
    // and an agent holds no business of its own.
    let (_, stranger) = guest_device();
    assert!(
        offered(&host, workspace, &caller(&stranger))
            .await
            .is_empty()
    );
    assert_eq!(
        invoke(&host, workspace, &stranger, permits()).await,
        Err(ActionHostError::UnknownAction)
    );
    let agent = Caller::delegated_agent("writer", ada.user_public_key, ada.device_public_key);
    assert!(offered(&host, workspace, &agent).await.is_empty());

    // The member's business is that room's only.
    let mut elsewhere = permits();
    elsewhere.target.address = ActionAddress::new(member_resource("another"), RESOURCE_PERMITS);
    assert_eq!(
        invoke(&host, workspace, &ada, elsewhere).await,
        Err(ActionHostError::UnknownAction)
    );
    relay.shutdown().await;
}

#[tokio::test]
async fn a_receipt_sent_again_after_a_restart_is_recorded_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let (room, workspace, host) = governed(relay.rooms()).await;
    let ada = admit(relay.rooms(), &room, &owner, 1).await;
    let invocation = uuid::Uuid::new_v4();
    let receipt = || {
        member_request(
            &room,
            RECORD_EFFECT_RECEIPT,
            json!({
                "invocation": invocation.to_string(),
                "redacted": terminal_audit(&ada.user_public_key, invocation),
            }),
        )
    };
    let recorded = invoke(&host, workspace, &ada, receipt()).await.unwrap();
    assert_eq!(
        invoke(&host, workspace, &ada, receipt()).await.unwrap(),
        recorded,
        "a receipt asked again commits nothing new"
    );
    relay.shutdown().await;

    // The member's device asks again after the host restarted.
    let relay = test_host(dir.path(), &key, &owner).await;
    let host = HostRoomActions::new();
    host.attach(relay.rooms().clone());
    assert_eq!(
        invoke(&host, workspace, &ada, receipt()).await.unwrap(),
        recorded,
        "nor after the host restarted"
    );
    relay.shutdown().await;
}

fn shortcut(id: &str) -> tilo_automation::Shortcut {
    tilo_automation::Shortcut {
        id: id.into(),
        name: id.into(),
        inputs: Vec::new(),
        steps: Vec::new(),
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: Vec::new(),
        saved_by: None,
        saved_proof: None,
    }
}

#[tokio::test]
async fn a_member_saves_a_shortcut_only_as_themselves() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let rooms = relay.rooms();
    let (room, workspace, host) = governed(rooms).await;
    let (ada_key, ada) = admit_device(rooms, &room, &owner, 1).await;
    let save = |shortcut: tilo_automation::Shortcut| {
        let change = tilo_automation::ShortcutChange::Save(Box::new(shortcut));
        member_request(
            &room,
            SAVE_SHORTCUT,
            json!({ "change": serde_json::to_string(&change).unwrap() }),
        )
    };
    let held = || async {
        let governed = rooms.room(&room).await.unwrap();
        let governed = governed.lock().await;
        tilo_desktop_model::projection::workspace_shortcuts_of(
            governed.peer().admitted_room().unwrap(),
        )
    };

    // Ada names the owner as who saved it, with her own device's proof.
    let mut forged = shortcut("water");
    tilo_peer_state::room::sign_saved_shortcut(&room, &mut forged, &ada, &ada_key).unwrap();
    forged.saved_by = Some(hex::encode(owner.user_public_key));
    assert!(matches!(
        invoke(&host, workspace, &ada, save(forged)).await,
        Err(ActionHostError::Refused { .. })
    ));
    assert!(held().await.is_empty());

    let mut own = shortcut("water");
    tilo_peer_state::room::sign_saved_shortcut(&room, &mut own, &ada, &ada_key).unwrap();
    let saved = invoke(&host, workspace, &ada, save(own.clone()))
        .await
        .unwrap();
    assert!(saved["generation"].as_u64().is_some(), "{saved}");
    assert_eq!(held().await, vec![own]);
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
    let (room, workspace, host) = governed(rooms).await;
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
        let host = &host;
        let bo = &bo;
        async move {
            invoke(host, workspace, bo, request).await.unwrap()["undoable"]
                .as_bool()
                .unwrap()
        }
    };

    // Bo, a Viewer, reads the note and writes nothing.
    let before = invoke(&host, workspace, &bo, permits(&p1)).await.unwrap();
    assert_eq!(before, json!({ "read": true, "undoable": false }));

    // Bo cannot hand out more than he holds.
    assert!(matches!(
        invoke(&host, workspace, &bo, share("edit")).await,
        Err(ActionHostError::Refused { .. })
    ));

    // A view share gives no writes.
    let viewed = invoke(&host, workspace, &ada, share("view")).await.unwrap();
    assert_eq!(viewed, json!({ "grants": 4 }));
    assert!(!bo_writes(&p1).await);

    // Ada, an Editor, shares it to edit: Bo writes that note, not its sibling.
    invoke(&host, workspace, &ada, share("edit")).await.unwrap();
    assert!(bo_writes(&p1).await);
    assert!(
        !bo_writes(&p2).await,
        "a sibling note stays as Bo's role has it"
    );
    assert_eq!(
        invoke(&host, workspace, &ada, share("edit")).await.unwrap(),
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
        invoke(&host, workspace, &bo, unshare()).await.unwrap(),
        json!({ "revoked": 0 })
    );
    assert!(bo_writes(&p1).await);
    assert_eq!(
        invoke(&host, workspace, &ada, unshare()).await.unwrap(),
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
    let (room, workspace, host) = governed(rooms).await;
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
    invoke(
        &host,
        workspace,
        &ada,
        member_request(&room, SHARE_RESOURCE, shared),
    )
    .await
    .unwrap();
    let permits = || {
        invoke(
            &host,
            workspace,
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
    assert!(matches!(
        invoke(
            &host,
            workspace,
            &bo,
            member_request(&room, SHARE_RESOURCE, reshare),
        )
        .await,
        Err(ActionHostError::Refused { .. })
    ));

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
    use tilo_desktop_model::window_share::{window_node, window_share_id, window_shares};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let rooms = relay.rooms();
    let (room, workspace, host) = governed(rooms).await;
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
    let refused = |answer: Result<Value, ActionHostError>| {
        matches!(answer, Err(ActionHostError::Refused { .. }))
    };
    assert!(
        refused(invoke(&host, workspace, &bo, share(notes, &window_share_id(notes))).await),
        "Bo can't share a window of Ada's device"
    );
    assert!(
        refused(invoke(&host, workspace, &ada, share(notes, "m1")).await),
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
        refused(
            invoke(
                &host,
                workspace,
                &ada,
                share(elsewhere, &window_share_id(elsewhere))
            )
            .await
        ),
        "nor a window in a namespace no device holds"
    );
    let held_room = room.as_str();
    let shared_now = || async move {
        let state = rooms.room(held_room).await.unwrap();
        let held = state.lock().await;
        let doc = held.peer().admitted_room().unwrap();
        window_shares(
            &tilo_desktop_model::projection::room_policy_of(doc, held_room),
            &tilo_desktop_model::window_share::device_salts(doc),
            tilo_clock::unix_now_ms(),
        )
    };
    assert!(shared_now().await.is_empty());

    invoke(
        &host,
        workspace,
        &ada,
        share(notes, &window_share_id(notes)),
    )
    .await
    .unwrap();
    let shares = shared_now().await;
    let shared = shares.get(&notes).expect("Ada's Notes is shared");
    assert_eq!(shared.host, hex::encode(ada.device_public_key));
    assert_eq!(shared.sharer, hex::encode(ada.user_public_key));
    assert_eq!(shared.app_id, "so.tilo.notes");

    invoke(
        &host,
        workspace,
        &ada,
        member_request(
            &room,
            UNSHARE_RESOURCE,
            json!({ "share": window_share_id(notes) }),
        ),
    )
    .await
    .unwrap();
    assert!(shared_now().await.is_empty(), "the share ended");
    relay.shutdown().await;
}

/// What a Tilo app's shared window shows is shared beside the window, under
/// its share id, by the device the window runs on alone; ending the
/// window's share ends it too.
#[tokio::test]
async fn what_a_shared_window_shows_is_shared_by_its_device_and_ends_with_it() {
    use tilo_desktop_model::window_share::{
        WindowShows, shows_node, window_node, window_share_id, window_shares, window_shows_share_id,
    };

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let rooms = relay.rooms();
    let (room, workspace, host) = governed(rooms).await;
    let ada = admit(rooms, &room, &owner, 1).await;
    let bo = admit(rooms, &room, &owner, 2).await;
    // Ada's device mints its windows under the salt its key derives.
    let salt = tilo_desktop_model::ids::device_salt_of_key(&ada.device_public_key);
    let notes = (u64::from(salt) << 48) | 4;
    let note = WindowShows {
        record: String::new(),
        item: Some("notes.page/pages/7".into()),
    };
    let request = |id: String, node: tilo_action_policy::TreePath| {
        member_request(
            &room,
            SHARE_RESOURCE,
            json!({ "share": id, "node": node.to_string(), "access": "view" }),
        )
    };
    let shows = || {
        request(
            window_shows_share_id(notes),
            shows_node(&room, "so.tilo.notes", notes, "notes.notebook", &note),
        )
    };
    let refused = |answer: Result<Value, ActionHostError>| {
        matches!(answer, Err(ActionHostError::Refused { .. }))
    };
    assert!(
        refused(invoke(&host, workspace, &bo, shows()).await),
        "Bo can't say what a window of Ada's device shows"
    );
    assert!(
        refused(
            invoke(
                &host,
                workspace,
                &ada,
                request(
                    window_share_id(notes),
                    shows_node(&room, "so.tilo.notes", notes, "notes.notebook", &note),
                ),
            )
            .await
        ),
        "the window's own share id is the window's node alone"
    );
    invoke(&host, workspace, &ada, shows()).await.unwrap();
    invoke(
        &host,
        workspace,
        &ada,
        request(
            window_share_id(notes),
            window_node(&room, "so.tilo.notes", notes),
        ),
    )
    .await
    .unwrap();
    let held_room = room.as_str();
    let shared_now = || async move {
        let state = rooms.room(held_room).await.unwrap();
        let held = state.lock().await;
        let doc = held.peer().admitted_room().unwrap();
        window_shares(
            &tilo_desktop_model::projection::room_policy_of(doc, held_room),
            &tilo_desktop_model::window_share::device_salts(doc),
            tilo_clock::unix_now_ms(),
        )
    };
    assert_eq!(
        shared_now()
            .await
            .get(&notes)
            .and_then(|share| share.shows.clone()),
        Some(note),
        "the room's record says which note Ada's Notes window is at"
    );

    // Ada's window moves to another note: sharing what it shows again, under
    // the same share id, is the room's record of where it is now, in place
    // of where it was.
    let moved = WindowShows {
        record: String::new(),
        item: Some("notes.page/pages/8".into()),
    };
    let grants_of_shows = || async move {
        let state = rooms.room(held_room).await.unwrap();
        let held = state.lock().await;
        let doc = held.peer().admitted_room().unwrap();
        tilo_desktop_model::projection::room_policy_of(doc, held_room)
            .grants
            .iter()
            .filter(|grant| {
                tilo_action_policy::share::is_of_share(grant, &window_shows_share_id(notes))
            })
            .count()
    };
    let before = grants_of_shows().await;
    invoke(
        &host,
        workspace,
        &ada,
        request(
            window_shows_share_id(notes),
            shows_node(&room, "so.tilo.notes", notes, "notes.notebook", &moved),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        shared_now()
            .await
            .get(&notes)
            .and_then(|share| share.shows.clone()),
        Some(moved),
        "the room's record follows Ada's window to the other note"
    );
    assert_eq!(
        grants_of_shows().await,
        before,
        "the same share, written over: never two notes at once"
    );

    invoke(
        &host,
        workspace,
        &ada,
        member_request(
            &room,
            UNSHARE_RESOURCE,
            json!({ "share": window_share_id(notes) }),
        ),
    )
    .await
    .unwrap();
    let state = rooms.room(held_room).await.unwrap();
    let held = state.lock().await;
    let doc = held.peer().admitted_room().unwrap();
    assert!(
        !tilo_desktop_model::projection::room_policy_of(doc, held_room)
            .grants
            .iter()
            .any(|grant| tilo_action_policy::share::is_of_share(grant, &window_share_id(notes))),
        "ending the window's share ended what it shows"
    );
    drop(held);
    relay.shutdown().await;
}
