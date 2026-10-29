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

use crate::door::{DoorState, Drafting, Refined, RunRecord, RunStatus};
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
            state: ui::tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(&resource.id, action),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            action_label: action_label.into(),
            keywords: Vec::new(),
            icon: None,
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
            {"id": "ai", "step": "ai", "prompt": "Write a short digest of these titles",
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
            {"id": "ai", "step": "ai", "prompt": "Write a short digest of these titles",
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
    Some(Drafting::Ready(Box::new(Refined {
        drafted: Drafted {
            shortcut,
            problems: Vec::new(),
        },
        changes: Vec::new(),
    })))
}

/// Repeated work the app noticed: the same two steps, three times in a row.
fn proposal() -> crate::proposal::Proposal {
    crate::proposal::Proposal {
        steps: vec![
            occurrence("list", "List notes in Inbox"),
            occurrence("create", "New note"),
        ],
        times: 3,
        resources: Vec::new(),
    }
}

/// One action the person took, as a catalogue names it.
fn occurrence(action: &str, label: &str) -> crate::proposal::Occurrence {
    crate::proposal::Occurrence {
        app: NOTES.into(),
        kind: "notes.folder".into(),
        action: action.into(),
        resource: Some("notes.folder:inbox".into()),
        label: Some(label.into()),
    }
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
                {"id": "ai", "step": "ai", "prompt": "Summarise this",
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
                        RunStatus::Completed {
                            value: Some(json!("Three new notes, one needs a reply")),
                            degraded: None,
                        },
                        "10:31",
                    ),
                    run(
                        2,
                        "quick-note",
                        "Quick note",
                        RunStatus::Completed {
                            value: None,
                            degraded: None,
                        },
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
            proposal: (scenario == Some("repeat")).then(proposal),
            feedback: String::new(),
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

#[cfg(test)]
mod roundtrip_tests {
    use std::collections::BTreeMap;

    use tilo_automation::text::{self, Shared};

    use super::*;

    /// Every sample program the app ships: the ones its scenarios stage.
    fn programs() -> Vec<(&'static str, Value)> {
        vec![
            ("morning digest", morning_digest()),
            ("weekly summary", weekly_summary()),
            ("quick note", quick_note()),
            ("mail me my notes", mail_me_my_notes()),
            ("tidy notes", tidy_notes()),
        ]
    }

    /// A sample reads back as the program it states, and its text is the text
    /// that program writes: the two views agree on the whole corpus, not only
    /// on the shapes a generated program covers.
    #[test]
    fn every_sample_shortcut_reads_back_as_itself() {
        let catalog = catalog();
        let shared = Shared {
            catalog: &catalog,
            app_label: &|app_id: &str| app_id.rsplit('.').next().unwrap_or(app_id).to_owned(),
        };
        for (label, value) in programs() {
            let shortcut: Shortcut =
                serde_json::from_value(value).unwrap_or_else(|error| panic!("{label}: {error}"));
            let (written, _) = text::render(&shared, &shortcut, &BTreeMap::new())
                .unwrap_or_else(|error| panic!("{label}: {error}\n{shortcut:?}"));
            let edit = text::edit(&shared, &shortcut, &written)
                .unwrap_or_else(|error| panic!("{label}: line {}: {error}\n{written}", error.line));
            assert_eq!(
                edit.shortcut.steps, shortcut.steps,
                "{label}: the steps the text reads back to\n{written}"
            );
            assert_eq!(
                edit.shortcut.inputs, shortcut.inputs,
                "{label}: the inputs the text reads back to\n{written}"
            );
            assert_eq!(
                edit.shortcut.output, shortcut.output,
                "{label}: what the text gives back\n{written}"
            );
            // A program that has not changed has text that has not changed, so
            // the second person to open it reads the words the first one left.
            let (again, _) = text::render(&shared, &edit.shortcut, &BTreeMap::new())
                .unwrap_or_else(|error| panic!("{label}: {error}"));
            assert_eq!(again, written, "{label}: the text reads back to itself");
        }
    }
}
