//! The room's own changes as governance actions: each decided by the
//! caller's standing in the room and written into the governance every
//! member reads.

use serde_json::{Value, json};
use tilo_action_policy::roles::governance;
use tilo_action_policy::{Answer, KindSet, Subject};
use tilo_actions::remote::{
    ActionHost as _, ActionHostError, Caller, CatalogueQuery, InvokeArgument, InvokeRequest,
    ROOM_GOVERNANCE_PROVIDER, ReceiptStatus,
};
use tilo_desktop_model::projection::{RoomLayout, room_sandbox_of, workspace_presets_of};
use tilo_peer_state::join::MemberRole;
use tilo_user_identity::DeviceCert;

use super::{
    ASSIGN_PRESET, Args, GRANT_APP_ACTION, GRANT_SANDBOX, OWNER_VERBS, RENAME, REVOKE_APP_ACTION,
    REVOKE_SANDBOX, SAVE_PRESET, command_of,
};
use crate::rooms::actions::HostRoomActions;
use crate::rooms::actions::tests::{admit, caller, governed, request, settled};
use crate::rooms::tests::{governor, test_host};
use crate::rooms::{OwnerCommand, Rooms, SetMemberRoleRequest};

fn args(value: Value) -> Args {
    let Value::Object(map) = value else {
        panic!("arguments are an object");
    };
    Args::of(&InvokeArgument::Json(map)).unwrap()
}

/// `request` by `caller`: what it produced, or why not.
async fn invoke(
    host: &HostRoomActions,
    workspace_id: u64,
    caller: &Caller,
    request: InvokeRequest,
) -> Result<Value, ActionHostError> {
    let receipt = settled(host.in_workspace(workspace_id).invoke(caller, request)).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("governance completes at once: {other:?}"),
    }
}

/// The room's own changes `caller` is offered.
async fn offered(host: &HostRoomActions, workspace_id: u64, caller: &Caller) -> Vec<String> {
    let query = CatalogueQuery {
        provider: Some(ROOM_GOVERNANCE_PROVIDER.into()),
        max_results: 100,
        ..CatalogueQuery::default()
    };
    settled(host.in_workspace(workspace_id).query(caller, query))
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter_map(|row| row.action.map(|action| action.address.action))
        .filter(|action| OWNER_VERBS.iter().any(|verb| verb.id == action))
        .collect()
}

/// What `room`'s governor has committed, read as members read it.
async fn committed<T>(
    rooms: &Rooms,
    room: &str,
    read: impl FnOnce(&automerge::AutoCommit) -> T,
) -> T {
    let governed = rooms.room(room).await.unwrap();
    let governed = governed.lock().await;
    read(
        governed
            .peer()
            .admitted_room()
            .expect("the room has governance"),
    )
}

async fn make_moderator(rooms: &Rooms, room: &str, owner: &DeviceCert, person: &DeviceCert) {
    rooms
        .set_member_role(
            room,
            owner,
            SetMemberRoleRequest {
                request_id: [person.user_public_key[0]; 32],
                user_public_key: person.user_public_key,
                role: MemberRole::Moderator,
            },
        )
        .await
        .unwrap();
}

fn person(cert: &DeviceCert) -> String {
    hex::encode(cert.user_public_key)
}

#[tokio::test]
async fn the_owner_and_moderators_rename_the_workspace_and_editors_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let rooms = relay.rooms();
    let (room, workspace, host) = governed(rooms).await;
    let ada = admit(rooms, &room, &owner, 1).await;
    let bo = admit(rooms, &room, &owner, 2).await;
    make_moderator(rooms, &room, &owner, &bo).await;

    invoke(
        &host,
        workspace,
        &caller(&owner),
        request(&room, RENAME, json!({ "name": "  Design  " })),
    )
    .await
    .unwrap();
    assert_eq!(rooms.summary(&room).await.unwrap().name, "Design");
    let governed = rooms.room(&room).await.unwrap();
    assert_eq!(
        governed.lock().await.peer().governance().room_name(),
        Some("Design".to_owned()),
        "members read the name from the room's governance"
    );

    invoke(
        &host,
        workspace,
        &caller(&bo),
        request(&room, RENAME, json!({ "name": "Review" })),
    )
    .await
    .unwrap();
    assert_eq!(rooms.summary(&room).await.unwrap().name, "Review");

    // An Editor is neither offered renaming nor let do it.
    assert!(
        !offered(&host, workspace, &caller(&ada))
            .await
            .contains(&RENAME.to_owned())
    );
    assert!(matches!(
        invoke(
            &host,
            workspace,
            &caller(&ada),
            request(&room, RENAME, json!({ "name": "Mine" })),
        )
        .await,
        Err(ActionHostError::UnknownAction)
    ));

    // A room that shares no member's layout is named by its owner alone.
    rooms.set_layout(&room, RoomLayout::Personal).await.unwrap();
    assert!(
        !offered(&host, workspace, &caller(&bo))
            .await
            .contains(&RENAME.to_owned())
    );
    invoke(
        &host,
        workspace,
        &caller(&owner),
        request(&room, RENAME, json!({ "name": "Lounge" })),
    )
    .await
    .unwrap();
    assert_eq!(rooms.summary(&room).await.unwrap().name, "Lounge");
    relay.shutdown().await;
}

#[tokio::test]
async fn a_saved_grant_set_is_assigned_as_the_grants_it_expands_to() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let rooms = relay.rooms();
    let (room, workspace, host) = governed(rooms).await;
    let ada = admit(rooms, &room, &owner, 1).await;

    let preset = json!({
        "name": "Readers",
        "description": "May read the notes",
        "rules": [{
            "node": tilo_action_policy::TreePath::root().app("so.tilo.notes"),
            "kinds": KindSet::READ,
            "answer": Answer::Allow,
        }],
    });
    // The shell sends the set as its JSON text.
    invoke(
        &host,
        workspace,
        &caller(&owner),
        request(&room, SAVE_PRESET, json!({ "preset": preset.to_string() })),
    )
    .await
    .unwrap();
    let saved = committed(rooms, &room, workspace_presets_of).await;
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].name, "Readers");

    invoke(
        &host,
        workspace,
        &caller(&owner),
        request(
            &room,
            ASSIGN_PRESET,
            json!({ "preset": "readers", "subject_kind": "person", "subject": person(&ada) }),
        ),
    )
    .await
    .unwrap();
    let grants = committed(rooms, &room, |doc| {
        tilo_desktop_model::projection::room_policy_of(doc, &room).grants
    })
    .await;
    let assigned: Vec<_> = grants
        .iter()
        .filter(|grant| grant.subject == Subject::Person(person(&ada)))
        .filter(|grant| grant.id.as_str().starts_with("preset:"))
        .collect();
    assert_eq!(assigned.len(), 1, "{grants:?}");
    assert_eq!(assigned[0].granted_by, Subject::Person(person(&owner)));
    assert_eq!(assigned[0].node.room_id(), Some(room.as_str()));

    // An Editor neither saves nor assigns sets.
    for (verb, argument) in [
        (SAVE_PRESET, json!({ "preset": preset.clone() })),
        (
            ASSIGN_PRESET,
            json!({ "preset": "Readers", "subject_kind": "person", "subject": person(&ada) }),
        ),
    ] {
        assert!(matches!(
            invoke(
                &host,
                workspace,
                &caller(&ada),
                request(&room, verb, argument)
            )
            .await,
            Err(ActionHostError::UnknownAction)
        ));
    }
    relay.shutdown().await;
}

#[tokio::test]
async fn an_app_is_let_use_one_action_of_another_and_that_is_taken_back() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let rooms = relay.rooms();
    let (room, workspace, host) = governed(rooms).await;
    let action = json!({
        "app": "so.tilo.agent",
        "target_app": "so.tilo.notes",
        "resource": "note",
        "action": "edit",
    });
    let app_grants = |grants: Vec<tilo_action_policy::Grant>| {
        grants
            .into_iter()
            .filter(|grant| grant.subject == Subject::App("so.tilo.agent".into()))
            .collect::<Vec<_>>()
    };

    let mut grant = action.clone();
    grant["lifetime"] = json!("always");
    invoke(
        &host,
        workspace,
        &caller(&owner),
        request(&room, GRANT_APP_ACTION, grant),
    )
    .await
    .unwrap();
    let granted = app_grants(
        committed(rooms, &room, |doc| {
            tilo_desktop_model::projection::room_policy_of(doc, &room).grants
        })
        .await,
    );
    assert_eq!(granted.len(), 1);
    assert_eq!(granted[0].kinds, KindSet::READ_UNDOABLE);
    assert_eq!(granted[0].granted_by, Subject::Person(person(&owner)));

    invoke(
        &host,
        workspace,
        &caller(&owner),
        request(&room, REVOKE_APP_ACTION, action),
    )
    .await
    .unwrap();
    assert!(
        app_grants(
            committed(rooms, &room, |doc| {
                tilo_desktop_model::projection::room_policy_of(doc, &room).grants
            })
            .await,
        )
        .is_empty()
    );
    relay.shutdown().await;
}

#[tokio::test]
async fn a_policy_grant_is_recorded_as_given_by_its_asker_who_gives_no_more_than_they_hold() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let rooms = relay.rooms();
    let (room, workspace, host) = governed(rooms).await;
    let ada = admit(rooms, &room, &owner, 1).await;
    let grant = tilo_action_policy::Grant::new(
        "grant:read-notes",
        Subject::Person(person(&ada)),
        tilo_action_policy::TreePath::room(room.clone()).app("so.tilo.notes"),
        KindSet::READ,
        Answer::Allow,
        // Whoever the shell names, the room records the asker.
        Subject::Person(String::new()),
    );

    invoke(
        &host,
        workspace,
        &caller(&owner),
        request(&room, governance::GRANT, json!({ "grant": grant })),
    )
    .await
    .unwrap();
    let recorded = committed(rooms, &room, |doc| {
        tilo_desktop_model::projection::room_policy_of(doc, &room).grants
    })
    .await
    .into_iter()
    .find(|held| held.id.as_str() == "grant:read-notes")
    .expect("the grant is recorded");
    assert_eq!(recorded.granted_by, Subject::Person(person(&owner)));

    // An Editor holds no grant action, so may give nothing.
    assert!(matches!(
        invoke(
            &host,
            workspace,
            &caller(&ada),
            request(&room, governance::GRANT, json!({ "grant": grant })),
        )
        .await,
        Err(ActionHostError::UnknownAction)
    ));

    invoke(
        &host,
        workspace,
        &caller(&owner),
        request(
            &room,
            governance::REVOKE,
            json!({ "grant": "grant:read-notes" }),
        ),
    )
    .await
    .unwrap();
    assert!(
        committed(rooms, &room, |doc| {
            tilo_desktop_model::projection::room_policy_of(doc, &room).grants
        })
        .await
        .iter()
        .all(|held| held.id.as_str() != "grant:read-notes")
    );
    relay.shutdown().await;
}

#[tokio::test]
async fn sandbox_capabilities_are_the_rooms_and_only_lasting_ones_are_written() {
    use collab_types::caps::{Access, CapOverride, Expiry, Principal, Scope};
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let rooms = relay.rooms();
    let (room, workspace, host) = governed(rooms).await;
    let camera = CapOverride::grant(
        collab_types::caps::Subject::Principal(Principal::App("so.tilo.camera".into())),
        Scope::Device("camera".into()),
        Access::Read,
    );

    let granted = request(&room, GRANT_SANDBOX, json!({ "capability": camera }));
    let first = invoke(&host, workspace, &caller(&owner), granted.clone())
        .await
        .unwrap();
    // A retry of the same invocation is the same request to the room.
    let again = invoke(&host, workspace, &caller(&owner), granted)
        .await
        .unwrap();
    assert_eq!(first, again);
    let sandbox = committed(rooms, &room, room_sandbox_of).await;
    assert_eq!(sandbox.len(), 1);
    assert!(sandbox[0].id.starts_with("cap-"));
    assert_eq!(
        sandbox[0].granted_by,
        Some(Principal::Person(person(&owner)))
    );

    let session = camera.clone().expiring(Expiry::EndOfSession);
    assert!(matches!(
        invoke(
            &host,
            workspace,
            &caller(&owner),
            request(&room, GRANT_SANDBOX, json!({ "capability": session })),
        )
        .await,
        Err(ActionHostError::Refused { .. })
    ));

    invoke(
        &host,
        workspace,
        &caller(&owner),
        request(
            &room,
            REVOKE_SANDBOX,
            json!({ "capability": sandbox[0].id.clone() }),
        ),
    )
    .await
    .unwrap();
    assert!(committed(rooms, &room, room_sandbox_of).await.is_empty());
    relay.shutdown().await;
}

#[test]
fn a_grant_set_arrives_as_its_json_text_or_as_an_object() {
    let preset = json!({
        "name": "Reviewers",
        "description": "",
        "rules": [],
    });
    for given in [preset.clone(), json!(preset.to_string())] {
        let Ok(OwnerCommand::SavePreset { preset }) =
            command_of(SAVE_PRESET, &args(json!({ "preset": given })))
        else {
            panic!("a grant set decodes");
        };
        assert_eq!(preset.name, "Reviewers");
    }
}

#[test]
fn an_app_action_granted_for_a_session_is_refused_before_it_reaches_the_room() {
    let refused = command_of(
        GRANT_APP_ACTION,
        &args(json!({
            "app": "so.tilo.agent",
            "target_app": "so.tilo.notes",
            "resource": "note",
            "action": "edit",
            "lifetime": "session",
        })),
    );
    assert!(matches!(
        refused,
        Err(ActionHostError::InvalidRequest { .. })
    ));
}

#[test]
fn every_owner_action_is_distinct_from_the_rooms_other_governance() {
    for verb in &OWNER_VERBS {
        assert!(
            super::super::VERBS.iter().all(|other| other.id != verb.id),
            "{} is served twice",
            verb.id
        );
    }
}
