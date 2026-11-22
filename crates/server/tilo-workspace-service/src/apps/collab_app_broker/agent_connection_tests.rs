//! An agent session's connection ([`super::AgentConnection`]): it takes an
//! application's action, a host step and a session step on the workspace's
//! grants, asks its person on the host's sheet where they ask first, and loses
//! all of it once its session ends.
#![allow(
    clippy::significant_drop_tightening,
    reason = "each scenario holds its connection to the end, as the MCP server does"
)]

use super::*;
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    ActionProvider as _, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_peer_state::join::agent_cert::AgentCert;

const HOUR_NS: u64 = 3600 * 1_000_000_000;
/// The agent session the connection is.
const SESSION: &str = "so.tilo.session/agent-session/one";
/// What the person reads the session by.
const NAME: &str = "Claude Code #1";
/// The host steps' provider in this fixture.
const DESKTOP_PROVIDER: &str = "test-desktop";

/// A desktop whose one step lists its windows.
#[derive(Default)]
struct Desktop {
    started: Mutex<Vec<tilo_actions::MemberId>>,
    /// Whether its steps are an agent session's own.
    for_agent_sessions: bool,
    /// Whether its steps are a turn's (the session's own, as a session's shell publishes them).
    for_turns: bool,
}

impl crate::apps::services::HostStepServices for Desktop {
    fn provider(&self) -> &'static str {
        DESKTOP_PROVIDER
    }
    fn app_id(&self) -> &'static str {
        "so.tilo.desktop"
    }
    fn for_agent_sessions(&self) -> bool {
        self.for_agent_sessions
    }
    fn for_turns(&self) -> bool {
        self.for_turns
    }
    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        vec![tilo_actions::ActionDescriptor {
            did: None,
            semantic_identity: None,
            address: ActionAddress::new("desktop", "list-windows"),
            resource_kind: "desktop".into(),
            resource_label: "Desktop".into(),
            action_label: "List windows".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            state: tilo_actions::ActionState::default(),
            locus: tilo_actions::ExecutionLocus::default(),
            kind: tilo_actions::OperationKind::View,
            contract: tilo_actions::ActionContract::view(),
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        }]
    }
    fn start(
        &self,
        request: &crate::apps::services::HostStepRequest<'_>,
    ) -> Result<tilo_actions::remote::Pending<crate::apps::services::HostStepOutcome>, String> {
        self.started
            .lock()
            .unwrap()
            .push(request.actor.member().clone());
        Ok(tilo_actions::remote::Pending::ready(Ok(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

/// An agent session, as the desktop makes it when the person admits a client:
/// an agent member, certified for the session by the person's own (not
/// foreign) device.
pub(super) struct SessionAgent {
    key: SigningKey,
    pub(super) delegation: AgentCert,
    pub(super) device: tilo_user_identity::DeviceCert,
}

fn session_agent(f: &Fixture) -> SessionAgent {
    let now = tilo_clock::try_unix_now_ns().unwrap();
    let key = SigningKey::from_bytes(&[57; 32]);
    let (device_key, device) = f.second_device();
    assert!(!device.capabilities.foreign);
    let delegation = AgentCert::sign(
        &device_key,
        device.user_public_key,
        key.verifying_key().to_bytes(),
        ("claude-code", SESSION),
        workspace_id(),
        now.saturating_sub(1),
        now.saturating_add(HOUR_NS),
    )
    .unwrap();
    SessionAgent {
        key,
        delegation,
        device,
    }
}

impl SessionAgent {
    pub(super) fn presentation(&self) -> Vec<u8> {
        let workspace = workspace_id().to_string();
        runtime_collab::AgentJoinIdentity {
            workspace_id: workspace.clone(),
            device_certificate: serde_json::to_vec(&self.device).unwrap(),
            certificate: self.delegation.encode(),
            proof: self
                .key
                .sign(&tilo_identity_client::collaboration_join_digest(&workspace).unwrap())
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap()
    }
}

/// The person admitted the client: the workspace lists its agent, and its
/// session goes.
pub(super) fn admit(f: &Fixture) -> SessionAgent {
    let agent = session_agent(f);
    f.native
        .listed
        .lock()
        .unwrap()
        .push(crate::apps::services::ListedAgent {
            id: agent.delegation.agent_id.clone(),
            name: "Claude Code".into(),
            acts_for: String::new(),
        });
    f.native.sessions.lock().unwrap().push((
        SESSION.into(),
        crate::apps::services::GoingSession::default(),
    ));
    agent
}

/// The session ends, as the person's Stop or the client leaving ends it.
fn end(f: &Fixture) {
    f.native.sessions.lock().unwrap().clear();
}

/// The device an agent says it came from, in its own request: nothing the
/// host verified.
fn request_naming_a_device(invocation: u128, agent: &SessionAgent) -> InvokeRequest {
    let said = tilo_actions::InvokerDevice::certified([0x66; 32], "Somewhere else");
    InvokeRequest {
        context: Some(
            tilo_actions::ActionContext::default().with_invoker(
                tilo_actions::ActionInvoker::agent(agent.delegation.agent_id.clone())
                    .from_device(Some(said)),
            ),
        ),
        ..append_request(invocation)
    }
}

/// The invoker the host set on the invocation an application received.
fn delivered_invoker(wire: &InvocationEnvelope) -> tilo_actions::ActionInvoker {
    wire.context()
        .and_then(|context| context.invoker.clone())
        .expect("the host names the invoker")
}

fn append_request(invocation: u128) -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(invocation)),
        target: RemoteActionTarget {
            provider: "200".into(),
            address: ActionAddress::new("notes:release", "append"),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        appearance: None,
        wait_ms: None,
        signed: None,
    }
}

fn list_windows_request(invocation: u128) -> InvokeRequest {
    InvokeRequest {
        target: RemoteActionTarget {
            provider: DESKTOP_PROVIDER.into(),
            address: ActionAddress::new("desktop", "list-windows"),
        },
        ..append_request(invocation)
    }
}

/// A caller of the connection, as the MCP server hands it each call.
pub(super) fn connection() -> Caller {
    Caller::local_person("someone").in_new_execution()
}

/// `pending`'s answer, taking the broker's turn each pass, which is where an
/// application's invocation is delivered: `None` when nothing answers it.
pub(super) async fn answered<T>(
    f: &Fixture,
    mut pending: tilo_actions::remote::Pending<T>,
) -> Option<T> {
    tilo_test_env::within("the broker answers", async {
        loop {
            f.state.take_turn(&f.directories);
            match pending.poll_ready() {
                tilo_actions::remote::PendingPoll::Ready(value) => break Some(value),
                tilo_actions::remote::PendingPoll::Abandoned => break None,
                tilo_actions::remote::PendingPoll::Waiting => tokio::task::yield_now().await,
            }
        }
    })
    .await
}

/// The session takes an application's action: attributed to its agent, named
/// by the session's name, in the session's group, and read back.
#[tokio::test(flavor = "current_thread")]
async fn an_agent_session_takes_an_app_action() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = admit(&f);
            let (_target, stream) = notes_target(&f).await;
            let api = f
                .state
                .agent_connection(&agent.presentation(), NAME)
                .unwrap();
            let caller = connection();
            let receipt = answered(&f, api.invoke(&caller, append_request(41)))
                .await
                .expect("answered")
                .unwrap();
            assert!(went_on(&receipt), "{receipt:?}");
            let wire = stream.next().await.unwrap();
            assert_eq!(
                wire.actor,
                tilo_actions::MemberId::agent(agent.delegation.agent_id.clone()).into()
            );
            let context = wire.context().cloned().expect("the session's context");
            let named = context.invoker.and_then(|invoker| invoker.name);
            assert_eq!(named.as_deref(), Some(NAME));
            assert_eq!(context.group.expect("the session's group").id, SESSION);
            let id = tilo_actions::InvocationId(uuid::Uuid::from_u128(41));
            assert!(
                api.observe(&caller, &id).is_some(),
                "the connection reads its own invocation back"
            );
            drop(f);
        })
        .await;
}

/// A session from another of the person's devices: the application its action
/// reaches is told that device, the one the host verified, never one the
/// agent names.
#[tokio::test(flavor = "current_thread")]
async fn an_application_is_told_the_device_an_agent_session_came_from() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = admit(&f);
            let laptop = tilo_actions::InvokerDevice::certified([0x5c; 32], "Studio laptop");
            f.native.sessions.lock().unwrap()[0].1.origin = Some(laptop.clone());
            let (_target, stream) = notes_target(&f).await;
            let api = f
                .state
                .agent_connection(&agent.presentation(), NAME)
                .unwrap();
            answered(
                &f,
                api.invoke(&connection(), request_naming_a_device(43, &agent)),
            )
            .await
            .expect("answered")
            .unwrap();
            let invoker = delivered_invoker(&stream.next().await.unwrap());
            assert_eq!(
                invoker.acting.by,
                tilo_actions::Actor::member(tilo_actions::MemberId::agent(
                    agent.delegation.agent_id.clone()
                ))
            );
            assert_eq!(invoker.device, Some(laptop), "the device the host verified");
            drop(f);
        })
        .await;
}

/// A session of a program on this device names no device, whatever its own
/// request says.
#[tokio::test(flavor = "current_thread")]
async fn an_agent_session_of_this_device_is_on_no_other_device_whatever_it_says() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = admit(&f);
            let (_target, stream) = notes_target(&f).await;
            let api = f
                .state
                .agent_connection(&agent.presentation(), NAME)
                .unwrap();
            answered(
                &f,
                api.invoke(&connection(), request_naming_a_device(44, &agent)),
            )
            .await
            .expect("answered")
            .unwrap();
            let invoker = delivered_invoker(&stream.next().await.unwrap());
            assert_eq!(invoker.device, None, "no host verified a device for it");
            drop(f);
        })
        .await;
}

/// The session ends: the connection lists nothing and takes nothing more, and
/// no new connection joins for it.
#[tokio::test(flavor = "current_thread")]
async fn ending_the_session_cuts_its_connection() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = admit(&f);
            let (_target, _stream) = notes_target(&f).await;
            let api = f
                .state
                .agent_connection(&agent.presentation(), NAME)
                .unwrap();
            let caller = connection();
            end(&f);
            assert!(
                answered(&f, api.invoke(&caller, append_request(42)))
                    .await
                    .expect("answered")
                    .is_err()
            );
            assert!(
                answered(
                    &f,
                    api.query(&caller, tilo_actions::remote::CatalogueQuery::default())
                )
                .await
                .expect("answered")
                .is_err()
            );
            let rejoined = f
                .state
                .agent_connection(&agent.presentation(), NAME)
                .unwrap_err();
            assert!(rejoined.contains("has ended"), "{rejoined}");
            assert_eq!(unread_invocations(200), 0, "nothing reached the app");
            drop(f);
        })
        .await;
}

/// A host step, and a session's own step, are taken as the session's agent;
/// once the session ends, refused.
#[tokio::test(flavor = "current_thread")]
async fn an_agent_session_takes_host_and_session_steps_while_it_goes() {
    for for_turns in [false, true] {
        tokio::task::LocalSet::new()
            .run_until(async {
                let desktop = Arc::new(Desktop {
                    for_turns,
                    ..Desktop::default()
                });
                let f = Fixture::with_services(None, vec![desktop.clone()]);
                let agent = admit(&f);
                let api = f
                    .state
                    .agent_connection(&agent.presentation(), NAME)
                    .unwrap();
                let caller = connection();
                let receipt = answered(&f, api.invoke(&caller, list_windows_request(51)))
                    .await
                    .expect("answered")
                    .unwrap();
                assert!(went_on(&receipt), "{receipt:?}");
                assert_eq!(
                    desktop.started.lock().unwrap().as_slice(),
                    [tilo_actions::MemberId::agent(
                        agent.delegation.agent_id.clone()
                    )]
                );
                end(&f);
                assert!(
                    answered(&f, api.invoke(&caller, list_windows_request(52)))
                        .await
                        .expect("answered")
                        .is_err()
                );
                assert_eq!(desktop.started.lock().unwrap().len(), 1);
                drop(f);
            })
            .await;
    }
}

/// The shell's sheet, as a test answers it: what it was asked, and what it took back.
#[derive(Default)]
pub(super) struct Shell {
    asked: Mutex<Vec<crate::apps::app_actions::AppActionConsentRequest>>,
    answers: Mutex<Vec<tokio::sync::oneshot::Sender<crate::apps::app_actions::AppActionAnswer>>>,
    withdrawn: Mutex<Vec<String>>,
}

impl crate::apps::app_actions::AppActionConsentPrompter for Shell {
    fn ask(
        &self,
        request: crate::apps::app_actions::AppActionConsentRequest,
    ) -> Option<tokio::sync::oneshot::Receiver<crate::apps::app_actions::AppActionAnswer>> {
        let (answer, answered) = tokio::sync::oneshot::channel();
        self.asked.lock().unwrap().push(request);
        self.answers.lock().unwrap().push(answer);
        Some(answered)
    }

    fn withdraw(&self, handle: &str) {
        self.withdrawn.lock().unwrap().push(handle.to_owned());
    }
}

impl Shell {
    /// The question the sheet shows next, once it is up.
    pub(super) async fn shown(
        &self,
        count: usize,
    ) -> crate::apps::app_actions::AppActionConsentRequest {
        for _ in 0..500 {
            if let Some(asked) = self.asked.lock().unwrap().get(count.saturating_sub(1)) {
                return asked.clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the sheet never asked");
    }

    /// The person answers the question up last: allowed for a lifetime, or declined for `None`.
    pub(super) fn answer(&self, lifetime: Option<tilo_action_policy::Lifetime>) {
        let answer = self
            .answers
            .lock()
            .unwrap()
            .pop()
            .expect("a question is up");
        let _ = answer.send(crate::apps::app_actions::AppActionAnswer::unedited(
            lifetime,
        ));
    }
}

/// A session whose person asks first about Notes: the question goes to the shell's sheet.
fn asking_on_the_shell(f: &Fixture, agent: &SessionAgent) -> Arc<Shell> {
    asking_about_notes(f, &agent.delegation.agent_id)
}

/// A workspace whose person asks first about Notes for `agent`: the question goes to the shell's
/// sheet.
pub(super) fn asking_about_notes(f: &Fixture, agent: &str) -> Arc<Shell> {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    let shell = Arc::new(Shell::default());
    f.state.set_app_action_consent(shell.clone());
    put_grant(
        f,
        agent_answers::grant(
            &workspace_id().to_string(),
            agent,
            (
                &AnswerScope::app("so.tilo.notes"),
                tilo_action_policy::ActionKind::Undoable,
            ),
            tilo_action_policy::Answer::Ask,
            tilo_action_policy::Grantee::person(hex::encode(f.native.certificate.user_public_key)),
        ),
    );
    shell
}

fn went_on(receipt: &tilo_actions::remote::InvocationReceipt) -> bool {
    matches!(
        receipt.status,
        tilo_actions::remote::ReceiptStatus::Accepted
            | tilo_actions::remote::ReceiptStatus::Completed { .. }
    )
}

/// A step the person asks first about is put to them on the shell's sheet, by the session's
/// name: the connection is told it needs confirming, asks, waits while the sheet is up, and the
/// person allowing it once lets exactly that step go on.
#[tokio::test(flavor = "current_thread")]
async fn an_agent_sessions_question_is_asked_on_the_shell_and_allowed_once() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = admit(&f);
            let shell = asking_on_the_shell(&f, &agent);
            let (_target, stream) = notes_target(&f).await;
            let api = f
                .state
                .agent_connection(&agent.presentation(), NAME)
                .unwrap();
            let caller = connection();
            let first = answered(&f, api.invoke(&caller, append_request(71)))
                .await
                .expect("answered")
                .unwrap_err();
            assert_eq!(
                first,
                tilo_actions::remote::ActionCallError::confirmation_required()
            );
            let asking = api.confirm(&caller, &append_request(71));
            let shown = shell.shown(1).await;
            assert!(shown.confirming, "{shown:?}");
            assert_eq!(shown.scope.as_deref(), Some(NAME), "{shown:?}");
            shell.answer(Some(tilo_action_policy::Lifetime::Once));
            let confirmed = asking.await.expect("answered").unwrap();
            let mut again = append_request(71);
            confirmed.apply(&mut again);
            let receipt = answered(&f, api.invoke(&caller, again))
                .await
                .expect("answered")
                .unwrap();
            assert!(went_on(&receipt), "{receipt:?}");
            let wire = stream.next().await.unwrap();
            assert_eq!(
                wire.actor,
                tilo_actions::MemberId::agent(agent.delegation.agent_id.clone()).into()
            );
            let next = answered(&f, api.invoke(&caller, append_request(72)))
                .await
                .expect("answered")
                .unwrap_err();
            assert_eq!(
                next,
                tilo_actions::remote::ActionCallError::confirmation_required(),
                "once is once"
            );
            drop(f);
        })
        .await;
}

/// The person denying it refuses the step, in the words a declined step is refused in, and
/// nothing reaches the app.
#[tokio::test(flavor = "current_thread")]
async fn an_agent_sessions_question_the_person_denies_refuses_the_step() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = admit(&f);
            let shell = asking_on_the_shell(&f, &agent);
            let (_target, _stream) = notes_target(&f).await;
            let api = f
                .state
                .agent_connection(&agent.presentation(), NAME)
                .unwrap();
            let asking = api.confirm(&connection(), &append_request(81));
            shell.shown(1).await;
            shell.answer(None);
            let refused = asking.await.expect("answered").unwrap_err();
            assert!(
                matches!(&refused, tilo_actions::remote::ActionCallError::Refused { reason }
                    if reason.contains(tilo_actions::draft::DECLINED)),
                "{refused:?}"
            );
            assert_eq!(unread_invocations(200), 0, "nothing reached the app");
            drop(f);
        })
        .await;
}

/// "Allow for this agent" covers its next such step: it goes on without asking again, for as
/// long as the session goes.
#[tokio::test(flavor = "current_thread")]
async fn allowing_an_agent_session_covers_its_next_step() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = admit(&f);
            let shell = asking_on_the_shell(&f, &agent);
            let (_target, stream) = notes_target(&f).await;
            let api = f
                .state
                .agent_connection(&agent.presentation(), NAME)
                .unwrap();
            let caller = connection();
            let asking = api.confirm(&caller, &append_request(91));
            shell.shown(1).await;
            shell.answer(Some(tilo_action_policy::Lifetime::UntilRevoked));
            let confirmed = asking.await.expect("answered").unwrap();
            let mut again = append_request(91);
            confirmed.apply(&mut again);
            let receipt = answered(&f, api.invoke(&caller, again))
                .await
                .expect("answered")
                .unwrap();
            assert!(went_on(&receipt), "{receipt:?}");
            stream.next().await.unwrap();
            let next = answered(&f, api.invoke(&caller, append_request(92)))
                .await
                .expect("answered")
                .unwrap();
            assert!(went_on(&next), "the next step is not asked about: {next:?}");
            stream.next().await.unwrap();
            assert_eq!(shell.asked.lock().unwrap().len(), 1, "asked once");
            drop(f);
        })
        .await;
}

/// A question nobody answers does not hold the agent forever: the caller leaving takes the
/// sheet down, and so does the session ending, which the agent hears.
#[tokio::test(flavor = "current_thread")]
async fn an_agent_sessions_question_is_withdrawn_when_it_leaves_or_ends() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = admit(&f);
            let shell = asking_on_the_shell(&f, &agent);
            let (_target, _stream) = notes_target(&f).await;
            let api = f
                .state
                .agent_connection(&agent.presentation(), NAME)
                .unwrap();
            let caller = connection();
            let asking = api.confirm(&caller, &append_request(101));
            let shown = shell.shown(1).await;
            drop(asking);
            for _ in 0..500 {
                if !shell.withdrawn.lock().unwrap().is_empty() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert_eq!(*shell.withdrawn.lock().unwrap(), [shown.handle]);

            let asking = api.confirm(&caller, &append_request(102));
            shell.shown(2).await;
            end(&f);
            let refused = asking.await.expect("answered").unwrap_err();
            assert!(
                matches!(&refused, tilo_actions::remote::ActionCallError::Refused { reason }
                    if reason.contains("withdrawn")),
                "{refused:?}"
            );
            assert_eq!(shell.withdrawn.lock().unwrap().len(), 2);
            drop(f);
        })
        .await;
}
