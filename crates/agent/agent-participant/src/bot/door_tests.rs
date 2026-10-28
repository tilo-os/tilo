#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion"
)]

use std::collections::VecDeque;
use std::future::Future;
use std::task::{Context, Poll, Waker};

use tilo_actions::remote::RemoteActionTarget;

use super::*;

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

type Invoked = Arc<Mutex<Vec<(ActionInvoker, ActionContext, ActionAddress)>>>;

/// Records `rec-*` held; a `send` queues an outbox entry and a field edit
/// settles one.
#[derive(Default)]
struct FakeParticipant {
    invoked: Invoked,
    outbox: Arc<Mutex<Vec<OutboxRow>>>,
    history: Arc<Mutex<Vec<(DocumentRoute, ActionContext)>>>,
    /// The entries the step in history queued, as its change shows.
    queued_in_history: Arc<Mutex<Vec<String>>>,
}

fn route(app: &str, record: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: app.into(),
        cluster_id: ROOM.into(),
        record: record.into(),
    }
}

impl DoorParticipant for FakeParticipant {
    fn room(&self) -> &str {
        ROOM
    }

    fn offered(&self) -> Vec<(DocumentRoute, Vec<tilo_actions::door::ActionDescriptor>)> {
        Vec::new()
    }

    fn holds(&self, route: &DocumentRoute) -> bool {
        route.record.starts_with("rec-")
    }

    fn invoke(
        &mut self,
        _route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
        invoker: &ActionInvoker,
        context: &ActionContext,
        _confirmed: bool,
    ) -> Result<ActionOutput, DoorError> {
        self.invoked
            .lock()
            .unwrap()
            .push((invoker.clone(), context.clone(), address.clone()));
        let mut outbox = self.outbox.lock().unwrap();
        match address.action.as_str() {
            "send" => {
                let id = format!("q{}", outbox.len() + 1);
                outbox.push(OutboxRow {
                    id,
                    value: json!({"op": {"send": arguments}}),
                    settled: None,
                });
            }
            "field-edit" => {
                let edit = &arguments["edit"];
                let id = edit["id"].as_str().unwrap();
                let row = outbox.iter_mut().find(|row| row.id == id).unwrap();
                row.settled = Some(edit["outcome"].clone());
            }
            _ => {}
        }
        Ok(ActionOutput::default())
    }

    fn find_in_history(
        &self,
        step: &RunStep,
        actor: &str,
    ) -> Option<(DocumentRoute, ActionContext)> {
        self.history
            .lock()
            .unwrap()
            .iter()
            .find(|(_, context)| {
                RunStep::of(context).as_ref() == Some(step)
                    && context.invoker.as_ref().map(|i| i.actor.as_str()) == Some(actor)
            })
            .cloned()
    }

    fn outbox(&self, _route: &DocumentRoute, _field: &str) -> Vec<OutboxRow> {
        self.outbox.lock().unwrap().clone()
    }

    fn queued_in_history(
        &self,
        _route: &DocumentRoute,
        _step: &RunStep,
        _actor: &str,
        _field: &str,
    ) -> Vec<String> {
        self.queued_in_history.lock().unwrap().clone()
    }
}

struct Scripted(Mutex<VecDeque<Result<Value, PerformError>>>);

impl Performer for Scripted {
    fn perform(&self, _queued: Value) -> Result<Value, PerformError> {
        self.0.lock().unwrap().pop_front().unwrap()
    }
}

fn door(
    fake: FakeParticipant,
    results: Vec<Result<Value, PerformError>>,
) -> BotDoor<FakeParticipant> {
    BotDoor::new(
        Arc::new(Mutex::new(fake)),
        "b1".into(),
        "ada".into(),
        vec![OutboxSpec {
            app_id: "so.tilo.discord".into(),
            field: "outbox".into(),
            connector: Arc::new(Scripted(Mutex::new(results.into()))),
        }],
    )
}

/// What the runner puts on step `0` of run `r1`.
fn step_context() -> ActionContext {
    ActionContext::default()
        .with_group("r1", "Mirror", "shortcut")
        .with_facet(tilo_actions::ActionFacet::new(
            tilo_automation::STEP_FACET,
            "0",
        ))
}

fn request(app: &str, record: &str, action: &str) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: record_provider(&route(app, record)),
            address: ActionAddress::new("r", action),
        },
        argument: InvokeArgument::Json(Map::from_iter([("text".into(), json!("hi"))])),
        context: Some(step_context()),
        confirmation: None,
        appearance: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
    }
}

fn post() -> InvokeRequest {
    request("so.tilo.chat", "rec-1", "post")
}

fn send() -> InvokeRequest {
    request("so.tilo.discord", "rec-2", "send")
}

fn bot() -> Caller {
    Caller::bot("b1", [1; 32], [2; 32]).in_room(ROOM)
}

fn block<T>(pending: Pending<T>) -> T {
    let mut pending = std::pin::pin!(pending);
    match pending
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(Some(value)) => value,
        _ => panic!("the door answers at once"),
    }
}

#[test]
fn a_caller_that_is_not_this_bot_is_refused() {
    let door = door(FakeParticipant::default(), vec![]);
    let person = Caller::local_person("ada").in_room(ROOM);
    assert!(matches!(
        block(door.invoke(&person, post())),
        Err(ActionHostError::NotPermitted)
    ));
    let other = Caller::bot("b2", [1; 32], [3; 32]).in_room(ROOM);
    assert!(matches!(
        block(door.invoke(&other, post())),
        Err(ActionHostError::NotPermitted)
    ));
    assert!(matches!(
        block(door.query(&other, CatalogueQuery::default())),
        Err(ActionHostError::NotPermitted)
    ));
}

#[test]
fn a_step_is_recorded_as_the_bot_with_its_run_step_and_cause() {
    let fake = FakeParticipant::default();
    let invoked = fake.invoked.clone();
    let door = door(fake, vec![]);
    door.set_cause(Some(Cause {
        installation: "b1".into(),
        event: "e".into(),
        depth: 2,
    }));
    block(door.invoke(&bot(), post())).unwrap();
    let (by, context, _) = invoked.lock().unwrap().last().cloned().unwrap();
    assert_eq!(by.actor, "bot:b1");
    assert_eq!(by.kind, ActorKind::Bot);
    assert_eq!(by.on_behalf_of.as_deref(), Some("ada"));
    assert_eq!(
        RunStep::of(&context),
        Some(RunStep {
            run: "r1".into(),
            step: "0".into()
        })
    );
    assert_eq!(context.caused_by.unwrap().depth, 2);
}

#[test]
fn a_record_the_bot_does_not_hold_is_unknown() {
    let door = door(FakeParticipant::default(), vec![]);
    assert!(matches!(
        block(door.invoke(&bot(), request("so.tilo.chat", "other", "post"))),
        Err(ActionHostError::UnknownAction)
    ));
}

#[test]
fn a_step_already_in_history_is_answered_not_repeated() {
    let fake = FakeParticipant::default();
    let request = post();
    fake.history.lock().unwrap().push((
        route("so.tilo.chat", "rec-1"),
        step_context().with_invoker(ActionInvoker::actor("bot:b1")),
    ));
    let invoked = fake.invoked.clone();
    let door = door(fake, vec![]);
    assert!(
        door.observe(&bot(), &request.invocation).is_none(),
        "a restarted door has not seen the invocation"
    );
    let receipt = block(door.invoke(&bot(), request)).unwrap();
    assert!(matches!(receipt.status, ReceiptStatus::Completed { .. }));
    assert!(invoked.lock().unwrap().is_empty(), "nothing ran twice");
}

#[test]
fn an_outside_step_answers_the_connectors_result_and_settles_its_entry() {
    let fake = FakeParticipant::default();
    let outbox = fake.outbox.clone();
    let door = door(
        fake,
        vec![Ok(json!({"result": "sent", "message_id": "d1"}))],
    );
    let request = send();
    let id = request.invocation;
    let receipt = block(door.invoke(&bot(), request)).unwrap();
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("{receipt:?}")
    };
    assert_eq!(output.value.unwrap()["message_id"], "d1");
    assert_eq!(
        outbox.lock().unwrap()[0].settled,
        Some(json!({"state": "done", "result": {"result": "sent", "message_id": "d1"}}))
    );
    let observed = door.observe(&bot(), &id).unwrap().receipt();
    assert!(matches!(observed.status, ReceiptStatus::Completed { .. }));
}

#[test]
fn a_refused_op_settles_failed_and_an_unavailable_one_stays_pending() {
    let fake = FakeParticipant::default();
    let outbox = fake.outbox.clone();
    let door = door(
        fake,
        vec![
            Err(PerformError::Unavailable("down".into())),
            Err(PerformError::Refused("Missing Permissions".into())),
        ],
    );
    assert!(matches!(
        block(door.invoke(&bot(), send())),
        Err(ActionHostError::Unavailable { .. })
    ));
    assert!(outbox.lock().unwrap()[0].settled.is_none());
    let refused = block(door.invoke(&bot(), send())).unwrap();
    assert!(matches!(refused.status, ReceiptStatus::Failed { .. }));
    // In the settled form the record reads, whatever its results are.
    let settled = outbox.lock().unwrap()[1].settled.clone().unwrap();
    assert_eq!(
        ui::tilo_resource::Settled::<Vec<String>>::from_json_value(settled).unwrap(),
        ui::tilo_resource::Settled::failed("Missing Permissions")
    );
}

/// A send whose file had not reached the host yet queued its entry before
/// the connector turned it away for now. The retried run finds the step in
/// history and hands the entry over then, answering what Discord answered,
/// so the step after it reads the message it sent.
#[test]
fn a_retried_step_hands_over_the_entry_it_queued_before() {
    let fake = FakeParticipant::default();
    let outbox = fake.outbox.clone();
    let history = fake.history.clone();
    let queued_in_history = fake.queued_in_history.clone();
    let invoked = fake.invoked.clone();
    let door = door(
        fake,
        vec![
            Err(PerformError::Unavailable("the file is not here yet".into())),
            Ok(json!({"result": "sent", "message_id": "d1"})),
        ],
    );
    assert!(matches!(
        block(door.invoke(&bot(), send())),
        Err(ActionHostError::Unavailable { .. })
    ));
    // A send names no item: the entry is found by what its change queued.
    history.lock().unwrap().push((
        route("so.tilo.discord", "rec-2"),
        step_context().with_invoker(ActionInvoker::actor("bot:b1")),
    ));
    queued_in_history.lock().unwrap().push("q1".into());
    let receipt = block(door.invoke(&bot(), send())).unwrap();
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("{receipt:?}")
    };
    assert_eq!(output.value.unwrap()["message_id"], "d1");
    let outbox = outbox.lock().unwrap();
    assert_eq!(outbox.len(), 1, "the send was not repeated");
    assert_eq!(outbox[0].settled.as_ref().unwrap()["state"], "done");
    let actions: Vec<String> = invoked
        .lock()
        .unwrap()
        .iter()
        .map(|(_, _, address)| address.action.clone())
        .collect();
    assert_eq!(actions, ["send", "field-edit"]);
}
