//! An installed bundle runs the packages it uses: their types resolve, their views draw and
//! their actions run on the resources of theirs it holds, as Today draws the todo list's rows.

use automerge::AutoCommit;
use tilolang_runtime::{Principal, UiNode, Value};
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::host::Host;

/// Today compiled with the packages it uses, as `tilolang build` bundles it.
fn today() -> Bundle {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/today");
    let dirs = tilolang::source::dependency_order(&dir, &[]).unwrap();
    let built =
        tilolang::source::compile_dirs(&dirs, &[], &mut tilolang::OsEntropy::default()).unwrap();
    let (today, used) = built.split_last().unwrap();
    Bundle::new(&today.compiled, used.iter().map(|used| &used.compiled))
}

/// Every text `node` and the nodes inside it show.
fn texts_of(node: &UiNode, out: &mut Vec<String>) {
    out.extend(node.props.values().filter_map(|value| match value {
        Value::Text(text) => Some(text.clone()),
        _ => None,
    }));
    for child in &node.children {
        texts_of(child, out);
    }
}

/// The keys of the lists under `node`.
fn lists(node: &UiNode, out: &mut Vec<String>) {
    if node.kind == "list" {
        out.push(node.key.clone());
    }
    for child in &node.children {
        lists(child, out);
    }
}

#[test]
fn a_bundle_draws_the_rows_of_the_packages_it_uses() {
    let doc = AutoCommit::new();
    let host = Host::new(&doc, Principal::new("ada"), || None).unwrap();
    let bundle = today();
    host.load_bundle(&bundle).unwrap();
    let ada = Principal::new("ada");
    // The todo list's own first scenario fills it, then Today's attaches it.
    for import in &bundle.imports {
        host.root_of(&import.meta.app).unwrap();
        let first = import.meta.scenarios.first().unwrap();
        host.runtime()
            .invoke_action(&ada, (first.action.as_str(), vec![]))
            .unwrap();
    }
    host.root_of(&bundle.meta.app).unwrap();
    let scenario = bundle.meta.scenarios.first().unwrap();
    host.runtime()
        .invoke_action(&ada, (scenario.action.as_str(), vec![]))
        .unwrap();
    let mut view = host.open_main(&bundle.meta.app).unwrap();
    let mut keys = Vec::new();
    lists(view.tree(), &mut keys);
    let mut shown = Vec::new();
    texts_of(view.tree(), &mut shown);
    for key in &keys {
        for i in 0..view.list_len(key).unwrap_or(0) {
            if let Some(row) = view.row(key, i).unwrap() {
                texts_of(row, &mut shown);
            }
        }
    }
    for text in ["Buy oat milk and eggs", "Weekend plans"] {
        assert!(
            shown.iter().any(|shown| shown.contains(text)),
            "Today draws `{text}` with the view of the package it belongs to: {shown:?}"
        );
    }
}
