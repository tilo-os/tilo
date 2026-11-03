//! A valid signed foreign device must not inherit its person's private access.
use super::*;

#[tokio::test]
async fn signed_foreign_device_is_denied_over_quic_while_local_editor_keeps_access() {
    let directory = tempfile::tempdir().unwrap();
    let (governor_key, governor_cert) = tests::governor();
    exercise_foreign_device(
        tests::test_host(directory.path(), &governor_key, &governor_cert).await,
        &governor_key,
        &governor_cert,
    )
    .await;
}

async fn exercise_foreign_device(
    relay: crate::workspaces::fixtures::TestHost,
    governor_key: &SigningKey,
    governor_cert: &DeviceCert,
) {
    let workspaces = relay.workspaces().clone();
    let created = workspaces.create("Foreign private caller").await.unwrap();
    let (alice_key, alice_cert) = identity(31, None);
    let (bob_key, bob_cert) = identity(41, None);
    let alice = NativeEditor::join(
        &workspaces,
        &created.workspace,
        governor_cert,
        &alice_key,
        &alice_cert,
    )
    .await;
    let bob = NativeEditor::join(
        &workspaces,
        &created.workspace,
        governor_cert,
        &bob_key,
        &bob_cert,
    )
    .await;
    // Same person, different transport key, and a genuinely root-signed
    // foreign certificate: refusal must not come from a forged certificate,
    // missing membership, or an endpoint/certificate key mismatch.
    let foreign_key = SigningKey::from_bytes(&[35; 32]);
    let mut foreign_cert = alice_cert.clone();
    foreign_cert.device_public_key = foreign_key.verifying_key().to_bytes();
    foreign_cert.capabilities.foreign = true;
    sign_device_cert(&mut foreign_cert, &SigningKey::from_bytes(&[31; 32])).unwrap();
    let foreign = NativeEditor::join(
        &workspaces,
        &created.workspace,
        governor_cert,
        &foreign_key,
        &foreign_cert,
    )
    .await;
    {
        let state = workspaces.workspace(&created.workspace).await.unwrap();
        let workspace = state.lock().await;
        assert!(workspace.peer().certificates().contains(&foreign_cert));
        let member = workspace
            .peer()
            .permissions(foreign_cert.device_public_key, SystemTime::now())
            .unwrap();
        drop(workspace);
        assert!(member.foreign);
        assert!(member.delegate.is_none());
        assert_eq!(member.role, collab_types::Role::Editor);
    }
    let server = endpoint(governor_key).await;
    let router = iroh::protocol::Router::builder(server.clone())
        .accept(PRIVATE_ALPN, PrivateProtocol::new(workspaces.clone()))
        .spawn();
    drop(workspaces);
    let address = server.addr();
    let peer = hex::encode(bob_cert.user_public_key);
    let invocation = uuid::Uuid::new_v4();
    assert!(matches!(
        rpc(
            &alice.client,
            &address,
            &created.workspace,
            PrivateOperation::Send {
                peer: peer.clone(),
                invocation,
                text: BODY.into(),
            },
        )
        .await,
        PrivateReply::Sent
    ));
    assert_foreign_operations_refused(
        &foreign.client,
        &address,
        &created.workspace,
        &peer,
        invocation,
    )
    .await;
    assert_editors_keep_access(
        &alice.client,
        &bob.client,
        &address,
        &created.workspace,
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
    workspace: &str,
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
            rpc(foreign, address, workspace, operation).await,
            PrivateReply::Refused
        ));
    }
}

async fn assert_editors_keep_access(
    alice: &PrivateClient,
    bob: &PrivateClient,
    address: &iroh::EndpointAddr,
    workspace: &str,
    peer: &str,
    alice_peer: &str,
) {
    let retained = conversation(
        rpc(
            alice,
            address,
            workspace,
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
            workspace,
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
            workspace,
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
