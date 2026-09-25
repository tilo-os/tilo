use super::*;
use atomic_file::test_support::{
    Checkpoint, ExistingCheckpoint, with_existing_hooks, with_failure,
};

fn enrolled() -> (tempfile::TempDir, Store, SigningKey, EnrollmentRequest) {
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut store = Store::open(dir.path(), "durability").unwrap();
    let enrollment = crate::tests::approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&enrollment, 11).unwrap();
    (dir, store, device, enrollment)
}
fn insert(store: &Store, id: &str) -> ActionRequest {
    ActionRequest {
        version: 1,
        room: store.state.room.clone(),
        invocation_id: id.into(),
        operation: NativeOperation::WriterInsert {
            text: format!("edit {id}"),
        },
    }
}

#[test]
fn durability_failed_publication_blocks_stale_owner_and_recovers_exact_receipt() {
    for (checkpoint, published) in [
        (Checkpoint::BeforeDataFlush, false),
        (Checkpoint::BeforePublish, false),
        (Checkpoint::AfterPublish, true),
        (Checkpoint::BeforeCommit, true),
    ] {
        let (dir, mut store, device, enrollment) = enrolled();
        let before = store.state.generation;
        let request = insert(&store, "first");
        assert!(
            with_failure(checkpoint, || store.action(
                device.verifying_key().to_bytes(),
                &request,
                12
            ))
            .is_err()
        );
        let bytes = std::fs::read(dir.path().join("state.json")).unwrap();
        let observed: State = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(observed.generation, before + u64::from(published));
        assert!(
            store.peer().is_err(),
            "failed transaction still provides authority at {checkpoint:?}"
        );
        assert!(store.inspect(13).is_err());
        assert!(store.apply(&enrollment, 13).is_err());
        assert!(
            store
                .action(device.verifying_key().to_bytes(), &request, 13)
                .is_err()
        );
        assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), bytes);
        drop(store);
        let mut store = Store::open(dir.path(), "durability").unwrap();
        if published {
            let rows = store.peer().unwrap().rows();
            let outbox = serde_json::to_value(&store.state.outbox).unwrap();
            assert_eq!(
                store
                    .action(device.verifying_key().to_bytes(), &request, 14)
                    .unwrap()
                    .status,
                "completed"
            );
            assert_eq!(store.state.generation, before + 1);
            assert_eq!(store.peer().unwrap().rows(), rows);
            assert_eq!(serde_json::to_value(&store.state.outbox).unwrap(), outbox);
        }
        let second = insert(&store, "second");
        store
            .action(device.verifying_key().to_bytes(), &second, 15)
            .unwrap();
        assert_eq!(store.state.generation, before + u64::from(published) + 1);
    }
}

#[test]
fn durability_reopen_must_commit_observed_file_before_serving() {
    let (dir, store, _, _) = enrolled();
    drop(store);
    let bytes = std::fs::read(dir.path().join("state.json")).unwrap();
    assert!(
        with_existing_hooks(
            Some(ExistingCheckpoint::BeforeNamespaceFlush),
            |_| {},
            || Store::open(dir.path(), "durability")
        )
        .is_err()
    );
    assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), bytes);
    assert!(Store::open(dir.path(), "durability").is_ok());
}

#[test]
fn durability_duplicate_state_keys_are_not_silently_collapsed() {
    let (dir, store, _, _) = enrolled();
    drop(store);
    let path = dir.path().join("state.json");
    let bytes = std::fs::read_to_string(&path).unwrap();
    let ambiguous = bytes.replacen("\"generation\":", "\"generation\":7,\"generation\":", 1);
    assert_ne!(ambiguous, bytes);
    std::fs::write(&path, &ambiguous).unwrap();
    assert!(Store::open(dir.path(), "durability").is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), ambiguous);
}

#[test]
fn durability_legacy_upgrade_preserves_existing_collaboration_and_generation() {
    let (dir, mut store, device, _) = enrolled();
    let request = ActionRequest {
        operation: NativeOperation::CollaborationInvite {
            role: qos_peer_state::join::MemberRole::Editor,
            recipient_user_public_key: None,
            invitee_name: Some("preserved".into()),
            expires_at_ns: 600_000_000_000,
        },
        ..insert(&store, "invite")
    };
    store
        .action(device.verifying_key().to_bytes(), &request, 12)
        .unwrap();
    let mut old = serde_json::to_value(&store.state).unwrap();
    assert!(!old["invitations"].as_object().unwrap().is_empty());
    old["version"] = 2.into();
    old["outbox"] = serde_json::Value::Array(
        old["outbox"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["message"].clone())
            .collect(),
    );
    drop(store);
    std::fs::write(
        dir.path().join("state.json"),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    let reopened = Store::open(dir.path(), "durability").unwrap();
    let current = serde_json::to_value(&reopened.state).unwrap();
    assert_eq!(current["invitations"], old["invitations"]);
    assert_eq!(current["generation"], old["generation"]);
    assert_eq!(current["receipts"], old["receipts"]);
}

#[test]
fn durability_unknown_aggregate_fields_refuse_without_loss() {
    let (dir, store, _, _) = enrolled();
    drop(store);
    let path = dir.path().join("state.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["future_receipts"] = serde_json::json!({"keep": 1});
    let bytes = serde_json::to_vec(&value).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert!(Store::open(dir.path(), "durability").is_err());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

fn poison(store: &mut Store) {
    let state = store.state.clone();
    let error = with_failure(Checkpoint::BeforeCommit, || store.commit(state)).unwrap_err();
    assert!(error.is::<UncertainStoreCommit>());
}

#[test]
fn durability_cached_model_admission_and_completion_require_reconciliation() {
    for completed in [false, true] {
        let (dir, mut store, device, _) = enrolled();
        let source = device.verifying_key().to_bytes();
        let request = ActionRequest {
            operation: NativeOperation::ModelComplete {
                text: "one request".into(),
            },
            ..insert(&store, "model")
        };
        store.action(source, &request, 12).unwrap();
        let expected = store.state.pending_models.values().next().unwrap().clone();
        if completed {
            store
                .finish_model(
                    source,
                    &request,
                    &expected,
                    gateway::Outcome::Completed("saved".into()),
                    13,
                )
                .unwrap();
        }
        poison(&mut store);
        let saved = std::fs::read(dir.path().join("state.json")).unwrap();
        let result = if completed {
            store.finish_model_authorized(
                source,
                &request,
                &expected,
                gateway::Outcome::Completed("must not replace".into()),
                None,
            )
        } else {
            store.begin_model(source, &request, expected.digest.clone(), None)
        };
        assert!(
            result.is_err(),
            "cached model branch returned success from poisoned Store"
        );
        assert!(store.authorize_model(source, 14).is_err());
        assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), saved);
    }
}
#[test]
fn durability_cached_mesh_challenge_and_catalogue_require_reconciliation() {
    let (dir, mut store, _, _) = enrolled();
    store.mesh_inspect(12).unwrap();
    assert!(store.mesh_inspect(13).is_ok());
    poison(&mut store);
    let saved = std::fs::read(dir.path().join("state.json")).unwrap();
    assert!(store.mesh_inspect(14).is_err());
    assert!(store.mesh_status(14).is_err());
    assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), saved);
}
#[test]
fn durability_cached_membership_challenge_requires_reconciliation() {
    let (dir, mut store, device, enrollment) = enrolled();
    let request = qos_peer_state::join::MembershipChallengeRequest {
        room: store.state.room.clone(),
        workspace_id: qos_relay::room_workspace_id(&store.state.room).unwrap(),
        device_cert: enrollment.browser_certificate,
        request_id: [45; 32],
    };
    let source = device.verifying_key().to_bytes();
    store
        .membership_challenge(source, request.clone(), 12)
        .unwrap();
    store
        .membership_challenge(source, request.clone(), 13)
        .unwrap();
    poison(&mut store);
    let saved = std::fs::read(dir.path().join("state.json")).unwrap();
    assert!(store.membership_challenge(source, request, 14).is_err());
    assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), saved);
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
#[test]
fn durability_collaboration_trait_preserves_uncertain_commit_category() {
    use qos_action_broker::collaboration_admin::*;
    let (dir, store, _, enrollment) = enrolled();
    let room = store.state.room.clone();
    drop(store);
    let service = crate::broker::CloudCollaborationAdmin::new(dir.path(), "durability", &room);
    let request = CollaborationAdminRequest {
        room,
        invocation: qos_actions::InvocationId::new(),
        caller: enrollment.browser_certificate,
        operation: CollaborationAdminOperation::List,
    };
    let runtime = runtime();
    let result = with_failure(Checkpoint::BeforeCommit, || {
        runtime.block_on(service.execute(request))
    });
    assert!(
        matches!(result, Err(CollaborationAdminError::Uncertain(_))),
        "{result:?}"
    );
}

#[test]
fn durability_join_transport_distinguishes_owner_unavailable_from_rejection() {
    use qos_peer_state::join::{
        JoinTransportRequest, JoinTransportResponse, MembershipChallengeRequest,
    };
    use qos_relay::workspace_join::WorkspaceJoinService;
    let (dir, store, device, enrollment) = enrolled();
    let request = MembershipChallengeRequest {
        room: store.state.room.clone(),
        workspace_id: qos_relay::room_workspace_id(&store.state.room).unwrap(),
        device_cert: enrollment.browser_certificate,
        request_id: [67; 32],
    };
    drop(store);
    let (updates, _receiver) = tokio::sync::mpsc::channel(1);
    let service = crate::collaboration::CloudJoinService::new(dir.path(), "durability", updates);
    let runtime = runtime();
    // Issuing a membership challenge is process-local (lane M): no commit.
    let challenge = with_failure(Checkpoint::BeforeCommit, || {
        runtime.block_on(service.handle(
            device.verifying_key().to_bytes(),
            JoinTransportRequest::MembershipChallenge(request),
        ))
    });
    let JoinTransportResponse::MembershipChallenge(challenge) = challenge else {
        panic!("challenge issuance must not need a Store commit: {challenge:?}")
    };
    let proof = qos_peer_state::join::sign_membership_proof(challenge.challenge, &device).unwrap();
    let result = with_failure(Checkpoint::BeforeCommit, || {
        runtime.block_on(service.handle(
            device.verifying_key().to_bytes(),
            JoinTransportRequest::MembershipComplete(
                qos_peer_state::join::MembershipCompleteRequest { proof },
            ),
        ))
    });
    assert!(
        matches!(result, JoinTransportResponse::Error { code: 503, .. }),
        "{result:?}"
    );
    let result = runtime.block_on(service.handle(
        [9; 32],
        JoinTransportRequest::Inspect {
            signed_invitation: vec![],
        },
    ));
    assert!(matches!(
        result,
        JoinTransportResponse::Error { code: 400, .. }
    ));
}

async fn addressed_request(
    address: iroh_base::EndpointAddr,
    enrollment: &EnrollmentRequest,
    device: &SigningKey,
    request: &ActionRequest,
) -> Result<ActionReceipt> {
    use qos_relay::{
        auth,
        framing::{read_frame, write_frame},
    };
    let client = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await?;
    let connection = client.connect(address, actions::ACTION_ALPN).await?;
    let result: Result<ActionReceipt> = async {
        let (mut send, mut recv) = connection.open_bi().await?;
        let nonce = [17; 32];
        write_frame(
            &mut send,
            &postcard::to_stdvec(&auth::Hello {
                cert: enrollment.browser_certificate.clone(),
                client_nonce: nonce,
            })?,
        )
        .await?;
        let ack: auth::HelloAck = postcard::from_bytes(&read_frame(&mut recv, 8192).await?)?;
        auth::verify_pinned_host_ack(
            auth::AuthDomain::LegacyQos,
            &ack,
            &RevocationList::new(),
            qos_relay::clock::now_ns(),
            &nonce,
        )?;
        write_frame(
            &mut send,
            &postcard::to_stdvec(&auth::build_hello_ack2(
                auth::AuthDomain::LegacyQos,
                device,
                &ack.server_nonce,
            ))?,
        )
        .await?;
        write_frame(&mut send, &serde_json::to_vec(request)?).await?;
        Ok(serde_json::from_slice(
            &read_frame(&mut recv, 64 * 1024).await?,
        )?)
    }
    .await;
    connection.close(0u32.into(), b"test complete");
    client.close().await;
    result
}
#[test]
fn durability_addressed_exchange_does_not_report_uncertain_commit_as_rejected() {
    let (dir, store, device, enrollment) = enrolled();
    let request = insert(&store, "network");
    drop(store);
    let runtime = runtime();
    let router = runtime
        .block_on(action_server::start(
            dir.path(),
            "durability",
            iroh::RelayMode::Disabled,
        ))
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
    let result = with_failure(Checkpoint::BeforeCommit, || {
        runtime.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                addressed_request(address, &enrollment, &device, &request),
            )
            .await
            .unwrap()
        })
    });
    runtime.block_on(router.shutdown()).unwrap();
    assert!(
        result.is_err(),
        "owner failure became a terminal wire receipt: {result:?}"
    );
    let mut store = Store::open(dir.path(), "durability").unwrap();
    assert_eq!(
        store
            .action(device.verifying_key().to_bytes(), &request, 16)
            .unwrap()
            .status,
        "completed"
    );
}

pub(super) fn document_message(store: &Store, device: &SigningKey) -> NeighbourMsg {
    use qos_actions::Action;
    let mut browser = store.peer_at(qos_relay::clock::now_ns()).unwrap();
    let app = "so.tilo.word-processor";
    let record = "tab:0000000000000001";
    let mut doc = automerge::AutoCommit::load(&browser.document(app, record).unwrap()).unwrap();
    let before = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(
        device.verifying_key().to_bytes().to_vec(),
    ));
    let model = qdoc::model::open(&doc, &automerge::ROOT).unwrap();
    let block = qdoc::model::blocks_in_order(&doc, &model)[0].clone();
    word_processor::actions::WordProcessorAction::SpliceText {
        region: word_processor::actions::RegionKind::Body,
        block,
        at: 0,
        del: 0,
        ins: "durability receive proof".into(),
    }
    .apply(&mut doc)
    .unwrap();
    doc.commit();
    browser
        .publish_incremental(
            app,
            record,
            doc.save_after(&before),
            device,
            now_time(qos_relay::clock::now_ns()),
        )
        .unwrap()
}

#[test]
fn durability_broker_cannot_convert_storage_failure_to_terminal_observation() {
    use qos_relay::peer_actions::{ActionMessage, DirectedAction};
    let (dir, store, device, enrollment) = enrolled();
    let room = store.state.room.parse().unwrap();
    drop(store);
    let request = DirectedAction {
        room,
        request_id: uuid::Uuid::new_v4(),
        source: device.verifying_key().to_bytes(),
        target: enrollment.host_certificate.device_public_key,
        message: ActionMessage::InvokeBound {
            host: room,
            invocation: qos_actions::InvocationId::new(),
            app_peer: String::new(),
            app_id: qos_action_broker::collaboration_admin::APP_ID.into(),
            record_id: String::new(),
            resource: qos_action_broker::collaboration_admin::resource(&room.to_string()),
            action: "invitation.list".into(),
            argument: None,
            confirmation: None,
            revision: None,
        },
    };
    let runtime = runtime();
    let result = with_failure(Checkpoint::BeforeCommit, || {
        runtime.block_on(crate::broker::handle(
            dir.path(),
            "durability",
            &enrollment.browser_certificate,
            &request,
            12,
        ))
    });
    assert!(
        matches!(&result, Err(error) if is_store_failure(error)),
        "storage failure became definitive observation: {result:?}"
    );
}

#[test]
fn durability_crash_child() {
    let Some(path) = std::env::var_os("QOS_STORE_TEST_ROOT") else {
        return;
    };
    let phase = std::env::var("QOS_STORE_TEST_PHASE").unwrap();
    let mut store = Store::open(Path::new(&path), "durability").unwrap();
    let request = insert(&store, "crash");
    let device = SigningKey::from_bytes(&[8; 32]);
    atomic_file::test_support::with_observer(
        move |event| {
            if format!("{:?}", event.checkpoint) == phase {
                std::process::exit(73);
            }
        },
        || {
            store
                .action(device.verifying_key().to_bytes(), &request, 12)
                .unwrap();
        },
    );
    // The transaction succeeded, but no application reply has been sent.
    std::process::exit(74);
}

#[test]
fn durability_process_interruption_preserves_one_generation_and_effect() {
    for (phase, published, exit) in [
        ("BeforePublish", false, 73),
        ("AfterPublish", true, 73),
        ("Committed", true, 73),
        ("AfterSuccess", true, 74),
    ] {
        let (dir, store, device, _) = enrolled();
        let generation = store.state.generation;
        let before_rows = store.peer().unwrap().rows();
        let request = insert(&store, "crash");
        drop(store);
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "durability_tests::durability_crash_child",
                "--nocapture",
            ])
            .env("QOS_STORE_TEST_ROOT", dir.path())
            .env("QOS_STORE_TEST_PHASE", phase)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(exit),
            "child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let bytes = std::fs::read(dir.path().join("state.json")).unwrap();
        let observed: State = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(observed.generation, generation + u64::from(published));
        assert!(
            with_existing_hooks(
                Some(ExistingCheckpoint::BeforeNamespaceFlush),
                |_| {},
                || Store::open(dir.path(), "durability")
            )
            .is_err()
        );
        assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), bytes);
        let mut store = Store::open(dir.path(), "durability").unwrap();
        if published {
            let rows = store.peer().unwrap().rows();
            assert_ne!(rows, before_rows);
            let publications = serde_json::to_value(&store.state.outbox).unwrap();
            let receipt = store
                .action(device.verifying_key().to_bytes(), &request, 13)
                .unwrap();
            assert_eq!(receipt.status, "completed");
            assert_eq!(store.state.generation, generation + 1);
            assert_eq!(store.peer().unwrap().rows(), rows);
            assert_eq!(
                serde_json::to_value(&store.state.outbox).unwrap(),
                publications
            );
        } else {
            assert_eq!(store.peer().unwrap().rows(), before_rows);
            assert!(store.state.receipts.is_empty());
        }
        let second = insert(&store, "after-crash");
        store
            .action(device.verifying_key().to_bytes(), &second, 14)
            .unwrap();
        assert_eq!(
            store.state.generation,
            generation + u64::from(published) + 1
        );
        assert_eq!(store.state.receipts.len(), usize::from(published) + 1);
    }
}

#[test]
fn durability_nested_duplicate_keys_and_trailing_json_refuse_without_rewrite() {
    for suffix in [",\"nested\":{\"id\":1,\"id\":2}", ",\"nested\":1"] {
        let (dir, store, _, _) = enrolled();
        let mut value = serde_json::to_value(&store.state).unwrap();
        value["receipts"]["fixture"] = serde_json::json!(["digest", {"invocation_id":"fixture","status":"completed","error":null,"output":{"marker":null}}]);
        drop(store);
        let text = serde_json::to_string(&value)
            .unwrap()
            .replace("\"marker\":null", &format!("\"marker\":null{suffix}"));
        let text = if suffix.ends_with(":1") {
            format!("{text} true")
        } else {
            text
        };
        std::fs::write(dir.path().join("state.json"), &text).unwrap();
        assert!(Store::open(dir.path(), "durability").is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("state.json")).unwrap(),
            text
        );
    }
}

#[test]
fn durability_receive_owner_failure_never_audits_committed_document() {
    let (dir, mut store, device, _) = enrolled();
    // Refresh the real attestation to current wall time before a live edit.
    let now = qos_relay::clock::now_ns();
    let (before, mut peer) = store.peer_transition_at(now).unwrap();
    let messages = peer
        .durable_bootstrap_messages(
            &store.key(),
            now_time(now),
            store.next_generation().unwrap(),
        )
        .unwrap();
    let mut state = store.state.clone();
    state
        .outbox
        .extend(live_projection_from_bootstrap(&before, &peer, &messages).unwrap());
    state.bound.as_mut().unwrap().peer = peer.durable().unwrap();
    store.commit(state).unwrap();
    let message = document_message(&store, &device);
    let before_rows = store.peer().unwrap().rows();
    let mut invalid = message.clone();
    if let NeighbourMsg::AppDocument { envelope, .. } = &mut invalid {
        envelope.signature = collab_types::Signature::from_bytes([0; 64]);
    }
    assert!(matches!(
        store.receive(
            TransportOrigin::RelayDelta(device.verifying_key().to_bytes()),
            invalid,
            now
        ),
        Err(ReceiveError::Rejected(_))
    ));
    take_test_audit_events();
    let result = with_failure(Checkpoint::BeforeCommit, || {
        store.receive(
            TransportOrigin::RelayDelta(device.verifying_key().to_bytes()),
            message,
            qos_relay::clock::now_ns(),
        )
    });
    assert!(matches!(result, Err(ReceiveError::Owner(_))));
    assert!(
        !take_test_audit_events()
            .iter()
            .any(|(event, _)| event == "collaboration_document_committed")
    );
    assert!(store.peer().is_err());
    drop(store);
    assert_ne!(
        Store::open(dir.path(), "durability")
            .unwrap()
            .peer()
            .unwrap()
            .rows(),
        before_rows
    );
}

#[test]
fn durability_populated_current_state_and_typed_publications_survive_uncertain_reopen() {
    use qos_action_broker::collaboration_admin::*;
    use qos_peer_state::join::*;
    let (dir, store, device, enrollment) = enrolled();
    let room = store.state.room.clone();
    drop(store);
    // Canonical intent is created through the same real broker adapter as production.
    let service = crate::broker::CloudCollaborationAdmin::new(dir.path(), "durability", &room);
    runtime()
        .block_on(service.execute(CollaborationAdminRequest {
            room: room.clone(),
            invocation: qos_actions::InvocationId::new(),
            caller: enrollment.browser_certificate.clone(),
            operation: CollaborationAdminOperation::Issue(IssueInvitationInput {
                role: CollaborationRole::Editor,
                expires_in_seconds: 600,
                invitee_name: Some("retained guest".into()),
            }),
        }))
        .unwrap();
    let mut store = Store::open(dir.path(), "durability").unwrap();
    let now = qos_relay::clock::now_ns();
    let guest_root = SigningKey::from_bytes(&[31; 32]);
    let guest_device = SigningKey::from_bytes(&[32; 32]);
    let mut cert = DeviceCert {
        user_public_key: guest_root.verifying_key().to_bytes(),
        device_public_key: guest_device.verifying_key().to_bytes(),
        device_label: "retained guest".into(),
        capabilities: qos_user_identity::DeviceCapabilities::member(),
        issued_at_ns: now,
        expires_at_ns: None,
        signature: [0; 64],
    };
    qos_user_identity::sign_device_cert(&mut cert, &guest_root).unwrap();
    let request = JoinChallengeRequest {
        signed_invitation: store
            .state
            .invitations
            .values()
            .next()
            .unwrap()
            .signed_invitation
            .clone(),
        device_cert: cert.clone(),
        request_id: [48; 32],
    };
    let challenge = store
        .join_challenge(cert.device_public_key, request.clone(), now)
        .unwrap();
    store
        .join_challenge(cert.device_public_key, request.clone(), now)
        .unwrap();
    let proof = JoinCompleteRequest {
        proof: sign_join_proof(challenge.challenge, &guest_device).unwrap(),
    };
    store
        .join_complete(cert.device_public_key, proof.clone(), now)
        .unwrap();
    store
        .join_complete(cert.device_public_key, proof.clone(), now)
        .unwrap();
    let membership = MembershipChallengeRequest {
        room,
        workspace_id: qos_relay::room_workspace_id(&store.state.room).unwrap(),
        device_cert: cert.clone(),
        request_id: [49; 32],
    };
    let challenge = store
        .membership_challenge(cert.device_public_key, membership.clone(), now)
        .unwrap();
    let refresh = MembershipCompleteRequest {
        proof: sign_membership_proof(challenge.challenge, &guest_device).unwrap(),
    };
    // Without process reuse (a restarted host) the refresh commits a receipt.
    crate::collaboration::forget_reused_governance(dir.path());
    store
        .membership_complete(cert.device_public_key, refresh.clone(), now)
        .unwrap();
    store
        .membership_complete(cert.device_public_key, refresh.clone(), now)
        .unwrap();
    let mut pending_membership = membership.clone();
    pending_membership.request_id = [50; 32];
    store
        .membership_challenge(cert.device_public_key, pending_membership, now)
        .unwrap();
    // Completed proofs consume their challenges; retain a second pending join too.
    let pending_invite = ActionRequest {
        operation: NativeOperation::CollaborationInvite {
            role: MemberRole::Editor,
            recipient_user_public_key: None,
            invitee_name: Some("pending guest".into()),
            expires_at_ns: now + 600_000_000_000,
        },
        ..insert(&store, "pending-invite")
    };
    let issued = store
        .action(device.verifying_key().to_bytes(), &pending_invite, now)
        .unwrap()
        .output
        .unwrap();
    let signed = decode_and_verify_invitation_text(
        issued["signed_invitation"].as_str().unwrap(),
        None,
        &RevocationList::new(),
        now,
    )
    .unwrap()
    .signed_invitation()
    .to_vec();
    let pending_root = SigningKey::from_bytes(&[33; 32]);
    let pending_device = SigningKey::from_bytes(&[34; 32]);
    let mut pending_cert = cert.clone();
    pending_cert.user_public_key = pending_root.verifying_key().to_bytes();
    pending_cert.device_public_key = pending_device.verifying_key().to_bytes();
    qos_user_identity::sign_device_cert(&mut pending_cert, &pending_root).unwrap();
    store
        .join_challenge(
            pending_cert.device_public_key,
            JoinChallengeRequest {
                signed_invitation: signed,
                device_cert: pending_cert,
                request_id: [51; 32],
            },
            now,
        )
        .unwrap();
    store.mesh_inspect(now).unwrap();
    let model = ActionRequest {
        operation: NativeOperation::ModelComplete {
            text: "pending survives".into(),
        },
        ..insert(&store, "pending")
    };
    store
        .action(device.verifying_key().to_bytes(), &model, now)
        .unwrap();
    let writer = insert(&store, "preserved-write");
    store
        .action(device.verifying_key().to_bytes(), &writer, now)
        .unwrap();
    assert!(
        store
            .state
            .outbox
            .iter()
            .any(|item| item.kind == PublicationKind::Delta)
    );
    assert!(
        store
            .state
            .outbox
            .iter()
            .any(|item| item.kind == PublicationKind::Checkpoint)
    );
    let expected = serde_json::to_value(&store.state).unwrap();
    for field in [
        "consumed",
        "receipts",
        "pending_models",
        "canonical_intents",
        "invitations",
        "join_challenges",
        "join_responses",
        // Membership challenges are process-local (lane M), never persisted.
        "membership_responses",
    ] {
        assert!(
            !expected[field].as_object().unwrap().is_empty(),
            "fixture did not populate {field}"
        );
    }
    poison(&mut store);
    assert!(
        store
            .join_challenge(cert.device_public_key, request, now)
            .is_err()
    );
    assert!(
        store
            .join_complete(cert.device_public_key, proof, now)
            .is_err()
    );
    assert!(
        store
            .membership_challenge(cert.device_public_key, membership, now)
            .is_err()
    );
    assert!(
        store
            .membership_complete(cert.device_public_key, refresh, now)
            .is_err()
    );
    drop(store);
    let reopened = Store::open(dir.path(), "durability").unwrap();
    let mut actual = serde_json::to_value(&reopened.state).unwrap();
    assert_eq!(
        actual["generation"].as_u64().unwrap(),
        expected["generation"].as_u64().unwrap() + 1
    );
    actual["generation"] = expected["generation"].clone();
    assert_eq!(
        actual, expected,
        "reconciliation changed persisted semantic state"
    );
}

#[test]
fn durability_generation_exhaustion_is_owner_failure_and_blocks_authority() {
    let (_dir, mut store, _, _) = enrolled();
    store.state.generation = u64::MAX;
    let candidate = store.state.clone();
    let error = store.commit(candidate).unwrap_err();
    assert!(
        is_store_failure(&error),
        "generation exhaustion was ordinary rejection"
    );
    assert!(
        store.peer().is_err(),
        "failed owner transaction still provided authority"
    );
}
