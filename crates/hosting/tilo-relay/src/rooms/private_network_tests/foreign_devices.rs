//! A valid signed foreign device must not inherit its person's private access.
use super::*;

#[tokio::test]
async fn signed_foreign_device_is_denied_over_quic_while_local_editor_keeps_access() {
    let directory = tempfile::tempdir().unwrap();
    let (governor_key, governor_cert) = tests::governor();
    exercise_foreign_device(
        tests::view_read_relay(directory.path(), &governor_key, &governor_cert).await,
        &governor_key,
        &governor_cert,
    )
    .await;
}

async fn exercise_foreign_device(
    relay: crate::daemon::Relay,
    governor_key: &SigningKey,
    governor_cert: &DeviceCert,
) {
    let rooms = relay.rooms().unwrap().clone();
    let created = rooms.create("Foreign private caller").await.unwrap();
    let (alice_key, alice_cert) = identity(31, None);
    let (bob_key, bob_cert) = identity(41, None);
    let alice = NativeEditor::join(
        &rooms,
        &created.room,
        governor_cert,
        &alice_key,
        &alice_cert,
    )
    .await;
    let bob = NativeEditor::join(&rooms, &created.room, governor_cert, &bob_key, &bob_cert).await;
    // Same person, different transport key, and a genuinely root-signed
    // foreign certificate: refusal must not come from a forged certificate,
    // missing membership, or an endpoint/certificate key mismatch.
    let foreign_key = SigningKey::from_bytes(&[35; 32]);
    let mut foreign_cert = alice_cert.clone();
    foreign_cert.device_public_key = foreign_key.verifying_key().to_bytes();
    foreign_cert.capabilities.foreign = true;
    sign_device_cert(&mut foreign_cert, &SigningKey::from_bytes(&[31; 32])).unwrap();
    let foreign = NativeEditor::join(
        &rooms,
        &created.room,
        governor_cert,
        &foreign_key,
        &foreign_cert,
    )
    .await;
    {
        let state = rooms.room(&created.room).await.unwrap();
        let room = state.lock().await;
        assert!(room.peer().certificates().contains(&foreign_cert));
        let member = room
            .peer()
            .permissions(foreign_cert.device_public_key, SystemTime::now())
            .unwrap();
        drop(room);
        assert!(member.foreign);
        assert!(member.delegate.is_none());
        assert_eq!(member.role, collab_types::Role::Editor);
    }
    let server = endpoint(governor_key).await;
    let router = iroh::protocol::Router::builder(server.clone())
        .accept(PRIVATE_ALPN, PrivateProtocol::new(rooms.clone()))
        .spawn();
    drop(rooms);
    let address = server.addr();
    let peer = hex::encode(bob_cert.user_public_key);
    let invocation = uuid::Uuid::new_v4();
    assert!(matches!(
        rpc(
            &alice.client,
            &address,
            &created.room,
            PrivateOperation::Send {
                peer: peer.clone(),
                invocation,
                text: BODY.into(),
            },
        )
        .await,
        PrivateReply::Sent
    ));
    assert_foreign_operations_refused(&foreign.client, &address, &created.room, &peer, invocation)
        .await;
    assert_editors_keep_access(
        &alice.client,
        &bob.client,
        &address,
        &created.room,
        &peer,
        &hex::encode(alice_cert.user_public_key),
    )
    .await;
    alice.endpoint.close().await;
    bob.endpoint.close().await;
    foreign.endpoint.close().await;
    router.shutdown().await.unwrap();
    relay.shutdown().await;
}

async fn assert_foreign_operations_refused(
    foreign: &PrivateClient,
    address: &iroh::EndpointAddr,
    room: &str,
    peer: &str,
    invocation: uuid::Uuid,
) {
    // Include the exact accepted invocation as well as a fresh send: neither
    // an idempotency hit nor a retained pair may bypass current authorization.
    for operation in [
        PrivateOperation::Contacts,
        PrivateOperation::Open { peer: peer.into() },
        PrivateOperation::Read {
            peer: peer.into(),
            after_sequence: 0,
            limit: 50,
        },
        PrivateOperation::Watch {
            peer: peer.into(),
            after_sequence: 1,
        },
        PrivateOperation::Send {
            peer: peer.into(),
            invocation,
            text: BODY.into(),
        },
        PrivateOperation::Send {
            peer: peer.into(),
            invocation: uuid::Uuid::new_v4(),
            text: "foreign write must not persist".into(),
        },
    ] {
        assert!(matches!(
            rpc(foreign, address, room, operation).await,
            PrivateReply::Refused
        ));
    }
}

async fn assert_editors_keep_access(
    alice: &PrivateClient,
    bob: &PrivateClient,
    address: &iroh::EndpointAddr,
    room: &str,
    peer: &str,
    alice_peer: &str,
) {
    let retained = conversation(
        rpc(
            alice,
            address,
            room,
            PrivateOperation::Read {
                peer: peer.into(),
                after_sequence: 0,
                limit: 50,
            },
        )
        .await,
    );
    assert_eq!(retained.messages.len(), 1);
    assert_eq!(retained.messages[0].text, BODY);
    assert!(matches!(
        rpc(
            alice,
            address,
            room,
            PrivateOperation::Watch {
                peer: peer.into(),
                after_sequence: 0,
            },
        )
        .await,
        PrivateReply::Changed(1)
    ));
    let received = conversation(
        rpc(
            bob,
            address,
            room,
            PrivateOperation::Read {
                peer: alice_peer.into(),
                after_sequence: 0,
                limit: 50,
            },
        )
        .await,
    );
    assert_eq!(received.messages.len(), 1);
    assert_eq!(received.messages[0].id, retained.messages[0].id);
}
