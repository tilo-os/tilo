//! Every Rust app of the tree, run the way a person's computer runs one with no window, and
//! driven through the daemon's one dispatch: the actions the app publishes on its own record are
//! what a session, an agent or another device reaches there, an edit through them changes the
//! record the app keeps, and the record reads back through the device's one record reader. An
//! app whose record no published action changes is a coverage gap and fails here by name.
//!
//! What keeps this honest is the launcher and the broker: each process runs under the launcher's
//! sandbox (bubblewrap with the package hash proven against the signed package), and every record
//! write passes the broker's admission for the caller (`may_write`). An app that runs here with
//! no window is the package this tree ships, writing what the workspace admits.

use std::collections::HashSet;
use std::fmt::Write as _;

use serde_json::{Map, Value};
use tilo_actions::api::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::{Availability, ConfirmationPolicy, OperationKind, PROMPTED_PARAM, ParamType};

use super::{
    Host, app_action, listed, output, record, run_with_packages, running_record, start_app,
};

/// How many attempts an app gets at an edit before it is a gap.
const ATTEMPTS: usize = 40;

/// The text every edit writes.
const WRITTEN: &str = "Written by an agent";

/// Apps whose state is the device's own by design, so no published action changes the record
/// every member reads: there is nothing for a caller to edit there.
const LOCAL_STATE: &[&str] = &["so.tilo.totp"];

/// One Rust app of this tree: its package (a directory of `apps/`), the entrypoint this run
/// built, its id, and the kind of record it keeps for the workspace.
struct App {
    package: &'static str,
    bin: &'static str,
    id: &'static str,
    kind: &'static str,
}

const APPS: &[App] = &[
    App {
        package: "console",
        bin: "console",
        id: "so.tilo.console",
        kind: "console.view",
    },
    App {
        package: "messages",
        bin: "messages",
        id: "so.tilo.messages",
        kind: "tilo.messages",
    },
    App {
        package: "poker",
        bin: "poker",
        id: "so.tilo.poker",
        kind: "so.tilo.poker.table",
    },
    App {
        package: "slides",
        bin: "slides",
        id: "so.tilo.slides",
        kind: "slides.deck",
    },
    App {
        package: "terminal",
        bin: "terminal",
        id: "so.tilo.terminal",
        kind: "terminal.session",
    },
    App {
        package: "totp",
        bin: "totp",
        id: "so.tilo.totp",
        kind: "totp.vault",
    },
];

/// Every app's package and entrypoint binary, as the run built them.
fn rust() -> Vec<(&'static str, &'static str)> {
    APPS.iter().map(|app| (app.package, app.bin)).collect()
}

/// One edit a caller unfamiliar with the app would try: the action, and arguments of a value of
/// each type it declares.
#[derive(Clone, Debug)]
struct Edit {
    label: String,
    address: ActionAddress,
    arguments: Map<String, Value>,
}

/// Every app takes an edit through the actions its record publishes, found without app-specific
/// knowledge: each app starts with no window on its own record, and a document action it offers
/// without a confirmation changes the record, which reads back through the record reader.
#[test]
fn every_app_takes_an_edit_through_its_published_actions() {
    run_with_packages(&[], &rust(), |host| async move {
        let workspace = host.workspace().to_owned();
        let mut table = String::from("| App | Actions | Edit |\n|---|---|---|\n");
        let mut gaps = Vec::new();
        for app in APPS {
            let id = app.id;
            start_app(&host, id)
                .await
                .unwrap_or_else(|refused| panic!("{id} starts with no window: {refused:?}"));
            let page = listed(&host, id)
                .await
                .unwrap_or_else(|refused| panic!("{id} lists what it publishes: {refused:?}"));
            let opened = running_record(&page, &workspace, id)
                .unwrap_or_else(|| panic!("{id} runs with no window in {workspace}"));
            assert_eq!(
                host.daemon()
                    .apps()
                    .no_window_processes(id, &workspace, None),
                1,
                "{id} runs one process with no window"
            );
            let before = record(&host, id, &opened)
                .await
                .unwrap_or_else(|refused| panic!("{id}'s record reads: {refused:?}"));
            let actions = published(&page, app.kind);
            let (edit, refused) = find_edit(&host, id, app.kind, &opened, &before).await;
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

/// Every app's record reads with no window open: the record its process opened answers what it
/// holds through the device's one record reader, the path `tilo_read record:<app>:<record>` takes,
/// and the app publishes the `read` that path's caller looks for. A record an app keeps with no
/// read at all is a gap.
#[test]
fn every_apps_record_reads_with_no_window_open() {
    run_with_packages(&[], &rust(), |host| async move {
        let workspace = host.workspace().to_owned();
        let mut gaps = Vec::new();
        let mut readable = 0;
        for app in APPS {
            let id = app.id;
            start_app(&host, id)
                .await
                .unwrap_or_else(|refused| panic!("{id} starts with no window: {refused:?}"));
            let page = listed(&host, id)
                .await
                .unwrap_or_else(|refused| panic!("{id} lists what it publishes: {refused:?}"));
            let opened = running_record(&page, &workspace, id)
                .unwrap_or_else(|| panic!("{id} runs with no window in {workspace}"));
            let publishes_read =
                page.rows
                    .iter()
                    .filter_map(|row| row.action.as_ref())
                    .any(|descriptor| {
                        descriptor.address.action == "read"
                            && descriptor.address.resource == app.kind
                    });
            if !publishes_read {
                gaps.push(format!("{id}: its record publishes no read"));
                continue;
            }
            match record(&host, id, &opened).await {
                Ok(entry) if entry["value"].is_object() => readable += 1,
                Ok(entry) => gaps.push(format!("{id}: the read answers {entry}")),
                Err(refused) => gaps.push(format!("{id}: {refused:?}")),
            }
        }
        assert!(
            gaps.is_empty(),
            "records a caller with no window open cannot read:\n{}",
            gaps.join("\n")
        );
        assert_eq!(readable, APPS.len(), "every app reads its record");
    });
}

/// Every app's empty state offers the actions it publishes: on a record nothing wrote yet, the
/// document actions the app offers reach it through the dispatch when pressed. An app whose
/// empty state offers none is not a gap (an empty camera offers no edit of a capture it never
/// took); no app offering one is.
#[test]
fn every_apps_empty_state_offers_the_actions_it_publishes() {
    run_with_packages(&[], &rust(), |host| async move {
        let workspace = host.workspace().to_owned();
        let mut gaps = String::new();
        let mut pressed = 0;
        for app in APPS {
            let id = app.id;
            start_app(&host, id)
                .await
                .unwrap_or_else(|refused| panic!("{id} starts with no window: {refused:?}"));
            let page = listed(&host, id)
                .await
                .unwrap_or_else(|refused| panic!("{id} lists what it publishes: {refused:?}"));
            let opened = running_record(&page, &workspace, id)
                .unwrap_or_else(|| panic!("{id} runs with no window in {workspace}"));
            let empty = record(&host, id, &opened)
                .await
                .unwrap_or_else(|refused| panic!("{id}'s empty record reads: {refused:?}"));
            assert!(
                empty["value"].is_object(),
                "{id}'s empty state reads as what it holds: {empty}"
            );
            let offers = candidates(&page, app.kind);
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

/// The descriptors the running app publishes on `kind`: what the catalogue lists for it now.
fn published(page: &tilo_actions::remote::CataloguePage, kind: &str) -> Vec<ActionDescriptor> {
    page.rows
        .iter()
        .filter_map(|row| row.action.clone())
        .filter(|descriptor| descriptor.address.resource == kind)
        .collect()
}

/// Try [`candidates`] until one changes the record, listing what the app publishes again after
/// every attempt: an edit can change what the record offers. Returns the edit and every attempt
/// before it.
async fn find_edit(
    host: &Host,
    app: &str,
    kind: &str,
    opened: &str,
    first: &Value,
) -> (Option<Edit>, Vec<String>) {
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
                let after = record(host, app, opened)
                    .await
                    .unwrap_or_else(|error| panic!("{app}'s record reads: {error:?}"));
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
fn candidates(page: &tilo_actions::remote::CataloguePage, kind: &str) -> Vec<Edit> {
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
