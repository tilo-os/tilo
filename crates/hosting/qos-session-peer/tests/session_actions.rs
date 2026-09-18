use ed25519_dalek::SigningKey;
use qos_peer_state::enrollment::{self, ChallengeRequest, ChallengeResponse};
use qos_session_peer::SessionOwner;
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
struct Host(std::process::Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
async fn request(
    origin: &str,
    route: &str,
    body: Option<String>,
    capability: Option<&str>,
) -> (u16, serde_json::Value) {
    let address = origin.strip_prefix("http://").unwrap();
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let wire = match body {
        None => format!("GET {route} HTTP/1.1\r\nHost: {address}\r\n\r\n"),
        Some(body) => format!(
            "POST {route} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://127.0.0.1:9998\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            capability.unwrap_or("unauthorized"),
            body.len()
        ),
    };
    socket.write_all(wire.as_bytes()).await.unwrap();
    let mut bytes = vec![];
    tokio::time::timeout(Duration::from_secs(15), socket.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let response = String::from_utf8(bytes).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    (
        head.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}
async fn start(state: &Path, ready: &Path) -> (Host, serde_json::Value, String) {
    let child = Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
        .arg("run")
        .arg(state)
        .arg(ready)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let pid = child.id();
    let host = Host(child);
    let deadline = Instant::now() + Duration::from_secs(30);
    let metadata = loop {
        assert!(
            Instant::now() < deadline,
            "isolated native host did not become ready"
        );
        if let Ok(bytes) = fs::read(ready) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if value["pid"].as_u64() == Some(u64::from(pid)) {
                    break value;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let origin = metadata["fixture_origin"].as_str().unwrap();
    loop {
        assert!(Instant::now() < deadline, "native peer never subscribed");
        let (code, status) = request(origin, "/status", None, None).await;
        if code == 200 && status["native_peer"] == "subscribed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let capability = fs::read_to_string(state.join("enrollment-capability")).unwrap();
    assert_eq!(metadata["version"], 2);
    assert_eq!(metadata["enrollment_version"], 2);
    assert!(!metadata.to_string().contains(&capability));
    (host, metadata, capability)
}
async fn proof(origin: &str, cap: &str, key: &SigningKey, id: u8) -> enrollment::CompleteRequest {
    let request_body = serde_json::to_string(&ChallengeRequest {
        device_public_key: hex::encode(key.verifying_key().to_bytes()),
        request_id: hex::encode([id; 32]),
    })
    .unwrap();
    let (code, response) = request(
        origin,
        "/enrollment/challenge",
        Some(request_body),
        Some(cap),
    )
    .await;
    assert_eq!(code, 200, "{response}");
    let challenge: ChallengeResponse = serde_json::from_value(response).unwrap();
    enrollment::sign(&enrollment::decode(&challenge.challenge).unwrap(), key).unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn normal_host_serves_real_checked_application_catalogue() {
    use qos_relay::{
        RelayClient,
        peer_actions::{ActionMessage, DirectedAction},
        transport::Envelope,
        transport_v2::EnvelopeV2,
    };
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("owner");
    let ready = temp.path().join("ready.json");
    let mut owner = SessionOwner::initialize(&state).unwrap();
    // Persist a floor far ahead of any SDK wall-clock allocator, then reopen
    // through the ordinary executable before its actual checked workers sign.
    let key = qos_user_identity::persist::read_device_key(
        &qos_user_identity::persist::IdentityPaths::under(state.join("identity")).device_privkey,
    )
    .unwrap();
    let mut candidate = owner.peer().clone();
    let app = "dev.qos.word-processor";
    let record = "tab:0000000000000001";
    let mut envelope = collab_types::SignedOp::new_unsigned(
        collab_types::DocumentMutation::Snapshot(candidate.document(app, record).unwrap())
            .encode()
            .unwrap(),
        collab_types::document_artifact_id(app, candidate.room(), record),
        collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
        collab_types::document_attestation_id(app, candidate.room(), record),
        collab_types::OpClass::Splice,
        web_time::SystemTime::now(),
        9_000_000_000_000_000_000,
    );
    collab_types::sign_op(&mut envelope, &key).unwrap();
    candidate
        .admit_local_app_document(app, record, envelope, web_time::SystemTime::now())
        .unwrap();
    owner
        .commit_admission(candidate, owner.directory().to_vec())
        .unwrap();
    let mut remote = owner.peer().clone();
    drop(owner);
    let (mut host, metadata, cap) = start(&state, &ready).await;
    let origin = metadata["fixture_origin"].as_str().unwrap();
    let key = SigningKey::from_bytes(&[92; 32]);
    let complete_request = proof(origin, &cap, &key, 92).await;
    let (code, bootstrap) = request(
        origin,
        "/enrollment/complete",
        Some(serde_json::to_string(&complete_request).unwrap()),
        Some(&cap),
    )
    .await;
    assert_eq!(code, 200, "{bootstrap}");
    let cert: qos_user_identity::DeviceCert =
        serde_json::from_value(bootstrap["certificate"].clone()).unwrap();
    let host_cert: qos_user_identity::DeviceCert =
        serde_json::from_value(bootstrap["host_certificate"].clone()).unwrap();
    let native: qos_user_identity::DeviceCert =
        serde_json::from_value(bootstrap["bootstrap"]["directory_author"].clone()).unwrap();
    let packet: iroh_base::RelayUrl = metadata["packet_relay"].as_str().unwrap().parse().unwrap();
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::from(packet)))
        .bind()
        .await
        .unwrap();
    endpoint.online().await;
    let client = RelayClient::connect_v2_to_enrolled_home_peer(
        endpoint,
        serde_json::from_value(metadata["relay_address"].clone()).unwrap(),
        cert.user_public_key,
        host_cert.device_public_key,
        cert.clone(),
        key,
        qos_user_identity::RevocationList::new(),
    )
    .await
    .unwrap();
    let room: uuid::Uuid = metadata["room"].as_str().unwrap().parse().unwrap();
    client
        .send(Envelope::Subscribe {
            workspace_id: qos_relay::room_workspace_id(&room.to_string()).unwrap(),
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = client.recv().await.unwrap();
            if matches!(event, Envelope::Ready { .. }) {
                break;
            }
            apply_state(&mut remote, event);
        }
    })
    .await
    .unwrap();
    client
        .send_action(EnvelopeV2::EnableActions { room })
        .unwrap();
    client
        .send_action(EnvelopeV2::EnableBrokerClient { room })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let (broker_host, catalogue) = loop {
        let request_id = uuid::Uuid::new_v4();
        client
            .send_action(EnvelopeV2::Directed(DirectedAction {
                room,
                request_id,
                source: cert.device_public_key,
                target: native.device_public_key,
                message: ActionMessage::CatalogueRefresh,
            }))
            .unwrap();
        let response = tokio::time::timeout(Duration::from_millis(500), async {
            loop {
                match client.recv_action().await.unwrap() {
                    EnvelopeV2::Delivered {
                        action,
                        source_user,
                    } if action.request_id == request_id => {
                        assert_eq!(source_user, native.user_public_key);
                        assert_eq!(action.room, room);
                        assert_eq!(action.source, native.device_public_key);
                        assert_eq!(action.target, cert.device_public_key);
                        break Some(action.message);
                    }
                    EnvelopeV2::Unavailable { request_id: id, .. } if id == request_id => {
                        break None;
                    }
                    _ => {}
                }
            }
        })
        .await
        .ok()
        .flatten();
        if let Some(ActionMessage::BrokerCatalogue {
            host: broker_host,
            catalogue,
        }) = response
        {
            if catalogue["surfaces"]
                .as_array()
                .is_some_and(|s| s.iter().any(|s| s["app_id"] == "dev.qos.word-processor"))
            {
                assert!(!broker_host.is_nil());
                break (broker_host, catalogue);
            }
        }
        if Instant::now() >= deadline {
            panic!(
                "normal executable never served its actual checked Writer action catalogue: {:?}",
                request(origin, "/status", None, None).await
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    for app in ["dev.qos.word-processor", "dev.qos.chat", "dev.qos.notes"] {
        assert!(
            catalogue["surfaces"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["app_id"] == app),
            "missing actual worker {app}: {catalogue}"
        );
    }
    let surface = catalogue["surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["app_id"] == "dev.qos.word-processor")
        .unwrap();
    let action = surface["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["action"] == "insert-table")
        .unwrap();
    let invocation = qos_actions::InvocationId::new();
    let operation = ActionMessage::InvokeBound {
        host: broker_host,
        invocation,
        app_peer: surface["peer_id"].as_u64().unwrap().to_string(),
        app_id: surface["app_id"].as_str().unwrap().into(),
        record_id: surface["record_id"].as_str().unwrap().into(),
        resource: action["resource"].as_str().unwrap().into(),
        action: "insert-table".into(),
        argument: None,
        confirmation: None,
        revision: None,
    };
    assert_eq!(table_cells(&remote), 0);
    #[cfg(feature = "test-support")]
    active_replacement(
        &mut host,
        origin,
        &client,
        &mut remote,
        room,
        &cert,
        &native,
        broker_host,
        invocation,
        operation.clone(),
    )
    .await;
    invoke_and_observe(
        &client,
        &mut remote,
        room,
        &cert,
        &native,
        broker_host,
        invocation,
        operation.clone(),
    )
    .await;
    assert_eq!(
        table_cells(&remote),
        4,
        "actual checked Writer insert-table must create exactly 2x2 cells"
    );
    owner_and_room_isolation(
        origin,
        &cap,
        &metadata,
        &client,
        room,
        &cert,
        &native,
        &host_cert,
        broker_host,
        invocation,
    )
    .await;
    let heads = remote
        .rows()
        .into_iter()
        .find(|r| r.app_id == "dev.qos.word-processor")
        .unwrap()
        .heads;
    invoke_and_observe(
        &client,
        &mut remote,
        room,
        &cert,
        &native,
        broker_host,
        invocation,
        operation.clone(),
    )
    .await;
    assert_eq!(
        table_cells(&remote),
        4,
        "retry executed the actual effect twice"
    );
    use std::io::Write;
    let generation =
        request(origin, "/status", None, None).await.1["native_subscription_generation"]
            .as_u64()
            .unwrap();
    writeln!(host.0.stdin.as_mut().unwrap(), "reconnect").unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (_, status) = request(origin, "/status", None, None).await;
        if status["actions"] == "ready"
            && status["native_peer"] == "subscribed"
            && status["native_subscription_generation"].as_u64().unwrap() > generation
        {
            break;
        }
        assert!(Instant::now() < deadline, "reconnect failed: {status}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    invoke_and_observe(
        &client,
        &mut remote,
        room,
        &cert,
        &native,
        broker_host,
        invocation,
        operation.clone(),
    )
    .await;
    assert_eq!(table_cells(&remote), 4);
    client.close().await;
    writeln!(host.0.stdin.as_mut().unwrap(), "shutdown").unwrap();
    let exit = tokio::task::spawn_blocking(move || host.0.wait().unwrap())
        .await
        .unwrap();
    assert!(exit.success(), "normal graceful worker shutdown failed");
    let reopened = SessionOwner::open(&state).unwrap();
    assert_eq!(table_cells(reopened.peer()), 4);
    assert_eq!(
        reopened
            .peer()
            .rows()
            .into_iter()
            .find(|r| r.app_id == "dev.qos.word-processor")
            .unwrap()
            .heads,
        heads
    );
    drop(reopened);
    let (mut restarted, metadata2, cap2) = start(&state, &ready).await;
    assert_eq!(cap2, cap);
    assert_eq!(metadata2["room"], metadata["room"]);
    let origin2 = metadata2["fixture_origin"].as_str().unwrap();
    let key = SigningKey::from_bytes(&[92; 32]);
    let complete_request = proof(origin2, &cap2, &key, 93).await;
    let (code, bootstrap2) = request(
        origin2,
        "/enrollment/complete",
        Some(serde_json::to_string(&complete_request).unwrap()),
        Some(&cap2),
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(bootstrap2["certificate"], bootstrap["certificate"]);
    let packet: iroh_base::RelayUrl = metadata2["packet_relay"].as_str().unwrap().parse().unwrap();
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::from(packet)))
        .bind()
        .await
        .unwrap();
    endpoint.online().await;
    let client = RelayClient::connect_v2_to_enrolled_home_peer(
        endpoint,
        serde_json::from_value(metadata2["relay_address"].clone()).unwrap(),
        cert.user_public_key,
        host_cert.device_public_key,
        cert.clone(),
        key,
        qos_user_identity::RevocationList::new(),
    )
    .await
    .unwrap();
    client
        .send(Envelope::Subscribe {
            workspace_id: qos_relay::room_workspace_id(&room.to_string()).unwrap(),
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = client.recv().await.unwrap();
            if matches!(event, Envelope::Ready { .. }) {
                break;
            }
            apply_state(&mut remote, event);
        }
    })
    .await
    .unwrap();
    client
        .send_action(EnvelopeV2::EnableActions { room })
        .unwrap();
    client
        .send_action(EnvelopeV2::EnableBrokerClient { room })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, status) = request(origin2, "/status", None, None).await;
        if status["actions"] == "ready" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "restart actions never ready: {status}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    invoke_and_observe(
        &client,
        &mut remote,
        room,
        &cert,
        &native,
        broker_host,
        invocation,
        ActionMessage::Status { invocation },
    )
    .await;
    invoke_and_observe(
        &client,
        &mut remote,
        room,
        &cert,
        &native,
        broker_host,
        invocation,
        operation.clone(),
    )
    .await;
    assert_eq!(
        table_cells(&remote),
        4,
        "restarting ordinary executable repeated the effect"
    );
    let refresh = uuid::Uuid::new_v4();
    client
        .send_action(EnvelopeV2::Directed(DirectedAction {
            room,
            request_id: refresh,
            source: cert.device_public_key,
            target: native.device_public_key,
            message: ActionMessage::CatalogueRefresh,
        }))
        .unwrap();
    let current_catalogue = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let EnvelopeV2::Delivered {
                action,
                source_user,
            } = client.recv_action().await.unwrap()
            {
                if action.request_id == refresh {
                    assert_eq!(source_user, native.user_public_key);
                    assert_eq!(action.source, native.device_public_key);
                    assert_eq!(action.target, cert.device_public_key);
                    assert_eq!(action.room, room);
                    if let ActionMessage::BrokerCatalogue { host, catalogue } = action.message {
                        assert_eq!(host, broker_host);
                        break catalogue;
                    } else {
                        panic!("catalogue response missing");
                    }
                }
            }
        }
    })
    .await
    .unwrap();
    let surface = current_catalogue["surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["app_id"] == "dev.qos.word-processor")
        .unwrap();
    let action = surface["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["action"] == "insert-table")
        .unwrap();
    // Fail the real aggregate replacement barrier after broker admission. This
    // must retire capability and cannot report a successful/non-effect result.
    let saved = state.join("session.saved");
    fs::rename(state.join("session.bin"), &saved).unwrap();
    fs::create_dir(state.join("session.bin")).unwrap();
    let failed_invocation = qos_actions::InvocationId::new();
    let faulted = ActionMessage::InvokeBound {
        host: broker_host,
        invocation: failed_invocation,
        app_peer: surface["peer_id"].as_u64().unwrap().to_string(),
        app_id: surface["app_id"].as_str().unwrap().into(),
        record_id: surface["record_id"].as_str().unwrap().into(),
        resource: action["resource"].as_str().unwrap().into(),
        action: "insert-table".into(),
        argument: None,
        confirmation: None,
        revision: None,
    };
    let request_id = uuid::Uuid::new_v4();
    client
        .send_action(EnvelopeV2::Directed(DirectedAction {
            room,
            request_id,
            source: cert.device_public_key,
            target: native.device_public_key,
            message: faulted,
        }))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, status) = request(origin2, "/status", None, None).await;
        if status["actions"] == "unavailable" && status["native_peer"] == "disconnected" {
            assert!(status["action_error"].is_string(), "{status}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "actual owner barrier failure did not withdraw actions: {status}"
        );
        tokio::select! {
            event = client.recv_action() => if let Some(EnvelopeV2::Delivered { action, .. }) = event { if action.request_id == request_id { assert!(!matches!(action.message, ActionMessage::Rejected { .. }), "fault invocation rejected before reaching actual owner: {:?}", action.message); } },
            _ = tokio::time::sleep(Duration::from_millis(20)) => {},
        }
    }
    // Local connection closure is not an acknowledgement that the remote
    // relay actor finished unregistering. A probe queued in that interval can
    // be lost with the closing target. Bound the read-only probes and require
    // an explicit post-unregistration relay refusal, never a fake success.
    let mut probes = std::collections::BTreeSet::new();
    let mut probe_tick = tokio::time::interval(Duration::from_millis(100));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                _ = probe_tick.tick() => {
                    let check = uuid::Uuid::new_v4(); probes.insert(check);
                    client.send_action(EnvelopeV2::Directed(DirectedAction {
                        room, request_id: check, source: cert.device_public_key,
                        target: native.device_public_key, message: ActionMessage::CatalogueRefresh,
                    })).unwrap();
                },
                event = client.recv_action() => { match event.unwrap() {
                    EnvelopeV2::Unavailable { request_id, .. } if probes.contains(&request_id) => break,
                    EnvelopeV2::Delivered { action, .. } if action.request_id == request_id => {
                        assert!(!matches!(action.message, ActionMessage::Observation {
                            update: Some(qos_actions::InvocationUpdate::Succeeded { .. }), ..
                        }), "failed durable commit produced successful effect terminal");
                    },
                    EnvelopeV2::Delivered { action, .. } if probes.contains(&action.request_id) => {
                        panic!("failed broker retained capability: {:?}", action.message)
                    },
                    _ => {},
                } },
                event = client.recv() => { if let Some(event) = event { apply_state(&mut remote, event); } else { panic!("retirement client state disconnected"); } },
            }
        }
    }).await.expect("relay did not explicitly retire the failed broker within the bounded probe window");
    client.close().await;
    restarted.0.kill().unwrap();
    restarted.0.wait().unwrap();
    fs::remove_dir(state.join("session.bin")).unwrap();
    fs::rename(saved, state.join("session.bin")).unwrap();
    let recovered = SessionOwner::open(&state).unwrap();
    assert_eq!(
        table_cells(recovered.peer()),
        4,
        "failed commit replaced prior durable Writer state"
    );
}
fn apply_state(peer: &mut qos_peer_state::StatePeer, event: qos_relay::transport::Envelope) {
    use qos_relay::{coedit::CoeditPayload, transport::Envelope};
    if let Envelope::Delta {
        author_device_public_key,
        delta,
        ..
    } = event
    {
        if let Some(
            CoeditPayload::Checkpoint { body, .. } | CoeditPayload::RetainedDelta { body, .. },
        ) = CoeditPayload::decode(&delta)
        {
            let message = postcard::from_bytes(&body).unwrap();
            peer.receive_from(
                qos_peer_state::TransportOrigin::RelayDelta(author_device_public_key),
                message,
                web_time::SystemTime::now(),
            )
            .unwrap();
        }
    }
}
fn table_cells(peer: &qos_peer_state::StatePeer) -> usize {
    let doc = ui::automerge::AutoCommit::load(
        &peer
            .document("dev.qos.word-processor", "tab:0000000000000001")
            .unwrap(),
    )
    .unwrap();
    let containers = qdoc::model::open(&doc, &ui::automerge::ROOT).unwrap();
    qdoc::model::materialize(&doc, &containers)
        .blocks
        .iter()
        .filter(|b| b.kind == "cell")
        .count()
}
async fn invoke_and_observe(
    client: &qos_relay::RelayClient,
    remote: &mut qos_peer_state::StatePeer,
    room: uuid::Uuid,
    cert: &qos_user_identity::DeviceCert,
    native: &qos_user_identity::DeviceCert,
    host: uuid::Uuid,
    invocation: qos_actions::InvocationId,
    operation: qos_relay::peer_actions::ActionMessage,
) {
    use qos_relay::{
        peer_actions::{ActionMessage, DirectedAction},
        transport_v2::EnvelopeV2,
    };
    let request_id = uuid::Uuid::new_v4();
    client
        .send_action(EnvelopeV2::Directed(DirectedAction {
            room,
            request_id,
            source: cert.device_public_key,
            target: native.device_public_key,
            message: operation,
        }))
        .unwrap();
    let mut success = false;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if success && table_cells(remote) == 4 { break; }
            tokio::select! {
                state = client.recv() => apply_state(remote, state.unwrap()),
                event = client.recv_action() => match event.unwrap() {
                    EnvelopeV2::Delivered { action, source_user } if action.request_id == request_id => {
                        assert_eq!(source_user, native.user_public_key); assert_eq!(action.room, room); assert_eq!(action.source, native.device_public_key); assert_eq!(action.target, cert.device_public_key);
                        match action.message {
                            ActionMessage::Observation { host: got, invocation: id, update: Some(update), uncertain, .. } => {
                                assert_eq!(got, host); assert_eq!(id, invocation); assert!(!uncertain);
                                if update.is_terminal() { assert!(matches!(update, qos_actions::InvocationUpdate::Succeeded { .. }), "actual operation failed: {update:?}"); success = true; }
                            },
                            other => panic!("actual operation response: {other:?}"),
                        }
                    },
                    EnvelopeV2::Unavailable { request_id: id, reason, .. } if id == request_id => panic!("operation unavailable: {reason:?}"),
                    _ => {},
                }
            }
        }
    }).await.expect("actual Writer operation never committed, replicated and completed");
}

#[cfg(feature = "test-support")]
async fn active_replacement(
    host: &mut Host,
    origin: &str,
    client: &qos_relay::RelayClient,
    remote: &mut qos_peer_state::StatePeer,
    room: uuid::Uuid,
    cert: &qos_user_identity::DeviceCert,
    native: &qos_user_identity::DeviceCert,
    broker_host: uuid::Uuid,
    invocation: qos_actions::InvocationId,
    operation: qos_relay::peer_actions::ActionMessage,
) {
    use qos_relay::{
        peer_actions::{ActionMessage, DirectedAction},
        transport::Envelope,
        transport_v2::EnvelopeV2,
    };
    use std::io::Write;
    use ui::automerge::transaction::Transactable;
    writeln!(
        host.0.stdin.as_mut().unwrap(),
        "qualification hold dev.qos.word-processor tab:0000000000000001"
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let status = request(origin, "/status", None, None).await.1;
        if status["qualification"]["armed"] == true {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "qualification hold was not armed: {status}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let request_id = uuid::Uuid::new_v4();
    client
        .send_action(EnvelopeV2::Directed(DirectedAction {
            room,
            request_id,
            source: cert.device_public_key,
            target: native.device_public_key,
            message: operation,
        }))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut accepted = false;
    let generation = loop {
        let status = request(origin, "/status", None, None).await.1;
        if status["qualification"]["held"] == true && accepted {
            break status["native_subscription_generation"].as_u64().unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "actual action never held after admission: {status}; accepted={accepted}"
        );
        tokio::select! {
            state = client.recv() => apply_state(remote, state.unwrap()),
            event = client.recv_action() => if let EnvelopeV2::Delivered { action, source_user } = event.unwrap() {
                if action.request_id == request_id {
                    assert_eq!(source_user, native.user_public_key); assert_eq!(action.source, native.device_public_key); assert_eq!(action.target, cert.device_public_key); assert_eq!(action.room, room);
                    if let ActionMessage::Observation { host, invocation: id, update: Some(update), uncertain, .. } = action.message {
                        assert_eq!(host, broker_host); assert_eq!(id, invocation); assert!(!uncertain); assert!(!update.is_terminal(), "held action completed prematurely: {update:?}"); accepted = true;
                    } else { panic!("held action was not admitted"); }
                }
            },
            _ = tokio::time::sleep(Duration::from_millis(5)) => {},
        }
    };
    // Publish a real signed incremental Notes mutation on this same session
    // while the actual Writer operation is waiting for its committed response.
    let row = remote
        .rows()
        .into_iter()
        .find(|row| row.app_id == "dev.qos.notes")
        .unwrap();
    let mut doc =
        ui::automerge::AutoCommit::load(&remote.document(&row.app_id, &row.record_id).unwrap())
            .unwrap();
    doc.set_actor(ui::automerge::ActorId::from(
        cert.device_public_key.to_vec(),
    ));
    let _ = doc.save_incremental();
    doc.put(
        ui::automerge::ROOT,
        "active-action-state-proof",
        "received during Writer action",
    )
    .unwrap();
    doc.commit();
    let head = doc.get_heads()[0].to_string();
    let signed = remote
        .signed_mutation(
            &row.app_id,
            &row.record_id,
            collab_types::DocumentMutation::Incremental(doc.save_incremental()),
            &SigningKey::from_bytes(&[92; 32]),
            web_time::SystemTime::now(),
        )
        .unwrap();
    client
        .send(Envelope::Delta {
            workspace_id: qos_relay::room_workspace_id(&room.to_string()).unwrap(),
            author_device_public_key: cert.device_public_key,
            authored_at_ns: qos_relay::clock::now_ns(),
            delta: qos_relay::coedit::CoeditPayload::RetainedDelta {
                channel: *blake3::hash(
                    format!("{room}/{}/{}", row.app_id, row.record_id).as_bytes(),
                )
                .as_bytes(),
                body: postcard::to_stdvec(&signed).unwrap(),
            }
            .encode()
            .unwrap(),
            signature: [0; 64],
        })
        .unwrap();
    // Endpoint::online takes about 3s on this isolated relay. Keep the
    // observation below the 4s hold and unchanged 5s signer deadline.
    let deadline = Instant::now() + Duration::from_millis(3800);
    loop {
        let status = request(origin, "/status", None, None).await.1;
        assert_eq!(
            status["qualification"]["held"], true,
            "response was not held while delivering state: {status}"
        );
        assert_eq!(status["qualification"]["expired"], false);
        if status["qualification"]["workers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|worker| {
                worker["app"] == row.app_id
                    && worker["record"] == row.record_id
                    && worker["heads"]
                        .as_array()
                        .unwrap()
                        .contains(&serde_json::json!(head))
            })
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "incoming state did not reach checked Notes worker: {status}"
        );
        tokio::select! {
            state = client.recv() => apply_state(remote, state.unwrap()),
            event = client.recv_action() => if let EnvelopeV2::Delivered { action, source_user } = event.unwrap() {
                if action.request_id == request_id {
                    assert_eq!(source_user, native.user_public_key); assert_eq!(action.source, native.device_public_key); assert_eq!(action.target, cert.device_public_key); assert_eq!(action.room, room);
                    assert!(matches!(action.message, ActionMessage::Observation { update: Some(ref update), uncertain: false, .. } if !update.is_terminal()), "held old-generation invocation completed or lost custody: {:?}", action.message);
                }
            },
            _ = tokio::time::sleep(Duration::from_millis(5)) => {},
        }
    }
    writeln!(host.0.stdin.as_mut().unwrap(), "reconnect").unwrap();
    loop {
        let status = request(origin, "/status", None, None).await.1;
        assert_eq!(
            status["qualification"]["held"], true,
            "old completion escaped replacement boundary: {status}"
        );
        assert_eq!(status["qualification"]["expired"], false);
        if status["actions"] == "ready"
            && status["native_subscription_generation"].as_u64().unwrap() > generation
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "active replacement did not become ready: {status}"
        );
        tokio::select! {
            state = client.recv() => apply_state(remote, state.unwrap()),
            event = client.recv_action() => if let EnvelopeV2::Delivered { action, source_user } = event.unwrap() {
                if action.request_id == request_id {
                    assert_eq!(source_user, native.user_public_key); assert_eq!(action.source, native.device_public_key); assert_eq!(action.target, cert.device_public_key); assert_eq!(action.room, room);
                    assert!(matches!(action.message, ActionMessage::Observation { update: Some(ref update), uncertain: false, .. } if !update.is_terminal()), "held old-generation invocation completed or lost custody: {:?}", action.message);
                }
            },
            _ = tokio::time::sleep(Duration::from_millis(5)) => {},
        }
    }
    writeln!(host.0.stdin.as_mut().unwrap(), "qualification release").unwrap();
    // The caller now resumes the full original identity on the replacement.
    // Any stale old-session completion must neither detach it nor rerun effect.
}

async fn owner_and_room_isolation(
    origin: &str,
    capability: &str,
    metadata: &serde_json::Value,
    first: &qos_relay::RelayClient,
    room: uuid::Uuid,
    first_cert: &qos_user_identity::DeviceCert,
    native: &qos_user_identity::DeviceCert,
    host_cert: &qos_user_identity::DeviceCert,
    broker_host: uuid::Uuid,
    invocation: qos_actions::InvocationId,
) {
    use qos_relay::{
        RelayClient,
        peer_actions::{ActionMessage, DirectedAction, UnavailableReason},
        transport::Envelope,
        transport_v2::EnvelopeV2,
    };
    let key = SigningKey::from_bytes(&[94; 32]);
    let complete = proof(origin, capability, &key, 94).await;
    let (code, bootstrap) = request(
        origin,
        "/enrollment/complete",
        Some(serde_json::to_string(&complete).unwrap()),
        Some(capability),
    )
    .await;
    assert_eq!(code, 200);
    let cert: qos_user_identity::DeviceCert =
        serde_json::from_value(bootstrap["certificate"].clone()).unwrap();
    // Same human principal, distinct Certified{root,device} invocation owner.
    assert_eq!(cert.user_public_key, first_cert.user_public_key);
    assert_ne!(cert.device_public_key, first_cert.device_public_key);
    let packet: iroh_base::RelayUrl = metadata["packet_relay"].as_str().unwrap().parse().unwrap();
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::from(packet)))
        .bind()
        .await
        .unwrap();
    endpoint.online().await;
    let second = RelayClient::connect_v2_to_enrolled_home_peer(
        endpoint,
        serde_json::from_value(metadata["relay_address"].clone()).unwrap(),
        cert.user_public_key,
        host_cert.device_public_key,
        cert.clone(),
        key,
        qos_user_identity::RevocationList::new(),
    )
    .await
    .unwrap();
    second
        .send(Envelope::Subscribe {
            workspace_id: qos_relay::room_workspace_id(&room.to_string()).unwrap(),
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !matches!(second.recv().await.unwrap(), Envelope::Ready { .. }) {}
    })
    .await
    .unwrap();
    second
        .send_action(EnvelopeV2::EnableActions { room })
        .unwrap();
    second
        .send_action(EnvelopeV2::EnableBrokerClient { room })
        .unwrap();
    for foreign_or_absent in [invocation, qos_actions::InvocationId::new()] {
        let query = uuid::Uuid::new_v4();
        second
            .send_action(EnvelopeV2::Directed(DirectedAction {
                room,
                request_id: query,
                source: cert.device_public_key,
                target: native.device_public_key,
                message: ActionMessage::Status {
                    invocation: foreign_or_absent,
                },
            }))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let EnvelopeV2::Delivered {
                    action,
                    source_user,
                } = second.recv_action().await.unwrap()
                {
                    if action.request_id == query {
                        assert_eq!(source_user, native.user_public_key);
                        assert_eq!(action.source, native.device_public_key);
                        assert_eq!(action.target, cert.device_public_key);
                        assert_eq!(action.room, room);
                        // Unknown status reveals only the public host and caller's
                        // own requested ID, identically for absent and foreign IDs.
                        assert_eq!(
                            action.message,
                            ActionMessage::Observation {
                                host: broker_host,
                                invocation: foreign_or_absent,
                                update: None,
                                uncertain: false,
                                cancellation_requested: false,
                            },
                            "another device learned original invocation state"
                        );
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
    }
    let wrong_room = uuid::Uuid::new_v4();
    let query = uuid::Uuid::new_v4();
    first
        .send_action(EnvelopeV2::Directed(DirectedAction {
            room: wrong_room,
            request_id: query,
            source: first_cert.device_public_key,
            target: native.device_public_key,
            message: ActionMessage::Status { invocation },
        }))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match first.recv_action().await.unwrap() {
                EnvelopeV2::Unavailable {
                    room: got,
                    request_id,
                    reason,
                    ..
                } if request_id == query => {
                    assert_eq!(got, wrong_room);
                    assert_eq!(reason, UnavailableReason::NotAuthorised);
                    break;
                }
                EnvelopeV2::Delivered { action, .. } if action.request_id == query => {
                    panic!("wrong room disclosed private result: {:?}", action.message)
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    second.close().await;
}
