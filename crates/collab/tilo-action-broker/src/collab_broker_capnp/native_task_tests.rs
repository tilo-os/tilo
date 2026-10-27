#![expect(
    clippy::future_not_send,
    reason = "Capnp RPC clients are intentionally thread confined to the existing LocalSet"
)]
use super::super::*;
use crate::peer_actions::{CertificateAuthority, OwnerPolicyClock, PolicyOrigin};
use crate::services::{
    NativeTaskActionRequirement, TrackerDirection, TrackerRun, TrackerRuns, WorkspaceAuthority,
    WorkspaceMemberships,
};
use ed25519_dalek::{Signer as _, SigningKey};
use tilo_action_policy::roles::AgentPreset;
use tilo_owner_commit::{CommitFuture, CommitOperation, OwnerCommitRequest, OwnerCommitService};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

#[path = "native_task_launch_tests.rs"]
mod closed_app;
#[path = "native_task_confirmation_tests.rs"]
mod confirmation;
#[path = "native_task_cross_app_tests.rs"]
mod cross_app;
#[path = "native_store_witness_tests.rs"]
mod store_witness;

struct Native {
    owner: tilo_joined_room::NativeWorkspaceOwner,
    key: SigningKey,
    certificate: tilo_user_identity::DeviceCert,
    origin: PolicyOrigin,
    cached_projection: Mutex<Option<tilo_desktop_model::DesktopTree>>,
    capped_connection: std::sync::atomic::AtomicBool,
    spent_grants: Mutex<Vec<tilo_action_policy::GrantId>>,
    /// The tracker's runs, as the person approves, steers and stops them.
    runs: Mutex<Vec<TrackerRun>>,
    /// The grants recorded as given by the person, while they may grant.
    granted: Mutex<Option<Vec<tilo_action_policy::Grant>>>,
}

/// The issue the fixture's agent runs.
const ISSUE: &str = "tasks.tracker/issues/release";
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
            let mut workspace = tilo_desktop_model::Workspace::in_room(
                tilo_desktop_model::WorkspaceId(1),
                room(),
                "Native execution",
            );
            workspace.policy =
                tilo_desktop_model::projection::room_policy_of(doc, &room().to_string());
            workspace
                .policy
                .grants
                .retain(|grant| !self.spent_grants.lock().unwrap().contains(&grant.id));
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
        self.granted.lock().unwrap().is_some()
    }
    fn record(
        &self,
        _request: &crate::app_actions::AppActionConsentRequest,
        grant: tilo_action_policy::Grant,
        _answer: crate::app_actions::AppActionConsent,
    ) -> Option<String> {
        let id = grant.id.as_str().to_owned();
        self.granted.lock().unwrap().as_mut()?.push(grant);
        Some(id)
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
    fn execute(&self, request: OwnerCommitRequest) -> CommitFuture {
        self.commit(request)
    }
}
impl TrackerRuns for Native {
    fn requires_room(&self, requested: &str) -> bool {
        OwnerCommitService::requires_room(self, requested)
    }
    fn runs(&self, requested: &str) -> Result<Vec<TrackerRun>, String> {
        if !OwnerCommitService::requires_room(self, requested) {
            return Err("foreign tracker room".into());
        }
        Ok(self.runs.lock().unwrap().clone())
    }
    fn check_agent(&self, requested: &str, agent: &str) -> Result<(), String> {
        if !OwnerCommitService::requires_room(self, requested) {
            return Err("foreign tracker room".into());
        }
        self.owner.with_current(room(), |view| {
            view.peer()
                .workspace()
                .check_admitted_agent(agent, std::time::SystemTime::now())
        })
    }
    fn authorize_action(
        &self,
        requested: &str,
        run: &TrackerRun,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String> {
        if !OwnerCommitService::requires_room(self, requested) {
            return Err("foreign tracker room".into());
        }
        self.owner.with_current(room(), |view| {
            let now = std::time::SystemTime::now();
            view.peer()
                .workspace()
                .check_admitted_agent(&run.agent, now)?;
            requirement.check_tracker(view.peer(), requested, run, now)
        })
    }
}
struct Fixture {
    directory: tempfile::TempDir,
    _fanout: Arc<FanoutState>,
    native: Arc<Native>,
    state: Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    task: String,
    _identity_env: tilo_test_env::ScopedEnv,
}
impl Fixture {
    fn new() -> Self {
        Self::with_launcher(None)
    }
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, end to end, reads as one test"
    )]
    fn with_launcher(launcher: Option<Arc<dyn crate::services::LauncherServices>>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut identity_env = tilo_test_env::ScopedEnv::new();
        identity_env
            .set("XDG_CONFIG_HOME", directory.path().join("config"))
            .set("TILO_IDENTITY_ROOT", directory.path().join("identity"))
            .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
        tilo_identity_host::provisioning::ensure_collaboration_identity(
            &directory.path().join("config/tilo/identity"),
            &tilo_user_identity::IdentityPaths::under(directory.path().join("identity")),
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .expect("provision the host admission and terminal receipt signer");
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
            runs: Mutex::new(Vec::new()),
            granted: Mutex::new(None),
        });
        let membership = OwnerCommitRequest {
            room: room().to_string(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: native.certificate.clone(),
                command: WorkspaceMembershipCommand::AddAgent {
                    agent: "scribe".into(),
                    // Reading only, as a fresh session admits an agent:
                    // what its runs may do comes from their starter.
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
        let task = ISSUE.to_owned();
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
                tracker_runs: Some(native.clone()),
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
            _identity_env: identity_env,
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
    /// Change the issue's run as the person does on the issue's page, and
    /// let the broker read it.
    fn on_run(&self, change: impl FnOnce(&mut TrackerRun)) {
        change(
            self.native
                .runs
                .lock()
                .unwrap()
                .iter_mut()
                .find(|run| run.issue == self.task)
                .expect("the issue has a run"),
        );
        self.state.refresh_native_task_delivery();
    }
    /// The person sends the run a message.
    fn direct(&self, text: &str) {
        self.on_run(|run| {
            run.directions.push(TrackerDirection {
                id: format!("message-{}", run.directions.len().saturating_add(1)),
                author: hex::encode(self.native.certificate.user_public_key),
                text: text.into(),
                at_ms: 1_000,
                incorporated: false,
            });
        });
    }
    /// The person stops the run.
    fn stop(&self) {
        self.on_run(|run| run.active = false);
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
    /// The person approves a run of the issue for the agent: the issue's
    /// next run.
    fn approve(&self) {
        self.approve_issue(&self.task);
    }
    fn approve_issue(&self, issue: &str) {
        {
            let mut runs = self.native.runs.lock().unwrap();
            let epoch = runs
                .iter()
                .find(|run| run.issue == issue)
                .map_or(1, |run| run.epoch.saturating_add(1));
            runs.retain(|run| run.issue != issue);
            runs.push(TrackerRun {
                issue: issue.into(),
                title: "Prepare release notes".into(),
                instruction: "Inspect every release-note section, preserve all cross references, and report every blocking inconsistency. ".repeat(4),
                agent: "scribe".into(),
                epoch,
                starter: hex::encode(self.native.certificate.user_public_key),
                active: true,
                budget: None,
                subject: None,
                model: None,
                directions: Vec::new(),
                question: None,
            });
        }
        self.state.refresh_native_task_delivery();
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
            connection_id: peer,
            verified_identity: admitted,
            record_kind: None,
            route_access: RouteAccess::Own,
            lease: None,
            _hold: None,
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
            next.publication_mut().durable_mutation_messages(
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
            let other = "tasks.tracker/issues/other".to_owned();
            f.approve_issue(&other);
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
    use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath};
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (_target, _stream) = notes_target(&f).await;
            grant_notes(&f);
            let append = |resource: &str| {
                tilo_actions::action_node(
                    &TreePath::room(room().to_string()),
                    "so.tilo.notes",
                    "document",
                    resource,
                    None,
                    "append",
                )
            };
            for (id, resource, answer) in [
                ("task-other-resource-only", "notes:other", Answer::Allow),
                ("task-not-release", "notes:release", Answer::Deny),
            ] {
                put_grant(
                    &f,
                    Grant::new(
                        id,
                        Subject::Task(f.task.clone()),
                        append(resource),
                        KindSet::UNDOABLE,
                        answer,
                        Subject::person(hex::encode(f.native.certificate.user_public_key)),
                    ),
                );
            }
            assert!(invoke(&agent, &run, uuid::Uuid::new_v4()).await.is_err());
            assert!(
                ordinary_invoke(&agent)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("authenticated task action admission")
            );
            assert_eq!(
                crate::collab_broker_capnp::unread_invocations(200),
                0,
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
            f.stop();
            assert!(invoke(&agent, &run, uuid::Uuid::new_v4()).await.is_err());
            assert!(
                ordinary_invoke(&agent)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("authenticated task action admission")
            );
            assert_eq!(
                crate::collab_broker_capnp::unread_invocations(200),
                0,
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
            assert_eq!(crate::collab_broker_capnp::unread_invocations(200), 0);
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

/// The agent may append to the notes, but a grant naming the issue denies
/// it at the same node: Deny wins, and the run may not.
#[tokio::test(flavor = "current_thread")]
async fn a_native_run_is_held_to_its_issues_grants() {
    use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath};
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (_target, _stream) = notes_target(&f).await;
            grant_notes(&f);
            put_grant(
                &f,
                Grant::new(
                    "task-no-append",
                    Subject::Task(f.task.clone()),
                    tilo_actions::action_node(
                        &TreePath::room(room().to_string()),
                        "so.tilo.notes",
                        "document",
                        "notes:release",
                        None,
                        "append",
                    ),
                    KindSet::UNDOABLE,
                    Answer::Deny,
                    Subject::person(hex::encode(f.native.certificate.user_public_key)),
                ),
            );
            assert!(
                invoke(&agent, &run, uuid::Uuid::from_u128(8100))
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("not allowed")
            );
            assert_eq!(
                crate::collab_broker_capnp::unread_invocations(200),
                0,
                "the refused action never reached the app"
            );
            drop(f);
        })
        .await;
}

/// A failed run ends with its messages; the person's next approval is a new
/// run of the issue, delivered as its own, and the old run reads as replaced.
#[tokio::test(flavor = "current_thread")]
async fn a_failed_run_is_replaced_by_the_next_approval_and_reads_as_replaced() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            f.direct("Ask me before changing the public API");
            let control = observed_control(&agent, &run).await;
            assert_eq!(
                control.directions[0].text,
                "Ask me before changing the public API"
            );
            report(
                &agent,
                &run,
                runtime_collab::TaskRunUpdate::Failed {
                    message: "Need a decision".into(),
                },
            )
            .await
            .unwrap();
            f.approve();
            let restarted = delivered(&agent).await;
            assert_ne!(restarted.run_id, run.run_id);
            let mut old = agent.task_run_cancelled_request();
            old.get().set_run_id(run.run_id);
            old.get().set_lease(&run.lease);
            assert!(
                old.send().promise.await.is_err(),
                "the replaced run no longer reads"
            );
            let control = observed_control(&agent, &restarted).await;
            assert_eq!(control.run_epoch, 2);
            assert!(control.directions.is_empty(), "the new run starts afresh");
            drop(f);
        })
        .await;
}

/// The fixture's agent was admitted reading only, as a fresh session admits
/// one. The run its person started still takes the change that person may
/// take, attributed to the agent, until the person's answer for the agent
/// denies it; a deleted target fails closed.
#[tokio::test(flavor = "current_thread")]
async fn a_started_run_acts_as_its_starter_and_deleted_targets_fail_closed() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            let (target, stream) = notes_target(&f).await;
            let id = uuid::Uuid::from_u128(800);
            let first = invoke(&agent, &run, id).await.unwrap();
            assert!(matches!(
                first.1,
                tilo_actions::InvocationUpdate::Accepted { .. }
            ));
            let delivered_request = stream.read_request().send().promise.await.unwrap();
            let wire = delivered_request.get().unwrap();
            assert_eq!(wire.get_actor().unwrap().to_str().unwrap(), "agent:scribe");
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
            // Observing the retained effect after a reconnect is held to the
            // starter too, not to the agent's admission.
            f.state.remove_connection(100);
            let agent = f.channel(101, true);
            let run = delivered(&agent).await;
            assert_eq!(invoke(&agent, &run, id).await.unwrap().1, terminal);

            // The person tells the agent it may not change Notes.
            let answer = |answer| {
                tilo_action_policy::agent_answers::grant(
                    &room().to_string(),
                    "scribe",
                    (
                        &tilo_action_policy::agent_answers::AnswerScope::app("so.tilo.notes"),
                        tilo_action_policy::ActionKind::Undoable,
                    ),
                    answer,
                    tilo_action_policy::Subject::person(hex::encode(
                        f.native.certificate.user_public_key,
                    )),
                )
            };
            put_grant(&f, answer(tilo_action_policy::Answer::Deny));
            assert!(
                invoke(&agent, &run, uuid::Uuid::from_u128(801))
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("not allowed")
            );
            assert_eq!(crate::collab_broker_capnp::unread_invocations(200), 0);
            put_grant(&f, answer(tilo_action_policy::Answer::Allow));
            f.state.remove_connection(200);
            assert!(
                invoke(&agent, &run, uuid::Uuid::from_u128(802))
                    .await
                    .is_err()
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
            f.approve();
            f.on_run(|run| run.budget = Some(budget));
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            assert_eq!(run.execution_budget, Some(budget));
            let replaced = delivered(&agent).await;
            assert_eq!(run, replaced);
            let (_target, _stream) = notes_target(&f).await;
            grant_notes(&f);
            f.stop();
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
            assert_eq!(crate::collab_broker_capnp::unread_invocations(200), 0);
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
            assert_eq!(context.group.as_ref().unwrap().id, run.group());
            let duplicate = invoke(&agent, &run, id).await.unwrap();
            assert_eq!(duplicate, first);
            assert_eq!(
                crate::collab_broker_capnp::unread_invocations(200),
                0,
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
            assert_eq!(crate::collab_broker_capnp::unread_invocations(200), 0);
            report(
                &reconnected,
                &resumed,
                runtime_collab::TaskRunUpdate::Succeeded {
                    message: "Release notes updated".into(),
                },
            )
            .await
            .unwrap();
            // The run goes to history once, as one run to undo whole.
            let ended = f.state.take_ended_task_runs();
            assert_eq!(
                ended
                    .iter()
                    .map(|ended| (
                        ended.group.as_str(),
                        ended.agent.as_str(),
                        ended.outcome.as_str()
                    ))
                    .collect::<Vec<_>>(),
                [(run.group().as_str(), "agent:scribe", "completed")]
            );
            f.native
                .owner
                .with_current(room(), |view| {
                    let effects = tilo_desktop_model::projection::workspace_effect_receipts_of(
                        view.peer().admitted_room().unwrap(),
                    );
                    assert_eq!(effects.len(), 1);
                    assert_eq!(effects[0].actor, "agent:scribe");
                    assert_eq!(effects[0].context.group.as_ref().unwrap().id, run.group());
                    Ok(())
                })
                .unwrap();
        })
        .await;
}

/// Nothing runs until the person approves; a message to the run stays
/// pending until its agent took it in, also across a reconnect, and an agent
/// removed from the room reads its run no more.
#[tokio::test(flavor = "current_thread")]
async fn native_approved_run_reconnects_and_steering_stays_pending_until_applied() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
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
            f.direct("Keep compatibility examples");
            let control = observed_control(&agent, &run).await;
            assert_eq!(control.directions[0].text, "Keep compatibility examples");
            assert_eq!(control.applied_revision, 0);
            let (_target, _stream) = notes_target(&f).await;
            grant_notes(&f);
            assert!(
                invoke(&agent, &run, uuid::Uuid::from_u128(5100))
                    .await
                    .is_err(),
                "a run acts only once it took in every message to it"
            );
            assert_eq!(crate::collab_broker_capnp::unread_invocations(200), 0);
            f.on_run(|run| run.directions[0].incorporated = true);
            f.state.remove_connection(100);
            let reconnect = f.channel(101, true);
            let resumed = delivered(&reconnect).await;
            assert_eq!(resumed.run_id, run.run_id);
            let control = observed_control(&reconnect, &resumed).await;
            assert_eq!(control.applied_revision, 1);
            f.remove_agent();
            assert!(
                invoke(&reconnect, &resumed, uuid::Uuid::from_u128(5101))
                    .await
                    .is_err(),
                "a removed agent acts no more"
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

/// Wait until a re-ask spawned after `before` was counted holds on the
/// broker's invocation changes.
async fn holding(f: &Fixture, before: usize) {
    tilo_test_env::within("the re-ask to hold", async {
        while f.state.invocation_generation.receiver_count() <= before {
            tokio::task::yield_now().await;
        }
    })
    .await;
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
            let before = f.state.invocation_generation.receiver_count();
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
            holding(&f, before).await;
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
            // The update is answered once its outcome is on disk. From here
            // the clock moves only while every task waits: the settled
            // outcome, not a sample or the hold's end, must answer.
            tokio::time::pause();
            let settled_at = tokio::time::Instant::now();
            let answered = tilo_test_env::within("the held re-ask's answer", held)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                tokio::time::Instant::now(),
                settled_at,
                "answered once the child settled, not at a later sample or the end of the hold"
            );
            tokio::time::resume();
            assert_eq!(answered, (wire, terminal));
            assert_eq!(
                crate::collab_broker_capnp::unread_invocations(200),
                0,
                "a re-ask never delivers a second effect"
            );
            drop(f);
        })
        .await;
}

/// Cancelling the run from Task Center ends a held re-ask promptly: the
/// stop is announced, and the broker reading it cancels the children.
#[tokio::test(flavor = "current_thread")]
async fn a_held_native_reask_ends_when_the_run_is_cancelled() {
    const HOLD_MS: u32 = 5000;
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
            // The clock moves only while every task waits, so the hold is
            // timed by the test, from before it starts.
            tokio::time::pause();
            let asked_at = tokio::time::Instant::now();
            let before = f.state.invocation_generation.receiver_count();
            let held = tokio::task::spawn_local({
                let agent = agent.clone();
                let run = run.clone();
                async move {
                    observe_held(
                        &agent,
                        &run,
                        id,
                        HOLD_MS,
                        tilo_actions::remote::UpdateMark::Accepted,
                    )
                    .await
                }
            });
            holding(&f, before).await;
            assert!(!held.is_finished(), "held while the child runs");
            f.stop();
            tilo_identity_host::policy_change::announce();
            let _answer = tilo_test_env::within("the cancelled re-ask's answer", held)
                .await
                .unwrap();
            assert!(
                asked_at.elapsed() < Duration::from_millis(u64::from(HOLD_MS)),
                "a cancelled run ends the hold when it is announced, not at the hold's end"
            );
            tokio::time::resume();
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
