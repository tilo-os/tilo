//! The desktop's agent door, served as a task agent in its run
//! ([`super::TaskRunHost`]): an admitted connection takes an application's
//! action and a host step only as the run admits them, and loses both once
//! its person stops the run.
#![allow(
    clippy::significant_drop_tightening,
    reason = "each scenario holds its connection to the end, as the door does"
)]

use super::*;
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    ActionHost as _, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_peer_state::join::task_delegation::TaskDelegation;

const HOUR_NS: u64 = 3600 * 1_000_000_000;
/// The issue the door's connection works in.
const DOOR_ISSUE: &str = "so.tilo.session/agent-door/door-session";
/// The host steps' provider in this fixture.
const DESKTOP_PROVIDER: &str = "test-desktop";

/// A desktop whose one step lists its windows.
#[derive(Default)]
struct Desktop {
    started: Mutex<Vec<String>>,
    /// Whether its steps are a task agent's steps of its run.
    for_tasks: bool,
    /// Whether its steps are a turn's (the session's own, as a session's desk publishes them).
    for_turns: bool,
}

impl crate::services::HostStepServices for Desktop {
    fn provider(&self) -> &'static str {
        DESKTOP_PROVIDER
    }
    fn app_id(&self) -> &'static str {
        "so.tilo.desktop"
    }
    fn for_tasks(&self) -> bool {
        self.for_tasks
    }
    fn for_turns(&self) -> bool {
        self.for_turns
    }
    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        vec![tilo_actions::ActionDescriptor {
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
        request: &crate::services::HostStepRequest<'_>,
    ) -> Result<tilo_actions::remote::Pending<crate::services::HostStepOutcome>, String> {
        self.started
            .lock()
            .unwrap()
            .push(request.actor.id().to_owned());
        Ok(tilo_actions::remote::Pending::ready(Ok(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

/// The door's connection, as the desktop makes it when the person admits a
/// client: a task agent minted for the issue, delegated by the person's
/// own (not foreign) device, the one at the desktop.
pub(super) struct DoorAgent {
    key: SigningKey,
    delegation: TaskDelegation,
    device: tilo_user_identity::DeviceCert,
}

fn door_agent(f: &Fixture) -> DoorAgent {
    let now = tilo_clock::try_unix_now_ns().unwrap();
    let key = SigningKey::from_bytes(&[57; 32]);
    let (device_key, device) = f.second_device();
    assert!(!device.capabilities.foreign);
    let delegation = TaskDelegation::sign(
        &device_key,
        device.user_public_key,
        key.verifying_key().to_bytes(),
        DOOR_ISSUE,
        workspace_id().to_string(),
        now.saturating_sub(1),
        now.saturating_add(HOUR_NS),
    )
    .unwrap();
    DoorAgent {
        key,
        delegation,
        device,
    }
}

impl DoorAgent {
    pub(super) fn presentation(&self) -> Vec<u8> {
        let workspace = workspace_id().to_string();
        runtime_collab::TaskAgentJoinIdentity {
            workspace_id: workspace.clone(),
            device_certificate: serde_json::to_vec(&self.device).unwrap(),
            delegation: self.delegation.encode(),
            proof: self
                .key
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    &workspace,
                ))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap()
    }
}

/// The person admitted the client: Tasks holds the issue, assigned to the
/// door's task agent, with its run going and carrying its delegation.
pub(super) fn admit(f: &Fixture) -> DoorAgent {
    let agent = door_agent(f);
    f.native.runs.lock().unwrap().push(TrackerRun {
        issue: DOOR_ISSUE.into(),
        title: "claude-code in this session".into(),
        instruction: "Work for the person through the agent door.".into(),
        agent: agent.delegation.agent_id.clone(),
        epoch: 1,
        starter: hex::encode(agent.device.user_public_key),
        active: true,
        finished_by_agent: false,
        paused_through: None,
        budget: None,
        subject: None,
        model: None,
        directions: Vec::new(),
        turn: None,
        question: None,
        delegation: Some(Ok(agent.delegation.encode())),
        origin: None,
    });
    agent
}

/// The device a door's connection says it came from, in its own request:
/// nothing the door verified.
fn request_naming_a_device(invocation: u128, agent: &DoorAgent) -> InvokeRequest {
    let said = tilo_actions::InvokerDevice::certified([0x66; 32], "Somewhere else");
    InvokeRequest {
        context: Some(
            tilo_actions::ActionContext::default().with_invoker(
                tilo_actions::ActionInvoker::actor(format!("agent:{}", agent.delegation.agent_id))
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

/// The connection's own caller, as the door's MCP server makes it.
pub(super) fn connection(host: &TaskRunHost) -> Caller {
    Caller::local_agent(host.agent())
        .in_workspace(host.workspace())
        .acting_for(host.person())
        .in_new_execution()
}

/// Admitted at the door, the connection takes an application's action as a
/// child of its run: attributed to its task agent, in the run's group, owned
/// by the run, named by the name its agent is seated as, and observed back by the connection.
#[tokio::test(flavor = "current_thread")]
async fn an_admitted_door_connection_takes_an_app_action_in_its_run() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = admit(&f);
            let (_target, stream) = notes_target(&f).await;
            let host = f.state.task_run_host(&agent.presentation()).unwrap();
            assert_eq!(host.issue(), DOOR_ISSUE);
            let caller = connection(&host);
            let receipt = host
                .invoke(&caller, append_request(41))
                .await
                .expect("answered")
                .unwrap();
            assert!(
                matches!(
                    receipt.status,
                    tilo_actions::remote::ReceiptStatus::Accepted
                        | tilo_actions::remote::ReceiptStatus::Completed { .. }
                ),
                "{receipt:?}"
            );
            let wire = stream.next().await.unwrap();
            assert_eq!(wire.actor, format!("agent:{}", agent.delegation.agent_id));
            let context = wire.context().cloned().expect("the run's context");
            let named = context.invoker.and_then(|invoker| invoker.name);
            assert_eq!(named.as_deref(), Some("claude-code in this session"));
            let group = context.group.expect("the run's group");
            assert!(group.id.contains("door-session"), "{group:?}");
            let id = tilo_actions::InvocationId(uuid::Uuid::from_u128(41));
            assert!(
                host.observe(&caller, &id).is_some(),
                "the connection reads its own invocation back"
            );
            drop(f);
        })
        .await;
}

/// A connection the door admitted from another of the person's devices
/// works in a run that names that device, and the application its action
/// reaches is told so by the host, on the invoker beside the task agent it
/// acts as: the device the run keeps, never one the connection names.
#[tokio::test(flavor = "current_thread")]
async fn an_application_is_told_the_device_a_door_connection_came_from() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = admit(&f);
            let laptop = tilo_actions::InvokerDevice::certified([0x5c; 32], "Studio laptop");
            f.native
                .runs
                .lock()
                .unwrap()
                .last_mut()
                .expect("the door's run")
                .origin = Some(laptop.clone());
            let (_target, stream) = notes_target(&f).await;
            let host = f.state.task_run_host(&agent.presentation()).unwrap();
            let caller = connection(&host);
            host.invoke(&caller, request_naming_a_device(43, &agent))
                .await
                .expect("answered")
                .unwrap();
            let invoker = delivered_invoker(&stream.next().await.unwrap());
            assert_eq!(
                invoker.actor,
                format!("agent:{}", agent.delegation.agent_id)
            );
            assert_eq!(invoker.device, Some(laptop), "the device the door verified");
            drop(f);
        })
        .await;
}

/// A connection of a program on this device works in a run that names no
/// device, and the application is told of none, whatever device the
/// connection's own request names.
#[tokio::test(flavor = "current_thread")]
async fn a_door_connection_of_this_device_is_on_no_other_device_whatever_it_says() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = admit(&f);
            let (_target, stream) = notes_target(&f).await;
            let host = f.state.task_run_host(&agent.presentation()).unwrap();
            let caller = connection(&host);
            host.invoke(&caller, request_naming_a_device(44, &agent))
                .await
                .expect("answered")
                .unwrap();
            let invoker = delivered_invoker(&stream.next().await.unwrap());
            assert_eq!(
                invoker.actor,
                format!("agent:{}", agent.delegation.agent_id)
            );
            assert_eq!(invoker.device, None, "no door verified a device for it");
            drop(f);
        })
        .await;
}

/// Its person stops the run on the issue: the connection lists nothing and
/// takes nothing more, and a new connection cannot join for it.
#[tokio::test(flavor = "current_thread")]
async fn stopping_the_run_cuts_the_door_connection() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = admit(&f);
            let (_target, _stream) = notes_target(&f).await;
            let host = f.state.task_run_host(&agent.presentation()).unwrap();
            let caller = connection(&host);
            for run in f.native.runs.lock().unwrap().iter_mut() {
                run.active = false;
            }
            f.state.refresh_native_task_delivery();
            let refused = host
                .invoke(&caller, append_request(42))
                .await
                .expect("answered")
                .unwrap_err();
            assert!(
                matches!(
                    refused,
                    tilo_actions::remote::ActionHostError::Refused { .. }
                ),
                "{refused:?}"
            );
            assert!(
                host.query(&caller, tilo_actions::remote::CatalogueQuery::default())
                    .await
                    .expect("answered")
                    .is_err()
            );
            let rejoined = f.state.task_run_host(&agent.presentation()).unwrap_err();
            assert!(rejoined.contains("has ended"), "{rejoined}");
            assert_eq!(unread_invocations(200), 0, "nothing reached the app");
            drop(f);
        })
        .await;
}

/// A host step (the desktop's) goes through the same run: admitted by the
/// run, taken by the task agent; once the run stops, refused.
#[tokio::test(flavor = "current_thread")]
async fn a_door_connection_takes_a_host_step_through_its_run() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let desktop = Arc::new(Desktop::default());
            let f = Fixture::with_services(None, vec![desktop.clone()]);
            let agent = admit(&f);
            let host = f.state.task_run_host(&agent.presentation()).unwrap();
            let caller = connection(&host);
            let receipt = host
                .invoke(&caller, list_windows_request(51))
                .await
                .expect("answered")
                .unwrap();
            assert!(
                matches!(
                    receipt.status,
                    tilo_actions::remote::ReceiptStatus::Accepted
                        | tilo_actions::remote::ReceiptStatus::Completed { .. }
                ),
                "{receipt:?}"
            );
            assert_eq!(
                desktop.started.lock().unwrap().as_slice(),
                [format!("agent:{}", agent.delegation.agent_id)]
            );
            for run in f.native.runs.lock().unwrap().iter_mut() {
                run.active = false;
            }
            f.state.refresh_native_task_delivery();
            assert!(
                host.invoke(&caller, list_windows_request(52))
                    .await
                    .expect("answered")
                    .is_err()
            );
            assert_eq!(desktop.started.lock().unwrap().len(), 1);
            drop(f);
        })
        .await;
}

/// A session's own step is a turn's, and the door's connection works no turn: its run, started
/// by the person's admission, is what admits it, as it admits the connection's app actions. Taken
/// as the task agent; refused once the run stops.
#[tokio::test(flavor = "current_thread")]
async fn a_door_connection_takes_a_session_step_in_its_run_though_it_works_no_turn() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let desk = Arc::new(Desktop {
                for_turns: true,
                ..Desktop::default()
            });
            let f = Fixture::with_services(None, vec![desk.clone()]);
            let agent = admit(&f);
            let host = f.state.task_run_host(&agent.presentation()).unwrap();
            let caller = connection(&host);
            let receipt = host
                .invoke(&caller, list_windows_request(61))
                .await
                .expect("answered")
                .unwrap();
            assert!(
                matches!(
                    receipt.status,
                    tilo_actions::remote::ReceiptStatus::Accepted
                        | tilo_actions::remote::ReceiptStatus::Completed { .. }
                ),
                "{receipt:?}"
            );
            assert_eq!(
                desk.started.lock().unwrap().as_slice(),
                [format!("agent:{}", agent.delegation.agent_id)]
            );
            for run in f.native.runs.lock().unwrap().iter_mut() {
                run.active = false;
            }
            f.state.refresh_native_task_delivery();
            assert!(
                host.invoke(&caller, list_windows_request(62))
                    .await
                    .expect("answered")
                    .is_err()
            );
            assert_eq!(desk.started.lock().unwrap().len(), 1);
            drop(f);
        })
        .await;
}

/// A task agent launched for its task (not the door's connection) joins
/// over its own channel. A host step marked for tasks (a task's programs) is
/// in its catalogue, and it takes the step as a child of its run through the
/// run-admitted host step path the door's connection takes: taken once, read
/// back by the run, refused once the run stops.
#[tokio::test(flavor = "current_thread")]
async fn a_launched_task_agent_takes_its_task_steps_in_its_run() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let steps = Arc::new(Desktop {
                for_tasks: true,
                ..Desktop::default()
            });
            let f = Fixture::with_services(None, vec![steps.clone()]);
            let agent = admit(&f);
            let route = CrdtRoute::record(TASK_AGENT_APP, workspace_id().to_string(), "");
            let admitted = verify_join_identity(
                &route,
                &agent.presentation(),
                f.state.membership.as_ref(),
                f.state.trusted_devices.as_deref().map(Vec::as_slice),
                f.state.certificate_authority.get().map(Arc::as_ref),
            )
            .unwrap();
            let actor = admitted.broker_actor();
            let catalogue = f
                .state
                .action_catalogue_for(&workspace_id().to_string(), &actor)
                .unwrap();
            assert!(
                catalogue.iter().any(|surface| surface.provider.as_deref()
                    == Some(DESKTOP_PROVIDER)
                    && surface.actions[0].action == "list-windows"),
                "the task step is in the task agent's catalogue: {catalogue:?}"
            );
            let channel = f.channel_admitted(300, &route, admitted);
            let run = delivered(&channel).await;
            // The step the run takes: its own task action, under the run's lease.
            let task_action = runtime_collab::TaskAction {
                peer_id: 0,
                provider: DESKTOP_PROVIDER,
                resource: "desktop",
                action: "list-windows",
                argument: None,
            };
            let take = |invocation: u128| {
                let channel = channel.clone();
                let (run_id, lease) = (run.run_id, run.lease.clone());
                async move {
                    channel
                        .invoke_task_action(
                            run_id,
                            &lease,
                            task_action,
                            &uuid::Uuid::from_u128(invocation).to_string(),
                        )
                        .await
                }
            };
            let (_id, update) = take(61).await.expect("the run takes its task step");
            let update: tilo_actions::InvocationUpdate =
                tilo_actions::decode_json(update.as_bytes()).unwrap();
            assert!(
                matches!(
                    update,
                    tilo_actions::InvocationUpdate::Accepted { .. }
                        | tilo_actions::InvocationUpdate::Succeeded { .. }
                ),
                "{update:?}"
            );
            assert_eq!(
                steps.started.lock().unwrap().as_slice(),
                [format!("agent:{}", agent.delegation.agent_id)],
                "taken once, by the task agent"
            );
            // Asked again, the same step is read back, not taken again.
            take(61).await.expect("read back");
            assert_eq!(steps.started.lock().unwrap().len(), 1);
            for run in f.native.runs.lock().unwrap().iter_mut() {
                run.active = false;
            }
            f.state.refresh_native_task_delivery();
            assert!(take(62).await.is_err(), "a stopped run takes no step");
            assert_eq!(steps.started.lock().unwrap().len(), 1);
            drop(f);
        })
        .await;
}

/// The origin of the workspace's records, as a test sees what reaches it.
#[derive(Default)]
struct Origin {
    applied: Mutex<Vec<(String, String)>>,
}

impl crate::services::RecordChangeServices for Origin {
    fn apply(
        &self,
        _workspace: &str,
        change: &tilo_actions::remote::RecordChange,
        invoker: &tilo_actions::ActionInvoker,
    ) -> Result<serde_json::Value, String> {
        self.applied
            .lock()
            .unwrap()
            .push((invoker.actor.clone(), change.action.clone()));
        Ok(serde_json::json!({"ok": {"value": null}, "differs": false}))
    }
}

/// A run changes a record outside its app (a program's `tasks.new_task`, from
/// the door's REPL or a launched task agent): the run admits it as it admits
/// its other steps, the change goes to the record's origin as the run's task
/// agent, and once the run stops it is refused.
#[tokio::test(flavor = "current_thread")]
async fn a_run_changes_a_record_as_its_task_agent_while_it_goes() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let origin = Arc::new(Origin::default());
            let f = Fixture::with_origin(None, Vec::new(), Some(origin.clone()));
            let agent = admit(&f);
            let host = f.state.task_run_host(&agent.presentation()).unwrap();
            let caller = connection(&host);
            let changes = host.record_changes().expect("a run changes records");
            let change = tilo_actions::remote::RecordChange {
                app: "so.tilo.notes".into(),
                record: "main".into(),
                resource: "notes:release".into(),
                action: "append".into(),
                arguments: serde_json::Map::new(),
                preview: None,
            };
            changes
                .apply(&caller, &change)
                .expect("the run admits its change");
            assert_eq!(
                origin.applied.lock().unwrap().as_slice(),
                [(
                    format!("agent:{}", agent.delegation.agent_id),
                    "append".to_owned()
                )],
                "applied as the run's task agent"
            );
            for run in f.native.runs.lock().unwrap().iter_mut() {
                run.active = false;
            }
            f.state.refresh_native_task_delivery();
            assert!(
                changes.apply(&caller, &change).is_err(),
                "a stopped run changes nothing"
            );
            assert_eq!(origin.applied.lock().unwrap().len(), 1);
            drop(f);
        })
        .await;
}

/// The desk's sheet, as a test answers it: what it was asked, and what it took back.
#[derive(Default)]
struct Desk {
    asked: Mutex<Vec<crate::app_actions::AppActionConsentRequest>>,
    answers: Mutex<Vec<tokio::sync::oneshot::Sender<crate::app_actions::AppActionAnswer>>>,
    withdrawn: Mutex<Vec<String>>,
}

impl crate::app_actions::AppActionConsentPrompter for Desk {
    fn ask(
        &self,
        request: crate::app_actions::AppActionConsentRequest,
    ) -> Option<tokio::sync::oneshot::Receiver<crate::app_actions::AppActionAnswer>> {
        let (answer, answered) = tokio::sync::oneshot::channel();
        self.asked.lock().unwrap().push(request);
        self.answers.lock().unwrap().push(answer);
        Some(answered)
    }

    fn withdraw(&self, handle: &str) {
        self.withdrawn.lock().unwrap().push(handle.to_owned());
    }
}

impl Desk {
    /// The question the sheet shows next, once it is up.
    async fn shown(&self, count: usize) -> crate::app_actions::AppActionConsentRequest {
        for _ in 0..500 {
            if let Some(asked) = self.asked.lock().unwrap().get(count.saturating_sub(1)) {
                return asked.clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the sheet never asked");
    }

    /// The person answers the question up last with `consent`.
    fn answer(&self, consent: crate::app_actions::AppActionConsent) {
        let answer = self
            .answers
            .lock()
            .unwrap()
            .pop()
            .expect("a question is up");
        let _ = answer.send(crate::app_actions::AppActionAnswer::unedited(consent));
    }
}

/// A door's run whose person asks first about Notes: the question goes to the desk's sheet.
fn asking_on_the_desk(f: &Fixture, agent: &DoorAgent) -> Arc<Desk> {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    f.native
        .asks_on_host
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let desk = Arc::new(Desk::default());
    f.state.set_app_action_consent(desk.clone());
    put_grant(
        f,
        agent_answers::grant(
            &workspace_id().to_string(),
            &agent.delegation.agent_id,
            (
                &AnswerScope::app("so.tilo.notes"),
                tilo_action_policy::ActionKind::Undoable,
            ),
            tilo_action_policy::Answer::Ask,
            tilo_action_policy::Grantee::person(hex::encode(f.native.certificate.user_public_key)),
        ),
    );
    desk
}

/// The question the run holds now.
fn question_on_run(f: &Fixture) -> Option<crate::services::TrackerQuestion> {
    f.native
        .runs
        .lock()
        .unwrap()
        .iter()
        .find(|run| run.issue == DOOR_ISSUE)
        .and_then(|run| run.question.clone())
}

fn went_on(receipt: &tilo_actions::remote::InvocationReceipt) -> bool {
    matches!(
        receipt.status,
        tilo_actions::remote::ReceiptStatus::Accepted
            | tilo_actions::remote::ReceiptStatus::Completed { .. }
    )
}

/// A step the door's run asks first about is put to its person on the desk's sheet, not on a
/// Tasks issue: the connection is told it needs confirming, asks, waits while the sheet is up,
/// and the person allowing it once lets exactly that step go on, as the run's.
#[tokio::test(flavor = "current_thread")]
async fn a_door_runs_question_is_asked_on_the_desk_and_allowed_once() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = admit(&f);
            let desk = asking_on_the_desk(&f, &agent);
            let (_target, stream) = notes_target(&f).await;
            let host = f.state.task_run_host(&agent.presentation()).unwrap();
            let caller = connection(&host);
            let first = host
                .invoke(&caller, append_request(71))
                .await
                .expect("answered")
                .unwrap_err();
            assert_eq!(
                first,
                tilo_actions::remote::ActionHostError::confirmation_required()
            );
            let asking = host.confirm(&caller, &append_request(71));
            let shown = desk.shown(1).await;
            assert!(
                shown.handle.starts_with("run-question:"),
                "{}",
                shown.handle
            );
            assert!(shown.confirming && shown.scope.is_some(), "{shown:?}");
            assert!(
                matches!(
                    question_on_run(&f).map(|question| question.answer),
                    Some(crate::services::TrackerAnswer::Waiting)
                ),
                "the run holds its question while the person is asked"
            );
            desk.answer(crate::app_actions::AppActionConsent::Once);
            let confirmed = asking.await.expect("answered").unwrap();
            let mut again = append_request(71);
            confirmed.apply(&mut again);
            let receipt = host
                .invoke(&caller, again)
                .await
                .expect("answered")
                .unwrap();
            assert!(went_on(&receipt), "{receipt:?}");
            let wire = stream.next().await.unwrap();
            assert_eq!(wire.actor, format!("agent:{}", agent.delegation.agent_id));
            assert!(question_on_run(&f).is_none(), "the answer is spent");
            let next = host
                .invoke(&caller, append_request(72))
                .await
                .expect("answered")
                .unwrap_err();
            assert_eq!(
                next,
                tilo_actions::remote::ActionHostError::confirmation_required(),
                "once is once"
            );
            drop(f);
        })
        .await;
}

/// The person denying it refuses the step, in the words a declined step is refused in, and
/// nothing reaches the app.
#[tokio::test(flavor = "current_thread")]
async fn a_door_runs_question_the_person_denies_refuses_the_step() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = admit(&f);
            let desk = asking_on_the_desk(&f, &agent);
            let (_target, _stream) = notes_target(&f).await;
            let host = f.state.task_run_host(&agent.presentation()).unwrap();
            let caller = connection(&host);
            let asking = host.confirm(&caller, &append_request(81));
            desk.shown(1).await;
            desk.answer(crate::app_actions::AppActionConsent::Deny);
            let refused = asking.await.expect("answered").unwrap_err();
            assert!(
                matches!(&refused, tilo_actions::remote::ActionHostError::Refused { reason }
                    if reason.contains(tilo_actions::draft::DECLINED)),
                "{refused:?}"
            );
            assert!(question_on_run(&f).is_none(), "nothing is left asked");
            assert_eq!(unread_invocations(200), 0, "nothing reached the app");
            drop(f);
        })
        .await;
}

/// "Always" for the agent covers its next such step: it goes on without asking again, for as
/// long as the run goes.
#[tokio::test(flavor = "current_thread")]
async fn always_allowing_a_door_runs_question_covers_its_next_step() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = admit(&f);
            let desk = asking_on_the_desk(&f, &agent);
            let (_target, stream) = notes_target(&f).await;
            let host = f.state.task_run_host(&agent.presentation()).unwrap();
            let caller = connection(&host);
            let asking = host.confirm(&caller, &append_request(91));
            desk.shown(1).await;
            desk.answer(crate::app_actions::AppActionConsent::Always);
            let confirmed = asking.await.expect("answered").unwrap();
            let mut again = append_request(91);
            confirmed.apply(&mut again);
            let receipt = host
                .invoke(&caller, again)
                .await
                .expect("answered")
                .unwrap();
            assert!(went_on(&receipt), "{receipt:?}");
            stream.next().await.unwrap();
            let next = host
                .invoke(&caller, append_request(92))
                .await
                .expect("answered")
                .unwrap();
            assert!(went_on(&next), "the next step is not asked about: {next:?}");
            stream.next().await.unwrap();
            assert_eq!(desk.asked.lock().unwrap().len(), 1, "asked once");
            drop(f);
        })
        .await;
}

/// A question nobody answers does not hold the agent forever: the connection leaving takes the
/// sheet down and the question off the run, and so does the run ending, which the agent hears.
#[tokio::test(flavor = "current_thread")]
async fn a_door_runs_question_is_withdrawn_when_its_agent_leaves_or_its_run_ends() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = admit(&f);
            let desk = asking_on_the_desk(&f, &agent);
            let (_target, _stream) = notes_target(&f).await;
            let host = f.state.task_run_host(&agent.presentation()).unwrap();
            let caller = connection(&host);
            let asking = host.confirm(&caller, &append_request(101));
            let shown = desk.shown(1).await;
            drop(asking);
            for _ in 0..500 {
                if !desk.withdrawn.lock().unwrap().is_empty() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert_eq!(*desk.withdrawn.lock().unwrap(), [shown.handle]);
            assert!(question_on_run(&f).is_none(), "the question left the run");

            let asking = host.confirm(&caller, &append_request(102));
            desk.shown(2).await;
            for run in f.native.runs.lock().unwrap().iter_mut() {
                run.active = false;
            }
            f.state.refresh_native_task_delivery();
            let refused = asking.await.expect("answered").unwrap_err();
            assert!(
                matches!(&refused, tilo_actions::remote::ActionHostError::Refused { reason }
                    if reason.contains("withdrawn")),
                "{refused:?}"
            );
            assert_eq!(desk.withdrawn.lock().unwrap().len(), 2);
            drop(f);
        })
        .await;
}
