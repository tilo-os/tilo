//! Settings' About page: the problems this computer recorded, listed and each opened to its own
//! record, and what a person does with one there — send it, or remove it from this computer.
//! Opened and pressed here, and what it sends is what the device publishes, never a copy of it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

const APP: &str = "so.tilo.settings";

/// The app as it ships, with the platform's packages it reads through loaded, as a window's
/// process loads them.
fn settings() -> (Host, tilolang::Compiled) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/settings");
    let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
    )
    .expect("its ids parse");
    let metas = crate::fixtures::settings_packages().expect("the platform's packages read");
    let compiled =
        tilolang::compile_package_source(&source, &ids, &metas).expect("the package compiles");
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .expect("a host");
    host.load_compiled(&compiled).expect("the package loads");
    tilolang_tilo::platform_apps::load(&host, &compiled.meta.uses);
    (host, compiled)
}

/// The resource `doc` of `ty`, as a view of a platform package reads it.
fn resource(doc: &str, ty: &str) -> ResourceRef {
    ResourceRef::parse_id(doc, ty).expect("a resource id")
}

/// Hand `host` the reading `json` of `doc`, as the session publishes it there.
fn reading(host: &Host, doc: &str, ty: &str, json: serde_json::Value) {
    host.runtime()
        .set_reading(&resource(doc, ty), &json.to_string())
        .unwrap_or_else(|why| panic!("{doc} is read: {why}"));
}

/// The app's view `name`, opened as a window opens it.
fn open(host: &Host, name: &str) -> ViewInstance {
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .views
        .iter()
        .find(|view| view.name == name)
        .unwrap_or_else(|| panic!("the app declares the view `{name}`"))
        .id
        .clone();
    let mut view = host
        .runtime()
        .open_view(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            &id,
            Vec::new(),
        )
        .unwrap_or_else(|why| panic!("{name} opens: {why}"));
    view.poll_patches().expect("the view draws");
    view
}

/// The words a view shows, its text as written.
fn texts(view: &ViewInstance) -> Vec<String> {
    crate::fixtures::nodes(view.tree())
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            Value::Rich(rich) => Some(rich.written_text()),
            _ => None,
        })
        .collect()
}

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

/// One invocation as it was sent, with its arguments by name.
#[derive(Clone)]
struct Sent(Rc<RefCell<Vec<Invocation>>>);

impl Sent {
    fn new() -> Self {
        Self(Rc::default())
    }

    fn last(&self) -> Invocation {
        self.0
            .borrow()
            .last()
            .cloned()
            .unwrap_or_else(|| panic!("the pane sends a verb"))
    }

    /// The app and action of the last invocation.
    fn called(&self) -> (String, String) {
        let Invocation::Action { app, name, .. } = self.last() else {
            panic!("the pane sends an action: {:?}", self.last());
        };
        (app.to_string(), name)
    }

    fn argument(&self, named: &str) -> Value {
        let Invocation::Action { args, .. } = self.last() else {
            panic!("the pane sends an action: {:?}", self.last());
        };
        args.iter().find(|(name, _)| name == named).map_or_else(
            || panic!("the verb takes `{named}`: {args:?}"),
            |(_, value)| value.clone(),
        )
    }
}

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
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
        serde_json::json!({ "hostname": "build", "arch": "x86_64" }),
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
    view.press(&send_button)
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
    assert!(
        !shown.iter().any(|text| text.contains("Kernel panic")),
        "no record is read to list it: {shown:?}"
    );

    // The row opens the report's own page, with what happened and the record itself.
    let row = named(view.tree(), "ui.ListRow", "title", "The computer stopped");
    view.fire(&row.key, "activate")
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
    view.press(&remove).expect("the person removes the record");
    assert_eq!(
        sent.called(),
        ("tilo.crash-reports".to_owned(), "remove".to_owned())
    );
    assert_eq!(sent.argument("report"), Value::text(STOPPED));

    let back = named(view.tree(), "ui.ListRow", "title", "Back")
        .key
        .clone();
    view.fire(&back, "activate").expect("the person goes back");
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
