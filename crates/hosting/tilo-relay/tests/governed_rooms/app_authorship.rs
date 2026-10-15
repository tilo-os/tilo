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
