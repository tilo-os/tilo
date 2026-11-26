#![expect(
    clippy::future_not_send,
    reason = "the broker's Rc-backed channel handles stay on the test LocalSet"
)]
use super::*;
use crate::certificate_authority::CertificateAuthority;
use crate::policy_clock::{OwnerPolicyClock, PolicyOrigin};
use crate::services::{WorkspaceAuthority, WorkspaceMemberships};
use ed25519_dalek::{Signer as _, SigningKey};
use tilo_broker_core::workspace_commits::{EffectFuture, WorkspaceCommits};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

#[path = "agent_session_tests.rs"]
mod agent_session;

struct Native {
    owner: tilo_peer_state::owner::DeviceWorkspaces,
    key: SigningKey,
    certificate: tilo_user_identity::DeviceCert,
    origin: PolicyOrigin,
    cached_projection: Mutex<Option<tilo_desktop_model::Layout>>,
    spent_grants: Mutex<Vec<tilo_action_policy::GrantId>>,
    /// The workspace's host records no effect receipt: one it is asked for
    /// is refused, as a host out of reach refuses it.
    receipts_refused: std::sync::atomic::AtomicBool,
    /// The owner is still opening, as a session's is at its start: health
    /// says so until it is open.
    opening: std::sync::atomic::AtomicBool,
    /// The agent sessions going, each with what its person allowed it.
    sessions: Mutex<Vec<(String, crate::services::GoingSession)>>,
    /// The agents the workspace's record of its agents names.
    listed: Mutex<Vec<crate::services::ListedAgent>>,
}

/// The session the fixture's agent works in.
const SESSION: &str = "so.tilo.session/agent-session/release";
/// The grant admitting the fixture's agent to read the workspace.
const AGENT_GRANT: &str = "agent-member:scribe";
fn workspace_id() -> tilo_desktop_model::WorkspaceId {
    tilo_desktop_model::WorkspaceId::parse(tilo_peer_state::owner::testing::WORKSPACE).unwrap()
}
impl crate::services::AgentRoster for Native {
    fn listed(&self, _: &str) -> Vec<crate::services::ListedAgent> {
        self.listed.lock().unwrap().clone()
    }

    fn lists(&self, workspace: &str, agent: &tilo_action_policy::MemberId) -> bool {
        let mut granted = false;
        let read = self.with_tree(&mut |tree| {
            granted = crate::services::workspace_in(tree, workspace).is_some_and(|held| {
                held.policy.grants.iter().any(|grant| {
                    grant.node == held.policy.node()
                        && grant.answer == Answer::Allow
                        && grant.grantee.as_member() == Some(agent)
                })
            });
        });
        read.is_ok()
            && (granted
                || self.listed(workspace).iter().any(|listed| {
                    Some((listed.id.as_str(), listed.acts_for.as_str()))
                        == agent.as_agent().map(|(id, person)| (id, person.as_str()))
                }))
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
    fn policy_time(&self) -> Result<Option<crate::policy_clock::PolicyTime>, String> {
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
}
impl WorkspaceCommits for Native {
    fn projects_workspace(&self, requested: tilo_desktop_model::WorkspaceId) -> bool {
        requested == workspace_id()
    }

    fn health(&self) -> Result<(), String> {
        if self.opening.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("the native record is still opening; try again in a moment".into());
        }
        self.owner.health()
    }

    fn effect(&self, workspace: String, redacted: Vec<u8>) -> EffectFuture {
        let owner = self.owner.clone();
        let certificate = self.certificate.clone();
        let receipts_refused = self
            .receipts_refused
            .load(std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            tokio::task::yield_now().await;
            if receipts_refused {
                return Err("the workspace's record records no receipt".into());
            }
            let workspace = tilo_desktop_model::WorkspaceId::parse(&workspace)
                .ok_or_else(|| "invalid workspace".to_owned())?;
            let receipt: tilo_actions::receipt::WorkspaceEffectReceipt =
                serde_json::from_slice(&redacted).map_err(|error| error.to_string())?;
            owner.record_effect(
                workspace,
                &certificate,
                &receipt,
                std::time::SystemTime::now(),
            )
        })
    }
}

impl crate::services::WorkspaceGrants for Native {
    fn spend(&self, requested: &str, grant: &str) -> bool {
        assert_eq!(requested, workspace_id().to_string());
        // Like the shell's grant service, update the broker's live policy
        // projection synchronously without re-entering the task mutation turn.
        let grant = tilo_action_policy::GrantId::new(grant);
        let mut spent = self.spent_grants.lock().unwrap();
        if spent.contains(&grant) {
            return false;
        }
        spent.push(grant);
        true
    }
}

impl WorkspaceMemberships for Native {
    fn requires_workspace(&self, requested: &str) -> bool {
        requested == workspace_id().to_string()
    }
    fn health(&self) -> Result<(), String> {
        self.owner.health()
    }
    fn execute(
        &self,
        workspace: String,
        caller: tilo_user_identity::DeviceCert,
        command: WorkspaceMembershipCommand,
    ) -> crate::services::WorkspaceMembershipFuture {
        let owner = self.owner.clone();
        let key = self.key.clone();
        Box::pin(async move {
            let workspace = tilo_desktop_model::WorkspaceId::parse(&workspace)
                .ok_or_else(|| "invalid workspace".to_owned())?;
            owner
                .commit_membership(
                    workspace,
                    &caller,
                    &command,
                    &key,
                    std::time::SystemTime::now(),
                )
                .await
                .map(drop)
                .map_err(|error| error.to_string())
        })
    }
}
impl crate::services::AgentSessions for Native {
    fn going(
        &self,
        _workspace: &str,
        _agent: &str,
        session: &str,
    ) -> Option<crate::services::GoingSession> {
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
    home: crate::test_support::ProvisionedHome,
    _fanout: Arc<FanoutState>,
    native: Arc<Native>,
    state: Arc<AppBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
}
impl Fixture {
    fn new() -> Self {
        Self::with_launcher(None)
    }
    fn with_launcher(launcher: Option<Arc<dyn crate::services::LauncherServices>>) -> Self {
        Self::with_services(
            launcher,
            tilo_workspace_service::workspaces::providers::Providers::default(),
        )
    }
    /// The fixture with the providers `providers` registered on this device beside the apps.
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, end to end, reads as one test"
    )]
    fn with_services(
        launcher: Option<Arc<dyn crate::services::LauncherServices>>,
        providers: tilo_workspace_service::workspaces::providers::Providers,
    ) -> Self {
        Self::built(launcher, providers, false)
    }
    /// The fixture, its signed device `foreign` to this host or one of its own.
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, end to end, reads as one test"
    )]
    fn built(
        launcher: Option<Arc<dyn crate::services::LauncherServices>>,
        providers: tilo_workspace_service::workspaces::providers::Providers,
        foreign: bool,
    ) -> Self {
        let home = crate::test_support::ProvisionedHome::new();
        let owner = tilo_peer_state::owner::open_file(&home.root().join("native.json")).unwrap();
        let key = SigningKey::from_bytes(&[92; 32]);
        let mut certificate = tilo_peer_state::owner::testing::certificate(
            &SigningKey::from_bytes(&[91; 32]),
            &key,
            "Host",
        );
        // A device of the person's own runs the agents: a foreign one is held to the
        // workspace's cap, which no agent runs under.
        certificate.capabilities.foreign = foreign;
        tilo_user_identity::sign_device_cert(&mut certificate, &SigningKey::from_bytes(&[91; 32]))
            .unwrap();
        tilo_peer_state::owner::testing::add_interactive_device(&owner, &key, &certificate)
            .unwrap();
        let native = Arc::new(Native {
            owner,
            key,
            certificate,
            origin: OwnerPolicyClock::new().unwrap().new_origin(),
            cached_projection: Mutex::new(None),
            spent_grants: Mutex::new(Vec::new()),
            receipts_refused: std::sync::atomic::AtomicBool::new(false),
            opening: std::sync::atomic::AtomicBool::new(false),
            sessions: Mutex::new(Vec::new()),
            listed: Mutex::new(Vec::new()),
        });
        let command = WorkspaceMembershipCommand::Grant {
            grant: tilo_action_policy::Grant::new(
                AGENT_GRANT,
                tilo_action_policy::Grantee::agent(
                    "scribe",
                    hex::encode(native.certificate.user_public_key),
                ),
                tilo_action_policy::TreePath::workspace(workspace_id().to_string()),
                tilo_action_policy::KindSet::READ,
                tilo_action_policy::Answer::Allow,
                tilo_action_policy::Grantee::person(hex::encode(
                    native.certificate.user_public_key,
                )),
            ),
        };
        futures::executor::block_on(native.owner.commit_membership(
            workspace_id(),
            &native.certificate,
            &command,
            &native.key,
            std::time::SystemTime::now(),
        ))
        .unwrap();
        let fanout = Arc::new(FanoutState::new());
        let config = crate::NativePeerBrokerConfig {
            journal_root: home.root().join("journal"),
            certificate_authority: native.clone(),
        };
        let (state, rx) = AppBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::with_commits(native.clone()),
            Some(native.clone()),
            None,
            Some(&config),
            ProcessServices {
                launcher,
                workspace_grants: Some(native.clone()),
                agents: Some(native.clone()),
                agent_sessions: Some(native.clone()),
                workspace_memberships: Some(native.clone()),
                providers: Some(providers),
                ..Default::default()
            },
        );
        Self {
            home,
            _fanout: fanout,
            native,
            state,
            directories: Rc::new(LocalActionDirectoryRegistry::new(rx)),
        }
    }
    /// The workspace's record of its agents names the fixture's agent, kept by its person.
    fn list_scribe(&self) {
        let mut listed = self.native.listed.lock().unwrap();
        if !listed.iter().any(|agent| agent.id == "scribe") {
            listed.push(crate::services::ListedAgent {
                avatar: None,
                id: "scribe".into(),
                name: "Scribe".into(),
                acts_for: String::new(),
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
            let kept = crate::test_support::KeptAgents::default()
                .listing(&route.cluster_id, ("scribe", &admitted.actor_key));
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
            committed_read: Rc::new(std::cell::RefCell::new(committed_document::Slot::default())),
            state: self.state.clone(),
            directories: self.directories.clone(),
            channel_id: route.channel_key(),
            peer_id: peer,
            connection_id: peer,
            verified_identity: admitted,
            resource_type: None,
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
async fn notes_target(f: &Fixture) -> (runtime_collab::RecordHandle, InvocationStreamServer) {
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
            let mut next = view.peer().clone();
            next.governance_mut()
                .put_member_grants(&[grant], &f.native.key, now)?;
            Ok(vec![next.workspace_message()])
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

use tilo_actions::remote::{ActionCallError, Caller, InvocationReceipt, InvokeRequest, Pending};

/// A step that is a turn's (registered for turns): the session's layout, out here.
#[derive(Default)]
struct TurnSteps {
    taken: Mutex<Vec<tilo_actions::Actor>>,
    /// The turn each step was taken on, as the session reads it to stand what the step shows
    /// beside the turn that asked.
    on_turns: Mutex<Vec<Option<tilo_action_policy::turn::Turn>>>,
}

impl tilo_actions::remote::Admits for TurnSteps {
    const PROVIDER: &'static str = tilo_actions::remote::session_actions::PROVIDER;
    const APP: &'static str = tilo_actions::remote::session_actions::PROVIDER;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        tilo_actions::remote::admitted::top_level(TurnSteps::descriptors())
    }

    fn start(
        &self,
        caller: &Caller,
        _request: &InvokeRequest,
        _admitted: &tilo_actions::remote::Admitted,
    ) -> Result<tilo_actions::remote::Pending<tilo_actions::remote::Outcome>, String> {
        let actor = tilo_broker_core::BrokerActor::for_caller(caller);
        self.taken.lock().unwrap().push(actor.actor().clone());
        self.on_turns.lock().unwrap().push(actor.turn().cloned());
        Ok(Pending::ready(Ok(tilo_actions::ActionOutput::default())))
    }
}

impl TurnSteps {
    fn descriptors() -> Vec<tilo_actions::ActionDescriptor> {
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

    /// `steps`, this device's turn steps provider.
    fn providers(steps: Arc<Self>) -> tilo_workspace_service::workspaces::providers::Providers {
        let providers = tilo_workspace_service::workspaces::providers::Providers::default();
        providers.provide(
            tilo_actions::remote::session_actions::PROVIDER,
            tilo_actions::remote::session_actions::PROVIDER,
            tilo_workspace_service::workspaces::providers::Reach::Turns,
            steps as Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
        );
        providers
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
            let f = Fixture::with_services(None, TurnSteps::providers(Arc::clone(&steps)));
            f.list_scribe();
            f.native
                .sessions
                .lock()
                .unwrap()
                .push((SESSION.into(), crate::services::GoingSession::default()));
            grant_the_layout(&f, true);
            let person = hex::encode(f.native.certificate.user_public_key);

            // The agent working in its session, answering for the person.
            let agent = Caller::local_person(person.clone())
                .delegated_to("scribe")
                .in_workspace(workspace_id().to_string())
                .in_agent_session(SESSION)
                .in_new_execution();
            stray_invoke(&f.state, &agent, 92).expect("the session takes the session's step");
            assert_eq!(
                steps.taken.lock().unwrap().as_slice(),
                [tilo_actions::Actor::from(tilo_actions::MemberId::agent(
                    "scribe",
                    person.clone()
                ))],
                "the step ran once, as the session's agent"
            );

            // A turn's step is listed to the agent working in its session, and to the person at
            // this device as their other steps are: the REPL types what the agent would.
            let listed = |caller: &Caller| {
                f.state
                    .eager_catalogue(caller, action_provider::RowScope::All)
                    .0
                    .iter()
                    .any(|row| row.provider == tilo_actions::remote::session_actions::PROVIDER)
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
            let ended = Caller::local_person(person)
                .delegated_to("scribe")
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
            Grantee::agent("scribe", hex::encode(f.native.certificate.user_public_key)),
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

/// The workspace's contact directory as the provider applying its typed actions answers it: one
/// action, and who reached it.
#[derive(Default)]
struct ContactDirectory {
    reached: Mutex<Vec<tilo_actions::Actor>>,
}

/// The contact directory's own type, as every caller names its record.
const CONTACT_KIND: &str = "tilo.contacts.identities";

impl ContactDirectory {
    fn provider() -> String {
        format!(
            "{}:{}:{}",
            tilo_actions::remote::RESOURCE_PROVIDER,
            tilo_app_id::apps::CONTACTS,
            collab_types::workspace_document_id(CONTACT_KIND)
        )
    }

    fn address() -> tilo_actions::ActionAddress {
        tilo_actions::ActionAddress::new(CONTACT_KIND, "project-contact")
    }

    /// `directory`, registered as a device registers the provider of every built-in record.
    fn providers(directory: Arc<Self>) -> tilo_workspace_service::workspaces::providers::Providers {
        let providers = tilo_workspace_service::workspaces::providers::Providers::default();
        providers.provide(
            tilo_actions::remote::RESOURCE_PROVIDER,
            tilo_actions::remote::RESOURCE_PROVIDER,
            tilo_workspace_service::workspaces::providers::Reach::Devices,
            directory as Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
        );
        providers
    }
}

impl tilo_actions::remote::ActionProvider for ContactDirectory {
    fn serves(&self, provider: &str) -> bool {
        provider == Self::provider()
    }

    fn query(
        &self,
        _caller: &Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> Pending<Result<tilo_actions::remote::CataloguePage, ActionCallError>> {
        Pending::ready(Ok(tilo_actions::remote::CataloguePage::from_rows(
            "0".to_owned(),
            Vec::new(),
            &query,
        )))
    }

    fn invocation(
        &self,
        address: &tilo_actions::ActionAddress,
        _argument: Option<&str>,
    ) -> Option<tilo_actions::ActionDescriptor> {
        (address == &Self::address()).then(|| tilo_actions::ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: Self::address(),
            resource_kind: CONTACT_KIND.into(),
            resource_label: "Contacts".into(),
            action_label: "Add a contact".into(),
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
        })
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let actor = tilo_broker_core::BrokerActor::for_caller(caller);
        self.reached.lock().unwrap().push(actor.actor().clone());
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status: tilo_actions::remote::ReceiptStatus::Completed {
                output: tilo_actions::ActionOutput::default(),
            },
        }))
    }

    fn observe(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

/// A write to the contact directory record, made by `caller` through the broker as `invocation`.
fn write_contacts(
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
                    provider: ContactDirectory::provider(),
                    address: ContactDirectory::address(),
                },
                argument: tilo_actions::remote::InvokeArgument::Json(serde_json::Map::new()),
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

/// Whether the broker took the write and handed it to the directory: it answers it accepted,
/// and follows it there.
fn taken(wrote: &Result<InvocationReceipt, ActionCallError>) -> bool {
    matches!(
        wrote,
        Ok(InvocationReceipt {
            status: tilo_actions::remote::ReceiptStatus::Accepted
                | tilo_actions::remote::ReceiptStatus::Completed { .. },
            ..
        })
    )
}

/// A record's typed actions are decided under the record's own app, whichever provider applies
/// them: a rule on Contacts' node holds for a write to the contact directory record, for an
/// agent and for a key the workspace otherwise lets do anything, and the person's own write
/// still goes.
#[tokio::test(flavor = "current_thread")]
async fn a_rule_on_an_apps_node_holds_for_its_record_whoever_applies_it() {
    use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
    tokio::task::LocalSet::new()
        .run_until(async {
            let directory = Arc::new(ContactDirectory::default());
            let f =
                Fixture::with_services(None, ContactDirectory::providers(Arc::clone(&directory)));
            f.list_scribe();
            f.native
                .sessions
                .lock()
                .unwrap()
                .push((SESSION.into(), crate::services::GoingSession::default()));
            let person = hex::encode(f.native.certificate.user_public_key);
            let workspace = workspace_id().to_string();
            let agent = Caller::local_person(person.clone())
                .delegated_to("scribe")
                .in_workspace(workspace.clone())
                .in_agent_session(SESSION)
                .in_new_execution();
            // A key is a grant like any agent's, with what its person gave it.
            let key = Caller::local_agent("deploy-key", person.clone())
                .in_workspace(workspace.clone())
                .in_new_execution();
            let own = Caller::local_person(person.clone())
                .in_workspace(workspace.clone())
                .in_new_execution();
            let contacts = TreePath::workspace(workspace.clone()).app(tilo_app_id::apps::CONTACTS);
            let rule = |id: &str, member: &str, node: TreePath, answer: Answer| {
                Grant::new(
                    id,
                    Grantee::agent(member, person.clone()),
                    node,
                    KindSet::ALL,
                    answer,
                    Grantee::person(person.clone()),
                )
            };
            for (id, member) in [("scribe-all", "scribe"), ("key-all", "deploy-key")] {
                put_grant(
                    &f,
                    rule(
                        id,
                        member,
                        TreePath::workspace(workspace.clone()),
                        Answer::Allow,
                    ),
                );
            }
            for (invocation, caller) in [(1, &agent), (2, &key)] {
                let wrote = write_contacts(&f.state, caller, invocation);
                assert!(
                    taken(&wrote),
                    "the workspace's grants let it write the directory: {wrote:?}"
                );
            }
            assert_eq!(directory.reached.lock().unwrap().len(), 2);

            // Contacts' node denies them: the directory record is Contacts', refused outright.
            for (id, member) in [
                ("scribe-contacts", "scribe"),
                ("key-contacts", "deploy-key"),
            ] {
                put_grant(&f, rule(id, member, contacts.clone(), Answer::Deny));
            }
            for (invocation, caller) in [(3, &agent), (4, &key)] {
                let refused = write_contacts(&f.state, caller, invocation);
                assert!(
                    matches!(
                        refused,
                        Err(ActionCallError::NotPermitted | ActionCallError::Refused { .. })
                    ),
                    "a rule on Contacts' node refuses the write to its record: {refused:?}"
                );
            }
            assert_eq!(directory.reached.lock().unwrap().len(), 2, "nothing ran");

            // Contacts' node asks first: the write waits for the person's answer.
            for (id, member) in [
                ("scribe-contacts", "scribe"),
                ("key-contacts", "deploy-key"),
            ] {
                put_grant(&f, rule(id, member, contacts.clone(), Answer::Ask));
            }
            for (invocation, caller) in [(5, &agent), (6, &key)] {
                let asked = write_contacts(&f.state, caller, invocation);
                assert!(
                    matches!(asked, Err(ActionCallError::ConfirmationRequired(_))),
                    "a rule on Contacts' node that asks first asks: {asked:?}"
                );
            }
            assert_eq!(directory.reached.lock().unwrap().len(), 2, "nothing ran");

            // The person's own write is theirs, as before.
            let wrote = write_contacts(&f.state, &own, 7);
            assert!(
                taken(&wrote),
                "the person writes their workspace's directory: {wrote:?}"
            );
            assert_eq!(
                directory.reached.lock().unwrap().last(),
                Some(&tilo_actions::Actor::from(tilo_actions::MemberId::person(
                    person
                ))),
            );
            drop(f);
        })
        .await;
}
