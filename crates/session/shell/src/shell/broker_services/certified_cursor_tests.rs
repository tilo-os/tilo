//! Certified cursor provenance and private expansion through authenticated app writers.
use super::{NativeFixture, fixture};
use crate::shell::broker_services::NativeOwnerCommitService;
use ed25519_dalek::Signer as _;
use futures_util::future::LocalBoxFuture;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_action_broker::services::{
    AuthenticatedApplicationLaunch, LauncherServices, ManagedApplicationIdentity,
    WorkspaceMemberships,
};
use tilo_identity_host::native_agent::{ManagedNativeAgent, NativeAgentRequest};
use tilo_owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand};
use tilo_relay_protocol::delegation::Delegation;

// Reuse the protected MODEL executor, including its actual task subscription.
// It never sends a model request to the configured endpoint.
use super::executor;

#[path = "certified_cursor_render_tests.rs"]
mod render;

const NOTES: &str = "so.tilo.notes";
const CHAT: &str = "so.tilo.chat";
const RESOURCE: &str = "notes:release";

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
    room: String,
    actor: Arc<Actor>,
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
        Err("the fixture already owns the application launches".into())
    }
    fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
        pid == std::process::id()
            && room == self.room
            && matches!(app, NOTES | CHAT | "so.tilo.agent")
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
                managed_task_agent: (app == "so.tilo.agent").then(|| "scribe".into()),
                managed_identity: (app == "so.tilo.agent")
                    .then(|| self.actor.clone() as Arc<dyn ManagedApplicationIdentity>),
                creation: None,
                continuation_instance: None,
                launch_ticket: None,
            }))
    }
    fn protected_model_launch(
        &self,
        pid: u32,
        app: &str,
        room: &str,
    ) -> Result<Option<launcher::registration_gate::ProtectedModelLaunch>, String> {
        Ok(
            (app == "so.tilo.agent" && self.attest_process(pid, app, room))
                .then(|| self.protected.clone()),
        )
    }
}
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

/// Keeps the native owner, broker, authenticated writer and original run alive.
/// UI tests belong in child modules here so they can use the same broker evidence.
pub(super) struct CertifiedCursorFixture {
    pub(super) actions: tilo_action_broker::collab_broker::AppActionBroker,
    pub(super) interaction: Option<tilo_actions::SemanticInteraction>,
    pub(super) task_a: String,
    pub(super) task_b: Option<String>,
    broker: tilo_action_broker::collab_broker::BrokerHandle,
    executor: executor::Executor,
    _settings: crate::shell::test_support::InstalledSettings,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    thread: Option<std::thread::JoinHandle<()>>,
    captured: Arc<Mutex<Vec<tilo_actions::SemanticInteraction>>>,
    adopted: Arc<Mutex<Option<runtime_collab::private_draft::Draft>>>,
    _subscription: Arc<tilo_action_broker::collab_broker::RoomSubscription>,
    pub(super) native: NativeFixture,
}
enum Command {
    Invoke(std::sync::mpsc::SyncSender<()>, u64),
    Finish(std::sync::mpsc::SyncSender<()>, String),
    Stop,
}
impl CertifiedCursorFixture {
    pub(super) fn new() -> Self {
        let native = fixture();
        let settings = cursor_settings();
        let executor = executor::start(&native);
        let tree = crate::shell::native_workspace_owner::test_tree_of(&native.owner);
        let room = native.room.to_string();
        let identity = provision_identity(&native, &room);
        let services = cursor_services(&tree, &executor, &room, identity.actor.clone());
        let socket = native.home.runtime().join("cursor-broker.sock");
        let broker = spawn_broker(
            &native,
            socket.clone(),
            tree,
            executor.service.clone(),
            services,
        );
        let actions = broker.app_action_broker().unwrap();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = captured.clone();
        let subscription = broker.fanout.subscribe_rooms(
            vec![room.clone()],
            Box::new(move |event, _| {
                if let ui::collaboration::CollabEvent::RemotePresenceUpdate {
                    payload: ui::collaboration::PresencePayload::Interaction(interaction),
                    ..
                } = event
                {
                    sink.lock().unwrap().push(interaction.as_ref().clone());
                }
            }),
        );
        let presentations = join_presentations(&room, &identity);
        let adopted = Arc::new(Mutex::new(None));
        let (chat_stream, chat_authorizer) = actions
            .state()
            .connect_local_worker_with_authorizer(
                tilo_action_broker::route::CrdtRoute::record(CHAT, &room, ""),
                &identity.governor,
            )
            .unwrap();
        let (commands, thread) = spawn_clients(
            socket,
            room,
            presentations,
            chat_stream,
            chat_authorizer,
            adopted.clone(),
        );
        let task_a = start_task(&native, &executor.service, "Write release notes");
        Self {
            native,
            actions,
            interaction: None,
            task_a,
            task_b: None,
            broker,
            executor,
            _settings: settings,
            commands,
            thread: Some(thread),
            captured,
            adopted,
            _subscription: subscription,
        }
    }
    pub(super) fn invoke_a(&mut self) -> tilo_actions::SemanticInteraction {
        let (ready, wait) = std::sync::mpsc::sync_channel(1);
        let peer = self
            .actions
            .state()
            .published_surfaces()
            .into_iter()
            .find(|surface| surface.live && surface.app_id == NOTES)
            .expect("authenticated Notes writer published its real action")
            .peer_id;
        self.commands.send(Command::Invoke(ready, peer)).unwrap();
        wait.recv_timeout(Duration::from_secs(3)).unwrap();
        let interaction = self
            .captured
            .lock()
            .unwrap()
            .iter()
            .find(|i| i.paint_origin.is_some())
            .cloned()
            .expect("certified Notes admission must emit its own paint origin");
        assert_eq!(
            self.actions.state().certified_painted_task(
                &self.native.room.to_string(),
                NOTES,
                "scribe",
                &interaction,
            ),
            Some((self.task_a.clone(), 1))
        );
        self.interaction = Some(interaction.clone());
        interaction
    }
    pub(super) fn finish_a_start_b(&mut self) -> String {
        let (ready, wait) = std::sync::mpsc::sync_channel(1);
        self.commands
            .send(Command::Finish(ready, self.task_a.clone()))
            .unwrap();
        wait.recv_timeout(Duration::from_secs(3)).unwrap();
        let task = start_task(&self.native, &self.executor.service, "Write another note");
        self.task_b = Some(task.clone());
        task
    }
    /// The exact editor fetched by Chat through the original invocation's private lane.
    /// Remains absent until the broker has accepted Chat's acknowledgement.
    pub(super) fn adopted_draft(&self) -> Option<runtime_collab::private_draft::Draft> {
        self.adopted.lock().unwrap().clone()
    }
    pub(super) fn owner(&self) -> &tilo_joined_room::NativeWorkspaceOwner {
        &self.native.owner
    }
    pub(super) fn broker(&self) -> &tilo_action_broker::collab_broker::BrokerHandle {
        &self.broker
    }
}
impl Drop for CertifiedCursorFixture {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
/// The provisioned agent plus the person custody the certified clients join with.
struct Identity {
    actor: Arc<Actor>,
    certificate: tilo_user_identity::DeviceCert,
    proof: [u8; 64],
    key: ed25519_dalek::SigningKey,
    governor: tilo_user_identity::DeviceCert,
}

fn cursor_settings() -> crate::shell::test_support::InstalledSettings {
    let profile = agent_provider_config::ProviderProfile {
        id: "cursor-test".into(),
        label: "Local test".into(),
        endpoint: "http://localhost:8000/v1".into(),
        location: agent_provider_config::EndpointLocation::ThisDevice,
        default_model: Some("selected-model".into()),
        models: vec![agent_provider_config::ModelDescriptor {
            id: "selected-model".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let config = agent_provider_config::ProviderConfig {
        profiles: vec![profile],
        default_profile: Some("cursor-test".into()),
    };
    assert!(config.validation_errors().is_empty());
    let source = settings_store::HostDocument::new(
        serde_json::json!({"agent_providers": {"config": config}})
            .as_object()
            .unwrap()
            .clone(),
        |_| Err(std::io::Error::other("read-only test Settings")),
    );
    crate::shell::test_support::install_settings(Arc::new(source))
}

/// Provision the throwaway agent and admit it as this workspace's operator.
fn provision_identity(native: &NativeFixture, room: &str) -> Identity {
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
    native
        .owner
        .admit_managed_agent(
            native.room,
            actor.certificate().clone(),
            delegation.clone(),
            &governor,
            &key,
            std::time::SystemTime::now(),
        )
        .unwrap();
    let actor = Arc::new(Actor { actor, delegation });
    let (certificate, _, proof) = actor.identity().unwrap();
    Identity {
        actor,
        certificate,
        proof,
        key,
        governor,
    }
}

fn cursor_services(
    tree: &crate::shell::state::WorkspaceTreeHandle,
    executor: &executor::Executor,
    room: &str,
    actor: Arc<Actor>,
) -> tilo_action_broker::services::HostServices {
    let mut services =
        crate::shell::broker_services::native_services(Some(tree.clone()), None, None);
    services.native_task_executors = Some(executor.service.task_executors.clone());
    services.workspace_memberships = Some(Arc::new(executor.service.clone()));
    services.launcher = Some(Arc::new(Launch {
        room: room.to_string(),
        actor,
        protected: launcher::registration_gate::ProtectedModelLaunch {
            app: "so.tilo.agent".into(),
            room: room.to_string(),
            agent: "scribe".into(),
            launch: uuid::Uuid::new_v4().to_string(),
        },
    }));
    services
}

fn spawn_broker(
    native: &NativeFixture,
    socket: std::path::PathBuf,
    tree: crate::shell::state::WorkspaceTreeHandle,
    service: NativeOwnerCommitService,
    services: tilo_action_broker::services::HostServices,
) -> tilo_action_broker::collab_broker::BrokerHandle {
    tilo_action_broker::collab_broker::spawn_with_services(
        socket,
        Some(Arc::new(Authority {
            tree,
            journal: native.home.config().join("cursor-broker-journal"),
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

/// The agent's and the person's certified join presentations for this workspace.
fn join_presentations(room: &str, identity: &Identity) -> (Vec<u8>, Vec<u8>) {
    let agent = runtime_collab::CertifiedJoinIdentity {
        workspace_id: room.to_string(),
        device_certificate: serde_json::to_vec(&identity.certificate).unwrap(),
        proof: identity.proof.to_vec(),
    }
    .encode()
    .unwrap();
    let notes = runtime_collab::CertifiedJoinIdentity {
        workspace_id: room.to_string(),
        device_certificate: serde_json::to_vec(&identity.governor).unwrap(),
        proof: identity
            .key
            .sign(&runtime_identity::collaboration_join_signing_bytes(room))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .unwrap();
    (agent, notes)
}

/// Run the client thread and hand back the command channel that drives it.
fn spawn_clients(
    socket: std::path::PathBuf,
    room: String,
    presentations: (Vec<u8>, Vec<u8>),
    chat_stream: std::os::unix::net::UnixStream,
    chat_authorizer: impl Send + 'static,
    adopted: Arc<Mutex<Option<runtime_collab::private_draft::Draft>>>,
) -> (
    tokio::sync::mpsc::UnboundedSender<Command>,
    std::thread::JoinHandle<()>,
) {
    let (agent_presentation, notes_presentation) = presentations;
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (ready, wait) = std::sync::mpsc::sync_channel(1);
    let thread = std::thread::spawn(move || {
        let _chat_authorizer = chat_authorizer;
        run_clients(
            socket,
            room,
            agent_presentation,
            notes_presentation,
            chat_stream,
            adopted,
            receiver,
            ready,
        );
    });
    wait.recv_timeout(Duration::from_secs(3)).unwrap();
    (commands, thread)
}

fn start_task(native: &NativeFixture, service: &NativeOwnerCommitService, title: &str) -> String {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let execute = |command| {
        runtime
            .block_on(service.execute(OwnerCommitRequest {
                room: native.room.to_string(),
                operation: CommitOperation::WorkspaceTaskCommand {
                    invocation: uuid::Uuid::new_v4(),
                    authority: WorkspaceTaskAuthority::Person {
                        caller: native.certificate.clone(),
                    },
                    command,
                },
            }))
            .unwrap()
    };
    let before = native.owner.workspace_task_snapshot(native.room).unwrap();
    execute(WorkspaceTaskCommand::QueueInstruction {
        title: title.into(),
        instruction: "Append a release note".into(),
        assigned_agent: "scribe".into(),
        route: tilo_desktop_model::WorkspaceTaskRoute::Model,
    });
    let task = native
        .owner
        .workspace_task_snapshot(native.room)
        .unwrap()
        .tasks
        .into_iter()
        .find(|task| !before.tasks.iter().any(|old| old.id == task.id))
        .unwrap()
        .id;
    execute(WorkspaceTaskCommand::ApproveRun {
        task_id: task.clone(),
        agent_id: "scribe".into(),
        expected_run_epoch: 0,
        after_revision: 0,
    });
    task
}
/// Take the certified join handoff for `app` in `room` off the broker's socket.
fn client(socket: &std::path::Path, room: &str, app: &str) -> runtime_collab::CollabClient {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    launcher::services::write_collab_handoff(&mut stream, app, room).unwrap();
    stream.set_nonblocking(true).unwrap();
    runtime_collab::CollabClient::from_raw(
        runtime_portal::CapnpClient::from_connected_io(
            tokio::net::UnixStream::from_std(stream).unwrap(),
            capnp_rpc::rpc_twoparty_capnp::Side::Client,
        )
        .bootstrap(),
    )
}

/// Join `app`'s channel in `room` through the broker's certified handoff.
fn join_channel<'a>(
    socket: &'a std::path::Path,
    room: &'a str,
    app: &'a str,
    presentation: &'a [u8],
    participant: &'a str,
    kind: &'a str,
) -> LocalBoxFuture<'a, runtime_collab::ChannelHandle> {
    Box::pin(async move {
        client(socket, room, app)
            .join_channel_as_kind(
                &tilo_action_broker::route::CrdtRoute::record(app, room, "main").channel_key(),
                presentation,
                participant,
                kind,
                "",
            )
            .await
            .unwrap()
    })
}

/// The Notes writer's one real action, which the agent invokes under its lease.
fn notes_action() -> runtime_collab::ExportedAction {
    runtime_collab::ExportedAction {
        resource: RESOURCE.into(),
        semantic_identity: None,
        action: "append".into(),
        resource_kind: "document".into(),
        resource_label: "Release notes".into(),
        action_label: "Append".into(),
        description: None,
        operation_kind: tilo_actions::OperationKind::Effect,
        params: Vec::new(),
        contract: tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Undoable,
            execution: tilo_actions::ExecutionMode::Background { cancellable: true },
            result: tilo_actions::ResultSpec::Job,
            ..Default::default()
        },
        view: None,
        input: runtime_collab::ExportedInput::None,
    }
}

/// Chat's one real view action: open the private draft the task run produced.
fn chat_action() -> runtime_collab::ExportedAction {
    runtime_collab::ExportedAction {
        resource: "chat".into(),
        semantic_identity: None,
        action: "adopt-private-draft".into(),
        resource_kind: "chat".into(),
        resource_label: "Chat".into(),
        action_label: "Open private draft".into(),
        description: None,
        operation_kind: tilo_actions::OperationKind::View,
        params: vec![tilo_actions::ActionParam::required(
            "token",
            tilo_actions::ParamType::Text,
            "Private draft handoff token",
        )],
        contract: tilo_actions::ActionContract {
            result: tilo_actions::ResultSpec::Json {
                schema: serde_json::json!({"type": "object"}),
            },
            ..tilo_actions::ActionContract::view()
        },
        view: None,
        input: runtime_collab::ExportedInput::None,
    }
}

/// Chat's handoff argument: the opaque token of the private draft to adopt.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Argument {
    token: uuid::Uuid,
}

fn draft_token(argument: &str) -> uuid::Uuid {
    let argument = argument
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .unwrap_or(argument);
    serde_json::from_str::<Argument>(argument).unwrap().token
}
struct ChatDirectory(runtime_collab::ExportedAction);
impl ChatDirectory {
    fn row(&self) -> runtime_collab::ActionDirectoryRow {
        runtime_collab::ActionDirectoryRow {
            parent_resource: String::new(),
            resource: self.0.resource.clone(),
            resource_kind: self.0.resource_kind.clone(),
            resource_label: self.0.resource_label.clone(),
            presentation: None,
            action: Some(self.0.action.clone()),
            descriptor: Some(self.0.clone()),
            has_children: false,
            visibility: Some(runtime_collab::RowVisibility {
                governing_kind: Some(self.0.operation_kind),
                action: self.0.action.clone(),
                contract: self.0.contract.clone(),
            }),
        }
    }
}
impl runtime_collab::raw::action_directory::Server for ChatDirectory {
    fn query(
        self: std::rc::Rc<Self>,
        _params: runtime_collab::raw::action_directory::QueryParams,
        mut results: runtime_collab::raw::action_directory::QueryResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        std::future::ready(runtime_collab::write_action_directory_page(
            results.get().init_page(),
            &runtime_collab::ActionDirectoryPage {
                revision: "chat-fixture-1".into(),
                rows: vec![self.row()],
                next_cursor: Vec::new(),
                exhausted: true,
                stale_cursor: false,
                budget_exhausted: false,
            },
        ))
    }
    fn resolve(
        self: std::rc::Rc<Self>,
        _params: runtime_collab::raw::action_directory::ResolveParams,
        mut results: runtime_collab::raw::action_directory::ResolveResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let mut resolution = results.get().init_resolution();
        let result = runtime_collab::write_action_directory_row(
            resolution.reborrow().init_found(),
            &self.row(),
        );
        resolution.set_resolved_revision("chat-fixture-1");
        std::future::ready(result)
    }
}

/// Drive the two certified clients and Chat's writer on one `LocalSet`.
///
/// The capnp clients are `Rc`-based, so the futures they hand back are `!Send`
/// by construction; this function owns the runtime that drives them.
fn run_clients(
    socket: std::path::PathBuf,
    room: String,
    agent_presentation: Vec<u8>,
    notes_presentation: Vec<u8>,
    chat_stream: std::os::unix::net::UnixStream,
    adopted: Arc<Mutex<Option<runtime_collab::private_draft::Draft>>>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
    ready: std::sync::mpsc::SyncSender<()>,
) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(tokio::task::LocalSet::new().run_until(async move {
            let agent = join_channel(
                &socket,
                &room,
                "so.tilo.agent",
                &agent_presentation,
                "scribe",
                "agent",
            )
            .await;
            let mut runs = agent.subscribe_task_runs().await.unwrap();
            let notes = join_channel(
                &socket,
                &room,
                NOTES,
                &notes_presentation,
                "Writer",
                "person",
            )
            .await;
            notes
                .publish_actions(NOTES, &[notes_action()])
                .await
                .unwrap();
            let mut invocations = notes.subscribe_stable_invocations().await.unwrap();
            let _context = notes.subscribe_context().await.unwrap();
            chat_stream.set_nonblocking(true).unwrap();
            let chat = runtime_collab::CollabClient::from_raw(
                runtime_portal::CapnpClient::from_connected_io(
                    tokio::net::UnixStream::from_std(chat_stream).unwrap(),
                    capnp_rpc::rpc_twoparty_capnp::Side::Client,
                )
                .bootstrap(),
            )
            .join_channel_as_kind(
                &tilo_action_broker::route::CrdtRoute::record(CHAT, &room, "").channel_key(),
                &notes_presentation,
                "Writer",
                "person",
                "",
            )
            .await
            .unwrap();
            let chat_action = chat_action();
            chat.publish_actions(CHAT, std::slice::from_ref(&chat_action))
                .await
                .unwrap();
            let chat_invocations = chat.subscribe_stable_invocations().await.unwrap();
            chat.publish_action_directory(CHAT, capnp_rpc::new_client(ChatDirectory(chat_action)))
                .await
                .unwrap();
            ready.send(()).unwrap();
            let mut run: Option<runtime_collab::TaskRunEnvelope> = None;
            loop {
                let command = tokio::select! {
                    incoming = chat_invocations.read() => {
                        adopt_private_draft(&chat, &incoming.unwrap(), &adopted, &room).await;
                        continue;
                    }
                    command = commands.recv() => {
                        let Some(command) = command else { break };
                        command
                    }
                };
                match command {
                    Command::Invoke(ready, peer_id) => {
                        run = Some(
                            invoke_task(
                                &agent,
                                &mut runs,
                                &notes,
                                &mut invocations,
                                peer_id,
                                ready,
                            )
                            .await,
                        );
                    }
                    Command::Finish(ready, task) => {
                        finish_run(&agent, run.as_ref().unwrap(), task, ready).await;
                    }
                    Command::Stop => break,
                }
            }
        }));
}

/// Adopt the private draft Chat was handed, then acknowledge it as the writer.
fn adopt_private_draft<'a>(
    chat: &'a runtime_collab::ChannelHandle,
    incoming: &'a runtime_collab::Invocation,
    adopted: &'a Mutex<Option<runtime_collab::private_draft::Draft>>,
    room: &'a str,
) -> LocalBoxFuture<'a, ()> {
    Box::pin(async move {
        assert_eq!(incoming.resource, "chat");
        assert_eq!(incoming.action, "adopt-private-draft");
        let token = draft_token(incoming.argument.as_deref().unwrap());
        let bytes = chat
            .private_draft(
                &runtime_collab::private_draft::Request {
                    private_draft: token,
                    acknowledge: false,
                },
                &incoming.invocation_uuid,
            )
            .await
            .unwrap();
        let draft: runtime_collab::private_draft::Draft = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(draft.workspace, room);
        chat.private_draft(
            &runtime_collab::private_draft::Request {
                private_draft: token,
                acknowledge: true,
            },
            &incoming.invocation_uuid,
        )
        .await
        .unwrap();
        *adopted.lock().unwrap() = Some(draft);
        chat.update_invocation(
            incoming.id,
            &serde_json::to_string(&tilo_actions::InvocationUpdate::Succeeded {
                invocation: tilo_actions::InvocationId(uuid::Uuid::from_bytes(
                    incoming.invocation_uuid,
                )),
                output: tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({"adopted": token.to_string()})),
                    ..Default::default()
                },
            })
            .unwrap(),
        )
        .await
        .unwrap();
    })
}

/// Invoke the Notes action under the delivered lease and settle its invocation.
fn invoke_task<'a>(
    agent: &'a runtime_collab::ChannelHandle,
    runs: &'a mut runtime_collab::TaskRunStream,
    notes: &'a runtime_collab::ChannelHandle,
    invocations: &'a mut runtime_collab::InvocationStream,
    peer_id: u64,
    ready: std::sync::mpsc::SyncSender<()>,
) -> LocalBoxFuture<'a, runtime_collab::TaskRunEnvelope> {
    Box::pin(async move {
        let delivered = runs.read().await.unwrap();
        let invocation = uuid::Uuid::new_v4();
        agent
            .invoke_task_action(
                delivered.run_id,
                &delivered.lease,
                runtime_collab::TaskAction {
                    peer_id,
                    provider: "",
                    resource: RESOURCE,
                    action: "append",
                    argument: None,
                },
                "",
                &invocation.to_string(),
            )
            .await
            .unwrap();
        let incoming = invocations.read().await.unwrap();
        assert_eq!(incoming.invocation_uuid, *invocation.as_bytes());
        assert_eq!(incoming.resource, RESOURCE);
        notes
            .update_invocation(
                incoming.id,
                &serde_json::to_string(&tilo_actions::InvocationUpdate::Succeeded {
                    invocation: tilo_actions::InvocationId(invocation),
                    output: tilo_actions::ActionOutput::default(),
                })
                .unwrap(),
            )
            .await
            .unwrap();
        ready.send(()).unwrap();
        delivered
    })
}

/// Report the run that carried the original cursor capture as finished.
fn finish_run<'a>(
    agent: &'a runtime_collab::ChannelHandle,
    run: &'a runtime_collab::TaskRunEnvelope,
    task: String,
    ready: std::sync::mpsc::SyncSender<()>,
) -> LocalBoxFuture<'a, ()> {
    Box::pin(async move {
        assert_eq!(run.task_id, task);
        agent
            .report_task_run(
                run.run_id,
                &run.lease,
                &runtime_collab::TaskRunUpdate::Succeeded {
                    message: "Finished the first cursor capture".into(),
                },
            )
            .await
            .unwrap();
        ready.send(()).unwrap();
    })
}

#[test]
fn native_notes_writer_retains_certified_cursor_origin_after_agent_changes_tasks() {
    let mut fixture = CertifiedCursorFixture::new();
    let original = fixture.invoke_a();
    let other = fixture.finish_a_start_b();
    assert_ne!(other, fixture.task_a);
    assert_eq!(
        fixture.actions.state().certified_painted_task(
            &fixture.native.room.to_string(),
            NOTES,
            "scribe",
            &original,
        ),
        Some((fixture.task_a.clone(), 1))
    );
    assert!(
        fixture
            .owner()
            .workspace_task_snapshot(fixture.native.room)
            .unwrap()
            .controls[&other]
            .active_run
            .is_some()
    );
    assert!(fixture.broker().app_action_broker().is_some());
    drop(fixture);
}
