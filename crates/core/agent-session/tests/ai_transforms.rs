//! Shortcuts running `Ai` steps against the transforms an agent publishes.
//!
//! The service below offers exactly [`transform::descriptors`] and answers an
//! invocation the way the agent does — binding the wire argument and running
//! the transform — with a scripted model in place of the provider. So the
//! checker reads the real published parameters and result specs, and the
//! interpreter holds the real answers to them: a step that declares a shape
//! gets that shape or a typed failure, a step's answer feeds the next step,
//! and a secret never reaches the model.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap or a panic in a test is its assertion"
)]

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use agent_provider_config::Interaction;
use serde_json::{Map, Value, json};
use tilo_actions::outcome::{InvocationId, InvocationUpdate};
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, CatalogueResource,
    CatalogueRow, InvocationLog, InvocationReceipt, InvocationState, InvokeRequest, Pending,
};
use tilo_agent_session::transform::{self, TransformModel};
use tilo_automation::{
    AGENT_APP_ID, AGENT_TASK_ACTION, ActionCatalog, AskRequest, AutomationInvocationId, Binding,
    CheckErrorKind, InvocationHooks, InvocationMode, InvocationOutcome, InvocationReport,
    InvocationRequest, InvocationTrigger, NoShortcuts, Shortcut, Step, StepFailure, StepKind,
    check, run,
};

/// Answers from a script, counting what each attempt was asked.
struct Scripted {
    answers: Mutex<VecDeque<String>>,
    asked: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(answers: &[&str]) -> Self {
        Self {
            answers: Mutex::new(answers.iter().map(|answer| (*answer).to_string()).collect()),
            asked: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl TransformModel for Scripted {
    fn answer(
        &self,
        _kind: Interaction,
        _instructions: &str,
        input: &str,
        _max_output_tokens: u32,
        _timeout: Duration,
    ) -> Result<String, String> {
        self.asked.lock().unwrap().push(input.to_string());
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "the script ran out".to_string())
    }
}

/// A host offering exactly the agent's published transforms, answering them as the agent does.
struct AgentService {
    rows: Vec<CatalogueRow>,
    model: Scripted,
    content: ui::content::ContentStore,
    log: Mutex<InvocationLog<InvocationUpdate>>,
}

impl AgentService {
    fn new(answers: &[&str]) -> Self {
        let rows = transform::descriptors()
            .into_iter()
            .map(|descriptor| CatalogueRow {
                offered: None,
                provider: format!("{AGENT_APP_ID}#1"),
                app_id: AGENT_APP_ID.into(),
                parent_resource: String::new(),
                resource: CatalogueResource {
                    id: descriptor.address.resource.clone(),
                    kind: descriptor.resource_kind.clone(),
                    label: descriptor.resource_label.clone(),
                    presentation: None,
                },
                action: Some(descriptor),
                has_children: false,
                visibility: None,
                surface: None,
            })
            .collect();
        Self {
            rows,
            model: Scripted::new(answers),
            content: ui::content::ContentStore::memory(),
            log: Mutex::new(InvocationLog::default()),
        }
    }

    fn catalog(&self) -> ActionCatalog {
        ActionCatalog::new(self.rows.clone())
    }

    fn answer(&self, request: &InvokeRequest) -> InvocationUpdate {
        let invocation = request.invocation;
        let action = request.target.address.action.as_str();
        let slot = request.argument.to_slot();
        match transform::bind_wire(action, slot.as_deref(), &self.content)
            .and_then(|call| transform::run(&call, &self.model))
        {
            Ok(output) => InvocationUpdate::Succeeded { invocation, output },
            Err(error) => InvocationUpdate::Failed {
                invocation,
                error: *error,
            },
        }
    }
}

impl ActionProvider for AgentService {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::from_rows(
            "1".into(),
            self.rows.clone(),
            &query,
        )))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let offered = self.rows.iter().any(|row| {
            row.provider == request.target.provider
                && row.action.as_ref().map(|a| &a.address) == Some(&request.target.address)
        });
        if !offered {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let receipt = self
            .log
            .lock()
            .unwrap()
            .admit(caller, &request, || Ok(self.answer(&request)))
            .map(|(update, _)| InvocationState::new(request.invocation, update.clone()).receipt());
        Pending::ready(receipt)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.log
            .lock()
            .unwrap()
            .get(caller, id)
            .map(|update| InvocationState::new(*id, update.clone()))
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionCallError>> {
        tilo_actions::remote::Pending::ready(Err(ActionCallError::NotCancellable {
            reason: "finished".into(),
        }))
    }
}

/// Nobody is present: no step is confirmed and nothing is asked.
struct Unattended;

impl InvocationHooks for Unattended {
    fn ask(&self, _request: AskRequest) -> Pending<Option<Value>> {
        Pending::ready(None)
    }

    fn settle(&self, _invocation: InvocationId) -> Pending<()> {
        Pending::ready(())
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("the service answers at once"),
    }
}

fn grant() -> Vec<tilo_action_policy::Rule> {
    vec![tilo_action_policy::Rule::new(
        tilo_action_policy::TreePath::root()
            .any(tilo_action_policy::Level::Workspace)
            .app(AGENT_APP_ID),
        tilo_action_policy::KindSet::ALL,
        tilo_action_policy::Answer::Allow,
    )]
}

fn ai_step(id: &str, prompt: &str, output: Option<Value>, input: Option<Binding>) -> Step {
    Step::new(
        id,
        StepKind::Ai {
            prompt: prompt.into(),
            output,
            input,
        },
    )
}

fn ai_shortcut(prompt: &str, output: Option<Value>, input: Option<Binding>) -> Shortcut {
    Shortcut {
        id: "digest".into(),
        name: "Digest".into(),
        inputs: Vec::new(),
        steps: vec![ai_step("ai", prompt, output, input)],
        step_timeout_secs: None,
        output: Some(Binding::step("ai", "")),
        grant: grant(),
        triggers: Vec::new(),
        saved_by: None,
        saved_proof: None,
        paused: false,
    }
}

fn run_shortcut(
    service: &AgentService,
    shortcut: &Shortcut,
    inputs: Map<String, Value>,
) -> InvocationReport {
    block_on(run(
        service,
        &Unattended,
        InvocationRequest {
            shortcut,
            invocation: AutomationInvocationId::new(),
            caller: Caller::local_person("ada"),
            mode: InvocationMode::Unattended,
            trigger: InvocationTrigger::Manual,
            inputs,
            caused_by: None,
            resume: None,
            from: None,
            parent: None,
            chain: Vec::new(),
            approval: None,
        },
    ))
}

fn failure(report: &InvocationReport) -> Option<&StepFailure> {
    match &report.outcome {
        InvocationOutcome::Completed | InvocationOutcome::Cancelled { .. } => None,
        InvocationOutcome::Failed { failure, .. } => Some(failure),
    }
}

/// A shortcut's `Ai` step succeeds against the descriptor the agent publishes: the checker reads
/// the real parameters, the step runs, and the model's answer is the step's output.
#[test]
fn a_summary_step_succeeds_against_the_published_descriptor() {
    let service = AgentService::new(&[r#"{"result": "Two notes, both about the launch."}"#]);
    let digest = ai_shortcut(
        "Summarise this briefly",
        None,
        Some(Binding::Literal(json!(
            "A long note about the launch. Another about the launch."
        ))),
    );
    assert_eq!(check(&digest, &service.catalog(), &NoShortcuts), Ok(()));

    let report = run_shortcut(&service, &digest, Map::new());
    assert_eq!(report.outcome, InvocationOutcome::Completed, "{report:?}");
    assert_eq!(
        report.output,
        Some(json!("Two notes, both about the launch."))
    );
    assert_eq!(service.model.asked().len(), 1);
}

/// A step's answer is what the next step is bound to: a shortcut pipes one `Ai` step into the
/// next, and the second is asked about the first's output rather than its own input.
#[test]
fn one_ai_steps_answer_feeds_the_next() {
    let service = AgentService::new(&[
        r#"{"result": "What colour is the sky?"}"#,
        r#"{"result": "Blue."}"#,
    ]);
    let mut pipeline = ai_shortcut(
        "Turn this into a question",
        None,
        Some(Binding::Literal(json!("the sky is blue"))),
    );
    pipeline.steps = vec![
        ai_step(
            "ask",
            "Turn this into a question",
            None,
            Some(Binding::Literal(json!("the sky is blue"))),
        ),
        ai_step(
            "answer",
            "Answer the question",
            None,
            Some(Binding::step("ask", "")),
        ),
    ];
    pipeline.output = Some(Binding::step("answer", ""));
    assert_eq!(check(&pipeline, &service.catalog(), &NoShortcuts), Ok(()));

    let report = run_shortcut(&service, &pipeline, Map::new());
    assert_eq!(report.outcome, InvocationOutcome::Completed, "{report:?}");
    assert_eq!(report.output, Some(json!("Blue.")));
    let asked = service.model.asked();
    assert_eq!(asked.len(), 2);
    assert!(
        asked[1].contains("What colour is the sky?"),
        "the second step is asked about the first's answer: {asked:?}"
    );
}

/// An extraction is repaired once when its first answer misses the shape the step declared, and
/// the repair tells the model why: the shortcut's own schema, not the agent's, holds the answer.
#[test]
fn an_extraction_is_repaired_once_when_it_misses_the_schema() {
    let schema = json!({
        "type": "object",
        "properties": {"due": {"type": "string"}, "owner": {"type": "string"}},
        "required": ["due", "owner"],
        "additionalProperties": false
    });
    let service = AgentService::new(&[
        r#"{"result": {"due": "Friday"}}"#,
        r#"{"result": {"due": "Friday", "owner": "Ada"}}"#,
    ]);
    let extract = ai_shortcut(
        "Pull out the due date and owner",
        Some(schema),
        Some(Binding::Literal(json!("Ada ships it by Friday."))),
    );
    assert_eq!(check(&extract, &service.catalog(), &NoShortcuts), Ok(()));

    let report = run_shortcut(&service, &extract, Map::new());
    assert_eq!(report.outcome, InvocationOutcome::Completed, "{report:?}");
    assert_eq!(
        report.output,
        Some(json!({"due": "Friday", "owner": "Ada"}))
    );
    let asked = service.model.asked();
    assert_eq!(asked.len(), 2);
    assert!(asked[1].contains("owner"), "the repair names the reason");
}

/// An answer that misses the shape twice fails the step as a typed agent failure, never as a
/// best-effort value.
#[test]
fn an_answer_that_misses_the_schema_twice_fails_the_step() {
    let service = AgentService::new(&[r#"{"result": {"due": 5}}"#, "Sure! The owner is Ada."]);
    let extract = ai_shortcut(
        "Pull out the due date",
        Some(json!({
            "type": "object",
            "properties": {"due": {"type": "string"}},
            "required": ["due"]
        })),
        Some(Binding::Literal(json!("Ada ships it by Friday."))),
    );
    let report = run_shortcut(&service, &extract, Map::new());
    match failure(&report) {
        Some(StepFailure::Action { error }) => {
            assert_eq!(error.code, transform::code::INVALID_OUTPUT);
            assert!(!error.retryable);
        }
        other => panic!("expected a typed agent failure, got {other:?}"),
    }
    assert_eq!(service.model.asked().len(), 2, "one attempt and one repair");
}

/// A classification is one of the labels the step names: a label outside them is refused and
/// asked again.
#[test]
fn a_classification_is_one_of_the_labels() {
    let service = AgentService::new(&[r#"{"result": "maybe"}"#, r#"{"result": "urgent"}"#]);
    let triage = ai_shortcut(
        "Label this urgent or later",
        Some(json!({"type": "string", "enum": ["urgent", "later"]})),
        Some(Binding::Literal(json!("The server is down."))),
    );
    assert_eq!(check(&triage, &service.catalog(), &NoShortcuts), Ok(()));
    let report = run_shortcut(&service, &triage, Map::new());
    assert_eq!(report.outcome, InvocationOutcome::Completed, "{report:?}");
    assert_eq!(report.output, Some(json!("urgent")));
}

/// A secret is never part of what an `Ai` step sends: the checker refuses it, and a shortcut that
/// was saved anyway fails the step at binding without asking the model at all.
#[test]
fn a_secret_never_reaches_the_agent() {
    let service = AgentService::new(&[r#"{"result": "leaked"}"#]);
    let mut leak = ai_shortcut("Summarise this", None, Some(Binding::Input("token".into())));
    leak.inputs = vec![ActionParam::required(
        "token",
        ParamType::Secret,
        "An API token",
    )];
    let errors = check(&leak, &service.catalog(), &NoShortcuts).unwrap_err();
    assert!(
        errors.iter().any(|error| matches!(
            &error.kind,
            CheckErrorKind::Incompatible(reason) if reason.contains("secret")
        )),
        "{errors:?}"
    );

    let mut inputs = Map::new();
    inputs.insert("token".into(), json!("s3cret"));
    let report = run_shortcut(&service, &leak, inputs);
    assert!(
        matches!(failure(&report), Some(StepFailure::Binding { reason }) if reason.contains("secret")),
        "{report:?}"
    );
    assert!(service.model.asked().is_empty());
}

/// A task step runs against the published task descriptor, is asked once — it reads no caller
/// input — and answers what it could not do as its summary rather than a failure.
#[test]
fn a_task_step_runs_against_the_published_task_descriptor() {
    let service = AgentService::new(&[r#"{"result": "I cannot reach the mailbox."}"#]);
    let mut tidy = ai_shortcut("Tidy the inbox", None, None);
    tidy.steps = vec![Step::new(
        "task",
        StepKind::Task {
            instruction: "Tidy the inbox".into(),
            allow: Vec::new(),
            output: None,
        },
    )];
    tidy.output = Some(Binding::step("task", ""));
    assert_eq!(check(&tidy, &service.catalog(), &NoShortcuts), Ok(()));

    let report = run_shortcut(&service, &tidy, Map::new());
    assert_eq!(report.outcome, InvocationOutcome::Completed, "{report:?}");
    assert_eq!(report.output, Some(json!("I cannot reach the mailbox.")));
    let asked = service.model.asked();
    assert_eq!(
        asked.len(),
        1,
        "a task reads no caller input, so it is asked once"
    );
    assert!(
        !asked[0].contains("Input:"),
        "a task is handed no input: {asked:?}"
    );
    let published = transform::descriptors()
        .into_iter()
        .find(|descriptor| descriptor.address.action == AGENT_TASK_ACTION)
        .expect("the agent publishes the task action");
    assert!(
        published
            .params
            .iter()
            .any(|param| param.name == "instruction")
    );
}
