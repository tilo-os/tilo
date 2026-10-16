//! A member claims their own topology namespace as the relay's `tilo.member`
//! action; the room signs and retains the claim.
use super::governance_actions::{admitted, ask, host_operation, invocation, outcome, subscribed};
use super::*;

/// Admit `member`'s person to `room` as an Editor, through an invitation
/// `host` issues and `member`'s device redeems.
pub(super) async fn admit(relay: &Relay, host: &TestDevice, member: &TestDevice, room: &str) {
    let issued = relay
        .rooms()
        .unwrap()
        .issue(
            room,
            &host.cert,
            tilo_relay::rooms::IssueInvitationRequest {
                // One per member, so a room admits several this way.
                request_id: *blake3::hash(&member.device_pubkey()).as_bytes(),
                role: MemberRole::Editor,
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

fn owns(peer: &StatePeer, salt: u16, member: &TestDevice) -> bool {
    let Some(doc) = peer.admitted_room() else {
        return false;
    };
    let Some((_, owners)) = doc.get(ROOT, "contents_owners_v1").unwrap() else {
        return false;
    };
    doc.get(owners, format!("{salt:04x}"))
        .unwrap()
        .is_some_and(|(value, _)| {
            value.as_str() == Some(hex::encode(member.device_pubkey()).as_str())
        })
}

/// Claiming the topology namespace `salt`, as the member's own action in
/// `room`.
fn claim(room: &str, salt: u16) -> tilo_actions::remote::RemoteActionMessage {
    invocation(
        tilo_relay::rooms::MEMBER_ACTIONS_PROVIDER,
        tilo_relay::rooms::member_resource(room),
        tilo_relay::rooms::CLAIM_TOPOLOGY_NAMESPACE,
        serde_json::json!({ "salt": salt }),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn namespace_claim_requires_exact_current_writer_and_reaches_signed_replicas() {
    let alice = TestUser::fresh("Fixture governor");
    let bob = TestUser::fresh("Fixture editor");
    let carol = TestUser::fresh("Fixture second editor");
    let host = alice.enrol("private-host", DeviceCapabilities::host());
    let member = bob.enrol("private-member", DeviceCapabilities::member());
    let other = carol.enrol("private-other", DeviceCapabilities::member());
    let scratch = tempfile::tempdir().unwrap();
    let relay = start(scratch.path(), &alice, &host).await;
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Private topology claim").await.unwrap();
    let workspace = room.workspace_id;
    assert!(
        !admitted(&relay, &member, workspace).await,
        "certificate alone grants no namespace"
    );
    admit(&relay, &host, &member, &room.room).await;

    // The member claims; a retry answers the committed generation, and the
    // claim grants no administration.
    let client = subscribed(&relay, &member, workspace).await;
    let reply = outcome(ask(&client, &host, workspace, claim(&room.room, 42)).await).unwrap();
    assert!(reply["generation"].as_u64().is_some(), "{reply}");
    assert_eq!(
        outcome(ask(&client, &host, workspace, claim(&room.room, 42)).await).unwrap(),
        reply,
        "retry keeps committed generation"
    );
    assert_eq!(
        outcome(
            ask(
                &client,
                &host,
                workspace,
                host_operation(tilo_relay::rooms::LIST_ROOMS, serde_json::json!({}))
            )
            .await
        ),
        Err(tilo_actions::remote::ActionHostError::UnknownAction),
        "namespace confers no administration"
    );
    drop(client);

    // The member's own replica verifies the signed publication.
    let state_session = session(&relay, &member, workspace).await;
    let mut peer = StatePeer::new(
        &room.room,
        host.cert.clone(),
        vec![host.cert.clone(), member.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    peer.select_admission_v5().unwrap();
    receive_until(
        "member verifies signed namespace publication",
        &state_session,
        &mut peer,
        &mut Vec::new(),
        |peer, _| owns(peer, 42, &member),
    )
    .await;
    state_session.close().await;

    // Another writer cannot take the namespace over.
    admit(&relay, &host, &other, &room.room).await;
    let theirs = subscribed(&relay, &other, workspace).await;
    assert!(
        outcome(ask(&theirs, &host, workspace, claim(&room.room, 42)).await).is_err(),
        "namespace never rebinds"
    );
    drop(theirs);

    // A retry is decided again: after the member's Write narrows, and after
    // their removal.
    rooms
        .set_member_role(
            &room.room,
            &host.cert,
            tilo_relay::rooms::SetMemberRoleRequest {
                request_id: [73; 32],
                user_public_key: bob.user_pk,
                role: MemberRole::Viewer,
            },
        )
        .await
        .unwrap();
    let client = subscribed(&relay, &member, workspace).await;
    assert!(
        outcome(ask(&client, &host, workspace, claim(&room.room, 42)).await).is_err(),
        "retry rechecks narrowed Write"
    );
    rooms
        .remove_member(
            &room.room,
            &host.cert,
            RemoveMemberRequest {
                request_id: [74; 32],
                user_public_key: bob.user_pk,
            },
        )
        .await
        .unwrap();
    assert!(
        !matches!(
            super::governance_actions::try_ask(&client, &host, workspace, claim(&room.room, 42))
                .await
                .map(outcome),
            Ok(Ok(_))
        ),
        "retry rechecks removal"
    );
    relay.shutdown().await;
}
