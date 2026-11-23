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

use tilo_actions::remote::{CatalogueRow, RemoteActionTarget};

use super::*;
use crate::agents::programs::host_steps::{Instruction, Judgement, Question};
use crate::agents::programs::kept::Kept;

const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

type Invoked = Arc<Mutex<Vec<(ActionInvoker, ActionContext, ActionAddress)>>>;

/// Records `rec-*` held; a `send` queues an outbox entry and a field edit
/// settles one.
#[derive(Default)]
struct FakeMember {
    invoked: Invoked,
    outbox: Arc<Mutex<Vec<OutboxRow>>>,
    history: Arc<Mutex<Vec<(DocumentRoute, ActionContext)>>>,
    /// The entries the step in history queued, as its change shows.
    queued_in_history: Arc<Mutex<Vec<String>>>,
    /// The records' documents, by id; `secret` may not be read.
    records: std::collections::BTreeMap<String, Vec<u8>>,
}

fn route(app: &str, record: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: app.into(),
        cluster_id: WORKSPACE.into(),
        document: record.into(),
    }
}

impl ApiMember for FakeMember {
    fn workspace(&self) -> &str {
        WORKSPACE
    }

    fn offered(&self) -> Vec<(DocumentRoute, Vec<CatalogueRow>)> {
        Vec::new()
    }

    fn has_record(&self, route: &DocumentRoute) -> bool {
        route.document.starts_with("rec-")
    }

    fn record(&self, route: &DocumentRoute) -> Result<Option<Vec<u8>>, String> {
        if route.document == "secret" {
            return Err("not the program's to read".into());
        }
        Ok(self.records.get(&route.document).cloned())
    }

    fn invoke(
        &mut self,
        _route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
        invoker: &ActionInvoker,
        context: &ActionContext,
        _confirmed: bool,
    ) -> Result<ActionOutput, ApiError> {
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
        step: &AutomationStep,
        actor: &Actor,
    ) -> Option<(DocumentRoute, ActionContext)> {
        self.history
            .lock()
            .unwrap()
            .iter()
            .find(|(_, context)| {
                AutomationStep::of(context).as_ref() == Some(step)
                    && context.invoker.as_ref().map(|invoker| &invoker.acting.by) == Some(actor)
            })
            .cloned()
    }

    fn outbox(&self, _route: &DocumentRoute, _field: &str) -> Vec<OutboxRow> {
        self.outbox.lock().unwrap().clone()
    }

    fn queued_in_history(
        &self,
        _route: &DocumentRoute,
        _step: &AutomationStep,
        _actor: &Actor,
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

fn api(fake: FakeMember, results: Vec<Result<Value, PerformError>>) -> ProgramApi<FakeMember> {
    ProgramApi::new(
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
    step_of("r1")
}

/// What the runner puts on step `0` of run `run`.
fn step_of(run: &str) -> ActionContext {
    ActionContext::default()
        .with_group(run, "Mirror", "shortcut")
        .with_facet(tilo_actions::ActionFacet::new(
            tilo_automation::STEP_FACET,
            "0",
        ))
}

fn request(app: &str, resource: &str, action: &str) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: resource_provider(&route(app, resource)),
            address: ActionAddress::new("r", action),
        },
        argument: InvokeArgument::Json(Map::from_iter([("text".into(), json!("hi"))])),
        context: Some(step_context()),
        confirmation: None,
        appearance: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        signed: None,
    }
}

fn post() -> InvokeRequest {
    request("so.tilo.chat", "rec-1", "post")
}

fn send() -> InvokeRequest {
    request("so.tilo.discord", "rec-2", "send")
}

fn bot() -> Caller {
    Caller::bot("b1", [1; 32], [2; 32]).in_workspace(WORKSPACE)
}

fn block<T>(pending: Pending<T>) -> T {
    let mut pending = std::pin::pin!(pending);
    match pending
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(Some(value)) => value,
        _ => panic!("the API answers at once"),
    }
}

#[test]
fn a_runs_send_goes_out_through_the_connectors_worker() {
    struct Recording(Arc<Mutex<Vec<Value>>>);

    impl Performer for Recording {
        fn perform(&self, queued: Value) -> Result<Value, PerformError> {
            self.0.lock().unwrap().push(queued);
            Ok(json!({"result": "sent", "message_id": "d1"}))
        }
    }

    let fake = FakeMember::default();
    let outbox = fake.outbox.clone();
    let performed = Arc::new(Mutex::new(Vec::new()));
    let connectors = crate::agents::connectors::Connectors::new().running(
        "so.tilo.discord",
        Arc::new(Recording(Arc::clone(&performed))),
    );
    let api = ProgramApi::new(
        Arc::new(Mutex::new(fake)),
        "b1".into(),
        "ada".into(),
        connectors.outboxes(),
    );
    let receipt = block(api.invoke(&bot(), send())).unwrap();
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("{receipt:?}")
    };
    assert_eq!(output.value.unwrap()["message_id"], "d1");
    let performed = performed.lock().unwrap();
    assert_eq!(
        performed.len(),
        1,
        "the send reached the connector's worker"
    );
    assert_eq!(performed[0]["op"]["send"]["text"], "hi");
    assert_eq!(
        outbox.lock().unwrap()[0].settled,
        Some(json!({"state": "done", "result": {"result": "sent", "message_id": "d1"}})),
        "and what the worker answered settled the run's entry"
    );
}

#[test]
fn a_caller_that_is_not_this_bot_is_refused() {
    let api = api(FakeMember::default(), vec![]);
    let person = Caller::local_person("ada").in_workspace(WORKSPACE);
    assert!(matches!(
        block(api.invoke(&person, post())),
        Err(ActionCallError::NotPermitted)
    ));
    let other = Caller::bot("b2", [1; 32], [3; 32]).in_workspace(WORKSPACE);
    assert!(matches!(
        block(api.invoke(&other, post())),
        Err(ActionCallError::NotPermitted)
    ));
    assert!(matches!(
        block(api.query(&other, CatalogueQuery::default())),
        Err(ActionCallError::NotPermitted)
    ));
}

#[test]
fn a_step_is_recorded_as_the_bot_with_its_run_step_and_cause() {
    let fake = FakeMember::default();
    let invoked = fake.invoked.clone();
    let api = api(fake, vec![]);
    api.set_cause(Some(Cause {
        installation: "b1".into(),
        event: "e".into(),
        depth: 2,
    }));
    block(api.invoke(&bot(), post())).unwrap();
    let (by, context, _) = invoked.lock().unwrap().last().cloned().unwrap();
    assert_eq!(by.acting.by, tilo_actions::MemberId::bot("b1").into());
    assert!(by.acting.is_bot());
    assert_eq!(by.acting.acted_for_person(), Some("ada"));
    assert_eq!(
        AutomationStep::of(&context),
        Some(AutomationStep {
            invocation: "r1".into(),
            step: "0".into()
        })
    );
    assert_eq!(context.caused_by.unwrap().depth, 2);
}

#[test]
fn a_record_the_bot_does_not_hold_is_unknown() {
    let api = api(FakeMember::default(), vec![]);
    assert!(matches!(
        block(api.invoke(&bot(), request("so.tilo.chat", "other", "post"))),
        Err(ActionCallError::UnknownAction)
    ));
}

#[test]
fn a_step_already_in_history_is_answered_not_repeated() {
    let fake = FakeMember::default();
    let request = post();
    fake.history.lock().unwrap().push((
        route("so.tilo.chat", "rec-1"),
        step_context().with_invoker(ActionInvoker::bot("b1")),
    ));
    let invoked = fake.invoked.clone();
    let api = api(fake, vec![]);
    assert!(
        api.observe(&bot(), &request.invocation).is_none(),
        "a restarted api has not seen the invocation"
    );
    let receipt = block(api.invoke(&bot(), request)).unwrap();
    assert!(matches!(receipt.status, ReceiptStatus::Completed { .. }));
    assert!(invoked.lock().unwrap().is_empty(), "nothing ran twice");
}

#[test]
fn an_outside_step_answers_the_connectors_result_and_settles_its_entry() {
    let fake = FakeMember::default();
    let outbox = fake.outbox.clone();
    let api = api(
        fake,
        vec![Ok(json!({"result": "sent", "message_id": "d1"}))],
    );
    let request = send();
    let id = request.invocation;
    let receipt = block(api.invoke(&bot(), request)).unwrap();
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("{receipt:?}")
    };
    assert_eq!(output.value.unwrap()["message_id"], "d1");
    assert_eq!(
        outbox.lock().unwrap()[0].settled,
        Some(json!({"state": "done", "result": {"result": "sent", "message_id": "d1"}}))
    );
    let observed = api.observe(&bot(), &id).unwrap().receipt();
    assert!(matches!(observed.status, ReceiptStatus::Completed { .. }));
}

#[test]
fn a_refused_op_settles_failed_and_an_unavailable_one_stays_pending() {
    let fake = FakeMember::default();
    let outbox = fake.outbox.clone();
    let api = api(
        fake,
        vec![
            Err(PerformError::Unavailable("down".into())),
            Err(PerformError::Refused("Missing Permissions".into())),
        ],
    );
    assert!(matches!(
        block(api.invoke(&bot(), send())),
        Err(ActionCallError::Unavailable { .. })
    ));
    assert!(outbox.lock().unwrap()[0].settled.is_none());
    let refused = block(api.invoke(&bot(), send())).unwrap();
    assert!(matches!(refused.status, ReceiptStatus::Failed { .. }));
    // In the settled form the record reads, whatever its results are.
    let settled = outbox.lock().unwrap()[1].settled.clone().unwrap();
    assert_eq!(
        tilo_resource::Settled::<Vec<String>>::from_json_value(settled).unwrap(),
        tilo_resource::Settled::failed("Missing Permissions")
    );
}

/// A send whose file had not reached the host yet queued its entry before
/// the connector turned it away for now. The retried run finds the step in
/// history and hands the entry over then, answering what Discord answered,
/// so the step after it reads the message it sent.
#[test]
fn a_retried_step_hands_over_the_entry_it_queued_before() {
    let fake = FakeMember::default();
    let outbox = fake.outbox.clone();
    let history = fake.history.clone();
    let queued_in_history = fake.queued_in_history.clone();
    let invoked = fake.invoked.clone();
    let api = api(
        fake,
        vec![
            Err(PerformError::Unavailable("the file is not here yet".into())),
            Ok(json!({"result": "sent", "message_id": "d1"})),
        ],
    );
    assert!(matches!(
        block(api.invoke(&bot(), send())),
        Err(ActionCallError::Unavailable { .. })
    ));
    // A send names no item: the entry is found by what its change queued.
    history.lock().unwrap().push((
        route("so.tilo.discord", "rec-2"),
        step_context().with_invoker(ActionInvoker::bot("b1")),
    ));
    queued_in_history.lock().unwrap().push("q1".into());
    let receipt = block(api.invoke(&bot(), send())).unwrap();
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

#[test]
fn a_bot_reads_the_records_it_holds_through_the_record_api() {
    use automerge::transaction::Transactable as _;
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, "title", "Plan").unwrap();
    let fake = FakeMember {
        records: std::collections::BTreeMap::from([("rec-1".to_owned(), doc.save())]),
        ..FakeMember::default()
    };
    let api = api(fake, vec![]);
    let reads = api.resource_reads().unwrap();
    let ask = |record: &str| tilo_actions::remote::ResourceAsk {
        address: ResourceAddress {
            app: "so.tilo.notes".into(),
            record: record.into(),
        },
        joined: false,
    };
    let now = tilo_clock::now();
    let read = tilo_actions::remote::read_resources(
        reads.as_ref(),
        &bot(),
        &[ask("rec-1"), ask("rec-9")],
        None,
        now,
    )
    .unwrap()
    .readable()
    .unwrap();
    assert_eq!(
        read["records"],
        json!([
            {"record": "rec-1", "value": {"title": "Plan"}},
            {"record": "rec-9", "absent": true}
        ])
    );
    let person = Caller::local_person("ada").in_workspace(WORKSPACE);
    assert!(matches!(
        tilo_actions::remote::read_resources(reads.as_ref(), &person, &[ask("rec-1")], None, now),
        Err(ActionCallError::NotPermitted)
    ));
    assert!(matches!(
        tilo_actions::remote::read_resources(reads.as_ref(), &bot(), &[ask("secret")], None, now),
        Err(ActionCallError::Refused { .. })
    ));
}

/// A host whose assistant takes instructions and whose model judges,
/// answering each as scripted, and with nothing once a script is spent.
#[derive(Default)]
struct Assistant {
    taken: Mutex<Vec<Instruction>>,
    answers: Mutex<VecDeque<Result<String, HostStepError>>>,
    judged: Mutex<Vec<Judgement>>,
    judgements: Mutex<VecDeque<Result<Value, HostStepError>>>,
}

impl Assistant {
    fn answering(answers: Vec<Result<String, HostStepError>>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into()),
            ..Self::default()
        })
    }

    fn judging(judgements: Vec<Result<Value, HostStepError>>) -> Arc<Self> {
        Arc::new(Self {
            judgements: Mutex::new(judgements.into()),
            ..Self::default()
        })
    }
}

impl HostSteps for Assistant {
    fn instruct(&self, instruction: Instruction) -> Result<String, HostStepError> {
        self.taken.lock().unwrap().push(instruction);
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(String::new()))
    }

    fn judge(&self, judgement: Judgement) -> Result<Value, HostStepError> {
        self.judged.lock().unwrap().push(judgement);
        self.judgements
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(Value::Null))
    }
}

/// A step that asks the person's model through transform `action`.
fn judge(action: &str, arguments: Value) -> InvokeRequest {
    let Value::Object(arguments) = arguments else {
        panic!("a step's arguments are an object");
    };
    InvokeRequest {
        target: RemoteActionTarget {
            provider: host_steps::PROVIDER.into(),
            address: ActionAddress::new(tilo_automation::AGENT_TRANSFORM_RESOURCE, action),
        },
        argument: InvokeArgument::Json(arguments),
        ..post()
    }
}

/// A step that hands `text` to the assistant.
fn instruct(text: &str) -> InvokeRequest {
    InvokeRequest {
        target: RemoteActionTarget {
            provider: host_steps::PROVIDER.into(),
            address: ActionAddress::new(
                tilo_automation::AGENT_TRANSFORM_RESOURCE,
                tilo_automation::AGENT_TASK_ACTION,
            ),
        },
        argument: InvokeArgument::Json(Map::from_iter([("instruction".into(), json!(text))])),
        ..post()
    }
}

/// The host steps `api` offers the program.
fn offered_host_steps(api: &ProgramApi<FakeMember>) -> Vec<String> {
    let query = CatalogueQuery {
        app_id: Some(tilo_automation::AGENT_APP_ID.into()),
        ..CatalogueQuery::default()
    };
    block(api.query(&bot(), query))
        .unwrap()
        .rows
        .into_iter()
        .filter(|row| row.provider == host_steps::PROVIDER)
        .filter_map(|row| row.action.map(|action| action.address.action))
        .collect()
}

#[test]
fn a_program_that_runs_for_a_person_hands_its_instruction_to_their_assistant() {
    let api = api(FakeMember::default(), vec![]);
    let assistant = Assistant::answering(vec![Ok("At 11:32.".into())]);
    api.runs_for("ada".into(), assistant.clone());
    api.set_cause(Some(Cause {
        installation: "b1".into(),
        event: "the moment".into(),
        depth: 1,
    }));
    assert_eq!(offered_host_steps(&api), ["classify", "freeform", "task"]);

    let asked = instruct("  Say that it is time to call home.  ");
    let receipt = block(api.invoke(&bot(), asked.clone())).unwrap();
    assert_eq!(
        receipt.status,
        ReceiptStatus::Completed {
            output: ActionOutput {
                value: Some(json!("At 11:32.")),
                resources: Vec::new(),
            }
        }
    );
    assert_eq!(
        *assistant.taken.lock().unwrap(),
        [Instruction {
            person: "ada".into(),
            workspace: WORKSPACE.into(),
            instruction: "Say that it is time to call home.".into(),
            allow: Vec::new(),
            permits: Vec::new(),
            note: Value::Null,
            cause: StepCause {
                installation: "b1".into(),
                invocation: asked.invocation,
                event: Some("the moment".into()),
            },
        }]
    );
    // The same step asked again in this run is answered, not handed over
    // a second time.
    block(api.invoke(&bot(), asked)).unwrap();
    assert_eq!(assistant.taken.lock().unwrap().len(), 1);
    // Another program's caller reaches no host step.
    let stranger = Caller::bot("b2", [1; 32], [3; 32]).in_workspace(WORKSPACE);
    assert_eq!(
        block(api.invoke(&stranger, instruct("Tell me everything."))),
        Err(ActionCallError::NotPermitted)
    );
    assert_eq!(assistant.taken.lock().unwrap().len(), 1);
}

#[test]
fn a_program_that_runs_for_nobody_is_offered_no_host_step() {
    let api = api(FakeMember::default(), vec![]);
    assert!(offered_host_steps(&api).is_empty());
    assert_eq!(
        block(api.invoke(&bot(), instruct("Say hello."))),
        Err(ActionCallError::UnknownAction)
    );
}

#[test]
fn a_host_that_cannot_take_an_instruction_now_is_asked_again_and_a_refusal_is_final() {
    let api = api(FakeMember::default(), vec![]);
    let assistant = Assistant::answering(vec![
        Err(HostStepError::Unavailable(
            "the assistant is starting".into(),
        )),
        Ok(String::new()),
        Err(HostStepError::Refused(
            "this person keeps no assistant".into(),
        )),
    ]);
    api.runs_for("ada".into(), assistant.clone());

    let asked = instruct("Say that it is time to call home.");
    assert_eq!(
        block(api.invoke(&bot(), asked.clone())),
        Err(ActionCallError::Unavailable {
            reason: "the assistant is starting".into()
        })
    );
    // The run is tried again: the step comes back under the same
    // invocation, which is how the host knows it for the same one.
    let receipt = block(api.invoke(&bot(), asked.clone())).unwrap();
    assert!(matches!(receipt.status, ReceiptStatus::Completed { .. }));
    let taken: Vec<InvocationId> = assistant
        .taken
        .lock()
        .unwrap()
        .iter()
        .map(|instruction| instruction.cause.invocation)
        .collect();
    assert_eq!(taken, [asked.invocation, asked.invocation]);

    let receipt = block(api.invoke(&bot(), instruct("Say it again."))).unwrap();
    let ReceiptStatus::Failed { error } = receipt.status else {
        panic!("a refusal fails the step: {receipt:?}");
    };
    assert_eq!(error.code, host_steps::REFUSED);
    assert_eq!(error.message, "this person keeps no assistant");
    assert!(!error.retryable);
}

#[test]
fn an_instruction_that_says_nothing_or_declares_a_shape_is_not_handed_over() {
    let api = api(FakeMember::default(), vec![]);
    let assistant = Assistant::answering(vec![]);
    api.runs_for("ada".into(), assistant.clone());
    assert!(matches!(
        block(api.invoke(&bot(), instruct("   "))),
        Err(ActionCallError::InvalidRequest { .. })
    ));
    let mut shaped = instruct("Count the open questions.");
    if let InvokeArgument::Json(arguments) = &mut shaped.argument {
        arguments.insert("schema".into(), json!({"type": "integer"}));
    }
    assert!(matches!(
        block(api.invoke(&bot(), shaped)),
        Err(ActionCallError::InvalidRequest { .. })
    ));
    assert!(assistant.taken.lock().unwrap().is_empty());
}

#[test]
fn a_judgement_is_one_call_of_the_persons_model_held_to_what_was_asked() {
    let api = api(FakeMember::default(), vec![]);
    let assistant = Assistant::judging(vec![
        Ok(json!("yes")),
        Ok(json!("perhaps")),
        Ok(json!(true)),
    ]);
    api.runs_for("ada".into(), assistant.clone());
    let completed = |value: Value| ReceiptStatus::Completed {
        output: ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        },
    };

    let labelled = judge(
        "classify",
        json!({"input": "A note from the bank.", "choices": ["yes", "no"]}),
    );
    let receipt = block(api.invoke(&bot(), labelled.clone())).unwrap();
    assert_eq!(receipt.status, completed(json!("yes")));
    assert_eq!(
        assistant.judged.lock().unwrap()[0],
        Judgement {
            person: "ada".into(),
            workspace: WORKSPACE.into(),
            question: Question::Labels {
                choices: vec!["yes".into(), "no".into()],
                multi: false,
            },
            input: Some("A note from the bank.".into()),
            note: Value::Null,
            cause: StepCause {
                installation: "b1".into(),
                invocation: labelled.invocation,
                event: None,
            },
        }
    );

    // An answer that is none of the labels is no answer.
    let receipt = block(api.invoke(
        &bot(),
        judge("classify", json!({"input": "x", "choices": ["yes", "no"]})),
    ))
    .unwrap();
    assert!(
        matches!(&receipt.status, ReceiptStatus::Failed { error } if error.code == host_steps::REFUSED),
        "{receipt:?}"
    );

    // A prompt is answered in the shape the step declared.
    let shaped = judge(
        "freeform",
        json!({"prompt": "Does it ask for money?", "input": "x", "schema": {"type": "boolean"}}),
    );
    let receipt = block(api.invoke(&bot(), shaped)).unwrap();
    assert_eq!(receipt.status, completed(json!(true)));
    assert_eq!(
        assistant.judged.lock().unwrap()[2].question,
        Question::Prompt {
            prompt: "Does it ask for money?".into(),
            schema: Some(json!({"type": "boolean"})),
        }
    );

    // A host judges the text it is given: it opens no content by address,
    // and a classification with no labels asks nothing.
    for unasked in [
        judge(
            "classify",
            json!({"resource": "tilo.content:abc", "choices": ["yes"]}),
        ),
        judge("classify", json!({"input": "x", "choices": []})),
        judge("freeform", json!({"input": "x"})),
    ] {
        assert!(matches!(
            block(api.invoke(&bot(), unasked)),
            Err(ActionCallError::InvalidRequest { .. })
        ));
    }
    assert_eq!(assistant.judged.lock().unwrap().len(), 3);
}

/// A step on what the program itself keeps.
fn own(action: &str, arguments: Value) -> InvokeRequest {
    let Value::Object(arguments) = arguments else {
        panic!("a step's arguments are an object");
    };
    InvokeRequest {
        target: RemoteActionTarget {
            provider: host_steps::PROVIDER.into(),
            address: ActionAddress::new(tilo_automation::shortcut_resource_id("b1"), action),
        },
        argument: InvokeArgument::Json(arguments),
        ..post()
    }
}

#[test]
fn a_program_counts_and_keeps_its_note_which_goes_with_what_it_asks_of_the_host() {
    let api = api(FakeMember::default(), vec![]);
    let kept = Arc::new(Mutex::new(Kept::default()));
    api.keeps(kept.clone());
    let value = |receipt: InvocationReceipt| match receipt.status {
        ReceiptStatus::Completed { output } => output.value,
        other => panic!("the step completes: {other:?}"),
    };
    // Its own resource offers both, also to a program that runs for nobody.
    let query = CatalogueQuery {
        app_id: Some(tilo_automation::SHORTCUTS_APP_ID.into()),
        ..CatalogueQuery::default()
    };
    let offered: Vec<String> = block(api.query(&bot(), query))
        .unwrap()
        .rows
        .into_iter()
        .filter(|row| row.resource.kind == tilo_automation::SHORTCUT_RESOURCE_KIND)
        .filter_map(|row| row.action.map(|action| action.address.action))
        .collect();
    assert_eq!(offered, ["count", "keep"]);

    let first = own("count", json!({"every": 2}));
    assert_eq!(
        value(block(api.invoke(&bot(), first)).unwrap()),
        Some(json!({"count": 1, "reached": false}))
    );
    // The run is tried again after a restart: the same step of the same
    // run, under an API that never took it, advances nothing.
    assert_eq!(
        value(block(api.invoke(&bot(), own("count", json!({"every": 2})))).unwrap()),
        Some(json!({"count": 1, "reached": false}))
    );
    let mut next = own("count", json!({"every": 2}));
    next.context = Some(step_of("r2"));
    assert_eq!(
        value(block(api.invoke(&bot(), next)).unwrap()),
        Some(json!({"count": 2, "reached": true}))
    );
    assert!(matches!(
        block(api.invoke(&bot(), own("count", json!({"every": 0})))),
        Err(ActionCallError::InvalidRequest { .. })
    ));

    let keeping = own(
        "keep",
        json!({"style": "Short. First names.", "kept": {"seen": "m-41"}}),
    );
    assert_eq!(value(block(api.invoke(&bot(), keeping)).unwrap()), None);
    assert_eq!(kept.lock().unwrap().note().style, "Short. First names.");

    // The note goes with an instruction, as it stands then.
    let assistant = Assistant::answering(vec![]);
    api.runs_for("ada".into(), assistant.clone());
    block(api.invoke(&bot(), instruct("Answer it."))).unwrap();
    let taken = assistant.taken.lock().unwrap();
    assert_eq!(taken[0].note["style"], "Short. First names.");
    assert_eq!(taken[0].note["kept"]["seen"], "m-41");
    assert_eq!(taken[0].note["accepted"], json!([]));
}

#[test]
fn an_instruction_lets_through_only_the_outside_action_its_program_was_permitted() {
    use tilo_actions::policy::{Answer, KindSet};
    let api = api(FakeMember::default(), vec![]);
    let kept = Arc::new(Mutex::new(Kept::default()));
    api.keeps(kept.clone());
    let assistant = Assistant::answering(vec![]);
    api.runs_for("ada".into(), assistant.clone());
    let node = |action: &str| {
        tilo_automation::step_node(
            Some(WORKSPACE),
            "so.tilo.mail",
            "mail.mailbox",
            &ActionAddress::new("mail.mailbox:inbox", action),
        )
    };

    block(api.invoke(&bot(), instruct("Answer the inquiry."))).unwrap();
    assert!(
        !assistant.taken.lock().unwrap()[0].lets_through(&node("send")),
        "setting a program up gives it no reach: sending asks"
    );

    let send = tilo_automation::action_rule(
        "so.tilo.mail",
        "mail.mailbox",
        None,
        "send",
        KindSet::OUTSIDE,
        Answer::Allow,
    );
    kept.lock().unwrap().permit(send).unwrap();
    block(api.invoke(&bot(), instruct("Answer the next inquiry."))).unwrap();
    let taken = assistant.taken.lock().unwrap();
    assert!(taken[1].lets_through(&node("send")));
    assert!(
        !taken[1].lets_through(&node("delete")),
        "nothing wider than the one action"
    );
}
