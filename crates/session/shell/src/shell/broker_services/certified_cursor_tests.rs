//! Certified cursor provenance and private expansion through authenticated app writers.
use super::{NativeFixture, fixture};
use crate::shell::broker_services::NativeOwnerCommitService;
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
#[path = "model_start_tests/executor.rs"]
mod executor;

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
        let settings = crate::shell::test_support::install_settings(Arc::new(source));
        let executor = executor::start(&native);
        let tree = crate::shell::native_workspace_owner::test_tree_of(&native.owner);
        let room = native.room.to_string();
        let (key, governor) = tilo_identity_host::broker::trusted_current_device(&room).unwrap();
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
                expires_at_ns: tilo_clock::unix_now_ns() + 3_600_000_000_000,
            },
        )
        .unwrap();
        let delegation = actor.delegate_room(&room).unwrap();
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
        let mut services =
            crate::shell::broker_services::native_services(Some(tree.clone()), None, None);
        services.native_task_executors = Some(executor.service.task_executors.clone());
        services.workspace_memberships = Some(Arc::new(executor.service.clone()));
        services.launcher = Some(Arc::new(Launch {
            room: room.clone(),
            actor,
            protected: launcher::registration_gate::ProtectedModelLaunch {
                app: "so.tilo.agent".into(),
                room: room.clone(),
                agent: "scribe".into(),
                launch: uuid::Uuid::new_v4().to_string(),
            },
        }));
        let socket = native.home.runtime().join("cursor-broker.sock");
        let broker = tilo_action_broker::collab_broker::spawn_with_services(
            socket.clone(),
            Some(Arc::new(Authority {
                tree,
                journal: native.home.config().join("cursor-broker-journal"),
            })),
            tilo_action_broker::collab_outbound::PeerPublisher::with_optional_owner_commit(
                None,
                Arc::new(executor.service.clone()),
            ),
            None,
            services,
            Arc::default(),
            Arc::default(),
        )
        .unwrap();
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
        let agent_presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.clone(),
            device_certificate: serde_json::to_vec(&certificate).unwrap(),
            proof: proof.to_vec(),
        }
        .encode()
        .unwrap();
        use ed25519_dalek::Signer as _;
        let notes_presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.clone(),
            device_certificate: serde_json::to_vec(&governor).unwrap(),
            proof: key
                .sign(&runtime_identity::collaboration_join_signing_bytes(&room))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap();
        let adopted = Arc::new(Mutex::new(None));
        let adopted_sink = adopted.clone();
        let (chat_stream, chat_authorizer) = actions
            .state()
            .connect_local_worker_with_authorizer(
                tilo_action_broker::route::CrdtRoute::record(CHAT, &room, ""),
                &governor,
            )
            .unwrap();
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (ready, wait) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let _chat_authorizer = chat_authorizer;
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(tokio::task::LocalSet::new().run_until(run_clients(
                    socket,
                    room,
                    agent_presentation,
                    notes_presentation,
                    chat_stream,
                    adopted_sink,
                    receiver,
                    ready,
                )));
        });
        wait.recv_timeout(Duration::from_secs(3)).unwrap();
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
async fn connect(
    socket: &std::path::Path,
    room: &str,
    app: &str,
    presentation: &[u8],
    kind: &str,
) -> runtime_collab::ChannelHandle {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    launcher::services::write_collab_handoff(&mut stream, app, room).unwrap();
    stream.set_nonblocking(true).unwrap();
    let client = runtime_collab::CollabClient::from_raw(
        runtime_portal::CapnpClient::from_connected_io(
            tokio::net::UnixStream::from_std(stream).unwrap(),
            capnp_rpc::rpc_twoparty_capnp::Side::Client,
        )
        .bootstrap(),
    );
    client
        .join_channel_as_kind(
            &tilo_action_broker::route::CrdtRoute::record(app, room, "main").channel_key(),
            presentation,
            if kind == "agent" { "scribe" } else { "Writer" },
            kind,
            "",
        )
        .await
        .unwrap()
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

async fn run_clients(
    socket: std::path::PathBuf,
    room: String,
    agent_presentation: Vec<u8>,
    notes_presentation: Vec<u8>,
    chat_stream: std::os::unix::net::UnixStream,
    adopted: Arc<Mutex<Option<runtime_collab::private_draft::Draft>>>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
    ready: std::sync::mpsc::SyncSender<()>,
) {
    let agent = connect(
        &socket,
        &room,
        "so.tilo.agent",
        &agent_presentation,
        "agent",
    )
    .await;
    let runs = agent.subscribe_task_runs().await.unwrap();
    let notes = connect(&socket, &room, NOTES, &notes_presentation, "person").await;
    notes
        .publish_actions(
            NOTES,
            &[runtime_collab::ExportedAction {
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
            }],
        )
        .await
        .unwrap();
    let invocations = notes.subscribe_stable_invocations().await.unwrap();
    let _context = notes.subscribe_context().await.unwrap();
    chat_stream.set_nonblocking(true).unwrap();
    let chat_client = runtime_collab::CollabClient::from_raw(
        runtime_portal::CapnpClient::from_connected_io(
            tokio::net::UnixStream::from_std(chat_stream).unwrap(),
            capnp_rpc::rpc_twoparty_capnp::Side::Client,
        )
        .bootstrap(),
    );
    let chat = chat_client
        .join_channel_as_kind(
            &tilo_action_broker::route::CrdtRoute::record(CHAT, &room, "").channel_key(),
            &notes_presentation,
            "Writer",
            "person",
            "",
        )
        .await
        .unwrap();
    let chat_action = runtime_collab::ExportedAction {
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
    };
    chat.publish_actions(CHAT, &[chat_action.clone()])
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
                let incoming = incoming.unwrap();
                assert_eq!(incoming.resource, "chat");
                assert_eq!(incoming.action, "adopt-private-draft");
                let argument = incoming.argument.as_deref().unwrap();
                let argument = argument
                    .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                    .unwrap_or(argument);
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Argument {
                    token: uuid::Uuid,
                }
                let Argument { token } = serde_json::from_str(argument).unwrap();
                let bytes = chat.private_draft(
                    &runtime_collab::private_draft::Request {
                        private_draft: token,
                        acknowledge: false,
                    },
                    &incoming.invocation_uuid,
                ).await.unwrap();
                let draft: runtime_collab::private_draft::Draft =
                    serde_json::from_slice(&bytes).unwrap();
                assert_eq!(draft.workspace, room);
                chat.private_draft(
                    &runtime_collab::private_draft::Request {
                        private_draft: token,
                        acknowledge: true,
                    },
                    &incoming.invocation_uuid,
                ).await.unwrap();
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
                    }).unwrap(),
                ).await.unwrap();
                continue;
            }
            command = commands.recv() => {
                let Some(command) = command else { break };
                command
            }
        };
        match command {
            Command::Invoke(ready, peer_id) => {
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
                run = Some(delivered);
                ready.send(()).unwrap();
            }
            Command::Finish(ready, task) => {
                let run = run.as_ref().unwrap();
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
            }
            Command::Stop => break,
        }
    }
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
}
