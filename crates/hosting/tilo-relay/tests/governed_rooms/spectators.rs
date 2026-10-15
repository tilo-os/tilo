//! Visitors who only watch a public room: devices its spectator person
//! certified read the room through the relay and write nothing into it, up
//! to the room's spectator seats.

use tilo_relay::Spectators;
use tilo_user_identity::{DeviceCert, sign_device_cert};

use super::*;

const WATCH_ONLY: &str = "a spectator only watches this room";

/// Invite `person` to `room` as `role` and redeem the invitation with
/// `device`, as a page's join does: the device's replica of the room.
async fn join(
    relay: &Relay,
    issuer: &TestDevice,
    room: &str,
    (person, device): (&TestUser, &TestDevice),
    role: MemberRole,
    request: u8,
) -> StatePeer {
    let invitation = invite(relay, issuer, room, role, person.user_pk).await;
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
            [request; 32],
        )
        .await
        .unwrap()
        .bootstrap;
    let mut replica = StatePeer::new(
        &bootstrap.room,
        bootstrap.authority.clone(),
        bootstrap.certified_roster.clone(),
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    replica.select_admission_v5().unwrap();
    for bytes in &bootstrap.governance_messages {
        replica
            .receive_from(
                TransportOrigin::RetainedRelayDelta(bootstrap.authority.device_public_key),
                postcard::from_bytes(bytes).unwrap(),
                SystemTime::now(),
            )
            .unwrap();
    }
    replica
}

/// A watching page's device: an hour-long certificate of the spectator
/// person with no capabilities, as the room's host signs it.
fn watching_device(spectator: &TestUser, seed: u8) -> TestDevice {
    let sk = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let now = tilo_clock::unix_now_ns();
    let mut cert = DeviceCert {
        user_public_key: spectator.user_pk,
        device_public_key: sk.verifying_key().to_bytes(),
        device_label: "Spectator".into(),
        capabilities: tilo_user_identity::DeviceCapabilities {
            can_own_workspaces: false,
            can_admit_devices: false,
            can_act_as_host: false,
            can_read_all_workspaces: false,
            foreign: false,
            can_administer_devices: false,
        },
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &spectator.user_sk).unwrap();
    TestDevice { sk, cert }
}

async fn connect(relay: &Relay, device: &TestDevice) -> RelayClient {
    RelayClient::connect_to_pinned_host(
        device_endpoint(device).await,
        relay.node_addr(),
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap()
}

/// The first envelope `client` receives that `wanted` picks, skipping the
/// room's state streaming in meanwhile.
async fn answer<T>(
    what: &str,
    client: &RelayClient,
    mut wanted: impl FnMut(Envelope) -> Option<T>,
) -> T {
    tilo_test_env::within(what, async {
        loop {
            let envelope = client.recv().await.expect("the connection stays open");
            if let Some(found) = wanted(envelope) {
                return found;
            }
        }
    })
    .await
}

/// Subscribe `client` to `workspace_id`: `Ready`, or the refusal's reason.
async fn subscribe(client: &RelayClient, workspace_id: u64) -> Result<(), String> {
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    answer(
        "the relay answers a subscription",
        client,
        |envelope| match envelope {
            Envelope::Ready { .. } => Some(Ok(())),
            Envelope::NotAuthorised { reason, .. } => Some(Err(reason)),
            _ => None,
        },
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spectators_read_a_public_room_write_nothing_and_fill_only_its_seats() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let spectator = TestUser::fresh("Watching visitors");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let first = spectator.enrol("joined", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;

    // Alice's public room: Bob is an editor, and the spectator person a
    // viewer, as the room's host makes it.
    let room = create_and_enter(&relay, &host, &laptop, "Plaza").await;
    let workspace_id = room.workspace_id;
    let mut bob_peer = join(
        &relay,
        &laptop,
        &room.room,
        (&bob, &phone),
        MemberRole::Editor,
        1,
    )
    .await;
    join(
        &relay,
        &laptop,
        &room.room,
        (&spectator, &first),
        MemberRole::Viewer,
        2,
    )
    .await;

    // Before the room lets anyone watch, a watching page reads nothing:
    // this is what every embed met.
    let early = connect(&relay, &watching_device(&spectator, 40)).await;
    assert_eq!(
        subscribe(&early, workspace_id).await,
        Err("device not in workspace membership".to_owned())
    );
    drop(early);

    relay
        .admit_spectators(
            workspace_id,
            Spectators {
                root: spectator.user_pk,
                seats: 2,
                public: true,
            },
        )
        .await
        .unwrap();

    let mut alice_peer = StatePeer::new(
        &room.room,
        host.cert.clone(),
        vec![host.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    let alice_session = session(&relay, &laptop, workspace_id).await;
    receive_until(
        "Alice's replica admits her laptop and Bob",
        &alice_session,
        &mut alice_peer,
        &mut Vec::new(),
        |peer, _| {
            role(peer, laptop.device_pubkey()) == Some(collab_types::Role::Owner)
                && role(peer, phone.device_pubkey()) == Some(collab_types::Role::Editor)
        },
    )
    .await;
    let bob_session = session(&relay, &phone, workspace_id).await;
    crate::common::wait_until_subscribed(&relay, workspace_id, &phone.device_pubkey()).await;

    // A watching page subscribes and reads: the room's state, what members
    // write live, and a sync answer.
    let watcher = watching_device(&spectator, 41);
    let watcher_key = watcher.device_pubkey();
    let watching = connect(&relay, &watcher).await;
    assert_eq!(subscribe(&watching, workspace_id).await, Ok(()));
    crate::common::wait_until_subscribed(&relay, workspace_id, &watcher_key).await;
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "welcome").unwrap();
    doc.commit_edit("fixture", "topic");
    let welcome = alice_peer
        .publish_incremental(APP, "", doc.save_after(&[]), &laptop.sk, SystemTime::now())
        .unwrap();
    publish(&alice_session, &welcome);
    receive_until(
        "Alice's write in Bob's replica",
        &bob_session,
        &mut bob_peer,
        &mut Vec::new(),
        |peer, _| text(peer, "topic").as_deref() == Some("welcome"),
    )
    .await;
    answer(
        "Alice's write reaches the watching page",
        &watching,
        |envelope| {
            matches!(
                envelope,
                Envelope::Delta { author_device_public_key, .. }
                    if author_device_public_key == laptop.device_pubkey()
            )
            .then_some(())
        },
    )
    .await;
    watching
        .send(Envelope::SyncRequest {
            workspace_id,
            have_heads: Vec::new(),
        })
        .unwrap();
    answer(
        "the watching page's sync is answered",
        &watching,
        |envelope| matches!(envelope, Envelope::SyncResponse { .. }).then_some(()),
    )
    .await;

    // Every way a device writes into the room is refused to it. Its change
    // could be a channel access request: it reaches no steward either.
    let change = tilo_relay::CoeditPayload::RetainedDelta {
        channel: [9; 32],
        body: b"a spectator's change".to_vec(),
    }
    .encode()
    .unwrap();
    watching
        .send(Envelope::Delta {
            workspace_id,
            author_device_public_key: watcher_key,
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: change.clone(),
            signature: [0; 64],
        })
        .unwrap();
    let refused = answer(
        "the spectator's change is refused",
        &watching,
        |envelope| match envelope {
            Envelope::NotAuthorised { reason, .. } => Some(reason),
            _ => None,
        },
    )
    .await;
    assert_eq!(refused, WATCH_ONLY);
    watching
        .send(Envelope::Commit {
            workspace_id,
            op: [3; 16],
            author_device_public_key: watcher_key,
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: change,
        })
        .unwrap();
    let refused = answer(
        "the spectator's commit is refused",
        &watching,
        |envelope| match envelope {
            Envelope::CommitRefused { op, reason, .. } if op == [3; 16] => Some(reason),
            _ => None,
        },
    )
    .await;
    assert_eq!(refused, WATCH_ONLY);
    watching.send(Envelope::LiveJoin { workspace_id }).unwrap();
    let reason = answer(
        "the spectator's live join is refused",
        &watching,
        |envelope| match envelope {
            Envelope::NotAuthorised { reason, .. } => Some(reason),
            _ => None,
        },
    )
    .await;
    assert_eq!(reason, WATCH_ONLY);
    watching
        .send(Envelope::RemoteAction {
            workspace_id,
            version: 1,
            to_device_public_key: laptop.device_pubkey(),
            from_device_public_key: watcher_key,
            from_user_public_key: spectator.user_pk,
            frame: b"an action".to_vec(),
        })
        .unwrap();
    answer(
        "the spectator's action is undeliverable",
        &watching,
        |envelope| matches!(envelope, Envelope::RemoteActionUndeliverable { .. }).then_some(()),
    )
    .await;
    let attributed = tilo_relay::AttributedPresence {
        author: watcher_key,
        body: b"a spectator's cursor".to_vec(),
    }
    .encode()
    .unwrap();
    watching
        .send(Envelope::Presence {
            workspace_id,
            payload: attributed,
        })
        .unwrap();
    watching
        .send(Envelope::RecordContent {
            workspace_id,
            to_device_public_key: laptop.device_pubkey(),
            from_device_public_key: watcher_key,
            frame: b"a spectator's content".to_vec(),
        })
        .unwrap();
    watching.send(Envelope::Ping).unwrap();
    answer("the relay took every frame", &watching, |envelope| {
        matches!(envelope, Envelope::Pong).then_some(())
    })
    .await;

    // Alice, a member, receives Bob's next write and nothing the spectator
    // sent before it: no change, presence, content or action.
    publish(
        &bob_session,
        &write(&mut bob_peer, "topic", "after", &phone.sk),
    );
    let mut from_spectator = Vec::new();
    tilo_test_env::within("Bob's next write reaches Alice", async {
        while text(&alice_peer, "topic").as_deref() != Some("after") {
            match alice_session
                .recv()
                .await
                .expect("Alice's session stays open")
            {
                Inbound::Delta { author, .. } if author == watcher_key => {
                    from_spectator.push("change");
                }
                Inbound::Delta { author, data } => {
                    let message: NeighbourMsg = postcard::from_bytes(&data).unwrap();
                    let origin = if author == host.device_pubkey() {
                        TransportOrigin::RetainedRelayDelta(author)
                    } else {
                        TransportOrigin::RelayDelta(author)
                    };
                    alice_peer
                        .receive_from(origin, message, SystemTime::now())
                        .unwrap();
                }
                Inbound::Presence { author, .. } if author == watcher_key => {
                    from_spectator.push("presence");
                }
                Inbound::RecordContent { from, .. } if from == watcher_key => {
                    from_spectator.push("content");
                }
                Inbound::RemoteAction { from, .. } if from == watcher_key => {
                    from_spectator.push("action");
                }
                _ => {}
            }
        }
    })
    .await;
    assert!(
        from_spectator.is_empty(),
        "a spectator reached a member: {from_spectator:?}"
    );
    // What the spectator sends is dropped, but it still sees the members'
    // cursors: presence reaches every watching page.
    bob_session
        .publish_presence(b"Bob's cursor".to_vec())
        .unwrap();
    let cursor = answer(
        "Bob's presence reaches the watching page",
        &watching,
        |envelope| match envelope {
            Envelope::Presence { payload, .. } => tilo_relay::AttributedPresence::decode(&payload)
                .filter(|presence| presence.author == phone.device_pubkey())
                .map(|presence| presence.body),
            _ => None,
        },
    )
    .await;
    assert_eq!(cursor, b"Bob's cursor");

    // Two seats: a second page watches, a third is refused until one leaves.
    let second = connect(&relay, &watching_device(&spectator, 42)).await;
    assert_eq!(subscribe(&second, workspace_id).await, Ok(()));
    let third = connect(&relay, &watching_device(&spectator, 43)).await;
    assert_eq!(
        subscribe(&third, workspace_id).await,
        Err("too many people are watching this room".to_owned())
    );
    // Members are never held back by watching pages.
    assert_eq!(
        subscribe(&connect(&relay, &phone).await, workspace_id).await,
        Ok(())
    );
    second.send(Envelope::Unsubscribe { workspace_id }).unwrap();
    let seated = tilo_test_env::within("a freed seat is taken", async {
        loop {
            match subscribe(&third, workspace_id).await {
                Ok(()) => break true,
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
    })
    .await;
    assert!(seated);

    alice_session.close().await;
    bob_session.close().await;
    relay.shutdown().await;
}
