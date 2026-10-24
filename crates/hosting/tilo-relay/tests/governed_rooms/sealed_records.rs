//! A member seals a write to a record the room's governor holds, as every
//! member of a room whose channels a node stewards does. The governor keeps
//! the ciphertext as the record's history and goes on governing: the next
//! person is still let in, and so is one after the relay restarts.
use super::*;
use channel_crypto::{ChannelGroup, MemberKey};
use collab_types::ContentRetention;

/// Seal `message`, a member's cleartext write, as its author's page does
/// once it holds the record channel's content keys.
fn sealed(
    message: NeighbourMsg,
    group: &ChannelGroup,
    key: &ed25519_dalek::SigningKey,
) -> NeighbourMsg {
    let NeighbourMsg::AppDocument {
        peer,
        app_id,
        cluster_id,
        record_id,
        mut envelope,
    } = message
    else {
        panic!("a document write")
    };
    let (metadata, payload) = group
        .seal_content(
            envelope.artifact,
            ContentRetention::Delta,
            &envelope.payload,
        )
        .unwrap();
    envelope.payload = payload;
    envelope.encryption = Some(metadata);
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer,
        app_id,
        cluster_id,
        record_id,
        envelope,
    }
}

/// `person`'s `device` redeems an invitation to `room` that `issuer` issues;
/// its replica, from the bootstrap alone.
pub(super) async fn join(
    relay: &Relay,
    issuer: &TestDevice,
    room: &str,
    person: &TestUser,
    device: &TestDevice,
    request_id: [u8; 32],
) -> Result<StatePeer, String> {
    let invitation = invite(relay, issuer, room, MemberRole::Editor, person.user_pk).await;
    let verified = decode_and_verify_invitation(
        &invitation.signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let admission = WorkspaceJoinClient::new(device_endpoint(device).await)
        .redeem_hint(
            &verified.grant().transport[0],
            invitation.signed_invitation.clone(),
            device.cert.clone(),
            &device.sk,
            request_id,
        )
        .await
        .map_err(|error| error.to_string())?;
    let bootstrap = admission.bootstrap;
    let mut peer = StatePeer::new(
        &bootstrap.room,
        bootstrap.authority.clone(),
        bootstrap.certified_roster.clone(),
        RevocationList::new(),
        SystemTime::now(),
    )?;
    peer.select_admission_v5()
        .map_err(|error| error.to_string())?;
    for bytes in &bootstrap.governance_messages {
        peer.receive_from(
            TransportOrigin::RetainedRelayDelta(bootstrap.authority.device_public_key),
            postcard::from_bytes(bytes).map_err(|error| error.to_string())?,
            SystemTime::now(),
        )?;
    }
    Ok(peer)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sealed_write_to_a_governed_record_never_keeps_the_next_person_out() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let room = create_and_enter(&relay, &host, &laptop, "Lounge").await;
    let mut bob_peer = join(&relay, &laptop, &room.room, &bob, &phone, [7; 32])
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
    let alice_session = session(&relay, &laptop, room.workspace_id).await;
    let bob_session = session(&relay, &phone, room.workspace_id).await;
    let mut presence = Vec::new();
    receive_until(
        "Alice's replica admits Bob",
        &alice_session,
        &mut alice_peer,
        &mut presence,
        |peer, _| role(peer, phone.device_pubkey()) == Some(collab_types::Role::Editor),
    )
    .await;
    crate::common::wait_until_subscribed(&relay, room.workspace_id, &phone.device_pubkey()).await;

    // Alice's first write makes the record, and the governor holds it.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "welcome").unwrap();
    doc.commit_edit("fixture", "topic");
    let message = alice_peer
        .publication_mut()
        .publish_incremental(APP, "", doc.save_after(&[]), &laptop.sk, SystemTime::now())
        .unwrap();
    publish(&alice_session, &message);
    receive_until(
        "the record at Bob",
        &bob_session,
        &mut bob_peer,
        &mut presence,
        |peer, _| text(peer, "topic").as_deref() == Some("welcome"),
    )
    .await;

    // Bob's write leaves his device sealed; the governor takes it in.
    let rooms = relay.rooms().unwrap();
    let mut followed = rooms.record_writes();
    let group = ChannelGroup::create(
        collab_types::document_channel_id(APP, &room.room, ""),
        MemberKey::generate("bob").unwrap(),
    )
    .unwrap();
    let cleartext = write(&mut bob_peer, "reply", "hi", &phone.sk);
    publish(&bob_session, &sealed(cleartext, &group, &phone.sk));
    tilo_test_env::within("the governor follows Bob's sealed write", async {
        while followed.recv().await.unwrap().app != APP {}
    })
    .await;

    // A page writes its records as commits: the governor follows those too.
    let bob_client = bob_session.into_client();
    let message = sealed(
        write(&mut bob_peer, "reply", "again", &phone.sk),
        &group,
        &phone.sk,
    );
    let (channel, _) = neighbour_wire::retention(&message, false).unwrap();
    let delta = tilo_relay::CoeditPayload::RetainedDelta {
        channel,
        body: postcard::to_stdvec(&message).unwrap(),
    }
    .encode()
    .unwrap();
    bob_client
        .send(Envelope::Commit {
            workspace_id: room.workspace_id,
            op: [5; 16],
            author_device_public_key: phone.device_pubkey(),
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta,
        })
        .unwrap();
    tilo_test_env::within("the governor follows Bob's sealed commit", async {
        while followed.recv().await.unwrap().app != APP {}
    })
    .await;

    // The next person is let in, by a governance change published after it.
    let carol = TestUser::fresh("Carol");
    let tablet = carol.enrol("carol-tablet", DeviceCapabilities::member());
    join(&relay, &laptop, &room.room, &carol, &tablet, [8; 32])
        .await
        .expect("a sealed record write keeps nobody out");
    let seated = |members: &[tilo_peer_state::GovernedMember], person: [u8; 32]| {
        members
            .iter()
            .any(|member| member.user_public_key == person && !member.removed)
    };
    assert!(seated(
        &rooms.summary(&room.room).await.unwrap().members,
        carol.user_pk
    ));

    // Reopened, the governor replays the sealed write and still lets people in.
    alice_session.close().await;
    drop(bob_client);
    relay.shutdown().await;
    let relay = start(state.path(), &alice, &host).await;
    let dan = TestUser::fresh("Dan");
    let watch = dan.enrol("dan-watch", DeviceCapabilities::member());
    join(&relay, &laptop, &room.room, &dan, &watch, [9; 32])
        .await
        .expect("a reopened room with a sealed record keeps nobody out");
    assert!(seated(
        &relay
            .rooms()
            .unwrap()
            .summary(&room.room)
            .await
            .unwrap()
            .members,
        dan.user_pk
    ));
    relay.shutdown().await;
}
