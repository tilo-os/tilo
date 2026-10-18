//! Two owner-certified native replicas use the authenticated private QUIC door.
//! HTTP is a real loopback peer; only the host's connection/lease adapter and
//! HTTP framing are fixtures, not the governor's MODEL admission or dispatch.
//! This deliberately does not exercise the broker launch RPC or model gateway.
use super::*;
use ed25519_dalek::Signer;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_joined_room::task_admission::model::{
    FrozenNativeModelSelection, HostDispatchModel, NativeModelAuthority, NativeModelContext,
    NativeModelContract, NativeModelHttpAttempt, NativeModelPlanning, NativeModelStartAuthority,
    TypedNativeModelRequirement,
};
use tilo_joined_room::task_admission::model_host::{
    NativeModelConnection, NativeModelHostRegistration,
};
use tilo_joined_room::task_admission::model_start::NativeModelPersonStartSnapshot;
use tilo_joined_room::task_admission::{
    self, CommitOperation, NativeAdmissionCommand as C, NativeAdmissionReply as R,
    NativeInvocationPermit, OwnerCommitRequest,
};
use tilo_joined_room::task_policy::NativeTaskBinding;
use tilo_peer_state::workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand as T};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PRIVATE_GOAL: &str = "Keep this draft private";

/// Only the pinned governor can write policy in the desktop task namespace.
/// An Observer's general preset deliberately does not authorize outside effects.
async fn model_permission(
    rooms: &Rooms,
    room: &str,
    task: &str,
    governor_key: &SigningKey,
    agent_answer: tilo_action_policy::Answer,
) {
    use tilo_action_policy::{Answer, Grant, KindSet, Subject};
    let state = rooms.room(room).await.unwrap();
    let mut held = state.lock().await;
    let mut candidate = held.peer().clone();
    let now = SystemTime::now();
    let governor = candidate.authority();
    let person = candidate
        .permissions(governor.device_public_key, now)
        .unwrap()
        .person;
    let node = NativeModelContract::task_resource(room, task).action("requestModel");
    let grants = [
        ("starter", Subject::person(person.clone()), Answer::Allow),
        ("agent", Subject::agent("scribe"), agent_answer),
    ]
    .into_iter()
    .map(|(name, subject, answer)| {
        Grant::new(
            format!("remote-model-{task}-{name}"),
            subject,
            node.clone(),
            KindSet::OUTSIDE,
            answer,
            Subject::person(person.clone()),
        )
    })
    .collect::<Vec<_>>();
    candidate
        .governance_mut()
        .put_governed_grants(&grants, governor_key, now)
        .unwrap();
    let collaboration = held.collaboration().clone();
    held.commit(candidate, governor_key, now, |_, _| Ok(collaboration))
        .unwrap();
}

struct Selection(NativeModelContract);
impl NativeModelStartAuthority for Selection {
    fn approved_model_start(
        &self,
        _: &tilo_joined_room::NativeCommittedView,
        _: &str,
        _: &DeviceCert,
        _: SystemTime,
    ) -> Result<NativeModelContract, String> {
        Ok(self.0.clone())
    }
}

/// Host-held live lease, populated from the governor's authenticated private
/// digest and contract reads, independently of each request's wire requirement.
struct Lease {
    context: NativeModelContext,
    connected: AtomicBool,
}
impl NativeModelAuthority for Lease {
    fn current_model_context(
        &self,
        _: &tilo_joined_room::NativeCommittedView,
        _: SystemTime,
    ) -> Result<NativeModelContext, String> {
        if !self.connected.load(Ordering::SeqCst) {
            return Err("native connection replaced".into());
        }
        Ok(self.context.clone())
    }
}

fn host(seed: u8) -> (SigningKey, DeviceCert) {
    // The actual room owner's root, not a self-signed foreign user identity.
    let root = SigningKey::from_bytes(&[1; 32]);
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("Native model host {seed}"),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    cert.capabilities.can_act_as_host = true;
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

async fn command(
    native: &NativeEditor,
    address: &iroh::EndpointAddr,
    room: &str,
    command: C,
) -> Option<R> {
    match rpc(
        &native.client,
        address,
        room,
        PrivateOperation::NativeTask {
            payload: task_admission::encode_admission_command(&command).unwrap(),
        },
    )
    .await
    {
        PrivateReply::NativeTask { payload } => {
            Some(task_admission::decode_admission_reply(&payload).unwrap())
        }
        PrivateReply::Refused => None,
        other => panic!("unexpected native custody reply: {other:?}"),
    }
}

async fn coordinate(native: &NativeEditor, address: &iroh::EndpointAddr, room: &str, command_: T) {
    assert!(matches!(
        command(
            native,
            address,
            room,
            C::PrivateCoordination {
                invocation: uuid::Uuid::new_v4(),
                command: command_,
            }
        )
        .await,
        Some(R::Coordination(_))
    ));
}

async fn lease(
    native: &NativeEditor,
    address: &iroh::EndpointAddr,
    room: &str,
    task: &str,
) -> Lease {
    let id = tilo_desktop_model::RoomId::parse(room).unwrap();
    let state = native.owner.workspace_task_snapshot(id).unwrap();
    let control = &state.controls[task];
    let reference = tilo_peer_state::workspace_task::private_instruction_reference(PRIVATE_GOAL);
    assert_eq!(
        control.private_instruction_ref,
        Some(reference),
        "the joined sanitized baseline must bind the governor's private goal"
    );
    let run = control.active_run.as_ref().unwrap();
    let read = |operation| PrivateOperation::NativeTaskRead {
        task: task.into(),
        epoch: run.epoch,
        agent: run.agent_id.clone(),
        evidence: Vec::new(),
        operation: Box::new(operation),
    };
    let PrivateReply::TaskRunDigest(digest) = rpc(
        &native.client,
        address,
        room,
        read(PrivateOperation::TaskRunDigest {
            task: task.into(),
            epoch: run.epoch,
            revision: run.applied_direction_revision,
        }),
    )
    .await
    else {
        panic!("native private digest refused")
    };
    let PrivateReply::TaskModelContract(contract) = rpc(
        &native.client,
        address,
        room,
        read(PrivateOperation::TaskModelContract {
            task: task.into(),
            epoch: run.epoch,
            revision: run.applied_direction_revision,
        }),
    )
    .await
    else {
        panic!("native private contract refused")
    };
    let instructions = rpc(
        &native.client,
        address,
        room,
        read(PrivateOperation::TaskInstructions {
            task: task.into(),
            epoch: run.epoch,
        }),
    )
    .await;
    let PrivateReply::TaskInstructions(encoded) = instructions else {
        panic!("authenticated instructions refused before MODEL enrollment: {instructions:?}")
    };
    let private: tilo_joined_room::NativePrivateTaskInstructions =
        serde_json::from_str(&encoded).unwrap();
    assert_eq!(private.instruction.as_deref(), Some(PRIVATE_GOAL));
    assert_eq!(
        tilo_joined_room::task_admission::private_run::private_run_digest(
            id,
            &state,
            &NativeTaskBinding {
                task_id: task.into(),
                agent_id: run.agent_id.clone(),
                epoch: run.epoch,
                revision: run.applied_direction_revision,
                starter: Some(run.approval.as_ref().unwrap().person.clone()),
            },
            &private,
        )
        .expect("joined baseline and authenticated private instructions must describe one run"),
        digest,
        "host baseline digest must match the governor's authenticated run digest"
    );
    Lease {
        context: NativeModelContext {
            actor: run.agent_id.clone(),
            task: task.into(),
            epoch: run.epoch,
            revision: run.applied_direction_revision,
            planning: NativeModelPlanning::Model,
            private_run_digest: digest,
            contract: serde_json::from_str(&contract).unwrap(),
        },
        connected: AtomicBool::new(true),
    }
}

fn register(
    native: &NativeEditor,
    room: &str,
    key: &SigningKey,
    lease: &Lease,
) -> NativeModelHostRegistration {
    NativeModelHostRegistration::capture(
        &native.owner,
        tilo_desktop_model::RoomId::parse(room).unwrap(),
        NativeModelConnection {
            connection: uuid::Uuid::new_v4(),
            lease: uuid::Uuid::new_v4(),
        },
        lease,
        key,
    )
    .unwrap()
}

fn requirement(
    native: &NativeEditor,
    room: &str,
    lease: &Lease,
    body: &[u8],
) -> TypedNativeModelRequirement {
    let id = tilo_desktop_model::RoomId::parse(room).unwrap();
    let state = native.owner.workspace_task_snapshot(id).unwrap();
    let run = state.controls[&lease.context.task]
        .active_run
        .as_ref()
        .unwrap();
    let host = native
        .owner
        .with_current(id, |view| Ok(view.device().device_public_key))
        .unwrap();
    TypedNativeModelRequirement::freeze(
        TypedNativeModelRequirement {
            room: id,
            run: NativeTaskBinding {
                task_id: lease.context.task.clone(),
                agent_id: run.agent_id.clone(),
                epoch: run.epoch,
                revision: run.applied_direction_revision,
                starter: run
                    .approval
                    .as_ref()
                    .map(|approval| approval.person.clone()),
            },
            invocation: uuid::Uuid::new_v4(),
            request: uuid::Uuid::new_v4(),
            host,
            private_run_digest: lease.context.private_run_digest,
            modelselection: lease.context.contract.clone(),
            requestdigest: [0; 32],
        },
        body,
    )
    .unwrap()
}

async fn claim(
    rooms: &Rooms,
    native: &NativeEditor,
    address: &iroh::EndpointAddr,
    registration: &NativeModelHostRegistration,
    need: &TypedNativeModelRequirement,
) -> NativeInvocationPermit {
    let room = need.room.to_string();
    // Diagnose policy/enrollment failures before the private wire intentionally
    // collapses their details into Refused. This never issues an extra permit.
    {
        let caller = native
            .owner
            .with_current(need.room, |view| Ok(view.device().clone()))
            .unwrap();
        let state = rooms.room(&room).await.unwrap();
        let held = state.lock().await;
        let current = held
            .load_native_admission()
            .unwrap()
            .check_model_host(registration.id(), need, &caller)
            .expect("MODEL source must be independently enrolled before reservation");
        need.check_registered_context(
            held.peer(),
            &held.task_policy_snapshot().unwrap(),
            &need.binding().unwrap(),
            &current,
            SystemTime::now(),
        )
        .unwrap_or_else(|error| panic!("governor MODEL reservation preflight: {error}"));
    }
    let Some(R::Reserved(reservation)) = command(
        native,
        address,
        &room,
        C::ReserveModel {
            registration: registration.id(),
            requirement: need.clone(),
        },
    )
    .await
    else {
        panic!("authenticated MODEL reservation refused")
    };
    let mut changed = need.clone();
    changed.request = uuid::Uuid::new_v4();
    assert!(
        command(
            native,
            address,
            &room,
            C::ClaimModel {
                registration: registration.id(),
                requirement: changed,
                reservation: reservation.clone(),
            }
        )
        .await
        .is_none(),
        "claim must bind the exact reserved requirement"
    );
    let Some(R::Claimed(permit)) = command(
        native,
        address,
        &room,
        C::ClaimModel {
            registration: registration.id(),
            requirement: need.clone(),
            reservation,
        },
    )
    .await
    else {
        panic!("authenticated MODEL claim refused")
    };
    permit
}

/// Sign adversarial wire statements using a real enrolled host's key. This
/// distinguishes custody/frontier rejection from mere signature rejection.
fn resign(mut wire: serde_json::Value, key: &SigningKey) -> NativeModelHostRegistration {
    #[derive(serde::Deserialize)]
    struct Statement {
        id: uuid::Uuid,
        room: tilo_desktop_model::RoomId,
        host: [u8; 32],
        connection: NativeModelConnection,
        context: NativeModelContext,
        issued_at_ns: u64,
        expires_at_ns: u64,
    }
    let s: Statement = serde_json::from_value(wire.clone()).unwrap();
    let bytes = postcard::to_stdvec(&(
        "tilo-native-model-host-v1",
        (
            s.id,
            s.room,
            s.host,
            s.connection,
            s.context,
            s.issued_at_ns,
            s.expires_at_ns,
        ),
    ))
    .unwrap();
    let signed = key.sign(&bytes).to_bytes();
    let mut signature = [[0; 32]; 2];
    signature[0].copy_from_slice(&signed[..32]);
    signature[1].copy_from_slice(&signed[32..]);
    wire["signature"] = serde_json::to_value(signature).unwrap();
    serde_json::from_value(wire).unwrap()
}

/// Genuinely expired, validly signed enrollment: no sleeping and no fake clock
/// in the governor admission RPC.
fn expired(
    registration: &NativeModelHostRegistration,
    key: &SigningKey,
) -> NativeModelHostRegistration {
    let mut wire = serde_json::to_value(registration).unwrap();
    wire["issued_at_ns"] = serde_json::json!(1_000_000_000_u64);
    wire["expires_at_ns"] = serde_json::json!(121_000_000_000_u64);
    resign(wire, key)
}

async fn http_peer(
    listener: tokio::net::TcpListener,
    expected: Vec<Vec<u8>>,
) -> tokio::net::TcpListener {
    for expected in expected {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(socket.read_u8().await.unwrap());
            assert!(head.len() < 8192);
        }
        let head = String::from_utf8(head).unwrap();
        assert!(head.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        let length: usize = head
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(length, expected.len());
        let mut body = vec![0; length];
        socket.read_exact(&mut body).await.unwrap();
        assert_eq!(body, expected, "the exact frozen bytes must reach HTTP");
        let reply = b"{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"done\"}}]}";
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    reply.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.write_all(reply).await.unwrap();
    }
    listener
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_authenticated_native_hosts_dispatch_exact_model_http_and_persist_stop() {
    let directory = tempfile::tempdir().unwrap();
    let (governor_key, governor) = tests::governor();
    let relay = tests::view_read_relay(directory.path(), &governor_key, &governor).await;
    let rooms = relay.rooms().unwrap().clone();
    let room = rooms
        .create("Remote native model custody")
        .await
        .unwrap()
        .room;
    let id = tilo_desktop_model::RoomId::parse(&room).unwrap();
    // Task-agent membership goes through the actual governor reducer, not a
    // hand-written Automerge control/run or a fabricated certificate list.
    {
        let state = rooms.room(&room).await.unwrap();
        let mut held = state.lock().await;
        let mut candidate = held.peer().clone();
        candidate
            .workspace_mut()
            .apply_workspace_membership_command(
                &governor,
                &tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::AddAgent {
                    agent: "scribe".into(),
                    preset: tilo_action_policy::roles::AgentPreset::Observer,
                },
                &governor_key,
                SystemTime::now(),
            )
            .unwrap();
        let collaboration = held.collaboration().clone();
        held.commit(candidate, &governor_key, SystemTime::now(), |_, _| {
            Ok(collaboration)
        })
        .unwrap();
    }
    let server = endpoint(&governor_key).await;
    let router = iroh::protocol::Router::builder(server.clone())
        .accept(PRIVATE_ALPN, PrivateProtocol::new(rooms.clone()))
        .spawn();
    let address = server.addr();
    let (key_a, cert_a) = host(71);
    let (key_b, cert_b) = host(72);
    assert_eq!(cert_a.user_public_key, governor.user_public_key);
    assert_eq!(cert_b.user_public_key, governor.user_public_key);
    assert_ne!(cert_a.device_public_key, cert_b.device_public_key);
    assert_ne!(cert_a.device_public_key, governor.device_public_key);
    assert_ne!(cert_b.device_public_key, governor.device_public_key);
    let starter = NativeEditor::join(&rooms, &room, &governor, &key_a, &cert_a).await;
    coordinate(
        &starter,
        &address,
        &room,
        T::QueueInstruction {
            title: "Review a private draft".into(),
            instruction: PRIVATE_GOAL.into(),
            assigned_agent: "scribe".into(),
            route: tilo_desktop_model::WorkspaceTaskRoute::Model,
        },
    )
    .await;
    let task = rooms
        .room(&room)
        .await
        .unwrap()
        .lock()
        .await
        .task_policy_snapshot()
        .unwrap()
        .tasks[0]
        .id
        .clone();
    model_permission(
        &rooms,
        &room,
        &task,
        &governor_key,
        tilo_action_policy::Answer::Allow,
    )
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http = listener.local_addr().unwrap();
    let selection = Selection(NativeModelContract::for_task(
        &room,
        &task,
        tilo_action_policy::ActionKind::Outside,
        FrozenNativeModelSelection {
            app: "so.tilo.agent".into(),
            provider: "remote-loopback".into(),
            model: "selected".into(),
            contract: serde_json::json!({"endpoint": format!("http://{http}/v1"), "protocol": "openai"}),
        },
    ));
    let invocation = uuid::Uuid::new_v4();
    let approval = T::ApproveRun {
        task_id: task.clone(),
        agent_id: "scribe".into(),
        expected_run_epoch: 0,
        after_revision: 0,
    };
    let request = OwnerCommitRequest {
        room: room.clone(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation,
            authority: WorkspaceTaskAuthority::Person {
                caller: cert_a.clone(),
            },
            command: approval.clone(),
        },
    };
    let snapshot = NativeModelPersonStartSnapshot::capture(
        &starter.owner,
        &request,
        &selection,
        &key_a,
        SystemTime::now(),
    )
    .unwrap();
    assert!(matches!(
        command(
            &starter,
            &address,
            &room,
            C::PersonModelStart {
                invocation,
                command: approval,
                snapshot,
            }
        )
        .await,
        Some(R::Coordination(_))
    ));
    starter.endpoint.close().await;
    drop(starter);
    // Fresh, independently invited native stores observe the committed run.
    let a = NativeEditor::join(&rooms, &room, &governor, &key_a, &cert_a).await;
    let b = NativeEditor::join(&rooms, &room, &governor, &key_b, &cert_b).await;
    assert_ne!(a.directory.path(), b.directory.path());
    assert_ne!(a.endpoint.id(), b.endpoint.id());
    for native in [&a, &b] {
        native
            .owner
            .with_current(id, |view| {
                assert!(view.device().capabilities.can_act_as_host);
                assert_eq!(view.governor(), &governor);
                Ok(())
            })
            .unwrap();
    }
    let lease_a = lease(&a, &address, &room, &task).await;
    let lease_b = lease(&b, &address, &room, &task).await;
    assert_eq!(lease_a.context, lease_b.context);
    let reg_a = register(&a, &room, &key_a, &lease_a);
    let reg_b = register(&b, &room, &key_b, &lease_b);
    for (native, reg) in [(&a, &reg_a), (&b, &reg_b)] {
        assert!(matches!(
            command(native, &address, &room, C::RegisterModelHost(reg.clone())).await,
            Some(R::Acknowledged)
        ));
    }
    let bodies = [
        br#"{"model":"selected","messages":[{"role":"user","content":"Plan the review"}],"max_tokens":16}"#.to_vec(),
        br#"{"model":"selected","messages":[{"role":"user","content":"Complete the review"}],"max_tokens":16}"#.to_vec(),
    ];
    // Enrollment and a still-live host lease do not override current policy.
    // Keep the replicas stale on purpose: the governor must reject independently.
    model_permission(
        &rooms,
        &room,
        &task,
        &governor_key,
        tilo_action_policy::Answer::Deny,
    )
    .await;
    for (native, lease, registration) in [(&a, &lease_a, &reg_a), (&b, &lease_b, &reg_b)] {
        assert!(
            command(
                native,
                &address,
                &room,
                C::ReserveModel {
                    registration: registration.id(),
                    requirement: requirement(native, &room, lease, &bodies[0]),
                },
            )
            .await
            .is_none(),
            "a live enrolled host must not bypass a denied requestModel action"
        );
    }
    model_permission(
        &rooms,
        &room,
        &task,
        &governor_key,
        tilo_action_policy::Answer::Allow,
    )
    .await;
    let peer = tokio::spawn(http_peer(listener, bodies.to_vec()));
    for ((native, lease, registration, key), body) in [
        (&a, &lease_a, &reg_a, &key_a),
        (&b, &lease_b, &reg_b, &key_b),
    ]
    .into_iter()
    .zip(&bodies)
    {
        let need = requirement(native, &room, lease, body);
        let permit = claim(&rooms, native, &address, registration, &need).await;
        let gate = HostDispatchModel {
            owner: &native.owner,
            authority: lease,
        };
        let socket = gate.dispatch(&need, &permit, NativeModelHttpAttempt {
            governor: &governor, outbound_body: body,
        }, |bytes| {
            let mut socket = std::net::TcpStream::connect(http).map_err(|e| e.to_string())?;
            write!(socket, "POST /v1/chat/completions HTTP/1.1\r\nHost: {http}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).map_err(|e| e.to_string())?;
            socket.write_all(bytes).map_err(|e| e.to_string())?;
            socket.set_nonblocking(true).map_err(|e| e.to_string())?;
            Ok(socket)
        }).unwrap();
        let mut socket = tokio::net::TcpStream::from_std(socket).unwrap();
        let mut response = Vec::new();
        tilo_test_env::within(
            "native model HTTP response",
            socket.read_to_end(&mut response),
        )
        .await
        .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200 OK"));
        assert!(String::from_utf8_lossy(&response).contains("done"));
        let completion = native
            .owner
            .complete_native_invocation(
                id,
                need.invocation,
                *blake3::hash(&response).as_bytes(),
                key,
            )
            .unwrap();
        assert!(matches!(
            command(native, &address, &room, C::Complete(completion)).await,
            Some(R::Acknowledged)
        ));
        // Persisted host intent, not an HTTP counter, prevents duplicate sends.
        let reopened =
            tilo_joined_room::open_file(&native.directory.path().join("joined.json")).unwrap();
        let replay = HostDispatchModel {
            owner: &reopened,
            authority: lease,
        };
        replay
            .dispatch(
                &need,
                &permit,
                NativeModelHttpAttempt {
                    governor: &governor,
                    outbound_body: body,
                },
                |_| -> Result<(), String> {
                    panic!("a completed invocation must not enqueue again after restart")
                },
            )
            .unwrap_err();
    }
    let listener = tilo_test_env::within("both remote model HTTP posts", peer)
        .await
        .unwrap();

    // Unknown enrollment, foreign authenticated host, changed profile/task and
    // signed expiry all fail on the actual governor private-command path.
    let need = requirement(&a, &room, &lease_a, &bodies[0]);
    for (native, registration, requirement_) in [
        (&a, uuid::Uuid::new_v4(), need.clone()),
        (&b, reg_a.id(), need.clone()),
        (&a, reg_a.id(), {
            let mut n = need.clone();
            n.private_run_digest = [0x55; 32];
            n
        }),
        (&a, reg_a.id(), {
            let mut n = need.clone();
            n.run.epoch = n.run.epoch.checked_add(1).unwrap();
            n
        }),
        (&a, reg_a.id(), {
            let mut n = need.clone();
            n.run.revision = n.run.revision.checked_add(1).unwrap();
            n
        }),
        (&a, reg_a.id(), {
            let mut n = need.clone();
            n.modelselection.selection.provider = "other-profile".into();
            n
        }),
        (&a, reg_a.id(), {
            let mut n = need.clone();
            n.run.task_id = "forged-task".into();
            n
        }),
        (&a, reg_a.id(), {
            let mut n = need.clone();
            n.modelselection.selection.contract =
                serde_json::json!({"endpoint":"http://127.0.0.1:1"});
            n
        }),
    ] {
        assert!(
            command(
                native,
                &address,
                &room,
                C::ReserveModel {
                    registration,
                    requirement: requirement_
                }
            )
            .await
            .is_none()
        );
    }
    assert!(
        command(
            &a,
            &address,
            &room,
            C::RegisterModelHost(expired(&reg_a, &key_a))
        )
        .await
        .is_none()
    );
    for (field, value) in [("actor", "forged-agent"), ("task", "forged-task")] {
        let mut forged = serde_json::to_value(&reg_a).unwrap();
        forged["context"][field] = serde_json::json!(value);
        // A signature alone is not proof that a host has that task/agent lease.
        assert!(
            command(
                &a,
                &address,
                &room,
                C::RegisterModelHost(resign(forged, &key_a))
            )
            .await
            .is_none()
        );
    }
    let mut forged = serde_json::to_value(&reg_a).unwrap();
    forged["context"]["private_run_digest"] = serde_json::to_value([0x55_u8; 32]).unwrap();
    assert!(
        command(
            &a,
            &address,
            &room,
            C::RegisterModelHost(resign(forged, &key_a))
        )
        .await
        .is_none(),
        "even a certified host cannot enroll a different private run digest"
    );
    let mut forged = serde_json::to_value(&reg_a).unwrap();
    forged["context"]["contract"]["selection"]["model"] = serde_json::json!("tampered-model");
    assert!(
        command(
            &a,
            &address,
            &room,
            C::RegisterModelHost(serde_json::from_value(forged).unwrap())
        )
        .await
        .is_none()
    );
    assert!(
        command(&b, &address, &room, C::RegisterModelHost(reg_a.clone()))
            .await
            .is_none()
    );

    let stopped_need = requirement(&b, &room, &lease_b, &bodies[1]);
    let queued = claim(&rooms, &b, &address, &reg_b, &stopped_need).await;
    let queued_need = requirement(&a, &room, &lease_a, &bodies[0]);
    let queued_a = claim(&rooms, &a, &address, &reg_a, &queued_need).await;
    HostDispatchModel {
        owner: &a.owner,
        authority: &lease_a,
    }
    .dispatch(
        &queued_need,
        &queued_a,
        NativeModelHttpAttempt {
            governor: &governor,
            outbound_body: b"changed after claim",
        },
        |_| -> Result<(), String> { panic!("changed final bytes must enqueue zero HTTP requests") },
    )
    .unwrap_err();
    lease_a.connected.store(false, Ordering::SeqCst);
    HostDispatchModel {
        owner: &a.owner,
        authority: &lease_a,
    }
    .dispatch(
        &queued_need,
        &queued_a,
        NativeModelHttpAttempt {
            governor: &governor,
            outbound_body: &bodies[0],
        },
        |_| -> Result<(), String> {
            panic!("a replaced connection must enqueue zero HTTP requests")
        },
    )
    .unwrap_err();
    lease_a.connected.store(true, Ordering::SeqCst);
    assert!(matches!(
        command(
            &a,
            &address,
            &room,
            C::RevokeModelHost {
                registration: reg_a.id()
            }
        )
        .await,
        Some(R::Acknowledged)
    ));
    assert!(
        command(
            &a,
            &address,
            &room,
            C::ReserveModel {
                registration: reg_a.id(),
                requirement: need.clone()
            }
        )
        .await
        .is_none()
    );
    {
        let state = rooms.room(&room).await.unwrap();
        let held = state.lock().await;
        let reopened = GovernedRoom::open(held.path.clone(), &governor, &governor_key).unwrap();
        let ledger = reopened.load_native_admission().unwrap();
        assert_eq!(
            ledger.state(&task, lease_a.context.epoch),
            task_admission::NativeTaskAdmissionState::Open
        );
        assert!(
            ledger.check_model_host(reg_a.id(), &need, &cert_a).is_err(),
            "revocation must survive governor restart without stopping the run"
        );
        ledger
            .check_model_host(reg_b.id(), &stopped_need, &cert_b)
            .unwrap();
    }
    coordinate(
        &a,
        &address,
        &room,
        T::CancelForRun {
            task_id: task.clone(),
            run_epoch: lease_a.context.epoch,
            after_revision: lease_a.context.revision,
        },
    )
    .await;
    for (native, key, certificate) in [(&a, &key_a, &cert_a), (&b, &key_b, &cert_b)] {
        let Some(R::HostFences(fences)) =
            command(native, &address, &room, C::PendingHostFences).await
        else {
            panic!("host fences absent")
        };
        assert_eq!(fences.len(), 1);
        assert_eq!(
            fences[0].hosts,
            std::collections::BTreeSet::from([cert_a.device_public_key, cert_b.device_public_key])
        );
        let ack = native
            .owner
            .acknowledge_native_host_fence(&fences[0], &governor, key)
            .unwrap();
        assert_eq!(ack.host, certificate.device_public_key);
        assert!(matches!(
            command(native, &address, &room, C::FenceAcknowledgement(ack)).await,
            Some(R::Acknowledged)
        ));
    }
    // The fence survives reopening a real host store and blocks a permit which
    // was claimed before Stop, even when its local public replica is stale.
    let reopened = tilo_joined_room::open_file(&b.directory.path().join("joined.json")).unwrap();
    HostDispatchModel {
        owner: &reopened,
        authority: &lease_b,
    }
    .dispatch(
        &stopped_need,
        &queued,
        NativeModelHttpAttempt {
            governor: &governor,
            outbound_body: &bodies[1],
        },
        |_| -> Result<(), String> {
            panic!("a stopped queued permit must enqueue zero HTTP requests")
        },
    )
    .unwrap_err();
    assert!(matches!(
        command(
            &b,
            &address,
            &room,
            C::State {
                task: task.clone(),
                epoch: lease_b.context.epoch,
            }
        )
        .await,
        Some(R::State(task_admission::NativeTaskAdmissionState::Stopped))
    ));
    // Reopen the governor's durable room rather than reserializing a test ledger.
    {
        let state = rooms.room(&room).await.unwrap();
        let held = state.lock().await;
        let reopened = GovernedRoom::open(held.path.clone(), &governor, &governor_key).unwrap();
        let ledger = reopened.load_native_admission().unwrap();
        assert!(ledger.check_model_host(reg_a.id(), &need, &cert_a).is_err());
        assert!(
            ledger
                .check_model_host(reg_b.id(), &stopped_need, &cert_b)
                .is_err()
        );
        assert_eq!(
            ledger.state(&task, lease_b.context.epoch),
            task_admission::NativeTaskAdmissionState::Stopped
        );
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "rejected attempts must not make another HTTP connection"
    );
    a.endpoint.close().await;
    b.endpoint.close().await;
    router.shutdown().await.unwrap();
    relay.shutdown().await;
}
