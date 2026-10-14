//! Opening an app for the first time in a room a home relay governs: the
//! relay's person asks through the room-admin door to let the app author the
//! room's records, and every member's replica verifies the change.
use super::*;
use tilo_relay::rooms::GrantAppAuthorshipRequest;

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
async fn the_hosts_person_grants_app_authorship_through_the_room_admin_door() {
    let alice = TestUser::fresh("Fixture governor");
    let bob = TestUser::fresh("Fixture editor");
    let host = alice.enrol("private-host", DeviceCapabilities::host());
    let laptop = alice.enrol("private-laptop", DeviceCapabilities::primary());
    let member = bob.enrol("private-member", DeviceCapabilities::member());
    let scratch = tempfile::tempdir().unwrap();
    let relay = start(scratch.path(), &alice, &host).await;
    let address = relay.node_addr();
    let admin = RoomAdminClient::new(device_endpoint(&laptop).await, laptop.cert.clone());
    let room = admin
        .create(address.clone(), "Private app opening")
        .await
        .unwrap();
    admin
        .send(
            address.clone(),
            RoomCommand::Enter {
                room: room.room.clone(),
            },
        )
        .await
        .unwrap();
    topology_namespace::admit(&relay, &host, &member, &room.room).await;
    let grant = |request_id, app: &str| RoomCommand::GrantAppAuthorship {
        room: room.room.clone(),
        request: GrantAppAuthorshipRequest {
            request_id,
            app: app.into(),
        },
    };

    let editor = RoomAdminClient::new(device_endpoint(&member).await, member.cert.clone());
    assert!(
        editor
            .send(address.clone(), grant([81; 32], OPENED))
            .await
            .is_err(),
        "only the host's person lets an app author the room"
    );
    let mismatch = RoomAdminClient::new(device_endpoint(&member).await, laptop.cert.clone());
    assert!(
        mismatch
            .send(address.clone(), grant([81; 32], OPENED))
            .await
            .is_err(),
        "the caller must be the authenticated QUIC endpoint"
    );

    let state_session = session(&relay, &member, room.workspace_id).await;
    let mut peer = StatePeer::new(
        &room.room,
        host.cert.clone(),
        vec![host.cert.clone(), laptop.cert.clone(), member.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    peer.select_admission_v5().unwrap();
    let reply = admin
        .send(address.clone(), grant([82; 32], OPENED))
        .await
        .unwrap();
    assert!(matches!(reply, RoomReply::Governance { .. }), "{reply:?}");
    receive_until(
        "a member verifies the signed app authorship",
        &state_session,
        &mut peer,
        &mut Vec::new(),
        |peer, _| authors(peer, &member),
    )
    .await;
    assert_eq!(
        admin
            .send(address.clone(), grant([82; 32], OPENED))
            .await
            .unwrap(),
        reply,
        "an exact retry answers the change it committed"
    );
    assert!(
        admin
            .send(address, grant([82; 32], "so.tilo.file-explorer"))
            .await
            .is_err(),
        "a retry naming another app is refused"
    );
    relay.shutdown().await;
}
