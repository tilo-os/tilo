//! Settings' About page: the problems this computer recorded, listed and each opened to its own
//! record, and what a person does with one there — send it, or remove it from this computer.
//! Opened and pressed here, and what it sends is what the device publishes, never a copy of it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use tilolang_runtime::{Actor, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

/// The person at the settings window.
fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

use crate::fixtures::invoker::{Answered as _, Sent};
use crate::fixtures::reading_at as reading;
use crate::fixtures::settings_host as settings;
use crate::fixtures::settings_view as open;
use crate::fixtures::texts;

/// The node of `kind` whose `prop` reads `word`.
fn named<'a>(tree: &'a UiNode, kind: &str, prop: &str, word: &str) -> &'a UiNode {
    tree.of_kind(kind)
        .into_iter()
        .find(|node| node.props.get(prop) == Some(&Value::text(word)))
        .unwrap_or_else(|| panic!("a {kind} whose {prop} reads {word}"))
}

/// The control of `kind` labelled `label`, by the key the window gives it.
fn labelled(tree: &UiNode, kind: &str, label: &str) -> String {
    named(tree, kind, "label", label).key.clone()
}

const STOPPED: &str = "2026-10-25-081400-3f9a0c1d2e4b5a69-stopped.log";
const FREEZE: &str = "2026-10-24-225201-412-freeze.log";

/// The readings the About page lists, as the session publishes them: this computer's update and
/// apps, what it is, the problems it recorded, and the record of the one a person opens.
fn serve_about_readings(host: &Host) {
    reading(
        host,
        "system-update",
        "so.tilo.device#SystemUpdate",
        serde_json::json!({}),
    );
    reading(
        host,
        "session:apps",
        "so.tilo.device#Apps",
        serde_json::json!({}),
    );
    reading(
        host,
        "host-info",
        "so.tilo.device#HostInfo",
        serde_json::json!({
            "hostname": "build",
            "arch": "x86_64",
            "model": "Framework Laptop 13 (AMD)",
            "firmware": "03.03",
            "cpu": "AMD Ryzen 7 7840U",
            "memory": "32 GB",
            "components": [
                { "kind": "graphics", "name": "AMD Radeon 780M", "state": "supported", "present": true },
                { "kind": "fingerprint", "name": "Goodix reader", "state": "limited", "present": true },
                { "kind": "camera", "name": "", "state": "unverified", "present": false }
            ],
            "blocked": false
        }),
    );
    let reports = serde_json::json!([
        {
            "id": STOPPED,
            "at": 1_761_376_440,
            "when": "25 Oct 2026, 08:14",
            "kind": "computer-stopped",
            "reason": "panic",
            "version": null,
            "build": null
        },
        {
            "id": FREEZE,
            "at": 1_761_342_721,
            "when": "24 Oct 2026, 22:52",
            "kind": "froze",
            "reason": null,
            "version": "0.1.0",
            "build": "9f2c41d07be3a65e"
        }
    ]);
    reading(
        host,
        "crash-reports",
        "tilo.crash-reports#CrashReports",
        serde_json::json!({ "reports": reports }),
    );
    reading(
        host,
        &format!("crash-report:{STOPPED}"),
        "tilo.crash-reports#CrashReportText",
        serde_json::json!({
            "report": reports[0],
            "text": "tilo computer stopped\nreason:    panic\n\nKernel panic - not syncing: Oops: Fatal exception\n",
            "truncated": false
        }),
    );
}

/// Send the opened problem where the people who make Tilo read problems: Chat's own problem
/// report, carrying what happened, when, and the record itself with its stacks. The page's own
/// action runs, and what it asks of other packages is recorded, so what is read here is the report
/// the app really files.
fn send_the_opened_problem(host: &Host, view: &ViewInstance) -> Sent {
    let sent = Sent::new();
    host.runtime().set_foreign_invoker(sent.clone());
    let send_button = labelled(view.tree(), "ui.Button", "Send a report");
    view.press_answering(host, &me(), &send_button)
        .expect("the person sends the report");
    assert_eq!(
        sent.called(),
        ("tilo.actions".to_owned(), "call".to_owned())
    );
    assert_eq!(
        sent.argument("address"),
        Value::text("so.tilo.chat/conversations#report_problem"),
        "a report is a problem report, the one place the workspace keeps them"
    );
    let arguments = sent.argument("arguments");
    let body = match &arguments {
        Value::Json(document) => document["body"]
            .as_str()
            .unwrap_or_else(|| panic!("the report's words: {document:#?}"))
            .to_owned(),
        other => panic!("a call's arguments are a document: {other:?}"),
    };
    for word in ["The computer stopped", "25 Oct 2026, 08:14"] {
        assert!(body.contains(word), "{word} is in the report: {body}");
    }
    assert!(
        body.contains("Kernel panic - not syncing"),
        "the record goes with the report: {body}"
    );
    sent
}

/// About lists the problems this computer recorded and opens one's page: what happened, when,
/// why, and the record itself, read only when it is opened.
#[test]
fn the_about_pane_lists_problems_and_opens_one_to_its_record() {
    let (host, _) = settings();
    serve_about_readings(&host);
    let mut view = open(&host, "about_pane");

    let shown = texts(&view);
    for word in [
        "Problems",
        "The computer stopped",
        "The desktop stopped responding",
        "25 Oct 2026, 08:14",
        "24 Oct 2026, 22:52",
        "These reports stay on this computer.",
    ] {
        assert!(shown.iter().any(|text| text == word), "{word}: {shown:?}");
    }

    // Hardware is the report an installation read: the model, the firmware and each part the
    // check found, and no row for a part this computer does not have.
    for word in [
        "Framework Laptop 13 (AMD)",
        "AMD Ryzen 7 7840U",
        "32 GB",
        "03.03",
        "Graphics",
        "Supported",
        "Fingerprint reader",
        "Limited",
    ] {
        assert!(shown.iter().any(|text| text == word), "{word}: {shown:?}");
    }
    assert!(
        !shown.iter().any(|text| text == "Camera"),
        "an absent part is no row: {shown:?}"
    );
    assert!(
        !shown.iter().any(|text| text.contains("Kernel panic")),
        "no record is read to list it: {shown:?}"
    );

    // The row opens the report's own page, with what happened and the record itself.
    let row = named(view.tree(), "ui.ListRow", "title", "The computer stopped");
    view.fire_answering(&host, &me(), &row.key, "activate")
        .expect("the person opens the problem");
    view.poll_patches().expect("its page draws");
    let page = texts(&view);
    for word in [
        "What happened",
        "The computer stopped",
        "Why",
        "The system met an error it could not recover from",
        "Record",
    ] {
        assert!(page.iter().any(|text| text == word), "{word}: {page:?}");
    }
    assert!(
        page.iter()
            .any(|text| text.contains("Kernel panic - not syncing")),
        "the record as it was written: {page:?}"
    );

    // The page sends the record where the people who make Tilo read problems.
    let sent = send_the_opened_problem(&host, &view);

    // Removing it takes the record off this computer, through the reports' own verb.
    let remove = labelled(view.tree(), "ui.Button", "Remove it from this computer");
    view.press_answering(&host, &me(), &remove)
        .expect("the person removes the record");
    assert_eq!(
        sent.called(),
        ("tilo.crash-reports".to_owned(), "remove".to_owned())
    );
    assert_eq!(sent.argument("report"), Value::text(STOPPED));

    let back = named(view.tree(), "ui.ListRow", "title", "Back")
        .key
        .clone();
    view.fire_answering(&host, &me(), &back, "activate")
        .expect("the person goes back");
    view.poll_patches().expect("the list draws");
    assert!(
        texts(&view)
            .iter()
            .any(|text| text == "The desktop stopped responding"),
        "{:?}",
        texts(&view)
    );
}

/// About with no report says so, and offers no row to open.
#[test]
fn about_without_reports_says_that_none_were_recorded() {
    let (host, _) = settings();
    reading(
        &host,
        "system-update",
        "so.tilo.device#SystemUpdate",
        serde_json::json!({}),
    );
    reading(
        &host,
        "session:apps",
        "so.tilo.device#Apps",
        serde_json::json!({}),
    );
    reading(
        &host,
        "host-info",
        "so.tilo.device#HostInfo",
        serde_json::json!({}),
    );
    reading(
        &host,
        "crash-reports",
        "tilo.crash-reports#CrashReports",
        serde_json::json!({ "reports": [] }),
    );
    let view = open(&host, "about_pane");
    let shown = texts(&view);
    assert!(
        shown.contains(&"No problems recorded".to_owned()),
        "{shown:?}"
    );
    assert!(
        !shown.iter().any(|text| text == "The computer stopped"),
        "{shown:?}"
    );
}
