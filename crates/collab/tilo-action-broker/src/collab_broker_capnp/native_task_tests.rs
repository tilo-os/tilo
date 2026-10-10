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
use tilo_peer_state::owner_commit::{
    CommitFuture, CommitOperation, OwnerCommitRequest, OwnerCommitService,
};
use tilo_peer_state::workspace_membership::{AgentMembershipPreset, WorkspaceMembershipCommand};
use tilo_peer_state::workspace_task::{
    WorkspaceTaskAuthority, WorkspaceTaskCommand, WorkspaceTaskSnapshot,
};

struct Native {
    owner: tilo_joined_room::NativeWorkspaceOwner,
    key: SigningKey,
    certificate: tilo_user_identity::DeviceCert,
    origin: PolicyOrigin,
    cached_projection: Mutex<Option<tilo_desktop_model::DesktopTree>>,
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
            let (_, owner, capabilities) = tilo_peer_state::room::extract_governed_fields(doc);
            let mut workspace = tilo_desktop_model::Workspace::new(
                tilo_desktop_model::WorkspaceId(1),
                "Native execution",
            );
            workspace.room = room();
            workspace.owner = owner;
            workspace.capabilities = capabilities;
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
            view.peer()
                .check_task_execution_actor(agent, None, std::time::SystemTime::now())?;
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
            view.peer().check_task_execution_actor(agent, None, now)?;
            requirement.check(view.peer(), requested, agent, now)
        })
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
        let directory = tempfile::tempdir().unwrap();
        let owner = tilo_joined_room::open_file(&directory.path().join("native.json")).unwrap();
        let key = SigningKey::from_bytes(&[92; 32]);
        let certificate = tilo_joined_room::testing::certificate(
            &SigningKey::from_bytes(&[91; 32]),
            &key,
            "Governor",
        );
        tilo_joined_room::testing::enroll_interactive_governor(&owner, &key, &certificate).unwrap();
        let native = Arc::new(Native {
            owner,
            key,
            certificate,
            origin: OwnerPolicyClock::new().unwrap().new_origin(),
            cached_projection: Mutex::new(None),
        });
        let membership = OwnerCommitRequest {
            room: room().to_string(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: native.certificate.clone(),
                command: WorkspaceMembershipCommand::AddAgent {
                    agent: "scribe".into(),
                    preset: AgentMembershipPreset::Operator,
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
                        command: WorkspaceTaskCommand::Queue {
                            title: "Prepare release notes".into(),
                            assigned_agent: "scribe".into(),
                            route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                            resource: None,
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
        let room = room().to_string();
        let proof = runtime_collab::CertifiedJoinIdentity {
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
        .unwrap();
        let mut admitted = self
            .state
            .verify_action_caller(&room, &proof)
            .unwrap()
            .admitted;
        let route = CrdtRoute::record(
            if agent {
                "so.tilo.agent"
            } else {
                "so.tilo.notes"
            },
            room,
            "main",
        );
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
            permission: tilo_actions::PermissionRequirement::Write,
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
    use collab_types::caps::{CapOverride, Effect, Scope, Subject, Verb};
    let now = std::time::SystemTime::now();
    let messages = f
        .native
        .owner
        .with_current(room(), |view| {
            let before = view.peer().clone();
            let mut next = before.clone();
            next.put_governed_capabilities(
                &[CapOverride::new(
                    Subject::agent("scribe"),
                    Scope::Action {
                        app: "so.tilo.notes".into(),
                        resource: "notes:release".into(),
                        action: "append".into(),
                    },
                    if may_write {
                        Effect::Grant
                    } else {
                        Effect::Restrict
                    },
                    if may_write { Verb::Write } else { Verb::Read },
                )
                .with_id(if may_write {
                    "fixture-notes-append"
                } else {
                    "fixture-notes-restrict"
                })],
                &f.native.key,
                now,
            )?;
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

#[tokio::test(flavor = "current_thread")]
async fn native_live_owner_revocation_fences_new_and_retained_actions_with_stale_projection() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
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
                    .contains("owner")
            );
            assert!(
                invoke(&agent, &run, uuid::Uuid::from_u128(8011))
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("owner")
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
                    .contains("authorized")
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
