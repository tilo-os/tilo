//! The environments a device publishes are a reading a view follows: a window open on them shows
//! an environment made after it opened, without the window being reopened. The reading is the
//! device's to make again; a view holds no copy of it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use serde_json::json;
use tilolang_tilo::host::Host;
use tilolang_tilo::runtime::{Actor, ResourceRef, Value, ViewInstance};

/// The tilolang package a view of the environments is: it reads the root the platform package
/// publishes, so its fields are the types `tilo.environment` declares.
const VIEWING: &str = r#"app so.tilo.test.environments "Environments"

use tilo.environment as environment

/// What the window shows: every environment of the workspace, by name.
view panel() {
    let all = environment.environments
    page {
        for entry in all.environments {
            text("{entry.name}, {entry.runtime}") as entry_name
        }
    }
}
"#;

fn ada() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// The root the platform's package publishes the environments at.
fn environments() -> ResourceRef {
    ResourceRef::parse_id("environments", "tilo.environment#Environments").expect("the root")
}

/// What a device publishes: each environment's record, name, runtime and the rest a window lists.
fn reading(entries: &[(&str, &str)]) -> String {
    let environments: Vec<serde_json::Value> = entries
        .iter()
        .map(|(record, name)| {
            json!({
                "record": record, "name": name, "tools": [], "grants": [],
                "home": "Persistent", "runtime": "2026.11",
            })
        })
        .collect();
    json!({ "environments": environments }).to_string()
}

/// The window open on the environments, on `reading`.
fn open(reading: &str) -> (Host, ViewInstance) {
    let platforms = tilolang_tilo::platform::platform_packages().expect("the platform's packages");
    let compiled = tilolang::compile_package(VIEWING, &tilolang::IdsLock::new(), &platforms)
        .unwrap_or_else(|diagnostics| {
            panic!(
                "{}",
                diagnostics
                    .iter()
                    .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        });
    let host = Host::new(ada(), || None).expect("a host");
    for meta in platforms
        .iter()
        .filter(|meta| compiled.meta.uses.iter().any(|used| used == &meta.app))
    {
        host.runtime()
            .load_foreign(meta.clone())
            .expect("the device is loaded");
    }
    host.load_compiled(&compiled).expect("the package loads");
    host.runtime()
        .set_reading(&environments(), reading)
        .expect("the reading is set");
    let view = compiled
        .meta
        .views
        .iter()
        .find(|view| view.name == "panel")
        .expect("the window")
        .id
        .clone();
    let window = host
        .runtime()
        .open_view(&ada(), &view, Vec::new())
        .expect("the window opens over the environments");
    (host, window)
}

/// What the window shows as its words.
fn shown(view: &ViewInstance) -> Vec<String> {
    crate::fixtures::nodes(view.tree())
        .into_iter()
        .filter_map(|node| node.props.get("value"))
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn an_open_window_shows_an_environment_made_after_it_opened() {
    let (host, mut window) = open(&reading(&[("default", "Default")]));
    let words = shown(&window);
    assert!(
        words.iter().any(|text| text == "Default, 2026.11"),
        "the window shows the environment the device published: {words:?}"
    );

    // The device makes the reading again, with what the workspace holds now: a window already
    // open shows it, without being reopened.
    host.runtime()
        .set_reading(
            &environments(),
            &reading(&[("default", "Default"), ("work", "Work")]),
        )
        .expect("the reading is made again");
    window.poll_patches().expect("the window draws the change");
    let words = shown(&window);
    assert!(
        words.iter().any(|text| text == "Work, 2026.11"),
        "the environment made after it opened is shown: {words:?}"
    );
    assert!(
        words.iter().any(|text| text == "Default, 2026.11"),
        "what it showed before is still there: {words:?}"
    );
}
