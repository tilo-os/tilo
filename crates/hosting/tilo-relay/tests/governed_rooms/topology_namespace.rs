//! The actual endpoint-bound room service must sign and retain namespace claims.
use super::*;

async fn admit(relay: &Relay, host: &TestDevice, member: &TestDevice, room: &str) {
    let issued = relay
        .rooms()
        .unwrap()
        .issue(
            room,
            &host.cert,
            tilo_relay::rooms::IssueInvitationRequest {
                request_id: [71; 32],
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
            [72; 32],
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn namespace_claim_requires_exact_current_writer_and_reaches_signed_replicas() {
    let alice = TestUser::fresh("Fixture governor");
    let bob = TestUser::fresh("Fixture editor");
    let host = alice.enrol("private-host", DeviceCapabilities::host());
    let member = bob.enrol("private-member", DeviceCapabilities::member());
    let scratch = tempfile::tempdir().unwrap();
    let relay = start(scratch.path(), &alice, &host).await;
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Private topology claim").await.unwrap();
    let client = RoomAdminClient::new(device_endpoint(&member).await, member.cert.clone());
    let command = || RoomCommand::ClaimTopologyNamespace {
        room: room.room.clone(),
        salt: 42,
    };
    assert!(
        client.send(relay.node_addr(), command()).await.is_err(),
        "certificate alone grants no namespace"
    );
    admit(&relay, &host, &member, &room.room).await;
    let state_session = session(&relay, &member, room.workspace_id).await;
    let mut peer = StatePeer::new(
        &room.room,
        host.cert.clone(),
        vec![host.cert.clone(), member.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    peer.select_admission_v5().unwrap();
    let reply = client.send(relay.node_addr(), command()).await.unwrap();
    assert!(matches!(reply, RoomReply::Governance { .. }));
    receive_until(
        "member verifies signed namespace publication",
        &state_session,
        &mut peer,
        &mut Vec::new(),
        |peer, _| owns(peer, 42, &member),
    )
    .await;
    assert_eq!(
        client.send(relay.node_addr(), command()).await.unwrap(),
        reply,
        "retry keeps committed generation"
    );
    let mismatch = RoomAdminClient::new(device_endpoint(&host).await, member.cert.clone());
    assert!(
        mismatch.send(relay.node_addr(), command()).await.is_err(),
        "caller must match QUIC endpoint"
    );
    assert!(
        client
            .send(relay.node_addr(), RoomCommand::List)
            .await
            .is_err(),
        "namespace confers no administration"
    );
    let host_client = RoomAdminClient::new(device_endpoint(&host).await, host.cert.clone());
    assert!(
        host_client
            .send(relay.node_addr(), command())
            .await
            .is_err(),
        "namespace never rebinds"
    );
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
    assert!(
        client.send(relay.node_addr(), command()).await.is_err(),
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
        client.send(relay.node_addr(), command()).await.is_err(),
        "retry rechecks removal"
    );
    relay.shutdown().await;
}
