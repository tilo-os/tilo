//! Shortcuts' draft editor opens from New shortcut: the control the walk presses starts a draft,
//! and the trigger form stands in it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::path::Path;

use tilolang_runtime::{Actor, ViewInstance};
use tilolang_tilo::host::Host;

const APP: &str = "so.tilo.shortcuts";

/// The app as it ships, its window drawn: a host with no session publishes nothing else.
fn window() -> (Host, ViewInstance) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/shortcuts");
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
    tilolang_tilo::platform::load(host.runtime(), &compiled.meta.uses);
    host.root_of(APP).expect("the workbench exists");
    let view = host.open_main(APP).expect("the window opens");
    (host, view)
}

/// The key of the control of `kind` whose label `label` reads.
fn labelled(tree: &tilolang_runtime::UiNode, kind: &str, label: &str) -> String {
    tree.of_kind(kind)
        .into_iter()
        .find(|node| {
            node.props.get("label").map(|value| match value {
                tilolang_runtime::Value::Text(text) => text == label,
                _ => false,
            }) == Some(true)
        })
        .unwrap_or_else(|| panic!("a {kind} labelled {label}"))
        .key
        .clone()
}

/// Every text the window draws.
fn texts(view: &ViewInstance) -> Vec<String> {
    crate::fixtures::nodes(view.tree())
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            tilolang_runtime::Value::Text(text) => Some(text.clone()),
            tilolang_runtime::Value::Rich(rich) => Some(rich.written_text()),
            _ => None,
        })
        .collect()
}

/// New shortcut opens the draft it makes: the trigger form, its control and the step editor
/// stand in the window, where the person sets what starts the shortcut and what it does.
#[test]
fn new_shortcut_opens_the_draft_it_makes() {
    let (_host, mut view) = window();
    for _ in 0..4 {
        view.poll_patches().expect("the window draws");
    }
    let new = labelled(view.tree(), "ui.Button", "New shortcut");
    view.press(&new).expect("a draft starts");
    for _ in 0..8 {
        view.poll_patches().expect("the window draws");
    }
    let shown = texts(&view);
    assert!(
        shown.iter().any(|line| line == "Add trigger") || shown.iter().any(|line| line == "Apply"),
        "the draft's trigger form stands in the window: {shown:?}"
    );
    labelled(view.tree(), "ui.Button", "Review and save…");
}
