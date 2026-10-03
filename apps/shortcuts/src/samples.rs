//! Sample content for screenshots only (`--shots`). A person's window starts
//! empty: nothing here runs on the live path.

use serde_json::{Value, json};
use tilo_automation::{
    AGENT_APP_ID, AGENT_SUMMARISE_ACTION, AGENT_TRANSFORM_KIND, ActionCatalog, SHORTCUTS_APP_ID,
    Shortcut,
};
use ui::LocalState;
use ui::prelude::*;
use ui::tilo_actions::remote::{CatalogueResource, CatalogueRow};
use ui::tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionParam, ExecutionLocus, InputSpec,
    OperationKind, ParamType, PermissionRequirement, ResultSpec,
};

use crate::door::{DoorState, Loaded, RunRecord, RunStatus};
use crate::model::Workbench;

/// The `--shots` scenarios.
pub const SCENARIOS: &[&str] = &["library", "editor", "review"];

const NOTES: &str = "so.tilo.notes";
const MAIL: &str = "so.tilo.mail";

fn resource(id: &str, kind: &str, label: &str) -> CatalogueResource {
    CatalogueResource {
        id: id.into(),
        kind: kind.into(),
        label: label.into(),
        presentation: None,
    }
}

fn contract(permission: PermissionRequirement, result: ResultSpec) -> ActionContract {
    ActionContract {
        permission,
        result,
        ..ActionContract::default()
    }
}

/// One document action, `(id, label)`, offered on `resource` by `app`.
fn row(
    app: &str,
    resource: CatalogueResource,
    (action, action_label): (&str, &str),
    params: Vec<ActionParam>,
    contract: ActionContract,
) -> CatalogueRow {
    CatalogueRow {
        provider: format!("sample:{app}"),
        app_id: app.into(),
        parent_resource: String::new(),
        action: Some(ActionDescriptor {
            address: ActionAddress::new(&resource.id, action),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            action_label: action_label.into(),
            description: None,
            locus: ExecutionLocus::default(),
            kind: OperationKind::Document,
            contract,
            input: InputSpec::None,
            params,
            view: None,
        }),
        resource,
        has_children: false,
        visibility: None,
    }
}

/// A catalogue like the one a workspace with notes and mail offers.
#[must_use]
pub fn catalog() -> ActionCatalog {
    let titles = ResultSpec::Json {
        schema: json!({
            "type": "object",
            "properties": {"titles": {"type": "array", "items": {"type": "string"}}},
            "additionalProperties": false
        }),
    };
    ActionCatalog::new(vec![
        row(
            NOTES,
            resource("notes.folder:inbox", "notes.folder", "Inbox"),
            ("list", "List notes"),
            Vec::new(),
            contract(PermissionRequirement::Read, titles),
        ),
        row(
            NOTES,
            resource("notes.folder:inbox", "notes.folder", "Inbox"),
            ("create", "New note"),
            vec![
                ActionParam::required("title", ParamType::Text, "Title"),
                ActionParam::required("pinned", ParamType::Boolean, "Pin it"),
            ],
            contract(PermissionRequirement::Write, ResultSpec::None),
        ),
        row(
            MAIL,
            resource("mail.outbox", "mail.outbox", "Outbox"),
            ("send", "Send message"),
            vec![
                ActionParam::required("to", ParamType::Text, "Recipient"),
                ActionParam::required("body", ParamType::Text, "Message"),
            ],
            contract(PermissionRequirement::Write, ResultSpec::None),
        ),
        row(
            AGENT_APP_ID,
            resource("agent.transform:text", AGENT_TRANSFORM_KIND, "Text"),
            (AGENT_SUMMARISE_ACTION, "Summarise"),
            vec![ActionParam::required("input", ParamType::Text, "Text")],
            contract(
                PermissionRequirement::Read,
                ResultSpec::Json {
                    schema: json!({"type": "string"}),
                },
            ),
        ),
        row(
            SHORTCUTS_APP_ID,
            resource("shortcuts", "shortcuts.library", "Shortcuts"),
            ("list", "List shortcuts"),
            Vec::new(),
            contract(PermissionRequirement::Read, ResultSpec::None),
        ),
    ])
}

fn morning_digest() -> Value {
    json!({
        "id": "morning-digest",
        "name": "Morning digest",
        "steps": [
            {"id": "list", "step": "invoke", "app_id": NOTES,
             "resource": {"fixed": "notes.folder:inbox"}, "action": "list"},
            {"id": "ai", "step": "ai", "transform": "summarise",
             "input": {"template": "{{step.list/titles}}"}},
            {"id": "send", "step": "invoke", "app_id": MAIL,
             "resource": {"fixed": "mail.outbox"}, "action": "send",
             "args": {"to": {"literal": "me@example.org"}, "body": {"step_output": {"step": "ai"}}}}
        ]
    })
}

/// The drafts a scenario starts with.
#[must_use]
pub fn drafts(name: Option<&str>) -> Vec<Invoke<Workbench>> {
    match name {
        Some("editor" | "review") => vec![Workbench::OPEN_SHORTCUT.with(morning_digest())],
        _ => Vec::new(),
    }
}

/// Stage a scenario's view: a library, a catalogue and recent runs as a
/// workspace would have them, and the first draft open.
pub fn stage(
    cx: &ViewCx<Workbench>,
    door: &LocalState<DoorState>,
    selected: &LocalState<Option<String>>,
    narrow: &LocalState<NarrowScreen>,
    sheet: &LocalState<bool>,
) {
    let door = door.clone();
    let selected = selected.clone();
    let narrow = narrow.clone();
    let sheet = sheet.clone();
    let live = cx.live().clone();
    cx.on_stage(move |scenario| {
        let saved: Vec<Shortcut> = [
            morning_digest(),
            json!({
                "id": "tidy-inbox", "name": "Tidy the inbox", "steps": []
            }),
        ]
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect();
        door.set(DoorState {
            catalogue: Loaded::Ready(catalog()),
            library: Loaded::Ready(saved),
            runs: vec![RunRecord {
                serial: 1,
                shortcut_id: "morning-digest".into(),
                name: "Morning digest".into(),
                status: RunStatus::Completed(None),
            }],
            notice: None,
            drafted: None,
        });
        if matches!(scenario, Some("editor" | "review")) {
            let first = live.with(|workbench| {
                workbench
                    .drafts
                    .ids()
                    .next()
                    .map(|id| id.as_str().to_owned())
            });
            selected.set(first);
            narrow.set(NarrowScreen::Content);
            sheet.set(scenario == Some("review"));
        }
    });
}
