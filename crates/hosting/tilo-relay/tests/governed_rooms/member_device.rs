//! A member's own second browser: the device their root certified is admitted
//! as theirs, reads the workspace like any of their devices, and writes only
//! what their role writes — nothing as a Viewer, its changes as an Editor.

use tilo_peer_state::join::MemberRole;
use tilo_relay::workspaces::{SetMemberRoleRequest, WorkspaceContent};
use tilo_relay_protocol::refusal::{PLAIN_NOT_ALLOWED, plain_reason};

use super::*;

/// Content the room's host keeps: one item, by its BLAKE3 address.
struct Kept(Vec<u8>);

impl WorkspaceContent for Kept {
    fn read(&self, _room: &str, _app: &str, hash: &str) -> Option<Vec<u8>> {
        (blake3::hash(&self.0).to_hex().as_str() == hash).then(|| self.0.clone())
    }
}

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
        bootstrap.host.clone(),
        bootstrap.certified_roster.clone(),
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    replica.select_admission_v5().unwrap();
    for bytes in &bootstrap.governance_messages {
        replica
            .receive_from(
                TransportOrigin::RetainedRelayDelta(bootstrap.host.device_public_key),
                postcard::from_bytes(bytes).unwrap(),
                SystemTime::now(),
            )
            .unwrap();
    }
    replica
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

/// `key` set to `value` in `base`'s copy of the record, signed by `device` as
/// the page's device signs it: the change the room decides, made without the
/// page's own replica checking it first.
fn change(
    base: &StatePeer,
    workspace: &str,
    surface: Option<[u8; 32]>,
    device: &TestDevice,
    key: &str,
    value: &str,
    seq: u64,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&base.document(APP, "").unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, key, value).unwrap();
    doc.commit_edit("fixture", key);
    let mutation = collab_types::DocumentMutation::Incremental(doc.save_after(&heads));
    let author = collab_types::Ed25519PublicKey::from_bytes(device.device_pubkey());
    let mut op = collab_types::SignedOp::new_unsigned(
        mutation.encode().unwrap(),
        collab_types::document_artifact_id(APP, workspace, ""),
        author,
        collab_types::document_attestation_id(APP, workspace, ""),
        collab_types::OpKind::Change,
        SystemTime::now(),
        seq,
    )
    .with_surface(surface);
    collab_types::sign_op(&mut op, &device.sk).unwrap();
    NeighbourMsg::AppDocument {
        peer: tilo_peer_state::peer_handle(author.as_bytes()),
        app_id: APP.into(),
        cluster_id: workspace.into(),
        record_id: String::new(),
        envelope: Box::new(op),
    }
}

/// `message`, signed by [`change`], as the commit a page sends: the retained
/// change the relay decides.
fn commit(
    client: &RelayClient,
    workspace_id: u64,
    op: [u8; 16],
    author: [u8; 32],
    message: &NeighbourMsg,
) {
    let (channel, kind) = neighbour_wire::retention(message, false).unwrap();
    assert_ne!(kind, neighbour_wire::Retention::Checkpoint);
    let delta = tilo_relay::CoeditPayload::RetainedDelta {
        channel,
        body: postcard::to_stdvec(message).unwrap(),
    }
    .encode()
    .unwrap();
    client
        .send(Envelope::Commit {
            workspace_id,
            op,
            author_device_public_key: author,
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta,
        })
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_members_second_browser_is_admitted_and_writes_what_the_role_writes() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let carol = TestUser::fresh("Carol");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let carol_laptop = carol.enrol("carol-laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;

    // Alice's workspace: Bob is an editor, Carol a viewer.
    let room = create_and_enter(&relay, &laptop, "Plaza").await;
    let workspace_id = room.workspace_id;
    let mut bob_peer = join(
        &relay,
        &laptop,
        &room.workspace,
        (&bob, &phone),
        MemberRole::Editor,
        1,
    )
    .await;
    // Carol's first device, which the invitation makes a member holding the
    // Viewer role her host gives her.
    let _carol_laptop = join(
        &relay,
        &laptop,
        &room.workspace,
        (&carol, &carol_laptop),
        MemberRole::Viewer,
        2,
    )
    .await;

    let mut alice_peer = StatePeer::new(
        &room.workspace,
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

    // Carol's browser, a second device her root certified, is no member
    // until the workspace admits it as hers.
    let browser = carol.enrol("carol-browser", DeviceCapabilities::member());
    let page_key = browser.device_pubkey();
    let early = connect(&relay, &browser).await;
    assert_eq!(
        subscribe(&early, workspace_id).await,
        Err("device not in workspace membership".to_owned())
    );
    drop(early);

    // Admitted, it is a member device like a second desktop: it subscribes,
    // reads the room and what its members write live, and is answered.
    assert!(
        relay
            .workspaces()
            .unwrap()
            .admit_member_device(&room.workspace, &browser.cert)
            .await
            .unwrap(),
        "the workspace admits Carol's browser as her device"
    );
    let visiting = connect(&relay, &browser).await;
    assert_eq!(subscribe(&visiting, workspace_id).await, Ok(()));
    crate::common::wait_until_subscribed(&relay, workspace_id, &page_key).await;
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "welcome").unwrap();
    doc.commit_edit("fixture", "topic");
    let welcome = alice_peer
        .publication_mut()
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
    answer("Alice's write reaches the browser", &visiting, |envelope| {
        matches!(
            envelope,
            Envelope::Delta { author_device_public_key, .. }
                if author_device_public_key == laptop.device_pubkey()
        )
        .then_some(())
    })
    .await;
    visiting
        .send(Envelope::SyncRequest {
            workspace_id,
            have_heads: Vec::new(),
        })
        .unwrap();
    answer("the browser's sync is answered", &visiting, |envelope| {
        matches!(envelope, Envelope::SyncResponse { .. }).then_some(())
    })
    .await;
    // Asked what the room lets it change, the browser hears it changes
    // nothing: its person is a viewer.
    for node in tilo_action_policy::roles::telling_nodes(&room.workspace) {
        assert_eq!(
            relay
                .workspaces()
                .unwrap()
                .page_rooms(
                    &browser.cert,
                    tilo_relay_protocol::workspaces::WorkspacesRequest::Permits {
                        workspace: room.workspace.clone(),
                        node: node.to_string(),
                    },
                )
                .await,
            tilo_relay_protocol::workspaces::WorkspacesReply::Permits(false),
            "{node}"
        );
    }

    // A viewer's change is refused by name, and reaches nobody: the room's
    // host keeps what a member may change to their role.
    let surface = bob_peer.signed_surface(APP);
    let edit = change(
        &bob_peer,
        &room.workspace,
        surface,
        &browser,
        "topic",
        "a viewer's edit",
        1,
    );
    commit(&visiting, workspace_id, [3; 16], page_key, &edit);
    let refused = answer(
        "the viewer's change is refused",
        &visiting,
        |envelope| match envelope {
            Envelope::CommitRefused { op, reason, .. } if op == [3; 16] => Some(reason),
            _ => None,
        },
    )
    .await;
    assert_eq!(plain_reason(&refused), PLAIN_NOT_ALLOWED, "{refused}");

    // The room's members see Carol's browser as one of them: Bob's presence
    // reaches it, and its own reaches Alice, whose cursor it reads.
    bob_session
        .publish_presence(b"Bob's cursor".to_vec())
        .unwrap();
    let cursor = answer(
        "Bob's presence reaches the browser",
        &visiting,
        |envelope| match envelope {
            Envelope::RetiredPresence { payload, .. } => {
                tilo_relay::AttributedPresence::decode(&payload)
                    .filter(|presence| presence.author == phone.device_pubkey())
                    .map(|presence| presence.body)
            }
            _ => None,
        },
    )
    .await;
    assert_eq!(cursor, b"Bob's cursor");

    // Made an editor, the same browser writes: the room's members take its
    // change in, and Alice reads it.
    relay
        .workspaces()
        .unwrap()
        .set_member_role(
            &room.workspace,
            &laptop.cert,
            SetMemberRoleRequest {
                request_id: [4; 32],
                user_public_key: carol.user_pk,
                role: MemberRole::Editor,
            },
        )
        .await
        .unwrap();
    let written = change(
        &bob_peer,
        &room.workspace,
        surface,
        &browser,
        "topic",
        "the editor writes",
        2,
    );
    commit(&visiting, workspace_id, [5; 16], page_key, &written);
    receive_until(
        "the room takes in the browser's change as its editor",
        &alice_session,
        &mut alice_peer,
        &mut Vec::new(),
        |peer, _| text(peer, "topic").as_deref() == Some("the editor writes"),
    )
    .await;

    // A browser reads what the room's members read: the host answers it the
    // content those records reference.
    let image = b"an image a member posted".to_vec();
    let hash = blake3::hash(&image).to_hex().to_string();
    relay
        .workspaces()
        .expect("the host governs rooms")
        .serve_content(std::sync::Arc::new(Kept(image.clone())));
    let want = tilo_relay::AttributedPresence {
        author: page_key,
        body: postcard::to_stdvec(&NeighbourMsg::RecordContentWant {
            requester: tilo_peer_state::peer_handle(&page_key),
            app_id: "so.tilo.chat".into(),
            cluster_id: room.workspace.clone(),
            hash: hash.clone(),
        })
        .unwrap(),
    }
    .encode()
    .unwrap();
    visiting
        .send(Envelope::RetiredPresence {
            workspace_id,
            payload: want,
        })
        .unwrap();
    let piece = answer(
        "the host answers the browser's request",
        &visiting,
        |envelope| match envelope {
            Envelope::RecordContent { frame, .. } => Some(frame),
            _ => None,
        },
    )
    .await;
    let Ok(NeighbourMsg::RecordContentPiece { bytes, .. }) = postcard::from_bytes(&piece) else {
        panic!("a piece of content");
    };
    assert_eq!(bytes, image);

    alice_session.close().await;
    bob_session.close().await;
    relay.shutdown().await;
}
