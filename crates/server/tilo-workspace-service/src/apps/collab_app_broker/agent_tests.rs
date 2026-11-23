#![expect(
    clippy::future_not_send,
    reason = "the broker's Rc-backed channel handles stay on the test LocalSet"
)]
use super::*;
use crate::apps::certificate_authority::CertificateAuthority;
use crate::apps::policy_clock::{OwnerPolicyClock, PolicyOrigin};
use crate::apps::services::{WorkspaceAuthority, WorkspaceMemberships};
use ed25519_dalek::{Signer as _, SigningKey};
use tilo_peer_state::owner_commit::{
    CommitFuture, CommitOperation, OwnerCommitRequest, OwnerCommitService,
};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

#[path = "agent_session_tests.rs"]
mod agent_session;

struct Native {
    owner: tilo_peer_state::owner::NativeWorkspaceOwner,
    key: SigningKey,
    certificate: tilo_user_identity::DeviceCert,
    origin: PolicyOrigin,
    cached_projection: Mutex<Option<tilo_desktop_model::Layout>>,
    capped_connection: std::sync::atomic::AtomicBool,
    spent_grants: Mutex<Vec<tilo_action_policy::GrantId>>,
    /// The grants recorded as given by the person, while they may grant.
    granted: Mutex<Option<Vec<tilo_action_policy::Grant>>>,
    /// The workspace's governor records no effect receipt: one it is asked for
    /// is refused, as a governor out of reach refuses it.
    receipts_refused: std::sync::atomic::AtomicBool,
    /// The owner is still opening, as a session's is at its start: health
    /// says so until it is open.
    opening: std::sync::atomic::AtomicBool,
    /// The agent sessions going, each with what its person allowed it.
    sessions: Mutex<Vec<(String, crate::apps::services::GoingSession)>>,
    /// The agents the workspace's record of its agents names.
    listed: Mutex<Vec<crate::apps::services::ListedAgent>>,
}

/// The session the fixture's agent works in.
const SESSION: &str = "so.tilo.session/agent-session/release";
/// The grant admitting the fixture's agent to read the workspace.
const AGENT_GRANT: &str = "agent-member:scribe";
fn workspace_id() -> tilo_desktop_model::WorkspaceId {
    tilo_desktop_model::WorkspaceId::parse(tilo_peer_state::owner::testing::WORKSPACE).unwrap()
}
impl crate::apps::services::AgentRoster for Native {
    fn listed(&self, _: &str) -> Vec<crate::apps::services::ListedAgent> {
        self.listed.lock().unwrap().clone()
    }

    fn lists(&self, workspace: &str, agent: &str) -> bool {
        let mut granted = false;
        let read = self.with_tree(&mut |tree| {
            granted = crate::apps::services::workspace_in(tree, workspace).is_some_and(|held| {
                held.policy.grants.iter().any(|grant| {
                    grant.node == held.node()
                        && grant.answer == Answer::Allow
                        && grant.grantee.agent_id() == Some(agent)
                })
            });
        });
        read.is_ok()
            && (granted
                || self
                    .listed(workspace)
                    .iter()
                    .any(|listed| listed.id == agent))
    }
}

impl WorkspaceAuthority for Native {
    fn with_tree(&self, read: &mut dyn FnMut(&tilo_desktop_model::Layout)) -> Result<(), String> {
        let tree = WorkspaceAuthority::snapshot(self)?;
        read(&tree);
        Ok(())
    }

    fn snapshot(&self) -> Result<tilo_desktop_model::Layout, String> {
        let cached = self.cached_projection.lock().unwrap().clone();
        if let Some(cached) = cached {
            return Ok(cached);
        }
        self.owner.with_current(workspace_id(), |view| {
            let doc = view
                .peer()
                .admitted_workspace()
                .ok_or("native document unavailable")?;
            let mut workspace =
                tilo_desktop_model::Workspace::new(workspace_id(), "Native execution");
            workspace.policy =
                tilo_peer_state::workspace::policy_of(doc, &workspace_id().to_string());
            workspace
                .policy
                .grants
                .retain(|grant| !self.spent_grants.lock().unwrap().contains(&grant.id));
            let mut tree = tilo_desktop_model::Layout {
                active: workspace.id(),
                ..Default::default()
            };
            tree.workspaces.push(workspace);
            Ok(tree)
        })
    }
}
impl CertificateAuthority for Native {
    fn policy_time(&self) -> Result<Option<crate::apps::policy_clock::PolicyTime>, String> {
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
        if requested.to_string() != workspace_id().to_string() {
            return Err("foreign workspace".into());
        }
        self.owner.with_current(workspace_id(), |view| {
            if !view.peer().certificates().contains(cert) {
                return Err("certificate removed".into());
            }
            view.peer()
                .permissions(cert.device_public_key, std::time::SystemTime::now())
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }
    fn governs(&self, requested: uuid::Uuid, device: Option<&[u8; 32]>) -> bool {
        requested.to_string() == workspace_id().to_string()
            && !(self
                .capped_connection
                .load(std::sync::atomic::Ordering::SeqCst)
                && device.is_some())
            && device.is_none_or(|key| *key == self.certificate.device_public_key)
    }
}
impl OwnerCommitService for Native {
    fn health(&self) -> Result<(), String> {
        if self.opening.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("the native owner is still opening; try again in a moment".into());
        }
        self.owner.health()
    }
    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        let owner = self.owner.clone();
        let key = self.key.clone();
        let receipts_refused = self
            .receipts_refused
            .load(std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            // A real owner commits asynchronously: let other requests run
            // first, as they do while a commit is on its way.
            tokio::task::yield_now().await;
            let receipt = match request.operation {
                CommitOperation::EffectReceipt { .. } if receipts_refused => {
                    Err("the workspace's governor records no receipt".into())
                }
                CommitOperation::EffectReceipt { .. } => {
                    owner.commit_effect_receipt(&request, std::time::SystemTime::now())
                }
                CommitOperation::WorkspaceMembershipCommand { .. } => {
                    owner
                        .commit_workspace_membership_command(
                            request.clone(),
                            &key,
                            std::time::SystemTime::now(),
                        )
                        .await
                }
                _ => Err("unsupported fixture owner operation".into()),
            }?;
            receipt.validate_for(&request)?;
            Ok(receipt)
        })
    }
}
impl crate::apps::services::WorkspaceGrants for Native {
    fn may_record(&self, _workspace: &str, _person: &str) -> bool {
        self.granted.lock().unwrap().is_some()
    }
    fn record(
        &self,
        _request: &crate::apps::app_actions::AppActionConsentRequest,
        grant: tilo_action_policy::Grant,
    ) -> crate::apps::services::GrantRecording {
        let id = grant.id.as_str().to_owned();
        let recorded = self
            .granted
            .lock()
            .unwrap()
            .as_mut()
            .map(|granted| granted.push(grant))
            .map(|()| id)
            .ok_or_else(|| "nothing is granted here".to_owned());
        Box::pin(std::future::ready(recorded))
    }
    fn revoke(&self, requested: &str, grant: &str) {
        assert_eq!(requested, workspace_id().to_string());
        // Like the shell's grant service, update the broker's live policy
        // projection synchronously without re-entering the task mutation turn.
        self.spent_grants
            .lock()
            .unwrap()
            .push(tilo_action_policy::GrantId::new(grant));
    }
}

impl WorkspaceMemberships for Native {
    fn requires_workspace(&self, requested: &str) -> bool {
        requested == workspace_id().to_string()
    }
    fn health(&self) -> Result<(), String> {
        self.owner.health()
    }
    fn execute(&self, request: OwnerCommitRequest) -> CommitFuture {
        self.commit(request)
    }
}
impl crate::apps::services::AgentSessions for Native {
    fn going(
        &self,
        _workspace: &str,
        _agent: &str,
        session: &str,
    ) -> Option<crate::apps::services::GoingSession> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .find(|(id, _)| id == session)
            .map(|(_, going)| going.clone())
    }

    fn keep_answer(&self, session: &str, grant: tilo_action_policy::Grant) {
        if let Some((_, going)) = self
            .sessions
            .lock()
            .unwrap()
            .iter_mut()
            .find(|(id, _)| id == session)
        {
            going.answers.push(grant);
        }
    }
}
struct Fixture {
    _directory: tempfile::TempDir,
    _fanout: Arc<FanoutState>,
    native: Arc<Native>,
    state: Arc<AppBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    _identity_env: tilo_test_env::ScopedEnv,
}
impl Fixture {
    fn new() -> Self {
        Self::with_launcher(None)
    }
    fn with_launcher(launcher: Option<Arc<dyn crate::apps::services::LauncherServices>>) -> Self {
        Self::with_services(launcher, Vec::new())
    }
    /// The fixture with the host's own steps `host_steps` beside the apps.
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, end to end, reads as one test"
    )]
    fn with_services(
        launcher: Option<Arc<dyn crate::apps::services::LauncherServices>>,
        host_steps: Vec<Arc<dyn crate::apps::services::HostStepServices>>,
    ) -> Self {
        Self::with_origin(launcher, host_steps, None)
    }
    /// As [`Self::with_services`], with `resource_changes` the origin of the
    /// workspace's records.
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, end to end, reads as one test"
    )]
    fn with_origin(
        launcher: Option<Arc<dyn crate::apps::services::LauncherServices>>,
        host_steps: Vec<Arc<dyn crate::apps::services::HostStepServices>>,
        resource_changes: Option<Arc<dyn crate::apps::services::DocumentChangeServices>>,
    ) -> Self {
        Self::built(launcher, host_steps, resource_changes, true)
    }
    /// The fixture, its signed device `foreign` to this host or one of its own.
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, end to end, reads as one test"
    )]
    fn built(
        launcher: Option<Arc<dyn crate::apps::services::LauncherServices>>,
        host_steps: Vec<Arc<dyn crate::apps::services::HostStepServices>>,
        resource_changes: Option<Arc<dyn crate::apps::services::DocumentChangeServices>>,
        foreign: bool,
    ) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut identity_env = tilo_test_env::ScopedEnv::new();
        identity_env
            .set("XDG_CONFIG_HOME", directory.path().join("config"))
            .set("TILO_IDENTITY_ROOT", directory.path().join("identity"))
            .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
        tilo_identity::provisioning::ensure_collaboration_identity(
            &directory.path().join("config/tilo/identity"),
            &tilo_device_identity::IdentityPaths::under(directory.path().join("identity")),
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .expect("provision the host admission and terminal receipt signer");
        let owner =
            tilo_peer_state::owner::open_file(&directory.path().join("native.json")).unwrap();
        let key = SigningKey::from_bytes(&[92; 32]);
        let mut certificate = tilo_peer_state::owner::testing::certificate(
            &SigningKey::from_bytes(&[91; 32]),
            &key,
            "Governor",
        );
        // The signed device is initially governed here, so its foreign-device
        // ceiling only applies after the live connection loses that authority.
        certificate.capabilities.foreign = foreign;
        tilo_user_identity::sign_device_cert(&mut certificate, &SigningKey::from_bytes(&[91; 32]))
            .unwrap();
        tilo_peer_state::owner::testing::enroll_interactive_governor(&owner, &key, &certificate)
            .unwrap();
        let native = Arc::new(Native {
            owner,
            key,
            certificate,
            origin: OwnerPolicyClock::new().unwrap().new_origin(),
            cached_projection: Mutex::new(None),
            capped_connection: std::sync::atomic::AtomicBool::new(false),
            spent_grants: Mutex::new(Vec::new()),
            granted: Mutex::new(None),
            receipts_refused: std::sync::atomic::AtomicBool::new(false),
            opening: std::sync::atomic::AtomicBool::new(false),
            sessions: Mutex::new(Vec::new()),
            listed: Mutex::new(Vec::new()),
        });
        let membership = OwnerCommitRequest {
            workspace: workspace_id().to_string(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: native.certificate.clone(),
                // Reading only, as a fresh session admits an agent: what
                // its runs may do comes from their starter.
                command: WorkspaceMembershipCommand::Grant {
                    grant: tilo_action_policy::Grant::new(
                        AGENT_GRANT,
                        tilo_action_policy::Grantee::agent("scribe"),
                        tilo_action_policy::TreePath::workspace(workspace_id().to_string()),
                        tilo_action_policy::KindSet::READ,
                        tilo_action_policy::Answer::Allow,
                        tilo_action_policy::Grantee::person(hex::encode(
                            native.certificate.user_public_key,
                        )),
                    ),
                },
            },
        };
        futures::executor::block_on(native.owner.commit_workspace_membership_command(
            membership,
            &native.key,
            std::time::SystemTime::now(),
        ))
        .unwrap();
        let fanout = Arc::new(FanoutState::new());
        let config = crate::apps::NativePeerBrokerConfig {
            journal_root: directory.path().join("journal"),
            certificate_authority: native.clone(),
        };
        let (state, rx) = AppBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::with_optional_owner_commit(None, native.clone()),
            Some(native.clone()),
            None,
            Some(&config),
            ProcessServices {
                launcher,
                workspace_grants: Some(native.clone()),
                agents: Some(native.clone()),
                agent_sessions: Some(native.clone()),
                workspace_memberships: Some(native.clone()),
                host_steps,
                resource_changes,
                ..Default::default()
            },
        );
        Self {
            _directory: directory,
            _fanout: fanout,
            native,
            state,
            directories: Rc::new(LocalActionDirectoryRegistry::new(rx)),
            _identity_env: identity_env,
        }
    }
    /// The workspace's record of its agents names the fixture's agent, kept by its person.
    fn list_scribe(&self) {
        let mut listed = self.native.listed.lock().unwrap();
        if !listed.iter().any(|agent| agent.id == "scribe") {
            listed.push(crate::apps::services::ListedAgent {
                id: "scribe".into(),
                name: "Scribe".into(),
                keeper: String::new(),
            });
        }
    }
    fn signed_presentation(&self) -> Vec<u8> {
        let workspace = workspace_id().to_string();
        runtime_collab::CertifiedJoinIdentity {
            workspace_id: workspace.clone(),
            device_certificate: serde_json::to_vec(&self.native.certificate).unwrap(),
            proof: self
                .native
                .key
                .sign(&tilo_identity_client::collaboration_join_digest(&workspace).unwrap())
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap()
    }
    fn app_handle(&self, peer: u64, app: &str, agent: bool) -> JoinedChannel {
        let workspace = workspace_id().to_string();
        let mut admitted = self
            .state
            .verify_action_caller(&workspace, &self.signed_presentation())
            .unwrap()
            .admitted;
        let route = CrdtRoute::record(app, workspace, "main");
        if agent {
            let kept = crate::apps::test_support::KeptAgents::default()
                .listing(&route.cluster_id, "scribe");
            admit_listed_agent(&kept, &route.cluster_id, &mut admitted, "scribe").unwrap();
        }
        self.admitted_handle(peer, &route, admitted)
    }
    /// The handle a channel on `route` is served over, admitted as
    /// `admitted`, on `fixture`'s broker.
    fn admitted_handle(
        &self,
        peer: u64,
        route: &CrdtRoute,
        admitted: AdmittedJoinIdentity,
    ) -> JoinedChannel {
        JoinedChannel {
            as_app: None,
            committed_read: Rc::new(std::cell::RefCell::new(committed_document::Slot::default())),
            state: self.state.clone(),
            directories: self.directories.clone(),
            channel_id: route.channel_key(),
            peer_id: peer,
            connection_id: peer,
            verified_identity: admitted,
            resource_type: None,
            route_access: RouteAccess::Own,
            lease: None,
            _hold: None,
        }
    }
}

/// The Notes app's published append action, the agent's target.
fn notes_append() -> runtime_collab::ExportedAction {
    runtime_collab::ExportedAction {
        did: None,
        semantic_identity: None,
        resource: "notes:release".into(),
        action: "append".into(),
        resource_kind: "document".into(),
        resource_label: "Release notes".into(),
        action_label: "Append".into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        state: tilo_actions::ActionState::default(),
        operation_kind: tilo_actions::OperationKind::Effect,
        params: Vec::new(),
        contract: tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Undoable,
            execution: tilo_actions::ExecutionMode::Background { cancellable: true },
            result: tilo_actions::ResultSpec::Job,
            ..Default::default()
        },
        input: runtime_collab::ExportedInput::None,
        view: None,
    }
}

/// The Notes app as peer 200: its channel, and its invocation feed read on
/// the broker's side so what it was not handed stays countable.
async fn notes_target(f: &Fixture) -> (runtime_collab::ChannelHandle, InvocationStreamServer) {
    let handle = Rc::new(f.app_handle(200, "so.tilo.notes", false));
    let target = wire_channel_rc(Rc::clone(&handle));
    target.publish_actions(&[notes_append()]).await.unwrap();
    let (stream, _end) = handle.invocation_feed(true).unwrap();
    (target, stream)
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
        .with_current(workspace_id(), |view| {
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
                workspace_id(),
                tilo_peer_state::TransportOrigin::RetainedServerDelta(
                    f.native.key.verifying_key().to_bytes(),
                ),
                message,
                now,
            )
            .unwrap();
    }
}
/// A membership change whose outcome the broker cannot record (its effect
/// receipt is refused) does not leave whoever asked for it waiting, as an
/// opening app's authorship was left: the asker hears at once that no
/// outcome was recorded, and the workspace's membership service goes on serving.
/// The receipt prepared for the change admits only the owner's own
/// terminal, so the journal keeps the change for recovery to answer.
#[tokio::test(flavor = "current_thread")]
async fn a_membership_change_whose_receipt_is_refused_does_not_leave_its_asker_waiting() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.native
                .receipts_refused
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let workspace = workspace_id().to_string();
            f.state.refresh_workspace_membership_services().unwrap();
            assert!(
                f.state.membership_published(&workspace),
                "the workspace's membership actions are offered"
            );
            let job = f
                .state
                .invoke_workspace_membership(
                    &workspace,
                    &WorkspaceMembershipCommand::Settings(
                        tilo_peer_state::workspace_membership::SettingsChange::Rename {
                            name: "Studio".into(),
                        },
                    ),
                    &f.native.certificate,
                )
                .unwrap();
            let deadline = tilo_test_env::hang_deadline();
            let heard = loop {
                match job.completion.try_recv() {
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    heard => break heard,
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the asker was left waiting on a membership service that stopped"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            if let Ok(completed) = heard {
                assert!(
                    !completed.code.is_empty(),
                    "a change whose outcome was not recorded never reads as done"
                );
            }
            tokio::task::yield_now().await;
            assert!(
                f.state.membership_published(&workspace),
                "one change's unrecorded outcome does not take the workspace's membership service away"
            );
            drop(f);
        })
        .await;
}

/// Apps opened together at a session's start each ask that they may author
/// the workspace, and their grants queue on the workspace's membership service. The
/// first grant's outcome cannot be recorded while the native owner is still
/// opening; that grant is told why, and every grant behind it still
/// completes on the same service: none hears that its outcome is unknown
/// because the service went away, which left the apps' windows refused
/// ("This workspace's host did not let you add ...") on their first open.
#[tokio::test(flavor = "current_thread")]
async fn grants_in_flight_survive_an_unrecorded_one_while_the_owner_opens() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let workspace = workspace_id().to_string();
            f.state.refresh_workspace_membership_services().unwrap();
            assert!(f.state.membership_published(&workspace));
            let grant = |name: &str| {
                f.state
                    .invoke_workspace_membership(
                        &workspace,
                        &WorkspaceMembershipCommand::Settings(
                            tilo_peer_state::workspace_membership::SettingsChange::Rename {
                                name: name.into(),
                            },
                        ),
                        &f.native.certificate,
                    )
                    .unwrap()
            };
            f.native
                .opening
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let first = grant("Notes");
            let first = answer_of(&first).await;
            f.native
                .opening
                .store(false, std::sync::atomic::Ordering::SeqCst);
            let queued = ["Tasks", "Chat", "Board"].map(grant);
            for job in &queued {
                let answer = answer_of(job).await;
                assert!(
                    answer.code.is_empty(),
                    "a grant behind an unrecorded one completes: {} {}",
                    answer.code,
                    answer.message
                );
            }
            assert!(
                !first.message.contains("disconnect"),
                "the unrecorded grant is told why, not that its service went away: {}",
                first.message
            );
            assert!(
                first.code.is_empty() || first.message.contains("still opening"),
                "the unrecorded grant hears the owner's own reason: {} {}",
                first.code,
                first.message
            );
            assert!(f.state.membership_published(&workspace));
            drop(f);
        })
        .await;
}

/// The answer `job` hears, waited for while the broker's loop runs.
async fn answer_of(job: &super::InvocationJob) -> InvocationCompletion {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        match job.completion.try_recv() {
            Ok(answer) => return answer,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                panic!("an asker was left without an answer")
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "an asker was left waiting on the workspace's membership service"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

use tilo_actions::remote::{ActionCallError, Caller, InvocationReceipt, InvokeRequest};

/// A host's own step that is a turn's (`for_turns`): the session's layout, out here.
#[derive(Default)]
struct TurnSteps {
    taken: Mutex<Vec<tilo_actions::MemberId>>,
    /// The turn each step was taken on, as the session reads it to stand what the step shows
    /// beside the turn that asked.
    on_turns: Mutex<Vec<Option<tilo_action_policy::turn::Turn>>>,
}

impl crate::apps::services::HostStepServices for TurnSteps {
    fn provider(&self) -> &'static str {
        tilo_actions::remote::session_actions::PROVIDER
    }
    fn app_id(&self) -> &'static str {
        tilo_actions::remote::session_actions::PROVIDER
    }
    fn for_turns(&self) -> bool {
        true
    }
    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        vec![tilo_actions::ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new("shell:layout", "place"),
            resource_kind: "shell".into(),
            resource_label: "The person's layout".into(),
            action_label: "Put a resource on the canvas".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: tilo_actions::ExecutionLocus::default(),
            kind: tilo_actions::OperationKind::Effect,
            contract: tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Undoable,
                ..tilo_actions::ActionContract::default()
            },
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        }]
    }
    fn start(
        &self,
        request: &crate::apps::services::HostStepRequest<'_>,
    ) -> Result<tilo_actions::remote::Pending<crate::apps::services::HostStepOutcome>, String> {
        self.taken
            .lock()
            .unwrap()
            .push(request.actor.member().clone());
        self.on_turns
            .lock()
            .unwrap()
            .push(request.actor.turn().cloned());
        Ok(tilo_actions::remote::Pending::ready(Ok(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

/// A step marked for turns is an agent session's step, as an app action is: the agent takes it on
/// the workspace's grants while its session goes, and an agent in no session that goes — or one
/// the grants refuse — takes none.
#[tokio::test(flavor = "current_thread")]
async fn an_agents_session_steps_are_taken_while_it_goes_and_nobody_elses_are() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let steps = Arc::new(TurnSteps::default());
            let f = Fixture::with_services(None, vec![steps.clone()]);
            f.list_scribe();
            f.native.sessions.lock().unwrap().push((
                SESSION.into(),
                crate::apps::services::GoingSession::default(),
            ));
            grant_the_layout(&f, true);
            let person = hex::encode(f.native.certificate.user_public_key);

            // The agent working in its session, answering for the person.
            let agent = Caller::local_agent("scribe")
                .acting_for(person.clone())
                .in_workspace(workspace_id().to_string())
                .in_agent_session(SESSION)
                .in_new_execution();
            stray_invoke(&f.state, &agent, 92).expect("the session takes the session's step");
            assert_eq!(
                steps.taken.lock().unwrap().as_slice(),
                [tilo_actions::MemberId::agent("scribe")],
                "the step ran once, as the session's agent"
            );

            // A turn's step is listed to the agent working in its session, and to the person at
            // this device as their other steps are: the REPL types what the agent would.
            let listed = |caller: &Caller| {
                let actor = BrokerActor::for_caller(caller);
                f.state
                    .action_catalogue_for(&workspace_id().to_string(), &actor)
                    .unwrap()
                    .iter()
                    .any(|surface| {
                        surface.provider.as_deref()
                            == Some(tilo_actions::remote::session_actions::PROVIDER)
                    })
            };
            assert!(
                listed(&agent),
                "the agent working in its session is offered the session's step"
            );
            let local =
                Caller::local_person(person.clone()).in_workspace(workspace_id().to_string());
            assert!(
                listed(&local),
                "the person at this device is offered the session's step too"
            );

            // An agent whose session is over takes no step.
            let ended = Caller::local_agent("scribe")
                .acting_for(person)
                .in_workspace(workspace_id().to_string())
                .in_agent_session("so.tilo.session/agent-session/over")
                .in_new_execution();
            let refused = stray_invoke(&f.state, &ended, 93);
            assert!(
                matches!(refused, Err(ActionCallError::Refused { .. })),
                "an agent whose session is over takes no step: {refused:?}"
            );
            assert_eq!(steps.taken.lock().unwrap().len(), 1, "nothing ran for it");

            // The grants refuse it: the same refusal an app action hears.
            grant_the_layout(&f, false);
            let refused = stray_invoke(&f.state, &agent, 94);
            assert!(
                matches!(
                    refused,
                    Err(ActionCallError::Refused { .. } | ActionCallError::NotPermitted)
                ),
                "the grants refuse the step: {refused:?}"
            );
            assert_eq!(steps.taken.lock().unwrap().len(), 1, "nothing ran for it");
            drop(f);
        })
        .await;
}

/// Let the turn's agent `place` on the session's layout, or refuse it, as the person who
/// started the turn.
fn grant_the_layout(f: &Fixture, may: bool) {
    use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
    let node = tilo_actions::action_node(
        &TreePath::workspace(workspace_id().to_string()),
        tilo_actions::remote::session_actions::PROVIDER,
        "shell",
        "shell:layout",
        None,
        "place",
    );
    put_grant(
        f,
        Grant::new(
            if may {
                "fixture-layout-place"
            } else {
                "fixture-layout-deny"
            },
            Grantee::agent("scribe"),
            node,
            if may { KindSet::UNDOABLE } else { KindSet::ALL },
            if may { Answer::Allow } else { Answer::Deny },
            Grantee::person(hex::encode(f.native.certificate.user_public_key)),
        ),
    );
}

/// The step of [`TurnSteps`], invoked by `caller` through the broker as the `invocation`.
fn stray_invoke(
    state: &Arc<AppBrokerState>,
    caller: &Caller,
    invocation: u128,
) -> Result<InvocationReceipt, ActionCallError> {
    state
        .action_invoke(
            caller,
            None,
            None,
            &InvokeRequest {
                invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(invocation)),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: tilo_actions::remote::session_actions::PROVIDER.into(),
                    address: tilo_actions::ActionAddress::new("shell:layout", "place"),
                },
                argument: tilo_actions::remote::InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                appearance: None,
                wait_ms: None,
                signed: None,
            },
        )
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("admission answers at once")
}
