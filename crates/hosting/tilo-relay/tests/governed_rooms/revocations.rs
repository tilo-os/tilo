//! A device revoked by its person's root, announced in a governed room by
//! another device of that person, stops at the relay: it is admitted to no
//! room the relay governs, administers none of them, and every member's
//! replica refuses it from the room's governance.
use ed25519_dalek::SigningKey;
use governance_actions::{admitted, connected, host_action};
use tilo_relay::workspaces::ENTER_ROOM;
use tilo_user_identity::{
    CERTIFICATION_SCHEMA_VERSION, CertificationRequest, RevocationEntry, RevocationReason,
    issue_device_cert_as_stand_in, sign_revocation_entry,
};

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
    }
}

async fn enter(relay: &Relay, device: &TestDevice, room: &str) {
    let home = connected(relay, device).await;
    let _: EnteredWorkspace =
        host_action(&home, ENTER_ROOM, serde_json::json!({ "room": room })).await;
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
        bootstrap.host.clone(),
        bootstrap.certified_roster.clone(),
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    peer.select_admission_v5().unwrap();
    for bytes in &bootstrap.governance_messages {
        peer.receive_from(
            TransportOrigin::RetainedRelayDelta(bootstrap.host.device_public_key),
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
    let plans = create_and_enter(&relay, &laptop, "Plans").await;
    let other = create_and_enter(&relay, &laptop, "Other").await;
    enter(&relay, &phone, &plans.workspace).await;
    let mut tablet_peer = join(&relay, &laptop, &plans.workspace, &bob, &tablet).await;
    join(&relay, &laptop, &plans.workspace, &bob, &bob_laptop).await;
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
            .workspaces()
            .unwrap()
            .issue(
                &other.workspace,
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

/// A person's root revokes the stand-in approver the relay's host holds for
/// them: a key no room lists, because a stand-in is no device. The relay keeps
/// the entry in its own revocation log — the list its approvals read — and what
/// the stand-in signed after the revocation is refused there while what it
/// signed before stays valid.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario: the stand-in approves, is revoked, and is refused from then on"
)]
async fn a_stand_in_revoked_in_a_room_is_refused_by_what_it_signs_after() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let tablet = bob.enrol("bob-tablet", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let plans = create_and_enter(&relay, &laptop, "Plans").await;
    join(&relay, &laptop, &plans.workspace, &bob, &tablet).await;

    // Bob set Tilo up through an agent: this host holds a stand-in approver for
    // him, and it certified a browser of his while his root lived in one tab.
    let day = 24 * 60 * 60 * 1_000_000_000;
    let stand_in_key = SigningKey::from_bytes(&[0x5a; 32]);
    let made_at = tilo_clock::unix_now_ns();
    let mut stand_in = tilo_user_identity::prepare_stand_in_cert(
        bob.user_pk,
        stand_in_key.verifying_key().to_bytes(),
        "Stand-in on the host",
        made_at,
        made_at + 30 * day,
    );
    tilo_user_identity::sign_device_cert(&mut stand_in, &bob.user_sk).unwrap();
    let request = |key: u8, at: u64| {
        let device = SigningKey::from_bytes(&[key; 32]);
        CertificationRequest {
            version: CERTIFICATION_SCHEMA_VERSION,
            user_public_key: bob.user_pk,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Browser".into(),
            requested: DeviceCapabilities::member(),
            nonce: [key; 16],
            invite: [0; 16],
            issued_at_ns: at,
            expires_at_ns: at + 60_000_000_000,
            signature: [0; 64],
        }
        .sign(&device)
        .unwrap()
    };
    let before = request(7, stand_in.issued_at_ns + 1);
    let approved = issue_device_cert_as_stand_in(
        &before,
        &stand_in_key,
        &stand_in,
        &RevocationList::new(),
        before.issued_at_ns,
    )
    .unwrap();

    // He keeps the root with a passkey: his tablet announces the revocation his
    // root signed in the room it shares with the relay.
    let revoked_at = stand_in.issued_at_ns + 2;
    let mut revocation = RevocationEntry {
        user_public_key: bob.user_pk,
        revoked_device_public_key: stand_in.device_public_key,
        revoked_at_ns: revoked_at,
        reason: RevocationReason::Retired,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut revocation, &bob.user_sk).unwrap();
    let tablet_session = session(&relay, &tablet, plans.workspace_id).await;
    publish(&tablet_session, &hello(&tablet, vec![revocation.clone()]));

    // The relay's revocation log holds it: the list every handshake and every
    // approval this host makes reads, and a restart reads again.
    let logged = tilo_test_env::within("the relay keeps the revocation", async {
        loop {
            if let Ok(entries) = tilo_relay::store::load_revocations(state.path())
                && entries.iter().any(|known| known == &revocation)
            {
                return entries;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    let mut revocations = RevocationList::new();
    for entry in logged {
        revocations.insert(entry).unwrap();
    }
    assert!(
        !revocations.refuses(&approved),
        "the browser the stand-in approved before stays a device of Bob's"
    );
    assert!(
        revocations.refuses(&stand_in),
        "the stand-in itself is refused"
    );
    // A stand-in that answers anyway, as a host still holding its key would.
    let later_request = request(8, revoked_at);
    let later = issue_device_cert_as_stand_in(
        &later_request,
        &stand_in_key,
        &stand_in,
        &RevocationList::new(),
        revoked_at,
    )
    .unwrap();
    assert!(
        revocations.refuses(&later),
        "and so is a device it signs after the revocation"
    );

    tablet_session.close().await;
    relay.shutdown().await;
}

/// Alice's root revokes thousands of keys, more than the relay's log once
/// had room for, and last her phone, a device in none of the relay's rooms:
/// only the relay's own log refuses it. The relay restarts as its daemon
/// does, from that log, and refuses the phone and every key revoked before
/// it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_revocation_is_refused_after_a_restart() {
    let alice = TestUser::fresh("Alice");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("alice-phone", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let plans = create_and_enter(&relay, &laptop, "Plans").await;
    let connects = |relay: &Relay| {
        let relay_addr = relay.node_addr();
        let phone = &phone;
        async move {
            RelayClient::connect_to_pinned_host(
                device_endpoint(phone).await,
                relay_addr,
                phone.cert.clone(),
                phone.sk.clone(),
                RevocationList::new(),
            )
            .await
            .is_ok()
        }
    };
    assert!(connects(&relay).await, "the phone reaches its relay");

    let mut revoked = (0..5000_u32)
        .map(|n| {
            let mut key = [0xa5; 32];
            key[..4].copy_from_slice(&n.to_le_bytes());
            let mut entry = RevocationEntry {
                user_public_key: alice.user_pk,
                revoked_device_public_key: key,
                revoked_at_ns: tilo_clock::unix_now_ns(),
                reason: RevocationReason::Retired,
                signature: [0; 64],
            };
            sign_revocation_entry(&mut entry, &alice.user_sk).unwrap();
            entry
        })
        .collect::<Vec<_>>();
    revoked.push(revoke(&alice, &phone));
    let laptop_session = session(&relay, &laptop, plans.workspace_id).await;
    publish(&laptop_session, &hello(&laptop, revoked.clone()));
    tilo_test_env::within("the relay keeps every revocation", async {
        while tilo_relay::store::load_revocations(state.path()).map_or(0, |entries| entries.len())
            < revoked.len()
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    laptop_session.close().await;
    relay.shutdown().await;

    // The daemon starts from the identity its log refuses.
    let identity = tilo_relay::daemon::with_persisted_revocations(
        tilo_relay::daemon::RelayIdentity {
            user_public_key: alice.user_pk,
            device_signing_key: host.sk.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
        state.path(),
    )
    .unwrap();
    for entry in &revoked {
        assert!(
            identity
                .revocations
                .is_revoked(&entry.user_public_key, &entry.revoked_device_public_key),
            "every key revoked before the restart is refused after it"
        );
    }
    let relay = crate::common::start_relay(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            iroh_listen: "127.0.0.1:0".parse().unwrap(),
            ..RelayConfig::default()
        },
        identity,
    )
    .await
    .unwrap();
    assert!(
        !connects(&relay).await,
        "the phone, revoked past the old log's room, is refused after the restart"
    );
    assert!(admitted(&relay, &laptop, plans.workspace_id).await);
    relay.shutdown().await;
}
