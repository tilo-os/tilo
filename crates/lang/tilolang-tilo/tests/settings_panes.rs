//! Settings' app-level actions show the pane a search page or setting names, as search's rows
//! run them from outside the window, on the app's published resource.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::path::Path;

use tilolang_tilo::host::Host;
use tilolang_tilo::runtime::{Actor, Value};

const APP: &str = "so.tilo.settings";

/// The app as it ships: its source, its ids and the platform's packages it reads through.
fn settings() -> (Host, tilolang::Compiled) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/settings");
    let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
    )
    .expect("its ids parse");
    let metas =
        tilolang_tilo::platform_apps::platform_packages().expect("the platform's packages read");
    let compiled =
        tilolang::compile_package_source(&source, &ids, &metas).expect("the package compiles");
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .expect("a host");
    host.load_compiled(&compiled)
        .expect("the package loads");
    (host, compiled)
}

/// The pane the window shows, as the app's own document holds it.
fn pane(host: &Host) -> String {
    let root = host.root_of(APP).expect("the app's root");
    match host.runtime().read(&root, "pane").expect("a pane") {
        Value::Text(text) => text,
        other => panic!("not text: {other:?}"),
    }
}

/// Run the app-level action `name` with `args`, as a search row does from outside the window.
fn invoke(host: &Host, name: &str, args: Vec<Value>) -> Value {
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .actions
        .iter()
        .find(|action| action.name == name)
        .unwrap_or_else(|| panic!("the app publishes no `{name}`"))
        .id
        .clone();
    match host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (id.as_str(), args),
        )
        .unwrap_or_else(|why| panic!("{name}: {why}"))
    {
        tilolang_runtime::InvokeOutcome::Done(value) => value,
        other => panic!("{name}: {other:?}"),
    }
}

#[test]
fn a_setting_opens_the_pane_it_is_on_and_a_page_its_own() {
    let (host, _) = settings();
    let _ = host.root_of(APP).expect("the app's root");
    assert_eq!(pane(&host), "appearance", "the window starts on Appearance");

    invoke(&host, "show_setting", vec![Value::text("keyboard.layout")]);
    assert_eq!(pane(&host), "input", "a keyboard setting is on Input");

    invoke(&host, "show_setting", vec![Value::text("a11y.high_contrast")]);
    assert_eq!(pane(&host), "accessibility");

    invoke(&host, "show_setting", vec![Value::text("notifications.banners")]);
    assert_eq!(pane(&host), "notifications");

    invoke(&host, "open_pane", vec![Value::text("external_accounts")]);
    assert_eq!(pane(&host), "accounts", "the index's page slug is Accounts");

    invoke(&host, "open_pane", vec![Value::text("keyboard")]);
    assert_eq!(pane(&host), "input");

    invoke(&host, "open_pane", vec![Value::text("displays")]);
    assert_eq!(pane(&host), "displays");
}
