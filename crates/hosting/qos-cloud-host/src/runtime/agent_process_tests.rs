//! Opt-in genuine signed-process qualification, never a fabricated authority fixture.
use super::*;
use qos_relay::{agent_reads::AgentReadBinding, peer_actions::ActionMessage as A};
use std::{
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
};

struct Reap(Child);
impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
async fn query(
    client: &RelayCoeditSession,
    room: uuid::Uuid,
    source: [u8; 32],
    host: [u8; 32],
    message: A,
) -> A {
    let request_id = uuid::Uuid::new_v4();
    client
        .send_action(qos_relay::peer_actions::DirectedAction {
            room,
            request_id,
            source,
            target: host,
            message,
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(
                EnvelopeV2::Delivered { action, .. }
                | EnvelopeV2::CertifiedDelivered { action, .. },
            ) = client.recv_action().await
            {
                if action.request_id == request_id {
                    assert_eq!(action.source, host);
                    assert_eq!(action.target, source);
                    return action.message;
                }
            }
        }
    })
    .await
    .expect("actual certified reply deadline")
}
fn readiness_reply(reply: &A) -> String {
    match reply {
        A::Rejected { code } => format!("rejected: {}", code.chars().take(256).collect::<String>()),
        A::Observation {
            uncertain, update, ..
        } => {
            let failure = match update {
                Some(qos_actions::InvocationUpdate::Failed { error, .. }) => {
                    error.code.chars().take(256).collect::<String>()
                }
                _ => format!("{:?}", update.as_ref().map(std::mem::discriminant)),
            };
            format!("observation uncertain={uncertain} update={failure}")
        }
        _ => format!("unexpected reply {:?}", std::mem::discriminant(reply)),
    }
}
fn gateway(root: &std::path::Path) -> (Reap, PathBuf) {
    let executable = PathBuf::from(
        std::env::var_os("QOS_AGENT_TEST_GATEWAY_BINARY")
            .expect("absolute retained gateway executable required"),
    );
    assert!(executable.is_absolute() && executable.is_file());
    let config = root.join("gateway-config");
    let empty_path = root.join("empty-path");
    let home = root.join("gateway-home");
    std::fs::create_dir_all(config.join("tilo")).unwrap();
    std::fs::create_dir(&empty_path).unwrap();
    std::fs::create_dir(&home).unwrap();
    std::fs::write(config.join("tilo/settings.json"),serde_json::to_vec(&serde_json::json!({"agent_providers":{"config":{"profiles":[{"id":"test-provider","label":"Local fixture","protocol":"open_ai_compatible","endpoint":"http://127.0.0.1:9/v1","location":"this_device","enabled":true,"models":[{"id":"test-model"}]}]}}})).unwrap()).unwrap();
    let socket = root.join("gateway.sock");
    let child = Reap(
        Command::new(executable)
            .args(["serve", "--socket"])
            .arg(&socket)
            .env_clear()
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", config)
            .env("PATH", empty_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        match UnixStream::connect(&socket) {
            Ok(s) => break s,
            Err(_) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "gateway listener startup"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    tilo_model_gateway::write_frame(
        &mut stream,
        &serde_json::json!({"protocol":1,"app_id":"so.tilo.settings"}),
    )
    .unwrap();
    tilo_model_gateway::write_frame(&mut stream,&serde_json::json!({"version":3,"verb":"set_api_key","profile_id":"test-provider","api_key":"fabricated-fixture-key"})).unwrap();
    let response: tilo_model_gateway::Response =
        tilo_model_gateway::read_frame(&mut stream).unwrap();
    assert!(
        response.ok && response.error.is_none(),
        "actual private gateway provisioning failed"
    );
    (child, socket)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicit signed fixture config and actual retained Linux gateway binary; run with --ignored"]
async fn agent_runtime_genuine_queued_started_shutdown_releases_exact_seat() {
    genuine_spawn_scenario(None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicit signed fixture config and actual retained Linux gateway binary; run with --ignored"]
async fn agent_runtime_genuine_source_queued_started_shutdown_releases_exact_seat() {
    genuine_spawn_scenario(Some(StageScenario {
        checkpoints: &[],
        expect_launch: true,
    }))
    .await;
}

#[derive(Clone, Copy)]
struct StageScenario {
    checkpoints: &'static [&'static str],
    expect_launch: bool,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicit signed fixture config and actual retained Linux gateway binary; run with --ignored"]
async fn agent_runtime_genuine_launch_stage_recovery_and_published_refusal() {
    for scenario in [
        StageScenario {
            checkpoints: &["publish-submitted"],
            expect_launch: true,
        },
        StageScenario {
            checkpoints: &["before-publish", "publish-submitted"],
            expect_launch: true,
        },
        StageScenario {
            checkpoints: &["after-publish"],
            expect_launch: false,
        },
        StageScenario {
            checkpoints: &["before-commit"],
            expect_launch: false,
        },
    ] {
        genuine_spawn_scenario(Some(scenario)).await;
    }
}

fn stage_audit(root: &std::path::Path, invocation: uuid::Uuid) -> Vec<String> {
    let bytes = std::fs::read_to_string(root.join("agent-test-launch-audit")).unwrap();
    // Ignore an in-progress final append during polling; final assertions follow
    // a completed phase or joined runtime shutdown.
    let complete = bytes.rfind('\n').map_or("", |end| &bytes[..=end]);
    let rows: Vec<_> = complete
        .lines()
        .map(|line| line.split('|').collect::<Vec<_>>())
        .collect();
    let mut original = None;
    for row in &rows {
        assert_eq!(row.len(), 3, "exact runtime audit shape");
        assert_eq!(row[0], invocation.to_string(), "no substituted invocation");
        if let Some(pointer) = original {
            assert_eq!(
                row[1], pointer,
                "same original live allocation across retries"
            );
        }
        original = Some(row[1]);
    }
    rows.into_iter().map(|row| row[2].to_owned()).collect()
}

async fn genuine_spawn_scenario(stage: Option<StageScenario>) {
    let fixture = PathBuf::from(
        std::env::var_os("QOS_AGENT_RUNTIME_FIXTURE_CONFIG")
            .expect("actual signed fixture config required"),
    );
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture).unwrap()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (_gateway, socket) = gateway(dir.path());
    config["gateway_socket"] = socket.to_string_lossy().to_string().into();
    config["agent_data_home"] = dir
        .path()
        .join("agent-data")
        .to_string_lossy()
        .to_string()
        .into();
    std::fs::create_dir(dir.path().join("agent-data")).unwrap();
    config["provider"] = serde_json::json!({"profile_id":"test-provider","model_id":"test-model","protocol":"open_ai_compatible"});
    std::fs::write(
        dir.path().join("agent-runtime.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let gate = dir.path().join("agent-test-hold-started");
    std::fs::write(&gate, b"hold genuine events before admission").unwrap();
    if let Some(stage) = stage {
        std::fs::write(dir.path().join("agent-test-launch-audit"), b"").unwrap();
        for checkpoint in stage.checkpoints {
            std::fs::write(
                dir.path()
                    .join(format!("agent-test-launch-fail-{checkpoint}")),
                b"one exact launch Store failure",
            )
            .unwrap();
        }
    }
    let root = SigningKey::from_bytes(&[91; 32]);
    let device = SigningKey::from_bytes(&[92; 32]);
    let mut store = Store::open(dir.path(), "runtime-process-exit").unwrap();
    let enrollment = crate::tests::approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&enrollment, 11).unwrap();
    let mut peer = store.peer().unwrap();
    peer.select_admission_v5().unwrap();
    let mut state = store.state.clone();
    state.bound.as_mut().unwrap().peer = peer.durable().unwrap();
    store.commit(state).unwrap();
    let caller = store
        .agent_caller(device.verifying_key().to_bytes())
        .unwrap();
    store.activate_private_agent_ledger(&caller).unwrap();
    store.activate_private_agent_runtime_v2(&caller).unwrap();
    let person = caller.certificate().clone();
    let host = store.peer().unwrap().authority().device_public_key;
    let room: uuid::Uuid = store.state.room.parse().unwrap();
    let workspace = qos_relay::room_workspace_id(&store.state.room).unwrap();
    let selected_source = if stage.is_some() {
        let clock = qos_action_broker::peer_actions::OwnerPolicyClock::new().unwrap();
        store
            .activate_delivery_storage(clock.now_ns().unwrap())
            .unwrap();
        let source =
            crate::https_owner::CloudStoreSource::bind_with_clock(&store, clock.clone()).unwrap();
        Some((clock, source))
    } else {
        None
    };
    drop(store);
    let selected_source = selected_source.map(|(clock, source)| {
        let crate::https_owner::Current::Current(time) =
            source.with_current(|turn| turn.policy_time().unwrap())
        else {
            panic!("actual original Store Source current turn required");
        };
        AgentTestSource {
            clock,
            time,
            source,
        }
    });
    let original_time = selected_source.as_ref().map(|source| source.time.clone());
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let path = dir.path().to_path_buf();
    let daemon = tokio::spawn(async move {
        let shutdown = async {
            let _ = stop_rx.await;
        };
        match selected_source {
            Some(source) => {
                serve_with_test_source(
                    &path,
                    "runtime-process-exit",
                    RelayMode::Disabled,
                    shutdown,
                    Some(ready_tx),
                    source,
                )
                .await
            }
            None => {
                serve_with_mode(
                    &path,
                    "runtime-process-exit",
                    RelayMode::Disabled,
                    shutdown,
                    Some(ready_tx),
                )
                .await
            }
        }
    });
    let address = tokio::time::timeout(Duration::from_secs(20), ready_rx)
        .await
        .unwrap()
        .unwrap();
    let endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let client = RelayClient::connect_v2_to_enrolled_home_peer(
        endpoint,
        address,
        root.verifying_key().to_bytes(),
        host,
        person,
        device,
        RevocationList::new(),
    )
    .await
    .unwrap();
    let client =
        std::sync::Arc::new(RelayCoeditSession::join(client, workspace, caller.device()).unwrap());
    let drain_client = client.clone();
    let drain = tokio::spawn(async move { while drain_client.recv().await.is_some() {} });
    client.enable_actions(room).unwrap();
    let mut pending = std::collections::VecDeque::new();
    await_action_ready(
        &client,
        ExpectedActionReady::Transport { room },
        &mut pending,
        "genuine process test",
    )
    .await
    .unwrap();
    client.enable_broker_client(room).unwrap();
    let mut last_catalogue = String::from("no reply");
    let mut last_capabilities = String::from("no reply");
    let readiness = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let reply = query(
                &client,
                room,
                caller.device(),
                host,
                A::CatalogueRefresh { scope: None },
            )
            .await;
            if let A::BrokerCatalogue { catalogue, .. } = reply {
                match serde_json::from_value::<AgentReadBinding>(catalogue["agentReadsV1"].clone())
                {
                    Ok(binding) => {
                        last_catalogue = format!(
                            "binding spawn={} message={}",
                            binding.spawn.is_some(),
                            binding.message.is_some()
                        );
                        // Capabilities are a disposable read even when spawn is absent.
                        let reply = query(
                            &client,
                            room,
                            caller.device(),
                            host,
                            A::AgentReadV1 {
                                host: room,
                                invocation: uuid::Uuid::new_v4(),
                                request: qos_agent_control_protocol::AgentRequest::Capabilities {
                                    version: 1,
                                },
                            },
                        )
                        .await;
                        if let A::AgentReadResultV1 { response, .. } = reply {
                            match response {
                                qos_agent_control_protocol::AgentResponse::Capabilities {
                                    capabilities,
                                    ..
                                } => {
                                    last_capabilities = format!(
                                        "spawn={} offers={} reasons={:?}",
                                        capabilities.spawn,
                                        capabilities.grant_offers.len(),
                                        capabilities
                                            .unavailable_reasons
                                            .iter()
                                            .take(16)
                                            .collect::<Vec<_>>()
                                    );
                                    if binding.spawn.is_some()
                                        && capabilities.spawn
                                        && capabilities.grant_offers.len() == 1
                                    {
                                        break (binding, capabilities.grant_offers[0].clone());
                                    }
                                }
                                qos_agent_control_protocol::AgentResponse::Error {
                                    error, ..
                                } => {
                                    last_capabilities = format!("error={:?}", error.code);
                                }
                                response => {
                                    last_capabilities = format!(
                                        "unexpected response {:?}",
                                        std::mem::discriminant(&response)
                                    )
                                }
                            }
                        } else {
                            last_capabilities = readiness_reply(&reply);
                        }
                    }
                    Err(error) => {
                        last_catalogue = format!(
                            "binding decode: {}",
                            error.to_string().chars().take(256).collect::<String>()
                        )
                    }
                }
            } else {
                last_catalogue = readiness_reply(&reply);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let (binding, offer) = match readiness {
        Ok(value) => value,
        Err(error) => {
            eprintln!("last actual catalogue: {last_catalogue}; capabilities: {last_capabilities}");
            let _ = stop_tx.send(());
            let stopped = tokio::time::timeout(Duration::from_secs(15), daemon).await;
            let outcome = match stopped {
                Ok(Ok(Ok(()))) => "completed-ok",
                Ok(Ok(Err(_))) => "completed-error",
                Ok(Err(_)) => "join-error",
                Err(_) => "shutdown-timeout",
            };
            eprintln!("actual runtime shutdown after readiness failure: {outcome}");
            client.close().await;
            drain.await.unwrap();
            panic!("real package/workers/gateway/native capability never installed: {error}");
        }
    };
    let route = binding.spawn.unwrap().route;
    let invocation = qos_actions::InvocationId(uuid::Uuid::new_v4());
    let message = A::InvokeBound {
        host: room,
        invocation,
        app_peer: route.app_peer.parse().unwrap(),
        app_id: route.app_id,
        record_id: route.record_id,
        resource: route.resource,
        action: route.action,
        argument: Some(
            serde_json::to_value(qos_agent_control_protocol::AgentRequest::Spawn {
                version: 1,
                name: "Custody fixture".into(),
                task: "Private shutdown qualification".into(),
                grant_set_id: offer.id,
            })
            .unwrap(),
        ),
        confirmation: None,
        revision: Some(route.revision),
    };
    let accepted = query(&client, room, caller.device(), host, message.clone()).await;
    assert!(
        matches!(
            accepted,
            A::Observation {
                update: Some(qos_actions::InvocationUpdate::Accepted { .. }),
                ..
            }
        ),
        "genuine Spawn was not durably admitted: {accepted:?}"
    );
    if let Some(stage) = stage {
        // Same immutable invocation is a status/retry request, never a new command.
        let duplicate = query(&client, room, caller.device(), host, message).await;
        assert!(
            matches!(duplicate, A::Observation { .. }),
            "original invocation replay must observe its retained admission"
        );
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let events = stage_audit(dir.path(), invocation.0);
                if events
                    .iter()
                    .filter(|event| event.as_str() == "parked")
                    .count()
                    == stage.checkpoints.len()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("actual joined failure never parked original retry");
        if !stage.expect_launch {
            // Allow the normal one-second recovery scan to run, then close the
            // actual runtime. A published LaunchIntended can never be relaunched.
            tokio::time::sleep(Duration::from_millis(1300)).await;
            let _ = stop_tx.send(());
            tokio::time::timeout(Duration::from_secs(15), daemon)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            client.close().await;
            drain.await.unwrap();
            assert!(
                original_time.as_ref().unwrap().now_ns().is_err(),
                "actual runtime close retires the same installed Source"
            );
            assert_eq!(
                stage_audit(dir.path(), invocation.0),
                ["joined", "commit_error", "closed", "parked"]
            );
            assert!(!gate.with_extension("observed").exists());
            let store = Store::open(dir.path(), "runtime-process-exit").unwrap();
            let Some(qos_peer_state::owner_commit::PrivateAgentState::V2(ledger)) =
                &store.state.private_agent_ledger
            else {
                panic!("selected ledger lost")
            };
            assert!(
                ledger
                    .facts
                    .contains_key(&format!("{}:launch_intended", invocation.0))
            );
            assert!(!ledger.facts.values().any(|row| matches!(
                row.change.fact,
                qos_peer_state::owner_commit::AgentRuntimeFact::ProcessBound { .. }
                    | qos_peer_state::owner_commit::AgentRuntimeFact::ProcessExited { .. }
            )));
            eprintln!(
                "actual stage {:?}: published launch fact, joined+closed+parked, no authorize/launch/process",
                stage.checkpoints
            );
            return;
        }
    }
    tokio::time::timeout(Duration::from_secs(8), async {
        while !gate.with_extension("observed").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("no genuine queued Started before normal pump");
    if let Some(stage) = stage {
        let mut expected = Vec::new();
        for n in 0..stage.checkpoints.len() {
            expected.push("joined".to_owned());
            if n > 0 {
                expected.push("retry_taken".into());
            }
            expected.extend(
                ["commit_error", "closed", "parked", "sole_after_error"].map(str::to_owned),
            );
        }
        expected.push("joined".into());
        if !stage.checkpoints.is_empty() {
            expected.push("retry_taken".into());
        }
        expected.extend(["barrier", "authorized", "launched"].map(str::to_owned));
        assert_eq!(
            stage_audit(dir.path(), invocation.0),
            expected,
            "full pump → current sole admission → scheduled original retry ordering"
        );
        for checkpoint in stage.checkpoints {
            assert!(
                !dir.path()
                    .join(format!("agent-test-launch-fail-{checkpoint}"))
                    .exists()
            );
        }
        eprintln!(
            "actual stage {:?}: original same-Arc command resumed through normal scanner; one barrier/authorize/launch",
            stage.checkpoints
        );
    }
    let before = Store::open(dir.path(), "runtime-process-exit").unwrap();
    let Some(qos_peer_state::owner_commit::PrivateAgentState::V2(ledger)) =
        &before.state.private_agent_ledger
    else {
        panic!("selected ledger lost")
    };
    assert!(
        !ledger
            .facts
            .contains_key(&format!("{}:process_bound", invocation.0)),
        "fixture gate admitted the channel"
    );
    assert!(
        !ledger
            .facts
            .contains_key(&format!("{}:process_exited", invocation.0))
    );
    drop(before);
    let _ = stop_tx.send(());
    tokio::time::timeout(Duration::from_secs(15), daemon)
        .await
        .expect("normal runtime shutdown deadline")
        .unwrap()
        .unwrap();
    client.close().await;
    drain.await.unwrap();
    if let Some(time) = original_time {
        assert!(
            time.now_ns().is_err(),
            "actual runtime close retires the original Source"
        );
    }
    let store = Store::open(dir.path(), "runtime-process-exit").unwrap();
    let Some(qos_peer_state::owner_commit::PrivateAgentState::V2(ledger)) =
        &store.state.private_agent_ledger
    else {
        panic!("selected ledger lost")
    };
    let exits = ledger
        .facts
        .values()
        .filter(|receipt| {
            matches!(
                receipt.change.fact,
                qos_peer_state::owner_commit::AgentRuntimeFact::ProcessExited { .. }
            )
        })
        .count();
    assert_eq!(
        exits, 1,
        "genuine queued Started/Finished must retire one exact process seat"
    );
    assert!(
        ledger
            .facts
            .contains_key(&format!("{}:unknown", invocation.0))
    );
    assert!(
        !ledger
            .facts
            .contains_key(&format!("{}:completed", invocation.0)),
        "process exit is not recipient acceptance"
    );
    assert!(
        !ledger
            .facts
            .contains_key(&format!("{}:task_finished", invocation.0)),
        "process exit is not a task outcome"
    );
    let caller = store.agent_caller(caller.device()).unwrap();
    let id = crate::agent_runtime_owner::AgentControlId::new(invocation.0).unwrap();
    assert_eq!(
        store.observe_agent_runtime(&caller, &id).unwrap().phase,
        crate::agent_runtime_owner::AgentRuntimePhase::Unknown
    );
    println!(
        "genuine queued Started -> exact Finished -> one Store ProcessExited; control/task remain unknown"
    );
}
