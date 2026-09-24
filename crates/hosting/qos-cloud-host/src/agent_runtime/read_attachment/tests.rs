use super::*;
use crate::{Store, agent_runtime::CloudAgentRuntime};
use qos_action_broker::agent_reads::{AgentReadError, CertifiedAgentReads};
use qos_agent_control_protocol::{AgentRequest, AgentResponse};
use qos_relay::{
    client::CertifiedAction,
    delivery_v3::{auth, http_proof, private_wire},
    peer_actions::{ActionMessage, DirectedAction},
};
use tokio::time::{Duration, Instant};
use uuid::Uuid;

// Actual Store authorization and signed HTTP admission, not a manufactured
// CertifiedAction or a positive installation/memory qualification witness.
pub(crate) fn certified(store: &Store, message: ActionMessage) -> CertifiedAction {
    struct Current<'a>(&'a Store);
    impl auth::Authority for Current<'_> {
        fn authorize(
            &self,
            b: &auth::Binding,
            device: &qos_user_identity::DeviceCert,
            host: &qos_user_identity::DeviceCert,
            _: u64,
        ) -> Result<(), auth::AuthError> {
            let peer = self.0.peer().map_err(|_| auth::AuthError::PolicyDenied)?;
            let caller = self
                .0
                .agent_caller(device.device_public_key)
                .map_err(|_| auth::AuthError::PolicyDenied)?;
            if caller.certificate() != device
                || peer.authority() != host
                || b.workspace != qos_relay::room_workspace_id(peer.room()).unwrap()
                || b.workspace_root != host.user_public_key
            {
                return Err(auth::AuthError::PolicyDenied);
            }
            Ok(())
        }
    }
    let peer = store.peer().unwrap();
    let device_key = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
    let caller = store
        .agent_caller(device_key.verifying_key().to_bytes())
        .unwrap();
    let now = qos_relay::clock::now_ns();
    let binding = auth::Binding {
        workspace: qos_relay::room_workspace_id(peer.room()).unwrap(),
        workspace_root: peer.authority().user_public_key,
        device: caller.device(),
        device_root: caller.certificate().user_public_key,
        host: peer.authority().device_public_key,
        host_root: peer.authority().user_public_key,
        origin: "https://localhost:9443".into(),
        descriptor_generation: 1,
        descriptor_digest: [8; 32],
        server_boot_id: [9; 16],
        carrier: auth::Carrier::Https,
        requested_capabilities: 6,
        mandatory_capabilities: 6,
    };
    let revocations = qos_user_identity::RevocationList::new();
    let authority = Current(store);
    let context = auth::VerificationContext {
        binding: &binding,
        capabilities: auth::CapabilityPolicy {
            supported: 6,
            allowed: 6,
        },
        credentials: auth::Credentials {
            device: caller.certificate(),
            host: peer.authority(),
            device_revocations: &revocations,
            host_revocations: &revocations,
        },
        authority: &authority,
        now_ns: now,
    };
    let (challenge, server) = auth::Challenge::issue(&context, [2; 32], &store.key()).unwrap();
    let client = auth::verify_server(
        &auth::ClientExpectations {
            scope: auth::ClientScope::from(&binding),
            capabilities: context.capabilities,
            credentials: context.credentials,
            authority: &authority,
            now_ns: now,
        },
        [2; 32],
        &server,
    )
    .unwrap()
    .client_proof(&device_key)
    .unwrap();
    let session = challenge.finish(&client, &context).unwrap();
    let id = session.session_id();
    let digest = http_proof::session_binding_digest(
        session.binding(),
        id,
        session.capabilities(),
        session.expires_at_ns(),
    )
    .unwrap();
    let mut admission = http_proof::HttpSessionAdmission::new(session, &context).unwrap();
    let operation = match &message {
        ActionMessage::CatalogueRefresh { .. } => private_wire::PrivateOperation::Catalogue,
        ActionMessage::AgentReadV1 { .. } => private_wire::PrivateOperation::AgentRead,
        ActionMessage::Status { .. } => private_wire::PrivateOperation::Status,
        ActionMessage::Cancel { .. } => private_wire::PrivateOperation::Cancel,
        ActionMessage::InvokeBound { .. } => private_wire::PrivateOperation::Invoke,
        _ => panic!("fixture operation"),
    };
    let action = DirectedAction {
        room: peer.room().parse().unwrap(),
        request_id: Uuid::new_v4(),
        source: caller.device(),
        target: peer.authority().device_public_key,
        message,
    };
    let bytes = private_wire::encode_request(
        operation,
        private_wire::PrivateScope {
            room: action.room,
            source: action.source,
            target: action.target,
        },
        &action,
    )
    .unwrap();
    let request = http_proof::Request::new("POST", operation.path(), &bytes).unwrap();
    let proof =
        http_proof::RequestProof::sign(id, digest, &request, now, now + 1_000_000_000, &device_key)
            .unwrap();
    let admitted = admission.admit(&proof, &request, &context).unwrap();
    CertifiedAction::from_http_request(admitted, &peer).unwrap()
}
fn evidence(root: &std::path::Path, request: AgentRequest) -> CertifiedAction {
    let store = Store::open(root, "agent-admission").unwrap();
    certified(
        &store,
        ActionMessage::AgentReadV1 {
            host: store.state.room.parse().unwrap(),
            invocation: Uuid::new_v4(),
            request,
        },
    )
}
#[tokio::test]
async fn agent_read_attachment_real_private_projection_and_binding_never_write() {
    let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
    store
        .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
        .unwrap();
    let id = Uuid::new_v4();
    let (caller, request) = crate::agent_owner_tests::prepare(&store, device, &id.to_string());
    store.commit_agent_request(&request, &caller).unwrap();
    let catalogue = certified(&store, ActionMessage::CatalogueRefresh { scope: None });
    drop(store);
    let mut runtime = CloudAgentRuntime::start(dir.path(), "agent-admission", true)
        .await
        .unwrap();
    let service = runtime.certified_commands();
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    let binding = service
        .binding(catalogue, Instant::now() + Duration::from_secs(5), None)
        .await
        .unwrap();
    let (binding, _) = binding.into_parts();
    assert!(binding.spawn.is_none() && binding.message.is_none());
    let response = service
        .read(
            evidence(
                dir.path(),
                AgentRequest::List {
                    version: 1,
                    cursor: None,
                },
            ),
            Instant::now() + Duration::from_secs(5),
            None,
        )
        .await
        .unwrap();
    let (response, _) = response.into_parts();
    let ActionMessage::AgentReadResultV1 {
        binding: actual,
        response: AgentResponse::List { agents, .. },
        ..
    } = response
    else {
        panic!("actual private list missing")
    };
    assert_eq!(actual, binding);
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].id, format!("agent:{id}"));
    assert_eq!(agents[0].description, "secret task never published");
    let response = service
        .read(
            evidence(
                dir.path(),
                AgentRequest::Conversation {
                    version: 1,
                    agent_id: agents[0].id.clone(),
                    cursor: None,
                },
            ),
            Instant::now() + Duration::from_secs(5),
            None,
        )
        .await
        .unwrap();
    let (response, _) = response.into_parts();
    assert!(
        matches!(response,ActionMessage::AgentReadResultV1{response:AgentResponse::Conversation{messages,..},..}if messages.is_empty()),
        "legacy ledger must not invent timestamps or conversation events"
    );
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    runtime.close().await.unwrap();
}
#[tokio::test]
async fn agent_read_attachment_deadline_shutdown_and_actual_queue_capacity() {
    let (dir, store, _) = crate::agent_owner_tests::enrolled();
    let catalogue = certified(&store, ActionMessage::CatalogueRefresh { scope: None });
    drop(store);
    let mut runtime = CloudAgentRuntime::start(dir.path(), "agent-admission", true)
        .await
        .unwrap();
    let service = runtime.certified_commands();
    assert_eq!(
        service
            .binding(catalogue.clone(), Instant::now(), None)
            .await
            .unwrap_err(),
        AgentReadError::Deadline
    );
    let permits = runtime.commands.try_reserve_many(32).unwrap();
    assert_eq!(
        service
            .binding(
                catalogue.clone(),
                Instant::now() + Duration::from_secs(5),
                None
            )
            .await
            .unwrap_err(),
        AgentReadError::Capacity
    );
    drop(permits);
    assert!(
        service
            .binding(
                catalogue.clone(),
                Instant::now() + Duration::from_secs(5),
                None
            )
            .await
            .is_ok()
    );
    runtime.close().await.unwrap();
    assert_eq!(
        service
            .binding(catalogue, Instant::now() + Duration::from_secs(5), None)
            .await
            .unwrap_err(),
        AgentReadError::Unavailable
    );
}

#[tokio::test]
async fn agent_read_attachment_result_retains_original_queue_custody() {
    use qos_action_broker::{agent_reads::AgentReadCustody, peer_actions::PeerActionQueue};
    let (dir, store, _) = crate::agent_owner_tests::enrolled();
    let evidence = certified(&store, ActionMessage::CatalogueRefresh { scope: None });
    drop(store);
    let mut runtime = CloudAgentRuntime::start(dir.path(), "agent-admission", true)
        .await
        .unwrap();
    let service = runtime.certified_commands();
    let (queue, mut requests) = PeerActionQueue::channel();
    let deadline = Instant::now() + Duration::from_secs(5);
    let pending = queue.submit_certified(evidence.clone(), deadline).unwrap();
    let request = requests.recv().await.unwrap();
    let custody = AgentReadCustody::from_request(&request);
    let reply = service
        .handle_certified_with_custody(evidence.clone(), deadline, custody)
        .await
        .unwrap();
    assert!(matches!(
        reply.value(),
        Some(ActionMessage::BrokerCatalogue { .. })
    ));
    drop(request);
    drop(pending);
    let mut held = Vec::new();
    for _ in 0..31 {
        held.push(queue.submit_certified(evidence.clone(), deadline).unwrap());
    }
    assert!(matches!(
        queue.submit_certified(evidence.clone(), deadline),
        Err(qos_action_broker::peer_actions::CarrierError::Capacity)
    ));
    drop(reply);
    assert!(queue.submit_certified(evidence, deadline).is_ok());
    drop(held);
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn agent_read_attachment_blocked_store_read_keeps_custody_after_waiter_drop() {
    use qos_action_broker::{
        agent_reads::AgentReadCustody,
        peer_actions::{CarrierError, PeerActionQueue},
    };
    let (dir, store, _) = crate::agent_owner_tests::enrolled();
    let evidence = certified(&store, ActionMessage::CatalogueRefresh { scope: None });
    drop(store);
    let owner = Arc::new(RuntimeOwner::open(dir.path(), "agent-admission").unwrap());
    let provider = AgentControlProvider::new_read_only(owner.clone());
    publish_runtime_state(&owner, None, &mut None, false, false).unwrap();
    let store_lock = Store::open(dir.path(), "agent-admission").unwrap();
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    let (queue, mut requests) = PeerActionQueue::channel();
    let deadline = Instant::now() + Duration::from_secs(5);
    let pending = queue.submit_certified(evidence.clone(), deadline).unwrap();
    let request = requests.recv().await.unwrap();
    let custody = AgentReadCustody::from_request(&request);
    let (reply, response) = oneshot::channel();
    let command = Command {
        policy_time: owner.policy_time().unwrap(),
        evidence: evidence.clone(),
        kind: CommandKind::Binding,
        deadline,
        reply,
        custody: Some(custody),
    };
    let root = dir.path().to_path_buf();
    let (started, begin) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        started.send(()).unwrap();
        runtime.block_on(dispatch(
            command,
            &root,
            &owner,
            Some(&provider),
            None,
            true,
            &mut Default::default(),
        ));
    });
    begin.recv_timeout(Duration::from_secs(1)).unwrap();
    // Real dispatch is synchronous through Store::open's OS lock. Keep its
    // receiver live while the worker enters that read, then dispose the waiter.
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(!thread.is_finished());
    drop(response);
    drop(request);
    drop(pending);
    let mut held = Vec::new();
    for _ in 0..31 {
        held.push(queue.submit_certified(evidence.clone(), deadline).unwrap());
    }
    let while_blocked = queue.submit_certified(evidence.clone(), deadline);
    // Always release the actual OS lock before an assertion can unwind.
    drop(store_lock);
    tokio::task::spawn_blocking(move || thread.join().unwrap())
        .await
        .unwrap();
    assert!(matches!(while_blocked, Err(CarrierError::Capacity)));
    assert!(queue.submit_certified(evidence, deadline).is_ok());
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    drop(held);
}

#[tokio::test]
async fn agent_read_attachment_broker_callback_uses_actual_owner_and_exact_scope() {
    let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
    store
        .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
        .unwrap();
    let host = store.state.room.parse().unwrap();
    drop(store);
    let mut runtime = CloudAgentRuntime::start(dir.path(), "agent-admission", true)
        .await
        .unwrap();
    let service = runtime.certified_commands();
    let evidence = evidence(dir.path(), AgentRequest::Capabilities { version: 1 });
    let reply = qos_action_broker::agent_reads::read_from_owner(
        &service,
        host,
        evidence.clone(),
        Instant::now() + Duration::from_secs(5),
        None,
    )
    .await
    .unwrap();
    assert!(matches!(
        reply.value(),
        ActionMessage::AgentReadResultV1 {
            response: AgentResponse::Capabilities { .. },
            ..
        }
    ));
    assert_eq!(
        qos_action_broker::agent_reads::read_from_owner(
            &service,
            Uuid::new_v4(),
            evidence,
            Instant::now() + Duration::from_secs(5),
            None
        )
        .await
        .unwrap_err(),
        AgentReadError::InvalidRequest
    );
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn agent_read_attachment_original_retained_status_cancel_and_ordinary_fallback() {
    let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
    store
        .activate_private_agent_ledger(&store.agent_caller(device).unwrap())
        .unwrap();
    let id = Uuid::new_v4();
    let (caller, request) = crate::agent_owner_tests::prepare(&store, device, &id.to_string());
    store.commit_agent_request(&request, &caller).unwrap();
    let invocation = qos_actions::InvocationId(id);
    let status = certified(&store, ActionMessage::Status { invocation });
    let cancel = certified(&store, ActionMessage::Cancel { invocation });
    let ordinary = certified(
        &store,
        ActionMessage::Status {
            invocation: qos_actions::InvocationId(Uuid::new_v4()),
        },
    );
    drop(store);
    let mut runtime = CloudAgentRuntime::start(dir.path(), "agent-admission", true)
        .await
        .unwrap();
    let commands = runtime.certified_commands();
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    for evidence in [status, cancel] {
        let response = commands
            .handle_certified(evidence, Instant::now() + Duration::from_secs(5))
            .await
            .unwrap();
        assert!(
            matches!(response,Some(ActionMessage::Observation{invocation:actual,uncertain:true,..})if actual==invocation)
        );
    }
    assert!(
        commands
            .handle_certified(ordinary, Instant::now() + Duration::from_secs(5))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    let (foreign_dir, foreign, _) = crate::agent_owner_tests::enrolled();
    let wrong = certified(&foreign, ActionMessage::CatalogueRefresh { scope: None });
    drop(foreign);
    assert_eq!(
        commands
            .binding(wrong, Instant::now() + Duration::from_secs(5), None)
            .await
            .unwrap_err(),
        AgentReadError::Denied
    );
    runtime.close().await.unwrap();
    drop(foreign_dir);
}

#[tokio::test]
async fn agent_read_attachment_expired_and_dropped_reads_skip_locked_store() {
    let (dir, store, _) = crate::agent_owner_tests::enrolled();
    let evidence = certified(&store, ActionMessage::CatalogueRefresh { scope: None });
    drop(store);
    let owner = Arc::new(RuntimeOwner::open(dir.path(), "agent-admission").unwrap());
    let provider = AgentControlProvider::new_read_only(owner.clone());
    publish_runtime_state(&owner, None, &mut None, false, false).unwrap();
    let lock = Store::open(dir.path(), "agent-admission").unwrap();
    let (expired_reply, expired) = oneshot::channel();
    let (dropped_reply, dropped) = oneshot::channel();
    drop(dropped);
    let commands = [
        Command {
            policy_time: owner.policy_time().unwrap(),
            evidence: evidence.clone(),
            kind: CommandKind::Binding,
            deadline: Instant::now(),
            custody: None,
            reply: expired_reply,
        },
        Command {
            policy_time: owner.policy_time().unwrap(),
            evidence,
            kind: CommandKind::Binding,
            deadline: Instant::now() + Duration::from_secs(5),
            custody: None,
            reply: dropped_reply,
        },
    ];
    let root = dir.path().to_path_buf();
    let (done, completed) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for command in commands {
            runtime.block_on(dispatch(
                command,
                &root,
                &owner,
                Some(&provider),
                None,
                true,
                &mut Default::default(),
            ));
        }
        done.send(()).unwrap();
    });
    let finished = completed.recv_timeout(Duration::from_secs(1));
    drop(lock);
    tokio::task::spawn_blocking(move || thread.join().unwrap())
        .await
        .unwrap();
    assert!(
        finished.is_ok(),
        "disposable command attempted a locked Store read"
    );
    assert!(matches!(
        expired.await.unwrap().value(),
        Err(CommandError::Read(AgentReadError::Deadline))
    ));
}

#[tokio::test]
async fn agent_read_attachment_ready_owner_reply_polled_after_absolute_deadline_is_not_disclosed() {
    use qos_action_broker::{
        agent_reads::AgentReadCustody,
        peer_actions::{CarrierError, PeerActionQueue},
    };
    use std::{future::Future, task::Poll};
    let (dir, store, _) = crate::agent_owner_tests::enrolled();
    let evidence = certified(&store, ActionMessage::CatalogueRefresh { scope: None });
    drop(store);
    let mut runtime = CloudAgentRuntime::start(dir.path(), "agent-admission", true)
        .await
        .unwrap();
    let service = runtime.certified_commands();
    let (queue, mut requests) = PeerActionQueue::channel();
    let deadline = Instant::now() + Duration::from_millis(300);
    let pending = queue.submit_certified(evidence.clone(), deadline).unwrap();
    let request = requests.recv().await.unwrap();
    let custody = AgentReadCustody::from_request(&request);
    let mut parked = Box::pin(service.binding(evidence.clone(), deadline, Some(custody)));
    // Poll exactly once to enqueue the actual command; never spawn the waiter.
    std::future::poll_fn(|cx| {
        assert!(parked.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(request);
    drop(pending);
    // FIFO completion of a second actual owner command proves the first result
    // was sent and ready. This does not infer readiness from a sleep or clock.
    service
        .binding(
            evidence.clone(),
            Instant::now() + Duration::from_secs(5),
            None,
        )
        .await
        .unwrap();
    let mut held = Vec::new();
    for _ in 0..31 {
        held.push(
            queue
                .submit_certified(evidence.clone(), Instant::now() + Duration::from_secs(5))
                .unwrap(),
        );
    }
    assert!(matches!(
        queue.submit_certified(evidence.clone(), Instant::now() + Duration::from_secs(5)),
        Err(CarrierError::Capacity)
    ));
    tokio::time::sleep_until(deadline + Duration::from_millis(1)).await;
    assert!(Instant::now() > deadline);
    let late = parked.await;
    let late_is_deadline = matches!(late, Err(AgentReadError::Deadline));
    drop(late);
    assert!(
        queue
            .submit_certified(evidence, Instant::now() + Duration::from_secs(5))
            .is_ok(),
        "late result must release original custody after its value is dropped"
    );
    runtime.close().await.unwrap();
    drop(held);
    assert!(
        late_is_deadline,
        "a ready owner result must not be disclosed after its absolute deadline"
    );
}

#[tokio::test]
async fn cloud_policy_clock_real_command_keeps_request_origin_and_denies_retired_queue() {
    use qos_action_broker::{agent_reads::AgentReadCustody, peer_actions::PeerActionQueue};
    let (dir, store, _) = crate::agent_owner_tests::enrolled();
    let evidence = certified(&store, ActionMessage::CatalogueRefresh { scope: None });
    drop(store);
    let owner = Arc::new(RuntimeOwner::open(dir.path(), "agent-admission").unwrap());
    let source = owner.policy_context().clock().new_origin();
    let original = source.capture().unwrap();
    owner
        .policy_context()
        .bind_source(original.clone())
        .unwrap();
    let (queue, mut requests) = PeerActionQueue::channel();
    let deadline = Instant::now() + Duration::from_secs(5);
    let pending = queue
        .submit_certified_with_policy_time(evidence.clone(), deadline, original.clone())
        .unwrap();
    let request = requests.recv().await.unwrap();
    let custody = AgentReadCustody::from_request(&request);
    assert!(custody.policy_time().unwrap().same_origin(&original));
    let (reply, response) = oneshot::channel();
    let command = Command {
        policy_time: custody.policy_time().unwrap().clone(),
        evidence,
        kind: CommandKind::Action,
        deadline,
        custody: Some(custody),
        reply,
    };
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    source.invalidate();
    dispatch(
        command,
        dir.path(),
        &owner,
        None,
        None,
        true,
        &mut AgentWorkQueue::default(),
    )
    .await;
    let (reply, retained) = response.await.unwrap().into_parts();
    assert!(reply.is_err());
    assert!(retained.unwrap().policy_time().unwrap().now_ns().is_err());
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    drop(request);
    drop(pending);
}

#[tokio::test]
async fn cloud_policy_clock_legacy_runtime_start_rejects_late_source_upgrade() {
    let (dir, store, _) = crate::agent_owner_tests::enrolled();
    let version = store.state.version;
    drop(store);
    let mut runtime = CloudAgentRuntime::start(dir.path(), "agent-admission", true)
        .await
        .unwrap();
    let commands = runtime.certified_commands();
    let source = commands.policy_clock().new_origin();
    assert!(
        commands
            .bind_source_policy_time(source.capture().unwrap())
            .is_err(),
        "already started workers must never adopt a different origin"
    );
    runtime.close().await.unwrap();
    assert_eq!(
        Store::open(dir.path(), "agent-admission")
            .unwrap()
            .state
            .version,
        version
    );
}
