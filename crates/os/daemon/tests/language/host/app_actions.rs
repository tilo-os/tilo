//! The tree's tilolang apps that keep a record, each run the way a person's computer runs one
//! with no window, and driven through the daemon's one dispatch: the actions the app publishes on
//! its own record are what a session, an agent or another device reaches there, an edit through
//! them changes the records the app keeps, and those records read back through the device's one
//! record reader. An app whose records no published action changes is a coverage gap and fails
//! here by name.
//!
//! A tilolang app's records are what `tilo_read record:<app>` lists and reads: each root's
//! document, which the first change to it makes. An app nothing wrote to yet lists none, as a
//! window of it shows an empty notebook: it starts empty.
//!
//! What keeps this honest is the launcher and the broker: each app's instance runs in its
//! tilolang process under the launcher's sandbox, from the package verified as a launch verifies
//! it, and every record write passes the broker's admission for the caller (`may_write`). An app
//! that runs here with no window is the package this tree ships, writing what the workspace
//! admits.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;

use serde_json::{Map, Value};
use tilo_actions::api::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::remote::answered;
use tilo_actions::remote::{CataloguePage, ReadAnswer, ResourceReadRequest};
use tilo_actions::{Availability, ConfirmationPolicy, OperationKind, PROMPTED_PARAM, ParamType};

use super::{
    Host, app_action, listed, output, record, run_with_packages, running_record, start_app,
};

/// How many attempts an app gets at an edit before it is a gap.
const ATTEMPTS: usize = 40;

/// The text every edit writes.
const WRITTEN: &str = "Written by an agent";

/// Apps whose state is the device's own by design, so no published action changes the records
/// every member reads: there is nothing for a caller to edit there.
const LOCAL_STATE: &[&str] = &[];

/// One tilolang app of this tree: its package (a directory of `apps/tl`) and its id.
struct App {
    package: &'static str,
    id: &'static str,
}

const APPS: &[App] = &[
    App {
        package: "notes",
        id: "so.tilo.notes",
    },
    App {
        package: "tasks",
        id: "so.tilo.tasks",
    },
    App {
        package: "contacts",
        id: "so.tilo.contacts",
    },
    App {
        package: "sheets",
        id: "so.tilo.sheets",
    },
    App {
        package: "word-processor",
        id: "so.tilo.word-processor",
    },
    App {
        package: "clocks",
        id: "so.tilo.clocks",
    },
    App {
        package: "messages",
        id: "so.tilo.messages",
    },
];

impl App {
    /// The resource the app publishes its records' actions on: the package's own (`tl:<app>`),
    /// where an action on one of its roots is reached whether or not a window opened the root.
    fn kind(&self) -> String {
        format!("tl:{}", self.id)
    }
}

/// Every app's package, as the device installs it.
fn packages() -> Vec<&'static str> {
    APPS.iter().map(|app| app.package).collect()
}

/// One edit a caller unfamiliar with the app would try: the action, and arguments of a value of
/// each type it declares.
#[derive(Clone, Debug)]
struct Edit {
    label: String,
    address: ActionAddress,
    arguments: Map<String, Value>,
}

/// Every app takes an edit through the actions it publishes, found without app-specific
/// knowledge: each app starts with no window, and a document action it offers without a
/// confirmation changes its records, which read back through the record reader.
#[test]
fn every_app_takes_an_edit_through_its_published_actions() {
    run_with_packages(&packages(), &[], |host| async move {
        let mut table = String::from("| App | Actions | Edit |\n|---|---|---|\n");
        let mut gaps = Vec::new();
        for app in APPS {
            let id = app.id;
            let page = started(&host, app).await;
            let before = records(&host, id).await;
            let actions = published(&page, &app.kind());
            let (edit, refused) = find_edit(&host, app, &before).await;
            let label = edit.as_ref().map_or_else(
                || "—".to_owned(),
                |edit| format!("{}#{}", edit.address.resource, edit.address.action),
            );
            let _ = writeln!(table, "| {id} | {} | `{label}` |", actions.len());
            if edit.is_none() && !LOCAL_STATE.contains(&id) {
                gaps.push(format!("{id}: {refused:#?}"));
            }
        }
        eprintln!("{table}");
        assert!(gaps.is_empty(), "agent coverage gaps:\n{}", gaps.join("\n"));
    });
}

/// Every app's records read with no window open: once a caller wrote through what the app
/// publishes, each record it lists answers what it holds through the device's one record reader,
/// the path `tilo_read record:<app>:<record>` takes. A record that answers nothing, or that the
/// reader refuses, is a gap.
#[test]
fn every_apps_record_reads_with_no_window_open() {
    run_with_packages(&packages(), &[], |host| async move {
        let mut gaps = Vec::new();
        let mut readable = 0;
        for app in APPS {
            let id = app.id;
            started(&host, app).await;
            let before = records(&host, id).await;
            if find_edit(&host, app, &before).await.0.is_none() {
                gaps.push(format!("{id}: no published action wrote a record"));
                continue;
            }
            let held = records(&host, id).await;
            let unread = held.values().any(|entry| !entry["value"].is_object());
            if held.is_empty() || unread {
                gaps.push(format!("{id}: its records read {held:#?}"));
                continue;
            }
            readable += 1;
        }
        assert!(
            gaps.is_empty(),
            "records a caller with no window open cannot read:\n{}",
            gaps.join("\n")
        );
        assert_eq!(readable, APPS.len(), "every app reads its records");
    });
}

/// Every app's empty state offers the actions it publishes: with nothing written yet, every
/// record it lists reads, and the document actions the app offers reach it through the dispatch
/// when pressed. An app whose empty state offers none is not a gap (an empty camera offers no
/// edit of a capture it never took); no app offering one is.
#[test]
fn every_apps_empty_state_offers_the_actions_it_publishes() {
    run_with_packages(&packages(), &[], |host| async move {
        let mut gaps = String::new();
        let mut pressed = 0;
        for app in APPS {
            let id = app.id;
            let page = started(&host, app).await;
            for (record, entry) in records(&host, id).await {
                if !entry["value"].is_object() {
                    let _ = writeln!(gaps, "{id}: its empty record {record:?} reads as {entry}");
                }
            }
            let offers = candidates(&page, &app.kind());
            let Some(press) = offers.first() else {
                continue;
            };
            let target = app_action(&host, id, &press.address.resource, &press.address.action);
            match output(&host, target, Value::Object(press.arguments.clone())).await {
                Ok(_) => pressed += 1,
                Err(refused) => {
                    let _ = writeln!(
                        gaps,
                        "{id}: pressing its empty-state action `{}` failed: {refused:?}",
                        press.label
                    );
                }
            }
        }
        assert!(
            gaps.is_empty(),
            "empty-state actions that do not reach the app:\n{gaps}"
        );
        assert!(pressed > 0, "no app offered an action in its empty state");
    });
}

/// `app` started with no window, as the workspace's own `start` does: its windowless instance
/// runs, and the catalogue lists what it publishes.
async fn started(host: &Host, app: &App) -> CataloguePage {
    let id = app.id;
    let workspace = host.workspace();
    start_app(host, id)
        .await
        .unwrap_or_else(|refused| panic!("{id} starts with no window: {refused:?}"));
    let page = listed(host, id)
        .await
        .unwrap_or_else(|refused| panic!("{id} lists what it publishes: {refused:?}"));
    running_record(&page, workspace, id)
        .unwrap_or_else(|| panic!("{id} publishes from its instance in {workspace}"));
    assert_eq!(
        host.daemon()
            .apps()
            .no_window_processes(id, workspace, None),
        1,
        "{id} runs one instance with no window"
    );
    page
}

/// Every record `app` keeps in the daemon's workspace, each as the device's record reader
/// answers it: what `tilo_read record:<app>` lists, read one by one.
async fn records(host: &Host, app: &str) -> BTreeMap<String, Value> {
    let reads = host
        .dispatch()
        .resource_reads()
        .expect("the dispatch reads records");
    let listing = answered(
        reads,
        &host.caller(),
        ResourceReadRequest {
            workspace: host.workspace().to_owned(),
            app: app.to_owned(),
            record: String::new(),
            joined: false,
            after: None,
            wait_ms: None,
            answer: ReadAnswer::Listing,
        },
    )
    .await
    .unwrap_or_else(|refused| panic!("{app}'s records list: {refused:?}"));
    let mut held = BTreeMap::new();
    for listed in listing["records"].as_array().into_iter().flatten() {
        let (Some(of), Some(name)) = (listed["app"].as_str(), listed["record"].as_str()) else {
            panic!("a listed record names its app and itself: {listed}");
        };
        if of != app {
            continue;
        }
        let entry = record(host, app, name)
            .await
            .unwrap_or_else(|refused| panic!("{app}'s record {name:?} reads: {refused:?}"));
        held.insert(name.to_owned(), entry);
    }
    held
}

/// The descriptors the running app publishes on `kind`: what the catalogue lists for it now.
fn published(page: &CataloguePage, kind: &str) -> Vec<ActionDescriptor> {
    page.rows
        .iter()
        .filter_map(|row| row.action.clone())
        .filter(|descriptor| descriptor.address.resource == kind)
        .collect()
}

/// Try [`candidates`] until one changes the app's records, listing what the app publishes again
/// after every attempt: an edit can change what the record offers. Returns the edit and every
/// attempt before it.
async fn find_edit(
    host: &Host,
    app: &App,
    first: &BTreeMap<String, Value>,
) -> (Option<Edit>, Vec<String>) {
    let kind = &app.kind();
    let app = app.id;
    let mut tried = HashSet::new();
    let mut refused = Vec::new();
    let mut before = first.clone();
    for _ in 0..ATTEMPTS {
        let page = listed(host, app)
            .await
            .unwrap_or_else(|error| panic!("{app} lists what it publishes: {error:?}"));
        let Some(edit) = candidates(&page, kind)
            .into_iter()
            .find(|edit| !tried.contains(&edit.label))
        else {
            break;
        };
        tried.insert(edit.label.clone());
        let target = app_action(host, app, &edit.address.resource, &edit.address.action);
        match output(host, target, Value::Object(edit.arguments.clone())).await {
            Ok(_) => {
                let after = records(host, app).await;
                if after != before {
                    return (Some(edit), refused);
                }
                refused.push(format!("{}: no change", edit.label));
                before = after;
            }
            Err(error) => refused.push(format!("{}: {error:?}", edit.label)),
        }
    }
    (None, refused)
}

/// The attempts a caller unfamiliar with the app would make, in order: the record's document
/// actions that need no confirmation, with a value of each declared type.
fn candidates(page: &CataloguePage, kind: &str) -> Vec<Edit> {
    let mut seen = HashSet::new();
    page.rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .filter(|descriptor| {
            descriptor.address.resource == kind
                && descriptor.kind == OperationKind::Document
                && basic(descriptor)
        })
        .filter_map(|descriptor| {
            let arguments = sample_arguments(descriptor)?;
            let label = format!(
                "{}#{}",
                descriptor.address.resource, descriptor.address.action
            );
            seen.insert(label.clone()).then_some(Edit {
                label,
                address: descriptor.address.clone(),
                arguments,
            })
        })
        .collect()
}

/// Available, without a confirmation step, and not an external effect.
fn basic(descriptor: &ActionDescriptor) -> bool {
    matches!(descriptor.contract.availability, Availability::Available)
        && matches!(descriptor.contract.confirmation, ConfirmationPolicy::None)
        && descriptor.kind != OperationKind::Effect
}

/// Arguments a caller with no knowledge of the app would try: a value of each declared type.
/// `None` when a required parameter has a type no generic value fits.
fn sample_arguments(descriptor: &ActionDescriptor) -> Option<Map<String, Value>> {
    let mut arguments = Map::new();
    if descriptor.params.is_empty()
        && let InputSpec::Text { .. } = descriptor.input
    {
        arguments.insert(PROMPTED_PARAM.into(), WRITTEN.into());
    }
    for param in descriptor.params.iter().filter(|param| param.required) {
        let value = if let Some(choice) = param.choices.first() {
            Value::from(choice.as_ref())
        } else if let Some(example) = param
            .schema
            .as_ref()
            .and_then(|schema| schema.get("examples"))
            .and_then(Value::as_array)
            .and_then(|examples| examples.first())
        {
            // A caller reads constrained input from the published contract, just as a person
            // follows the palette's parameter guidance.
            example.clone()
        } else {
            match param.ty {
                ParamType::Text => WRITTEN.into(),
                ParamType::Integer => 1.into(),
                ParamType::Number => 1.5.into(),
                ParamType::Boolean => true.into(),
                _ => return None,
            }
        };
        arguments.insert(param.name.to_string(), value);
    }
    Some(arguments)
}
