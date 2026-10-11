//! The editor driven through its accessible controls against a fake action
//! host: build a shortcut step by step, see the checker's verdict, review
//! the grant and save it through the door's `shortcuts#create`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use shortcuts::{APP_ID, App, Workbench};
use ui::a11y::Action;
use ui::headless::SemanticExpectation;
use ui::prelude::TestApp;
use ui::tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, CatalogueResource,
    CatalogueRow, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus,
};
use ui::tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionOutput, ActionParam, ExecutionLocus,
    InputSpec, InvocationId, OperationKind, ParamType, PermissionRequirement, ResultSpec,
};
use ui::{RemoteActions, RemoteActionsGuard};

const NOTES: &str = "so.tilo.notes";
const INBOX: &str = "notes.folder:inbox";

fn row(
    provider: &str,
    app: &str,
    resource: &str,
    kind: &str,
    action: &str,
    params: Vec<ActionParam>,
    permission: PermissionRequirement,
    result: ResultSpec,
) -> CatalogueRow {
    CatalogueRow {
        provider: provider.into(),
        app_id: app.into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: resource.into(),
            kind: kind.into(),
            label: "Inbox".into(),
            presentation: None,
        },
        action: Some(ActionDescriptor {
            semantic_identity: None,
            address: ActionAddress::new(resource, action),
            resource_kind: kind.into(),
            resource_label: "Inbox".into(),
            action_label: action.into(),
            description: None,
            locus: ExecutionLocus::default(),
            kind: OperationKind::Document,
            contract: ActionContract {
                permission,
                result,
                ..ActionContract::default()
            },
            input: InputSpec::None,
            params,
            view: None,
        }),
        fresh_binding: None,
        has_children: false,
        visibility: None,
    }
}

/// A workspace offering a notes app and the shortcut library; it records
/// every invocation and answers each at once.
struct FakeHost {
    rows: Vec<CatalogueRow>,
    /// What `shortcuts#list` answers: the saved shortcuts.
    saved: Vec<Value>,
    invoked: Mutex<Vec<InvokeRequest>>,
    callers: Mutex<Vec<Caller>>,
}

impl FakeHost {
    fn new() -> Self {
        let library = |action: &str, params: Vec<ActionParam>| {
            row(
                "workspace:room",
                APP_ID,
                "shortcuts",
                "shortcuts.library",
                action,
                params,
                PermissionRequirement::Write,
                ResultSpec::None,
            )
        };
        let shortcut_param = || {
            vec![ActionParam::required(
                "shortcut",
                ParamType::Object,
                "The shortcut",
            )]
        };
        Self {
            rows: vec![
                row(
                    "7",
                    NOTES,
                    INBOX,
                    "notes.folder",
                    "list",
                    Vec::new(),
                    PermissionRequirement::Read,
                    ResultSpec::Json {
                        schema: json!({
                            "type": "object",
                            "properties": {"titles": {"type": "array", "items": {"type": "string"}}},
                            "additionalProperties": false
                        }),
                    },
                ),
                row(
                    "7",
                    NOTES,
                    INBOX,
                    "notes.folder",
                    "create",
                    vec![
                        ActionParam::required("title", ParamType::Text, "Title"),
                        ActionParam::required("pinned", ParamType::Boolean, "Pin it"),
                    ],
                    PermissionRequirement::Write,
                    ResultSpec::None,
                ),
                library("list", Vec::new()),
                library("create", shortcut_param()),
                library("update", shortcut_param()),
                library(
                    "delete",
                    vec![ActionParam::required("id", ParamType::Text, "Id")],
                ),
            ],
            saved: Vec::new(),
            invoked: Mutex::new(Vec::new()),
            callers: Mutex::new(Vec::new()),
        }
    }

    fn library_calls(&self, action: &str) -> Vec<Value> {
        self.invoked
            .lock()
            .unwrap()
            .iter()
            .filter(|request| {
                request.target.address.resource == "shortcuts"
                    && request.target.address.action == action
            })
            .map(|request| match &request.argument {
                InvokeArgument::Json(arguments) => Value::Object(arguments.clone()),
                _ => Value::Null,
            })
            .collect()
    }
}

impl ActionHost for FakeHost {
    fn query(
        &self,
        caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.callers.lock().unwrap().push(caller.clone());
        Pending::ready(Ok(CataloguePage {
            revision: "1".into(),
            rows: self.rows.clone(),
            exhausted: true,
            ..CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        self.callers.lock().unwrap().push(caller.clone());
        let value = match request.target.address.action.as_str() {
            "list" if request.target.address.resource == "shortcuts" => {
                Some(Value::Array(self.saved.clone()))
            }
            "list" => Some(json!([])),
            "freeform" => Some(json!({
                "name": "Note the inbox",
                "steps": [
                    {"id": "list", "step": "invoke", "app_id": NOTES,
                     "resource": {"fixed": INBOX}, "action": "list"},
                    {"id": "note", "step": "invoke", "app_id": NOTES,
                     "resource": {"fixed": INBOX}, "action": "create",
                     "args": {"title": {"literal": "Inbox"}, "pinned": {"literal": false}}}
                ],
                "grant": [{"scope": "Any", "effect": "Grant", "verb": "Admin"}]
            })),
            _ => None,
        };
        let receipt = InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: ActionOutput {
                    value,
                    resources: Vec::new(),
                },
            },
        };
        self.invoked.lock().unwrap().push(request);
        Pending::ready(Ok(receipt))
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Result<(), ActionHostError> {
        Ok(())
    }
}

fn rule(action: &str, verb: collab_types::Verb) -> Value {
    serde_json::to_value(collab_types::PermissionProfileRule::grant(
        collab_types::Scope::Action {
            app: NOTES.into(),
            resource: INBOX.into(),
            action: action.into(),
        },
        verb,
    ))
    .unwrap()
}

fn open() -> (Arc<FakeHost>, RemoteActionsGuard, TestApp<App>) {
    open_with(FakeHost::new())
}

fn open_with(host: FakeHost) -> (Arc<FakeHost>, RemoteActionsGuard, TestApp<App>) {
    let host = Arc::new(host);
    let guard = RemoteActions::enter(
        Arc::clone(&host) as Arc<dyn ActionHost + Send + Sync>,
        Caller::local_app(APP_ID).in_room("room"),
    );
    let app = TestApp::<App>::new().expect("the app opens");
    (host, guard, app)
}

fn act(app: &mut TestApp<App>, target: &str, action: &Action) {
    app.harness()
        .wait_for_named(target, tilo_test_env::hang_bound())
        .unwrap_or_else(|error| {
            let mut known: Vec<String> = app.harness().targets().into_keys().collect();
            known.sort();
            panic!("{target} never appeared: {error}; targets: {known:?}")
        });
    app.harness()
        .perform_named(target, action)
        .unwrap_or_else(|error| panic!("{target}: {error}"));
}

#[test]
fn the_app_starts_empty_and_reads_the_door_as_itself() {
    let (host, _guard, mut app) = open();
    assert!(app.state().drafts.is_empty(), "no seeded drafts");
    app.harness()
        .wait_for_named("new-shortcut", tilo_test_env::hang_bound())
        .expect("the library shows");
    assert!(
        host.callers
            .lock()
            .unwrap()
            .iter()
            .all(|caller| *caller.principal()
                == ui::tilo_actions::remote::CallerPrincipal::App(APP_ID.into())),
        "every request goes to the door as the Shortcuts app"
    );
    assert_eq!(host.library_calls("list").len(), 1, "the library is read");
    let audit = app.harness().native_app_audit();
    assert!(audit.issues.is_empty(), "{audit:#?}");
}

#[test]
fn every_operation_is_a_published_action() {
    let (_host, _guard, mut app) = open();
    let resources = app.harness().resources();
    let workbench = resources
        .iter()
        .find(|resource| resource.kind == "shortcuts.workbench")
        .expect("the drafts are a published resource");
    for action in [
        "new-draft",
        "draft-shortcut",
        "set-schedule",
        "add-step",
        "set-argument",
        "remove-step",
        "move-step",
    ] {
        assert!(
            workbench.actions.iter().any(|spec| spec.id == action),
            "`{action}` is published"
        );
    }
    // The door's effects are published on the drafts, beside their edits.
    let mut effects: Vec<&str> = workbench
        .actions
        .iter()
        .filter(|spec| spec.kind == ui::tilo_actions::OperationKind::Effect)
        .map(|spec| spec.id.as_str())
        .collect();
    effects.sort_unstable();
    assert_eq!(effects, ["delete", "describe", "refresh", "run", "save"]);
    assert!(
        resources
            .iter()
            .all(|resource| resource.id.as_str() != "shortcuts.door:root"),
        "no second root carries them"
    );
}

#[test]
fn add_steps_bind_them_validate_and_save_through_the_door() {
    let (host, _guard, mut app) = open();
    app.invoke(Workbench::NEW_DRAFT.with("Tidy".into()))
        .expect("a new draft");
    let shortcut_id = app
        .state()
        .drafts
        .values()
        .next()
        .unwrap()
        .shortcut_id
        .clone();

    // A new draft is the page that opens.
    app.harness()
        .wait_for_named("draft-name", tilo_test_env::hang_bound())
        .expect("the new draft opens in the editor");

    // An input the steps can use.
    act(&mut app, "new-input-name", &Action::SetValue("who".into()));
    act(&mut app, "add-input", &Action::Activate);

    // Two steps from the live catalogue.
    act(&mut app, "add-action-top", &Action::Activate);
    act(
        &mut app,
        &format!("offer-{NOTES}-{INBOX}-list"),
        &Action::Activate,
    );
    act(&mut app, "add-action-top", &Action::Activate);
    act(
        &mut app,
        &format!("offer-{NOTES}-{INBOX}-create"),
        &Action::Activate,
    );

    // The checker's verdict on the unbound create step shows in the summary.
    app.harness()
        .expect(
            &SemanticExpectation::named("draft-problems")
                .name("2 problems to fix before saving")
                .within(tilo_test_env::hang_bound()),
        )
        .expect("the missing argument is reported");

    // `title` is offered the text input, not the list's array.
    act(
        &mut app,
        "arg-1-title",
        &Action::SetValue("input who".into()),
    );
    // `pinned` gets a typed value.
    act(
        &mut app,
        "arg-1-pinned",
        &Action::SetValue("Type a value".into()),
    );
    act(&mut app, "literal-1-pinned", &Action::ToggleCheck);

    app.harness()
        .expect(
            &SemanticExpectation::named("draft-problems")
                .name("Ready to save")
                .within(tilo_test_env::hang_bound()),
        )
        .expect("the draft checks");

    // Review what it may do, then save.
    act(&mut app, "review-save", &Action::Activate);
    app.harness()
        .wait_for_named("shortcuts-save-sheet", tilo_test_env::hang_bound())
        .expect("the sheet shows");
    // Keyboard activation of the reviewed primary action saves the grant.
    act(&mut app, "shortcuts-save-sheet.primary", &Action::Focus);
    app.harness().key("Enter");

    let created = host.library_calls("create");
    assert_eq!(created.len(), 1, "saved once through shortcuts#create");
    assert_eq!(
        created[0],
        json!({"shortcut": {
            "id": shortcut_id,
            "name": "Tidy",
            "inputs": [{
                "name": "who", "ty": "text", "required": true, "description": "who",
                "nullable": false
            }],
            "steps": [
                {"id": "list", "step": "invoke", "app_id": NOTES,
                 "resource": {"fixed": INBOX}, "action": "list"},
                {"id": "create", "step": "invoke", "app_id": NOTES,
                 "resource": {"fixed": INBOX}, "action": "create",
                 "args": {"pinned": {"literal": true}, "title": {"input": "who"}}}
            ],
            "grant": [
                rule("list", collab_types::Verb::Read),
                rule("create", collab_types::Verb::Write),
            ]
        }})
    );
}

/// The workspace also offers the agent's free-form transform.
fn with_agent() -> FakeHost {
    let mut host = FakeHost::new();
    host.rows.push(row(
        "9",
        tilo_automation::AGENT_APP_ID,
        "agent.transform:default",
        tilo_automation::AGENT_TRANSFORM_KIND,
        "freeform",
        vec![
            ActionParam::required("prompt", ParamType::Text, "Prompt"),
            ActionParam::required("input", ParamType::Text, "Input"),
            ActionParam::required("schema", ParamType::Object, "Schema"),
        ],
        PermissionRequirement::Write,
        ResultSpec::Json { schema: json!({}) },
    ));
    host
}

#[test]
fn drafting_is_hidden_without_the_agent() {
    let (_host, _guard, mut app) = open();
    app.harness()
        .wait_for_named("new-shortcut", tilo_test_env::hang_bound())
        .expect("the library shows");
    assert!(
        !app.harness().targets().contains_key("describe-shortcut"),
        "no agent, no drafting"
    );
}

#[test]
fn a_described_shortcut_is_drafted_checked_and_opened_but_never_saved() {
    let (host, _guard, mut app) = open_with(with_agent());
    act(
        &mut app,
        "describe-shortcut",
        &Action::SetValue("note what is in my inbox".into()),
    );
    act(&mut app, "draft-with-agent", &Action::Activate);
    app.harness()
        .expect(
            &SemanticExpectation::named("drafted-summary")
                .name_contains("Note the inbox")
                .within(tilo_test_env::hang_bound()),
        )
        .expect("the draft is offered");

    let asked: Vec<_> = host
        .invoked
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.target.address.action == "freeform")
        .cloned()
        .collect();
    assert_eq!(asked.len(), 1, "the agent was asked once");
    let InvokeArgument::Json(arguments) = &asked[0].argument else {
        panic!("typed arguments")
    };
    assert!(
        arguments["prompt"]
            .as_str()
            .unwrap()
            .ends_with("note what is in my inbox")
    );
    assert!(
        arguments["input"]
            .as_str()
            .unwrap()
            .contains("\"action\":\"create\""),
        "the agent sees the actions it may use"
    );

    act(&mut app, "open-drafted", &Action::Activate);
    // Let the open land.
    let _ = app
        .harness()
        .wait_until(Duration::from_millis(300), |_| false);
    let drafts: Vec<_> = app.state().drafts.values().cloned().collect();
    assert_eq!(drafts.len(), 1, "opened as a draft");
    let shortcut = drafts[0].shortcut(Vec::new());
    assert_eq!(shortcut.name, "Note the inbox");
    assert_eq!(shortcut.steps.len(), 2);
    assert!(
        host.library_calls("create").is_empty() && host.library_calls("update").is_empty(),
        "nothing is saved without the grant review"
    );
}

/// The workspace holds one saved shortcut that asks for a title.
fn with_saved_note() -> FakeHost {
    let mut host = FakeHost::new();
    host.rows.push(row(
        "workspace:room",
        APP_ID,
        &tilo_automation::shortcut_resource_id("quick-note"),
        "shortcuts.shortcut",
        tilo_automation::SHORTCUT_RUN_ACTION,
        Vec::new(),
        PermissionRequirement::Write,
        ResultSpec::None,
    ));
    host.saved.push(json!({
        "id": "quick-note",
        "name": "Quick note",
        "inputs": [{
            "name": "title", "ty": "text", "required": true,
            "description": "Note title", "nullable": false
        }],
        "steps": [
            {"id": "create", "step": "invoke", "app_id": NOTES,
             "resource": {"fixed": INBOX}, "action": "create",
             "args": {"title": {"input": "title"}, "pinned": {"literal": false}}}
        ]
    }));
    host
}

#[test]
fn a_saved_shortcut_opens_its_page_runs_with_its_inputs_and_a_run_leads_back() {
    let (host, _guard, mut app) = open_with(with_saved_note());
    // The library lists the saved shortcut under the overview: one step
    // down the list opens its page.
    app.harness()
        .wait_for_named("shortcuts-library", tilo_test_env::hang_bound())
        .expect("the library shows");
    let list = app.harness().targets()["shortcuts-library"];
    // A click in the first row (the overview) puts focus in the list.
    app.harness().click_at(list.x0 + 40.0, list.y0 + 16.0);
    // Each section header is a stop of its own (the empty Drafts says so
    // in a line the keys pass over); the next stop is the shortcut.
    app.harness().key("Down");
    app.harness().key("Down");
    app.harness().key("Down");
    act(
        &mut app,
        "run-quick-note-input-title",
        &Action::SetValue("Call Sam".into()),
    );
    act(&mut app, "run-quick-note", &Action::Activate);

    let _ = app
        .harness()
        .wait_until(Duration::from_millis(300), |_| false);
    let runs: Vec<_> = host
        .invoked
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.target.address.action == "run")
        .cloned()
        .collect();
    assert_eq!(runs.len(), 1, "run once");
    let InvokeArgument::Json(inputs) = &runs[0].argument else {
        panic!("typed inputs")
    };
    assert_eq!(inputs["title"], "Call Sam");

    // Back on the overview, the run is listed and leads to the shortcut.
    app.harness().key("Up");
    app.harness().key("Up");
    app.harness().key("Up");
    act(&mut app, "run-record-1", &Action::Activate);
    app.harness()
        .wait_for_named("run-quick-note", tilo_test_env::hang_bound())
        .expect("the run opens the shortcut it started");
}

#[test]
fn morning_refresh_draft_requires_schedule_choice_and_keyboard_save_review() {
    let mut fixture = FakeHost::new();
    let mut refresh = row(
        "weather",
        "so.tilo.weather",
        "weather.location:fixture",
        "weather.location",
        "refresh",
        Vec::new(),
        PermissionRequirement::Read,
        ResultSpec::Job,
    );
    refresh.resource.label = "Chosen weather".into();
    refresh.action.as_mut().unwrap().kind = OperationKind::Effect;
    refresh.action.as_mut().unwrap().resource_label = "Chosen weather".into();
    fixture.rows.push(refresh);
    let (host, _guard, mut app) = open_with(fixture);
    app.invoke(Workbench::DRAFT_SHORTCUT.with(json!({
        "id": "fixture-refresh", "name": "Refresh chosen weather", "steps": [{"id": "refresh", "step": "invoke", "app_id": "so.tilo.weather", "resource": {"fixed": "weather.location:fixture"}, "action": "refresh"}]
    }))).unwrap();
    act(&mut app, "schedule-enabled", &Action::Focus);
    assert!(host.library_calls("create").is_empty());
    assert!(
        app.state()
            .drafts
            .values()
            .next()
            .unwrap()
            .program()
            .triggers
            .is_empty()
    );
    act(&mut app, "schedule-enabled", &Action::ToggleCheck);
    act(&mut app, "schedule-hour", &Action::SetValue("09".into()));
    act(&mut app, "schedule-minute", &Action::SetValue("15".into()));
    let program = app.state().drafts.values().next().unwrap().program();
    assert_eq!(
        program.triggers,
        vec![tilo_automation::TriggerSpec::Schedule {
            schedule: tilo_automation::Schedule::Calendar {
                at: tilo_automation::TimeOfDay {
                    hour: 9,
                    minute: 15
                },
                weekdays: Vec::new()
            },
            catch_up: tilo_automation::CatchUp::RunOnce
        }]
    );
    assert!(
        host.library_calls("create").is_empty(),
        "schedule choice edits only the draft"
    );
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/morning-refresh-renders");
    std::fs::create_dir_all(&output).unwrap();
    render_morning_matrix(&mut app, &output, "draft");
    app.harness().set_theme(ui::Theme::light());
    app.harness().resize((1600, 1000)).unwrap();
    act(&mut app, "review-save", &Action::Activate);
    app.harness()
        .expect(
            &SemanticExpectation::named("schedule-review-0")
                .name(ui::i18n::I18nManager::format(
                    "shortcuts-schedule-review",
                    &[("time", ui::i18n::ArgValue::from("09:15"))],
                ))
                .within(tilo_test_env::hang_bound()),
        )
        .expect("the actual save review must disclose the chosen schedule");
    render_morning_matrix(&mut app, &output, "review");
    app.harness().set_theme(ui::Theme::light());
    app.harness().resize((1600, 1000)).unwrap();
    act(&mut app, "shortcuts-save-sheet.primary", &Action::Focus);
    app.harness().key("Enter");
    let saved = host.library_calls("create");
    assert_eq!(saved.len(), 1);
    let saved: tilo_automation::Shortcut =
        serde_json::from_value(saved[0]["shortcut"].clone()).unwrap();
    assert_eq!(saved.triggers, program.triggers);
    assert_eq!(
        saved.grant,
        vec![collab_types::PermissionProfileRule::grant(
            collab_types::Scope::Action {
                app: "so.tilo.weather".into(),
                resource: "weather.location:fixture".into(),
                action: "refresh".into()
            },
            collab_types::Verb::Read
        )]
    );
    assert!(saved.consents.is_empty());
}

fn render_morning_matrix(app: &mut TestApp<App>, output: &std::path::Path, page: &str) {
    for (name, theme) in [
        ("light", ui::Theme::light()),
        ("dark", ui::Theme::dark()),
        ("hc200", ui::Theme::high_contrast().with_text_scale(2.0)),
    ] {
        for (size, width, height) in [("normal", 1600, 1000), ("compact", 640, 900)] {
            app.harness().set_theme(theme.clone());
            app.harness().resize((width, height)).unwrap();
            app.harness()
                .render_png(&output.join(format!("{name}-{size}-{page}.png")));
        }
    }
}
