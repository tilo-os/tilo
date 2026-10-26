use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};

use serde_json::{Value, json};
use tilo_action_policy::{Answer, KindSet, Level, Rule, TreePath};
use tilo_actions::ActionKey;
use tilo_actions::action::{ExecutionLocus, OperationKind};
use tilo_actions::contract::{ActionContract, ConfirmationPolicy, ResultSpec, RiskLevel};
use tilo_actions::door::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::outcome::{ActionOutput, InvocationId, InvocationUpdate};
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, CatalogueResource,
    CatalogueRow, Completer, Confirmation, InvocationLedger, InvocationReceipt, InvocationState,
    InvokeArgument, InvokeRequest, Pending, pending,
};

use crate::{
    AGENT_APP_ID, AGENT_SUMMARISE_ACTION, AGENT_TRANSFORM_KIND, ActionCatalog, AiTransform,
    AskRequest, Binding, CatchUp, CheckErrorKind, Condition, ConfirmationRequest, NoShortcuts,
    RunHooks, RunId, RunMode, RunOutcome, RunReport, RunRequest, RunTrigger,
    SHORTCUT_RESOURCE_KIND, SHORTCUT_RUN_ACTION, SHORTCUTS_APP_ID, Schedule, Selector, Shortcut,
    Step, StepFailure, StepKind, StepPath, StepRecord, StepStatus, StopWatch, TimeOfDay,
    TriggerSpec, Weekday, check, run, shortcut_resource_id,
};

const NOTES: &str = "so.tilo.notes";
const MAIL: &str = "so.tilo.mail";
const INBOX: &str = "notes.folder:inbox";
const NOTE: &str = "notes.note:1";

fn row(
    app: &str,
    resource: &str,
    kind: &str,
    action: &str,
    params: Vec<ActionParam>,
    contract: ActionContract,
) -> CatalogueRow {
    CatalogueRow {
        provider: format!("{app}#1"),
        app_id: app.into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: resource.into(),
            kind: kind.into(),
            label: resource.into(),
            presentation: None,
        },
        action: Some(ActionDescriptor {
            semantic_identity: None,
            address: ActionAddress::new(resource, action),
            resource_kind: kind.into(),
            resource_label: resource.into(),
            action_label: action.into(),
            description: None,
            locus: ExecutionLocus::ClientLocal,
            kind: OperationKind::Document,
            contract,
            input: InputSpec::None,
            params,
            view: None,
        }),
        has_children: false,
        visibility: None,
    }
}

fn returning(result: ResultSpec) -> ActionContract {
    ActionContract {
        result,
        ..ActionContract::default()
    }
}

fn list_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "titles": {"type": "array", "items": {"type": "string"}},
            "count": {"type": "integer"}
        },
        "additionalProperties": false
    })
}

/// A host offering a notes app, a mail app, the agent's summariser and the
/// shortcuts app. It runs whatever it is asked, as long as it offers it: the
/// caller's own authority is not what these tests are about.
struct FakeHost {
    rows: Vec<CatalogueRow>,
    state: Mutex<HostState>,
}

#[derive(Default)]
struct HostState {
    ledger: InvocationLedger<InvocationUpdate>,
    started: Vec<InvokeRequest>,
}

impl FakeHost {
    fn new() -> Self {
        let delete = ActionContract {
            risk: RiskLevel::Destructive,
            confirmation: ConfirmationPolicy::Confirm,
            ..ActionContract::default()
        };
        let rows = vec![
            row(
                NOTES,
                INBOX,
                "notes.folder",
                "list",
                Vec::new(),
                ActionContract {
                    kind: tilo_actions::contract::ActionKind::Read,
                    ..returning(ResultSpec::Json {
                        schema: list_schema(),
                    })
                },
            ),
            row(
                NOTES,
                INBOX,
                "notes.folder",
                "create",
                vec![
                    ActionParam::required("title", ParamType::Text, "Title of the new note"),
                    ActionParam::optional("pinned", ParamType::Boolean, "Pin it; default no"),
                ],
                returning(ResultSpec::Resource {
                    resource_kind: "notes.note".into(),
                }),
            ),
            row(
                NOTES,
                NOTE,
                "notes.note",
                "archive",
                Vec::new(),
                ActionContract::default(),
            ),
            row(NOTES, NOTE, "notes.note", "delete", Vec::new(), delete),
            row(
                MAIL,
                "mail.outbox:main",
                "mail.outbox",
                "send",
                vec![ActionParam::required(
                    "body",
                    ParamType::Text,
                    "Message body",
                )],
                ActionContract::default(),
            ),
            row(
                AGENT_APP_ID,
                "agent.transform:default",
                AGENT_TRANSFORM_KIND,
                AGENT_SUMMARISE_ACTION,
                vec![ActionParam::required(
                    "input",
                    ParamType::Text,
                    "Text to condense",
                )],
                returning(ResultSpec::Json {
                    schema: json!({"type": "string"}),
                }),
            ),
            row(
                SHORTCUTS_APP_ID,
                &shortcut_resource_id("a"),
                SHORTCUT_RESOURCE_KIND,
                SHORTCUT_RUN_ACTION,
                Vec::new(),
                ActionContract::default(),
            ),
            row(
                SHORTCUTS_APP_ID,
                &shortcut_resource_id("b"),
                SHORTCUT_RESOURCE_KIND,
                SHORTCUT_RUN_ACTION,
                Vec::new(),
                ActionContract::default(),
            ),
        ];
        Self {
            rows,
            state: Mutex::new(HostState::default()),
        }
    }

    fn catalog(&self) -> ActionCatalog {
        ActionCatalog::new(self.rows.clone())
    }

    fn started(&self) -> Vec<InvokeRequest> {
        self.state.lock().unwrap().started.clone()
    }

    fn output(request: &InvokeRequest) -> ActionOutput {
        let text = |name: &str| match &request.argument {
            InvokeArgument::Json(args) => args.get(name).and_then(Value::as_str).map(str::to_owned),
            InvokeArgument::None | InvokeArgument::Text(_) => None,
        };
        match request.target.address.action.as_str() {
            "list" => ActionOutput {
                value: Some(json!({"titles": ["a", "b"], "count": 2})),
                resources: Vec::new(),
            },
            "create" | "send" => ActionOutput {
                value: None,
                resources: vec![NOTE.into()],
            },
            AGENT_SUMMARISE_ACTION => ActionOutput {
                value: Some(json!(format!("summary of {}", text("input").unwrap()))),
                resources: Vec::new(),
            },
            _ => ActionOutput::default(),
        }
    }
}

impl ActionHost for FakeHost {
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
        let mut state = self.state.lock().unwrap();
        let HostState { ledger, started } = &mut *state;
        let receipt = ledger
            .admit(caller, &request, || {
                started.push(request.clone());
                Ok(InvocationUpdate::Succeeded {
                    invocation: request.invocation,
                    output: Self::output(&request),
                })
            })
            .map(|(update, _)| InvocationState::new(request.invocation, update.clone()).receipt());
        drop(state);
        Pending::ready(receipt)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let state = self.state.lock().unwrap();
        state
            .ledger
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

#[derive(Debug, Default)]
struct Hooks {
    confirmation: Option<Confirmation>,
    answer: Option<Value>,
    confirmations: Mutex<Vec<ConfirmationRequest>>,
    /// How many times a run fell back to waiting out a poll interval.
    settles: AtomicUsize,
}

impl RunHooks for Hooks {
    fn confirm(&self, request: ConfirmationRequest) -> Pending<Option<Confirmation>> {
        self.confirmations.lock().unwrap().push(request);
        Pending::ready(self.confirmation.clone())
    }

    fn ask(&self, _request: AskRequest) -> Pending<Option<Value>> {
        Pending::ready(self.answer.clone())
    }

    fn settle(&self, _invocation: InvocationId) -> Pending<()> {
        self.settles.fetch_add(1, Ordering::Relaxed);
        Pending::ready(())
    }
}

/// Drive a future whose every answer is already known.
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("the fake host answers at once"),
    }
}

fn caller() -> Caller {
    Caller::local_person("ada")
}

fn app_rule(app: &str) -> Rule {
    Rule::new(
        TreePath::root().any(Level::Room).app(app),
        KindSet::ALL,
        Answer::Allow,
    )
}

fn notes_grant() -> Vec<Rule> {
    vec![app_rule(NOTES)]
}

fn shortcut(id: &str, steps: Vec<Step>) -> Shortcut {
    Shortcut {
        id: id.into(),
        name: "Tidy inbox".into(),
        inputs: Vec::new(),
        steps,
        step_timeout_secs: None,
        output: None,
        grant: notes_grant(),
        triggers: Vec::new(),
        saved_by: None,
        saved_proof: None,
    }
}

/// A step invoking `action` of `app` on the resource `resource` selects. Its
/// key names the app's root kind, so it reaches any kind the app publishes.
fn invoke(id: &str, app: &str, resource: Selector, action: &str, args: &[(&str, Binding)]) -> Step {
    let kind = app.rsplit('.').next().unwrap_or(app);
    Step::new(
        id,
        StepKind::Invoke {
            action: ActionKey::new(app, kind, action),
            resource,
            args: args
                .iter()
                .map(|(name, binding)| ((*name).to_string(), binding.clone()))
                .collect::<BTreeMap<_, _>>(),
            appearance: None,
        },
    )
}

fn list() -> Step {
    invoke("list", NOTES, Selector::Fixed(INBOX.into()), "list", &[])
}

fn run_with(
    host: &FakeHost,
    hooks: &Hooks,
    shortcut: &Shortcut,
    run_id: RunId,
    mode: RunMode,
) -> RunReport {
    block_on(run(
        host,
        hooks,
        RunRequest {
            shortcut,
            run: run_id,
            caller: caller(),
            mode,
            trigger: RunTrigger::Manual,
            inputs: serde_json::Map::new(),
            caused_by: None,
        },
    ))
}

fn failure(report: &RunReport) -> Option<(&StepPath, &StepFailure)> {
    match &report.outcome {
        RunOutcome::Completed | RunOutcome::Cancelled { .. } => None,
        RunOutcome::Failed { path, failure } => Some((path, failure)),
    }
}

#[test]
fn an_incompatible_binding_is_refused_at_save() {
    let host = FakeHost::new();
    let bad = shortcut(
        "tidy",
        vec![
            list(),
            invoke(
                "count-as-title",
                NOTES,
                Selector::Fixed(INBOX.into()),
                "create",
                &[("title", Binding::step("list", "/count"))],
            ),
            invoke(
                "nowhere",
                NOTES,
                Selector::Fixed(INBOX.into()),
                "create",
                &[
                    ("title", Binding::step("list", "/missing")),
                    ("pinned", Binding::Literal(json!("yes"))),
                ],
            ),
        ],
    );
    let errors = check(&bad, &host.catalog(), &NoShortcuts).unwrap_err();
    let at = |path: &str| {
        errors
            .iter()
            .filter(|error| error.path.as_str() == path)
            .map(|error| error.kind.clone())
            .collect::<Vec<_>>()
    };
    assert!(
        matches!(at("1").as_slice(), [CheckErrorKind::Incompatible(reason)] if reason.contains("title")),
        "{errors:?}"
    );
    let second = at("2");
    assert!(
        second
            .iter()
            .any(|kind| matches!(kind, CheckErrorKind::BadPointer { .. })),
        "{errors:?}"
    );
    assert!(
        second.iter().any(
            |kind| matches!(kind, CheckErrorKind::Incompatible(reason) if reason.contains("pinned"))
        ),
        "{errors:?}"
    );

    let good = shortcut(
        "tidy",
        vec![
            list(),
            invoke(
                "first-title",
                NOTES,
                Selector::Fixed(INBOX.into()),
                "create",
                &[("title", Binding::step("list", "/titles/0"))],
            ),
        ],
    );
    assert_eq!(check(&good, &host.catalog(), &NoShortcuts), Ok(()));
}

#[test]
fn a_step_output_feeds_the_next_steps_input() {
    let host = FakeHost::new();
    let mut tidy = shortcut(
        "tidy",
        vec![
            list(),
            invoke(
                "create",
                NOTES,
                Selector::Fixed(INBOX.into()),
                "create",
                &[(
                    "title",
                    Binding::Template("Re: {{step.list/titles/0}}".into()),
                )],
            ),
            invoke(
                "archive",
                NOTES,
                Selector::FromBinding(Binding::step("create", "")),
                "archive",
                &[],
            ),
        ],
    );
    tidy.output = Some(Binding::step("create", ""));
    assert_eq!(check(&tidy, &host.catalog(), &NoShortcuts), Ok(()));

    let run_id = RunId::new();
    let report = run_with(&host, &Hooks::default(), &tidy, run_id, RunMode::Unattended);
    assert_eq!(report.outcome, RunOutcome::Completed, "{report:?}");
    assert_eq!(report.output, Some(json!(NOTE)));

    let started = host.started();
    assert_eq!(started.len(), 3);
    let InvokeArgument::Json(args) = &started[1].argument else {
        panic!("create carries typed arguments");
    };
    assert_eq!(args.get("title"), Some(&json!("Re: a")));
    assert_eq!(
        started[2].target.address,
        ActionAddress::new(NOTE, "archive")
    );

    let context = started[1].context.as_ref().unwrap();
    assert_eq!(context.purpose.as_deref(), Some("Tidy inbox"));
    let group = context.group.as_ref().unwrap();
    assert_eq!(group.id, run_id.0.to_string());
    assert_eq!(group.kind, "shortcut");
    assert!(
        context
            .facets
            .iter()
            .any(|f| f.key == "kind" && f.value == "shortcut")
    );
    assert!(
        context
            .facets
            .iter()
            .any(|f| f.key == "trigger" && f.value == "manual")
    );
}

#[test]
fn a_step_outside_the_grant_is_refused_even_when_the_caller_could_run_it() {
    let host = FakeHost::new();
    let leak = shortcut(
        "leak",
        vec![invoke(
            "send",
            MAIL,
            Selector::Fixed("mail.outbox:main".into()),
            "send",
            &[("body", Binding::Literal(json!("hello")))],
        )],
    );
    let errors = check(&leak, &host.catalog(), &NoShortcuts).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| matches!(error.kind, CheckErrorKind::OutsideGrant { .. })),
        "{errors:?}"
    );

    let report = run_with(
        &host,
        &Hooks::default(),
        &leak,
        RunId::new(),
        RunMode::Interactive,
    );
    assert!(
        matches!(failure(&report), Some((path, StepFailure::OutsideGrant { app_id, .. })) if path.as_str() == "0" && app_id == MAIL),
        "{report:?}"
    );
    assert!(host.started().is_empty(), "nothing reached the host");
}

#[test]
fn confirmation_is_refused_unattended_and_asked_for_interactively() {
    let host = FakeHost::new();
    let purge = shortcut(
        "purge",
        vec![invoke(
            "delete",
            NOTES,
            Selector::Fixed(NOTE.into()),
            "delete",
            &[],
        )],
    );
    assert_eq!(check(&purge, &host.catalog(), &NoShortcuts), Ok(()));

    let report = run_with(
        &host,
        &Hooks::default(),
        &purge,
        RunId::new(),
        RunMode::Unattended,
    );
    assert!(
        matches!(
            failure(&report),
            Some((_, StepFailure::ConfirmationRequired))
        ),
        "{report:?}"
    );
    assert!(host.started().is_empty());

    let declining = Hooks::default();
    let report = run_with(
        &host,
        &declining,
        &purge,
        RunId::new(),
        RunMode::Interactive,
    );
    assert!(
        matches!(failure(&report), Some((_, StepFailure::Declined))),
        "{report:?}"
    );
    assert_eq!(declining.confirmations.lock().unwrap().len(), 1);
    assert!(host.started().is_empty());

    let confirming = Hooks {
        confirmation: Some(Confirmation::Confirmed),
        ..Hooks::default()
    };
    let report = run_with(
        &host,
        &confirming,
        &purge,
        RunId::new(),
        RunMode::Interactive,
    );
    assert_eq!(report.outcome, RunOutcome::Completed, "{report:?}");
    let started = host.started();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].confirmation, Some(Confirmation::Confirmed));
}

#[test]
fn an_unattended_run_takes_only_the_exact_actions_consented_to() {
    let host = FakeHost::new();
    let purge = shortcut(
        "purge",
        vec![invoke(
            "delete",
            NOTES,
            Selector::Fixed(NOTE.into()),
            "delete",
            &[],
        )],
    );
    let consented = |resource: &str| {
        let mut shortcut = purge.clone();
        shortcut.grant.push(crate::action_rule(
            NOTES,
            "notes.note",
            Some(resource),
            "delete",
            KindSet::ALL,
            Answer::Allow,
        ));
        shortcut
    };
    let report = run_with(
        &host,
        &Hooks::default(),
        &consented("notes.note:other"),
        RunId::new(),
        RunMode::Unattended,
    );
    assert!(
        matches!(
            failure(&report),
            Some((_, StepFailure::ConfirmationRequired))
        ),
        "consent to another resource is no consent here: {report:?}"
    );
    assert!(host.started().is_empty());

    let hooks = Hooks::default();
    let report = run_with(
        &host,
        &hooks,
        &consented(NOTE),
        RunId::new(),
        RunMode::Unattended,
    );
    assert_eq!(report.outcome, RunOutcome::Completed, "{report:?}");
    assert!(
        hooks.confirmations.lock().unwrap().is_empty(),
        "nobody is asked in an unattended run"
    );
    let started = host.started();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].confirmation, Some(Confirmation::Confirmed));
}

/// An action that needs its person attending (it opens a chooser) is never
/// consented to in advance: an unattended run stops at it even when a rule
/// names it, and an interactive run asks.
#[test]
fn an_attended_action_is_never_consented_to_in_advance() {
    let mut host = FakeHost::new();
    host.rows.push(row(
        NOTES,
        NOTE,
        "notes.note",
        "attach",
        Vec::new(),
        ActionContract {
            tags: vec![tilo_actions::ATTENDED_TAG.to_owned()],
            ..ActionContract::default()
        },
    ));
    let mut attach = shortcut(
        "attach",
        vec![invoke(
            "attach",
            NOTES,
            Selector::Fixed(NOTE.into()),
            "attach",
            &[],
        )],
    );
    attach.grant.push(crate::action_rule(
        NOTES,
        "notes.note",
        Some(NOTE),
        "attach",
        KindSet::ALL,
        Answer::Allow,
    ));
    let report = run_with(
        &host,
        &Hooks::default(),
        &attach,
        RunId::new(),
        RunMode::Unattended,
    );
    assert!(
        matches!(
            failure(&report),
            Some((_, StepFailure::ConfirmationRequired))
        ),
        "{report:?}"
    );
    let declining = Hooks::default();
    run_with(
        &host,
        &declining,
        &attach,
        RunId::new(),
        RunMode::Interactive,
    );
    assert_eq!(declining.confirmations.lock().unwrap().len(), 1);
    assert!(host.started().is_empty());
}

#[test]
fn a_retried_run_reuses_its_invocation_ids_and_acts_once() {
    let host = FakeHost::new();
    let tidy = shortcut(
        "tidy",
        vec![
            list(),
            Step::new(
                "each",
                StepKind::ForEach {
                    items: Binding::step("list", "/titles"),
                    item: "title".into(),
                    body: vec![invoke(
                        "make",
                        NOTES,
                        Selector::Fixed(INBOX.into()),
                        "create",
                        &[("title", Binding::Var("title".into()))],
                    )],
                },
            ),
        ],
    );
    let run_id = RunId::new();
    let first = run_with(&host, &Hooks::default(), &tidy, run_id, RunMode::Unattended);
    assert_eq!(first.outcome, RunOutcome::Completed, "{first:?}");
    let ids = first.invocations();
    assert_eq!(ids.len(), 3);
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), 3, "every loop iteration has its own id");

    let retry = run_with(&host, &Hooks::default(), &tidy, run_id, RunMode::Unattended);
    assert_eq!(retry.outcome, RunOutcome::Completed, "{retry:?}");
    assert_eq!(retry.invocations(), ids);
    assert_eq!(
        host.started().len(),
        3,
        "the retry observed, it did not re-run"
    );

    let other = run_with(
        &host,
        &Hooks::default(),
        &tidy,
        RunId::new(),
        RunMode::Unattended,
    );
    assert!(other.invocations().iter().all(|id| !ids.contains(id)));
    assert_eq!(host.started().len(), 6);
}

#[test]
fn bindings_address_into_an_object_input_and_a_loop_item() {
    let host = FakeHost::new();
    let mut make = shortcut(
        "make",
        vec![
            Step::new(
                "each",
                StepKind::ForEach {
                    items: Binding::Input("event/notes".into()),
                    item: "note".into(),
                    body: vec![invoke(
                        "make",
                        NOTES,
                        Selector::Fixed(INBOX.into()),
                        "create",
                        &[("title", Binding::Var("note/title".into()))],
                    )],
                },
            ),
            invoke(
                "last",
                NOTES,
                Selector::Fixed(INBOX.into()),
                "create",
                &[(
                    "title",
                    Binding::Template("by {{input.event/by/name}}".into()),
                )],
            ),
        ],
    );
    make.inputs = vec![ActionParam::required(
        "event",
        ParamType::Object,
        "What fired",
    )];
    assert_eq!(check(&make, &host.catalog(), &NoShortcuts), Ok(()));
    let mut inputs = serde_json::Map::new();
    inputs.insert(
        "event".into(),
        json!({"notes": [{"title": "a"}, {"title": "b"}], "by": {"name": "Ada"}}),
    );
    let report = block_on(run(
        &host,
        &Hooks::default(),
        RunRequest {
            shortcut: &make,
            run: RunId::new(),
            caller: caller(),
            mode: RunMode::Unattended,
            trigger: RunTrigger::Manual,
            inputs,
            caused_by: None,
        },
    ));
    assert_eq!(report.outcome, RunOutcome::Completed, "{report:?}");
    let titles: Vec<_> = host
        .started()
        .iter()
        .map(|request| match &request.argument {
            InvokeArgument::Json(args) => args.get("title").cloned(),
            _ => None,
        })
        .collect();
    assert_eq!(
        titles,
        [Some(json!("a")), Some(json!("b")), Some(json!("by Ada"))]
    );

    let mut unknown = make;
    unknown.steps.truncate(1);
    if let StepKind::ForEach { items, .. } = &mut unknown.steps[0].kind {
        *items = Binding::Input("other/notes".into());
    }
    assert!(
        check(&unknown, &host.catalog(), &NoShortcuts).is_err(),
        "the input a pointer starts at must exist"
    );
}

#[test]
fn a_verb_answers_the_item_it_created_and_a_null_option_is_omitted() {
    let host = FakeHost::new();
    let mut send = shortcut(
        "send",
        vec![
            invoke(
                "send",
                MAIL,
                Selector::Fixed("mail.outbox:main".into()),
                "send",
                &[("body", Binding::Literal(json!("hi")))],
            ),
            invoke(
                "note",
                NOTES,
                Selector::Fixed(INBOX.into()),
                "create",
                &[
                    ("title", Binding::Literal(json!("t"))),
                    ("pinned", Binding::Literal(Value::Null)),
                ],
            ),
            invoke(
                "archive",
                NOTES,
                Selector::FromBinding(Binding::step("send", "")),
                "archive",
                &[],
            ),
        ],
    );
    send.grant.push(app_rule(MAIL));
    assert_eq!(check(&send, &host.catalog(), &NoShortcuts), Ok(()));
    let report = run_with(
        &host,
        &Hooks::default(),
        &send,
        RunId::new(),
        RunMode::Unattended,
    );
    assert_eq!(report.outcome, RunOutcome::Completed, "{report:?}");
    let started = host.started();
    assert_eq!(
        started[1].argument,
        InvokeArgument::Json(json!({"title": "t"}).as_object().unwrap().clone()),
        "a null optional argument is left out"
    );
    assert_eq!(started[2].target.address.resource, NOTE);
}

#[test]
fn an_invoke_step_carries_the_appearance_it_binds_and_refuses_a_bad_one() {
    let host = FakeHost::new();
    let mut step = invoke(
        "make",
        NOTES,
        Selector::Fixed(INBOX.into()),
        "create",
        &[("title", Binding::Literal(json!("hi")))],
    );
    if let StepKind::Invoke { appearance, .. } = &mut step.kind {
        *appearance = Some(Binding::Input("event/as".into()));
    }
    let mut make = shortcut("make", vec![step]);
    make.inputs = vec![ActionParam::required(
        "event",
        ParamType::Object,
        "What fired",
    )];
    assert_eq!(check(&make, &host.catalog(), &NoShortcuts), Ok(()));
    let go = |shown: Value| {
        let mut inputs = serde_json::Map::new();
        inputs.insert("event".into(), json!({ "as": shown }));
        block_on(run(
            &host,
            &Hooks::default(),
            RunRequest {
                shortcut: &make,
                run: RunId::new(),
                caller: caller(),
                mode: RunMode::Unattended,
                trigger: RunTrigger::Manual,
                inputs,
                caused_by: None,
            },
        ))
    };
    let shown = json!({"name": "Ada", "persona": {"service": "discord", "account": "42"}});
    let report = go(shown);
    assert_eq!(report.outcome, RunOutcome::Completed, "{report:?}");
    let sent = host.started()[0].appearance.clone().expect("an appearance");
    assert_eq!(sent.name, "Ada");
    assert_eq!(sent.persona.map(|p| p.account).as_deref(), Some("42"));

    let report = go(json!({"name": " padded "}));
    assert!(
        matches!(failure(&report), Some((_, StepFailure::Arguments { .. }))),
        "{report:?}"
    );
    assert_eq!(
        host.started().len(),
        1,
        "a refused appearance invokes nothing"
    );
}

#[test]
fn for_each_runs_its_body_per_item_and_if_takes_one_branch() {
    let host = FakeHost::new();
    let tidy = shortcut(
        "tidy",
        vec![
            list(),
            Step::new(
                "each",
                StepKind::ForEach {
                    items: Binding::step("list", "/titles"),
                    item: "title".into(),
                    body: vec![invoke(
                        "make",
                        NOTES,
                        Selector::Fixed(INBOX.into()),
                        "create",
                        &[("title", Binding::Var("title".into()))],
                    )],
                },
            ),
            Step::new(
                "two",
                StepKind::If {
                    cond: Condition::Equals {
                        left: Binding::step("list", "/count"),
                        right: Binding::Literal(json!(2)),
                    },
                    then: vec![invoke(
                        "archive",
                        NOTES,
                        Selector::FirstOfKind("notes.note".into()),
                        "archive",
                        &[],
                    )],
                    otherwise: vec![invoke(
                        "delete",
                        NOTES,
                        Selector::Fixed(NOTE.into()),
                        "delete",
                        &[],
                    )],
                },
            ),
        ],
    );
    assert_eq!(check(&tidy, &host.catalog(), &NoShortcuts), Ok(()));
    let report = run_with(
        &host,
        &Hooks::default(),
        &tidy,
        RunId::new(),
        RunMode::Unattended,
    );
    assert_eq!(report.outcome, RunOutcome::Completed, "{report:?}");

    let started = host.started();
    let actions: Vec<_> = started
        .iter()
        .map(|request| request.target.address.action.as_str())
        .collect();
    assert_eq!(actions, ["list", "create", "create", "archive"]);
    let titles: Vec<_> = started[1..3]
        .iter()
        .map(|request| match &request.argument {
            InvokeArgument::Json(args) => args.get("title").cloned(),
            _ => None,
        })
        .collect();
    assert_eq!(titles, [Some(json!("a")), Some(json!("b"))]);
    let paths: Vec<_> = report.steps.iter().map(|step| step.path.as_str()).collect();
    assert_eq!(
        paths,
        ["0", "1", "1.each[0].0", "1.each[1].0", "2", "2.then.0"]
    );

    let mut looped_var = tidy;
    looped_var.steps.push(invoke(
        "after",
        NOTES,
        Selector::Fixed(INBOX.into()),
        "create",
        &[("title", Binding::Var("title".into()))],
    ));
    let errors = check(&looped_var, &host.catalog(), &NoShortcuts).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.kind == CheckErrorKind::UnknownVariable("title".into())),
        "a loop variable does not outlive its loop: {errors:?}"
    );
}

#[test]
fn a_shortcut_that_could_run_itself_is_refused() {
    let host = FakeHost::new();
    let shortcuts_grant = app_rule(SHORTCUTS_APP_ID);
    let calling = |id: &str, callee: &str| {
        let mut shortcut = shortcut(
            id,
            vec![invoke(
                "call",
                SHORTCUTS_APP_ID,
                Selector::Fixed(shortcut_resource_id(callee)),
                SHORTCUT_RUN_ACTION,
                &[],
            )],
        );
        shortcut.grant.push(shortcuts_grant.clone());
        shortcut
    };

    let direct = calling("a", "a");
    let errors = check(&direct, &host.catalog(), &NoShortcuts).unwrap_err();
    assert_eq!(
        errors.iter().map(|e| e.kind.clone()).collect::<Vec<_>>(),
        [CheckErrorKind::Recursion {
            chain: vec!["a".into(), "a".into()]
        }]
    );
    let report = run_with(
        &host,
        &Hooks::default(),
        &direct,
        RunId::new(),
        RunMode::Interactive,
    );
    assert!(
        matches!(failure(&report), Some((_, StepFailure::Recursion))),
        "{report:?}"
    );
    assert!(host.started().is_empty());

    let a = calling("a", "b");
    let b = calling("b", "a");
    let resolver = |id: &str| (id == "b").then(|| b.clone());
    let errors = check(&a, &host.catalog(), &resolver).unwrap_err();
    assert_eq!(
        errors.iter().map(|e| e.kind.clone()).collect::<Vec<_>>(),
        [CheckErrorKind::Recursion {
            chain: vec!["a".into(), "b".into(), "a".into()]
        }]
    );
    assert_eq!(check(&a, &host.catalog(), &NoShortcuts), Ok(()));

    let mut any = calling("a", "b");
    any.steps = vec![invoke(
        "call",
        SHORTCUTS_APP_ID,
        Selector::FirstOfKind(SHORTCUT_RESOURCE_KIND.into()),
        SHORTCUT_RUN_ACTION,
        &[],
    )];
    let errors = check(&any, &host.catalog(), &NoShortcuts).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.kind == CheckErrorKind::UnresolvableShortcut),
        "{errors:?}"
    );
}

#[test]
fn an_ai_step_is_an_invocation_of_the_agent_transform() {
    let host = FakeHost::new();
    let mut digest = shortcut(
        "digest",
        vec![Step::new(
            "sum",
            StepKind::Ai {
                transform: AiTransform::Summarise,
                input: Binding::Literal(json!("long text")),
            },
        )],
    );
    digest.grant.push(app_rule(AGENT_APP_ID));
    digest.output = Some(Binding::step("sum", ""));
    assert_eq!(check(&digest, &host.catalog(), &NoShortcuts), Ok(()));

    let report = run_with(
        &host,
        &Hooks::default(),
        &digest,
        RunId::new(),
        RunMode::Unattended,
    );
    assert_eq!(report.outcome, RunOutcome::Completed, "{report:?}");
    assert_eq!(report.output, Some(json!("summary of long text")));
    let started = host.started();
    assert_eq!(
        started[0].target.address,
        ActionAddress::new("agent.transform:default", AGENT_SUMMARISE_ACTION)
    );
}

#[test]
fn an_ai_step_never_reads_a_secret() {
    let host = FakeHost::new();
    let mut leak = shortcut(
        "leak",
        vec![Step::new(
            "sum",
            StepKind::Ai {
                transform: AiTransform::Summarise,
                input: Binding::Input("token".into()),
            },
        )],
    );
    leak.inputs = vec![ActionParam::required(
        "token",
        ParamType::Secret,
        "An API token",
    )];
    leak.grant.push(app_rule(AGENT_APP_ID));
    let errors = check(&leak, &host.catalog(), &NoShortcuts).unwrap_err();
    assert!(
        errors.iter().any(|error| matches!(
            &error.kind,
            CheckErrorKind::Incompatible(reason) if reason.contains("secret")
        )),
        "{errors:?}"
    );

    // Refused at run time too, for a shortcut that was never checked.
    let mut inputs = serde_json::Map::new();
    inputs.insert("token".into(), json!("s3cret"));
    let report = block_on(run(
        &host,
        &Hooks::default(),
        RunRequest {
            shortcut: &leak,
            run: RunId::new(),
            caller: caller(),
            mode: RunMode::Unattended,
            trigger: RunTrigger::Manual,
            inputs,
            caused_by: None,
        },
    ));
    assert!(
        matches!(failure(&report), Some((_, StepFailure::Binding { reason })) if reason.contains("secret")),
        "{report:?}"
    );
    assert!(host.started().is_empty());
}

#[test]
fn secret_variables_cannot_be_rendered_or_sent_to_ai() {
    for last in [
        Step::new(
            "sum",
            StepKind::Ai {
                transform: AiTransform::Summarise,
                input: Binding::Var("copy".into()),
            },
        ),
        invoke(
            "send",
            MAIL,
            Selector::Fixed("mail.outbox:main".into()),
            "send",
            &[("body", Binding::Template("Token: {{var.copy}}".into()))],
        ),
    ] {
        let host = FakeHost::new();
        let mut leak = shortcut(
            "leak",
            vec![
                Step::new(
                    "token",
                    StepKind::Ask {
                        param: ActionParam::required("token", ParamType::Secret, "API token"),
                    },
                ),
                Step::new(
                    "copy",
                    StepKind::SetVar {
                        name: "copy".into(),
                        value: Binding::step("token", ""),
                    },
                ),
                last,
            ],
        );
        leak.grant.extend([app_rule(AGENT_APP_ID), app_rule(MAIL)]);
        let report = run_with(
            &host,
            &Hooks {
                answer: Some(json!("s3cret")),
                ..Hooks::default()
            },
            &leak,
            RunId::new(),
            RunMode::Interactive,
        );
        assert!(
            matches!(failure(&report), Some((_, StepFailure::Binding { reason })) if reason.contains("secret")),
            "{report:?}"
        );
        assert!(host.started().is_empty());
        assert!(!serde_json::to_string(&report).unwrap().contains("s3cret"));
    }
}

#[test]
fn an_unattended_run_cannot_ask() {
    let host = FakeHost::new();
    let asking = shortcut(
        "asking",
        vec![Step::new(
            "who",
            StepKind::Ask {
                param: ActionParam::required("name", ParamType::Text, "Who to greet"),
            },
        )],
    );
    let report = run_with(
        &host,
        &Hooks::default(),
        &asking,
        RunId::new(),
        RunMode::Unattended,
    );
    assert!(
        matches!(failure(&report), Some((_, StepFailure::NeedsPerson))),
        "{report:?}"
    );

    let answering = Hooks {
        answer: Some(json!("Ada")),
        ..Hooks::default()
    };
    let report = run_with(
        &host,
        &answering,
        &asking,
        RunId::new(),
        RunMode::Interactive,
    );
    assert_eq!(report.outcome, RunOutcome::Completed, "{report:?}");

    let wrong = Hooks {
        answer: Some(json!(7)),
        ..Hooks::default()
    };
    let report = run_with(&host, &wrong, &asking, RunId::new(), RunMode::Interactive);
    assert!(
        matches!(failure(&report), Some((_, StepFailure::Arguments { .. }))),
        "{report:?}"
    );
}

#[test]
fn a_shortcut_round_trips_through_serde() {
    let mut tidy = shortcut(
        "tidy",
        vec![
            list(),
            Step::new(
                "check",
                StepKind::If {
                    cond: Condition::Not(Box::new(Condition::Truthy(Binding::step(
                        "list", "/count",
                    )))),
                    then: vec![Step::new(
                        "flag",
                        StepKind::SetVar {
                            name: "empty".into(),
                            value: Binding::Literal(json!(true)),
                        },
                    )],
                    otherwise: Vec::new(),
                },
            ),
        ],
    );
    tidy.inputs = vec![ActionParam::optional(
        "folder",
        ParamType::Resource,
        "Where to look",
    )];
    tidy.triggers = vec![
        TriggerSpec::Keyboard {
            chord: "Super+Shift+T".into(),
        },
        TriggerSpec::Schedule {
            schedule: Schedule::Calendar {
                at: TimeOfDay { hour: 9, minute: 0 },
                weekdays: vec![Weekday::Monday, Weekday::Friday],
            },
            catch_up: CatchUp::RunOnce,
        },
        TriggerSpec::NetworkChanged {
            network: None,
            connected: Some(true),
        },
    ];
    let json = serde_json::to_value(&tidy).unwrap();
    assert_eq!(json["steps"][1]["step"], "if");
    assert_eq!(json["triggers"][1]["schedule"]["every"], "calendar");
    let back: Shortcut = serde_json::from_value(json).unwrap();
    assert_eq!(back, tidy);
}

#[test]
fn binding_options_are_exactly_what_the_checker_accepts_in_scope() {
    let host = FakeHost::new();
    let catalog = host.catalog();
    let mut tidy = shortcut(
        "tidy",
        vec![
            list(),
            Step::new(
                "label",
                StepKind::SetVar {
                    name: "label".into(),
                    value: Binding::Literal(json!("Inbox")),
                },
            ),
            Step::new(
                "each",
                StepKind::ForEach {
                    items: Binding::step("list", "/titles"),
                    item: "title".into(),
                    body: vec![invoke(
                        "copy",
                        NOTES,
                        Selector::Fixed(INBOX.into()),
                        "create",
                        &[],
                    )],
                },
            ),
        ],
    );
    tidy.inputs = vec![
        ActionParam::required("folder", ParamType::Text, "Folder"),
        ActionParam::required("limit", ParamType::Integer, "Limit"),
    ];
    let title = ActionParam::required("title", ParamType::Text, "Title");
    let offered = |at: &str, param: &ActionParam| {
        let mut path = StepPath::root();
        for segment in at.split('.') {
            path = match segment {
                "each" => path.each(None),
                index => path.child(index.parse().unwrap()),
            };
        }
        crate::binding_options(&tidy, &catalog, &path, param)
            .into_iter()
            .map(|option| option.binding)
            .collect::<Vec<_>>()
    };

    // Inside the loop: text inputs, the text variable, the loop item; the
    // integer input, the whole list result and its array field are not text.
    let inside = offered("2.each.0", &title);
    assert_eq!(
        inside,
        vec![
            Binding::Input("folder".into()),
            Binding::Var("label".into()),
            Binding::Var("title".into()),
        ]
    );
    // Before the variable is set only the input is in scope.
    assert_eq!(offered("1", &title), vec![Binding::Input("folder".into())]);
    // An array parameter is offered the list's array field.
    let items = ActionParam::required("items", ParamType::Array, "Items");
    assert_eq!(offered("1", &items), vec![Binding::step("list", "/titles")]);
    // A number parameter takes the integer input and the declared count.
    let number = ActionParam::required("n", ParamType::Number, "N");
    assert!(offered("1", &number).contains(&Binding::Input("limit".into())));
    assert!(offered("1", &number).contains(&Binding::step("list", "/count")));

    // Every option offered checks cleanly when bound.
    for binding in inside {
        let mut bound = tidy.clone();
        let StepKind::ForEach { body, .. } = &mut bound.steps[2].kind else {
            panic!("the loop body is an action step")
        };
        let StepKind::Invoke { args, .. } = &mut body[0].kind else {
            panic!("the loop body is an action step")
        };
        args.insert("title".into(), binding.clone());
        assert_eq!(
            check(&bound, &catalog, &NoShortcuts),
            Ok(()),
            "{binding:?} was offered, so it must check"
        );
    }

    // Past the last step, everything the top level introduced.
    let after = crate::values_at(&tidy, &catalog, &StepPath::root().child(3));
    assert!(
        after
            .iter()
            .any(|option| option.binding == Binding::Var("label".into()))
    );
    assert!(
        !after
            .iter()
            .any(|option| option.binding == Binding::Var("title".into())),
        "the loop item is gone after the loop"
    );
}

/// A host whose every action keeps running after it is accepted, as a
/// background job does, until the test or enough observations finish it.
struct RunningHost {
    offers: FakeHost,
    /// Whether the host can tell a waiter when the job changes.
    pushes: bool,
    job: Mutex<RunningJob>,
}

#[derive(Default)]
struct RunningJob {
    invocation: Option<InvocationId>,
    finished: bool,
    observes: usize,
    /// For a host that cannot push: how many more observations still find
    /// the job running.
    running_for: usize,
    /// The waiter [`ActionHost::next_change`] handed out.
    change: Option<Completer<()>>,
}

impl RunningHost {
    fn new(pushes: bool, running_for: usize) -> Self {
        Self {
            offers: FakeHost::new(),
            pushes,
            job: Mutex::new(RunningJob {
                running_for,
                ..RunningJob::default()
            }),
        }
    }

    /// The job ends, and whoever waits on it is told.
    fn finish(&self) {
        let change = {
            let mut job = self.job.lock().unwrap();
            job.finished = true;
            job.change.take()
        };
        change.expect("the run waits on the host").complete(());
    }

    fn observes(&self) -> usize {
        self.job.lock().unwrap().observes
    }
}

impl ActionHost for RunningHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.offers.query(caller, query)
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let id = request.invocation;
        self.job.lock().unwrap().invocation = Some(id);
        Pending::ready(Ok(InvocationState::new(
            id,
            InvocationUpdate::Accepted { invocation: id },
        )
        .receipt()))
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let finished = {
            let mut job = self.job.lock().unwrap();
            job.observes = job.observes.saturating_add(1);
            if job.invocation != Some(*id) {
                return None;
            }
            if !self.pushes {
                match job.running_for.checked_sub(1) {
                    Some(left) => job.running_for = left,
                    None => job.finished = true,
                }
            }
            job.finished
        };
        let update = if finished {
            InvocationUpdate::Succeeded {
                invocation: *id,
                output: ActionOutput::default(),
            }
        } else {
            InvocationUpdate::Accepted { invocation: *id }
        };
        Some(InvocationState::new(*id, update))
    }

    fn next_change(&self, _caller: &Caller, _id: &InvocationId) -> Option<Pending<()>> {
        if !self.pushes {
            return None;
        }
        let (change, waiter) = pending();
        self.job.lock().unwrap().change = Some(change);
        Some(waiter)
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionHostError>> {
        tilo_actions::remote::Pending::ready(Err(ActionHostError::NotCancellable {
            reason: "not in these tests".into(),
        }))
    }
}

fn archive_shortcut() -> Shortcut {
    shortcut(
        "archive",
        vec![invoke(
            "archive",
            NOTES,
            Selector::Fixed(NOTE.into()),
            "archive",
            &[],
        )],
    )
}

fn archive_run<'a, K: RunHooks>(
    host: &'a RunningHost,
    hooks: &'a K,
    shortcut: &'a Shortcut,
) -> impl Future<Output = RunReport> + 'a {
    run(
        host,
        hooks,
        RunRequest {
            shortcut,
            run: RunId::new(),
            caller: caller(),
            mode: RunMode::Unattended,
            trigger: RunTrigger::Manual,
            inputs: serde_json::Map::new(),
            caused_by: None,
        },
    )
}

#[test]
fn a_running_step_wakes_on_host_change_not_on_a_timer() {
    let host = RunningHost::new(true, 0);
    let hooks = Hooks::default();
    let archive = archive_shortcut();
    let mut running = std::pin::pin!(archive_run(&host, &hooks, &archive));
    let mut cx = Context::from_waker(Waker::noop());

    assert!(
        running.as_mut().poll(&mut cx).is_pending(),
        "an accepted step waits for its host"
    );
    // Only the look for an earlier invocation has observed so far: the run
    // is parked on the host's change, not re-observing on a schedule.
    assert_eq!(host.observes(), 1);
    assert!(running.as_mut().poll(&mut cx).is_pending());
    assert_eq!(
        host.observes(),
        1,
        "polling without a change observes nothing"
    );

    host.finish();
    let Poll::Ready(report) = running.as_mut().poll(&mut cx) else {
        panic!("the change finishes the step");
    };
    assert_eq!(report.outcome, RunOutcome::Completed, "{report:?}");
    assert_eq!(host.observes(), 2, "one observation per change");
    assert_eq!(
        hooks.settles.load(Ordering::Relaxed),
        0,
        "the poll fallback is unused"
    );
}

#[test]
fn a_host_without_next_change_still_completes_via_settle() {
    let host = RunningHost::new(false, 2);
    let hooks = Hooks::default();
    let archive = archive_shortcut();
    let report = block_on(archive_run(&host, &hooks, &archive));
    assert_eq!(report.outcome, RunOutcome::Completed, "{report:?}");
    // The look for an earlier invocation, two observations still running,
    // then the one that finds it finished: each after a poll interval.
    assert_eq!(host.observes(), 4);
    assert_eq!(hooks.settles.load(Ordering::Relaxed), 3);
}

/// Hooks whose runs can be stopped, at the stop `until` asks for.
#[derive(Debug)]
struct StoppableHooks {
    inner: Hooks,
    /// When a stop is reported: once the run asked `after` times, with this
    /// reason. `None` never stops.
    stop: Option<(usize, String)>,
    asked: AtomicUsize,
}

impl StoppableHooks {
    fn after(asked: usize, reason: &str) -> Self {
        Self {
            inner: Hooks::default(),
            stop: Some((asked, reason.into())),
            asked: AtomicUsize::new(0),
        }
    }
}

impl RunHooks for StoppableHooks {
    fn confirm(&self, request: ConfirmationRequest) -> Pending<Option<Confirmation>> {
        self.inner.confirm(request)
    }

    fn ask(&self, request: AskRequest) -> Pending<Option<Value>> {
        self.inner.ask(request)
    }

    fn settle(&self, invocation: InvocationId) -> Pending<()> {
        self.inner.settle(invocation)
    }

    fn stopped(&self, _what: StopWatch<'_>) -> Pending<Result<(), String>> {
        let asked = self.asked.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        Pending::ready(match &self.stop {
            Some((after, reason)) if asked >= *after => Err(reason.clone()),
            _ => Ok(()),
        })
    }
}

#[test]
fn a_stopped_run_ends_as_cancelled_and_starts_nothing_more() {
    // Two steps: the first archives, the second deletes. The stop is asked
    // before the first step, so the run never invokes anything.
    let host = RunningHost::new(true, 0);
    let hooks = StoppableHooks::after(1, "the person stopped it");
    let delete = Step::new(
        "delete",
        StepKind::Invoke {
            action: ActionKey::new(NOTES, "notes.note", "delete"),
            resource: Selector::Fixed(NOTE.into()),
            args: BTreeMap::new(),
            appearance: None,
        },
    );
    let mut tidy = archive_shortcut();
    tidy.steps.push(delete);
    let report = block_on(archive_run(&host, &hooks, &tidy));
    assert_eq!(
        report.outcome,
        RunOutcome::Cancelled {
            at: StepPath::root().child(0)
        },
        "{report:?}"
    );
    assert_eq!(host.observes(), 0, "no step was started");
    assert!(matches!(
        report.steps.as_slice(),
        [StepRecord {
            status: StepStatus::Failed {
                failure: StepFailure::Stopped { .. }
            },
            ..
        }]
    ));
}

#[test]
fn a_stop_between_steps_keeps_what_already_landed() {
    // The first step is asked about, runs, is looked at once more once it
    // finished, and the stop is asked before the second step.
    let host = RunningHost::new(true, 0);
    let hooks = StoppableHooks::after(3, "the person stopped it");
    let mut tidy = archive_shortcut();
    tidy.steps.push(Step::new(
        "delete",
        StepKind::Invoke {
            action: ActionKey::new(NOTES, "notes.note", "delete"),
            resource: Selector::Fixed(NOTE.into()),
            args: BTreeMap::new(),
            appearance: None,
        },
    ));
    // The first step is accepted and stays running until the test ends it,
    // so the stop is asked for between the two.
    let mut running = std::pin::pin!(archive_run(&host, &hooks, &tidy));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(running.as_mut().poll(&mut cx).is_pending(), "{hooks:?}");
    assert_eq!(hooks.asked.load(Ordering::Relaxed), 2);
    host.finish();
    let Poll::Ready(report) = running.as_mut().poll(&mut cx) else {
        panic!("the finished step lets the run look at the stop");
    };
    assert_eq!(
        report.outcome,
        RunOutcome::Cancelled {
            at: StepPath::root().child(1)
        },
        "{report:?}"
    );
    assert_eq!(
        host.observes(),
        2,
        "the step before the stop ran; the one after it never started"
    );
    let (first, second) = match report.steps.as_slice() {
        [first, second] => (first, second),
        other => panic!("the step that ran and the one that stopped: {other:?}"),
    };
    assert!(matches!(first.status, StepStatus::Succeeded { .. }));
    assert!(matches!(
        second.status,
        StepStatus::Failed {
            failure: StepFailure::Stopped { .. }
        }
    ));
}

#[test]
fn a_step_that_outstays_its_patience_stops_the_run_waiting() {
    // A host that never answers, and a shortcut impatient with it.
    let host = RunningHost::new(false, usize::MAX);
    let hooks = Hooks::default();
    let mut archive = archive_shortcut();
    archive.step_timeout_secs = Some(0);
    let report = block_on(archive_run(&host, &hooks, &archive));
    assert_eq!(
        failure(&report),
        Some((
            &StepPath::root().child(0),
            &StepFailure::TimedOut { seconds: 1 }
        )),
        "{report:?}"
    );
    assert!(
        host.observes() > 1,
        "the run looked again at a host that cannot push changes"
    );
}

#[test]
fn a_run_reports_the_version_of_the_shortcut_it_ran() {
    let archive = archive_shortcut();
    let edit = |name: &str| {
        let mut edited = archive.clone();
        edited.name = name.into();
        let bytes = crate::program::canonical_bytes(&edited).unwrap();
        crate::program::program_hash(&bytes)
    };
    let host = FakeHost::new();
    let first = block_on(run(
        &host,
        &Hooks::default(),
        RunRequest {
            shortcut: &archive,
            run: RunId::new(),
            caller: caller(),
            mode: RunMode::Unattended,
            trigger: RunTrigger::Manual,
            inputs: serde_json::Map::new(),
            caused_by: None,
        },
    ));
    let edited = edit("Tidy the inbox");
    // The record of the run says which version produced it, so editing the
    // shortcut afterwards does not make its history ambiguous.
    assert_eq!(first.version.as_deref(), Some(edit("Tidy inbox").as_str()));
    assert_ne!(first.version.as_deref(), Some(edited.as_str()));
}

#[test]
fn a_step_reaches_only_the_app_and_kind_its_key_names() {
    let catalog = ActionCatalog::new(vec![row(
        NOTES,
        INBOX,
        "notes.folder",
        "list",
        Vec::new(),
        ActionContract::default(),
    )]);
    let inbox = crate::ResourceTarget::Id(INBOX);
    let found = |key: ActionKey| catalog.find(&key, inbox).is_some();
    assert!(found(ActionKey::new(NOTES, "notes.folder", "list")));
    assert!(found(ActionKey::new(NOTES, "notes", "list")), "a sub-kind");
    assert!(!found(ActionKey::new(NOTES, "notes.note", "list")));
    assert!(!found(ActionKey::new(NOTES, "notes.folderish", "list")));
    assert!(!found(ActionKey::new(MAIL, "notes.folder", "list")));
    assert!(!found(ActionKey::new(NOTES, "notes.folder", "delete")));
}

/// A catalogue offering one undoable action: posting to chat `chat:1`.
fn chat_catalog() -> ActionCatalog {
    ActionCatalog::new(vec![row(
        "so.tilo.chat",
        "chat:1",
        "chat.workspace",
        "post",
        Vec::new(),
        ActionContract {
            kind: tilo_actions::contract::ActionKind::Undoable,
            ..ActionContract::default()
        },
    )])
}

/// One step posting to the first chat, with no grant yet.
fn posting_shortcut() -> Shortcut {
    let mut shortcut = shortcut(
        "bridge",
        vec![Step::new(
            "post",
            StepKind::Invoke {
                action: ActionKey::new("so.tilo.chat", "chat.workspace", "post"),
                resource: Selector::FirstOfKind("chat.workspace".into()),
                args: BTreeMap::new(),
                appearance: None,
            },
        )],
    );
    shortcut.grant.clear();
    shortcut
}

#[test]
fn check_bot_refuses_a_grant_wider_than_the_steps() {
    let catalog = chat_catalog();
    let mut shortcut = posting_shortcut();
    shortcut.grant = crate::grant::derived_rules(&shortcut, &catalog);
    assert_eq!(crate::check_bot(&shortcut, &catalog), Ok(()));

    shortcut.grant.push(crate::grant::action_rule(
        "so.tilo.chat",
        "chat.workspace",
        None,
        "delete",
        KindSet::READ_UNDOABLE,
        Answer::Allow,
    ));
    let errors = crate::check_bot(&shortcut, &catalog).expect_err("an unused rule");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e.kind, CheckErrorKind::UnusedRule(_)))
    );

    shortcut.grant.clear();
    let errors = crate::check_bot(&shortcut, &catalog).expect_err("a missing rule");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e.kind, CheckErrorKind::MissingRule(_)))
    );
}

#[test]
fn record_and_connector_triggers_round_trip_and_bind_an_event_object() {
    use crate::{ActorFilter, EVENT_INPUT};
    let record: TriggerSpec = serde_json::from_value(json!({
        "trigger": "record_changed",
        "kind": "chat.workspace",
        "actions": ["post"],
        "by": "not_bots"
    }))
    .unwrap();
    assert_eq!(
        record,
        TriggerSpec::RecordChanged {
            kind: "chat.workspace".into(),
            actions: vec!["post".into()],
            fields: vec![],
            by: ActorFilter::NotBots,
        }
    );
    assert_eq!(record.kind(), "record_changed");
    let connector: TriggerSpec = serde_json::from_value(json!({
        "trigger": "connector",
        "app_id": "so.tilo.discord",
        "source": "message-created",
        "filter": {"channel": "42"}
    }))
    .unwrap();
    assert_eq!(connector.kind(), "connector");
    for trigger in [
        &record,
        &connector,
        &TriggerSpec::IncomingMessage {
            app_id: None,
            from: None,
        },
    ] {
        let payload = trigger.payload();
        assert_eq!(payload.len(), 1);
        assert_eq!(payload[0].name.as_ref(), EVENT_INPUT);
        assert_eq!(payload[0].ty, ParamType::Object);
    }
    // Defaults: `by` is anyone, empty lists are any change.
    let any: TriggerSpec =
        serde_json::from_value(json!({"trigger": "record_changed", "kind": "tilo.room"})).unwrap();
    assert!(matches!(
        any,
        TriggerSpec::RecordChanged {
            by: ActorFilter::Anyone,
            ..
        }
    ));
}

#[test]
fn every_step_of_a_caused_run_records_the_cause() {
    let cause = tilo_actions::context::Cause {
        installation: "bot-1".into(),
        event: "cd".repeat(32),
        depth: 2,
    };
    let host = FakeHost::new();
    let tidy = shortcut("tidy", vec![list()]);
    let report = block_on(run(
        &host,
        &Hooks::default(),
        RunRequest {
            shortcut: &tidy,
            run: RunId::new(),
            caller: caller(),
            mode: RunMode::Unattended,
            trigger: RunTrigger::Manual,
            inputs: serde_json::Map::new(),
            caused_by: Some(cause.clone()),
        },
    ));
    assert_eq!(report.outcome, RunOutcome::Completed, "{report:?}");
    let started = host.started();
    assert!(!started.is_empty());
    for request in started {
        assert_eq!(request.context.unwrap().caused_by, Some(cause.clone()));
    }
}
