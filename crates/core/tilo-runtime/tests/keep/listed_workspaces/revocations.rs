//! A device revoked by its person's root, announced in a hosted workspace by
//! another device of that person, stops at the server: it is admitted to no
//! workspace the server hosts, administers none of them, and every member's
//! copy refuses it from the workspace's governance.
use ed25519_dalek::SigningKey;
use governance_actions::{asking, host_action};
use tilo_server_client::ServerAddress;
use tilo_server_client::link::{Dialer, Purpose};
use tilo_user_identity::{
    CERTIFICATION_SCHEMA_VERSION, CertificationRequest, RevocationEntry, RevocationReason,
    issue_device_cert_as_stand_in, sign_revocation_entry,
};
use tilo_workspace_service::workspaces::ENTER_WORKSPACE;

use super::*;
use tilo_server_protocol::WorkspaceId;

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

/// `device` enters `workspace` as a device of its owner's does: the host's own action hands it an
/// invitation of its own, and it signs its own row onto the member list, the host signing nothing
/// for it. A link it opens then is a member's at once, rather than one carrying its own row that
/// ends so it dials again.
async fn enter(server: &Server, device: &TestDevice, workspace: &str) {
    let home = asking(server, device, "").await;
    let _: EnteredWorkspace = host_action(
        &home,
        device,
        ENTER_WORKSPACE,
        serde_json::json!({ "workspace": workspace }),
    )
    .await;
    server
        .workspaces()
        .expect("the server holds workspaces")
        .enter(workspace, (&device.cert, &device.sk))
        .await
        .expect("the workspace lists the device that entered it");
}

/// `device` of `person` redeems an editor's invitation to `workspace` that
/// `issuer` issues, and holds what its join alone gave it.
async fn join(
    server: &Server,
    issuer: &TestDevice,
    workspace: &str,
    person: &TestUser,
    device: &TestDevice,
) -> StatePeer {
    let invitation = invite(server, issuer, workspace, Role::Editor, person.user_pk).await;
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
        &bootstrap.workspace_id.to_string(),
        bootstrap.host.clone(),
        bootstrap.certified_roster.clone(),
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    peer.select_exact_admissions().unwrap();
    for bytes in &bootstrap.governance_messages {
        peer.receive_from(
            TransportOrigin::RetainedServerDelta(bootstrap.host.device_public_key),
            postcard::from_bytes(bytes).unwrap(),
            SystemTime::now(),
        )
        .unwrap();
    }
    peer
}

/// Whether `linked`'s device is admitted to `workspace_id`: a link opens from its own
/// device's endpoint, or the host refuses it.
async fn admitted(linked: &Linked, server: &Server, workspace_id: WorkspaceId) -> bool {
    linked.reopen(server, workspace_id).await.is_ok()
}

/// Wait until `linked`'s device is refused `workspace_id`.
async fn until_refused(what: &str, link: &Linked, server: &Server, workspace_id: WorkspaceId) {
    tilo_test_env::within(what, async {
        while admitted(link, server, workspace_id).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// Whether `device` reaches its server at all: the member handshake every link starts with,
/// which the server's own revocation log decides before any workspace's roster.
async fn reaches(server: &Server, device: &TestDevice, host: &TestDevice) -> bool {
    let endpoint = device_endpoint(device).await;
    let dialer = Dialer::from_endpoint(ServerAddress::at(server.node_addr()), endpoint);
    let Ok(link) = dialer.link(Purpose::Records).await else {
        return false;
    };
    link.prove_member(&device.cert, &device.sk, &host.cert)
        .await
        .is_ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revocation_announced_in_one_workspace_reaches_the_server_and_every_workspace_it_hosts() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("alice-phone", DeviceCapabilities::primary());
    let tablet = bob.enrol("bob-tablet", DeviceCapabilities::member());
    let bob_laptop = bob.enrol("bob-laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &host).await;

    // Alice's laptop administers two workspaces through her server; her phone is in
    // the first, and Bob is in it with two devices.
    let plans = create_and_enter(&server, &laptop, "Plans").await;
    let other = create_and_enter(&server, &laptop, "Other").await;
    enter(&server, &phone, &plans.workspace).await;
    let tablet_copy = join(&server, &laptop, &plans.workspace, &bob, &tablet).await;
    let bob_laptop_copy = join(&server, &laptop, &plans.workspace, &bob, &bob_laptop).await;
    let tablet_link = Linked::open_seeded(&server, &tablet, plans.workspace_id, tablet_copy)
        .await
        .expect("Plans admits Bob's tablet");
    let bob_laptop_link =
        Linked::open_seeded(&server, &bob_laptop, plans.workspace_id, bob_laptop_copy)
            .await
            .expect("Plans admits Bob's laptop");
    let phone_link = Linked::open(&server, &phone, plans.workspace_id)
        .await
        .expect("Plans admits Alice's phone");
    let laptop_plans = Linked::open(&server, &laptop, plans.workspace_id)
        .await
        .expect("Plans admits Alice's laptop");
    let laptop_other = laptop_plans
        .reopen(&server, other.workspace_id)
        .await
        .expect("Other admits Alice's laptop");
    for (link, workspace_id) in [
        (&laptop_plans, plans.workspace_id),
        (&laptop_other, other.workspace_id),
    ] {
        assert!(admitted(link, &server, workspace_id).await);
    }
    assert!(admitted(&bob_laptop_link, &server, plans.workspace_id).await);

    // Alice's laptop is stolen. Her root revokes it, and her phone says so in
    // the workspace it shares with the server.
    phone_link
        .send(hello(&phone, vec![revoke(&alice, &laptop)]))
        .expect("the phone's revocation goes out on its link");

    // The server admits the laptop to none of its workspaces and lets it
    // administer none of them.
    until_refused(
        "the server stops admitting the revoked laptop",
        &laptop_plans,
        &server,
        plans.workspace_id,
    )
    .await;
    assert!(!admitted(&laptop_other, &server, other.workspace_id).await);
    assert!(
        server
            .workspaces()
            .unwrap()
            .issue(
                &other.workspace,
                &laptop.cert,
                IssueInvitationRequest {
                    request_id: [3; 32],
                    role: Role::Editor,
                    recipient_user_public_key: Some(bob.user_pk),
                    invitee_name: None,
                    expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
                    inviter_name: None,
                },
            )
            .await
            .is_err(),
        "a revoked device administers no workspace"
    );

    // Bob's copy refuses the laptop from the workspace's own governance.
    crate::keep::common::receives(&tablet_link, |peer| {
        peer.permissions(laptop.device_pubkey(), SystemTime::now())
            .is_err()
    })
    .await;

    // A member's own root revokes one of its devices the same way.
    tablet_link
        .send(hello(&tablet, vec![revoke(&bob, &bob_laptop)]))
        .expect("Bob's revocation goes out on his tablet's link");
    until_refused(
        "the server stops admitting Bob's revoked laptop",
        &bob_laptop_link,
        &server,
        plans.workspace_id,
    )
    .await;
    assert!(admitted(&tablet_link, &server, plans.workspace_id).await);

    // The revocations survive a restart of the server.
    drop(phone_link);
    drop(tablet_link);
    drop(bob_laptop_link);
    drop(laptop_plans);
    drop(laptop_other);
    server.shutdown().await;
    let server = start(state.path(), &alice, &host).await;
    assert!(
        Linked::open(&server, &laptop, other.workspace_id)
            .await
            .is_err()
    );
    assert!(
        Linked::open(&server, &bob_laptop, plans.workspace_id)
            .await
            .is_err()
    );
    assert!(
        Linked::open(&server, &phone, plans.workspace_id)
            .await
            .is_ok()
    );
    server.shutdown().await;
}

/// A revocation written into a workspace reaches a link that is already
/// open and ends it, with nothing polling for it: the write itself wakes what
/// applies it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revocation_announced_in_a_workspace_closes_a_connected_session() {
    let alice = TestUser::fresh("Alice");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("alice-phone", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &host).await;
    let plans = create_and_enter(&server, &laptop, "Plans").await;
    enter(&server, &phone, &plans.workspace).await;

    // The laptop is linked when the revocation is announced.
    let mut laptop_link = Linked::open(&server, &laptop, plans.workspace_id)
        .await
        .expect("Plans admits Alice's laptop");
    let phone_link = Linked::open(&server, &phone, plans.workspace_id)
        .await
        .expect("Plans admits Alice's phone");
    phone_link
        .send(hello(&phone, vec![revoke(&alice, &laptop)]))
        .expect("the phone's revocation goes out on its link");

    // Its link ends as the write takes effect; no timer drove it.
    tilo_test_env::within("the revoked laptop's link to end", async {
        let _ended = laptop_link.closed().await;
    })
    .await;

    drop(phone_link);
    server.shutdown().await;
}

/// A person's root revokes the stand-in approver the server's host holds for
/// them: a key no workspace lists, because a stand-in is no device. The server keeps
/// the entry in its own revocation log — the list its approvals read — and what
/// the stand-in signed after the revocation is refused there while what it
/// signed before stays valid.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario: the stand-in approves, is revoked, and is refused from then on"
)]
async fn a_stand_in_revoked_in_a_workspace_is_refused_by_what_it_signs_after() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let tablet = bob.enrol("bob-tablet", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &host).await;
    let plans = create_and_enter(&server, &laptop, "Plans").await;
    join(&server, &laptop, &plans.workspace, &bob, &tablet).await;

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
    // root signed in the workspace it shares with the server.
    let revoked_at = stand_in.issued_at_ns + 2;
    let mut revocation = RevocationEntry {
        user_public_key: bob.user_pk,
        revoked_device_public_key: stand_in.device_public_key,
        revoked_at_ns: revoked_at,
        reason: RevocationReason::Retired,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut revocation, &bob.user_sk).unwrap();
    let tablet_link = Linked::open(&server, &tablet, plans.workspace_id)
        .await
        .expect("Plans admits Bob's tablet");
    tablet_link
        .send(hello(&tablet, vec![revocation.clone()]))
        .expect("Bob's revocation goes out on his tablet's link");

    // The server's revocation log holds it: the list every handshake and every
    // approval this host makes reads, and a restart reads again.
    let logged = tilo_test_env::within("the server keeps the revocation", async {
        loop {
            if let Ok(entries) =
                tilo_workspace_service::workspace_files::load_revocations(state.path())
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

    drop(tablet_link);
    server.shutdown().await;
}

/// Alice's root revokes thousands of keys, more than the server's log once
/// had room for, and last her phone, a device in none of the server's workspaces:
/// only the server's own log refuses it. The server restarts as its daemon
/// does, from that log, and refuses the phone and every key revoked before
/// it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_revocation_is_refused_after_a_restart() {
    let alice = TestUser::fresh("Alice");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("alice-phone", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &host).await;
    let plans = create_and_enter(&server, &laptop, "Plans").await;
    assert!(
        reaches(&server, &phone, &host).await,
        "the phone reaches its server"
    );

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
    let laptop_link = Linked::open(&server, &laptop, plans.workspace_id)
        .await
        .expect("Plans admits Alice's laptop");
    laptop_link
        .send(hello(&laptop, revoked.clone()))
        .expect("the laptop's revocations go out on its link");
    tilo_test_env::within("the server keeps every revocation", async {
        while tilo_workspace_service::workspace_files::load_revocations(state.path())
            .map_or(0, |entries| entries.len())
            < revoked.len()
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    drop(laptop_link);
    server.shutdown().await;

    // The daemon starts from the identity its log refuses.
    let identity = tilo_runtime::device::keep::server::with_persisted_revocations(
        tilo_runtime::device::keep::server::ServerIdentity {
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
    let server = crate::keep::common::start_server(
        ServerConfig {
            state_root: state.path().to_path_buf(),
            iroh_listen: "127.0.0.1:0".parse().unwrap(),
            ..ServerConfig::default()
        },
        identity,
    )
    .await
    .unwrap();
    assert!(
        !reaches(&server, &phone, &host).await,
        "the phone, revoked past the old log's workspace, is refused after the restart"
    );
    assert!(
        Linked::open(&server, &laptop, plans.workspace_id)
            .await
            .is_ok()
    );
    server.shutdown().await;
}
