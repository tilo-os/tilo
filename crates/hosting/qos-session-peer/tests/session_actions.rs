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

#[cfg(feature = "test-support")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_status_waits_for_durable_action_registration() {
    use std::io::Write;

    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("owner");
    let ready = temp.path().join("ready.json");
    SessionOwner::initialize(&state).unwrap();
    let (mut host, metadata, _) = start(&state, &ready).await;
    let origin = metadata["fixture_origin"].as_str().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let generation = loop {
        let status = request(origin, "/status", None, None).await.1;
        if status["actions"] == "ready" {
            break status["native_subscription_generation"].as_u64().unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "initial actions never ready: {status}"
        );
        tokio::task::yield_now().await;
    };

    writeln!(
        host.0.stdin.as_mut().unwrap(),
        "qualification action-registration hold"
    )
    .unwrap();
    host.0.stdin.as_mut().unwrap().flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = request(origin, "/status", None, None).await.1;
        if status["qualification"]["action_registration"]["armed"] == true {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "registration hold was not armed: {status}"
        );
        tokio::task::yield_now().await;
    }
    writeln!(host.0.stdin.as_mut().unwrap(), "reconnect").unwrap();
    host.0.stdin.as_mut().unwrap().flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let held = loop {
        let status = request(origin, "/status", None, None).await.1;
        if status["native_subscription_generation"].as_u64().unwrap() > generation
            && status["qualification"]["action_registration"]["reached"] == true
        {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "replacement never reached registration hold: {status}"
        );
        tokio::task::yield_now().await;
    };
    assert_eq!(held["native_peer"], "subscribed", "{held}");
    assert_eq!(held["actions"], "starting", "{held}");

    writeln!(
        host.0.stdin.as_mut().unwrap(),
        "qualification action-registration release"
    )
    .unwrap();
    host.0.stdin.as_mut().unwrap().flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = request(origin, "/status", None, None).await.1;
        if status["actions"] == "ready" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "registered actions never became ready: {status}"
        );
        tokio::task::yield_now().await;
    }
    writeln!(host.0.stdin.as_mut().unwrap(), "shutdown").unwrap();
    let exit = tokio::task::spawn_blocking(move || host.0.wait().unwrap())
        .await
        .unwrap();
    assert!(exit.success());
}

#[cfg(feature = "test-support")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ended_private_attachment_reconnects_without_poisoning_actions() {
    use std::io::Write;

    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("owner");
    let ready = temp.path().join("ready.json");
    SessionOwner::initialize(&state).unwrap();
    let (mut host, metadata, _) = start(&state, &ready).await;
    let origin = metadata["fixture_origin"].as_str().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let generation = loop {
        let status = request(origin, "/status", None, None).await.1;
        if status["actions"] == "ready" {
            assert_eq!(status["qualification"]["action_attachment_exit_armed"], true, "{status}");
            break status["native_subscription_generation"].as_u64().unwrap();
        }
        assert!(Instant::now() < deadline, "initial actions never ready: {status}");
        tokio::task::yield_now().await;
    };
    writeln!(host.0.stdin.as_mut().unwrap(), "qualification action-attachment eof").unwrap();
    host.0.stdin.as_mut().unwrap().flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = request(origin, "/status", None, None).await.1;
        assert_ne!(status["actions"], "unavailable", "ended attachment poisoned actions: {status}");
        if status["actions"] == "ready"
            && status["native_subscription_generation"].as_u64().unwrap() > generation
        {
            assert!(status["action_error"].is_null(), "{status}");
            break;
        }
        assert!(Instant::now() < deadline, "actions did not recover after attachment ended: {status}");
        tokio::task::yield_now().await;
    }
    writeln!(host.0.stdin.as_mut().unwrap(), "shutdown").unwrap();
    let exit = tokio::task::spawn_blocking(move || host.0.wait().unwrap())
        .await
        .unwrap();
    assert!(exit.success());
}

#[cfg(feature = "test-support")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_private_attachment_withdraws_actions() {
    use std::io::Write;

    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("owner");
    let ready = temp.path().join("ready.json");
    SessionOwner::initialize(&state).unwrap();
    let (mut host, metadata, _) = start(&state, &ready).await;
    let origin = metadata["fixture_origin"].as_str().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = request(origin, "/status", None, None).await.1;
        if status["actions"] == "ready" {
            assert_eq!(status["qualification"]["action_attachment_exit_armed"], true, "{status}");
            break;
        }
        assert!(Instant::now() < deadline, "initial actions never ready: {status}");
        tokio::task::yield_now().await;
    }
    writeln!(host.0.stdin.as_mut().unwrap(), "qualification action-attachment failure").unwrap();
    host.0.stdin.as_mut().unwrap().flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = request(origin, "/status", None, None).await.1;
        if status["actions"] == "unavailable" {
            assert_eq!(status["native_peer"], "disconnected", "{status}");
            assert!(status["action_error"].as_str().is_some_and(|error| error.contains("outcome unknown")), "{status}");
            break;
        }
        assert!(Instant::now() < deadline, "attachment failure did not withdraw actions: {status}");
        tokio::task::yield_now().await;
    }
    writeln!(host.0.stdin.as_mut().unwrap(), "shutdown").unwrap();
    let exit = tokio::task::spawn_blocking(move || host.0.wait().unwrap())
        .await
        .unwrap();
    assert!(exit.success());
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
    #[cfg(feature = "test-support")]
    let observer_binding = ObserverBinding {
        endpoint: endpoint.clone(),
        relay: serde_json::from_value(metadata["relay_address"].clone()).unwrap(),
        root: cert.user_public_key,
        host: host_cert.device_public_key,
        cert: cert.clone(),
        key: key.clone(),
    };
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
    let client = active_replacement(
        &mut host,
        origin,
        client,
        &observer_binding,
        &mut remote,
        room,
        &cert,
        &native,
        broker_host,
        invocation,
        operation.clone(),
    )
    .await;
    #[cfg(feature = "test-support")]
    let first_observation = ActionMessage::Status { invocation };
    #[cfg(not(feature = "test-support"))]
    let first_observation = operation.clone();
    invoke_and_observe(
        &client,
        &mut remote,
        room,
        &cert,
        &native,
        broker_host,
        invocation,
        first_observation,
    )
    .await;
    assert_eq!(
        table_cells(&remote),
        4,
        "actual checked Writer insert-table must create exactly 2x2 cells"
    );
    #[cfg(feature = "test-support")]
    observe_native_publication(origin, &client, &mut remote).await;
    let (desktop_invocation, desktop_operation, desktop_records) = desktop_open_through_broker(&client, &mut remote, room, &cert, &native, broker_host, &catalogue).await;
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
    invoke_and_observe(&client, &mut remote, room, &cert, &native, broker_host,
        desktop_invocation, ActionMessage::Status { invocation: desktop_invocation }).await;
    invoke_and_observe(&client, &mut remote, room, &cert, &native, broker_host,
        desktop_invocation, desktop_operation).await;
    assert_eq!(remote.rows().len(), desktop_records, "restart duplicate desktop invocation cannot create a record");
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
    // The production browser correlator, on the actual certified relay/broker path.
    let mut pending = qos_browser_runtime::action_client::PendingActions::default();
    let mut request_id = uuid::Uuid::new_v4();
    let directed=DirectedAction {room,request_id,source:cert.device_public_key,target:native.device_public_key,message:operation};
    pending.insert(directed.clone()).unwrap();
    client.send_action(EnvelopeV2::Directed(directed)).unwrap();
    let mut success = false;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if success && table_cells(remote) == 4 { break; }
            tokio::select! {
                state = client.recv() => apply_state(remote, state.unwrap()),
                event = client.recv_action() => {
                    if !pending.receive(event.unwrap(),native.user_public_key){continue;}
                    match pending.take(request_id).unwrap() {
                        ActionMessage::Observation { host: got, invocation: id, update, uncertain, .. } => {
                            assert_eq!(got, host); assert_eq!(id, invocation); assert!(!uncertain);
                            if let Some(update)=update {if update.is_terminal(){assert!(matches!(update,qos_actions::InvocationUpdate::Succeeded{..}),"actual operation failed: {update:?}");success=true;}}
                            if !success {
                                // Same original full UUID: lost/pending response recovery is Status,
                                // never another Invoke or another confirmation grant.
                                request_id=uuid::Uuid::new_v4();
                                let status=DirectedAction{room,request_id,source:cert.device_public_key,target:native.device_public_key,message:ActionMessage::Status{invocation}};
                                pending.insert(status.clone()).unwrap();client.send_action(EnvelopeV2::Directed(status)).unwrap();
                            }
                        }
                        other=>panic!("actual browser action response: {other:?}"),
                    }
                }
            }
        }
    }).await.expect("actual Writer operation never committed, replicated and completed");
}

#[cfg(feature = "test-support")]
fn active_replacement_state_delta(signed: &qos_peer_state::NeighbourMsg) -> Vec<u8> {
    let (channel, kind) = qos_peer_state::transport::retention(signed, false)
        .expect("active replacement requires retained application state");
    assert_eq!(kind, qos_peer_state::transport::Retention::Delta);
    qos_relay::coedit::CoeditPayload::RetainedDelta {
        channel,
        body: postcard::to_stdvec(signed).unwrap(),
    }
    .encode()
    .unwrap()
}

#[cfg(feature = "test-support")]
#[test]
fn active_replacement_state_delta_is_a_provable_retained_tail() {
    use qos_peer_state::{
        NeighbourMsg,
        retained_proof::{RetentionAuthority, RoomInput, verify_room, verify_snapshot},
        transport::retention,
    };
    use qos_relay::coedit::CoeditPayload;
    use ui::automerge::{AutoCommit, ROOT, transaction::Transactable};

    let temp = tempfile::tempdir().unwrap();
    let owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let key = qos_user_identity::persist::read_device_key(
        &qos_user_identity::persist::IdentityPaths::under(owner.root().join("identity"))
            .device_privkey,
    )
    .unwrap();
    let source = key.verifying_key().to_bytes();
    let authority =
        RetentionAuthority::new(owner.peer().room(), owner.peer().authority().clone()).unwrap();
    let messages = owner
        .outgoing()
        .iter()
        .map(|bytes| postcard::from_bytes::<NeighbourMsg>(bytes).unwrap())
        .collect::<Vec<_>>();
    let room_message = messages
        .iter()
        .find(|msg| matches!(msg, NeighbourMsg::SyncResp { .. }))
        .unwrap();
    let room = verify_room(
        &authority,
        source,
        retention(room_message, true).unwrap().0,
        &postcard::to_stdvec(room_message).unwrap(),
        RoomInput::Checkpoint,
        None,
    )
    .unwrap();
    let snapshot_message = messages.iter().find(|msg| matches!(msg, NeighbourMsg::AppDocument { app_id, .. } if app_id == "dev.qos.notes")).unwrap();
    let snapshot = verify_snapshot(
        &room,
        source,
        retention(snapshot_message, true).unwrap().0,
        &postcard::to_stdvec(snapshot_message).unwrap(),
    )
    .unwrap();
    let route = snapshot.coverage();
    let mut doc =
        AutoCommit::load(&owner.peer().document(&route.app, &route.record).unwrap()).unwrap();
    doc.set_actor(ui::automerge::ActorId::from(source.to_vec()));
    let _ = doc.save_incremental();
    doc.put(ROOT, "active-action-state-proof", "contract increment")
        .unwrap();
    doc.commit();
    let signed = owner
        .peer()
        .clone()
        .signed_mutation(
            &route.app,
            &route.record,
            collab_types::DocumentMutation::Incremental(doc.save_incremental()),
            &key,
            web_time::SystemTime::now(),
        )
        .unwrap();
    let CoeditPayload::RetainedDelta { channel, body } =
        CoeditPayload::decode(&active_replacement_state_delta(&signed)).unwrap()
    else {
        panic!("fixture must retain the signed incremental body");
    };
    assert_eq!(body, postcard::to_stdvec(&signed).unwrap());
    assert!(
        snapshot.tail_ready_after_snapshot(source, channel, &body),
        "actual fixture frame must satisfy the production signed-route and replay-tail proof: {:?}",
        snapshot.classify_tail(source, channel, &body)
    );
}

// The observer owns its identity/state independently of a replaceable transport.
#[cfg(feature = "test-support")]
struct ObserverBinding {
    endpoint: iroh::Endpoint,
    relay: iroh_base::EndpointAddr,
    root: [u8; 32],
    host: [u8; 32],
    cert: qos_user_identity::DeviceCert,
    key: SigningKey,
}

#[cfg(feature = "test-support")]
enum ObserverEvent {
    State(qos_relay::transport::Envelope),
    Action(qos_relay::transport_v2::EnvelopeV2),
    Ended,
}

#[cfg(feature = "test-support")]
async fn observer_event(client: &qos_relay::RelayClient) -> ObserverEvent {
    tokio::select! {
        state = client.recv() => state.map(ObserverEvent::State).unwrap_or(ObserverEvent::Ended),
        action = client.recv_action() => action.map(ObserverEvent::Action).unwrap_or(ObserverEvent::Ended),
    }
}

#[cfg(feature = "test-support")]
async fn connect_observer(
    binding: &ObserverBinding,
    room: uuid::Uuid,
    remote: &mut qos_peer_state::StatePeer,
    deadline: Instant,
) -> Result<(qos_relay::RelayClient, usize), String> {
    use qos_relay::{transport::Envelope, transport_v2::EnvelopeV2};
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
        let client = qos_relay::RelayClient::connect_v2_to_enrolled_home_peer(
            binding.endpoint.clone(),
            binding.relay.clone(),
            binding.root,
            binding.host,
            binding.cert.clone(),
            binding.key.clone(),
            qos_user_identity::RevocationList::new(),
        )
        .await
        .map_err(|error| error.to_string())?;
        client
            .send(Envelope::Subscribe {
                workspace_id: qos_relay::room_workspace_id(&room.to_string()).unwrap(),
            })
            .map_err(|_| "observer Subscribe failed")?;
        let mut frames = 0;
        loop {
            match observer_event(&client).await {
                ObserverEvent::State(Envelope::Ready { .. }) => break,
                ObserverEvent::State(event) => {
                    if matches!(event, Envelope::Delta { .. }) {
                        frames += 1;
                    }
                    apply_state(remote, event);
                }
                ObserverEvent::Action(_) => {
                    return Err("action before observer replay Ready".into());
                }
                ObserverEvent::Ended => return Err("observer ended during recovery replay".into()),
            }
        }
        client
            .send_action(EnvelopeV2::EnableActions { room })
            .map_err(|_| "observer EnableActions failed")?;
        client
            .send_action(EnvelopeV2::EnableBrokerClient { room })
            .map_err(|_| "observer EnableBrokerClient failed")?;
        Ok((client, frames))
    })
    .await
    .map_err(|_| "observer recovery exceeded the original deadline".to_string())?
}

#[cfg(feature = "test-support")]
async fn replace_observer_after_eof(
    ended: qos_relay::RelayClient,
    binding: &ObserverBinding,
    room: uuid::Uuid,
    remote: &mut qos_peer_state::StatePeer,
    replacements: &mut u8,
    deadline: Instant,
) -> Result<(qos_relay::RelayClient, usize), String> {
    if *replacements != 0 {
        return Err("second observer EOF".into());
    }
    *replacements += 1;
    // close() would also close the shared endpoint. Retire only this transport.
    drop(ended);
    connect_observer(binding, room, remote, deadline).await
}

#[cfg(feature = "test-support")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observer_eof_replays_once_on_the_same_endpoint_and_peer() {
    use qos_session_peer::host::{LocalHost, publish};
    use qos_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, persist};
    use ui::automerge::{AutoCommit, ROOT, transaction::Transactable};

    // Capture a weak handle only: retaining a Connection would change drop semantics.
    #[derive(Debug, Clone, Default)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Option<iroh::endpoint::WeakConnectionHandle>>>);
    impl iroh::endpoint::EndpointHooks for Capture {
        async fn after_handshake(
            &self,
            connection: &iroh::endpoint::Connection,
        ) -> iroh::endpoint::AfterHandshakeOutcome {
            *self.0.lock().unwrap() = Some(connection.weak_handle());
            iroh::endpoint::AfterHandshakeOutcome::accept()
        }
    }
    impl Capture {
        fn close_connection(&self) {
            self.0
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .upgrade()
                .unwrap()
                .close(0u32.into(), b"fixture observer closure");
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    let paths = persist::IdentityPaths::under(owner.root().join("identity"));
    let native_key = persist::read_device_key(&paths.device_privkey).unwrap();
    let root_key = persist::read_signing_key(&paths.user_privkey).unwrap();
    let key = SigningKey::from_bytes(&[92; 32]);
    let mut cert = DeviceCert {
        user_public_key: root_key.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "observer recovery fixture".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: qos_relay::clock::now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
    };
    qos_user_identity::sign_device_cert(&mut cert, &root_key).unwrap();
    let mut candidate = owner.peer().clone();
    candidate
        .admit_certificate(cert.clone(), &native_key, web_time::SystemTime::now())
        .unwrap();
    owner
        .commit_admission(candidate, owner.directory().to_vec())
        .unwrap();
    let host = LocalHost::start(&owner).await.unwrap();
    let mut native = host.connect(&owner).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !native.is_ready() {
            if let qos_relay::Inbound::Delta { author, data } = native.recv().await.unwrap() {
                owner
                    .receive(
                        qos_peer_state::TransportOrigin::RelayDelta(author),
                        postcard::from_bytes(&data).unwrap(),
                    )
                    .unwrap();
            }
        }
    })
    .await
    .unwrap();
    let capture = Capture::default();
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::from(
            host.packet_url.clone(),
        )))
        .hooks(capture.clone())
        .bind()
        .await
        .unwrap();
    endpoint.online().await;
    let endpoint_id = endpoint.id();
    let room = owner.peer().room().parse().unwrap();
    let binding = ObserverBinding {
        endpoint: endpoint.clone(),
        relay: host.address.clone(),
        root: cert.user_public_key,
        host: owner.host_certificate().device_public_key,
        cert,
        key,
    };
    let mut remote = qos_peer_state::StatePeer::new(
        owner.peer().room(),
        owner.peer().authority().clone(),
        owner.peer().certificates(),
        RevocationList::new(),
        web_time::SystemTime::now(),
    )
    .unwrap();
    let (client, _) = connect_observer(
        &binding,
        room,
        &mut remote,
        Instant::now() + Duration::from_secs(10),
    )
    .await
    .unwrap();
    publish(&owner, &native).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        // Leave one actual checkpoint unadmitted so recovery must do more than
        // count replay frames: it must fill the missing document in this peer.
        let mut omitted_chat = false;
        for _ in 0..owner.outgoing().len() {
            let event = client.recv().await.unwrap();
            let qos_relay::transport::Envelope::Delta { delta, .. } = &event else {
                panic!("expected the owner's retained publication");
            };
            let qos_relay::coedit::CoeditPayload::Checkpoint { body, .. } =
                qos_relay::coedit::CoeditPayload::decode(delta).unwrap() else {
                    panic!("expected the initial checkpoint basis");
                };
            if matches!(postcard::from_bytes::<qos_peer_state::NeighbourMsg>(&body).unwrap(),
                qos_peer_state::NeighbourMsg::AppDocument { app_id, .. } if app_id == "dev.qos.chat") {
                omitted_chat = true;
            } else {
                apply_state(&mut remote, event);
            }
        }
        assert!(omitted_chat);
        assert!(remote.document("dev.qos.chat", "").is_none());
    })
    .await
    .unwrap();
    // Retain a local mutation absent from the owner/relay's checkpoint basis.
    let row = remote
        .rows()
        .into_iter()
        .find(|r| r.app_id == "dev.qos.notes")
        .unwrap();
    let mut doc = AutoCommit::load(&remote.document(&row.app_id, &row.record_id).unwrap()).unwrap();
    doc.set_actor(ui::automerge::ActorId::from(
        binding.cert.device_public_key.to_vec(),
    ));
    let _ = doc.save_incremental();
    doc.put(
        ROOT,
        "observer-local-proof",
        "must survive transport replacement",
    )
    .unwrap();
    doc.commit();
    remote
        .signed_mutation(
            &row.app_id,
            &row.record_id,
            collab_types::DocumentMutation::Incremental(doc.save_incremental()),
            &binding.key,
            web_time::SystemTime::now(),
        )
        .unwrap();
    let mut expected_rows = owner.peer().rows();
    *expected_rows
        .iter_mut()
        .find(|r| r.app_id == row.app_id)
        .unwrap() = remote
        .rows()
        .into_iter()
        .find(|r| r.app_id == row.app_id)
        .unwrap();
    let certificates = remote.certificates();
    capture.close_connection();
    let deadline = Instant::now() + Duration::from_millis(3800);
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
        loop {
            if matches!(observer_event(&client).await, ObserverEvent::Ended) {
                break;
            }
        }
    })
    .await
    .expect("real connection closure must end a receive lane");
    let mut replacements = 0;
    let (client, replayed) = replace_observer_after_eof(
        client,
        &binding,
        room,
        &mut remote,
        &mut replacements,
        deadline,
    )
    .await
    .expect("first EOF must replay using the same owner");
    assert!(
        replayed > 0,
        "recovery must admit actual retained frames before Ready"
    );
    assert_eq!(binding.endpoint.id(), endpoint_id);
    assert_eq!(replacements, 1);
    assert_eq!(
        remote.rows(),
        expected_rows,
        "replay must fill Chat and preserve the local Notes mutation"
    );
    assert!(remote.document("dev.qos.chat", "").is_some());
    assert_eq!(
        remote.durable_coverage("dev.qos.chat", "").unwrap(),
        owner.peer().durable_coverage("dev.qos.chat", "").unwrap()
    );
    assert_eq!(remote.certificates(), certificates);
    assert_ne!(
        remote.rows(),
        owner.peer().rows(),
        "local state was replaced by owner state"
    );
    capture.close_connection();
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
        loop {
            if matches!(observer_event(&client).await, ObserverEvent::Ended) {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(replace_observer_after_eof(client, &binding, room, &mut remote,
        &mut replacements, deadline).await, Err(error) if error == "second observer EOF")
    );
    assert_eq!(replacements, 1);
    endpoint.close().await;
    native.into_client().close().await;
    host.shutdown().await.unwrap();
}

#[cfg(feature = "test-support")]
async fn active_replacement(
    host: &mut Host,
    origin: &str,
    client: qos_relay::RelayClient,
    binding: &ObserverBinding,
    remote: &mut qos_peer_state::StatePeer,
    room: uuid::Uuid,
    cert: &qos_user_identity::DeviceCert,
    native: &qos_user_identity::DeviceCert,
    broker_host: uuid::Uuid,
    invocation: qos_actions::InvocationId,
    operation: qos_relay::peer_actions::ActionMessage,
) -> qos_relay::RelayClient {
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
            delta: active_replacement_state_delta(&signed),
            signature: [0; 64],
        })
        .unwrap();
    // This one absolute budget includes Notes delivery, transport replacement,
    // replay/capability restoration and authenticated pending Status.
    let deadline = Instant::now() + Duration::from_millis(3800);
    let client = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
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
        let mut client = client;
        let mut replacements = 0;
        let mut pending = qos_browser_runtime::action_client::PendingActions::default();
        let mut status_request = None;
        let mut observed_pending = false;
        loop {
            let status = request(origin, "/status", None, None).await.1;
            assert_eq!(status["qualification"]["held"], true,
                "old completion escaped replacement boundary: {status}");
            assert_eq!(status["qualification"]["expired"], false);
            assert!(Instant::now() < deadline, "active replacement did not become ready: {status}");
            let native_ready = status["actions"] == "ready"
                && status["native_subscription_generation"].as_u64().unwrap() > generation;
            if native_ready && observed_pending { break; }
            if native_ready && status_request.is_none() {
                let id = uuid::Uuid::new_v4();
                let directed = DirectedAction {
                    room, request_id: id, source: cert.device_public_key,
                    target: native.device_public_key,
                    message: ActionMessage::Status { invocation },
                };
                pending.insert(directed.clone()).unwrap();
                status_request = Some(id);
                // An ended outbound queue still requires actual receive EOF to
                // enter recovery. Without EOF/proof the same deadline fails.
                let _ = client.send_action(EnvelopeV2::Directed(directed));
            }
            let event = tokio::select! {
                event = observer_event(&client) => Some(event),
                _ = tokio::time::sleep(Duration::from_millis(5)) => None,
            };
            match event {
                Some(ObserverEvent::State(state)) => apply_state(remote, state),
                Some(ObserverEvent::Action(event)) => {
                    if let EnvelopeV2::Delivered { action, source_user } = &event {
                        if action.request_id == request_id {
                            assert_eq!(*source_user, native.user_public_key);
                            assert_eq!(action.source, native.device_public_key);
                            assert_eq!(action.target, cert.device_public_key);
                            assert_eq!(action.room, room);
                            assert!(matches!(action.message, ActionMessage::Observation { update: Some(ref update), uncertain: false, .. } if !update.is_terminal()),
                                "held old-generation invocation completed or lost custody: {:?}", action.message);
                        }
                    }
                    if pending.receive(event, native.user_public_key) {
                        let message = pending.take(status_request.unwrap()).unwrap();
                        if let ActionMessage::Observation {
                            host, invocation: id, update: Some(update), uncertain, ..
                        } = message {
                            assert_eq!(host, broker_host);
                            assert_eq!(id, invocation);
                            assert!(!uncertain);
                            assert!(!update.is_terminal(), "held original invocation completed prematurely: {update:?}");
                            observed_pending = true;
                        } else {
                            panic!("original invocation lost pending custody: {message:?}");
                        }
                    }
                }
                Some(ObserverEvent::Ended) => {
                    eprintln!("observer EOF after native reconnect; replacement attempt={}", replacements + 1);
                    // Nothing observed on the ended transport may release the hold.
                    pending = Default::default();
                    status_request = None;
                    observed_pending = false;
                    let (replacement, frames) = {
                        let recovery = replace_observer_after_eof(
                            client, binding, room, remote, &mut replacements, deadline);
                        tokio::pin!(recovery);
                        loop {
                            tokio::select! {
                                result = &mut recovery => break result.expect("observer recovery failed"),
                                _ = tokio::time::sleep(Duration::from_millis(5)) => {
                                    let status = request(origin, "/status", None, None).await.1;
                                    assert_eq!(status["qualification"]["held"], true,
                                        "old completion escaped during observer recovery: {status}");
                                    assert_eq!(status["qualification"]["expired"], false);
                                }
                            }
                        }
                    };
                    client = replacement;
                    eprintln!("observer replay Ready; replacements={replacements}; frames={frames}");
                }
                None => {},
            }
        }
        eprintln!("original invocation authenticated pending after native replacement; observer replacements={replacements}");
        client
    }).await.expect("Notes/replacement/Status exceeded the original 3.8s held-action deadline");
    writeln!(host.0.stdin.as_mut().unwrap(), "qualification release").unwrap();
    // Terminal recovery is Status for this original UUID, never an implicit Invoke.
    client
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

/// Replacement action registration can precede live delivery of its new checkpoint.
/// Observe one fixed owner cut through normal relay admission before binding a command.
#[cfg(feature = "test-support")]
async fn observe_native_publication(
    origin: &str,
    client: &qos_relay::RelayClient,
    remote: &mut qos_peer_state::StatePeer,
) {
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(5), async {
        let (code, target) = request(origin, "/status", None, None).await;
        assert_eq!(code, 200);
        assert_eq!(target["actions"], "ready");
        assert_eq!(target["native_peer"], "subscribed");
        let publication = &target["qualification"]["publication"];
        assert!(!publication["room_heads"].as_array().unwrap().is_empty());
        let receipts = publication["receipts"].as_array().unwrap();
        assert_eq!(receipts.len(), target["records"].as_array().unwrap().len());
        assert!(receipts.iter().all(|receipt| !receipt.is_null()));
        let mut frames = 0usize;
        loop {
            let rows = remote.rows();
            let heads = remote.admitted_room().map(|room| {
                room.clone()
                    .get_heads()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            });
            let observed_receipts = rows
                .iter()
                .map(|row| remote.durable_coverage(&row.app_id, &row.record_id).unwrap())
                .collect::<Vec<_>>();
            if serde_json::json!(heads) == publication["room_heads"]
                && serde_json::json!(rows) == target["records"]
                && serde_json::json!(observed_receipts) == publication["receipts"]
            {
                break;
            }
            apply_state(remote, client.recv().await.expect("publication stream closed"));
            frames += 1;
        }
        // A later legitimate commit is a failed precondition, not permission to
        // move this target or rebind an already constructed command.
        let (code, current) = request(origin, "/status", None, None).await;
        assert_eq!(code, 200);
        assert_eq!(current["generation"], target["generation"], "native publication target advanced");
        assert_eq!(current["records"], target["records"]);
        assert_eq!(&current["qualification"]["publication"], publication);
        assert_eq!(current["actions"], "ready");
        assert_eq!(current["native_peer"], "subscribed");
        eprintln!(
            "desktop_publication_barrier {}",
            serde_json::json!({"generation": target["generation"], "room_heads": publication["room_heads"], "receipt_ids": receipts.iter().map(|receipt| &receipt["receipt_id"]).collect::<Vec<_>>(), "exact_rows_and_receipts": true, "frames": frames, "elapsed_us": started.elapsed().as_micros()})
        );
    })
    .await
    .expect("observer did not admit the fixed native publication within five seconds");
}

async fn desktop_open_through_broker(
    client: &qos_relay::RelayClient, remote: &mut qos_peer_state::StatePeer,
    room: uuid::Uuid, cert: &qos_user_identity::DeviceCert, native: &qos_user_identity::DeviceCert,
    host: uuid::Uuid, catalogue: &serde_json::Value,
) -> (qos_actions::InvocationId, qos_relay::peer_actions::ActionMessage, usize) {
    use qos_peer_state::desktop_command::*;
    use qos_relay::peer_actions::ActionMessage;
    #[cfg(not(feature = "test-support"))]
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(100), client.recv()).await { apply_state(remote, event); }
    let mut tree = qos_desktop::WorkspaceTree::default();
    qos_desktop::projection::RoomProjection::apply_admitted_room(&mut tree, room.into(), remote.admitted_room().unwrap()).unwrap();
    let strip = tree.workspaces[0].strips[0].id.0;
    let command = BoundCommand {
        source: CommandSource { room: room.to_string(), heads: remote.admitted_room().unwrap().clone().get_heads().into_iter().map(|h|h.0).collect() },
        command: DesktopCommand::OpenTab { app: "dev.qos.notes".into(), placement: TabPlacement::NewGroup { strip, after: None } },
    };
    let surface = catalogue["surfaces"].as_array().unwrap().iter().find(|s|s["app_id"]==APP_ID).expect("installed native desktop service must be advertised");
    let invocation = qos_actions::InvocationId::new();
    let operation = ActionMessage::InvokeBound { host, invocation, app_peer: surface["peer_id"].as_u64().unwrap().to_string(), app_id: APP_ID.into(), record_id: "".into(), resource: RESOURCE.into(), action: ACTION.into(), argument: Some(serde_json::json!({"command":serde_json::to_string(&command).unwrap()})), confirmation: None, revision: None };
    let prior = remote.rows().len();
    invoke_and_observe(client, remote, room, cert, native, host, invocation, operation.clone()).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while remote.rows().len() <= prior { apply_state(remote, client.recv().await.unwrap()); }
    }).await.expect("native desktop record must replicate");
    let records = remote.rows().len();
    invoke_and_observe(client, remote, room, cert, native, host, invocation, operation.clone()).await;
    assert_eq!(remote.rows().len(), records, "duplicate full UUID must not create another checked record");
    let mut reconstructed = qos_desktop::WorkspaceTree::default();
    qos_desktop::projection::RoomProjection::apply_admitted_room(&mut reconstructed, room.into(), remote.admitted_room().unwrap()).unwrap();
    let count = |tree: &qos_desktop::WorkspaceTree| tree.workspaces.iter().flat_map(|w| &w.strips).flat_map(|s| &s.window_groups).map(|g|g.tabs.len()).sum::<usize>();
    assert_eq!(count(&reconstructed), count(&tree)+1, "signed canonical topology replicates along with checked record");
    (invocation, operation, records)
}
