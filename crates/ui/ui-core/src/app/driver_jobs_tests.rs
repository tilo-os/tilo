//! An app shown by the display-less driver runs the jobs the workspace's broker starts on its own
//! collaboration path, for their invoker: a caller reads their progress and result, cancels them,
//! and is refused what the broker would refuse. What the app refused is read where the app says
//! it, its controls and the routes to its actions as a person finds them, and its hosted
//! components' resources and contracts as the host checks them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixture: a failure is the assertion"
)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use tilo_actions::{
    ActionAddress, ActionSpec, ApiError, InvocationId, InvocationUpdate, OperationKind,
    ResourceSpec,
};

use crate::collaboration::CollabRequest;
use crate::headless::Driver;
use crate::state::State;
use crate::theme::Theme;
use crate::widget::{ResourceOperationStart, TaffyTreeExt as _, Widget};

const COUNT: &str = "test.worker.count";
const WAIT: &str = "test.worker.wait";
const ERASE: &str = "test.worker.erase";
const FAIL: &str = "test.worker.fail";
const UNHANDLED: &str = "test.worker.unhandled";

/// A resource whose operations a caller starts as jobs: `count` reports a step of progress and
/// then succeeds, `wait` runs until its caller cancels it, `erase` runs at once once confirmed,
/// and `fail` fails at once. A broken worker also publishes an effect nothing handles.
#[derive(Default)]
struct Worker {
    rect: vello::kurbo::Rect,
    /// The running jobs, with how many times each was polled.
    running: Vec<(InvocationId, &'static str, u32)>,
    erased: Rc<Cell<u32>>,
    broken: bool,
    /// Publishes `open-photos`, which opens Photos in the window's workspace.
    opens_photos: bool,
}

impl Worker {
    fn erasing(erased: &Rc<Cell<u32>>) -> Self {
        Self {
            erased: Rc::clone(erased),
            ..Self::default()
        }
    }
}

impl Widget for Worker {
    crate::provides!(
        dyn crate::widget::resources::PublishesResources,
        dyn crate::widget::ResourceOperations
    );
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_node(taffy::Style::default(), &[], node_map, self)
    }
    crate::rect_geometry!();
    fn name(&self) -> &'static str {
        "Worker"
    }
}

impl crate::widget::AppWidget for Worker {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Worker"
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("W")
    }
}

fn job(id: &str, label: &str, type_id: &str, cancellable: bool) -> ActionSpec {
    ActionSpec::operation(id, label, OperationKind::Effect, type_id, &())
        .unwrap()
        .execution(tilo_actions::ExecutionMode::Background { cancellable })
        .returns(tilo_actions::ResultSpec::Job)
}

impl crate::widget::resources::PublishesResources for Worker {
    fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<ResourceSpec> {
        let erase =
            ActionSpec::operation("erase", "Erase", OperationKind::Effect, ERASE, &()).unwrap();
        let mut contract = erase.contract.clone();
        contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
        let erase = erase.with_contract(contract);
        let mut resource = ResourceSpec::new("test.worker", "test.worker", "Worker")
            .with_action(job("count", "Count", COUNT, false))
            .with_action(job("wait", "Wait", WAIT, true))
            .with_action(erase)
            .with_action(
                ActionSpec::operation("fail", "Fail", OperationKind::Effect, FAIL, &()).unwrap(),
            );
        if self.opens_photos {
            resource = resource.with_action(photos().spec(
                "open-photos",
                "Open Photos",
                "Open Photos beside this window",
            ));
        }
        if self.broken {
            resource = resource.with_action(
                ActionSpec::operation("lost", "Lost", OperationKind::Effect, UNHANDLED, &())
                    .unwrap(),
            );
        }
        vec![resource]
    }
}

impl crate::widget::ResourceOperations for Worker {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        // Opening another app is the host's to route to the broker, as it is for every app.
        &[
            COUNT,
            WAIT,
            ERASE,
            FAIL,
            tilo_actions::remote::ApplicationLaunch::TYPE,
        ]
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        _doc: &mut crate::widget::Edits<'_>,
        _path: &automerge::ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        let request = op.request();
        match request.type_id {
            COUNT => self.running.push((request.invocation, COUNT, 0)),
            WAIT => self.running.push((request.invocation, WAIT, 0)),
            ERASE => {
                self.erased.set(self.erased.get() + 1);
                return Ok(Some(ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                )));
            }
            FAIL => return Err("the disk is full".into()),
            _ => return Ok(None),
        }
        Ok(Some(ResourceOperationStart::Accepted))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: InvocationId,
        cancellation_requested: bool,
        _doc: &automerge::AutoCommit,
        _path: &automerge::ObjId,
    ) -> Option<InvocationUpdate> {
        let at = self
            .running
            .iter()
            .position(|(running, ..)| *running == invocation)?;
        let (_, kind, polls) = self.running.get_mut(at)?;
        *polls += 1;
        let update = match (*kind, *polls) {
            (WAIT, _) if cancellation_requested => InvocationUpdate::Cancelled {
                invocation,
                message: Some("stopped".into()),
            },
            (COUNT, 1) => {
                return Some(InvocationUpdate::Progress {
                    invocation,
                    sequence: 1,
                    phase: "counting".into(),
                    completed: Some(1),
                    total: Some(2),
                    message: None,
                });
            }
            (COUNT, _) => InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
            _ => return None,
        };
        self.running.remove(at);
        Some(update)
    }
}

/// What `open-photos` launches.
fn photos() -> tilo_actions::remote::ApplicationLaunch {
    tilo_actions::remote::ApplicationLaunch::app("so.tilo.photos")
}

fn worker_driver(worker: Worker) -> Driver {
    let mut driver = Driver::new(State::new(), Box::new(worker), (400, 300));
    driver.connect_peer();
    driver
}

fn address(action: &str) -> ActionAddress {
    ActionAddress::new("test.worker", action)
}

fn no_arguments() -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::new()
}

#[test]
fn a_job_started_for_the_person_reports_its_progress_and_then_its_result() {
    let mut driver = worker_driver(Worker::default());
    let (invocation, started) = driver
        .start_bound_job_for(None, &address("count"), &no_arguments(), None)
        .expect("the broker admits the call");
    assert!(
        matches!(started, ResourceOperationStart::Accepted),
        "the job runs on: {started:?}"
    );
    assert!(
        driver.wait_until(crate::time::Duration::from_secs(5), |driver| {
            driver
                .job_updates(invocation)
                .iter()
                .any(InvocationUpdate::is_terminal)
        }),
        "the job finishes: {:?}",
        driver.job_updates(invocation)
    );
    let updates = driver.job_updates(invocation);
    assert!(
        matches!(
            updates.as_slice(),
            [
                InvocationUpdate::Progress {
                    completed: Some(1),
                    total: Some(2),
                    ..
                },
                InvocationUpdate::Succeeded { .. }
            ]
        ),
        "its caller hears the step, then the result: {updates:?}"
    );
    assert!(driver.control_failures().is_empty());
}

#[test]
fn an_agent_s_job_stops_when_its_caller_cancels_it() {
    let mut driver = worker_driver(Worker::default());
    let (invocation, _) = driver
        .start_bound_job_for(
            Some(tilo_actions::ActionInvoker::agent("scribe")),
            &address("wait"),
            &no_arguments(),
            None,
        )
        .expect("the broker admits the agent's call");
    driver.idle();
    assert!(
        driver
            .job_updates(invocation)
            .iter()
            .all(|update| !update.is_terminal()),
        "the job runs on until it is cancelled: {:?}",
        driver.job_updates(invocation)
    );
    let ended = driver.cancel_job(invocation);
    assert!(
        matches!(ended, Some(InvocationUpdate::Cancelled { .. })),
        "the job answers that it stopped: {ended:?}"
    );
    assert!(
        matches!(
            driver.job_updates(invocation).last(),
            Some(InvocationUpdate::Cancelled { .. })
        ),
        "{:?}",
        driver.job_updates(invocation)
    );
    assert_eq!(
        driver.cancel_job(invocation),
        None,
        "an ended job stops once"
    );
    assert_eq!(driver.control_failures(), ["cancelled"]);
}

#[test]
fn a_call_needing_confirmation_is_refused_unconfirmed_and_runs_once_confirmed() {
    let erased = Rc::new(Cell::new(0));
    let mut driver = worker_driver(Worker::erasing(&erased));
    let refused = driver.start_bound_job_for(None, &address("erase"), &no_arguments(), None);
    assert!(
        matches!(refused, Err(ApiError::ConfirmationRequired)),
        "{refused:?}"
    );
    assert_eq!(erased.get(), 0, "nothing ran for the unconfirmed call");
    assert!(
        !driver
            .peer_requests()
            .iter()
            .any(|request| matches!(request, CollabRequest::UpdateInvocation { .. })),
        "the refused call never reached the app: {:?}",
        driver.peer_requests()
    );
    let approval = driver.approval();
    let (invocation, started) = driver
        .start_bound_job_for(None, &address("erase"), &no_arguments(), Some(&approval))
        .expect("the confirmed call runs");
    assert!(
        matches!(started, ResourceOperationStart::Completed(_)),
        "{started:?}"
    );
    assert_eq!(erased.get(), 1);
    assert!(
        matches!(
            driver.job_updates(invocation).as_slice(),
            [InvocationUpdate::Succeeded { .. }]
        ),
        "{:?}",
        driver.job_updates(invocation)
    );
}

#[test]
fn a_call_that_fails_at_once_is_refused_with_the_app_s_reason() {
    let mut driver = worker_driver(Worker::default());
    let refused = driver
        .start_bound_job_for(None, &address("fail"), &no_arguments(), None)
        .expect_err("the app failed the call");
    assert!(
        refused.to_string().contains("the disk is full"),
        "{refused}"
    );
    assert!(
        driver
            .control_failures()
            .iter()
            .any(|failure| failure.contains("the disk is full")),
        "{:?}",
        driver.control_failures()
    );
    let unknown = driver.start_bound_job_for(None, &address("missing"), &no_arguments(), None);
    assert!(matches!(unknown, Err(ApiError::Unknown(_))), "{unknown:?}");
}

#[test]
fn the_requests_the_app_made_of_its_peer_are_taken_once() {
    let mut driver = worker_driver(Worker::default());
    let (invocation, _) = driver
        .start_bound_job_for(None, &address("count"), &no_arguments(), None)
        .unwrap();
    driver.wait_until(crate::time::Duration::from_secs(5), |driver| {
        driver
            .job_updates(invocation)
            .iter()
            .any(InvocationUpdate::is_terminal)
    });
    let taken = driver.take_collaboration_requests();
    let answers = taken
        .iter()
        .filter(|request| {
            matches!(
                request,
                CollabRequest::UpdateInvocation { update, .. } if update.invocation() == invocation
            )
        })
        .count();
    assert_eq!(answers, 2, "the step and the result: {taken:?}");
    assert!(
        driver.take_collaboration_requests().is_empty(),
        "what was taken is not taken again"
    );
}

#[test]
fn kept_invocation_results_say_what_each_call_answered_and_never_its_arguments() {
    let erased = Rc::new(Cell::new(0));
    // The person's own calls go through the window's broker, which hands each back to run.
    let mut driver = worker_driver(Worker::erasing(&erased));
    assert!(
        driver.invocation_results_json().is_err(),
        "nothing is kept unless asked"
    );
    driver.retain_invocation_results();
    let invoke = |action: &str, arguments: serde_json::Value, confirm, expected: Option<&str>| {
        crate::headless::Action::Invoke(crate::headless::Invocation {
            resource: "test.worker".into(),
            action: action.into(),
            args: arguments.as_object().unwrap().clone(),
            confirm,
            confirmation_phrase: None,
            expect_error: expected.map(str::to_owned),
            bind: None,
        })
    };
    driver
        .perform(&invoke("erase", serde_json::json!({}), true, None))
        .unwrap();
    driver
        .perform(&invoke("count", serde_json::json!({}), false, None))
        .unwrap();
    driver
        .perform(&invoke(
            "missing",
            serde_json::json!({"secret": "never-copy-this"}),
            false,
            Some("unknown_action"),
        ))
        .unwrap();
    assert_eq!(
        erased.get(),
        1,
        "the broker handed the confirmed erase back once"
    );
    let raw = driver.invocation_results_json().unwrap();
    assert!(!raw.contains("never-copy-this"), "{raw}");
    let results: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(results["schema_version"], 1);
    assert_eq!(
        results["author"],
        serde_json::json!(driver.app().collaboration.local_author)
    );
    let statuses = results["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["status"].as_str().unwrap())
        .collect::<Vec<_>>();
    // What each call answered when it was made: the broker took the erase and the job, and
    // the unknown action never reached it.
    assert_eq!(statuses, ["accepted", "accepted", "refused"], "{raw}");
    assert_eq!(results["entries"][2]["code"], "unknown_action");
    assert_eq!(results["entries"][2]["address"]["action"], "missing");
}

/// Two resources whose `edit` a button runs, the same label on both.
fn editable_pair() -> (Driver, Rc<Cell<u32>>) {
    use crate::widget::{Button, Column, resource_actions};
    let calls = Rc::new(Cell::new(0));
    let mut column = Column::new();
    for id in ["first", "second"] {
        let mut resource = resource_actions::resource(id, "Same label", "edit", "Edit");
        resource.id = tilo_actions::ResourceId::new(id);
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
        let counter = Rc::clone(&calls);
        column = column.push(resource_actions::immediate(
            Box::new(Button::new(id, "Same label").on_action(binding)),
            resource,
            move |_| {
                counter.set(counter.get() + 1);
                Ok(tilo_actions::ActionOutput::default())
            },
        ));
    }
    (
        Driver::new(State::new(), Box::new(column), (400, 240)),
        calls,
    )
}

#[test]
fn the_route_to_an_action_is_the_control_a_person_presses_for_it() {
    let (mut driver, calls) = editable_pair();
    let routes = driver.action_ui_routes(&ActionAddress::new("second", "edit"));
    let [route] = routes.as_slice() else {
        panic!("one control runs the second resource's edit: {routes:?}");
    };
    let bounds = route.bounds.expect("the control is on screen");
    assert_eq!(bounds, driver.targets()["second"]);
    assert!(!route.disabled);
    let centre = bounds.center();
    driver.click_at(centre.x, centre.y);
    assert_eq!(
        calls.get(),
        1,
        "pressing where the route stands runs the action"
    );
}

#[test]
fn controls_list_what_a_person_can_press_with_their_name_bounds_and_focus() {
    use crate::widget::{Button, Column};
    let column = Column::new()
        .push(Box::new(Button::new("open", "Same label").on_click(|| {})))
        .push(Box::new(Button::new("close", "Same label").on_click(|| {})));
    let mut driver = Driver::new(State::new(), Box::new(column), (400, 240));
    let buttons = driver
        .controls()
        .into_iter()
        .filter(|control| control.role == accesskit::Role::Button)
        .collect::<Vec<_>>();
    assert_eq!(buttons.len(), 2, "{buttons:?}");
    for button in &buttons {
        assert_eq!(button.name.as_deref(), Some("Same label"));
        assert!(
            button.verbs.iter().any(|verb| verb == "activate"),
            "{button:?}"
        );
        assert!(button.bounds.is_some_and(|bounds| bounds.area() > 0.0));
        assert!(!button.disabled && !button.focused);
    }
    driver.tab(false);
    let focused = driver
        .controls()
        .into_iter()
        .filter(|control| control.focused)
        .collect::<Vec<_>>();
    assert_eq!(focused.len(), 1, "Tab focuses one control: {focused:?}");
}

fn button_for(role: crate::permissions::Role) -> Driver {
    let button = crate::widget::Button::new("go", "Send").on_click(|| {});
    let mut driver = Driver::new(State::new(), Box::new(button), (600, 200));
    // The window's press reaches the workspace that grants its effects, as a window placed in
    // one does: without the stand-in for it, the effect would run at once and never be asked.
    driver.connect_peer();
    driver.set_permission_ctx(crate::permissions::PermissionContext::member(
        &crate::permissions::MemberId::person("ada"),
        role,
    ));
    driver.relayout();
    driver
}

#[test]
fn a_control_naming_nothing_the_window_has_is_refused_said_and_kept_among_the_failures() {
    let (probe, observed) = crate::widget::control_result_probe::Probe::new(false);
    let mut driver = Driver::new(State::new(), Box::new(probe), (400, 300));
    assert_eq!(driver.refusal_said(), None);
    assert!(driver.control_failures().is_empty());
    observed.enqueue("missing");
    driver.send(&winit::event::WindowEvent::Focused(true));
    driver.relayout();
    assert!(
        matches!(observed.results.borrow().as_slice(), [Err(_)]),
        "the control hears it was refused: {:?}",
        observed.results.borrow()
    );
    assert!(
        driver.refusal_said().is_some(),
        "the refusal is said, never dropped unseen"
    );
    assert_eq!(
        driver.control_failures().len(),
        1,
        "and kept for the host to read: {:?}",
        driver.control_failures()
    );
    assert!(driver.asking().is_none(), "a refusal asks nothing");
}

#[test]
fn a_press_the_workspace_s_grants_ask_about_waits_for_the_person() {
    let mut driver = button_for(crate::permissions::Role::Editor);
    assert_eq!(driver.asking(), None, "nothing is asked before the press");
    driver.click_named("go").unwrap();
    driver.relayout();
    let (_, name) = driver.asking().expect("a guest's effect waits for the ask");
    assert!(!name.is_empty(), "the ask names the action");
    assert_eq!(driver.refusal_said(), None, "an ask is not a refusal");
}

thread_local! {
    static NEXT_WORKER: RefCell<Option<Worker>> = const { RefCell::new(None) };
}

fn worker_factory(
    mut context: crate::application::ProcessContext,
    _: crate::application::LaunchTarget,
    _: crate::application::RestoreInput,
) -> Result<crate::application::AppComponent, crate::launch::LaunchError> {
    context
        .initialize_new_resource(|_| Ok(()))
        .map_err(crate::launch::LaunchError::Record)?;
    Ok(crate::application::AppComponent::new(
        context,
        Box::new(NEXT_WORKER.with_borrow_mut(|next| next.take().unwrap())),
        tilo_actions::ActionRegistry::new(),
    ))
}

/// `worker` hosted as the component of an app of its own.
fn hosted(host: &crate::application_test_support::ProbeHarness, worker: Worker) -> Driver {
    NEXT_WORKER.with_borrow_mut(|next| *next = Some(worker));
    let definition = crate::application_test_support::definition(worker_factory);
    let target = crate::application::LaunchTarget::New;
    let context = host
        .prepare_with_permissions(
            &definition,
            31,
            &target,
            crate::application_test_support::runner_permissions(),
        )
        .unwrap();
    let component = definition
        .instantiate(context, target, crate::application::RestoreInput::None)
        .unwrap();
    Driver::mount(component).unwrap()
}

#[test]
fn a_hosted_component_lists_its_resources_whole_by_instance_and_by_id() {
    let host = crate::application_test_support::ProbeHarness::new();
    let driver = hosted(&host, Worker::default());
    let ids = |resources: &[ResourceSpec]| {
        resources
            .iter()
            .map(|resource| resource.id.as_str().to_owned())
            .collect::<Vec<_>>()
    };
    let all = driver.bound_resources(None).unwrap();
    let worker = all
        .iter()
        .find(|resource| resource.id.as_str() == "test.worker")
        .unwrap_or_else(|| panic!("the component publishes its worker: {:?}", ids(&all)));
    let actions = worker
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(actions, ["count", "wait", "erase", "fail"]);
    let instance = driver.component_instance_id().unwrap();
    assert_eq!(
        ids(&driver.bound_resources_of(Some(instance), None).unwrap()),
        ids(&all),
        "the one component's resources are the window's"
    );
    let found = driver.bound_resources(Some("test.worker")).unwrap();
    assert_eq!(found.len(), 1);
    assert!(
        driver
            .bound_resources(Some("test.nothing"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_action_breaking_its_contract_is_named_before_the_host_withdraws_the_component() {
    let host = crate::application_test_support::ProbeHarness::new();
    let mut healthy = hosted(&host, Worker::default());
    assert_eq!(healthy.component_contract_violations(), []);
    drop(healthy);
    let host = crate::application_test_support::ProbeHarness::new();
    let mut broken = hosted(
        &host,
        Worker {
            broken: true,
            ..Worker::default()
        },
    );
    let violations = broken.component_contract_violations();
    assert!(
        violations
            .iter()
            .any(|violation| violation.address.contains("lost")),
        "the action nothing handles is named: {violations:?}"
    );
}

#[test]
fn the_person_s_own_effect_comes_back_to_the_app_as_a_call_and_runs_once() {
    let erased = Rc::new(Cell::new(0));
    let mut driver = worker_driver(Worker::erasing(&erased));
    driver
        .invoke_action(&address("erase"), &no_arguments(), Some(&driver.approval()))
        .expect("the person's confirmed call is taken");
    driver.idle();
    assert_eq!(
        erased.get(),
        1,
        "the broker delivered the call back to the app"
    );
    let requests = driver.peer_requests();
    assert!(
        requests.iter().any(|request| matches!(
            request,
            CollabRequest::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::Invoke(invoke),
            } if invoke.target.address == address("erase")
        )),
        "the call went to the broker: {requests:?}"
    );
    assert!(
        driver.control_failures().is_empty(),
        "the broker's receipt refused nothing: {:?}",
        driver.control_failures()
    );
}

#[test]
fn a_job_started_within_a_run_keeps_the_run_s_invocation_and_is_awaited_to_its_end() {
    let mut driver = worker_driver(Worker::default());
    let run = InvocationId::new();
    let ambient = tilo_actions::ActionContext {
        source_invocation: Some(run),
        ..tilo_actions::ActionContext::default()
    };
    let (invocation, _) = driver
        .start_bound_job_within(
            Some(tilo_actions::ActionInvoker::agent("scribe")),
            Some(&ambient),
            &crate::bound_dispatch::BoundActionAddress {
                instance_id: 1,
                address: address("count"),
            },
            &no_arguments(),
            None,
        )
        .expect("the broker admits the agent's call");
    assert_eq!(invocation, run, "the job runs under the run's invocation");
    let ended = driver.await_job(invocation, crate::time::Duration::from_secs(5));
    assert!(
        matches!(ended, Some(InvocationUpdate::Succeeded { .. })),
        "the job ends: {ended:?}"
    );
}

/// The invocations of its own actions the app handed its broker, oldest first.
fn own_invocations(driver: &Driver) -> Vec<tilo_actions::remote::InvokeRequest> {
    driver
        .peer_requests()
        .into_iter()
        .filter_map(|request| match request {
            CollabRequest::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::Invoke(request),
            } if request.target.provider == tilo_actions::remote::OWN_PROVIDER => Some(request),
            _ => None,
        })
        .collect()
}

#[test]
fn a_person_s_press_is_handed_back_as_a_call_and_runs_once() {
    let erased = Rc::new(Cell::new(0));
    let mut driver = worker_driver(Worker::erasing(&erased));
    driver
        .invoke_action(&address("erase"), &no_arguments(), Some(&driver.approval()))
        .expect("the window takes the press");
    driver.idle();
    let asked = own_invocations(&driver);
    let [asked] = asked.as_slice() else {
        panic!("the press went to the broker once: {asked:?}");
    };
    assert_eq!(asked.target.address, address("erase"));
    assert_eq!(erased.get(), 1, "the delivered press ran once");
    assert!(
        matches!(
            driver.job_updates(asked.invocation).as_slice(),
            [InvocationUpdate::Succeeded { .. }]
        ),
        "the delivery answered: {:?}",
        driver.job_updates(asked.invocation)
    );
    assert_eq!(driver.refusal_said(), None);
}

#[test]
fn the_window_s_session_opens_another_app_and_its_receipt_comes_back() {
    let mut driver = worker_driver(Worker {
        opens_photos: true,
        ..Worker::default()
    });
    let workspace = uuid::Uuid::new_v4();
    let session = std::sync::Arc::new(tilo_actions::remote::TestProvider::new());
    driver.set_session_api(
        crate::RemoteActions::with_provider(
            session.clone(),
            tilo_actions::remote::Caller::local_app("test.worker")
                .in_workspace(workspace.to_string()),
        ),
        Some(workspace.to_string()),
    );
    driver
        .invoke_action(&address("open-photos"), &no_arguments(), None)
        .expect("the window takes the press");
    driver.idle();
    let asked = session.take_asked();
    assert_eq!(asked.len(), 1, "the session is asked to open Photos once");
    assert_eq!(asked[0].target, photos().target(&workspace.to_string()));
    let [own] = own_invocations(&driver)
        .try_into()
        .unwrap_or_else(|asked: Vec<_>| panic!("the press went to the broker once: {asked:?}"));
    assert!(
        matches!(
            driver.job_updates(own.invocation).as_slice(),
            [InvocationUpdate::Succeeded { .. }]
        ),
        "the session's receipt finished the press: {:?}",
        driver.job_updates(own.invocation)
    );
    assert_eq!(driver.refusal_said(), None);
}

#[test]
fn opening_another_app_with_no_session_is_refused_and_said() {
    let mut driver = worker_driver(Worker {
        opens_photos: true,
        ..Worker::default()
    });
    assert!(
        driver
            .app_mut()
            .collaboration
            .manager
            .send(CollabRequest::Hello {
                app_id: "test.worker".into(),
                cluster_id: uuid::Uuid::new_v4(),
                record: None,
                resource_type: None,
                handle: None,
            })
    );
    driver.relayout();
    driver
        .invoke_action(&address("open-photos"), &no_arguments(), None)
        .expect("the window takes the press");
    driver.idle();
    let [own] = own_invocations(&driver)
        .try_into()
        .unwrap_or_else(|asked: Vec<_>| panic!("the press went to the broker once: {asked:?}"));
    assert!(
        matches!(
            driver.job_updates(own.invocation).as_slice(),
            [InvocationUpdate::Failed { .. }]
        ),
        "nothing opened it, and the press says so: {:?}",
        driver.job_updates(own.invocation)
    );
    assert!(
        driver.refusal_said().is_some(),
        "the window says why nothing opened"
    );
}

// ---- what the retained invocation envelope keeps, and where it stops ----

const RESULT_ENTRY_LIMIT: usize = 4096;
const RESULT_BYTE_LIMIT: usize = 16 * 1024 * 1024;

/// An invocation of `action` on `resource` with `arguments`, as a scenario step runs one.
fn recorded(
    resource: &str,
    action: &str,
    arguments: serde_json::Value,
    expected: Option<&str>,
) -> crate::headless::Action {
    crate::headless::Action::Invoke(crate::headless::Invocation {
        resource: resource.into(),
        action: action.into(),
        args: arguments.as_object().unwrap().clone(),
        confirm: false,
        confirmation_phrase: Some("private confirmation".into()),
        expect_error: expected.map(str::to_owned),
        bind: None,
    })
}

fn unknown_recorded() -> crate::headless::Action {
    recorded(
        "missing",
        "action",
        serde_json::json!({"secret": "private arguments"}),
        Some("unknown_action"),
    )
}

#[test]
fn a_real_background_admission_is_not_relabelled_completed() {
    let owner = crate::application_test_support::ProbeHarness::new();
    let (component, observed) = crate::widget::control_result_probe::component(&owner, 17, true);
    let mut driver = Driver::from_component(
        component,
        (640, 480),
        crate::application_test_support::runner_inputs(),
    )
    .unwrap();
    let address = tilo_actions::api::survey(&driver.resources())
        .into_iter()
        .find(|action| action.address.action == "run")
        .expect("the real component publishes its action")
        .address;
    driver.retain_invocation_results();
    driver
        .perform(&recorded(
            &address.resource,
            &address.action,
            serde_json::json!({}),
            None,
        ))
        .unwrap();
    assert_eq!(observed.calls.get(), 1);
    let result: serde_json::Value =
        serde_json::from_str(&driver.invocation_results_json().unwrap()).unwrap();
    assert_eq!(
        result["entries"][0]["status"], "accepted",
        "an admitted call that runs on is not a completed one: {result}"
    );
    assert!(result["entries"][0].get("output").is_none());
}

#[test]
fn a_retention_byte_failure_preserves_the_prefix_and_does_not_hide_an_executed_action() {
    let (mut driver, ran) = editable_pair();
    driver.retain_invocation_results();
    driver.perform(&unknown_recorded()).unwrap();
    // Exhaust the recording through the real semantic operation rather than fabricate a refused
    // outcome after the operation ran.
    driver.set_invocation_result_bytes(0);
    assert!(
        driver
            .perform(&recorded("first", "edit", serde_json::json!({}), None))
            .is_err()
    );
    assert_eq!(
        ran.get(),
        1,
        "the operation really ran, whatever the envelope could keep"
    );
    // A caller may ignore the first error. It cannot silently resume the journal after an omitted
    // invocation and turn the log into a prefix.
    driver.set_invocation_result_bytes(RESULT_BYTE_LIMIT);
    assert!(driver.perform(&unknown_recorded()).is_err());
    let result: serde_json::Value =
        serde_json::from_str(&driver.invocation_results_json().unwrap()).unwrap();
    assert_eq!(result["entries"].as_array().unwrap().len(), 1);
    assert!(
        result["retention_error"]
            .as_str()
            .unwrap()
            .contains("action 2"),
        "the envelope says it stopped short: {result}"
    );
}

#[test]
fn the_entry_limit_fails_the_next_scenario_step_with_a_retained_prefix() {
    let mut driver = Driver::new(
        State::new(),
        Box::new(crate::widget::Text::new("results-fixture")),
        (640, 480),
    );
    driver.retain_invocation_results();
    for _ in 0..RESULT_ENTRY_LIMIT {
        driver.perform(&unknown_recorded()).unwrap();
    }
    assert!(driver.perform(&unknown_recorded()).is_err());
    let result: serde_json::Value =
        serde_json::from_str(&driver.invocation_results_json().unwrap()).unwrap();
    assert_eq!(
        result["entries"].as_array().unwrap().len(),
        RESULT_ENTRY_LIMIT
    );
    assert!(result["retention_error"].is_string());
}

/// A startup control that fails fails the run, and the explicit results envelope is kept, empty.
#[test]
fn a_failed_startup_control_keeps_an_empty_explicit_invocation_envelope() {
    let owner = crate::application_test_support::ProbeHarness::new();
    let (component, observed) = crate::widget::control_result_probe::component(&owner, 18, false);
    observed.enqueue("missing");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("results.json");
    let spec = crate::headless::Spec {
        results: Some(path.to_str().unwrap().to_owned()),
        ..Default::default()
    };
    let code = Driver::run_component(
        &spec,
        component,
        crate::application_test_support::runner_inputs(),
    );
    assert_eq!(code, 1, "the failed startup control fails the run");
    assert_eq!(observed.calls.get(), 0);
    let result: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        result["entries"],
        serde_json::json!([]),
        "the envelope is kept, and empty: {result}"
    );
}

// ---- visual waits service the writer, but never substitute submission for its ACK ----

use crate::application::{LaunchTarget, RestoreInput};
use crate::application_test_support::{
    self as probe, ProbeHarness, runner_inputs, runner_permissions, runner_probe_definition,
};
use crate::resource::{DocumentCheckout, ResourceOwners, ResourceSaveStatus};
use automerge::{ROOT, ReadDoc, transaction::Transactable};

struct VisualFixture {
    host: ProbeHarness,
    frame: Driver,
    checkout: DocumentCheckout,
}

impl VisualFixture {
    fn new(host: ProbeHarness) -> Self {
        let definition = runner_probe_definition();
        let component = definition
            .instantiate(
                host.prepare_with_permissions(
                    definition,
                    11,
                    &LaunchTarget::New,
                    runner_permissions(),
                )
                .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let checkout = component.record().clone();
        let frame = Driver::from_component(component, (400, 300), runner_inputs()).unwrap();
        Self {
            host,
            frame,
            checkout,
        }
    }

    /// An edit the window made, as its actions commit one.
    fn edit(&self) {
        self.host
            .records
            .with(&self.checkout.id, |doc| {
                doc.put(ROOT, "visual-wait", "edited while waiting")
                    .unwrap();
                doc.commit();
            })
            .unwrap();
    }

    fn status(&self) -> ResourceSaveStatus {
        self.host.records.save_status(&self.checkout.id).unwrap()
    }
}

/// What a process that did not write it reads back from the workspace.
fn reopened_value(host: &ProbeHarness, checkout: &DocumentCheckout) -> String {
    let mut owners = ResourceOwners::writing(host.workspace.clone());
    let records = owners.open(probe::APP, probe::WORKSPACE).unwrap();
    records.open_existing_resource_checked(checkout).unwrap();
    records
        .read(&checkout.id, |doc| {
            doc.get(ROOT, "visual-wait")
                .unwrap()
                .unwrap()
                .0
                .as_str()
                .unwrap()
                .to_owned()
        })
        .unwrap()
}

#[test]
fn visual_wait_submits_and_samples_a_save_before_its_first_readiness_check() {
    let mut fixture = VisualFixture::new(ProbeHarness::new());
    fixture.edit();
    fixture.frame.relayout();
    assert_eq!(fixture.status(), ResourceSaveStatus::Saving);
    assert!(fixture.frame.root_widget().has_pending_visual_work());
    fixture
        .frame
        .wait_for_visuals(crate::time::Duration::ZERO)
        .unwrap();
    assert_eq!(fixture.status(), ResourceSaveStatus::Saved);
    assert!(!fixture.frame.root_widget().has_pending_visual_work());
}

#[test]
fn visual_wait_does_not_confuse_writer_submission_with_acknowledgement() {
    let mut fixture = VisualFixture::new(ProbeHarness::holding_writes());
    fixture.edit();
    assert!(
        fixture
            .frame
            .wait_for_visuals(crate::time::Duration::ZERO)
            .is_err()
    );
    assert_eq!(fixture.status(), ResourceSaveStatus::Saving);
    let writes = fixture.host.held_writes();
    assert_eq!(
        writes.len(),
        1,
        "the wait must offer the actual dirty record"
    );
    assert_eq!(writes[0].id, fixture.checkout.id.route());
    assert!(fixture.frame.root_widget().has_pending_visual_work());
    // The workspace takes the write the wait offered, and the store hears the answer.
    // A close's flush waits for the workspace, so it takes the write up.
    fixture.host.records.try_flush().unwrap();
    // The answer is taken up by the window's own passes, which service the store's wake.
    fixture.frame.persist_tick();
    fixture.frame.service_pending_owner_models().unwrap();
    fixture.frame.persist_tick();
    assert_eq!(fixture.status(), ResourceSaveStatus::Saved);
    fixture
        .frame
        .wait_for_visuals(tilo_test_env::hang_bound())
        .unwrap();
    assert_eq!(fixture.status(), ResourceSaveStatus::Saved);
    assert!(!fixture.frame.root_widget().has_pending_visual_work());
    // Reopen only the bytes the wait actually submitted, not the live writer's cache or a
    // close/flush that could conceal a missing tick.
    assert_eq!(
        reopened_value(&fixture.host, &fixture.checkout),
        "edited while waiting"
    );
}

#[test]
fn waiting_services_edits_arriving_after_the_initial_idle_pass() {
    let mut fixture = VisualFixture::new(ProbeHarness::new());
    let records = fixture.host.records.clone();
    let id = fixture.checkout.id.clone();
    let mut edited = false;
    assert!(fixture.frame.wait_until(tilo_test_env::hang_bound(), |_| {
        if !edited {
            edited = true;
            records
                .with(&id, |doc| {
                    doc.put(ROOT, "late", true).unwrap();
                    doc.commit();
                })
                .unwrap();
            assert_eq!(
                records.save_status(&id).unwrap(),
                ResourceSaveStatus::Saving
            );
            return false;
        }
        records.save_status(&id).unwrap() == ResourceSaveStatus::Saved
    }));
    assert!(!fixture.frame.root_widget().has_pending_visual_work());
}
