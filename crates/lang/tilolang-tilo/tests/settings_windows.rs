//! The Windows layer on Settings' Updates page: what the layer's reading says, how far an
//! install has got, and only the actions the reading offers. Opened and pressed here, and what it
//! sends is what the device publishes, never a copy of it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

const APP: &str = "so.tilo.settings";

/// The person at the settings window.
fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

use crate::invoker::{Answered as _, Sent};

use crate::fixtures::settings_host as settings;

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

/// The type the device package declares for the root `root`: what a reading of it is typed as.
fn device_type(root: &str) -> String {
    tilolang_tilo::platform::platform_package("so.tilo.device")
        .expect("the device package reads")
        .named_roots
        .iter()
        .find(|named| named.name == root)
        .unwrap_or_else(|| panic!("the device package has a root {root}"))
        .ty
        .clone()
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

/// The widgets' choices as the person's settings keep them, each on the shell and shown after a

#[test]
fn the_updates_pane_shows_the_windows_layer_and_offers_what_it_allows() {
    let (host, _) = settings();
    reading(
        &host,
        "system-update",
        "so.tilo.device#SystemUpdate",
        serde_json::json!({}),
    );
    reading(
        &host,
        "settings/updates",
        "so.tilo.settings.sections#Updates",
        serde_json::json!({}),
    );
    reading(
        &host,
        "windows-compat",
        &device_type("windows_compat"),
        serde_json::json!({
            "available": true,
            "state": "installing",
            "summary": "Installing Windows support.",
            "size": null,
            "problem": null,
            "pending": [],
            "progress": 0.15,
            "step": {
                "label": "Fetching Windows support",
                "number": 2,
                "count": 3,
                "megabytes-done": 120,
                "megabytes-total": 800
            },
            "offered": []
        }),
    );
    let view = open(&host, "updates_pane");
    let shown = texts(&view);
    assert!(
        shown.contains(&"Windows support".to_owned()),
        "the layer shows where the image, the firmware and the codecs do: {shown:?}"
    );
    assert!(
        shown.contains(&"Fetching Windows support 120 of 800 MB (step 2 of 3)…".to_owned()),
        "{shown:?}"
    );
    let installing = named(
        view.tree(),
        "ui.ProgressBar",
        "label",
        "Installing Windows support",
    );
    assert_eq!(
        installing.props.get("value"),
        Some(&Value::Float(0.15)),
        "how far it has got"
    );
    let buttons: Vec<String> = view
        .tree()
        .of_kind("ui.Button")
        .into_iter()
        .filter_map(|button| button.props.get("label").cloned())
        .filter_map(|label| match label {
            Value::Text(text) => Some(text),
            _ => None,
        })
        .collect();
    for absent in ["Install", "Try again", "Remove", "Cancel"] {
        assert!(
            !buttons.contains(&absent.to_owned()),
            "nothing about the layer is offered while it installs: {buttons:?}"
        );
    }
}

/// A layer that is not installed offers the install, at the size it told the person; one that
/// failed offers trying again, and a removal; one waiting for a network offers the cancel.
#[test]
fn the_windows_card_offers_the_install_the_retry_and_the_removal() {
    let layer = |state: &str, offered: serde_json::Value, problem: serde_json::Value| {
        serde_json::json!({
            "available": true,
            "state": state,
            "summary": "…",
            "size": "1.2 GB",
            "problem": problem,
            "pending": [],
            "progress": null,
            "step": null,
            "offered": offered
        })
    };
    for (state, offered, problem, status, buttons) in [
        (
            "needs-install",
            serde_json::json!(["install"]),
            serde_json::Value::Null,
            "Not installed",
            vec!["Install"],
        ),
        (
            "failed",
            serde_json::json!(["install", "remove"]),
            serde_json::json!("disk full"),
            "Could not install",
            vec!["Try again", "Remove"],
        ),
        (
            "waiting-for-network",
            serde_json::json!(["remove"]),
            serde_json::Value::Null,
            "Waiting for an internet connection; it installs by itself once this device is online",
            vec!["Cancel"],
        ),
    ] {
        let (host, _) = settings();
        reading(
            &host,
            "system-update",
            "so.tilo.device#SystemUpdate",
            serde_json::json!({}),
        );
        reading(
            &host,
            "settings/updates",
            "so.tilo.settings.sections#Updates",
            serde_json::json!({}),
        );
        reading(
            &host,
            "windows-compat",
            &device_type("windows_compat"),
            layer(state, offered, problem),
        );
        let view = open(&host, "updates_pane");
        let shown = texts(&view);
        assert!(shown.contains(&status.to_owned()), "{state}: {shown:?}");
        let drawn: Vec<String> = view
            .tree()
            .of_kind("ui.Button")
            .into_iter()
            .filter_map(|button| button.props.get("label").cloned())
            .filter_map(|label| match label {
                Value::Text(text) => Some(text),
                _ => None,
            })
            .collect();
        for wanted in &buttons {
            assert!(
                drawn.contains(&(*wanted).to_owned()),
                "{state} offers {wanted}: {drawn:?}"
            );
        }
        if state == "needs-install" {
            assert!(
                shown.contains(&"Installing it downloads 1.2 GB.".to_owned()),
                "the size the install takes: {shown:?}"
            );
            let sent = Sent::new();
            host.runtime().set_invoker(sent.clone());
            let install = labelled(view.tree(), "ui.Button", "Install");
            view.press_answering(&host, &me(), &install)
                .expect("the person installs it");
            assert_eq!(
                sent.called(),
                ("so.tilo.device".to_owned(), "install".to_owned())
            );
        }
    }
}
