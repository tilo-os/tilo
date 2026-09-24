use super::*;
use ed25519_dalek::SigningKey;
use qos_user_identity::{
    DeviceCapabilities, DeviceCert,
    cloud_enrollment::{EnrollmentRequest, host_capabilities, sign_request},
    sign_device_cert,
};
pub(super) fn approved(
    challenge: qos_user_identity::cloud_enrollment::Challenge,
    root: &SigningKey,
    device: &SigningKey,
) -> EnrollmentRequest {
    let certificate = |key, caps| {
        let mut c = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key,
            device_label: "Test".into(),
            capabilities: caps,
            issued_at_ns: 10,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut c, root).unwrap();
        c
    };
    let mut r = EnrollmentRequest {
        version: 1,
        host_certificate: challenge
            .host_certificate
            .clone()
            .unwrap_or_else(|| certificate(challenge.host_public_key, host_capabilities())),
        browser_certificate: certificate(
            device.verifying_key().to_bytes(),
            DeviceCapabilities::member(),
        ),
        challenge,
        root_signature: vec![],
        device_signature: vec![],
    };
    sign_request(&mut r, root, device).unwrap();
    r
}
fn apply_live_publications(receiver: &mut StatePeer, publications: &[StoredPublication], now: u64) {
    let authority = receiver.authority().device_public_key;
    for publication in publications
        .iter()
        .filter(|publication| publication.kind != PublicationKind::Checkpoint)
    {
        receiver
            .receive_from(
                TransportOrigin::RetainedRelayDelta(authority),
                publication.message.clone(),
                now_time(now),
            )
            .unwrap();
    }
}
fn receiver_from_join_bootstrap(
    bootstrap: &qos_peer_state::join::JoinBootstrap,
    now: u64,
) -> StatePeer {
    let mut receiver = StatePeer::new(
        &bootstrap.room,
        bootstrap.authority.clone(),
        vec![bootstrap.authority.clone()],
        RevocationList::new(),
        now_time(now),
    )
    .unwrap();
    for bytes in &bootstrap.governance_messages {
        receiver
            .receive_from(
                TransportOrigin::RelayDelta(bootstrap.authority.device_public_key),
                postcard::from_bytes(bytes).unwrap(),
                now_time(now),
            )
            .unwrap();
    }
    receiver
}
pub(crate) fn invite_and_admit(
    store: &mut Store,
    owner_device: &SigningKey,
    guest_root: &SigningKey,
    guest_device: &SigningKey,
    invocation: &str,
    request_id: [u8; 32],
    now: u64,
) -> (DeviceCert, qos_peer_state::join::JoinBootstrap) {
    use qos_peer_state::join::{
        JoinChallengeRequest, JoinCompleteRequest, MemberRole, decode_and_verify_invitation_text,
        sign_join_proof,
    };
    let output = store
        .action(
            owner_device.verifying_key().to_bytes(),
            &ActionRequest {
                version: 1,
                room: store.state.room.clone(),
                invocation_id: invocation.into(),
                operation: NativeOperation::CollaborationInvite {
                    role: MemberRole::Editor,
                    recipient_user_public_key: Some(guest_root.verifying_key().to_bytes()),
                    invitee_name: Some(invocation.into()),
                    expires_at_ns: now + 600_000_000_000,
                },
            },
            now,
        )
        .unwrap()
        .output
        .unwrap();
    let invitation = decode_and_verify_invitation_text(
        output["signed_invitation"].as_str().unwrap(),
        Some(guest_root.verifying_key().to_bytes()),
        &RevocationList::new(),
        now + 1,
    )
    .unwrap()
    .signed_invitation()
    .to_vec();
    let mut cert = DeviceCert {
        user_public_key: guest_root.verifying_key().to_bytes(),
        device_public_key: guest_device.verifying_key().to_bytes(),
        device_label: invocation.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, guest_root).unwrap();
    let challenge = store
        .join_challenge(
            guest_device.verifying_key().to_bytes(),
            JoinChallengeRequest {
                signed_invitation: invitation,
                device_cert: cert.clone(),
                request_id,
            },
            now + 1,
        )
        .unwrap();
    let proof = sign_join_proof(challenge.challenge, guest_device).unwrap();
    let bootstrap = store
        .join_complete(
            guest_device.verifying_key().to_bytes(),
            JoinCompleteRequest { proof },
            now + 2,
        )
        .unwrap();
    (cert, bootstrap)
}
#[test]
fn enrollment_is_durable_idempotent_and_cannot_replace_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut store = Store::open(dir.path(), "machine-a").unwrap();
    let challenge = store.inspect(10).unwrap();
    let request = approved(challenge, &root, &device);
    let result = store.apply(&request, 11).unwrap();
    let mut directory = WorkspaceDirectory::new(
        root.verifying_key().to_bytes(),
        device.verifying_key().to_bytes(),
    );
    let update: qos_user_identity::DirectoryUpdate =
        serde_json::from_value(result["bootstrap"]["directory_update"].clone()).unwrap();
    directory
        .apply_incremental(
            &update,
            &request.host_certificate,
            &RevocationList::new(),
            11,
        )
        .unwrap();
    assert_eq!(directory.list_workspaces().len(), 1);
    assert_eq!(store.apply(&request, 12).unwrap(), result);
    drop(store);
    let mut store = Store::open(dir.path(), "machine-a").unwrap();
    assert_eq!(store.apply(&request, 13).unwrap(), result);
    assert_eq!(store.peer().unwrap().rows().len(), 2);
    let other = SigningKey::from_bytes(&[9; 32]);
    let new = approved(store.inspect(20).unwrap(), &other, &device);
    assert!(store.apply(&new, 21).is_err());
    let mut altered = request.clone();
    altered.browser_certificate.device_label = "replacement".into();
    sign_request(&mut altered, &root, &device).unwrap();
    assert!(store.apply(&altered, 22).is_err());
    drop(store);
    assert!(Store::open(dir.path(), "machine-b").is_err());
}
#[test]
fn corrupt_state_is_never_reinitialized() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("state.json"), b"broken").unwrap();
    assert!(Store::open(dir.path(), "machine-a").is_err());
}
#[test]
fn native_writer_action_is_attributed_durable_and_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut store = Store::open(dir.path(), "machine-a").unwrap();
    let r = approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&r, 11).unwrap();
    let before = store.peer().unwrap().rows();
    let command = ActionRequest {
        version: 1,
        room: r.challenge.room.clone(),
        invocation_id: "edit-1".into(),
        operation: NativeOperation::WriterInsert {
            text: "real native edit".into(),
        },
    };
    let receipt = store
        .action(device.verifying_key().to_bytes(), &command, 12)
        .unwrap();
    assert_eq!(receipt.status, "completed");
    assert_ne!(before, store.peer().unwrap().rows());
    let heads = store.peer().unwrap().rows();
    assert_eq!(
        receipt,
        store
            .action(device.verifying_key().to_bytes(), &command, 13)
            .unwrap()
    );
    assert_eq!(heads, store.peer().unwrap().rows());
    assert!(store.action([99; 32], &command, 14).is_err());
    drop(store);
    let mut store = Store::open(dir.path(), "machine-a").unwrap();
    assert_eq!(heads, store.peer().unwrap().rows());
    assert_eq!(
        receipt,
        store
            .action(device.verifying_key().to_bytes(), &command, 15)
            .unwrap()
    );
}
#[test]
fn cancellation_and_failure_are_terminal_and_chat_uses_real_document() {
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let source = device.verifying_key().to_bytes();
    let mut store = Store::open(dir.path(), "machine-a").unwrap();
    let r = approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&r, 11).unwrap();
    let request = |id: &str, operation| ActionRequest {
        version: 1,
        room: r.challenge.room.clone(),
        invocation_id: id.into(),
        operation,
    };
    let before = store.peer().unwrap().rows();
    store
        .action(
            source,
            &request(
                "cancel",
                NativeOperation::Cancel {
                    invocation_id: "never".into(),
                },
            ),
            12,
        )
        .unwrap();
    assert_eq!(
        store
            .action(
                source,
                &request(
                    "never",
                    NativeOperation::WriterInsert {
                        text: "must not execute".into()
                    }
                ),
                13
            )
            .unwrap()
            .status,
        "cancelled"
    );
    assert_eq!(before, store.peer().unwrap().rows());
    let bad = request(
        "bad",
        NativeOperation::ChatSend {
            channel: "c_general".into(),
            text: String::new(),
        },
    );
    assert_eq!(store.action(source, &bad, 14).unwrap().status, "failed");
    assert_eq!(before, store.peer().unwrap().rows());
    let good = request(
        "chat",
        NativeOperation::ChatSend {
            channel: "c_general".into(),
            text: "native message".into(),
        },
    );
    assert_eq!(store.action(source, &good, 15).unwrap().status, "completed");
    assert_ne!(before, store.peer().unwrap().rows());
    let mut substitute = good.clone();
    substitute.operation = NativeOperation::WriterInsert {
        text: "substitute".into(),
    };
    assert!(store.action(source, &substitute, 16).is_err());
}
#[test]
fn second_device_joins_existing_state_and_revocation_survives_restart() {
    use qos_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let second = SigningKey::from_bytes(&[9; 32]);
    let mut store = Store::open(dir.path(), "machine-a").unwrap();
    let first = approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&first, 11).unwrap();
    let mut live_receiver = store.peer_at(11).unwrap();
    let next = approved(store.inspect(20).unwrap(), &root, &second);
    let second_start = store.state.outbox.len();
    store.apply(&next, 21).unwrap();
    let second_live = store.state.outbox[second_start..].to_vec();
    assert_eq!(second_live[0].kind, PublicationKind::Delta);
    apply_live_publications(&mut live_receiver, &second_live, 22);
    assert!(
        live_receiver
            .permissions(second.verifying_key().to_bytes(), now_time(22))
            .is_ok(),
        "a current receiver must learn the newly enrolled device without another checkpoint"
    );
    assert!(
        store
            .peer()
            .unwrap()
            .permissions(second.verifying_key().to_bytes(), now_time(22))
            .is_ok()
    );
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: second.verifying_key().to_bytes(),
        revoked_at_ns: 22,
        reason: RevocationReason::Lost,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &root).unwrap();
    store
        .receive(
            TransportOrigin::RelayDelta(device.verifying_key().to_bytes()),
            NeighbourMsg::IdentityHello {
                cert: Box::new(first.browser_certificate),
                revocations: vec![entry],
            },
            23,
        )
        .unwrap();
    drop(store);
    let store = Store::open(dir.path(), "machine-a").unwrap();
    assert!(
        store
            .peer()
            .unwrap()
            .permissions(second.verifying_key().to_bytes(), now_time(24))
            .is_err()
    );
}

#[tokio::test]
async fn addressed_native_action_requires_device_proof_and_returns_private_receipt() {
    use iroh::{Endpoint, RelayMode, endpoint::presets};
    use qos_relay::{
        auth,
        framing::{read_frame, write_frame},
    };
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let dir = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[7; 32]);
        let device = SigningKey::from_bytes(&[8; 32]);
        let now = qos_relay::clock::now_ns();
        let r = {
            let mut store = Store::open(dir.path(), "network-test").unwrap();
            let r = approved(store.inspect(now).unwrap(), &root, &device);
            store.apply(&r, now).unwrap();
            r
        };
        let router = action_server::start(dir.path(), "network-test", RelayMode::Disabled)
            .await
            .unwrap();
        let port = router
            .endpoint()
            .bound_sockets()
            .iter()
            .find(|s| s.is_ipv4())
            .unwrap()
            .port();
        let address = iroh_base::EndpointAddr::new(router.endpoint().id())
            .with_ip_addr(std::net::SocketAddr::from(([127, 0, 0, 1], port)));
        let client = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        for valid in [false, true] {
            let connection = client
                .connect(address.clone(), actions::ACTION_ALPN)
                .await
                .unwrap();
            let (mut send, mut recv) = connection.open_bi().await.unwrap();
            let nonce = [17; 32];
            write_frame(
                &mut send,
                &postcard::to_stdvec(&auth::Hello {
                    cert: r.browser_certificate.clone(),
                    client_nonce: nonce,
                })
                .unwrap(),
            )
            .await
            .unwrap();
            let ack: auth::HelloAck =
                postcard::from_bytes(&read_frame(&mut recv, 8192).await.unwrap()).unwrap();
            assert_eq!(ack.cert, r.host_certificate);
            auth::verify_pinned_host_ack(
                auth::AuthDomain::LegacyQos,
                &ack,
                &RevocationList::new(),
                now,
                &nonce,
            )
            .unwrap();
            let key = if valid {
                device.clone()
            } else {
                SigningKey::from_bytes(&[99; 32])
            };
            write_frame(
                &mut send,
                &postcard::to_stdvec(&auth::build_hello_ack2(
                    auth::AuthDomain::LegacyQos,
                    &key,
                    &ack.server_nonce,
                ))
                .unwrap(),
            )
            .await
            .unwrap();
            let command = ActionRequest {
                version: 1,
                room: r.challenge.room.clone(),
                invocation_id: "network-native".into(),
                operation: NativeOperation::WriterInsert {
                    text: "addressed native".into(),
                },
            };
            let sent = write_frame(&mut send, &serde_json::to_vec(&command).unwrap()).await;
            if valid {
                sent.unwrap();
                let receipt: ActionReceipt =
                    serde_json::from_slice(&read_frame(&mut recv, 64 * 1024).await.unwrap())
                        .unwrap();
                assert_eq!(receipt.status, "completed");
            } else if sent.is_ok() {
                assert!(read_frame(&mut recv, 64 * 1024).await.is_err());
            }
            connection.close(0u32.into(), b"done");
        }
        let store = Store::open(dir.path(), "network-test").unwrap();
        assert_eq!(store.state.receipts.len(), 1);
        drop(store);
        client.close().await;
        router.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}
#[test]
fn initial_records_pass_the_real_browser_owner_admission() {
    use qos_desktop::{WorkspaceTree, projection::RoomProjection};
    use ui::{
        portable_storage::{PortableStorage, StorageKey},
        record::{RecordCheckout, RecordId, RecordOwners},
    };
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut store = Store::open(dir.path(), "record-test").unwrap();
    let request = approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&request, 11).unwrap();
    let peer = store.peer().unwrap();
    let admitted = peer.admitted_room().unwrap();
    let mut tree = WorkspaceTree::default();
    RoomProjection::apply_admitted_room(
        &mut tree,
        qos_desktop::RoomId::parse(peer.room()).unwrap(),
        admitted,
    )
    .unwrap();
    let routes = RoomProjection::record_routes(admitted);
    assert_eq!(tree.workspaces.len(), 1);
    let tabs = tree.workspaces[0]
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .collect::<Vec<_>>();
    assert_eq!(tabs.len(), 2);
    let writer = tabs
        .iter()
        .find(|tab| {
            qos_desktop::resolver::canonical_app_id(&tab.tile).as_deref()
                == Some("so.tilo.word-processor")
        })
        .unwrap();
    let chat = tabs
        .iter()
        .find(|tab| {
            qos_desktop::resolver::canonical_app_id(&tab.tile).as_deref() == Some("so.tilo.chat")
        })
        .unwrap();
    assert_eq!(
        routes.get(&writer.id).map(String::as_str),
        Some("tab:0000000000000001")
    );
    assert_eq!(routes.get(&chat.id).map(String::as_str), Some(""));
    let cache = PortableStorage::new("record-validation", peer.room()).unwrap();
    let mut owners = RecordOwners::portable(cache.clone());
    for row in peer.rows() {
        let definition = if row.app_id == "so.tilo.chat" {
            chat::definition()
        } else {
            word_processor::definition()
        };
        let (key, checkout) = if row.record_id.is_empty() {
            (
                StorageKey::Primary {
                    package: row.app_id.clone(),
                    workspace: peer.room().into(),
                },
                RecordCheckout::package_primary(definition.record_kind.clone()),
            )
        } else {
            (
                StorageKey::Record {
                    package: row.app_id.clone(),
                    workspace: peer.room().into(),
                    record: row.record_id.clone(),
                },
                RecordCheckout::new(
                    RecordId::parse(&row.record_id).unwrap(),
                    definition.record_kind.clone(),
                ),
            )
        };
        cache
            .put(
                key,
                peer.document(&row.app_id, &row.record_id).unwrap(),
                Some(device.to_bytes().to_vec()),
            )
            .unwrap();
        owners
            .open(&row.app_id, peer.room())
            .unwrap()
            .try_open_record(&checkout)
            .unwrap();
    }
}

#[test]
fn legacy_enrolled_workspace_gains_desktop_topology_once_without_replacing_state() {
    use automerge::{ROOT, ReadDoc};
    use qos_desktop::projection::RoomProjection;
    use qos_peer_state::{SeedDocument, SeedState, StatePeer};

    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[31; 32]);
    let browser = SigningKey::from_bytes(&[32; 32]);
    let mut store = Store::open(dir.path(), "legacy-desktop").unwrap();
    let request = approved(store.inspect(10).unwrap(), &root, &browser);
    store.apply(&request, 11).unwrap();
    let action = ActionRequest {
        version: 1,
        room: request.challenge.room.clone(),
        invocation_id: "legacy-writer".into(),
        operation: NativeOperation::WriterInsert {
            text: "preserve this edit".into(),
        },
    };
    store
        .action(browser.verifying_key().to_bytes(), &action, 12)
        .unwrap();

    // Model the deployed pre-topology state with the real governed room seed
    // and the exact current canonical record contents.
    let current = store.peer().unwrap();
    let documents = current
        .rows()
        .into_iter()
        .map(|row| {
            let doc = current
                .document(&row.app_id, &row.record_id)
                .unwrap()
                .clone();
            SeedDocument {
                kind: if row.app_id == "so.tilo.chat" {
                    chat::definition().record_kind.as_str().into()
                } else {
                    word_processor::definition().record_kind.as_str().into()
                },
                app_id: row.app_id,
                record_id: row.record_id,
                bytes: doc,
            }
        })
        .collect();
    let host_key = store.key();
    let authority = store.state.bound.as_ref().unwrap().authority.clone();
    let mut legacy = StatePeer::new(
        &store.state.room,
        authority.clone(),
        vec![authority.clone()],
        RevocationList::new(),
        now_time(13),
    )
    .unwrap();
    legacy
        .install_seed(
            SeedState {
                room_changes: qos_peer_state::seed_room(
                    &store.state.room,
                    &authority,
                    std::slice::from_ref(&authority),
                )
                .unwrap(),
                documents,
            },
            &host_key,
            now_time(13),
        )
        .unwrap();
    legacy
        .admit_certificate(request.browser_certificate.clone(), &host_key, now_time(13))
        .unwrap();
    assert!(
        legacy
            .admitted_room()
            .unwrap()
            .get(ROOT, "contents")
            .unwrap()
            .is_none()
    );
    let expected_rows = legacy.rows();
    let expected_receipts = store.state.receipts.clone();
    let mut state = store.state.clone();
    state.bound.as_mut().unwrap().peer = legacy.durable().unwrap();
    store.commit(state).unwrap();
    drop(store);

    let error = atomic_file::test_support::with_failure(
        atomic_file::test_support::Checkpoint::BeforeCommit,
        || Store::open(dir.path(), "legacy-desktop"),
    )
    .err()
    .expect("desktop migration must report its uncertain publication");
    assert!(error.is::<UncertainStoreCommit>());
    let replaced = std::fs::read(dir.path().join("state.json")).unwrap();
    let store = Store::open(dir.path(), "legacy-desktop").unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        replaced,
        "reopen must reconcile the migration, not publish another generation"
    );
    let migrated = store.peer().unwrap();
    assert_eq!(migrated.rows(), expected_rows);
    assert_eq!(store.state.receipts, expected_receipts);
    let room = migrated.admitted_room().unwrap();
    assert!(room.get(ROOT, "contents").unwrap().is_some());
    let routes = RoomProjection::record_routes(room);
    assert!(
        routes
            .values()
            .any(|record| record == "tab:0000000000000001")
    );
    assert!(routes.values().any(String::is_empty));
    let durable = store.state.bound.as_ref().unwrap().peer.clone();
    let outbox_len = store.state.outbox.len();
    let generation = store.state.generation;
    drop(store);

    let reopened = Store::open(dir.path(), "legacy-desktop").unwrap();
    assert_eq!(reopened.state.bound.as_ref().unwrap().peer, durable);
    assert_eq!(reopened.state.outbox.len(), outbox_len);
    assert_eq!(reopened.state.generation, generation);
    assert_eq!(reopened.state.receipts, expected_receipts);
}

#[test]
fn addressed_actions_honor_native_artifact_restrictions() {
    use automerge::{ReadDoc, transaction::Transactable};
    use collab_types::caps::{CapOverride, Effect, Scope, Subject, Verb};
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut store = Store::open(dir.path(), "restriction-test").unwrap();
    let request = approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&request, 11).unwrap();
    let mut peer = store.peer().unwrap();
    let NeighbourMsg::SyncResp { changes, .. } = peer.room_message() else {
        unreachable!()
    };
    let mut governance = qos_peer_state::room::candidate(None, &changes).unwrap();
    let caps = governance
        .get(automerge::ROOT, "capabilities")
        .unwrap()
        .unwrap()
        .1;
    let restriction = CapOverride::new(
        Subject::User(collab_types::Ed25519PublicKey::from_bytes(
            root.verifying_key().to_bytes(),
        )),
        Scope::ResourceInstance(collab_types::document_artifact_id(
            "so.tilo.word-processor",
            peer.room(),
            "tab:0000000000000001",
        )),
        Effect::Restrict,
        Verb::Read,
    );
    governance
        .put(
            &caps,
            "deny-writer",
            automerge::ScalarValue::Bytes(serde_json::to_vec(&restriction).unwrap()),
        )
        .unwrap();
    governance.commit();
    let changes = governance
        .get_changes(&[])
        .iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    peer.receive_from(
        TransportOrigin::RelayDelta(store.key().verifying_key().to_bytes()),
        NeighbourMsg::SyncResp {
            room: peer.room().into(),
            changes,
        },
        now_time(12),
    )
    .unwrap();
    let mut state = store.state.clone();
    state.bound.as_mut().unwrap().peer = peer.durable().unwrap();
    store.commit(state).unwrap();
    let before = store.peer().unwrap().rows();
    let command = ActionRequest {
        version: 1,
        room: request.challenge.room,
        invocation_id: "denied".into(),
        operation: NativeOperation::WriterInsert {
            text: "denied".into(),
        },
    };
    assert!(
        store
            .action(device.verifying_key().to_bytes(), &command, 13)
            .is_err()
    );
    assert_eq!(before, store.peer().unwrap().rows());
    let catalogue = store
        .action(
            device.verifying_key().to_bytes(),
            &ActionRequest {
                version: 1,
                room: command.room,
                invocation_id: "catalogue".into(),
                operation: NativeOperation::Catalogue,
            },
            14,
        )
        .unwrap();
    assert!(
        !catalogue.output.unwrap()["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|op| op["kind"] == "writer_insert")
    );
}

#[test]
fn expired_browser_does_not_brick_workspace_restart_or_new_enrollment() {
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let expired = SigningKey::from_bytes(&[8; 32]);
    let fresh = SigningKey::from_bytes(&[9; 32]);
    let now = qos_relay::clock::now_ns();
    let mut store = Store::open(dir.path(), "expiry-test").unwrap();
    let mut request = approved(store.inspect(now).unwrap(), &root, &expired);
    request.browser_certificate.expires_at_ns = Some(now + 1);
    sign_device_cert(&mut request.browser_certificate, &root).unwrap();
    sign_request(&mut request, &root, &expired).unwrap();
    store.apply(&request, now).unwrap();
    assert!(store.peer_at(now + 2).is_ok());
    assert!(
        store
            .peer_at(now + 2)
            .unwrap()
            .permissions(expired.verifying_key().to_bytes(), now_time(now + 2))
            .is_err()
    );
    drop(store);
    let mut store = Store::open(dir.path(), "expiry-test").unwrap();
    let fresh_request = approved(store.inspect(now + 2).unwrap(), &root, &fresh);
    store.apply(&fresh_request, now + 2).unwrap();
    assert!(
        store
            .peer_at(now + 3)
            .unwrap()
            .permissions(fresh.verifying_key().to_bytes(), now_time(now + 3))
            .is_ok()
    );
    let action = ActionRequest {
        version: 1,
        room: fresh_request.challenge.room,
        invocation_id: "fresh-edit".into(),
        operation: NativeOperation::WriterInsert {
            text: "still available".into(),
        },
    };
    assert_eq!(
        store
            .action(fresh.verifying_key().to_bytes(), &action, now + 3)
            .unwrap()
            .status,
        "completed"
    );
}

#[test]
fn compact_enrollment_imports_existing_history_after_prior_browser_expires() {
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let old = SigningKey::from_bytes(&[8; 32]);
    let fresh = SigningKey::from_bytes(&[9; 32]);
    let stable = SigningKey::from_bytes(&[10; 32]);
    let now = qos_relay::clock::now_ns();
    let mut store = Store::open(dir.path(), "fresh-expiry-bootstrap").unwrap();
    let mut first = approved(store.inspect(now).unwrap(), &root, &old);
    first.browser_certificate.expires_at_ns = Some(now + 2);
    sign_device_cert(&mut first.browser_certificate, &root).unwrap();
    sign_request(&mut first, &root, &old).unwrap();
    store.apply(&first, now).unwrap();
    store
        .action(
            old.verifying_key().to_bytes(),
            &ActionRequest {
                version: 1,
                room: first.challenge.room.clone(),
                invocation_id: "historical-edit".into(),
                operation: NativeOperation::WriterInsert {
                    text: "retain this history".into(),
                },
            },
            now + 1,
        )
        .unwrap();
    // Preserve a real document change signed by the device before it expires.
    use automerge::{ReadDoc, transaction::Transactable};
    let mut existing = store.peer_at(now + 1).unwrap();
    let app = "so.tilo.word-processor";
    let record = "tab:0000000000000001";
    let mut authored =
        automerge::AutoCommit::load(&existing.document(app, record).unwrap()).unwrap();
    let heads = authored.get_heads();
    authored.set_actor(automerge::ActorId::from(
        old.verifying_key().to_bytes().to_vec(),
    ));
    authored
        .put(automerge::ROOT, "historical_author", "expired browser")
        .unwrap();
    authored.commit();
    let message = existing
        .publish_incremental(
            app,
            record,
            authored.save_after(&heads),
            &old,
            now_time(now + 1),
        )
        .unwrap();
    store
        .receive(
            TransportOrigin::RelayDelta(old.verifying_key().to_bytes()),
            message,
            now + 1,
        )
        .unwrap();
    let stable_request = approved(store.inspect(now + 1).unwrap(), &root, &stable);
    store.apply(&stable_request, now + 1).unwrap();
    let mut causal = store.peer_at(now + 1).unwrap();
    let mut first_delta =
        automerge::AutoCommit::load(&causal.document(app, record).unwrap()).unwrap();
    let first_heads = first_delta.get_heads();
    first_delta
        .put(automerge::ROOT, "deferred_predecessor", "A")
        .unwrap();
    first_delta.commit();
    let predecessor = causal
        .publish_incremental(
            app,
            record,
            first_delta.save_after(&first_heads),
            &stable,
            now_time(now + 1),
        )
        .unwrap();
    let mut second_delta =
        automerge::AutoCommit::load(&causal.document(app, record).unwrap()).unwrap();
    let second_heads = second_delta.get_heads();
    second_delta
        .put(automerge::ROOT, "deferred_successor", "B")
        .unwrap();
    second_delta.commit();
    let deferred = causal
        .publish_incremental(
            app,
            record,
            second_delta.save_after(&second_heads),
            &stable,
            now_time(now + 1),
        )
        .unwrap();
    let mut live_receiver = store.peer_at(now + 1).unwrap();
    drop(store);

    let mut store = Store::open(dir.path(), "fresh-expiry-bootstrap").unwrap();
    let cleanup_start = store.state.outbox.len();
    store
        .receive(
            TransportOrigin::RelayDelta(stable.verifying_key().to_bytes()),
            deferred,
            now + 3,
        )
        .unwrap();
    assert_eq!(store.peer_at(now + 3).unwrap().pending_count(), 1);
    let cleanup_live = store.state.outbox[cleanup_start..].to_vec();
    assert_eq!(cleanup_live[0].kind, PublicationKind::Delta);
    apply_live_publications(&mut live_receiver, &cleanup_live, now + 3);
    let request = approved(store.inspect(now + 3).unwrap(), &root, &fresh);
    let fresh_start = store.state.outbox.len();
    let result = store.apply(&request, now + 3).unwrap();
    let fresh_live = store.state.outbox[fresh_start..].to_vec();
    assert_eq!(fresh_live[0].kind, PublicationKind::Delta);
    apply_live_publications(&mut live_receiver, &fresh_live, now + 3);
    assert_eq!(live_receiver.pending_count(), 0);
    assert!(
        live_receiver
            .permissions(fresh.verifying_key().to_bytes(), now_time(now + 3))
            .is_ok(),
        "expiry cleanup and fresh enrollment must share one causal live tail"
    );
    assert!(live_receiver.rows().iter().all(|row| {
        live_receiver
            .durable_covers_current(&row.app_id, &row.record_id)
            .unwrap()
    }));
    let predecessor_start = store.state.outbox.len();
    store
        .receive(
            TransportOrigin::RelayDelta(stable.verifying_key().to_bytes()),
            predecessor,
            now + 4,
        )
        .unwrap();
    assert_eq!(store.peer_at(now + 4).unwrap().pending_count(), 0);
    let predecessor_live = store.state.outbox[predecessor_start..].to_vec();
    apply_live_publications(&mut live_receiver, &predecessor_live, now + 4);
    assert_eq!(live_receiver.pending_count(), 0);
    assert_eq!(
        live_receiver.document_heads(app, record),
        store.peer_at(now + 4).unwrap().document_heads(app, record)
    );
    let roster: Vec<DeviceCert> =
        serde_json::from_value(result["bootstrap"]["certified_roster"].clone()).unwrap();
    assert_eq!(
        roster.len(),
        2,
        "enrollment stays independent of historical fleet size"
    );
    let mut receiver = StatePeer::new(
        &request.challenge.room,
        request.host_certificate.clone(),
        roster,
        RevocationList::new(),
        now_time(now + 3),
    )
    .unwrap();
    let mut host = store.peer_at(now + 3).unwrap();
    existing
        .receive_from(
            TransportOrigin::RelayDelta(request.host_certificate.device_public_key),
            host.room_message(),
            now_time(now + 3),
        )
        .unwrap();
    for message in host
        .bootstrap_messages(&store.key(), now_time(now + 3))
        .unwrap()
    {
        receiver
            .receive_from(
                TransportOrigin::RelayDelta(request.host_certificate.device_public_key),
                message,
                now_time(now + 3),
            )
            .unwrap();
    }
    assert_eq!(
        receiver.rows(),
        host.rows(),
        "new browser receives pre-expiry application history"
    );
    let recovered = automerge::AutoCommit::load(&receiver.document(app, record).unwrap()).unwrap();
    assert_eq!(
        recovered
            .get(automerge::ROOT, "historical_author")
            .unwrap()
            .unwrap()
            .0
            .to_str(),
        Some("expired browser")
    );
    assert!(existing.certificates().contains(&first.browser_certificate));
    assert!(existing.document(app, record).is_some());
    assert!(
        existing
            .permissions(old.verifying_key().to_bytes(), now_time(now + 3))
            .is_err()
    );
    assert!(
        host.certificates().contains(&first.browser_certificate),
        "durable historical identity remains available for audit"
    );
    assert!(
        host.permissions(old.verifying_key().to_bytes(), now_time(now + 3))
            .is_err()
    );
    assert!(
        receiver
            .permissions(old.verifying_key().to_bytes(), now_time(now + 3))
            .is_err()
    );
    assert!(
        receiver
            .permissions(fresh.verifying_key().to_bytes(), now_time(now + 3))
            .is_ok()
    );
    drop(store);
    let store = Store::open(dir.path(), "fresh-expiry-bootstrap").unwrap();
    assert_eq!(store.peer_at(now + 3).unwrap().rows(), receiver.rows());
}

#[test]
fn expiry_cleanup_publishes_room_delta_while_preserving_encrypted_metadata() {
    use automerge::transaction::Transactable;
    use collab_types::{
        Ed25519PublicKey, EncryptionMetadata, OpClass, SignedOp, document_artifact_id,
        document_attestation_id, document_channel_id,
    };

    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[11; 32]);
    let expiring = SigningKey::from_bytes(&[12; 32]);
    let stable = SigningKey::from_bytes(&[13; 32]);
    let now = qos_relay::clock::now_ns();
    let mut store = Store::open(dir.path(), "expiry-encrypted-metadata").unwrap();
    let mut first = approved(store.inspect(now).unwrap(), &root, &expiring);
    first.browser_certificate.expires_at_ns = Some(now + 2);
    sign_device_cert(&mut first.browser_certificate, &root).unwrap();
    sign_request(&mut first, &root, &expiring).unwrap();
    store.apply(&first, now).unwrap();
    let stable_request = approved(store.inspect(now + 1).unwrap(), &root, &stable);
    store.apply(&stable_request, now + 1).unwrap();
    let mut live_receiver = store.peer_at(now + 1).unwrap();
    let app = "so.tilo.word-processor";
    let record = "tab:0000000000000001";
    let room = store.state.room.clone();
    let mut authoring = store.peer_at(now + 1).unwrap();
    let mut doc = automerge::AutoCommit::load(&authoring.document(app, record).unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(automerge::ROOT, "encrypted-template", true)
        .unwrap();
    doc.commit();
    let template = authoring
        .publish_incremental(
            app,
            record,
            doc.save_after(&heads),
            &stable,
            now_time(now + 1),
        )
        .unwrap();
    let NeighbourMsg::AppDocument { peer, .. } = template else {
        panic!("signed template must be an app document")
    };
    let mut envelope = SignedOp::new_unsigned_with_encryption(
        b"opaque-mls-marker".to_vec(),
        document_artifact_id(app, &room, record),
        Ed25519PublicKey::from_bytes(stable.verifying_key().to_bytes()),
        document_attestation_id(app, &room, record),
        OpClass::Splice,
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(now + 1),
        1,
        EncryptionMetadata {
            epoch: 1,
            channel: document_channel_id(app, &room, record),
        },
    );
    collab_types::crypto::sign_op(&mut envelope, &stable).unwrap();
    drop(store);

    let mut store = Store::open(dir.path(), "expiry-encrypted-metadata").unwrap();
    let outbox_start = store.state.outbox.len();
    store
        .receive(
            TransportOrigin::RelayDelta(stable.verifying_key().to_bytes()),
            NeighbourMsg::AppDocument {
                peer,
                app_id: app.into(),
                cluster_id: room,
                record_id: record.into(),
                envelope: Box::new(envelope),
            },
            now + 3,
        )
        .unwrap();
    let tail = store.state.outbox[outbox_start..].to_vec();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].kind, PublicationKind::Delta);
    apply_live_publications(&mut live_receiver, &tail, now + 3);
    assert_eq!(live_receiver.pending_count(), 0);
    assert!(
        store
            .peer_at(now + 3)
            .unwrap()
            .incomplete_reason()
            .is_some()
    );
    drop(store);
    assert!(
        Store::open(dir.path(), "expiry-encrypted-metadata")
            .unwrap()
            .peer_at(now + 3)
            .unwrap()
            .incomplete_reason()
            .is_some(),
        "authenticated encrypted metadata must survive cleanup publication"
    );
}
#[test]
fn model_pending_survives_restart_and_rejects_substitution_and_foreign_actor() {
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut store = Store::open(dir.path(), "machine-a").unwrap();
    let enrollment = approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&enrollment, 11).unwrap();
    let command: ActionRequest = serde_json::from_value(serde_json::json!({
        "version":1,"room":enrollment.challenge.room,"invocation_id":"model-1",
        "operation":{"kind":"model_complete","text":"Explain gravity"}
    }))
    .expect("bounded native completion must be supported");
    let source = device.verifying_key().to_bytes();
    let first = store.action(source, &command, 12).unwrap();
    assert_eq!(first.status, "pending");
    drop(store);
    let mut store = Store::open(dir.path(), "machine-a").unwrap();
    assert_eq!(first, store.action(source, &command, 13).unwrap());
    let mut altered = serde_json::to_value(&command).unwrap();
    altered["operation"]["text"] = "Substitution".into();
    assert!(
        store
            .action(source, &serde_json::from_value(altered).unwrap(), 14)
            .is_err()
    );
    assert!(store.action([99; 32], &command, 14).is_err());
    let status = ActionRequest {
        invocation_id: "status-1".into(),
        operation: NativeOperation::Status {
            invocation_id: "model-1".into(),
        },
        ..command.clone()
    };
    assert_eq!(
        store.action(source, &status, 15).unwrap().output.unwrap()["status"],
        "pending"
    );
    let cancel = ActionRequest {
        invocation_id: "cancel-1".into(),
        operation: NativeOperation::Cancel {
            invocation_id: "model-1".into(),
        },
        ..command
    };
    assert_eq!(store.action(source, &cancel, 16).unwrap().status, "failed");
}

#[tokio::test]
async fn model_network_releases_store_lock_and_reuses_request_id_until_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let source = device.verifying_key().to_bytes();
    let command = {
        let mut store = Store::open(dir.path(), "model-machine").unwrap();
        let r = approved(store.inspect(10).unwrap(), &root, &device);
        store.apply(&r, 11).unwrap();
        ActionRequest {
            version: 1,
            room: r.challenge.room,
            invocation_id: "completion".into(),
            operation: NativeOperation::ModelComplete {
                text: "Hello".into(),
            },
        }
    };
    let captured = std::sync::Mutex::new(None);
    for outcome in [
        gateway::Outcome::Pending,
        gateway::Outcome::Completed("Real completion".into()),
    ] {
        let captured = &captured;
        let dir = &dir;
        let dispatched_outcome = outcome.clone();
        let receipt = model::invoke(
            dir.path(),
            "model-machine",
            source,
            &command,
            12,
            |pending, text| async move {
                let lock = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(dir.path().join("state.lock"))
                    .unwrap();
                lock.try_lock()
                    .expect("model network must run outside Store lock");
                drop(lock);
                assert_eq!(text, "Hello");
                let mut previous = captured.lock().unwrap();
                if let Some(id) = previous.as_ref() {
                    assert_eq!(id, &pending.gateway_id)
                } else {
                    *previous = Some(pending.gateway_id.clone())
                };
                let parsed = uuid::Uuid::parse_str(&pending.gateway_id).unwrap();
                assert_eq!(parsed.get_version_num(), 8);
                dispatched_outcome
            },
        )
        .await
        .unwrap();
        assert_eq!(
            receipt.status,
            if matches!(outcome, gateway::Outcome::Pending) {
                "pending"
            } else {
                "completed"
            }
        );
    }
    let replay = model::invoke(
        dir.path(),
        "model-machine",
        source,
        &command,
        13,
        |_, _| async { panic!("terminal replay must not dispatch") },
    )
    .await
    .unwrap();
    assert_eq!(replay.output.unwrap()["text"], "Real completion");
}

#[test]
fn model_finalization_is_first_terminal_wins_and_rechecks_revocation() {
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let source = device.verifying_key().to_bytes();
    let mut store = Store::open(dir.path(), "model-machine").unwrap();
    let r = approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&r, 11).unwrap();
    let request = ActionRequest {
        version: 1,
        room: r.challenge.room,
        invocation_id: "same".into(),
        operation: NativeOperation::ModelComplete {
            text: "Hello".into(),
        },
    };
    store.action(source, &request, 12).unwrap();
    let scope = format!("{}:same", hex::encode(source));
    let pending = store.state.pending_models[&scope].clone();
    let first = store
        .finish_model(
            source,
            &request,
            &pending,
            gateway::Outcome::Completed("First".into()),
            13,
        )
        .unwrap();
    assert_eq!(
        store
            .finish_model(
                source,
                &request,
                &pending,
                gateway::Outcome::Failed("late_failure"),
                14
            )
            .unwrap(),
        first
    );
    assert_eq!(
        store
            .finish_model(source, &request, &pending, gateway::Outcome::Pending, 15)
            .unwrap(),
        first
    );
    store
        .state
        .bound
        .as_mut()
        .unwrap()
        .roster
        .retain(|c| c.device_public_key != source);
    assert!(
        store
            .finish_model(
                source,
                &request,
                &pending,
                gateway::Outcome::Completed("Unauthorized".into()),
                16
            )
            .is_err()
    );
}

#[test]
fn model_journal_migrates_v1_without_losing_receipts_and_never_defaults_missing_v2_pending() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), "migrate").unwrap();
    drop(store);
    let path = dir.path().join("state.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["version"] = 1.into();
    value.as_object_mut().unwrap().remove("pending_models");
    value["receipts"]["old"] = serde_json::json!(["digest",{"invocation_id":"old","status":"completed","error":null,"output":null}]);
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let store = Store::open(dir.path(), "migrate").unwrap();
    assert_eq!(store.state.receipts["old"].0, "digest");
    assert!(store.state.pending_models.is_empty());
    drop(store);
    let mut migrated: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(migrated["version"], 4);
    // A persisted v2 state must still reject the historically mandatory model
    // journal instead of letting the v2 -> v3 migration default it away.
    migrated["version"] = 2.into();
    migrated.as_object_mut().unwrap().remove("pending_models");
    std::fs::write(&path, serde_json::to_vec(&migrated).unwrap()).unwrap();
    assert!(Store::open(dir.path(), "migrate").is_err());
}

#[test]
fn v3_publication_outbox_migrates_to_explicit_checkpoint_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[111; 32]);
    let device = SigningKey::from_bytes(&[112; 32]);
    let mut store = Store::open(dir.path(), "publication-v3").unwrap();
    let enrollment = approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&enrollment, 11).unwrap();
    drop(store);
    let path = dir.path().join("state.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["version"] = 3.into();
    value["outbox"] = serde_json::Value::Array(
        value["outbox"]
            .as_array()
            .unwrap()
            .iter()
            .map(|publication| publication["message"].clone())
            .collect(),
    );
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

    let store = Store::open(dir.path(), "publication-v3").unwrap();
    assert_eq!(store.state.version, 4);
    assert!(!store.state.outbox.is_empty());
    assert!(
        store
            .state
            .outbox
            .iter()
            .all(|publication| publication.kind == PublicationKind::Checkpoint)
    );
}

#[test]
fn model_replay_rechecks_current_model_action_capability() {
    use automerge::{ReadDoc, transaction::Transactable};
    use collab_types::caps::{CapOverride, Effect, Scope, Subject, Verb};
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let source = device.verifying_key().to_bytes();
    let mut store = Store::open(dir.path(), "model-policy").unwrap();
    let r = approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&r, 11).unwrap();
    let request = ActionRequest {
        version: 1,
        room: r.challenge.room,
        invocation_id: "same".into(),
        operation: NativeOperation::ModelComplete {
            text: "Hello".into(),
        },
    };
    store.action(source, &request, 12).unwrap();
    let pending = store.state.pending_models[&format!("{}:same", hex::encode(source))].clone();
    store
        .finish_model(
            source,
            &request,
            &pending,
            gateway::Outcome::Completed("Private".into()),
            13,
        )
        .unwrap();
    let mut peer = store.peer().unwrap();
    let NeighbourMsg::SyncResp { changes, .. } = peer.room_message() else {
        unreachable!()
    };
    let mut governance = qos_peer_state::room::candidate(None, &changes).unwrap();
    let caps = governance
        .get(automerge::ROOT, "capabilities")
        .unwrap()
        .unwrap()
        .1;
    let restriction = CapOverride::new(
        Subject::User(collab_types::Ed25519PublicKey::from_bytes(
            root.verifying_key().to_bytes(),
        )),
        Scope::Action {
            app: "dev.qos.model".into(),
            action: "model_complete".into(),
        },
        Effect::Restrict,
        Verb::Read,
    );
    governance
        .put(
            &caps,
            "deny-model",
            automerge::ScalarValue::Bytes(serde_json::to_vec(&restriction).unwrap()),
        )
        .unwrap();
    governance.commit();
    let changes = governance
        .get_changes(&[])
        .iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    peer.receive_from(
        TransportOrigin::RelayDelta(store.key().verifying_key().to_bytes()),
        NeighbourMsg::SyncResp {
            room: peer.room().into(),
            changes,
        },
        now_time(14),
    )
    .unwrap();
    let mut state = store.state.clone();
    state.bound.as_mut().unwrap().peer = peer.durable().unwrap();
    store.commit(state).unwrap();
    assert!(
        store.action(source, &request, 15).is_err(),
        "terminal replay must obey current model restrictions"
    );
}

#[tokio::test]
async fn model_catalogue_requires_valid_private_config_and_current_actor() {
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let source = device.verifying_key().to_bytes();
    let request = {
        let mut store = Store::open(dir.path(), "catalogue-machine").unwrap();
        let r = approved(store.inspect(10).unwrap(), &root, &device);
        store.apply(&r, 11).unwrap();
        ActionRequest {
            version: 1,
            room: r.challenge.room,
            invocation_id: "catalogue".into(),
            operation: NativeOperation::Catalogue,
        }
    };
    let path = dir.path().join("ai.json");
    let gateway = gateway::Gateway::new(path.clone());
    let absent =
        action_server::respond(dir.path(), "catalogue-machine", &gateway, source, &request)
            .await
            .unwrap();
    assert!(
        !absent.output.unwrap()["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|op| op["kind"] == "model_complete")
    );
    let now = qos_relay::clock::now_ns() / 1_000_000_000;
    let token = format!("tilo-ai-{}", "z".repeat(43));
    atomic_file::write_with_mode(&path,&serde_json::to_vec(&serde_json::json!({"machineId":"catalogue-machine","endpoint":"https://api.example/api/machines/catalogue-machine/ai/v1/chat/completions","model":"gpt-4.1-mini-2025-04-14","token":token,"expiresAt":now+1000})).unwrap(),0o600).unwrap();
    let available =
        action_server::respond(dir.path(), "catalogue-machine", &gateway, source, &request)
            .await
            .unwrap();
    assert!(
        available.output.unwrap()["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|op| op["kind"] == "model_complete")
    );
    assert!(
        action_server::respond(
            dir.path(),
            "catalogue-machine",
            &gateway,
            [99; 32],
            &request
        )
        .await
        .is_err()
    );
    assert!(
        !std::fs::read_to_string(dir.path().join("state.json"))
            .unwrap()
            .contains(&token)
    );
}

#[test]
fn browser_writer_incremental_survives_host_restart_and_fresh_checkpoints() {
    use qos_actions::Action;
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut store = Store::open(dir.path(), "restart-proof").unwrap();
    let request = approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&request, 11).unwrap();
    let mut browser = store.peer_at(11).unwrap();
    let app = "so.tilo.word-processor";
    let record = "tab:0000000000000001";
    let mut doc = automerge::AutoCommit::load(&browser.document(app, record).unwrap()).unwrap();
    let before = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(
        device.verifying_key().to_bytes().to_vec(),
    ));
    let model = qdoc::model::open(&doc, &automerge::ROOT).unwrap();
    let block = qdoc::model::blocks_in_order(&doc, &model)[0].clone();
    let expected = "Hosted native persistence proof. AWS hosted workspace verification.";
    word_processor::actions::WordProcessorAction::SpliceText {
        region: word_processor::actions::RegionKind::Body,
        block,
        at: 0,
        del: 0,
        ins: expected.into(),
    }
    .apply(&mut doc)
    .unwrap();
    doc.commit();
    let message = browser
        .publish_incremental(app, record, doc.save_after(&before), &device, now_time(12))
        .unwrap();
    store
        .receive(
            TransportOrigin::RelayDelta(device.verifying_key().to_bytes()),
            message,
            12,
        )
        .unwrap();
    let heads = store.peer_at(12).unwrap().rows();
    drop(store);
    let mut store = Store::open(dir.path(), "restart-proof").unwrap();
    let mut restarted = store.peer_at(13).unwrap();
    assert_eq!(heads, restarted.rows());
    // Match runtime startup: publish fresh checkpoints and persist their metadata.
    let messages = restarted
        .bootstrap_messages(&store.key(), now_time(13))
        .unwrap();
    let mut durable = store.state.clone();
    durable.bound.as_mut().unwrap().peer = restarted.durable().unwrap();
    store.commit(durable).unwrap();
    drop(store);
    let store = Store::open(dir.path(), "restart-proof").unwrap();
    let restored = store.peer_at(14).unwrap();
    assert_eq!(heads, restored.rows());
    let doc = automerge::AutoCommit::load(&restored.document(app, record).unwrap()).unwrap();
    let model = qdoc::model::open(&doc, &automerge::ROOT).unwrap();
    let text = qdoc::model::blocks_in_order(&doc, &model)
        .iter()
        .filter_map(|id| qdoc::model::block_text(&doc, &model, id))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(text, expected);
    for message in messages {
        browser
            .receive_from(
                TransportOrigin::RelayDelta(request.host_certificate.device_public_key),
                message,
                now_time(14),
            )
            .unwrap();
    }
    assert_eq!(browser.rows(), heads);
}

#[test]
fn compact_bootstrap_stays_small_beyond_sixty_four_devices() {
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let mut store = Store::open(dir.path(), "many-clients").unwrap();
    let mut first = None;
    for index in 0..70u8 {
        let device = SigningKey::from_bytes(&[index + 20; 32]);
        let request = approved(
            store.inspect(100 + u64::from(index) * 2).unwrap(),
            &root,
            &device,
        );
        let result = store.apply(&request, 101 + u64::from(index) * 2).unwrap();
        assert!(
            serde_json::to_vec(&result).unwrap().len() < 20_000,
            "bootstrap grew with client count at {index}"
        );
        assert_eq!(
            result["bootstrap"]["certified_roster"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        if first.is_none() {
            first = Some(store.peer().unwrap());
        }
    }
    assert_eq!(store.state.bound.as_ref().unwrap().roster.len(), 71);
    let directory = WorkspaceDirectory::load(
        root.verifying_key().to_bytes(),
        store.key().verifying_key().to_bytes(),
        &store.state.bound.as_ref().unwrap().directory,
    )
    .unwrap();
    assert_eq!(
        directory
            .as_map()
            .values()
            .next()
            .unwrap()
            .member_devices
            .len(),
        71
    );
    assert!(first.is_some());
}

#[test]
fn existing_client_admits_new_browser_from_governor_without_identity_presence() {
    use automerge::transaction::Transactable;
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let first = SigningKey::from_bytes(&[8; 32]);
    let second = SigningKey::from_bytes(&[9; 32]);
    let mut store = Store::open(dir.path(), "roster-hydration").unwrap();
    let initial = approved(store.inspect(10).unwrap(), &root, &first);
    store.apply(&initial, 11).unwrap();
    let mut existing = store.peer().unwrap();
    let request = approved(store.inspect(12).unwrap(), &root, &second);
    store.apply(&request, 13).unwrap();
    let mut newcomer = store.peer().unwrap();
    existing
        .receive_from(
            TransportOrigin::RelayDelta(request.host_certificate.device_public_key),
            newcomer.room_message(),
            now_time(14),
        )
        .unwrap();
    let app = "so.tilo.word-processor";
    let record = "tab:0000000000000001";
    let mut doc = automerge::AutoCommit::load(&newcomer.document(app, record).unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(
        second.verifying_key().to_bytes().to_vec(),
    ));
    doc.put(automerge::ROOT, "roster_test", 1).unwrap();
    doc.commit();
    let message = newcomer
        .publish_incremental(app, record, doc.save_after(&heads), &second, now_time(15))
        .unwrap();
    let outcome = existing
        .receive_from(
            TransportOrigin::RelayDelta(second.verifying_key().to_bytes()),
            message,
            now_time(16),
        )
        .unwrap();
    assert!(
        !outcome.deferred,
        "new device certificate was not hydrated from authenticated governance"
    );
    assert!(outcome.changed);
}

#[test]
fn compact_result_does_not_copy_unrelated_revocation_history() {
    use qos_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut store = Store::open(dir.path(), "revocation-history").unwrap();
    let request = approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&request, 11).unwrap();
    let mut state = store.state.clone();
    for index in 130..200u8 {
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: SigningKey::from_bytes(&[index; 32])
                .verifying_key()
                .to_bytes(),
            revoked_at_ns: 12,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &root).unwrap();
        state.bound.as_mut().unwrap().revocations.push(entry);
    }
    store.commit(state).unwrap();
    let next = approved(store.inspect(13).unwrap(), &root, &device);
    let result = store.apply(&next, 14).unwrap();
    assert!(serde_json::to_vec(&result).unwrap().len() < 20_000);
    assert!(result["revocations"].as_array().unwrap().is_empty());
    assert_eq!(store.state.bound.as_ref().unwrap().revocations.len(), 70);
    let mut peer = store.peer().unwrap();
    // The governor's signed metadata, not the compact response, conveys revocation history.
    let rejected = SigningKey::from_bytes(&[130; 32]);
    let mut cert = next.browser_certificate;
    cert.device_public_key = rejected.verifying_key().to_bytes();
    sign_device_cert(&mut cert, &root).unwrap();
    assert!(
        peer.admit_certificate(cert, &store.key(), now_time(15))
            .is_err()
    );
}

#[test]
fn revoking_host_persists_even_though_governor_can_no_longer_publish() {
    use qos_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut store = Store::open(dir.path(), "revoke-host").unwrap();
    let request = approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&request, 11).unwrap();
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: request.host_certificate.device_public_key,
        revoked_at_ns: 12,
        reason: RevocationReason::Lost,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &root).unwrap();
    store
        .receive(
            TransportOrigin::RelayDelta(device.verifying_key().to_bytes()),
            NeighbourMsg::IdentityHello {
                cert: Box::new(request.browser_certificate),
                revocations: vec![entry.clone()],
            },
            13,
        )
        .unwrap();
    assert!(
        store
            .state
            .bound
            .as_ref()
            .unwrap()
            .revocations
            .contains(&entry)
    );
    assert!(store.peer().is_err());
    drop(store);
    assert!(Store::open(dir.path(), "revoke-host").is_err());
}

#[test]
fn reordered_browser_writer_deltas_survive_restart_and_late_predecessor() {
    use automerge::transaction::Transactable;
    use qos_actions::Action;
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut store = Store::open(dir.path(), "reordered-proof").unwrap();
    let request = approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&request, 11).unwrap();
    let mut browser = store.peer_at(11).unwrap();
    let app = "so.tilo.word-processor";
    let record = "tab:0000000000000001";
    let mut doc = automerge::AutoCommit::load(&browser.document(app, record).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        device.verifying_key().to_bytes().to_vec(),
    ));
    let model = qdoc::model::open(&doc, &automerge::ROOT).unwrap();
    let block = qdoc::model::blocks_in_order(&doc, &model)[0].clone();
    let mut messages = Vec::new();
    for (at, text) in ["A", "B"].iter().enumerate() {
        let heads = doc.get_heads();
        word_processor::actions::WordProcessorAction::SpliceText {
            region: word_processor::actions::RegionKind::Body,
            block: block.clone(),
            at,
            del: 0,
            ins: (*text).into(),
        }
        .apply(&mut doc)
        .unwrap();
        doc.commit();
        messages.push(
            browser
                .publish_incremental(app, record, doc.save_after(&heads), &device, now_time(12))
                .unwrap(),
        );
    }
    let origin = TransportOrigin::RelayDelta(device.verifying_key().to_bytes());
    store.receive(origin, messages[1].clone(), 12).unwrap();
    assert_eq!(store.peer_at(12).unwrap().pending_count(), 1);
    assert!(!store.peer_at(12).unwrap().covers(&messages[1]));
    drop(store);
    let mut store = Store::open(dir.path(), "reordered-proof").unwrap();
    assert_eq!(store.peer_at(13).unwrap().pending_count(), 1);
    store.receive(origin, messages[0].clone(), 13).unwrap();
    store.receive(origin, messages[1].clone(), 13).unwrap();
    assert_eq!(store.peer_at(13).unwrap().pending_count(), 0);
    drop(store);
    let store = Store::open(dir.path(), "reordered-proof").unwrap();
    let peer = store.peer_at(14).unwrap();
    let actual = automerge::AutoCommit::load(&peer.document(app, record).unwrap()).unwrap();
    let model = qdoc::model::open(&actual, &automerge::ROOT).unwrap();
    let text = qdoc::model::blocks_in_order(&actual, &model)
        .iter()
        .filter_map(|id| qdoc::model::block_text(&actual, &model, id))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(text, "AB");
    assert_eq!(actual.base_heads(), doc.base_heads());
}

#[tokio::test]
async fn owner_action_invites_independent_root_and_role_changes_survive_restart() {
    use qos_peer_state::join::{
        JoinChallengeRequest, JoinCompleteRequest, MemberRole, MembershipChallengeRequest,
        MembershipCompleteRequest, decode_and_verify_invitation_text, sign_join_proof,
        sign_membership_proof,
    };

    let dir = tempfile::tempdir().unwrap();
    let owner_root = SigningKey::from_bytes(&[7; 32]);
    let owner_device = SigningKey::from_bytes(&[8; 32]);
    let guest_root = SigningKey::from_bytes(&[9; 32]);
    let guest_device = SigningKey::from_bytes(&[10; 32]);
    let mut store = Store::open(dir.path(), "independent-root").unwrap();
    let enrollment = approved(store.inspect(10).unwrap(), &owner_root, &owner_device);
    store.apply(&enrollment, 11).unwrap();

    drop(store);
    let service = crate::broker::CloudCollaborationAdmin::new(
        dir.path(),
        "independent-root",
        &enrollment.challenge.room,
    );
    let issued = qos_action_broker::collaboration_admin::CollaborationAdmin::execute(
        &service,
        qos_action_broker::collaboration_admin::CollaborationAdminRequest {
            room: enrollment.challenge.room.clone(),
            invocation: qos_actions::InvocationId::new(),
            caller: enrollment.browser_certificate.clone(),
            operation: qos_action_broker::collaboration_admin::CollaborationAdminOperation::Issue(
                qos_action_broker::collaboration_admin::IssueInvitationInput {
                    role: qos_action_broker::collaboration_admin::CollaborationRole::Editor,
                    expires_in_seconds: 600,
                    invitee_name: Some("Guest".into()),
                },
            ),
        },
    )
    .await
    .unwrap();
    let mut store = Store::open(dir.path(), "independent-root").unwrap();
    let signed_invitation = decode_and_verify_invitation_text(
        issued["invite"].as_str().unwrap(),
        None,
        &RevocationList::new(),
        13,
    )
    .unwrap()
    .signed_invitation()
    .to_vec();
    let mut guest_cert = DeviceCert {
        user_public_key: guest_root.verifying_key().to_bytes(),
        device_public_key: guest_device.verifying_key().to_bytes(),
        device_label: "Guest device".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 12,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut guest_cert, &guest_root).unwrap();
    let request_id = [44; 32];
    let challenge = store
        .join_challenge(
            guest_device.verifying_key().to_bytes(),
            JoinChallengeRequest {
                signed_invitation,
                device_cert: guest_cert.clone(),
                request_id,
            },
            13,
        )
        .unwrap();
    let proof = sign_join_proof(challenge.challenge, &guest_device).unwrap();
    let bootstrap = store
        .join_complete(
            guest_device.verifying_key().to_bytes(),
            JoinCompleteRequest {
                proof: proof.clone(),
            },
            14,
        )
        .unwrap();
    assert_eq!(bootstrap.role, MemberRole::Editor);
    assert!(
        store
            .peer_at(14)
            .unwrap()
            .governed_access(now_time(14))
            .unwrap()
            .member_user_roots
            .contains(&guest_root.verifying_key().to_bytes())
    );
    assert_eq!(
        store
            .join_complete(
                guest_device.verifying_key().to_bytes(),
                JoinCompleteRequest {
                    proof: proof.clone(),
                },
                15,
            )
            .unwrap(),
        bootstrap
    );
    let membership = store
        .membership_challenge(
            guest_device.verifying_key().to_bytes(),
            MembershipChallengeRequest {
                room: enrollment.challenge.room.clone(),
                workspace_id: bootstrap.workspace_id,
                device_cert: guest_cert.clone(),
                request_id: [45; 32],
            },
            15,
        )
        .unwrap();
    let membership_proof = sign_membership_proof(membership.challenge, &guest_device).unwrap();
    let peer_before_refresh = store.state.bound.as_ref().unwrap().peer.clone();
    let outbox_before_refresh = store.state.outbox.len();
    assert_eq!(
        store
            .membership_complete(
                guest_device.verifying_key().to_bytes(),
                MembershipCompleteRequest {
                    proof: membership_proof.clone(),
                },
                16,
            )
            .unwrap()
            .role,
        MemberRole::Editor
    );
    assert_ne!(
        store.state.bound.as_ref().unwrap().peer,
        peer_before_refresh,
        "refresh bootstrap mutations must be durable"
    );
    assert!(
        store.state.outbox.len() > outbox_before_refresh,
        "refresh bootstrap messages must be queued durably"
    );
    assert!(
        store
            .membership_complete(
                [99; 32],
                MembershipCompleteRequest {
                    proof: membership_proof.clone(),
                },
                16,
            )
            .is_err(),
        "cached refresh must remain bound to the authenticated endpoint"
    );

    drop(store);
    let mut store = Store::open(dir.path(), "independent-root").unwrap();
    let edit = ActionRequest {
        version: 1,
        room: enrollment.challenge.room.clone(),
        invocation_id: "guest-edit".into(),
        operation: NativeOperation::WriterInsert {
            text: "joined from another root".into(),
        },
    };
    assert_eq!(
        store
            .action(guest_device.verifying_key().to_bytes(), &edit, 16)
            .unwrap()
            .status,
        "completed"
    );
    drop(store);
    let forced = qos_action_broker::collaboration_admin::CollaborationAdmin::execute(
        &service,
        qos_action_broker::collaboration_admin::CollaborationAdminRequest {
            room: enrollment.challenge.room.clone(),
            invocation: qos_actions::InvocationId::new(),
            caller: guest_cert.clone(),
            operation: qos_action_broker::collaboration_admin::CollaborationAdminOperation::Remove(
                qos_action_broker::collaboration_admin::RemoveMemberInput {
                    participant_id: hex::encode(owner_root.verifying_key().to_bytes()),
                },
            ),
        },
    )
    .await;
    assert!(
        matches!(forced, Err(qos_action_broker::collaboration_admin::CollaborationAdminError::Rejected(message)) if message.contains("Owner authority"))
    );
    let mut store = Store::open(dir.path(), "independent-root").unwrap();
    let demote = ActionRequest {
        version: 1,
        room: enrollment.challenge.room.clone(),
        invocation_id: "demote-guest".into(),
        operation: NativeOperation::CollaborationSetRole {
            user_public_key: guest_root.verifying_key().to_bytes(),
            role: MemberRole::Viewer,
        },
    };
    store
        .action(owner_device.verifying_key().to_bytes(), &demote, 17)
        .unwrap();
    let denied = ActionRequest {
        invocation_id: "guest-edit-denied".into(),
        ..edit
    };
    assert!(
        store
            .action(guest_device.verifying_key().to_bytes(), &denied, 18)
            .is_err()
    );
    let remove = ActionRequest {
        version: 1,
        room: enrollment.challenge.room,
        invocation_id: "remove-guest".into(),
        operation: NativeOperation::CollaborationRemove {
            user_public_key: guest_root.verifying_key().to_bytes(),
        },
    };
    store
        .action(owner_device.verifying_key().to_bytes(), &remove, 19)
        .unwrap();
    assert!(
        !store
            .peer_at(19)
            .unwrap()
            .governed_access(now_time(19))
            .unwrap()
            .member_user_roots
            .contains(&guest_root.verifying_key().to_bytes())
    );
    assert!(
        store
            .membership_complete(
                guest_device.verifying_key().to_bytes(),
                MembershipCompleteRequest {
                    proof: membership_proof,
                },
                20,
            )
            .is_err(),
        "cached refresh must recheck current membership"
    );
    assert!(
        store
            .join_complete(
                guest_device.verifying_key().to_bytes(),
                JoinCompleteRequest { proof },
                20,
            )
            .is_err(),
        "cached initial completion must recheck current membership"
    );
}

#[test]
fn refresh_then_third_member_reaches_existing_live_receivers_without_checkpoint_replay() {
    use qos_peer_state::join::{
        MembershipChallengeRequest, MembershipCompleteRequest, sign_membership_proof,
    };

    let dir = tempfile::tempdir().unwrap();
    let owner_root = SigningKey::from_bytes(&[101; 32]);
    let owner_device = SigningKey::from_bytes(&[102; 32]);
    let first_root = SigningKey::from_bytes(&[103; 32]);
    let first_device = SigningKey::from_bytes(&[104; 32]);
    let second_root = SigningKey::from_bytes(&[105; 32]);
    let second_device = SigningKey::from_bytes(&[106; 32]);
    let mut store = Store::open(dir.path(), "three-member-live").unwrap();
    let enrollment = approved(store.inspect(10).unwrap(), &owner_root, &owner_device);
    store.apply(&enrollment, 11).unwrap();
    let mut owner_receiver = store.peer_at(11).unwrap();

    let first_start = store.state.outbox.len();
    let (first_cert, first_bootstrap) = invite_and_admit(
        &mut store,
        &owner_device,
        &first_root,
        &first_device,
        "first-live-member",
        [107; 32],
        12,
    );
    let first_live = store.state.outbox[first_start..].to_vec();
    assert_eq!(first_live[0].kind, PublicationKind::Delta);
    apply_live_publications(&mut owner_receiver, &first_live, 15);
    assert!(
        owner_receiver
            .governed_access(now_time(15))
            .unwrap()
            .member_user_roots
            .contains(&first_root.verifying_key().to_bytes())
    );

    let refresh = store
        .membership_challenge(
            first_device.verifying_key().to_bytes(),
            MembershipChallengeRequest {
                room: enrollment.challenge.room.clone(),
                workspace_id: first_bootstrap.workspace_id,
                device_cert: first_cert,
                request_id: [108; 32],
            },
            16,
        )
        .unwrap();
    let refresh_start = store.state.outbox.len();
    let refresh_bootstrap = store
        .membership_complete(
            first_device.verifying_key().to_bytes(),
            MembershipCompleteRequest {
                proof: sign_membership_proof(refresh.challenge, &first_device).unwrap(),
            },
            17,
        )
        .unwrap();
    let refresh_live = store.state.outbox[refresh_start..].to_vec();
    assert_eq!(refresh_live[0].kind, PublicationKind::Delta);
    apply_live_publications(&mut owner_receiver, &refresh_live, 17);
    let mut refreshed_receiver = receiver_from_join_bootstrap(&refresh_bootstrap, 17);

    let second_start = store.state.outbox.len();
    let (_, second_bootstrap) = invite_and_admit(
        &mut store,
        &owner_device,
        &second_root,
        &second_device,
        "second-live-member",
        [109; 32],
        18,
    );
    let second_live = store.state.outbox[second_start..].to_vec();
    assert_eq!(second_live[0].kind, PublicationKind::Delta);
    apply_live_publications(&mut owner_receiver, &second_live, 21);
    apply_live_publications(&mut refreshed_receiver, &second_live, 21);
    let mut second_receiver = receiver_from_join_bootstrap(&second_bootstrap, 21);
    apply_live_publications(&mut second_receiver, &second_live, 21);

    for receiver in [&owner_receiver, &refreshed_receiver, &second_receiver] {
        let access = receiver.governed_access(now_time(21)).unwrap();
        assert!(
            access
                .member_user_roots
                .contains(&first_root.verifying_key().to_bytes())
        );
        assert!(
            access
                .member_user_roots
                .contains(&second_root.verifying_key().to_bytes())
        );
        assert_eq!(receiver.pending_count(), 0);
        assert_eq!(receiver.incomplete_reason(), None);
        for row in receiver.rows() {
            assert!(
                receiver
                    .durable_covers_current(&row.app_id, &row.record_id)
                    .unwrap(),
                "{}:{} must retain a receipt-matched checkpoint",
                row.app_id,
                row.record_id,
            );
        }
    }
    assert_eq!(
        serde_json::to_value(owner_receiver.rows()).unwrap(),
        serde_json::to_value(store.peer_at(21).unwrap().rows()).unwrap()
    );
}

#[tokio::test]
async fn accepted_guest_document_logs_exact_durable_route_transition() {
    use qos_actions::Action;
    use qos_peer_state::join::{
        JoinChallengeRequest, JoinCompleteRequest, decode_and_verify_invitation_text,
        sign_join_proof,
    };

    let dir = tempfile::tempdir().unwrap();
    let owner_root = SigningKey::from_bytes(&[41; 32]);
    let owner_device = SigningKey::from_bytes(&[42; 32]);
    let guest_root = SigningKey::from_bytes(&[43; 32]);
    let guest_device = SigningKey::from_bytes(&[44; 32]);
    let mut store = Store::open(dir.path(), "guest-edit-audit").unwrap();
    let enrollment = approved(store.inspect(10).unwrap(), &owner_root, &owner_device);
    store.apply(&enrollment, 11).unwrap();
    drop(store);

    let service = crate::broker::CloudCollaborationAdmin::new(
        dir.path(),
        "guest-edit-audit",
        &enrollment.challenge.room,
    );
    let issued = qos_action_broker::collaboration_admin::CollaborationAdmin::execute(
        &service,
        qos_action_broker::collaboration_admin::CollaborationAdminRequest {
            room: enrollment.challenge.room.clone(),
            invocation: qos_actions::InvocationId::new(),
            caller: enrollment.browser_certificate.clone(),
            operation: qos_action_broker::collaboration_admin::CollaborationAdminOperation::Issue(
                qos_action_broker::collaboration_admin::IssueInvitationInput {
                    role: qos_action_broker::collaboration_admin::CollaborationRole::Editor,
                    expires_in_seconds: 600,
                    invitee_name: Some("Audit Guest".into()),
                },
            ),
        },
    )
    .await
    .unwrap();
    let signed_invitation = decode_and_verify_invitation_text(
        issued["invite"].as_str().unwrap(),
        None,
        &RevocationList::new(),
        12,
    )
    .unwrap()
    .signed_invitation()
    .to_vec();
    let mut guest_cert = DeviceCert {
        user_public_key: guest_root.verifying_key().to_bytes(),
        device_public_key: guest_device.verifying_key().to_bytes(),
        device_label: "Audit guest device".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 12,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut guest_cert, &guest_root).unwrap();
    let mut store = Store::open(dir.path(), "guest-edit-audit").unwrap();
    let challenge = store
        .join_challenge(
            guest_device.verifying_key().to_bytes(),
            JoinChallengeRequest {
                signed_invitation,
                device_cert: guest_cert,
                request_id: [45; 32],
            },
            13,
        )
        .unwrap();
    let proof = sign_join_proof(challenge.challenge, &guest_device).unwrap();
    store
        .join_complete(
            guest_device.verifying_key().to_bytes(),
            JoinCompleteRequest { proof },
            14,
        )
        .unwrap();

    let app = "so.tilo.word-processor";
    let record = "tab:0000000000000001";
    let mut guest = store.peer_at(14).unwrap();
    let mut doc = automerge::AutoCommit::load(&guest.document(app, record).unwrap()).unwrap();
    let before = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(
        guest_device.verifying_key().to_bytes().to_vec(),
    ));
    let model = qdoc::model::open(&doc, &automerge::ROOT).unwrap();
    let block = qdoc::model::blocks_in_order(&doc, &model)[0].clone();
    word_processor::actions::WordProcessorAction::SpliceText {
        region: word_processor::actions::RegionKind::Body,
        block,
        at: 0,
        del: 0,
        ins: "guest audit edit".into(),
    }
    .apply(&mut doc)
    .unwrap();
    doc.commit();
    let message = guest
        .publish_incremental(
            app,
            record,
            doc.save_after(&before),
            &guest_device,
            now_time(15),
        )
        .unwrap();
    let before_heads = store
        .peer_at(15)
        .unwrap()
        .document_heads(app, record)
        .unwrap();
    let expected_generation = store.next_generation().unwrap();
    take_test_audit_events();
    store
        .receive(
            TransportOrigin::RelayDelta(guest_device.verifying_key().to_bytes()),
            message,
            15,
        )
        .unwrap();
    let after_heads = store
        .peer_at(15)
        .unwrap()
        .document_heads(app, record)
        .unwrap();
    assert_ne!(before_heads, after_heads);
    let event = take_test_audit_events()
        .into_iter()
        .find(|(event, _)| event == "collaboration_document_committed")
        .expect("accepted guest edit must emit a native audit event");
    assert_eq!(event.1["attribution"], "accepted_transport_signer");
    assert_eq!(event.1["room"], enrollment.challenge.room);
    assert_eq!(event.1["app"], app);
    assert_eq!(event.1["record"], record);
    assert_eq!(
        event.1["signer_device"],
        hex::encode(guest_device.verifying_key().to_bytes())
    );
    assert_eq!(
        event.1["signer_root"],
        hex::encode(guest_root.verifying_key().to_bytes())
    );
    assert_eq!(event.1["generation"], expected_generation.to_string());
    assert_eq!(event.1["submitted_sequence"], "1");
    assert_eq!(
        event.1["before_heads"],
        serde_json::json!(
            before_heads
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        )
    );
    assert_eq!(
        event.1["after_heads"],
        serde_json::json!(
            after_heads
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        )
    );
    assert!(!event.1.to_string().contains("guest audit edit"));
    drop(store);

    let reopened = Store::open(dir.path(), "guest-edit-audit").unwrap();
    assert_eq!(reopened.state.generation, expected_generation);
    assert_eq!(
        reopened
            .peer_at(16)
            .unwrap()
            .document_heads(app, record)
            .unwrap(),
        after_heads
    );
}

#[test]
fn cancelled_invitation_cannot_complete_a_saved_challenge_after_restart() {
    use qos_peer_state::join::{
        JoinChallengeRequest, JoinCompleteRequest, MemberRole, decode_and_verify_invitation_text,
        sign_join_proof,
    };

    let dir = tempfile::tempdir().unwrap();
    let owner_root = SigningKey::from_bytes(&[81; 32]);
    let owner_device = SigningKey::from_bytes(&[82; 32]);
    let guest_root = SigningKey::from_bytes(&[83; 32]);
    let guest_device = SigningKey::from_bytes(&[84; 32]);
    let mut store = Store::open(dir.path(), "cancelled-challenge").unwrap();
    let enrollment = approved(store.inspect(10).unwrap(), &owner_root, &owner_device);
    store.apply(&enrollment, 11).unwrap();
    let issue = ActionRequest {
        version: 1,
        room: enrollment.challenge.room.clone(),
        invocation_id: "issue-cancelled".into(),
        operation: NativeOperation::CollaborationInvite {
            role: MemberRole::Editor,
            recipient_user_public_key: Some(guest_root.verifying_key().to_bytes()),
            invitee_name: Some("Cancelled guest".into()),
            expires_at_ns: 600_000_000_000,
        },
    };
    let issued = store
        .action(owner_device.verifying_key().to_bytes(), &issue, 12)
        .unwrap()
        .output
        .unwrap();
    let invite_id: [u8; 32] = hex::decode(issued["invite_id"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let signed_invitation = decode_and_verify_invitation_text(
        issued["signed_invitation"].as_str().unwrap(),
        Some(guest_root.verifying_key().to_bytes()),
        &RevocationList::new(),
        13,
    )
    .unwrap()
    .signed_invitation()
    .to_vec();
    let mut guest_cert = DeviceCert {
        user_public_key: guest_root.verifying_key().to_bytes(),
        device_public_key: guest_device.verifying_key().to_bytes(),
        device_label: "Cancelled guest device".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 12,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut guest_cert, &guest_root).unwrap();
    let challenge = store
        .join_challenge(
            guest_device.verifying_key().to_bytes(),
            JoinChallengeRequest {
                signed_invitation,
                device_cert: guest_cert,
                request_id: [85; 32],
            },
            13,
        )
        .unwrap();
    let proof = sign_join_proof(challenge.challenge, &guest_device).unwrap();
    store
        .action(
            owner_device.verifying_key().to_bytes(),
            &ActionRequest {
                version: 1,
                room: enrollment.challenge.room,
                invocation_id: "cancel-before-complete".into(),
                operation: NativeOperation::CollaborationCancelInvitation { invite_id },
            },
            14,
        )
        .unwrap();
    let peer_before = store.state.bound.as_ref().unwrap().peer.clone();
    drop(store);

    let mut store = Store::open(dir.path(), "cancelled-challenge").unwrap();
    let error = store
        .join_complete(
            guest_device.verifying_key().to_bytes(),
            JoinCompleteRequest { proof },
            15,
        )
        .unwrap_err();
    assert!(error.to_string().contains("revoked"));
    assert_eq!(store.state.bound.as_ref().unwrap().peer, peer_before);
    assert!(store.state.join_responses.is_empty());
    assert_eq!(
        store.state.invitations[&hex::encode(invite_id)].status,
        crate::collaboration::InvitationStatus::Revoked
    );
}

#[test]
fn rival_valid_challenges_report_the_invitation_as_consumed() {
    use qos_peer_state::join::{
        JoinChallengeRequest, JoinCompleteRequest, MemberRole, decode_and_verify_invitation_text,
        sign_join_proof,
    };

    let dir = tempfile::tempdir().unwrap();
    let owner_root = SigningKey::from_bytes(&[91; 32]);
    let owner_device = SigningKey::from_bytes(&[92; 32]);
    let guest_root = SigningKey::from_bytes(&[93; 32]);
    let guest_a = SigningKey::from_bytes(&[94; 32]);
    let guest_b = SigningKey::from_bytes(&[95; 32]);
    let mut store = Store::open(dir.path(), "rival-challenges").unwrap();
    let enrollment = approved(store.inspect(10).unwrap(), &owner_root, &owner_device);
    store.apply(&enrollment, 11).unwrap();
    let issued = store
        .action(
            owner_device.verifying_key().to_bytes(),
            &ActionRequest {
                version: 1,
                room: enrollment.challenge.room,
                invocation_id: "issue-rival".into(),
                operation: NativeOperation::CollaborationInvite {
                    role: MemberRole::Editor,
                    recipient_user_public_key: Some(guest_root.verifying_key().to_bytes()),
                    invitee_name: Some("Racing guest".into()),
                    expires_at_ns: 600_000_000_000,
                },
            },
            12,
        )
        .unwrap()
        .output
        .unwrap();
    let signed_invitation = decode_and_verify_invitation_text(
        issued["signed_invitation"].as_str().unwrap(),
        Some(guest_root.verifying_key().to_bytes()),
        &RevocationList::new(),
        13,
    )
    .unwrap()
    .signed_invitation()
    .to_vec();
    let certificate = |key: &SigningKey, label: &str| {
        let mut cert = DeviceCert {
            user_public_key: guest_root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 12,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &guest_root).unwrap();
        cert
    };
    let challenge_a = store
        .join_challenge(
            guest_a.verifying_key().to_bytes(),
            JoinChallengeRequest {
                signed_invitation: signed_invitation.clone(),
                device_cert: certificate(&guest_a, "Racing guest A"),
                request_id: [96; 32],
            },
            13,
        )
        .unwrap();
    let challenge_b = store
        .join_challenge(
            guest_b.verifying_key().to_bytes(),
            JoinChallengeRequest {
                signed_invitation: signed_invitation.clone(),
                device_cert: certificate(&guest_b, "Racing guest B"),
                request_id: [97; 32],
            },
            13,
        )
        .unwrap();
    let proof_a = sign_join_proof(challenge_a.challenge, &guest_a).unwrap();
    let proof_b = sign_join_proof(challenge_b.challenge, &guest_b).unwrap();
    store
        .join_complete(
            guest_a.verifying_key().to_bytes(),
            JoinCompleteRequest { proof: proof_a },
            14,
        )
        .unwrap();
    let loser = store
        .join_complete(
            guest_b.verifying_key().to_bytes(),
            JoinCompleteRequest { proof: proof_b },
            14,
        )
        .unwrap_err();
    assert!(loser.to_string().contains("consumed"));
    let later = store
        .join_challenge(
            guest_b.verifying_key().to_bytes(),
            JoinChallengeRequest {
                signed_invitation,
                device_cert: certificate(&guest_b, "Racing guest B"),
                request_id: [98; 32],
            },
            15,
        )
        .unwrap_err();
    assert!(later.to_string().contains("consumed"));
}

#[tokio::test]
async fn guest_authenticated_action_stream_rejects_forced_owner_operation() {
    use iroh::{Endpoint, RelayMode, endpoint::presets};
    use qos_peer_state::join::{
        JoinChallengeRequest, JoinCompleteRequest, MemberRole, decode_and_verify_invitation_text,
        sign_join_proof,
    };
    use qos_relay::{
        auth,
        framing::{read_frame, write_frame},
    };

    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let dir = tempfile::tempdir().unwrap();
        let now = qos_relay::clock::now_ns();
        let owner_root = SigningKey::from_bytes(&[31; 32]);
        let owner_device = SigningKey::from_bytes(&[32; 32]);
        let guest_root = SigningKey::from_bytes(&[33; 32]);
        let guest_device = SigningKey::from_bytes(&[34; 32]);
        let (enrollment, guest_cert) = {
            let mut store = Store::open(dir.path(), "forced-admin").unwrap();
            let enrollment = approved(store.inspect(now).unwrap(), &owner_root, &owner_device);
            store.apply(&enrollment, now + 1).unwrap();
            let invite = ActionRequest {
                version: 1,
                room: enrollment.challenge.room.clone(),
                invocation_id: "owner-invite".into(),
                operation: NativeOperation::CollaborationInvite {
                    role: MemberRole::Editor,
                    recipient_user_public_key: Some(guest_root.verifying_key().to_bytes()),
                    invitee_name: Some("Guest".into()),
                    expires_at_ns: now + 600_000_000_000,
                },
            };
            let receipt = store
                .action(owner_device.verifying_key().to_bytes(), &invite, now + 2)
                .unwrap();
            let signed = decode_and_verify_invitation_text(
                receipt.output.unwrap()["signed_invitation"]
                    .as_str()
                    .unwrap(),
                Some(guest_root.verifying_key().to_bytes()),
                &RevocationList::new(),
                now + 3,
            )
            .unwrap()
            .signed_invitation()
            .to_vec();
            let mut guest_cert = DeviceCert {
                user_public_key: guest_root.verifying_key().to_bytes(),
                device_public_key: guest_device.verifying_key().to_bytes(),
                device_label: "Guest device".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: now,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut guest_cert, &guest_root).unwrap();
            let challenge = store
                .join_challenge(
                    guest_device.verifying_key().to_bytes(),
                    JoinChallengeRequest {
                        signed_invitation: signed,
                        device_cert: guest_cert.clone(),
                        request_id: [35; 32],
                    },
                    now + 3,
                )
                .unwrap();
            let proof = sign_join_proof(challenge.challenge, &guest_device).unwrap();
            store
                .join_complete(
                    guest_device.verifying_key().to_bytes(),
                    JoinCompleteRequest { proof },
                    now + 4,
                )
                .unwrap();
            (enrollment, guest_cert)
        };

        let router = action_server::start(dir.path(), "forced-admin", RelayMode::Disabled)
            .await
            .unwrap();
        let port = router
            .endpoint()
            .bound_sockets()
            .iter()
            .find(|socket| socket.is_ipv4())
            .unwrap()
            .port();
        let address = iroh_base::EndpointAddr::new(router.endpoint().id())
            .with_ip_addr(std::net::SocketAddr::from(([127, 0, 0, 1], port)));
        let client = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let connection = client.connect(address, actions::ACTION_ALPN).await.unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let nonce = [36; 32];
        write_frame(
            &mut send,
            &postcard::to_stdvec(&auth::Hello {
                cert: guest_cert,
                client_nonce: nonce,
            })
            .unwrap(),
        )
        .await
        .unwrap();
        let ack: auth::HelloAck =
            postcard::from_bytes(&read_frame(&mut recv, 8192).await.unwrap()).unwrap();
        auth::verify_pinned_host_ack(
            auth::AuthDomain::LegacyQos,
            &ack,
            &RevocationList::new(),
            now + 5,
            &nonce,
        )
        .unwrap();
        write_frame(
            &mut send,
            &postcard::to_stdvec(&auth::build_hello_ack2(
                auth::AuthDomain::LegacyQos,
                &guest_device,
                &ack.server_nonce,
            ))
            .unwrap(),
        )
        .await
        .unwrap();
        let forced = ActionRequest {
            version: 1,
            room: enrollment.challenge.room,
            invocation_id: "forced-owner-invite".into(),
            operation: NativeOperation::CollaborationInvite {
                role: MemberRole::Editor,
                recipient_user_public_key: None,
                invitee_name: None,
                expires_at_ns: now + 600_000_000_000,
            },
        };
        write_frame(&mut send, &serde_json::to_vec(&forced).unwrap())
            .await
            .unwrap();
        let receipt: ActionReceipt =
            serde_json::from_slice(&read_frame(&mut recv, 64 * 1024).await.unwrap()).unwrap();
        assert_eq!(receipt.status, "rejected");
        assert!(
            receipt
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Owner authority"))
        );
        assert!(
            !Store::open(dir.path(), "forced-admin")
                .unwrap()
                .state
                .receipts
                .keys()
                .any(|key| key.ends_with(":forced-owner-invite"))
        );
        connection.close(0u32.into(), b"done");
        client.close().await;
        router.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}
