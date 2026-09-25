use super::*;
use qos_user_identity::machine_mesh::{MeshRequest, sign_request};
fn linked() -> (tempfile::TempDir, tempfile::TempDir, MeshRequest) {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let ra = SigningKey::from_bytes(&[7; 32]);
    let rb = SigningKey::from_bytes(&[9; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut sa = Store::open(a.path(), "a").unwrap();
    let mut sb = Store::open(b.path(), "b").unwrap();
    for (store, root) in [(&mut sa, &ra), (&mut sb, &rb)] {
        let r = super::tests::approved(store.inspect(10).unwrap(), root, &device);
        store.apply(&r, 11).unwrap();
    }
    let mut request = MeshRequest {
        version: 1,
        kind: "link".into(),
        link_id: "link-1".into(),
        source: sa.mesh_inspect(20).unwrap(),
        target: sb.mesh_inspect(20).unwrap(),
        allowed_actions: vec!["model_complete".into()],
        source_signature: vec![],
        target_signature: vec![],
    };
    sign_request(&mut request, &ra, &rb).unwrap();
    (a, b, request)
}
#[test]
fn mesh_bilateral_replay_substitution_and_durable_unlink() {
    let (a, b, r) = linked();
    let mut sa = Store::open(a.path(), "a").unwrap();
    let mut sb = Store::open(b.path(), "b").unwrap();
    let before = sa.state.bound.as_ref().unwrap().peer.clone();
    let room = sa.state.room.clone();
    let mut bad = r.clone();
    bad.target.machine_id = "c".into();
    assert!(sa.mesh_apply(&bad, 21).is_err());
    bad = r.clone();
    bad.target_signature = r.source_signature.clone();
    assert!(sa.mesh_apply(&bad, 21).is_err());
    sa.mesh_apply(&r, 21).unwrap();
    sb.mesh_apply(&r, 21).unwrap();
    sa.mesh_apply(&r, 22).unwrap();
    assert_eq!(sa.state.bound.as_ref().unwrap().peer, before);
    assert_eq!(sa.state.room, room);
    drop(sa);
    let mut sa = Store::open(a.path(), "a").unwrap();
    assert_eq!(
        sa.mesh_status(22).unwrap()["links"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let mut unlink = r.clone();
    unlink.kind = "unlink".into();
    unlink.source = sa.mesh_inspect(30).unwrap();
    unlink.target = sb.mesh_inspect(30).unwrap();
    sign_request(
        &mut unlink,
        &SigningKey::from_bytes(&[7; 32]),
        &SigningKey::from_bytes(&[9; 32]),
    )
    .unwrap();
    sa.mesh_apply(&unlink, 31).unwrap();
    drop(sa);
    let mut sa = Store::open(a.path(), "a").unwrap();
    assert_eq!(sa.mesh_status(32).unwrap()["links"][0]["active"], false);
    let persisted = std::fs::read(a.path().join("state.json")).unwrap();
    let mut repeated = unlink.clone();
    repeated.source.nonce = [99; 32];
    sign_request(
        &mut repeated,
        &SigningKey::from_bytes(&[7; 32]),
        &SigningKey::from_bytes(&[9; 32]),
    )
    .unwrap();
    sa.mesh_apply(&repeated, 32).unwrap();
    assert_eq!(
        persisted,
        std::fs::read(a.path().join("state.json")).unwrap()
    );

    assert!(sa.mesh_apply(&r, 33).is_err());
    assert_eq!(sa.mesh_status(33).unwrap()["links"][0]["active"], false);
}
#[tokio::test]
async fn mesh_actions_are_scoped_deduplicated_and_revoked() {
    let (a, b, r) = linked();
    let mut sa = Store::open(a.path(), "a").unwrap();
    let mut sb = Store::open(b.path(), "b").unwrap();
    sa.mesh_apply(&r, 21).unwrap();
    sb.mesh_apply(&r, 21).unwrap();
    drop(sa);
    drop(sb);
    let request = mesh::MeshAction {
        version: 1,
        link_id: r.link_id.clone(),
        source_machine_id: "a".into(),
        target_machine_id: "b".into(),
        source_device_public_key: [8; 32],
        request: ActionRequest {
            version: 1,
            room: format!("mesh:{}", r.link_id),
            invocation_id: "task-1".into(),
            operation: NativeOperation::ModelComplete {
                text: "Explain the sky".into(),
            },
        },
    };
    let result = mesh::invoke(
        b.path(),
        "b",
        &r.source.host_certificate,
        &request,
        22,
        |_, text| async move { gateway::Outcome::Completed(format!("executed:{text}")) },
    )
    .await
    .unwrap();
    assert_eq!(result.output.as_ref().unwrap()["execution_machine_id"], "b");
    assert_eq!(
        result.output.as_ref().unwrap()["text"],
        "executed:Explain the sky"
    );
    let repeat = mesh::invoke(
        b.path(),
        "b",
        &r.source.host_certificate,
        &request,
        23,
        |_, _| async { panic!("duplicate executed") },
    )
    .await
    .unwrap();
    assert_eq!(result, repeat);
    let mut altered = request.clone();
    altered.request.operation = NativeOperation::WriterInsert {
        text: "private".into(),
    };
    assert!(
        mesh::invoke(
            b.path(),
            "b",
            &r.source.host_certificate,
            &altered,
            24,
            |_, _| async { panic!() }
        )
        .await
        .is_err()
    );
    altered = request.clone();
    altered.request.operation = NativeOperation::ModelComplete {
        text: "substitute".into(),
    };
    assert!(
        mesh::invoke(
            b.path(),
            "b",
            &r.source.host_certificate,
            &altered,
            24,
            |_, _| async { panic!() }
        )
        .await
        .is_err()
    );
    altered = request.clone();
    altered.source_machine_id = "c".into();
    assert!(
        mesh::invoke(
            b.path(),
            "b",
            &r.source.host_certificate,
            &altered,
            24,
            |_, _| async { panic!() }
        )
        .await
        .is_err()
    );
    let mut revoke = r.clone();
    revoke.kind = "unlink".into();
    sign_request(
        &mut revoke,
        &SigningKey::from_bytes(&[7; 32]),
        &SigningKey::from_bytes(&[9; 32]),
    )
    .unwrap();
    Store::open(b.path(), "b")
        .unwrap()
        .mesh_apply(&revoke, 300_000_000_050)
        .unwrap();
    assert!(
        mesh::invoke(
            b.path(),
            "b",
            &r.source.host_certificate,
            &request,
            300_000_000_051,
            |_, _| async { panic!() }
        )
        .await
        .is_err()
    );
}
#[test]
fn mesh_rejects_wrong_roots_self_links_expiry_and_supports_refreshed_retry() {
    let (a, b, r) = linked();
    let mut sa = Store::open(a.path(), "a").unwrap();
    let mut sb = Store::open(b.path(), "b").unwrap();
    let ra = SigningKey::from_bytes(&[7; 32]);
    let rb = SigningKey::from_bytes(&[9; 32]);
    let mut wrong = r.clone();
    wrong.target = wrong.source.clone();
    sign_request(&mut wrong, &ra, &ra).unwrap();
    assert!(sa.mesh_apply(&wrong, 21).is_err());
    wrong = r.clone();
    wrong.source.root_public_key = rb.verifying_key().to_bytes();
    assert!(sign_request(&mut wrong, &ra, &rb).is_err());
    wrong = r.clone();
    wrong.allowed_actions.push("writer_insert".into());
    sign_request(&mut wrong, &ra, &rb).unwrap();
    assert!(sa.mesh_apply(&wrong, 21).is_err());
    assert!(sa.mesh_apply(&r, 300_000_000_021).is_err());
    sa.mesh_apply(&r, 21).unwrap();
    let mut fresh = r.clone();
    fresh.source = sa.mesh_inspect(300_000_000_100).unwrap();
    fresh.target = sb.mesh_inspect(300_000_000_100).unwrap();
    sign_request(&mut fresh, &ra, &rb).unwrap();
    sa.mesh_apply(&fresh, 300_000_000_101).unwrap();
    sb.mesh_apply(&fresh, 300_000_000_101).unwrap();
    assert_eq!(
        sa.mesh_status(300_000_000_102).unwrap()["links"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[tokio::test]
async fn two_hosts_exchange_real_action_catalogue_cancellation_and_receipts() {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let (a, b, mut grant) = linked();
        let ar = action_server::start(a.path(), "a", iroh::RelayMode::Disabled)
            .await
            .unwrap();
        // Replace only the provider call; production handler/auth/admission/framing remain real.
        let endpoint_key = Store::open(b.path(), "b").unwrap().action_key();
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(endpoint_key)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let br = iroh::protocol::Router::builder(endpoint)
            .accept(
                mesh::MESH_ALPN,
                mesh::Handler {
                    root: b.path().into(),
                    machine: "b".into(),
                    slots: tokio::sync::Semaphore::new(8),
                    gateway: gateway::Gateway::test_model("executed on b: "),
                },
            )
            .spawn();
        let address = |router: &iroh::protocol::Router| {
            let port = router
                .endpoint()
                .bound_sockets()
                .iter()
                .find(|s| s.is_ipv4())
                .unwrap()
                .port();
            serde_json::to_string(
                &iroh_base::EndpointAddr::new(router.endpoint().id())
                    .with_ip_addr(std::net::SocketAddr::from(([127, 0, 0, 1], port))),
            )
            .unwrap()
        };
        let now = qos_relay::clock::now_ns();
        {
            let mut sa = Store::open(a.path(), "a").unwrap();
            let mut sb = Store::open(b.path(), "b").unwrap();
            grant.source = sa.mesh_inspect(now).unwrap();
            grant.target = sb.mesh_inspect(now).unwrap();
            grant.source.action_address = address(&ar);
            grant.target.action_address = address(&br);
            sa.state.mesh.challenge = Some(grant.source.clone());
            sa.save(&sa.state).unwrap();
            sb.state.mesh.challenge = Some(grant.target.clone());
            sb.save(&sb.state).unwrap();
            sign_request(
                &mut grant,
                &SigningKey::from_bytes(&[7; 32]),
                &SigningKey::from_bytes(&[9; 32]),
            )
            .unwrap();
            sa.mesh_apply(&grant, now).unwrap();
            sb.mesh_apply(&grant, now).unwrap();
        }
        let source = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
        let request = |id: &str, operation| ActionRequest {
            version: 1,
            room: grant.source.room.clone(),
            invocation_id: id.into(),
            operation: NativeOperation::Remote {
                link_id: grant.link_id.clone(),
                target_machine_id: "b".into(),
                operation: Box::new(operation),
            },
        };
        let catalogue = mesh::forward(
            a.path(),
            "a",
            ar.endpoint(),
            source,
            &request("catalogue", NativeOperation::Catalogue),
        )
        .await
        .unwrap();
        assert_eq!(
            catalogue.output.as_ref().unwrap()["execution_machine_id"],
            "b"
        );
        assert_eq!(catalogue.output.as_ref().unwrap()["agent_execution"], false);
        let model = request(
            "compute",
            NativeOperation::ModelComplete {
                text: "summarize this shared task".into(),
            },
        );
        let completed = mesh::forward(a.path(), "a", ar.endpoint(), source, &model)
            .await
            .unwrap();
        assert_eq!(completed.status, "completed");
        assert_eq!(
            completed.output.as_ref().unwrap()["text"],
            "executed on b: summarize this shared task"
        );
        assert_eq!(
            completed.output.as_ref().unwrap()["execution_machine_id"],
            "b"
        );
        assert_eq!(
            completed,
            mesh::forward(a.path(), "a", ar.endpoint(), source, &model)
                .await
                .unwrap()
        );
        assert!(
            Store::open(b.path(), "b")
                .unwrap()
                .state
                .pending_models
                .is_empty()
        );
        let cancel = request(
            "cancel",
            NativeOperation::Cancel {
                invocation_id: "future-job".into(),
            },
        );
        assert_eq!(
            mesh::forward(a.path(), "a", ar.endpoint(), source, &cancel)
                .await
                .unwrap()
                .status,
            "completed"
        );
        assert_eq!(
            mesh::forward(a.path(), "a", ar.endpoint(), source, &cancel)
                .await
                .unwrap()
                .status,
            "completed"
        );
        let status = mesh::forward(
            a.path(),
            "a",
            ar.endpoint(),
            source,
            &request(
                "status",
                NativeOperation::Status {
                    invocation_id: "future-job".into(),
                },
            ),
        )
        .await
        .unwrap();
        assert_eq!(status.output.as_ref().unwrap()["status"], "cancelled");
        let mut revoke = grant.clone();
        revoke.kind = "unlink".into();
        sign_request(
            &mut revoke,
            &SigningKey::from_bytes(&[7; 32]),
            &SigningKey::from_bytes(&[9; 32]),
        )
        .unwrap();
        Store::open(b.path(), "b")
            .unwrap()
            .mesh_apply(&revoke, now)
            .unwrap();
        assert!(
            mesh::forward(
                a.path(),
                "a",
                ar.endpoint(),
                source,
                &request("revoked", NativeOperation::Catalogue)
            )
            .await
            .is_err()
        );
        ar.shutdown().await.unwrap();
        br.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn mesh_pending_recovery_limits_foreign_actor_and_inflight_revoke() {
    let (a, b, r) = linked();
    Store::open(a.path(), "a")
        .unwrap()
        .mesh_apply(&r, 21)
        .unwrap();
    Store::open(b.path(), "b")
        .unwrap()
        .mesh_apply(&r, 21)
        .unwrap();
    let call = mesh::MeshAction {
        version: 1,
        link_id: r.link_id.clone(),
        source_machine_id: "a".into(),
        target_machine_id: "b".into(),
        source_device_public_key: [8; 32],
        request: ActionRequest {
            version: 1,
            room: format!("mesh:{}", r.link_id),
            invocation_id: "pending".into(),
            operation: NativeOperation::ModelComplete {
                text: "task".into(),
            },
        },
    };
    let ids = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let observed = ids.clone();
    let first = mesh::invoke(
        b.path(),
        "b",
        &r.source.host_certificate,
        &call,
        22,
        |pending, _| async move {
            observed.lock().unwrap().push(pending.gateway_id);
            gateway::Outcome::Pending
        },
    )
    .await
    .unwrap();
    assert_eq!(first.status, "pending");
    assert_eq!(
        Store::open(b.path(), "b")
            .unwrap()
            .state
            .pending_models
            .len(),
        1
    );
    let mut alias = call.clone();
    alias.request.operation = NativeOperation::Cancel {
        invocation_id: "other-job".into(),
    };
    assert!(
        mesh::invoke(
            b.path(),
            "b",
            &r.source.host_certificate,
            &alias,
            23,
            |_, _| async { panic!() }
        )
        .await
        .is_err(),
        "pending invocation id cannot be reused for cancellation"
    );
    let observed = ids.clone();
    let result = mesh::invoke(
        b.path(),
        "b",
        &r.source.host_certificate,
        &call,
        23,
        |pending, _| async move {
            observed.lock().unwrap().push(pending.gateway_id);
            gateway::Outcome::Completed("resumed".into())
        },
    )
    .await
    .unwrap();
    assert_eq!(result.status, "completed");
    {
        let ids = ids.lock().unwrap();
        assert_eq!(ids[0], ids[1]);
    }
    let mut foreign = call.clone();
    foreign.request.operation = NativeOperation::Status {
        invocation_id: "pending".into(),
    };
    foreign.request.invocation_id = "status".into();
    foreign.source_device_public_key = [99; 32];
    let status = mesh::invoke(
        b.path(),
        "b",
        &r.source.host_certificate,
        &foreign,
        24,
        |_, _| async { panic!() },
    )
    .await
    .unwrap();
    assert_eq!(status.output, Some(serde_json::Value::Null));
    let mut over = call.clone();
    over.request.invocation_id = "large".into();
    over.request.operation = NativeOperation::ModelComplete {
        text: "x".repeat(32769),
    };
    assert!(
        mesh::invoke(
            b.path(),
            "b",
            &r.source.host_certificate,
            &over,
            25,
            |_, _| async { panic!() }
        )
        .await
        .is_err()
    );
    let mut later = call.clone();
    later.request.invocation_id = "inflight".into();
    let mut revoke = r.clone();
    revoke.kind = "unlink".into();
    sign_request(
        &mut revoke,
        &SigningKey::from_bytes(&[7; 32]),
        &SigningKey::from_bytes(&[9; 32]),
    )
    .unwrap();
    assert!(
        mesh::invoke(
            b.path(),
            "b",
            &r.source.host_certificate,
            &later,
            26,
            |_, _| async {
                Store::open(b.path(), "b")
                    .unwrap()
                    .mesh_apply(&revoke, 27)
                    .unwrap();
                gateway::Outcome::Completed("must not deliver".into())
            }
        )
        .await
        .is_err()
    );
    assert!(
        Store::open(b.path(), "b")
            .unwrap()
            .state
            .pending_models
            .is_empty()
    );
}

/// The reply is deliberately withheld until the original browser is revoked.
#[derive(Debug)]
struct RevokingReply {
    source_root: PathBuf,
    key: SigningKey,
    cert: DeviceCert,
    browser: [u8; 32],
}
impl iroh::protocol::ProtocolHandler for RevokingReply {
    async fn accept(
        &self,
        connection: iroh::endpoint::Connection,
    ) -> Result<(), iroh::protocol::AcceptError> {
        async {
            use qos_relay::{
                auth,
                framing::{read_frame, write_frame},
            };
            let (mut send, mut recv) = connection.accept_bi().await?;
            let hello: auth::Hello = postcard::from_bytes(&read_frame(&mut recv, 8192).await?)?;
            let ack = auth::build_hello_ack(
                auth::AuthDomain::LegacyQos,
                self.cert.clone(),
                &self.key,
                [17; 32],
                &hello.client_nonce,
            );
            write_frame(&mut send, &postcard::to_stdvec(&ack)?).await?;
            let proof: auth::HelloAck2 = postcard::from_bytes(&read_frame(&mut recv, 8192).await?)?;
            auth::verify_hello_ack2(
                auth::AuthDomain::LegacyQos,
                &proof,
                &hello.cert.device_public_key,
                &ack.server_nonce,
            )?;
            let call: mesh::MeshAction =
                serde_json::from_slice(&read_frame(&mut recv, 64 * 1024).await?)?;
            {
                let mut source = Store::open(&self.source_root, "a")?;
                let mut state = source.state.clone();
                state
                    .bound
                    .as_mut()
                    .unwrap()
                    .roster
                    .retain(|c| c.device_public_key != self.browser);
                source.commit(state)?;
            }
            let receipt = ActionReceipt {
                invocation_id: call.request.invocation_id,
                status: "completed".into(),
                error: None,
                output: Some(serde_json::json!({"text":"delayed result"})),
            };
            write_frame(&mut send, &serde_json::to_vec(&receipt)?).await?;
            send.finish()?;
            connection.closed().await;
            Ok::<_, anyhow::Error>(())
        }
        .await
        .map_err(|e| iroh::protocol::AcceptError::from_err(std::io::Error::other(e.to_string())))
    }
}
#[tokio::test]
async fn source_revoked_while_remote_action_runs_cannot_receive_result() {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let (a, b, mut grant) = linked();
        let now = qos_relay::clock::now_ns();
        let browser = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
        let ar = action_server::start(a.path(), "a", iroh::RelayMode::Disabled)
            .await
            .unwrap();
        let (endpoint_key, key, cert) = {
            let store = Store::open(b.path(), "b").unwrap();
            (
                store.action_key(),
                store.key(),
                store.state.bound.as_ref().unwrap().authority.clone(),
            )
        };
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(endpoint_key)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let br = iroh::protocol::Router::builder(endpoint)
            .accept(
                mesh::MESH_ALPN,
                RevokingReply {
                    source_root: a.path().into(),
                    key,
                    cert,
                    browser,
                },
            )
            .spawn();
        {
            let mut sa = Store::open(a.path(), "a").unwrap();
            let mut sb = Store::open(b.path(), "b").unwrap();
            grant.source = sa.mesh_inspect(now).unwrap();
            grant.target = sb.mesh_inspect(now).unwrap();
            let port = br
                .endpoint()
                .bound_sockets()
                .iter()
                .find(|s| s.is_ipv4())
                .unwrap()
                .port();
            grant.target.action_address = serde_json::to_string(
                &iroh_base::EndpointAddr::new(br.endpoint().id())
                    .with_ip_addr(std::net::SocketAddr::from(([127, 0, 0, 1], port))),
            )
            .unwrap();
            sb.state.mesh.challenge = Some(grant.target.clone());
            sb.save(&sb.state).unwrap();
            sign_request(
                &mut grant,
                &SigningKey::from_bytes(&[7; 32]),
                &SigningKey::from_bytes(&[9; 32]),
            )
            .unwrap();
            sa.mesh_apply(&grant, now).unwrap();
            sb.mesh_apply(&grant, now).unwrap();
        }
        let call = ActionRequest {
            version: 1,
            room: grant.source.room.clone(),
            invocation_id: "delayed".into(),
            operation: NativeOperation::Remote {
                link_id: grant.link_id.clone(),
                target_machine_id: "b".into(),
                operation: Box::new(NativeOperation::ModelComplete {
                    text: "Task".into(),
                }),
            },
        };
        let error = mesh::forward(a.path(), "a", ar.endpoint(), browser, &call)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not admitted"));
        ar.shutdown().await.unwrap();
        br.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn mesh_pending_capacity_is_bounded_and_unlink_reclaims_slots() {
    let (a, b, r) = linked();
    Store::open(a.path(), "a")
        .unwrap()
        .mesh_apply(&r, 21)
        .unwrap();
    Store::open(b.path(), "b")
        .unwrap()
        .mesh_apply(&r, 21)
        .unwrap();
    for index in 0..33 {
        let call = mesh::MeshAction {
            version: 1,
            link_id: r.link_id.clone(),
            source_machine_id: "a".into(),
            target_machine_id: "b".into(),
            source_device_public_key: [8; 32],
            request: ActionRequest {
                version: 1,
                room: format!("mesh:{}", r.link_id),
                invocation_id: format!("queued-{index}"),
                operation: NativeOperation::ModelComplete {
                    text: "bounded".into(),
                },
            },
        };
        let result = mesh::invoke(
            b.path(),
            "b",
            &r.source.host_certificate,
            &call,
            22,
            |_, _| async {
                assert!(index < 32, "saturated work must not dispatch");
                gateway::Outcome::Pending
            },
        )
        .await;
        if index < 32 {
            assert_eq!(result.unwrap().status, "pending");
        } else {
            assert!(result.unwrap_err().to_string().contains("saturated"));
        }
    }
    assert_eq!(
        Store::open(b.path(), "b")
            .unwrap()
            .state
            .pending_models
            .len(),
        32
    );
    let mut unlink = r.clone();
    unlink.kind = "unlink".into();
    sign_request(
        &mut unlink,
        &SigningKey::from_bytes(&[7; 32]),
        &SigningKey::from_bytes(&[9; 32]),
    )
    .unwrap();
    let mut store = Store::open(b.path(), "b").unwrap();
    store.mesh_apply(&unlink, 23).unwrap();
    assert!(store.state.pending_models.is_empty());
    assert!(store.state.mesh.pending_scopes.is_empty());
}

#[test]
fn durability_mesh_exchange_does_not_report_uncertain_admission_as_rejected() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (a, b, mut grant) = linked();
    let (ar, br) = runtime.block_on(async {
        let ar = action_server::start(a.path(), "a", iroh::RelayMode::Disabled)
            .await
            .unwrap();
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(Store::open(b.path(), "b").unwrap().action_key())
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let br = iroh::protocol::Router::builder(endpoint)
            .accept(
                mesh::MESH_ALPN,
                mesh::Handler {
                    root: b.path().into(),
                    machine: "b".into(),
                    slots: tokio::sync::Semaphore::new(8),
                    gateway: gateway::Gateway::test_model("must not dispatch: "),
                },
            )
            .spawn();
        (ar, br)
    });
    let address = |router: &iroh::protocol::Router| {
        let port = router
            .endpoint()
            .bound_sockets()
            .iter()
            .find(|s| s.is_ipv4())
            .unwrap()
            .port();
        serde_json::to_string(
            &iroh_base::EndpointAddr::new(router.endpoint().id())
                .with_ip_addr(std::net::SocketAddr::from(([127, 0, 0, 1], port))),
        )
        .unwrap()
    };
    let now = qos_relay::clock::now_ns();
    {
        let mut sa = Store::open(a.path(), "a").unwrap();
        let mut sb = Store::open(b.path(), "b").unwrap();
        grant.source = sa.mesh_inspect(now).unwrap();
        grant.target = sb.mesh_inspect(now).unwrap();
        grant.source.action_address = address(&ar);
        grant.target.action_address = address(&br);
        sa.state.mesh.challenge = Some(grant.source.clone());
        sa.save(&sa.state).unwrap();
        sb.state.mesh.challenge = Some(grant.target.clone());
        sb.save(&sb.state).unwrap();
        sign_request(
            &mut grant,
            &SigningKey::from_bytes(&[7; 32]),
            &SigningKey::from_bytes(&[9; 32]),
        )
        .unwrap();
        sa.mesh_apply(&grant, now).unwrap();
        sb.mesh_apply(&grant, now).unwrap();
    }
    let request = ActionRequest {
        version: 1,
        room: grant.source.room.clone(),
        invocation_id: "uncertain-mesh".into(),
        operation: NativeOperation::Remote {
            link_id: grant.link_id.clone(),
            target_machine_id: "b".into(),
            operation: Box::new(NativeOperation::ModelComplete {
                text: "must not dispatch".into(),
            }),
        },
    };
    let result = atomic_file::test_support::with_failure(
        atomic_file::test_support::Checkpoint::BeforeCommit,
        || {
            runtime.block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    mesh::forward(
                        a.path(),
                        "a",
                        ar.endpoint(),
                        SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes(),
                        &request,
                    ),
                )
                .await
                .unwrap()
            })
        },
    );
    runtime.block_on(async {
        ar.shutdown().await.unwrap();
        br.shutdown().await.unwrap();
    });
    assert!(
        result.is_err(),
        "uncertain mesh admission became a receipt: {result:?}"
    );
    let recovered = Store::open(b.path(), "b").unwrap();
    assert_eq!(recovered.state.pending_models.len(), 1);
    assert!(recovered.state.receipts.is_empty());
}
