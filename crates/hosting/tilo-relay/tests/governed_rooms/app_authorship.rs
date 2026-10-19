//! Opening an app for the first time in a room a home relay governs: the
//! relay's person asks, as the host's own `tilo.host` action on the room's
//! session, to let the app author the room's records, and every member's
//! replica verifies the change.
use super::governance_actions::{ask, host_operation, outcome, subscribed};
use super::*;
use tilo_actions::remote::{ActionHostError, InvokeArgument, RemoteActionMessage};
use tilo_relay::rooms::GRANT_APP_AUTHORSHIP;

const OPENED: &str = "so.tilo.notes";

fn authors(peer: &StatePeer, member: &TestDevice) -> bool {
    let now = SystemTime::now();
    peer.permissions(member.device_pubkey(), now)
        .is_ok_and(|member| {
            member
                .policy
                .app_authors(OPENED, &tilo_action_policy::Context::at_time(now))
        })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hosts_person_grants_app_authorship_through_the_hosts_action() {
    let alice = TestUser::fresh("Fixture governor");
    let bob = TestUser::fresh("Fixture editor");
    let host = alice.enrol("private-host", DeviceCapabilities::host());
    let laptop = alice.enrol("private-laptop", DeviceCapabilities::primary());
    let member = bob.enrol("private-member", DeviceCapabilities::member());
    let scratch = tempfile::tempdir().unwrap();
    let relay = start(scratch.path(), &alice, &host).await;
    let room = create_and_enter(&relay, &host, &laptop, "Private app opening").await;
    let workspace = room.workspace_id;
    topology_namespace::admit(&relay, &host, &member, &room.room).await;
    let grant = |app: &str| {
        host_operation(
            GRANT_APP_AUTHORSHIP,
            serde_json::json!({ "room": room.room, "app": app }),
        )
    };

    // The caller is the device its relay session authenticated: an editor
    // asking is refused as a stranger to the host's operations.
    let editor = subscribed(&relay, &member, workspace).await;
    assert_eq!(
        outcome(ask(&editor, &host, workspace, grant(OPENED)).await),
        Err(ActionHostError::UnknownAction),
        "only the host's person lets an app author the room"
    );
    drop(editor);

    let state_session = session(&relay, &member, workspace).await;
    let mut peer = StatePeer::new(
        &room.room,
        host.cert.clone(),
        vec![host.cert.clone(), laptop.cert.clone(), member.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    peer.select_admission_v5().unwrap();
    let owner = subscribed(&relay, &laptop, workspace).await;
    let first = grant(OPENED);
    let reply = outcome(ask(&owner, &host, workspace, first.clone()).await).unwrap();
    assert!(reply["generation"].as_u64().is_some(), "{reply}");
    receive_until(
        "a member verifies the signed app authorship",
        &state_session,
        &mut peer,
        &mut Vec::new(),
        |peer, _| authors(peer, &member),
    )
    .await;
    assert_eq!(
        outcome(ask(&owner, &host, workspace, first.clone()).await).unwrap(),
        reply,
        "an exact retry answers the change it committed"
    );
    let RemoteActionMessage::Invoke(mut other) = first else {
        unreachable!("a grant is an invocation")
    };
    let serde_json::Value::Object(argument) =
        serde_json::json!({ "room": room.room, "app": "so.tilo.file-explorer" })
    else {
        unreachable!("an object")
    };
    other.argument = InvokeArgument::Json(argument);
    assert!(
        outcome(ask(&owner, &host, workspace, RemoteActionMessage::Invoke(other)).await).is_err(),
        "a retry naming another app is refused"
    );
    relay.shutdown().await;
}

// USER REQUIREMENT (2026-10-19): "Joined members of a community's public home room get authorship for the room's home app (Chat) by default. The host can configure otherwise, but the default is that they can talk."
/// The persistent room's host settles its apps' authorship at every start
/// (`--author-app`): what it settled survives a restart, settling the same
/// again changes nothing, and an app the host takes out of its list stops
/// authoring the room for good.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settled_app_authorship_survives_a_restart_and_is_withdrawn_for_good() {
    use std::collections::BTreeSet;
    const CHAT: &str = "so.tilo.chat";
    let alice = TestUser::fresh("Lounge host's person");
    let host = alice.enrol("lounge-host", DeviceCapabilities::host());
    let scratch = tempfile::tempdir().unwrap();
    let chat = BTreeSet::from([CHAT.to_owned()]);
    let none = BTreeSet::new();

    let relay = start(scratch.path(), &alice, &host).await;
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Lounge").await.unwrap().room;
    assert!(rooms.app_authors(&room).await.unwrap().is_empty());
    assert!(
        rooms
            .settle_app_authorship(&room, &host.cert, &chat, &none)
            .await
            .unwrap()
    );
    assert_eq!(rooms.app_authors(&room).await.unwrap(), chat);
    relay.shutdown().await;

    // The next start settles the same: the room already holds it.
    let relay = start(scratch.path(), &alice, &host).await;
    let rooms = relay.rooms().unwrap();
    assert_eq!(
        rooms.app_authors(&room).await.unwrap(),
        chat,
        "the grant is the room's governance, kept across a restart"
    );
    assert!(
        !rooms
            .settle_app_authorship(&room, &host.cert, &chat, &none)
            .await
            .unwrap(),
        "settling what the room holds commits nothing"
    );
    // The host configured otherwise: Chat stops authoring.
    assert!(
        rooms
            .settle_app_authorship(&room, &host.cert, &none, &chat)
            .await
            .unwrap()
    );
    assert!(rooms.app_authors(&room).await.unwrap().is_empty());
    relay.shutdown().await;

    let relay = start(scratch.path(), &alice, &host).await;
    let rooms = relay.rooms().unwrap();
    assert!(
        rooms.app_authors(&room).await.unwrap().is_empty(),
        "a withdrawn authorship stays withdrawn after a restart"
    );
    // And the host can give it back.
    assert!(
        rooms
            .settle_app_authorship(&room, &host.cert, &chat, &none)
            .await
            .unwrap()
    );
    assert_eq!(rooms.app_authors(&room).await.unwrap(), chat);
    relay.shutdown().await;
}
