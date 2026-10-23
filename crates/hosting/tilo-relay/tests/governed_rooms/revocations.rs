//! A device revoked by its person's root, announced in a governed room by
//! another device of that person, stops at the relay: it is admitted to no
//! room the relay governs, administers none of them, and every member's
//! replica refuses it from the room's governance.
use governance_actions::{HOME_WORKSPACE, admitted, host_action, subscribed};
use tilo_relay::rooms::ENTER_ROOM;
use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};

use super::*;

fn revoke(user: &TestUser, device: &TestDevice) -> RevocationEntry {
    let mut entry = RevocationEntry {
        user_public_key: user.user_pk,
        revoked_device_public_key: device.device_pubkey(),
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason: RevocationReason::Stolen,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &user.user_sk).unwrap();
    entry
}

fn hello(device: &TestDevice, revocations: Vec<RevocationEntry>) -> NeighbourMsg {
    NeighbourMsg::IdentityHello {
        protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
        cert: Box::new(device.cert.clone()),
        revocations,
        successions: Vec::new(),
    }
}

async fn enter(relay: &Relay, host: &TestDevice, device: &TestDevice, room: &str) {
    let home = subscribed(relay, device, HOME_WORKSPACE).await;
    let _: EnteredRoom = host_action(
        &home,
        host,
        HOME_WORKSPACE,
        ENTER_ROOM,
        serde_json::json!({ "room": room }),
    )
    .await;
}

/// `device` of `person` redeems an editor's invitation to `room` that
/// `issuer` issues, and builds its replica from the bootstrap.
async fn join(
    relay: &Relay,
    issuer: &TestDevice,
    room: &str,
    person: &TestUser,
    device: &TestDevice,
) -> StatePeer {
    let invitation = invite(relay, issuer, room, MemberRole::Editor, person.user_pk).await;
    let verified = decode_and_verify_invitation(
        &invitation.signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let bootstrap = WorkspaceJoinClient::new(device_endpoint(device).await)
        .redeem_hint(
            &verified.grant().transport[0],
            invitation.signed_invitation.clone(),
            device.cert.clone(),
            &device.sk,
            *blake3::hash(&device.device_pubkey()).as_bytes(),
        )
        .await
        .unwrap()
        .bootstrap;
    let mut peer = StatePeer::new(
        &bootstrap.room,
        bootstrap.authority.clone(),
        bootstrap.certified_roster.clone(),
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    peer.select_admission_v5().unwrap();
    for bytes in &bootstrap.governance_messages {
        peer.receive_from(
            TransportOrigin::RetainedRelayDelta(bootstrap.authority.device_public_key),
            postcard::from_bytes(bytes).unwrap(),
            SystemTime::now(),
        )
        .unwrap();
    }
    peer
}

async fn until_refused(what: &str, relay: &Relay, device: &TestDevice, workspace_id: u64) {
    tilo_test_env::within(what, async {
        while admitted(relay, device, workspace_id).await {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revocation_announced_in_one_room_reaches_the_relay_and_every_room_it_governs() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("alice-phone", DeviceCapabilities::primary());
    let tablet = bob.enrol("bob-tablet", DeviceCapabilities::member());
    let bob_laptop = bob.enrol("bob-laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;

    // Alice's laptop governs two rooms through her relay; her phone is in
    // the first, and Bob is in it with two devices.
    let plans = create_and_enter(&relay, &host, &laptop, "Plans").await;
    let other = create_and_enter(&relay, &host, &laptop, "Other").await;
    enter(&relay, &host, &phone, &plans.room).await;
    let mut tablet_peer = join(&relay, &laptop, &plans.room, &bob, &tablet).await;
    join(&relay, &laptop, &plans.room, &bob, &bob_laptop).await;
    for workspace in [plans.workspace_id, other.workspace_id] {
        assert!(admitted(&relay, &laptop, workspace).await);
    }
    assert!(admitted(&relay, &bob_laptop, plans.workspace_id).await);

    // Alice's laptop is stolen. Her root revokes it, and her phone says so in
    // the room it shares with the relay.
    let phone_session = session(&relay, &phone, plans.workspace_id).await;
    publish(
        &phone_session,
        &hello(&phone, vec![revoke(&alice, &laptop)]),
    );

    // The relay admits the laptop to none of its rooms and lets it
    // administer none of them.
    until_refused(
        "the relay stops admitting the revoked laptop",
        &relay,
        &laptop,
        plans.workspace_id,
    )
    .await;
    assert!(!admitted(&relay, &laptop, other.workspace_id).await);
    assert!(
        relay
            .rooms()
            .unwrap()
            .issue(
                &other.room,
                &laptop.cert,
                IssueInvitationRequest {
                    request_id: [3; 32],
                    role: MemberRole::Editor,
                    recipient_user_public_key: Some(bob.user_pk),
                    invitee_name: None,
                    expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
                    inviter_name: None,
                },
            )
            .await
            .is_err(),
        "a revoked device administers no room"
    );

    // Bob's replica refuses the laptop from the room's own governance.
    let tablet_session = session(&relay, &tablet, plans.workspace_id).await;
    let mut seen = Vec::new();
    receive_until(
        "Bob's replica refuses the revoked laptop",
        &tablet_session,
        &mut tablet_peer,
        &mut seen,
        |peer, _| {
            peer.permissions(laptop.device_pubkey(), SystemTime::now())
                .is_err()
        },
    )
    .await;

    // A member's own root revokes one of its devices the same way.
    publish(
        &tablet_session,
        &hello(&tablet, vec![revoke(&bob, &bob_laptop)]),
    );
    until_refused(
        "the relay stops admitting Bob's revoked laptop",
        &relay,
        &bob_laptop,
        plans.workspace_id,
    )
    .await;
    assert!(admitted(&relay, &tablet, plans.workspace_id).await);

    // The revocations survive a restart of the relay.
    phone_session.close().await;
    tablet_session.close().await;
    relay.shutdown().await;
    let relay = start(state.path(), &alice, &host).await;
    assert!(!admitted(&relay, &laptop, other.workspace_id).await);
    assert!(!admitted(&relay, &bob_laptop, plans.workspace_id).await);
    assert!(admitted(&relay, &phone, plans.workspace_id).await);
    relay.shutdown().await;
}
