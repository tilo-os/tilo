//! Shortcuts running `Ai` steps against the transforms the agent publishes.
//!
//! The host below offers exactly [`transform::descriptors`] and answers an
//! invocation the way the agent does — binding the wire argument and running
//! the transform — with a scripted model standing in for the gateway. So the
//! checker reads the real published parameters and result specs, and the
//! interpreter holds the real answers to them.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_actions::outcome::{ActionOutput, InvocationId, InvocationUpdate};
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, CatalogueResource,
    CatalogueRow, Confirmation, InvocationLedger, InvocationReceipt, InvocationState,
    InvokeRequest, Pending,
};
use tilo_agent::transform::{self, TransformModel};
use tilo_automation::{
    AGENT_APP_ID, ActionCatalog, AiTransform, AskRequest, Binding, CheckErrorKind,
    ConfirmationRequest, NoShortcuts, RunHooks, RunId, RunMode, RunOutcome, RunReport, RunRequest,
    RunTrigger, Shortcut, Step, StepFailure, StepKind, check, run,
};

/// Answers from a script, counting how often it was asked.
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

/// How the host answers: as the agent does, or passing the model's raw
/// value through unchecked, as a misbehaving publisher would.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Answering {
    AsTheAgent,
    Unchecked,
}

struct AgentHost {
    rows: Vec<CatalogueRow>,
    model: Scripted,
    answering: Answering,
    content: ui::content::ContentStore,
    ledger: Mutex<InvocationLedger<InvocationUpdate>>,
}

impl AgentHost {
    fn new(answers: &[&str]) -> Self {
        let rows = transform::descriptors()
            .into_iter()
            .map(|descriptor| CatalogueRow {
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
            })
            .collect();
        Self {
            rows,
            model: Scripted::new(answers),
            answering: Answering::AsTheAgent,
            content: ui::content::ContentStore::memory(),
            ledger: Mutex::new(InvocationLedger::default()),
        }
    }

    fn unchecked(answers: &[&str]) -> Self {
        Self {
            answering: Answering::Unchecked,
            ..Self::new(answers)
        }
    }

    fn catalog(&self) -> ActionCatalog {
        ActionCatalog::new(self.rows.clone())
    }

    fn answer(&self, request: &InvokeRequest) -> InvocationUpdate {
        let invocation = request.invocation;
        let action = request.target.address.action.as_str();
        let slot = request.argument.to_slot();
        if self.answering == Answering::Unchecked {
            let raw = self
                .model
                .answer("", "", 1, Duration::from_secs(1))
                .unwrap();
            let value: Value = serde_json::from_str(&raw).unwrap();
            return InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: Some(value["result"].clone()),
                    resources: Vec::new(),
                },
            };
        }
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

impl ActionHost for AgentHost {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
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
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let offered = self.rows.iter().any(|row| {
            row.provider == request.target.provider
                && row.action.as_ref().map(|a| &a.address) == Some(&request.target.address)
        });
        if !offered {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        }
        let receipt = self
            .ledger
            .lock()
            .unwrap()
            .admit(caller, &request, || Ok(self.answer(&request)))
            .map(|(update, _)| InvocationState::new(request.invocation, update.clone()).receipt());
        Pending::ready(receipt)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.ledger
            .lock()
            .unwrap()
            .get(caller, id)
            .map(|update| InvocationState::new(*id, update.clone()))
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionHostError>> {
        tilo_actions::remote::Pending::ready(Err(ActionHostError::NotCancellable {
            reason: "finished".into(),
        }))
    }
}

struct Unattended;

impl RunHooks for Unattended {
    fn confirm(&self, _request: ConfirmationRequest) -> Pending<Option<Confirmation>> {
        Pending::ready(None)
    }

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
        Poll::Pending => panic!("the host answers at once"),
    }
}

fn ai_shortcut(transform: AiTransform, input: Binding) -> Shortcut {
    Shortcut {
        id: "digest".into(),
        name: "Digest".into(),
        inputs: Vec::new(),
        steps: vec![Step::new("ai", StepKind::Ai { transform, input })],
        output: Some(Binding::step("ai", "")),
        grant: vec![tilo_action_policy::Rule::new(
            tilo_action_policy::TreePath::root()
                .any(tilo_action_policy::Level::Room)
                .app(AGENT_APP_ID),
            tilo_action_policy::KindSet::READ_UNDOABLE,
            tilo_action_policy::Answer::Allow,
        )],
        triggers: Vec::new(),
        saved_by: None,
    }
}

fn run_shortcut(host: &AgentHost, shortcut: &Shortcut, inputs: Map<String, Value>) -> RunReport {
    block_on(run(
        host,
        &Unattended,
        RunRequest {
            shortcut,
            run: RunId::new(),
            caller: Caller::local_person("ada"),
            mode: RunMode::Unattended,
            trigger: RunTrigger::Manual,
            inputs,
            caused_by: None,
        },
    ))
}

fn failure(report: &RunReport) -> Option<&StepFailure> {
    match &report.outcome {
        RunOutcome::Completed => None,
        RunOutcome::Failed { failure, .. } => Some(failure),
    }
}

#[test]
fn a_summary_step_succeeds_against_the_published_descriptor() {
    let host = AgentHost::new(&[r#"{"result": "Two notes, both about the launch."}"#]);
    let digest = ai_shortcut(
        AiTransform::Summarise,
        Binding::Literal(json!(
            "A long note about the launch. Another about the launch."
        )),
    );
    assert_eq!(check(&digest, &host.catalog(), &NoShortcuts), Ok(()));

    let report = run_shortcut(&host, &digest, Map::new());
    assert_eq!(report.outcome, RunOutcome::Completed, "{report:?}");
    assert_eq!(
        report.output,
        Some(json!("Two notes, both about the launch."))
    );
    assert_eq!(host.model.asked().len(), 1);
}

#[test]
fn an_extraction_is_repaired_once_when_it_misses_the_schema() {
    let schema = json!({
        "type": "object",
        "properties": {"due": {"type": "string"}, "owner": {"type": "string"}},
        "required": ["due", "owner"],
        "additionalProperties": false
    });
    let host = AgentHost::new(&[
        r#"{"result": {"due": "Friday"}}"#,
        r#"{"result": {"due": "Friday", "owner": "Ada"}}"#,
    ]);
    let extract = ai_shortcut(
        AiTransform::Extract { schema },
        Binding::Literal(json!("Ada ships it by Friday.")),
    );
    assert_eq!(check(&extract, &host.catalog(), &NoShortcuts), Ok(()));

    let report = run_shortcut(&host, &extract, Map::new());
    assert_eq!(report.outcome, RunOutcome::Completed, "{report:?}");
    assert_eq!(
        report.output,
        Some(json!({"due": "Friday", "owner": "Ada"}))
    );
    let asked = host.model.asked();
    assert_eq!(asked.len(), 2);
    assert!(asked[1].contains("owner"), "the repair names the reason");
}

#[test]
fn an_answer_that_misses_the_schema_twice_fails_the_step() {
    let host = AgentHost::new(&[r#"{"result": {"due": 5}}"#, "Sure! The owner is Ada."]);
    let extract = ai_shortcut(
        AiTransform::Extract {
            schema: json!({
                "type": "object",
                "properties": {"due": {"type": "string"}},
                "required": ["due"]
            }),
        },
        Binding::Literal(json!("Ada ships it by Friday.")),
    );
    let report = run_shortcut(&host, &extract, Map::new());
    match failure(&report) {
        Some(StepFailure::Action { error }) => {
            assert_eq!(error.code, transform::code::INVALID_OUTPUT);
            assert!(!error.retryable);
        }
        other => panic!("expected a typed agent failure, got {other:?}"),
    }
    assert_eq!(host.model.asked().len(), 2, "one attempt and one repair");
}

#[test]
fn the_published_result_spec_fails_a_step_whose_answer_skipped_the_check() {
    // A publisher returning the model's value without checking it is caught
    // by the result spec the agent declares.
    let host = AgentHost::unchecked(&[r#"{"result": 42}"#]);
    let digest = ai_shortcut(AiTransform::Summarise, Binding::Literal(json!("text")));
    let report = run_shortcut(&host, &digest, Map::new());
    assert!(
        matches!(failure(&report), Some(StepFailure::Contract { .. })),
        "{report:?}"
    );
}

#[test]
fn a_classification_is_one_of_the_labels() {
    let host = AgentHost::new(&[r#"{"result": "maybe"}"#, r#"{"result": "urgent"}"#]);
    let triage = ai_shortcut(
        AiTransform::Classify {
            choices: vec!["urgent".into(), "later".into()],
        },
        Binding::Literal(json!("The server is down.")),
    );
    assert_eq!(check(&triage, &host.catalog(), &NoShortcuts), Ok(()));
    let report = run_shortcut(&host, &triage, Map::new());
    assert_eq!(report.outcome, RunOutcome::Completed, "{report:?}");
    assert_eq!(report.output, Some(json!("urgent")));
}

#[test]
fn a_secret_never_reaches_the_agent() {
    let host = AgentHost::new(&[r#"{"result": "leaked"}"#]);
    let mut leak = ai_shortcut(AiTransform::Summarise, Binding::Input("token".into()));
    leak.inputs = vec![ActionParam::required(
        "token",
        ParamType::Secret,
        "An API token",
    )];
    let errors = check(&leak, &host.catalog(), &NoShortcuts).unwrap_err();
    assert!(
        errors.iter().any(|error| matches!(
            &error.kind,
            CheckErrorKind::Incompatible(reason) if reason.contains("secret")
        )),
        "{errors:?}"
    );

    let mut inputs = Map::new();
    inputs.insert("token".into(), json!("s3cret"));
    let report = run_shortcut(&host, &leak, inputs);
    assert!(
        matches!(failure(&report), Some(StepFailure::Binding { reason }) if reason.contains("secret")),
        "{report:?}"
    );
    assert!(host.model.asked().is_empty());
}
