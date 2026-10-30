//! The desktop's agent door, served as a task agent in its run
//! ([`crate::collab_broker_capnp::TaskRunHost`]): an admitted connection
//! takes an application's action and a host step only as the run admits
//! them, and loses both once its person stops the run.
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
const DOOR_ISSUE: &str = "tasks.tracker/issues/door-session";
/// The host steps' provider in this fixture.
const DESKTOP_PROVIDER: &str = "test-desktop";

/// A desktop whose one step lists its windows.
#[derive(Default)]
struct Desktop {
    started: Mutex<Vec<String>>,
    /// Whether its steps are a task agent's steps of its run.
    for_tasks: bool,
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
struct DoorAgent {
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
        room().to_string(),
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
    fn presentation(&self) -> Vec<u8> {
        let room = room().to_string();
        runtime_collab::TaskAgentJoinIdentity {
            workspace_id: room.clone(),
            device_certificate: serde_json::to_vec(&self.device).unwrap(),
            delegation: self.delegation.encode(),
            proof: self
                .key
                .sign(&runtime_identity::collaboration_join_signing_bytes(&room))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap()
    }
}

/// The person admitted the client: Tasks holds the issue, assigned to the
/// door's task agent, with its run going and carrying its delegation.
fn admit(f: &Fixture) -> DoorAgent {
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
        question: None,
        delegation: Some(Ok(agent.delegation.encode())),
    });
    agent
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
fn connection(host: &TaskRunHost) -> Caller {
    Caller::local_agent(host.agent())
        .in_room(host.room())
        .acting_for(host.person())
        .in_new_execution()
}

/// Admitted at the door, the connection takes an application's action as a
/// child of its run: attributed to its task agent, in the run's group, owned
/// by the run, and observed back by the connection.
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
            let delivered = stream.read_request().send().promise.await.unwrap();
            let wire = delivered.get().unwrap();
            assert_eq!(
                wire.get_actor().unwrap().to_str().unwrap(),
                format!("agent:{}", agent.delegation.agent_id)
            );
            let context: tilo_actions::ActionContext =
                serde_json::from_str(wire.get_context_json().unwrap().to_str().unwrap()).unwrap();
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
            let route = CrdtRoute::record(
                crate::collab_broker_capnp::task_run_host::TASK_AGENT_APP,
                room().to_string(),
                "",
            );
            let admitted = crate::collab_broker_capnp::verify_join_identity(
                &route,
                &agent.presentation(),
                f.state.membership.as_ref(),
                f.state.trusted_devices.as_deref().map(Vec::as_slice),
                f.state.certificate_authority.get().map(Arc::as_ref),
            )
            .unwrap();
            let actor = admitted.broker_actor();
            let catalogue: serde_json::Value = serde_json::from_str(
                &f.state
                    .action_catalogue_json_for(&room().to_string(), &actor, None)
                    .unwrap(),
            )
            .unwrap();
            assert!(
                catalogue["surfaces"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|surface| surface["provider"] == DESKTOP_PROVIDER
                        && surface["actions"][0]["action"] == "list-windows"),
                "the task step is in the task agent's catalogue: {catalogue}"
            );
            let channel = f.channel_admitted(300, &route, admitted);
            let run = delivered(&channel).await;
            let take = |invocation: u128| {
                let mut request = channel.invoke_task_action_request();
                let mut p = request.get();
                p.set_run_id(run.run_id);
                p.set_lease(&run.lease);
                p.set_peer_id(0);
                p.set_provider(DESKTOP_PROVIDER);
                p.set_resource("desktop");
                p.set_action("list-windows");
                p.set_invocation(uuid::Uuid::from_u128(invocation).to_string());
                request.send().promise
            };
            let taken = take(61).await.expect("the run takes its task step");
            let update: tilo_actions::InvocationUpdate = serde_json::from_str(
                taken
                    .get()
                    .unwrap()
                    .get_update_json()
                    .unwrap()
                    .to_str()
                    .unwrap(),
            )
            .unwrap();
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

/// The origin of the room's records, as a test sees what reaches it.
#[derive(Default)]
struct Origin {
    applied: Mutex<Vec<(String, String)>>,
}

impl crate::services::RecordChangeServices for Origin {
    fn apply(
        &self,
        _room: &str,
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

/// A run changes a record outside its app (a program's `tasks.add`, from
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
