//! The editor driven through its accessible controls against a fake action
//! host: build a shortcut step by step, see the checker's verdict, review
//! the grant and save it through the door's `shortcuts#create`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::sync::Arc;

use serde_json::{Value, json};
use shortcuts::{APP_ID, App, Workbench};
use ui::a11y::Action;
use ui::headless::SemanticExpectation;
use ui::prelude::TestApp;
use ui::tilo_actions::remote::{
    ActionHost, Caller, CataloguePage, CatalogueResource, CatalogueRow, InvokeArgument, TestHost,
};
use ui::tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionKind, ActionOutput, ActionParam,
    ExecutionLocus, InputSpec, OperationKind, ParamType, ResultSpec,
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
    permission: ActionKind,
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
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: ExecutionLocus::default(),
            kind: OperationKind::Document,
            contract: ActionContract {
                kind: permission,
                result,
                ..ActionContract::default()
            },
            input: InputSpec::None,
            params,
            view: None,
        }),
        has_children: false,
        visibility: None,
    }
}

/// A workspace offering a notes app and the shortcut library.
struct Workspace {
    rows: Vec<CatalogueRow>,
    /// What `shortcuts#list` answers: the saved shortcuts.
    saved: Vec<Value>,
}

impl Workspace {
    fn new() -> Self {
        let library = |action: &str, params: Vec<ActionParam>| {
            row(
                "workspace:room",
                APP_ID,
                "shortcuts",
                "shortcuts.library",
                action,
                params,
                ActionKind::Undoable,
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
                    ActionKind::Read,
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
                    ActionKind::Undoable,
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
        }
    }

    /// The host serving this workspace: it records every invocation and
    /// answers each at once.
    fn host(self) -> TestHost {
        let Self { rows, saved } = self;
        TestHost::new()
            .with_catalogue(move |_, _| {
                Ok(CataloguePage {
                    revision: "1".into(),
                    rows: rows.clone(),
                    exhausted: true,
                    ..CataloguePage::default()
                })
            })
            .completing(move |request| {
                let value = match request.target.address.action.as_str() {
                    "list" if request.target.address.resource == "shortcuts" => {
                        Some(Value::Array(saved.clone()))
                    }
                    "list" => Some(json!([])),
                    "freeform" => Some(json!({
                        "name": "Note the inbox",
                        "steps": [
                            {"id": "list", "step": "invoke", "action": {"app": NOTES, "kind": "notes.folder", "action": "list"},
                             "resource": {"fixed": INBOX}},
                            {"id": "note", "step": "invoke", "action": {"app": NOTES, "kind": "notes.folder", "action": "create"},
                             "resource": {"fixed": INBOX},
                             "args": {"title": {"literal": "Inbox"}, "pinned": {"literal": false}}}
                        ],
                        "grant": [{"node": "**", "kinds": ["read", "undoable", "outside"], "answer": "allow"}]
                    })),
                    _ => None,
                };
                ActionOutput {
                    value,
                    resources: Vec::new(),
                }
            })
    }
}

/// The arguments of each call `host` was asked of the library's `action`.
fn library_calls(host: &TestHost, action: &str) -> Vec<Value> {
    host.asked()
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

fn rule(action: &str, kind: tilo_action_policy::ActionKind) -> Value {
    serde_json::to_value(
        tilo_automation::grant::Permission {
            app_id: NOTES.into(),
            reach: tilo_automation::grant::Reach::Resource(INBOX.into()),
            action: action.into(),
            kind,
            label: String::new(),
        }
        .rule(),
    )
    .unwrap()
}

fn open() -> (Arc<TestHost>, RemoteActionsGuard, TestApp<App>) {
    open_with(Workspace::new())
}

fn open_with(workspace: Workspace) -> (Arc<TestHost>, RemoteActionsGuard, TestApp<App>) {
    let host = Arc::new(workspace.host());
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

/// Press the window's action `name` in its bar, once the bar offers it.
fn press_bar(app: &mut TestApp<App>, name: &str) {
    app.settle();
    let offered = ui::window_toolbar::declared().is_some_and(|bar| {
        bar.actions
            .iter()
            .any(|action| action.name == name && action.enabled)
    });
    assert!(offered, "the window bar offers {name}");
    app.harness().press_window_action(name).unwrap();
    app.settle();
}

#[test]
fn the_app_starts_empty_and_reads_the_door_as_itself() {
    let (host, _guard, mut app) = open();
    assert!(app.state().drafts.is_empty(), "no seeded drafts");
    app.harness()
        .wait_for_named("shortcuts-library", tilo_test_env::hang_bound())
        .expect("the library shows");
    assert!(
        host.callers().iter().all(|caller| *caller.principal()
            == ui::tilo_actions::remote::CallerPrincipal::App(APP_ID.into())),
        "every request goes to the door as the Shortcuts app"
    );
    assert_eq!(library_calls(&host, "list").len(), 1, "the library is read");
    let audit = app.harness().native_app_audit();
    assert!(audit.issues.is_empty(), "{audit:#?}");
}

#[test]
fn the_window_bar_names_the_page_and_holds_its_actions() {
    let (_host, _guard, mut app) = open();
    app.harness()
        .wait_for_named("shortcuts-library", tilo_test_env::hang_bound())
        .expect("the library shows");
    let bar = |app: &mut TestApp<App>| {
        app.settle();
        let bar = ui::window_toolbar::declared().expect("the window declares its slots");
        let location = bar.location.map(|location| location.label);
        let actions: Vec<(String, bool)> = bar
            .actions
            .into_iter()
            .map(|action| (action.name, action.enabled))
            .collect();
        (location, actions)
    };
    let (location, actions) = bar(&mut app);
    assert_eq!(location.as_deref(), Some("Overview"));
    assert_eq!(
        actions,
        [
            ("New shortcut".to_owned(), true),
            ("Reload shortcuts".to_owned(), true)
        ]
    );

    press_bar(&mut app, "New shortcut");
    assert_eq!(app.state().drafts.len(), 1, "New shortcut starts a draft");
    let (location, actions) = bar(&mut app);
    assert_eq!(
        location.as_deref(),
        Some("New shortcut"),
        "the draft is open"
    );
    assert!(
        actions.contains(&("Review and save…".to_owned(), false)),
        "a draft without steps cannot be saved yet: {actions:?}"
    );
    press_bar(&mut app, "Discard draft");
    assert!(app.state().drafts.is_empty(), "Discard draft discards it");
}

#[test]
fn the_bar_duplicates_the_open_draft() {
    let (_host, _guard, mut app) = open();
    app.harness()
        .wait_for_named("shortcuts-library", tilo_test_env::hang_bound())
        .expect("the library shows");
    press_bar(&mut app, "New shortcut");
    press_bar(&mut app, "Duplicate");
    assert_eq!(app.state().drafts.len(), 2, "Duplicate starts a variant");
    assert!(
        app.state()
            .drafts
            .values()
            .any(|draft| draft.name == "New shortcut copy"),
        "the copy is named after the draft"
    );
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
        "duplicate-draft",
        "duplicate-shortcut",
        "draft-shortcut",
        "set-schedule",
        "add-step",
        "bind-argument",
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
    assert_eq!(
        effects,
        ["delete", "describe", "refine", "refresh", "run", "save"]
    );
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
    app.invoke(Workbench::NEW_DRAFT.with("Tidy"))
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
    press_bar(&mut app, "Review and save…");
    app.harness()
        .wait_for_named("shortcuts-save-sheet", tilo_test_env::hang_bound())
        .expect("the sheet shows");
    // Keyboard activation of the reviewed primary action saves the grant.
    act(&mut app, "shortcuts-save-sheet.primary", &Action::Focus);
    app.harness().key("Enter");

    let created = library_calls(&host, "create");
    assert_eq!(created.len(), 1, "saved once through shortcuts#create");
    assert_eq!(
        created[0],
        json!({"shortcut": {
            "id": shortcut_id,
            "name": "Tidy",
            "inputs": [{
                "name": "who", "ty": "text", "required": true, "description": "who"
            }],
            "steps": [
                {"id": "list", "step": "invoke", "action": {"app": NOTES, "kind": "notes.folder", "action": "list"},
                 "resource": {"fixed": INBOX}},
                {"id": "create", "step": "invoke", "action": {"app": NOTES, "kind": "notes.folder", "action": "create"},
                 "resource": {"fixed": INBOX},
                 "args": {"pinned": {"literal": true}, "title": {"input": "who"}}}
            ],
            "grant": [
                rule("list", tilo_action_policy::ActionKind::Read),
                rule("create", tilo_action_policy::ActionKind::Undoable),
            ]
        }})
    );
}

/// The workspace also offers the agent's free-form transform.
fn with_agent() -> Workspace {
    let mut host = Workspace::new();
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
        ActionKind::Undoable,
        ResultSpec::Json { schema: json!({}) },
    ));
    host
}

#[test]
fn drafting_is_hidden_without_the_agent() {
    let (_host, _guard, mut app) = open();
    app.harness()
        .wait_for_named("shortcuts-library", tilo_test_env::hang_bound())
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
        .asked()
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
    tilo_test_env::wait_until("the drafted shortcut to open", || {
        app.settle();
        !app.state().drafts.is_empty()
    });
    let drafts: Vec<_> = app.state().drafts.values().cloned().collect();
    assert_eq!(drafts.len(), 1, "opened as a draft");
    let shortcut = drafts[0].shortcut(Vec::new());
    assert_eq!(shortcut.name, "Note the inbox");
    assert_eq!(shortcut.steps.len(), 2);
    assert!(
        library_calls(&host, "create").is_empty() && library_calls(&host, "update").is_empty(),
        "nothing is saved without the grant review"
    );
}

/// The workspace holds one saved shortcut that asks for a title.
fn with_saved_note() -> Workspace {
    let mut host = Workspace::new();
    host.rows.push(row(
        "workspace:room",
        APP_ID,
        &tilo_automation::shortcut_resource_id("quick-note"),
        "shortcuts.shortcut",
        tilo_automation::SHORTCUT_RUN_ACTION,
        Vec::new(),
        ActionKind::Undoable,
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
            {"id": "create", "step": "invoke", "action": {"app": NOTES, "kind": "notes.folder", "action": "create"},
             "resource": {"fixed": INBOX},
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

    tilo_test_env::wait_until("the shortcut's run to be invoked", || {
        app.settle();
        host.asked()
            .iter()
            .any(|request| request.target.address.action == "run")
    });
    let runs: Vec<_> = host
        .asked()
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
    let mut fixture = Workspace::new();
    let mut refresh = row(
        "weather",
        "so.tilo.weather",
        "weather.location:fixture",
        "weather.location",
        "refresh",
        Vec::new(),
        ActionKind::Read,
        ResultSpec::Job,
    );
    refresh.resource.label = "Chosen weather".into();
    refresh.action.as_mut().unwrap().kind = OperationKind::Effect;
    refresh.action.as_mut().unwrap().resource_label = "Chosen weather".into();
    fixture.rows.push(refresh);
    let (host, _guard, mut app) = open_with(fixture);
    app.invoke(Workbench::DRAFT_SHORTCUT.with(json!({
        "id": "fixture-refresh", "name": "Refresh chosen weather", "steps": [{"id": "refresh", "step": "invoke", "action": {"app": "so.tilo.weather", "kind": "weather.location", "action": "refresh"}, "resource": {"fixed": "weather.location:fixture"}}]
    }))).unwrap();
    act(&mut app, "schedule-enabled", &Action::Focus);
    assert!(library_calls(&host, "create").is_empty());
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
        library_calls(&host, "create").is_empty(),
        "schedule choice edits only the draft"
    );
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/morning-refresh-renders");
    std::fs::create_dir_all(&output).unwrap();
    render_morning_matrix(&mut app, &output, "draft");
    app.harness().set_theme(ui::Theme::light());
    app.harness().resize((1600, 1000)).unwrap();
    press_bar(&mut app, "Review and save…");
    app.harness()
        .expect(
            &SemanticExpectation::named("schedule-review-0")
                .name(ui::i18n::I18nManager::format(
                    "shortcuts-schedule-review",
                    &[(
                        "time",
                        ui::i18n::ArgValue::from(ui::clock::hour_minute(9, 15)),
                    )],
                ))
                .within(tilo_test_env::hang_bound()),
        )
        .expect("the actual save review must disclose the chosen schedule");
    render_morning_matrix(&mut app, &output, "review");
    app.harness().set_theme(ui::Theme::light());
    app.harness().resize((1600, 1000)).unwrap();
    act(&mut app, "shortcuts-save-sheet.primary", &Action::Focus);
    app.harness().key("Enter");
    let saved = library_calls(&host, "create");
    assert_eq!(saved.len(), 1);
    let saved: tilo_automation::Shortcut =
        serde_json::from_value(saved[0]["shortcut"].clone()).unwrap();
    assert_eq!(saved.triggers, program.triggers);
    assert_eq!(
        saved.grant,
        vec![
            tilo_automation::grant::Permission {
                app_id: "so.tilo.weather".into(),
                reach: tilo_automation::grant::Reach::Resource("weather.location:fixture".into()),
                action: "refresh".into(),
                kind: ActionKind::Read,
                label: String::new(),
            }
            .rule()
        ]
    );
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
            if page == "review" {
                let snapshot = app.harness().inspection();
                let paragraph = inspection_node(&snapshot.root, "shortcuts-grant-explain")
                    .expect("the painted save review must expose its full explanation");
                let body_width = ui::widget::modal_dialog::body_width(
                    ui::kurbo::Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
                    560.0,
                    &theme,
                );
                assert!(
                    paragraph.rect[2] <= body_width + 1.0,
                    "{name}/{size}: explanation paints beyond the review body: {:?}, body width {body_width}",
                    paragraph.rect,
                );
                assert_eq!(
                    paragraph
                        .semantics
                        .as_ref()
                        .and_then(|node| node.name.as_deref()),
                    Some(ui::i18n!("shortcuts-grant-explain").as_str()),
                    "wrapping must retain the complete accessible explanation",
                );
            }
        }
    }
}

fn inspection_node<'a>(
    node: &'a ui::headless::InspectionNode,
    id: &str,
) -> Option<&'a ui::headless::InspectionNode> {
    if node.test_id.as_deref() == Some(id) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| inspection_node(child, id))
}

/// The save review tells a first-time user what saving agrees to: the
/// shortcut may use only the app actions listed, and a shortcut that lists
/// none says plainly that it can't touch their apps.
#[test]
fn the_save_review_says_what_saving_allows() {
    let (_host, _guard, mut app) = open();
    app.invoke(Workbench::DRAFT_SHORTCUT.with(json!({
        "id": "no-actions", "name": "Only a branch", "steps": [{"id": "branch", "step": "if", "cond": {"truthy": {"literal": true}}, "then": []}]
    })))
    .unwrap();
    press_bar(&mut app, "Review and save…");
    let explain = ui::i18n!("shortcuts-grant-explain");
    assert!(
        explain.contains("app actions listed") && explain.contains("person running it"),
        "{explain}"
    );
    let none = ui::i18n!("shortcuts-grant-none");
    assert!(none.contains("can't read or change"), "{none}");
    for (id, text) in [
        ("shortcuts-grant-explain", explain),
        ("shortcuts-grant-none", none),
    ] {
        app.harness()
            .expect(
                &SemanticExpectation::named(id)
                    .name(text)
                    .within(tilo_test_env::hang_bound()),
            )
            .unwrap_or_else(|error| panic!("the review shows {id}: {error}"));
    }
}

/// The window bar's sidebar button flips the sidebar on every click, in a
/// window wide enough to dock it and in a narrow one.
#[test]
fn the_window_bars_sidebar_button_flips_the_sidebar() {
    ui::framework::testing::check_window_sidebar_button(
        ui::framework::testing::TestApp::<App>::new,
    )
    .unwrap_or_else(|error| panic!("{error}"));
}

/// A draft with an `if` holding the list step in its `then` block, built the
/// way a person does: three steps from the catalogue, one of them a
/// condition, the last moved inside it.
fn draft_with_a_condition() -> (Arc<TestHost>, RemoteActionsGuard, TestApp<App>) {
    let (host, guard, mut app) = open();
    app.invoke(Workbench::NEW_DRAFT.with("Tidy"))
        .expect("a new draft");
    app.harness()
        .wait_for_named("draft-name", tilo_test_env::hang_bound())
        .expect("the new draft opens in the editor");
    act(&mut app, "add-action-top", &Action::Activate);
    act(&mut app, "offer-if", &Action::Activate);
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
    (host, guard, app)
}

#[test]
fn a_condition_holds_steps_and_a_step_folds_away() {
    let (_host, _guard, mut app) = draft_with_a_condition();
    let steps = |app: &mut TestApp<App>| {
        app.state()
            .drafts
            .values()
            .next()
            .expect("the draft")
            .program()
            .steps
    };
    assert_eq!(steps(&mut app).len(), 3, "three steps at the top level");

    // The third step moves into the condition's `then` block, where it is
    // no longer one of the top level's steps.
    let draft_id = app.state().drafts.ids().next().expect("the draft").clone();
    app.invoke(Workbench::SET_NESTED.with((draft_id, "2".to_owned(), "0.then".to_owned(), 0)))
        .expect("the step moves into the condition");
    let top = steps(&mut app);
    assert_eq!(top.len(), 2, "only the condition is left at the top");
    let tilo_automation::StepKind::If { then, .. } = &top[0].kind else {
        panic!("the first step is the condition: {:?}", top[0].kind);
    };
    assert_eq!(then.len(), 1, "the moved step is what the condition holds");

    // A step keeps its place in the program while it is folded: only its
    // drawing goes.
    let before = steps(&mut app);
    act(&mut app, "step-fold-0", &Action::Activate);
    assert_eq!(steps(&mut app), before, "folding changes no step");
    app.harness()
        .expect(&SemanticExpectation::named("step-open-0").within(tilo_test_env::hang_bound()))
        .expect("a folded step offers to open again");
    assert!(
        !app.harness().targets().contains_key("step-fold-0"),
        "and no longer offers to fold"
    );
    act(&mut app, "step-open-0", &Action::Activate);
    act(&mut app, "step-fold-0", &Action::Activate);
    assert!(app.harness().targets().contains_key("step-open-0"));
    assert_eq!(
        steps(&mut app),
        before,
        "opening and folding change nothing"
    );
}

#[test]
fn the_text_view_shows_the_same_steps_in_the_same_order() {
    let (_host, _guard, mut app) = draft_with_a_condition();
    // A step inside the condition, so the text has a block to indent.
    let draft_id = app.state().drafts.ids().next().expect("the draft").clone();
    app.invoke(Workbench::SET_NESTED.with((draft_id, "2".to_owned(), "0.then".to_owned(), 0)))
        .expect("the step moves into the condition");
    let draft = app
        .state()
        .drafts
        .values()
        .next()
        .expect("the draft")
        .clone();
    let program = draft.program();

    press_bar(&mut app, "Show program text");
    app.harness()
        .wait_for_named("draft-text", tilo_test_env::hang_bound())
        .expect("the program text shows");
    let snapshot = app.harness().inspection();
    let text = inspection_node(&snapshot.root, "draft-text")
        .and_then(|node| node.semantics.as_ref())
        .and_then(|node| node.name.clone())
        .expect("the text view draws its text");

    // Every step the blocks hold is a line of the text, numbered by its
    // position, and the condition's block is indented under it.
    for number in 1..=program.steps.len() {
        let numbered = format!("{number}. ");
        assert!(
            text.lines()
                .any(|line| line.trim_start().starts_with(&numbered)),
            "step {number} is in the text: {text}"
        );
    }
    assert!(
        text.contains("then:") && text.contains("otherwise:"),
        "the condition names both its blocks: {text}"
    );
    let indented = text
        .lines()
        .any(|line| line.starts_with("    ") && line.trim_start().starts_with(char::is_numeric));
    assert!(indented, "a block's steps are indented under it: {text}");
    // The text names each step by what it does, in the same order the
    // blocks show, and the nested step is under its block.
    let order: Vec<usize> = ["If", "create", "list"]
        .iter()
        .map(|substance| {
            text.find(substance).unwrap_or_else(|| {
                panic!("{substance} is in the text, in order: {text}");
            })
        })
        .collect();
    assert!(
        order.is_sorted(),
        "the text keeps the program's order: {text}"
    );
    assert!(
        text.contains("    1. create on notes.folder:inbox"),
        "the condition's step is indented under it: {text}"
    );

    // Going back to the blocks keeps the draft exactly as it was.
    press_bar(&mut app, "Show blocks");
    let state = app.state();
    let back = state.drafts.values().next().expect("the draft");
    assert_eq!(back.program(), program, "the text view edits nothing");
    assert!(
        !app.harness().targets().contains_key("draft-text"),
        "the text is gone"
    );
}

/// The header of a step moves it into the step above, and out again, without
/// anyone typing an address or a piece of JSON.
#[test]
fn a_step_nests_into_the_one_above_it_and_lifts_back_out() {
    let (_host, _guard, mut app) = draft_with_a_condition();
    let program = |app: &mut TestApp<App>| {
        app.state()
            .drafts
            .values()
            .next()
            .expect("the draft")
            .program()
    };
    let top = program(&mut app);
    assert_eq!(top.steps.len(), 3, "three steps at the top level");
    // Every step shows both nest controls, so they keep their place in its
    // header; one with nowhere to go is disabled.
    app.harness()
        .expect(&SemanticExpectation::named("step-nest-0").enabled(false))
        .expect("the first step has nothing above it to move into");
    app.harness()
        .expect(&SemanticExpectation::named("step-nest-1").enabled(true))
        .expect("the second step can move into the condition");
    app.harness()
        .expect(&SemanticExpectation::named("step-lift-1").enabled(false))
        .expect("and it is already at the top level");

    act(&mut app, "step-nest-1", &Action::Activate);
    let nested = program(&mut app);
    assert_eq!(nested.steps.len(), 2, "the condition and the last step");
    let tilo_automation::StepKind::If { then, .. } = &nested.steps[0].kind else {
        panic!(
            "the first step is the condition: {:?}",
            nested.steps[0].kind
        );
    };
    assert_eq!(
        then.iter().map(|step| step.id.clone()).collect::<Vec<_>>(),
        vec![top.steps[1].id.clone()],
        "the step it held above is now inside the condition"
    );

    act(&mut app, "step-lift-0.then.0", &Action::Activate);
    let lifted = program(&mut app);
    assert_eq!(lifted.steps.len(), 3, "the step came back out");
    assert_eq!(
        lifted
            .steps
            .iter()
            .map(|step| step.id.clone())
            .collect::<Vec<_>>(),
        top.steps
            .iter()
            .map(|step| step.id.clone())
            .collect::<Vec<_>>(),
        "and stands where it stood, after the condition"
    );
}

/// The steps' permissions are on the page while the shortcut is being built,
/// not only in the sheet that asks before saving it.
#[test]
fn the_grant_summary_stays_on_the_page_as_steps_are_added() {
    let (_host, _guard, mut app) = open();
    app.invoke(Workbench::NEW_DRAFT.with("Tidy"))
        .expect("a new draft");
    app.harness()
        .wait_for_named("draft-name", tilo_test_env::hang_bound())
        .expect("the new draft opens in the editor");
    let none = ui::i18n!("shortcuts-grant-none");
    app.harness()
        .expect(
            &SemanticExpectation::named("shortcuts-grant-summary-none")
                .name(none)
                .within(tilo_test_env::hang_bound()),
        )
        .expect("an empty shortcut is allowed to do nothing");

    act(&mut app, "add-action-top", &Action::Activate);
    act(
        &mut app,
        &format!("offer-{NOTES}-{INBOX}-list"),
        &Action::Activate,
    );
    // The list step needs to read the notes app's Inbox, and the summary
    // says so without waiting for the save sheet.
    app.harness()
        .expect(&SemanticExpectation::named("grant-summary-0").within(tilo_test_env::hang_bound()))
        .expect("the summary lists what the step needs");
    let inspection = app.harness().inspection();
    let line = inspection_node(&inspection.root, "grant-summary-0")
        .and_then(|node| node.semantics.as_ref())
        .and_then(|semantics| semantics.name.clone())
        .expect("the line says what the step is allowed to do");
    assert!(
        line.contains("Inbox"),
        "the summary names the folder the step reads, as the save sheet does: {line}"
    );
    assert!(
        !app.harness()
            .targets()
            .contains_key("shortcuts-grant-summary-none"),
        "and no longer says it does nothing"
    );
}
