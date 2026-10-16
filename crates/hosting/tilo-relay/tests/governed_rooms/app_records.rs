//! Opening an app for the first time in a room a home relay governs, after
//! the room lets the app author: a member's device asks the relay, as its own
//! `tilo.member` action, for the app's record; the relay signs its empty
//! genesis, and the app's first state reaches the record as the member's own
//! change, which every replica admits.
use super::governance_actions::{
    admitted, ask, host_operation, invocation, outcome, subscribed, try_ask,
};
use super::*;
use tilo_actions::remote::ActionHostError;
use tilo_relay::rooms::{GRANT_APP_AUTHORSHIP, PREPARE_APPLICATION_RECORD};

const OPENED: &str = "so.tilo.notes";

/// Asking for `app`'s primary record in `room`, as the member's own action.
fn prepare(room: &str, app: &str) -> tilo_actions::remote::RemoteActionMessage {
    invocation(
        tilo_relay::rooms::MEMBER_ACTIONS_PROVIDER,
        tilo_relay::rooms::member_resource(room),
        PREPARE_APPLICATION_RECORD,
        serde_json::json!({ "app": app }),
    )
}

fn first_state(peer: &StatePeer) -> Option<String> {
    let doc = AutoCommit::load(&peer.document(OPENED, "")?).ok()?;
    doc.get(ROOT, "title")
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

/// Admit `member`'s person to `room` as a Viewer, through an invitation the
/// relay issues and `member`'s device redeems.
async fn admit_viewer(relay: &Relay, host: &TestDevice, member: &TestDevice, room: &str) {
    let issued = relay
        .rooms()
        .unwrap()
        .issue(
            room,
            &host.cert,
            tilo_relay::rooms::IssueInvitationRequest {
                request_id: *blake3::hash(&member.device_pubkey()).as_bytes(),
                role: MemberRole::Viewer,
                recipient_user_public_key: Some(member.cert.user_public_key),
                invitee_name: None,
                inviter_name: None,
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(60_000_000_000)
                    .expect("fixture expiry fits"),
            },
        )
        .await
        .unwrap();
    let verified = decode_and_verify_invitation(
        &issued.signed_invitation,
        Some(member.cert.user_public_key),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    WorkspaceJoinClient::new(device_endpoint(member).await)
        .redeem_hint(
            &verified.grant().transport[0],
            issued.signed_invitation,
            member.cert.clone(),
            &member.sk,
            *blake3::hash(&member.cert.user_public_key).as_bytes(),
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admitted_member_has_the_relay_create_an_authored_apps_empty_record() {
    let alice = TestUser::fresh("Fixture governor");
    let bob = TestUser::fresh("Fixture editor");
    let carol = TestUser::fresh("Fixture viewer");
    let dave = TestUser::fresh("Fixture outsider");
    let host = alice.enrol("private-host", DeviceCapabilities::host());
    let laptop = alice.enrol("private-laptop", DeviceCapabilities::primary());
    let member = bob.enrol("private-member", DeviceCapabilities::member());
    let viewer = carol.enrol("private-viewer", DeviceCapabilities::member());
    let outsider = dave.enrol("private-outsider", DeviceCapabilities::member());
    let scratch = tempfile::tempdir().unwrap();
    let relay = start(scratch.path(), &alice, &host).await;
    let room = create_and_enter(&relay, &host, &laptop, "Private app records").await;
    let workspace = room.workspace_id;
    topology_namespace::admit(&relay, &host, &member, &room.room).await;
    admit_viewer(&relay, &host, &viewer, &room.room).await;

    let editor = subscribed(&relay, &member, workspace).await;
    assert!(
        matches!(
            outcome(ask(&editor, &host, workspace, prepare(&room.room, OPENED)).await),
            Err(ActionHostError::Refused { .. })
        ),
        "a record of an app the room does not let author is never created"
    );
    let owner = subscribed(&relay, &laptop, workspace).await;
    outcome(
        ask(
            &owner,
            &host,
            workspace,
            host_operation(
                GRANT_APP_AUTHORSHIP,
                serde_json::json!({ "room": room.room, "app": OPENED }),
            ),
        )
        .await,
    )
    .unwrap();
    drop(owner);

    assert!(
        !admitted(&relay, &outsider, workspace).await,
        "a device the room never admitted cannot reach the action"
    );
    // A viewer's session may not act in the workspace at all; were its
    // action delivered, the room would refuse it as a change it may not make.
    let reader = subscribed(&relay, &viewer, workspace).await;
    let refused = match try_ask(&reader, &host, workspace, prepare(&room.room, OPENED)).await {
        Err(reason) => reason.contains("may not act"),
        Ok(answer) => matches!(outcome(answer), Err(ActionHostError::Refused { .. })),
    };
    assert!(
        refused,
        "a member who may not change the record creates nothing"
    );
    drop(reader);
    assert!(
        outcome(
            ask(
                &editor,
                &host,
                workspace,
                prepare(&room.room, "so.tilo.notes\n")
            )
            .await
        )
        .is_err(),
        "a malformed application id is refused"
    );

    let reply = outcome(ask(&editor, &host, workspace, prepare(&room.room, OPENED)).await).unwrap();
    assert_eq!(reply["created"], serde_json::json!(true), "{reply}");
    let generation = reply["generation"].as_u64().expect("a generation");
    assert_eq!(
        outcome(ask(&editor, &host, workspace, prepare(&room.room, OPENED)).await).unwrap(),
        serde_json::json!({ "created": false, "generation": generation }),
        "a retry finds the record and changes nothing"
    );
    // A device's newest session is its current one: the replicas' sessions
    // below replace this one.
    drop(editor);

    let member_session = session(&relay, &member, workspace).await;
    let laptop_session = session(&relay, &laptop, workspace).await;
    let roster = vec![
        host.cert.clone(),
        laptop.cert.clone(),
        member.cert.clone(),
        viewer.cert.clone(),
    ];
    let replica = || {
        let mut peer = StatePeer::new(
            &room.room,
            host.cert.clone(),
            roster.clone(),
            RevocationList::new(),
            SystemTime::now(),
        )
        .unwrap();
        peer.select_admission_v5().unwrap();
        peer
    };
    let mut member_peer = replica();
    let mut laptop_peer = replica();
    receive_until(
        "the member holds the relay's empty genesis",
        &member_session,
        &mut member_peer,
        &mut Vec::new(),
        |peer, _| peer.document(OPENED, "").is_some(),
    )
    .await;
    let mut genesis = AutoCommit::load(&member_peer.document(OPENED, "").unwrap()).unwrap();
    assert!(
        genesis.get_heads().is_empty(),
        "the relay signs an empty document and nothing else"
    );

    // The app's first state is the member's own change onto the genesis.
    crate::common::wait_until_subscribed(&relay, workspace, &member.device_pubkey()).await;
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "title", "First note").unwrap();
    doc.commit_edit("fixture", "title");
    let bootstrap = member_peer
        .publish_incremental(
            OPENED,
            "",
            doc.save_after(&[]),
            &member.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&member_session, &bootstrap);
    receive_until(
        "another member's replica admits the member's first state",
        &laptop_session,
        &mut laptop_peer,
        &mut Vec::new(),
        |peer, _| first_state(peer).as_deref() == Some("First note"),
    )
    .await;
    member_session.close().await;
    let editor = subscribed(&relay, &member, workspace).await;
    let retry = outcome(ask(&editor, &host, workspace, prepare(&room.room, OPENED)).await).unwrap();
    assert_eq!(
        retry["created"],
        serde_json::json!(false),
        "a written record is never replaced: {retry}"
    );
    relay.shutdown().await;
}
