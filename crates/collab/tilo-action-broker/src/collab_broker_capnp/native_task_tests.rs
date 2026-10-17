//! Actual enrolled owner and signed delegated channel through production RPCs.
#![expect(
    clippy::future_not_send,
    reason = "Capnp RPC clients are intentionally thread confined to the existing LocalSet"
)]
use super::super::*;
use crate::peer_actions::{CertificateAuthority, OwnerPolicyClock, PolicyOrigin};
use crate::services::{
    NativeTaskActionRequirement, NativeTaskBinding, WorkspaceAuthority, WorkspaceMemberships,
    WorkspaceTaskExecution,
};
use ed25519_dalek::{Signer as _, SigningKey};
use tilo_action_policy::roles::AgentPreset;
use tilo_owner_commit::{CommitFuture, CommitOperation, OwnerCommitRequest, OwnerCommitService};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
use tilo_peer_state::workspace_task::{
    WorkspaceTaskAuthority, WorkspaceTaskCommand, WorkspaceTaskSnapshot,
};

#[path = "native_task_launch_tests.rs"]
mod closed_app;
#[path = "native_task_confirmation_tests.rs"]
mod confirmation;
#[path = "native_task_cross_app_tests.rs"]
mod cross_app;

struct Native {
    owner: tilo_joined_room::NativeWorkspaceOwner,
    key: SigningKey,
    certificate: tilo_user_identity::DeviceCert,
    origin: PolicyOrigin,
    cached_projection: Mutex<Option<tilo_desktop_model::DesktopTree>>,
    capped_connection: std::sync::atomic::AtomicBool,
    spent_grants: Mutex<Vec<tilo_action_policy::GrantId>>,
}
fn room() -> tilo_desktop_model::RoomId {
    tilo_desktop_model::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap()
}
impl WorkspaceAuthority for Native {
    fn with_tree(
        &self,
        read: &mut dyn FnMut(&tilo_desktop_model::DesktopTree),
    ) -> Result<(), String> {
        let tree = WorkspaceAuthority::snapshot(self)?;
        read(&tree);
        Ok(())
    }

    fn snapshot(&self) -> Result<tilo_desktop_model::DesktopTree, String> {
        let cached = self.cached_projection.lock().unwrap().clone();
        if let Some(cached) = cached {
            return Ok(cached);
        }
        self.owner.with_current(room(), |view| {
            let doc = view
                .peer()
                .admitted_room()
                .ok_or("native document unavailable")?;
            let mut workspace = tilo_desktop_model::Workspace::new(
                tilo_desktop_model::WorkspaceId(1),
                "Native execution",
            );
            workspace.room = room();
            workspace.policy =
                tilo_desktop_model::projection::room_policy_of(doc, &room().to_string());
            workspace
                .policy
                .grants
                .retain(|grant| !self.spent_grants.lock().unwrap().contains(&grant.id));
            workspace.tasks = tilo_peer_state::workspace_task::snapshot(doc)?.tasks;
            let mut tree = tilo_desktop_model::DesktopTree {
                active: workspace.id,
                ..Default::default()
            };
            tree.workspaces.push(workspace);
            Ok(tree)
        })
    }
}
impl CertificateAuthority for Native {
    fn policy_time(&self) -> Result<Option<crate::peer_actions::PolicyTime>, String> {
        self.origin
            .capture()
            .map(Some)
            .map_err(|error| error.to_string())
    }
    fn authorize(
        &self,
        requested: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        if requested.to_string() != room().to_string() {
            return Err("foreign room".into());
        }
        self.owner.with_current(room(), |view| {
            if !view.peer().certificates().contains(cert) {
                return Err("certificate removed".into());
            }
            view.peer()
                .permissions(cert.device_public_key, std::time::SystemTime::now())
                .map(|_| ())
        })
    }
    fn governs(&self, requested: uuid::Uuid, device: Option<&[u8; 32]>) -> bool {
        requested.to_string() == room().to_string()
            && !(self
                .capped_connection
                .load(std::sync::atomic::Ordering::SeqCst)
                && device.is_some())
            && device.is_none_or(|key| *key == self.certificate.device_public_key)
    }
}
impl OwnerCommitService for Native {
    fn requires_room(&self, requested: &str) -> bool {
        requested == room().to_string()
    }
    fn health(&self) -> Result<(), String> {
        self.owner.health()
    }
    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        let owner = self.owner.clone();
        let key = self.key.clone();
        let cert = self.certificate.clone();
        Box::pin(async move {
            // A real owner commits asynchronously: let other requests run
            // first, as they do while a commit is on its way.
            tokio::task::yield_now().await;
            let receipt = match request.operation {
                CommitOperation::WorkspaceTaskCommand { .. } => owner
                    .commit_workspace_task_command(request.clone(), std::time::SystemTime::now()),
                CommitOperation::EffectReceipt { .. } => {
                    owner.commit_effect_receipt(&request, &cert, &key, std::time::SystemTime::now())
                }
                _ => Err("unsupported fixture owner operation".into()),
            }?;
            receipt.validate_for(&request)?;
            Ok(receipt)
        })
    }
}
impl crate::services::AppActionGrants for Native {
    fn may_grant(&self, _room: &str, _person: &str) -> bool {
        false
    }
    fn record(
        &self,
        _request: &crate::app_actions::AppActionConsentRequest,
        _grant: tilo_action_policy::Grant,
        _answer: crate::app_actions::AppActionConsent,
    ) -> Option<String> {
        None
    }
    fn revoke(&self, requested: &str, grant: &str) {
        assert_eq!(requested, room().to_string());
        // Like the shell's grant service, update the broker's live policy
        // projection synchronously without re-entering the task mutation turn.
        self.spent_grants
            .lock()
            .unwrap()
            .push(tilo_action_policy::GrantId::new(grant));
    }
}

impl WorkspaceMemberships for Native {
    fn requires_room(&self, requested: &str) -> bool {
        OwnerCommitService::requires_room(self, requested)
    }
    fn health(&self) -> Result<(), String> {
        self.owner.health()
    }
    fn task_snapshot(&self, requested: &str) -> Result<WorkspaceTaskSnapshot, String> {
        if !OwnerCommitService::requires_room(self, requested) {
            return Err("foreign task room".into());
        }
        self.owner.workspace_task_snapshot(room())
    }
    fn execute(&self, request: OwnerCommitRequest) -> CommitFuture {
        self.commit(request)
    }
}
impl WorkspaceTaskExecution for Native {
    fn requires_room(&self, requested: &str) -> bool {
        OwnerCommitService::requires_room(self, requested)
    }
    fn snapshot(&self, requested: &str) -> Result<WorkspaceTaskSnapshot, String> {
        self.task_snapshot(requested)
    }
    fn agent_snapshot(
        &self,
        requested: &str,
        agent: &str,
    ) -> Result<WorkspaceTaskSnapshot, String> {
        if !OwnerCommitService::requires_room(self, requested) {
            return Err("foreign task room".into());
        }
        self.owner.with_current(room(), |view| {
            view.peer().workspace().check_task_execution_actor(
                agent,
                None,
                std::time::SystemTime::now(),
            )?;
            tilo_peer_state::workspace_task::snapshot(
                view.peer()
                    .admitted_room()
                    .ok_or("native task document unavailable")?,
            )
        })
    }
    fn report(
        &self,
        requested: &str,
        binding: NativeTaskBinding,
        command: WorkspaceTaskCommand,
    ) -> CommitFuture {
        let digest = blake3::hash(
            &serde_json::to_vec(&(requested, &binding.task_id, binding.epoch, &command)).unwrap(),
        );
        self.commit(OwnerCommitRequest {
            room: requested.into(),
            operation: CommitOperation::WorkspaceTaskCommand {
                invocation: uuid::Uuid::from_bytes(digest.as_bytes()[..16].try_into().unwrap()),
                authority: if matches!(command, WorkspaceTaskCommand::Abandon { .. }) {
                    WorkspaceTaskAuthority::Host {
                        attestor: self.certificate.clone(),
                    }
                } else {
                    WorkspaceTaskAuthority::Agent {
                        attestor: self.certificate.clone(),
                        agent_id: binding.agent_id,
                    }
                },
                command,
            },
        })
    }
    fn authorize_action(
        &self,
        requested: &str,
        agent: &str,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String> {
        if !OwnerCommitService::requires_room(self, requested) {
            return Err("foreign task room".into());
        }
        self.owner.with_current(room(), |view| {
            let now = std::time::SystemTime::now();
            view.peer()
                .workspace()
                .check_task_execution_actor(agent, None, now)?;
            requirement.check(view.peer(), requested, agent, now)
        })
    }

    fn admit_host_launch(
        &self,
        requested: &str,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
        launch: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        if !OwnerCommitService::requires_room(self, requested) {
            return Err("foreign task room".into());
        }
        self.owner.with_task_action_admission(
            room(),
            &binding.task_id,
            &binding.agent_id,
            binding.epoch,
            binding.revision,
            || {
                self.authorize_action(requested, &binding.agent_id, requirement)?;
                launch()
            },
        )
    }

    fn admit(
        &self,
        requested: &str,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
        action: &mut dyn FnMut() -> Result<InvocationJob, String>,
    ) -> Result<InvocationJob, String> {
        if !OwnerCommitService::requires_room(self, requested) {
            return Err("foreign task room".into());
        }
        self.owner.with_task_action_admission(
            room(),
            &binding.task_id,
            &binding.agent_id,
            binding.epoch,
            binding.revision,
            || {
                self.authorize_action(requested, &binding.agent_id, requirement)?;
                action()
            },
        )
    }
}
struct Fixture {
    directory: tempfile::TempDir,
    _fanout: Arc<FanoutState>,
    native: Arc<Native>,
    state: Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    task: String,
}
impl Fixture {
    fn new() -> Self {
        Self::with_launcher(None)
    }
    fn with_launcher(launcher: Option<Arc<dyn crate::services::LauncherServices>>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let owner = tilo_joined_room::open_file(&directory.path().join("native.json")).unwrap();
        let key = SigningKey::from_bytes(&[92; 32]);
        let mut certificate = tilo_joined_room::testing::certificate(
            &SigningKey::from_bytes(&[91; 32]),
            &key,
            "Governor",
        );
        // The signed device is initially governed here, so its foreign-device
        // ceiling only applies after the live connection loses that authority.
        certificate.capabilities.foreign = true;
        tilo_user_identity::sign_device_cert(&mut certificate, &SigningKey::from_bytes(&[91; 32]))
            .unwrap();
        tilo_joined_room::testing::enroll_interactive_governor(&owner, &key, &certificate).unwrap();
        let native = Arc::new(Native {
            owner,
            key,
            certificate,
            origin: OwnerPolicyClock::new().unwrap().new_origin(),
            cached_projection: Mutex::new(None),
            capped_connection: std::sync::atomic::AtomicBool::new(false),
            spent_grants: Mutex::new(Vec::new()),
        });
        let membership = OwnerCommitRequest {
            room: room().to_string(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: native.certificate.clone(),
                command: WorkspaceMembershipCommand::AddAgent {
                    agent: "scribe".into(),
                    // Reading only: the agent may use a tool when the
                    // room grants it, never because its run was approved.
                    preset: AgentPreset::Observer,
                },
            },
        };
        native
            .owner
            .commit_workspace_membership_command(
                membership,
                &native.certificate,
                &native.key,
                std::time::SystemTime::now(),
            )
            .unwrap();
        let queue = uuid::Uuid::new_v4();
        native
            .owner
            .commit_workspace_task_command(
                OwnerCommitRequest {
                    room: room().to_string(),
                    operation: CommitOperation::WorkspaceTaskCommand {
                        invocation: queue,
                        authority: WorkspaceTaskAuthority::Person {
                            caller: native.certificate.clone(),
                        },
                        command: WorkspaceTaskCommand::QueueInstruction {
                            title: "Prepare release notes".into(),
                            instruction: "Inspect every release-note section, preserve all cross references, and report every blocking inconsistency. ".repeat(4),
                            assigned_agent: "scribe".into(),
                            route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                        },
                    },
                },
                std::time::SystemTime::now(),
            )
            .unwrap();
        let task = native.owner.workspace_task_snapshot(room()).unwrap().tasks[0]
            .id
            .clone();
        let fanout = Arc::new(FanoutState::new());
        let config = crate::peer_actions::NativePeerBrokerConfig {
            journal_root: directory.path().join("journal"),
            certificate_authority: native.clone(),
        };
        let (state, rx) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::with_optional_owner_commit(None, native.clone()),
            Some(native.clone()),
            None,
            Some(&config),
            HostServices {
                launcher,
                app_action_grants: Some(native.clone()),
                task_execution: Some(native.clone()),
                workspace_memberships: Some(native.clone()),
                ..Default::default()
            },
        );
        Self {
            directory,
            _fanout: fanout,
            native,
            state,
            directories: Rc::new(LocalActionDirectoryRegistry::new(rx)),
            task,
        }
    }
    /// Attenuate the live connection without changing its signed identity or
    /// the owner's canonical authority over the delegated agent.
    fn cap_connection(&self) {
        self.native
            .capped_connection
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let current = self
            .state
            .verify_action_caller(&room().to_string(), &self.signed_presentation())
            .unwrap();
        assert!(current.admitted.verified.capped);
        assert!(self.native.certificate.capabilities.foreign);
        assert!(
            self.native
                .governs(uuid::Uuid::parse_str(&room().to_string()).unwrap(), None)
        );
    }
    fn person_command(&self, command: WorkspaceTaskCommand) -> Result<(), String> {
        self.native
            .owner
            .commit_workspace_task_command(
                OwnerCommitRequest {
                    room: room().to_string(),
                    operation: CommitOperation::WorkspaceTaskCommand {
                        invocation: uuid::Uuid::new_v4(),
                        authority: WorkspaceTaskAuthority::Person {
                            caller: self.native.certificate.clone(),
                        },
                        command,
                    },
                },
                std::time::SystemTime::now(),
            )
            .map(|_| ())
    }
    fn remove_agent(&self) {
        let removal = OwnerCommitRequest {
            room: room().to_string(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: self.native.certificate.clone(),
                command: WorkspaceMembershipCommand::RemoveAgent {
                    agent: "scribe".into(),
                },
            },
        };
        self.native
            .owner
            .commit_workspace_membership_command(
                removal,
                &self.native.certificate,
                &self.native.key,
                std::time::SystemTime::now(),
            )
            .unwrap();
    }
    fn approve(&self) {
        self.person_command(WorkspaceTaskCommand::ApproveRun {
            task_id: self.task.clone(),
            agent_id: "scribe".into(),
            expected_run_epoch: 0,
            after_revision: 0,
        })
        .unwrap();
    }
    fn channel(&self, peer: u64, agent: bool) -> channel_handle::Client {
        self.app_channel(
            peer,
            if agent {
                "so.tilo.agent"
            } else {
                "so.tilo.notes"
            },
            agent,
        )
    }
    fn signed_presentation(&self) -> Vec<u8> {
        let room = room().to_string();
        runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.clone(),
            device_certificate: serde_json::to_vec(&self.native.certificate).unwrap(),
            proof: self
                .native
                .key
                .sign(&runtime_identity::collaboration_join_signing_bytes(&room))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap()
    }
    fn app_channel(&self, peer: u64, app: &str, agent: bool) -> channel_handle::Client {
        let room = room().to_string();
        let mut admitted = self
            .state
            .verify_action_caller(&room, &self.signed_presentation())
            .unwrap()
            .admitted;
        let route = CrdtRoute::record(app, room, "main");
        if agent {
            admit_agent_delegation(
                &WorkspaceAuthority::snapshot(self.native.as_ref()).unwrap(),
                &route,
                &mut admitted,
                "scribe",
            )
            .unwrap();
        }
        capnp_rpc::new_client(CapnpChannelHandle {
            person_chat_views: std::cell::RefCell::default(),
            committed_read: Rc::new(std::cell::RefCell::new(committed_document::Slot::default())),
            state: self.state.clone(),
            directories: self.directories.clone(),
            channel_id: route.channel_key(),
            peer_id: peer,
            verified_identity: admitted,
            record_kind: None,
            lease: None,
        })
    }
}

async fn delivered(channel: &channel_handle::Client) -> runtime_collab::TaskRunEnvelope {
    let stream = channel
        .subscribe_task_runs_request()
        .send()
        .promise
        .await
        .unwrap()
        .get()
        .unwrap()
        .get_stream()
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(2), stream.read_request().send().promise)
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(
        reply
            .get()
            .unwrap()
            .get_task_json()
            .unwrap()
            .to_str()
            .unwrap(),
    )
    .unwrap()
}
async fn report(
    channel: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
    update: runtime_collab::TaskRunUpdate,
) -> Result<(), capnp::Error> {
    let mut request = channel.report_task_run_request();
    let mut p = request.get();
    p.set_run_id(run.run_id);
    p.set_lease(&run.lease);
    p.set_update_json(serde_json::to_string(&update).unwrap());
    request.send().promise.await.map(|_| ())
}

async fn notes_target(f: &Fixture) -> (channel_handle::Client, invocation_stream::Client) {
    let target = f.channel(200, false);
    let mut publish = target.publish_actions_request();
    publish.get().set_app_id("so.tilo.notes");
    {
        let mut row = publish.get().init_actions(1).get(0);
        row.set_resource("notes:release");
        row.set_action("append");
        row.set_resource_kind("document");
        row.set_resource_label("Release notes");
        row.set_action_label("Append");
        row.set_operation_kind("effect");
        row.set_params_json("[]");
        let contract = tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Undoable,
            execution: tilo_actions::ExecutionMode::Background { cancellable: true },
            result: tilo_actions::ResultSpec::Job,
            ..Default::default()
        };
        row.set_contract_json(serde_json::to_string(&contract).unwrap());
    }
    publish.send().promise.await.unwrap();
    let mut subscribe = target.subscribe_invocations_request();
    subscribe.get().set_stable_invocation_ids(true);
    let stream = subscribe
        .send()
        .promise
        .await
        .unwrap()
        .get()
        .unwrap()
        .get_stream()
        .unwrap();
    (target, stream)
}
fn grant_notes(f: &Fixture) {
    set_notes_permission(f, true);
}
fn set_notes_permission(f: &Fixture, may_write: bool) {
    use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath};
    let room_id = room().to_string();
    let append = tilo_actions::action_node(
        &TreePath::room(&room_id),
        "so.tilo.notes",
        "document",
        "notes:release",
        None,
        "append",
    );
    let grant = if may_write {
        Grant::new(
            "fixture-notes-append",
            Subject::agent("scribe"),
            append,
            KindSet::UNDOABLE,
            Answer::Allow,
            Subject::person(hex::encode(f.native.certificate.user_public_key)),
        )
    } else {
        Grant::new(
            "fixture-notes-restrict",
            Subject::agent("scribe"),
            append,
            KindSet::ALL,
            Answer::Deny,
            Subject::person(hex::encode(f.native.certificate.user_public_key)),
        )
    };
    put_grant(f, grant);
}
fn put_grant(f: &Fixture, grant: tilo_action_policy::Grant) {
    f.native
        .spent_grants
        .lock()
        .unwrap()
        .retain(|id| id != &grant.id);
    let now = std::time::SystemTime::now();
    let messages = f
        .native
        .owner
        .with_current(room(), |view| {
            let before = view.peer().clone();
            let mut next = before.clone();
            next.governance_mut()
                .put_governed_grants(&[grant], &f.native.key, now)?;
            next.durable_mutation_messages(
                &before,
                &f.native.key,
                now,
                view.generation().checked_add(1).unwrap(),
            )
        })
        .unwrap();
    for message in messages {
        f.native
            .owner
            .receive(
                room(),
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    f.native.key.verifying_key().to_bytes(),
                ),
                message,
                now,
            )
            .unwrap();
    }
}
async fn invoke(
    channel: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
    invocation: uuid::Uuid,
) -> Result<(u64, tilo_actions::InvocationUpdate), capnp::Error> {
    let mut request = channel.invoke_task_action_request();
    let mut p = request.get();
    p.set_run_id(run.run_id);
    p.set_lease(&run.lease);
    p.set_peer_id(200);
    p.set_resource("notes:release");
    p.set_action("append");
    p.set_invocation(invocation.to_string());
    let response = request.send().promise.await?;
    let value = response.get()?;
    Ok((
        value.get_id(),
        serde_json::from_str(value.get_update_json()?.to_str().unwrap()).unwrap(),
    ))
}

async fn ordinary_invoke(channel: &channel_handle::Client) -> Result<(), capnp::Error> {
    let mut request = channel.invoke_published_action_request();
    let mut p = request.get();
    p.set_peer_id(200);
    p.set_resource("notes:release");
    p.set_action("append");
    request.send().promise.await.map(|_| ())
}

#[tokio::test(flavor = "current_thread")]
async fn authenticated_native_action_ignores_another_running_tasks_ceiling() {
    use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath};
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (_target, stream) = notes_target(&f).await;
            grant_notes(&f);
            f.person_command(WorkspaceTaskCommand::QueueInstruction {
                title: "Other work".into(),
                instruction: "Inspect unrelated work".into(),
                assigned_agent: "scribe".into(),
                route: tilo_desktop_model::WorkspaceTaskRoute::Model,
            })
            .unwrap();
            let other = f
                .native
                .owner
                .workspace_task_snapshot(room())
                .unwrap()
                .tasks
                .into_iter()
                .find(|task| task.id != f.task)
                .unwrap()
                .id;
            f.person_command(WorkspaceTaskCommand::ApproveRun {
                task_id: other.clone(),
                agent_id: "scribe".into(),
                expected_run_epoch: 0,
                after_revision: 0,
            })
            .unwrap();
            put_grant(
                &f,
                Grant::new(
                    "other-task-denies-append",
                    Subject::Task(other.clone()),
                    TreePath::room(room().to_string()).app("so.tilo.notes"),
                    KindSet::UNDOABLE,
                    Answer::Deny,
                    Subject::person(hex::encode(f.native.certificate.user_public_key)),
                ),
            );
            assert_eq!(
                f.native
                    .owner
                    .workspace_task_snapshot(room())
                    .unwrap()
                    .tasks
                    .iter()
                    .find(|task| task.id == other)
                    .unwrap()
                    .status,
                WorkspaceTaskStatus::InProgress,
            );
            let invocation = uuid::Uuid::new_v4();
            let first = invoke(&agent, &run, invocation).await.unwrap();
            assert!(matches!(
                first.1,
                tilo_actions::InvocationUpdate::Accepted { .. }
            ));
            let reply = stream.read_request().send().promise.await.unwrap();
            assert_eq!(
                reply.get().unwrap().get_invocation_uuid().unwrap(),
                invocation.as_bytes()
            );
            assert_eq!(invoke(&agent, &run, invocation).await.unwrap(), first);
            let unbound_refused = ordinary_invoke(&agent).await.is_err();
            drop(f);
            assert!(unbound_refused);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_native_agent_invocation_cannot_escape_task_resource_scope() {
    use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, Subject, TreePath};
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (_target, stream) = notes_target(&f).await;
            grant_notes(&f);
            put_grant(
                &f,
                Grant::new(
                    "task-other-resource-only",
                    Subject::Task(f.task.clone()),
                    tilo_actions::action_node(
                        &TreePath::room(room().to_string()),
                        "so.tilo.notes",
                        "document",
                        "notes:other",
                        None,
                        "append",
                    ),
                    KindSet::UNDOABLE,
                    Answer::Allow,
                    Subject::person(hex::encode(f.native.certificate.user_public_key)),
                )
                .with_lifetime(Lifetime::Task(f.task.clone())),
            );
            assert!(invoke(&agent, &run, uuid::Uuid::new_v4()).await.is_err());
            assert!(
                ordinary_invoke(&agent)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("authenticated task action admission")
            );
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(30),
                    stream.read_request().send().promise
                )
                .await
                .is_err(),
                "neither route may dispatch outside the task resource scope"
            );
            drop(f);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_native_agent_invocation_remains_fenced_after_stop() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (target, stream) = notes_target(&f).await;
            grant_notes(&f);
            let invocation = uuid::Uuid::new_v4();
            let (id, _) = invoke(&agent, &run, invocation).await.unwrap();
            stream.read_request().send().promise.await.unwrap();
            let mut update = target.update_invocation_request();
            update.get().set_id(id);
            update.get().set_update_json(
                serde_json::to_string(&tilo_actions::InvocationUpdate::Succeeded {
                    invocation: tilo_actions::InvocationId(invocation),
                    output: tilo_actions::ActionOutput::default(),
                })
                .unwrap(),
            );
            update.send().promise.await.unwrap();
            f.person_command(WorkspaceTaskCommand::CancelForRun {
                task_id: f.task.clone(),
                run_epoch: 1,
                after_revision: 0,
            })
            .unwrap();
            assert!(invoke(&agent, &run, uuid::Uuid::new_v4()).await.is_err());
            assert!(
                ordinary_invoke(&agent)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("authenticated task action admission")
            );
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(30),
                    stream.read_request().send().promise
                )
                .await
                .is_err(),
                "ordinary invocation cannot dispatch after Stop"
            );
            drop(f);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_live_owner_revocation_fences_new_and_retained_actions_with_stale_projection() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            assert_eq!(run.title, "Prepare release notes");
            assert!(run.instruction().chars().count() > 240);
            assert!(run.instruction().contains("blocking inconsistency"));
            let (_target, stream) = notes_target(&f).await;
            grant_notes(&f);
            let projected = WorkspaceAuthority::snapshot(&*f.native).unwrap();
            *f.native.cached_projection.lock().unwrap() = Some(projected);
            let original = uuid::Uuid::from_u128(8010);
            invoke(&agent, &run, original).await.unwrap();
            stream.read_request().send().promise.await.unwrap();
            set_notes_permission(&f, false);
            assert!(
                invoke(&agent, &run, original)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("not allowed")
            );
            assert!(
                invoke(&agent, &run, uuid::Uuid::from_u128(8011))
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("not allowed")
            );
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(30),
                    stream.read_request().send().promise
                )
                .await
                .is_err()
            );
            f.remove_agent();
            let mut control = agent.task_run_cancelled_request();
            control.get().set_run_id(run.run_id);
            control.get().set_lease(&run.lease);
            assert!(
                control.send().promise.await.is_err(),
                "removed agent must not read pending directions through a stale projection"
            );
            drop(f);
        })
        .await;
}

/// The agent may append to the notes, but the task it runs only reads:
/// the run is held to its task's grant set.
#[tokio::test(flavor = "current_thread")]
async fn a_native_run_is_held_to_its_tasks_grant_set() {
    use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, Subject, TreePath};
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (_target, stream) = notes_target(&f).await;
            grant_notes(&f);
            put_grant(
                &f,
                Grant::new(
                    "task-reads-only",
                    Subject::Task(f.task.clone()),
                    TreePath::room(room().to_string()),
                    KindSet::READ,
                    Answer::Allow,
                    Subject::person(hex::encode(f.native.certificate.user_public_key)),
                )
                .with_lifetime(Lifetime::Task(f.task.clone())),
            );
            assert!(
                invoke(&agent, &run, uuid::Uuid::from_u128(8100))
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("not allowed")
            );
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(30),
                    stream.read_request().send().promise
                )
                .await
                .is_err(),
                "the refused action never reached the app"
            );
            drop(f);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_failure_preserves_pending_decisions_on_restart_and_old_run_cannot_retry() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            f.person_command(WorkspaceTaskCommand::DirectionForRun {
                task_id: f.task.clone(),
                run_epoch: 1,
                after_revision: 0,
                text: "Ask me before changing the public API".into(),
            })
            .unwrap();
            report(
                &agent,
                &run,
                runtime_collab::TaskRunUpdate::Failed {
                    message: "Need a decision".into(),
                },
            )
            .await
            .unwrap();
            let reopened =
                tilo_joined_room::open_file(&f.directory.path().join("native.json")).unwrap();
            let pending = reopened.workspace_task_snapshot(room()).unwrap();
            assert_eq!(pending.tasks[0].status, WorkspaceTaskStatus::Waiting);
            assert!(pending.controls[&f.task].active_run.is_none());
            assert_eq!(pending.controls[&f.task].directions.len(), 1);
            assert!(pending.controls[&f.task].directions[0].applied.is_none());
            f.person_command(WorkspaceTaskCommand::Retry {
                task_id: f.task.clone(),
                run_epoch: 1,
            })
            .unwrap();
            assert!(
                f.state
                    .dispatch_workspace_task(&room().to_string(), &f.task)
                    .is_err(),
                "retry alone does not approve a run"
            );
            f.person_command(WorkspaceTaskCommand::ApproveRun {
                task_id: f.task.clone(),
                agent_id: "scribe".into(),
                expected_run_epoch: 1,
                after_revision: 1,
            })
            .unwrap();
            let restarted = delivered(&agent).await;
            assert_ne!(restarted.run_id, run.run_id);
            assert!(
                report(
                    &agent,
                    &run,
                    runtime_collab::TaskRunUpdate::DirectionsApplied { revision: 1 }
                )
                .await
                .is_err()
            );
            let control = observed_control(&agent, &restarted).await;
            assert_eq!(control.run_epoch, 2);
            assert_eq!(control.applied_revision, 0);
            assert_eq!(
                control.directions[0].text,
                "Ask me before changing the public API"
            );
            drop(f);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_approval_grants_no_tool_permission_and_deleted_targets_fail_closed() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (_target, stream) = notes_target(&f).await;
            assert!(
                invoke(&agent, &run, uuid::Uuid::from_u128(800))
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("not allowed")
            );
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(30),
                    stream.read_request().send().promise
                )
                .await
                .is_err()
            );
            grant_notes(&f);
            f.state.remove_connection(200);
            assert!(
                invoke(&agent, &run, uuid::Uuid::from_u128(801))
                    .await
                    .is_err()
            );
            let snapshot = f.native.owner.workspace_task_snapshot(room()).unwrap();
            assert_eq!(snapshot.tasks[0].status, WorkspaceTaskStatus::InProgress);
            assert!(
                snapshot.controls[&f.task].execution.is_none(),
                "approval is not acceptance"
            );
            drop(f);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_stream_replacement_keeps_run_identity_and_cancel_fences_new_effects() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let budget = collab_types::task_budget::TaskExecutionBudget {
                turn_ms: 5000,
                total_ms: 20000,
            };
            f.person_command(WorkspaceTaskCommand::SetExecutionBudget {
                task_id: f.task.clone(),
                expected_run_epoch: 0,
                expected_budget: None,
                budget: Some(budget),
            })
            .unwrap();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            assert_eq!(run.execution_budget, Some(budget));
            let replaced = delivered(&agent).await;
            assert_eq!(run, replaced);
            let (_target, stream) = notes_target(&f).await;
            grant_notes(&f);
            f.person_command(WorkspaceTaskCommand::CancelForRun {
                task_id: f.task.clone(),
                run_epoch: 1,
                after_revision: 0,
            })
            .unwrap();
            let mut status = agent.task_run_cancelled_request();
            status.get().set_run_id(run.run_id);
            status.get().set_lease(&run.lease);
            assert!(
                status
                    .send()
                    .promise
                    .await
                    .unwrap()
                    .get()
                    .unwrap()
                    .get_cancelled()
            );
            assert!(
                invoke(&agent, &run, uuid::Uuid::from_u128(900))
                    .await
                    .is_err()
            );
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(30),
                    stream.read_request().send().promise
                )
                .await
                .is_err()
            );
            assert!(
                report(
                    &agent,
                    &run,
                    runtime_collab::TaskRunUpdate::Succeeded {
                        message: "Unseen work".into()
                    }
                )
                .await
                .is_err()
            );
            assert_eq!(
                f.native
                    .owner
                    .workspace_task_snapshot(room())
                    .unwrap()
                    .tasks[0]
                    .status,
                WorkspaceTaskStatus::Cancelled
            );
            drop(f);
        })
        .await;
}

/// An agent reports acceptance and its first progress without waiting for
/// the host in between. Progress is decided against the accepted run, so it
/// must apply after acceptance, not race its commit: refused, it failed the
/// run at its end ("public progress requires accepted run").
#[tokio::test(flavor = "current_thread")]
async fn progress_sent_right_after_acceptance_applies_in_order() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (accepted, progress) = tokio::join!(
                report(&agent, &run, runtime_collab::TaskRunUpdate::Accepted),
                report(
                    &agent,
                    &run,
                    runtime_collab::TaskRunUpdate::Progress {
                        message: "Reviewing the catalogue".into(),
                        presentation: None,
                    },
                ),
            );
            accepted.unwrap();
            progress.unwrap();
            drop(f);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_task_tool_effect_is_attributed_and_duplicate_reconnect_does_not_execute_again() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            report(&agent, &run, runtime_collab::TaskRunUpdate::Accepted)
                .await
                .unwrap();
            let (target, stream) = notes_target(&f).await;
            grant_notes(&f);
            let id = uuid::Uuid::from_u128(3133);
            let first = invoke(&agent, &run, id).await.unwrap();
            assert!(matches!(
                first.1,
                tilo_actions::InvocationUpdate::Accepted { .. }
            ));
            let invocation_reply = stream.read_request().send().promise.await.unwrap();
            let wire = invocation_reply.get().unwrap();
            assert_eq!(wire.get_actor().unwrap().to_str().unwrap(), "agent:scribe");
            assert_eq!(wire.get_invocation_uuid().unwrap(), id.as_bytes());
            let context: tilo_actions::ActionContext =
                serde_json::from_str(wire.get_context_json().unwrap().to_str().unwrap()).unwrap();
            assert_eq!(context.group.as_ref().unwrap().id, f.task);
            let duplicate = invoke(&agent, &run, id).await.unwrap();
            assert_eq!(duplicate, first);
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(30),
                    stream.read_request().send().promise
                )
                .await
                .is_err(),
                "duplicate request must not deliver a second effect"
            );
            let terminal = tilo_actions::InvocationUpdate::Succeeded {
                invocation: tilo_actions::InvocationId(id),
                output: tilo_actions::ActionOutput::default(),
            };
            let mut update = target.update_invocation_request();
            update.get().set_id(first.0);
            update
                .get()
                .set_update_json(serde_json::to_string(&terminal).unwrap());
            update.send().promise.await.unwrap();
            f.state.remove_connection(100);
            let reconnected = f.channel(101, true);
            let resumed = delivered(&reconnected).await;
            assert_eq!(
                invoke(&reconnected, &resumed, id).await.unwrap().1,
                terminal
            );
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(30),
                    stream.read_request().send().promise
                )
                .await
                .is_err()
            );
            report(
                &reconnected,
                &resumed,
                runtime_collab::TaskRunUpdate::Succeeded {
                    message: "Release notes updated".into(),
                },
            )
            .await
            .unwrap();
            let snapshot = f.native.owner.workspace_task_snapshot(room()).unwrap();
            assert_eq!(snapshot.tasks[0].status, WorkspaceTaskStatus::Done);
            f.native
                .owner
                .with_current(room(), |view| {
                    let effects = tilo_desktop_model::projection::workspace_effect_receipts_of(
                        view.peer().admitted_room().unwrap(),
                    );
                    assert_eq!(effects.len(), 1);
                    assert_eq!(effects[0].actor, "agent:scribe");
                    assert_eq!(effects[0].context.group.as_ref().unwrap().id, f.task);
                    Ok(())
                })
                .unwrap();
        })
        .await;
}

async fn assert_authenticated_presentations_coalesce(
    f: &Fixture,
    agent: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
) {
    let presentation = |sequence| task_presentation(run, sequence);
    let update = presentation_update;
    let before = WorkspaceTaskExecution::snapshot(f.native.as_ref(), &room().to_string()).unwrap();
    let audits = before
        .tasks
        .iter()
        .find(|task| task.id == f.task)
        .unwrap()
        .audit
        .len();
    let receipts = before.controls[&f.task].receipts.len();
    let person = f.channel(99, false);
    assert!(report(&person, run, update(presentation(0))).await.is_err());
    let mut wrong_lease = run.clone();
    wrong_lease.lease.push('x');
    assert!(
        report(agent, &wrong_lease, update(presentation(0)))
            .await
            .is_err()
    );
    for sequence in 0..140 {
        report(agent, run, update(presentation(sequence)))
            .await
            .unwrap();
    }
    let snapshot =
        WorkspaceTaskExecution::snapshot(f.native.as_ref(), &room().to_string()).unwrap();
    assert_eq!(
        snapshot
            .tasks
            .iter()
            .find(|task| task.id == f.task)
            .unwrap()
            .audit
            .len(),
        audits
    );
    assert_eq!(snapshot.controls[&f.task].receipts.len(), receipts);
    assert_eq!(snapshot.controls[&f.task].presentations.len(), 1);
    assert_ne!(
        snapshot.controls[&f.task]
            .presentations
            .values()
            .next()
            .unwrap()
            .received_at,
        Some(-100),
        "sender cannot mint the host's receipt time"
    );
    assert_eq!(
        snapshot.controls[&f.task]
            .presentations
            .values()
            .next()
            .unwrap()
            .text,
        "Reply 139"
    );
}

fn task_presentation(
    run: &runtime_collab::TaskRunEnvelope,
    sequence: u64,
) -> runtime_collab::TaskPresentation {
    runtime_collab::TaskPresentation {
        run: format!("{}:0", run.run_id),
        received_at: Some(-100), // Sender cannot backdate the reply.
        sequence,
        text: format!("Reply {sequence}"),
        reasoning: "Provider-exposed summary".into(),
        finish: None,
        detail: String::new(),
    }
}

fn presentation_update(value: runtime_collab::TaskPresentation) -> runtime_collab::TaskRunUpdate {
    runtime_collab::TaskRunUpdate::Progress {
        message: "Working".into(),
        presentation: Some(value),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn authenticated_presentations_coalesce_without_audit_or_receipt_exhaustion() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            assert_eq!(run.presentation_version, Some(1));
            let presentation = |sequence| task_presentation(&run, sequence);
            let update = presentation_update;
            assert!(
                report(&agent, &run, update(presentation(0))).await.is_err(),
                "presentation requires accepted execution"
            );
            report(&agent, &run, runtime_collab::TaskRunUpdate::Accepted)
                .await
                .unwrap();
            assert_authenticated_presentations_coalesce(&f, &agent, &run).await;
            assert!(
                report(&agent, &run, update(presentation(138)))
                    .await
                    .is_err()
            );
            let mut final_turn = presentation(140);
            final_turn.finish = Some(runtime_collab::TaskPresentationFinish::Complete);
            report(&agent, &run, update(final_turn)).await.unwrap();
            assert!(
                report(&agent, &run, update(presentation(141)))
                    .await
                    .is_err()
            );
            let mut second = presentation(0);
            second.run = format!("{}:1", run.run_id);
            second.text = "é".repeat(runtime_collab::TaskPresentation::MAX_TEXT_BYTES);
            let second_id = second.run.clone();
            report(&agent, &run, update(second)).await.unwrap();
            let snapshot =
                WorkspaceTaskExecution::snapshot(f.native.as_ref(), &room().to_string()).unwrap();
            let retained = &snapshot.controls[&f.task].presentations[&second_id];
            assert_eq!(
                retained.finish,
                Some(runtime_collab::TaskPresentationFinish::Truncated)
            );
            assert!(retained.text.len() <= runtime_collab::TaskPresentation::MAX_TEXT_BYTES);
            assert!(!retained.detail.is_empty());
            for turn in 2..collab_types::task_presentation::MAX_TASK_PRESENTATIONS {
                let mut next = presentation(0);
                next.run = format!("{}:{turn}", run.run_id);
                report(&agent, &run, update(next)).await.unwrap();
            }
            let mut excess = presentation(0);
            excess.run = format!("{}:overflow", run.run_id);
            assert!(report(&agent, &run, update(excess)).await.is_err());
            report(
                &agent,
                &run,
                runtime_collab::TaskRunUpdate::Progress {
                    message: "Legacy public note".into(),
                    presentation: None,
                },
            )
            .await
            .unwrap();
            report(
                &agent,
                &run,
                runtime_collab::TaskRunUpdate::Succeeded {
                    message: "Done".into(),
                },
            )
            .await
            .unwrap();
            assert!(
                report(&agent, &run, update(presentation(150)))
                    .await
                    .is_err()
            );
            let snapshot =
                WorkspaceTaskExecution::snapshot(f.native.as_ref(), &room().to_string()).unwrap();
            assert!(
                snapshot.controls[&f.task]
                    .presentations
                    .values()
                    .all(|p| p.finish.is_some()),
                "closing authority must not leave interrupted replies working forever"
            );
            drop(f);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_approved_run_reconnects_and_steering_stays_pending_until_applied() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            assert!(
                f.state
                    .dispatch_workspace_task(&room().to_string(), &f.task)
                    .is_err(),
                "queueing is not approval"
            );
            let person = f.channel(99, false);
            assert!(
                person
                    .subscribe_task_runs_request()
                    .send()
                    .promise
                    .await
                    .is_err()
            );
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            assert_eq!(run.task_id, f.task);
            assert_eq!(run.execution_budget, None);
            report(&agent, &run, runtime_collab::TaskRunUpdate::Accepted)
                .await
                .unwrap();
            report(&agent, &run, runtime_collab::TaskRunUpdate::Accepted)
                .await
                .unwrap();
            f.person_command(WorkspaceTaskCommand::DirectionForRun {
                task_id: f.task.clone(),
                run_epoch: 1,
                after_revision: 0,
                text: "Keep compatibility examples".into(),
            })
            .unwrap();
            let control = observed_control(&agent, &run).await;
            assert_eq!(control.directions[0].text, "Keep compatibility examples");
            assert_eq!(control.applied_revision, 0);
            assert!(
                f.native
                    .owner
                    .with_task_action_admission(
                        room(),
                        &f.task,
                        "scribe",
                        1,
                        0,
                        || -> Result<(), String> { panic!("old plan must not enter") }
                    )
                    .is_err()
            );
            report(
                &agent,
                &run,
                runtime_collab::TaskRunUpdate::DirectionsApplied { revision: 1 },
            )
            .await
            .unwrap();
            f.state.remove_connection(100);
            let reconnect = f.channel(101, true);
            let resumed = delivered(&reconnect).await;
            assert_eq!(resumed.run_id, run.run_id);
            assert_eq!(resumed.lease, run.lease);
            report(
                &reconnect,
                &resumed,
                runtime_collab::TaskRunUpdate::DirectionsApplied { revision: 1 },
            )
            .await
            .unwrap();
            let state = f.native.owner.workspace_task_snapshot(room()).unwrap();
            assert_eq!(
                state.controls[&f.task]
                    .active_run
                    .as_ref()
                    .unwrap()
                    .applied_direction_revision,
                1
            );
            assert_eq!(state.controls[&f.task].directions.len(), 1);
            f.remove_agent();
            assert!(
                report(
                    &reconnect,
                    &resumed,
                    runtime_collab::TaskRunUpdate::Succeeded {
                        message: "forged after removal".into()
                    }
                )
                .await
                .is_err()
            );
            assert_eq!(
                f.native.owner.workspace_task_snapshot(room()).unwrap(),
                state
            );
            drop(f);
        })
        .await;
}

async fn observed_control(
    channel: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
) -> runtime_collab::TaskRunControl {
    let mut status = channel.task_run_cancelled_request();
    status.get().set_run_id(run.run_id);
    status.get().set_lease(&run.lease);
    let reply = status.send().promise.await.unwrap();
    serde_json::from_str(
        reply
            .get()
            .unwrap()
            .get_control_json()
            .unwrap()
            .to_str()
            .unwrap(),
    )
    .unwrap()
}

/// Re-ask for the child `invocation` and let the broker hold the answer for
/// up to `wait_ms` until it moves past `seen`.
async fn observe_held(
    channel: &channel_handle::Client,
    run: &runtime_collab::TaskRunEnvelope,
    invocation: uuid::Uuid,
    wait_ms: u32,
    seen: tilo_actions::remote::UpdateMark,
) -> Result<(u64, tilo_actions::InvocationUpdate), capnp::Error> {
    let mut request = channel.invoke_task_action_request();
    let mut p = request.get();
    p.set_run_id(run.run_id);
    p.set_lease(&run.lease);
    p.set_peer_id(200);
    p.set_resource("notes:release");
    p.set_action("append");
    p.set_invocation(invocation.to_string());
    p.set_wait_ms(wait_ms);
    p.set_seen_json(serde_json::to_string(&seen).unwrap());
    let response = request.send().promise.await?;
    let value = response.get()?;
    Ok((
        value.get_id(),
        serde_json::from_str(value.get_update_json()?.to_str().unwrap()).unwrap(),
    ))
}

/// A held re-ask of a running native child is answered when the child
/// settles, with its outcome, and not before; it starts nothing.
#[tokio::test(flavor = "current_thread")]
async fn a_held_native_reask_is_answered_when_the_child_settles() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            report(&agent, &run, runtime_collab::TaskRunUpdate::Accepted)
                .await
                .unwrap();
            let (target, stream) = notes_target(&f).await;
            grant_notes(&f);
            let id = uuid::Uuid::from_u128(4242);
            let (wire, accepted) = invoke(&agent, &run, id).await.unwrap();
            assert!(matches!(
                accepted,
                tilo_actions::InvocationUpdate::Accepted { .. }
            ));
            stream.read_request().send().promise.await.unwrap();
            let held = tokio::task::spawn_local({
                let agent = agent.clone();
                let run = run.clone();
                async move {
                    observe_held(
                        &agent,
                        &run,
                        id,
                        5000,
                        tilo_actions::remote::UpdateMark::Accepted,
                    )
                    .await
                }
            });
            tokio::time::sleep(Duration::from_millis(120)).await;
            assert!(!held.is_finished(), "held while the child runs");
            let terminal = tilo_actions::InvocationUpdate::Succeeded {
                invocation: tilo_actions::InvocationId(id),
                output: tilo_actions::ActionOutput::default(),
            };
            let mut update = target.update_invocation_request();
            update.get().set_id(wire);
            update
                .get()
                .set_update_json(serde_json::to_string(&terminal).unwrap());
            update.send().promise.await.unwrap();
            let answered = tokio::time::timeout(Duration::from_secs(1), held)
                .await
                .expect("answered once the child settled, not at the end of the hold")
                .unwrap()
                .unwrap();
            assert_eq!(answered, (wire, terminal));
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(30),
                    stream.read_request().send().promise
                )
                .await
                .is_err(),
                "a re-ask never delivers a second effect"
            );
            drop(f);
        })
        .await;
}

/// Cancelling the run from Task Center ends a held re-ask promptly: the
/// broker samples the run while it holds and cancels its children.
#[tokio::test(flavor = "current_thread")]
async fn a_held_native_reask_ends_when_the_run_is_cancelled() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            report(&agent, &run, runtime_collab::TaskRunUpdate::Accepted)
                .await
                .unwrap();
            let (_target, stream) = notes_target(&f).await;
            grant_notes(&f);
            let id = uuid::Uuid::from_u128(4343);
            invoke(&agent, &run, id).await.unwrap();
            stream.read_request().send().promise.await.unwrap();
            let held = tokio::task::spawn_local({
                let agent = agent.clone();
                let run = run.clone();
                async move {
                    observe_held(
                        &agent,
                        &run,
                        id,
                        5000,
                        tilo_actions::remote::UpdateMark::Accepted,
                    )
                    .await
                }
            });
            tokio::time::sleep(Duration::from_millis(60)).await;
            assert!(!held.is_finished(), "held while the child runs");
            f.person_command(WorkspaceTaskCommand::CancelForRun {
                task_id: f.task.clone(),
                run_epoch: 1,
                after_revision: 0,
            })
            .unwrap();
            let _answer = tokio::time::timeout(Duration::from_secs(1), held)
                .await
                .expect("a cancelled run ends the hold, not the hold's end")
                .unwrap();
            assert!(
                invoke(&agent, &run, uuid::Uuid::from_u128(4344))
                    .await
                    .is_err(),
                "nothing new starts in a cancelled run"
            );
            drop(f);
        })
        .await;
}
