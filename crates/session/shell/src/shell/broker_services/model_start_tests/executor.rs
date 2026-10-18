//! A protected managed launch and real task subscription, using throwaway identity custody.
use super::NativeFixture;
use crate::shell::broker_services::NativeOwnerCommitService;
use std::sync::Arc;
use tilo_action_broker::services::WorkspaceMemberships;
use tilo_action_broker::services::{
    AuthenticatedApplicationLaunch, LauncherServices, ManagedApplicationIdentity,
};
use tilo_identity_host::native_agent::{ManagedNativeAgent, NativeAgentRequest};
use tilo_owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_relay_protocol::delegation::Delegation;

struct Actor {
    actor: ManagedNativeAgent,
    delegation: Delegation,
}
impl std::fmt::Debug for Actor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Actor")
            .field("certificate", self.actor.certificate())
            .finish_non_exhaustive()
    }
}
impl ManagedApplicationIdentity for Actor {
    fn identity(&self) -> Result<(tilo_user_identity::DeviceCert, Delegation, [u8; 64]), String> {
        Ok((
            self.actor.certificate().clone(),
            self.delegation.clone(),
            self.actor
                .join_proof_for(&self.delegation)
                .map_err(|e| e.to_string())?,
        ))
    }
}
struct Launch {
    identity: Arc<Actor>,
    protected: launcher::registration_gate::ProtectedModelLaunch,
}
impl LauncherServices for Launch {
    fn applications(&self) -> Vec<tilo_action_broker::services::LaunchableApplication> {
        Vec::new()
    }
    fn launch(
        &self,
        _: &str,
        _: &tilo_desktop_model::Workspace,
        _: Option<&str>,
        _: &tilo_action_broker::services::LaunchPlace,
    ) -> Result<(), String> {
        Err("fixture already owns its managed launch".into())
    }
    fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
        pid == std::process::id() && app == self.protected.app && room == self.protected.room
    }
    fn protected_model_launch(
        &self,
        pid: u32,
        app: &str,
        room: &str,
    ) -> Result<Option<launcher::registration_gate::ProtectedModelLaunch>, String> {
        Ok(self
            .attest_process(pid, app, room)
            .then(|| self.protected.clone()))
    }
    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        room: &str,
    ) -> Result<Option<AuthenticatedApplicationLaunch>, String> {
        Ok(self
            .attest_process(pid, app, room)
            .then(|| AuthenticatedApplicationLaunch {
                managed_task_agent: Some("scribe".into()),
                managed_identity: Some(self.identity.clone()),
                creation: None,
                continuation_instance: None,
                launch_ticket: None,
            }))
    }
}

/// The broker's view of the fixture's own tree and legacy journal root.
struct Authority {
    tree: crate::shell::state::WorkspaceTreeHandle,
    journal: std::path::PathBuf,
}
impl tilo_action_broker::services::WorkspaceAuthority for Authority {
    fn with_tree(
        &self,
        read: &mut dyn FnMut(&tilo_desktop_model::DesktopTree),
    ) -> Result<(), String> {
        read(&self.tree.read().desktop);
        Ok(())
    }
    fn legacy_journal_root(&self) -> Option<std::path::PathBuf> {
        Some(self.journal.clone())
    }
    fn announces_policy_changes(&self) -> bool {
        true
    }
}

pub(super) struct Executor {
    pub service: NativeOwnerCommitService,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    _broker: tilo_action_broker::collab_broker::BrokerHandle,
}
impl Drop for Executor {
    fn drop(&mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        self.thread.take().unwrap().join().unwrap();
    }
}

pub(super) fn start(fixture: &NativeFixture) -> Executor {
    let tree = crate::shell::native_workspace_owner::test_tree_of(&fixture.owner);
    tree.write().set_salt_override(2);
    let services = crate::shell::broker_services::native_services(Some(tree.clone()), None, None);
    let service = NativeOwnerCommitService {
        task_executors: services.native_task_executors.clone().unwrap(),
        ..NativeOwnerCommitService::new(tree.clone(), None)
    };
    admit_scribe(&service, fixture);
    let room = fixture.room.to_string();
    let identity = provision_scribe(fixture, &room);
    let (certificate, _, proof) = identity.identity().unwrap();
    let services = with_protected_launch(services, identity, &room);
    let broker = spawn_broker(fixture, tree, service.clone(), services);
    let (stop, thread) = subscribe_scribe(fixture, &room, &certificate, &proof);
    assert_subscription_ready(&service, fixture, &room);
    Executor {
        service,
        stop: Some(stop),
        thread: Some(thread),
        _broker: broker,
    }
}

fn admit_scribe(service: &NativeOwnerCommitService, fixture: &NativeFixture) {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(service.execute(OwnerCommitRequest {
            room: fixture.room.to_string(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: fixture.certificate.clone(),
                command:
                    tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::AddAgent {
                        agent: "scribe".into(),
                        preset: tilo_action_policy::roles::AgentPreset::Operator,
                    },
            },
        }))
        .unwrap();
}

/// Provision the throwaway agent and admit it as this workspace's operator.
fn provision_scribe(fixture: &NativeFixture, room: &str) -> Arc<Actor> {
    let (key, governor) = tilo_identity_host::broker::trusted_current_device(room).unwrap();
    let person_room = collab_types::person_room_of(&hex::encode(governor.user_public_key))
        .unwrap()
        .to_string();
    let actor = tilo_identity_host::native_agent::provision_retained(
        &person_room,
        &NativeAgentRequest {
            agent_id: "scribe".into(),
            label: "Scribe".into(),
            color: [10, 20, 30],
            apps: vec!["so.tilo.agent".into()],
            expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
        },
    )
    .unwrap();
    let delegation = actor.delegate_room(room).unwrap();
    fixture
        .owner
        .admit_managed_agent(
            fixture.room,
            actor.certificate().clone(),
            delegation.clone(),
            &governor,
            &key,
            std::time::SystemTime::now(),
        )
        .unwrap();
    assert!(!actor.certificate().capabilities.can_act_as_host);
    Arc::new(Actor { actor, delegation })
}

fn with_protected_launch(
    services: tilo_action_broker::services::HostServices,
    identity: Arc<Actor>,
    room: &str,
) -> tilo_action_broker::services::HostServices {
    let launch = Arc::new(Launch {
        identity,
        protected: launcher::registration_gate::ProtectedModelLaunch {
            app: "so.tilo.agent".into(),
            room: room.to_string(),
            agent: "scribe".into(),
            launch: uuid::Uuid::new_v4().to_string(),
        },
    });
    tilo_action_broker::services::HostServices {
        launcher: Some(launch),
        ..services
    }
}

fn spawn_broker(
    fixture: &NativeFixture,
    tree: crate::shell::state::WorkspaceTreeHandle,
    service: NativeOwnerCommitService,
    services: tilo_action_broker::services::HostServices,
) -> tilo_action_broker::collab_broker::BrokerHandle {
    tilo_action_broker::collab_broker::spawn_with_services(
        fixture.home.runtime().join("model-executor.sock"),
        Some(Arc::new(Authority {
            tree,
            journal: fixture.home.config().join("model-executor-journal"),
        })),
        tilo_action_broker::collab_outbound::PeerPublisher::with_optional_owner_commit(
            None,
            Arc::new(service),
        ),
        None,
        services,
        Arc::default(),
        Arc::default(),
    )
    .unwrap()
}

/// Join the protected agent channel on its own thread and hold the subscription open.
fn subscribe_scribe(
    fixture: &NativeFixture,
    room: &str,
    certificate: &tilo_user_identity::DeviceCert,
    proof: &[u8; 64],
) -> (
    tokio::sync::oneshot::Sender<()>,
    std::thread::JoinHandle<()>,
) {
    let route = tilo_action_broker::route::CrdtRoute::record("so.tilo.agent", room, "main");
    let mut stream =
        std::os::unix::net::UnixStream::connect(fixture.home.runtime().join("model-executor.sock"))
            .unwrap();
    launcher::services::write_collab_handoff(&mut stream, "so.tilo.agent", room).unwrap();
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: room.to_string(),
        device_certificate: serde_json::to_vec(certificate).unwrap(),
        proof: proof.to_vec(),
    }
    .encode()
    .unwrap();
    let (ready, wait) = std::sync::mpsc::sync_channel(1);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let thread = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(tokio::task::LocalSet::new().run_until(async move {
                stream.set_nonblocking(true).unwrap();
                let stream = tokio::net::UnixStream::from_std(stream).unwrap();
                let client = runtime_collab::CollabClient::from_raw(
                    runtime_portal::CapnpClient::from_connected_io(
                        stream,
                        capnp_rpc::rpc_twoparty_capnp::Side::Client,
                    )
                    .bootstrap(),
                );
                let channel = client
                    .join_channel_as_kind(
                        &route.channel_key(),
                        &presentation,
                        "scribe",
                        "agent",
                        "",
                    )
                    .await
                    .unwrap();
                let runs = channel.subscribe_task_runs().await.unwrap();
                ready.send(()).unwrap();
                stopped.await.unwrap();
                drop(runs);
                drop(channel);
            }));
    });
    wait.recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    (stop, thread)
}

fn assert_subscription_ready(
    service: &NativeOwnerCommitService,
    fixture: &NativeFixture,
    room: &str,
) {
    let compute = service
        .current_task_compute(room, &fixture.certificate)
        .unwrap()
        .unwrap();
    assert!(
        compute.iter().any(|entry| entry.agent_id == "scribe"
            && entry.current_certified_executor
            && entry.model_ready
            && entry.permitted),
        "real subscription and selected model must be ready"
    );
}
