//! Sample content for screenshots only (`--shots`). A person's window starts
//! empty: nothing here runs on the live path.

use serde_json::{Value, json};
use tilo_automation::{
    AGENT_APP_ID, AGENT_FREEFORM_ACTION, AGENT_SUMMARISE_ACTION, AGENT_TRANSFORM_KIND,
    ActionCatalog, SHORTCUTS_APP_ID, Shortcut,
};
use ui::LocalState;
use ui::prelude::*;
use ui::tilo_actions::remote::{CatalogueResource, CatalogueRow};
use ui::tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionKind, ActionParam, ExecutionLocus,
    InputSpec, OperationKind, ParamType, ResultSpec,
};

use crate::door::{DoorState, Drafting, RunRecord, RunStatus};
use crate::drafting::Drafted;
use crate::model::Workbench;
use crate::view;
use ui::Arrival;

/// The `--shots` scenarios.
pub const SCENARIOS: &[&str] = &[
    "library", "editor", "review", "saved", "empty", "blocks", "drafted", "picker",
];

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

fn contract(kind: ActionKind, result: ResultSpec) -> ActionContract {
    ActionContract {
        kind,
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
            semantic_identity: None,
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
            contract(ActionKind::Read, titles),
        ),
        row(
            NOTES,
            resource("notes.folder:inbox", "notes.folder", "Inbox"),
            ("create", "New note"),
            vec![
                ActionParam::required("title", ParamType::Text, "Title"),
                ActionParam::required("pinned", ParamType::Boolean, "Pin it"),
            ],
            contract(ActionKind::Undoable, ResultSpec::None),
        ),
        row(
            MAIL,
            resource("mail.outbox", "mail.outbox", "Outbox"),
            ("send", "Send message"),
            vec![
                ActionParam::required("to", ParamType::Text, "Recipient"),
                ActionParam::required("body", ParamType::Text, "Message"),
            ],
            contract(ActionKind::Undoable, ResultSpec::None),
        ),
        row(
            AGENT_APP_ID,
            resource("agent.transform:text", AGENT_TRANSFORM_KIND, "Text"),
            (AGENT_SUMMARISE_ACTION, "Summarise"),
            vec![ActionParam::required("input", ParamType::Text, "Text")],
            contract(
                ActionKind::Read,
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
            contract(ActionKind::Read, ResultSpec::None),
        ),
    ])
}

/// The catalogue a captured window shows: [`catalog`] and the agent's
/// free-form transform, so the window offers to draft a shortcut.
fn staged_catalog() -> ActionCatalog {
    let mut rows = catalog().rows().to_vec();
    rows.push(row(
        AGENT_APP_ID,
        resource("agent.transform:text", AGENT_TRANSFORM_KIND, "Text"),
        (AGENT_FREEFORM_ACTION, "Ask the agent"),
        vec![
            ActionParam::required("prompt", ParamType::Text, "Prompt"),
            ActionParam::required("input", ParamType::Text, "Text"),
            ActionParam::required("schema", ParamType::Object, "Shape of the answer"),
        ],
        contract(ActionKind::Read, ResultSpec::Json { schema: json!({}) }),
    ));
    ActionCatalog::new(rows)
}

fn morning_digest() -> Value {
    json!({
        "id": "morning-digest",
        "name": "Morning digest",
        "steps": [
            {"id": "list", "step": "invoke", "action": {"app": NOTES, "kind": "notes.folder", "action": "list"},
             "resource": {"fixed": "notes.folder:inbox"}},
            {"id": "ai", "step": "ai", "transform": "summarise",
             "input": {"template": "{{step.list/titles}}"}},
            {"id": "send", "step": "invoke", "action": {"app": MAIL, "kind": "mail.outbox", "action": "send"},
             "resource": {"fixed": "mail.outbox"},
             "args": {"to": {"literal": "me@example.org"}, "body": {"step_output": {"step": "ai"}}}}
        ]
    })
}

/// A draft being worked on: a shortcut no one has saved yet.
fn weekly_summary() -> Value {
    json!({
        "id": "weekly-summary",
        "name": "Weekly summary",
        "steps": [
            {"id": "list", "step": "invoke", "action": {"app": NOTES, "kind": "notes.folder", "action": "list"},
             "resource": {"fixed": "notes.folder:inbox"}},
            {"id": "ai", "step": "ai", "transform": "summarise",
             "input": {"template": "{{step.list/titles}}"}},
            {"id": "send", "step": "invoke", "action": {"app": MAIL, "kind": "mail.outbox", "action": "send"},
             "resource": {"fixed": "mail.outbox"},
             "args": {"to": {"literal": "me@example.org"}, "body": {"step_output": {"step": "ai"}}}}
        ]
    })
}

fn quick_note() -> Value {
    json!({
        "id": "quick-note",
        "name": "Quick note",
        "inputs": [{
            "name": "title", "ty": "text", "required": true,
            "description": "Note title", "nullable": false
        }],
        "steps": [
            {"id": "create", "step": "invoke", "action": {"app": NOTES, "kind": "notes.folder", "action": "create"},
             "resource": {"fixed": "notes.folder:inbox"},
             "args": {"title": {"input": "title"}, "pinned": {"literal": false}}}
        ]
    })
}

fn mail_me_my_notes() -> Value {
    json!({
        "id": "mail-me-my-notes",
        "name": "Mail me my notes",
        "steps": [
            {"id": "list", "step": "invoke", "action": {"app": NOTES, "kind": "notes.folder", "action": "list"},
             "resource": {"fixed": "notes.folder:inbox"}},
            {"id": "send", "step": "invoke", "action": {"app": MAIL, "kind": "mail.outbox", "action": "send"},
             "resource": {"fixed": "mail.outbox"},
             "args": {"to": {"literal": "me@example.org"},
                      "body": {"template": "Today's notes: {{step.list/titles}}"}}}
        ]
    })
}

/// What the agent drafted from a description: a complete shortcut, offered
/// to open as a draft.
fn drafted() -> Option<Drafting> {
    let shortcut = json!({
        "id": "note-the-inbox",
        "name": "Note the inbox",
        "steps": [
            {"id": "list", "step": "invoke", "action": {"app": NOTES, "kind": "notes.folder", "action": "list"},
             "resource": {"fixed": "notes.folder:inbox"}},
            {"id": "create", "step": "invoke", "action": {"app": NOTES, "kind": "notes.folder", "action": "create"},
             "resource": {"fixed": "notes.folder:inbox"},
             "args": {"title": {"literal": "Inbox today"}, "pinned": {"literal": false}}}
        ]
    });
    let shortcut = serde_json::from_value(shortcut).ok()?;
    Some(Drafting::Ready(Box::new(Drafted {
        shortcut,
        problems: Vec::new(),
    })))
}

/// A shortcut with a repeat and a condition.
fn tidy_notes() -> Value {
    json!({
        "id": "tidy-notes",
        "name": "Tidy notes",
        "inputs": [{
            "name": "keyword", "ty": "text", "required": true,
            "description": "Keyword", "nullable": false
        }],
        "steps": [
            {"id": "list", "step": "invoke", "action": {"app": NOTES, "kind": "notes.folder", "action": "list"},
             "resource": {"fixed": "notes.folder:inbox"}},
            {"id": "each", "step": "for_each",
             "items": {"step_output": {"step": "list", "pointer": "/titles"}},
             "item": "title",
             "body": [
                {"id": "create", "step": "invoke", "action": {"app": NOTES, "kind": "notes.folder", "action": "create"},
                 "resource": {"fixed": "notes.folder:inbox"},
                 "args": {"title": {"var": "title"}, "pinned": {"literal": false}}}
             ]},
            {"id": "if", "step": "if", "cond": {"truthy": {"input": "keyword"}},
             "then": [
                {"id": "ai", "step": "ai", "transform": "summarise",
                 "input": {"input": "keyword"}}
             ],
             "else": []}
        ]
    })
}

/// The drafts a scenario starts with.
#[must_use]
pub fn drafts(name: Option<&str>) -> Vec<Invoke<Workbench>> {
    let weekly = Workbench::NEW_DRAFT.with(String::from("Standup notes"));
    match name {
        Some("editor" | "review") => vec![weekly, Workbench::OPEN_SHORTCUT.with(weekly_summary())],
        Some("blocks" | "picker") => vec![Workbench::OPEN_SHORTCUT.with(tidy_notes())],
        Some("library" | "saved") => vec![weekly],
        _ => Vec::new(),
    }
}

/// Stage a scenario's view: a library, a catalogue and recent runs as a
/// workspace would have them, and the page the scenario shows open.
pub fn stage(
    cx: &ViewCx<Workbench>,
    door: &LocalState<DoorState>,
    selected: &LocalState<Option<String>>,
    pane: &LocalState<NavPane>,
    sheet: &LocalState<bool>,
    picker: &LocalState<Option<String>>,
) {
    let door = door.clone();
    let selected = selected.clone();
    let pane = pane.clone();
    let sheet = sheet.clone();
    let picker = picker.clone();
    let live = cx.live().clone();
    cx.on_stage(move |scenario| {
        let saved: Vec<Shortcut> = [morning_digest(), quick_note(), mail_me_my_notes()]
            .into_iter()
            .filter_map(|value| serde_json::from_value(value).ok())
            .collect();
        let first_run = scenario == Some("empty");
        let run = |serial, id: &str, name: &str, status, started: &str| RunRecord {
            serial,
            shortcut_id: id.into(),
            name: name.into(),
            status,
            started: started.into(),
        };
        door.set(DoorState {
            catalogue: Arrival::Ready(staged_catalog()),
            library: Arrival::Ready(if first_run { Vec::new() } else { saved }),
            runs: if first_run {
                Vec::new()
            } else {
                vec![
                    run(
                        3,
                        "morning-digest",
                        "Morning digest",
                        RunStatus::Completed(Some(json!("Three new notes, one needs a reply"))),
                        "10:31",
                    ),
                    run(
                        2,
                        "quick-note",
                        "Quick note",
                        RunStatus::Completed(None),
                        "09:58",
                    ),
                    run(
                        1,
                        "mail-me-my-notes",
                        "Mail me my notes",
                        RunStatus::Failed("the outbox is not reachable".into()),
                        "08:12",
                    ),
                ]
            },
            notice: None,
            drafted: if scenario == Some("drafted") {
                drafted()
            } else {
                None
            },
        });
        let draft = |name: &str| {
            live.with(|workbench| {
                workbench
                    .drafts
                    .iter()
                    .find(|(_, draft)| draft.name == name)
                    .map(|(id, _)| format!("{}{}", view::DRAFT, id.as_str()))
            })
        };
        let open = match scenario {
            Some("editor" | "review") => draft("Weekly summary"),
            Some("blocks" | "picker") => draft("Tidy notes"),
            Some("saved") => Some(format!("{}quick-note", view::SAVED)),
            _ => None,
        };
        // The overview is where a window starts; the sidebar is the first
        // screen of a narrow one.
        pane.set(if open.is_some() {
            NavPane::Content
        } else {
            NavPane::Sidebar
        });
        selected.set(Some(open.unwrap_or_else(|| view::OVERVIEW.to_owned())));
        sheet.set(scenario == Some("review"));
        // The picker of the top-level block, open.
        picker.set((scenario == Some("picker")).then(String::new));
    });
}
