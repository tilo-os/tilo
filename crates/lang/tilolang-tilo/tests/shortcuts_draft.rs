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
            matches!(
                node.props.get("label"),
                Some(tilolang_runtime::Value::Text(text)) if text == label
            )
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
    // Draft it waits for something to be described: an empty press is answered with what is
    // missing, not with an error in the window.
    let _ = view.press(&labelled(view.tree(), "ui.Button", "Draft it"));
    for _ in 0..4 {
        view.poll_patches().expect("the window draws");
    }
    assert!(
        !texts(&view)
            .iter()
            .any(|line| line == "That could not be done"),
        "an empty Draft it leaves no error: {:?}",
        texts(&view)
    );
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

/// The case `label` of the case field `field`, as the press that picks it.
fn case_of(field: &tilolang_runtime::UiNode, label: &str) -> String {
    field
        .children
        .iter()
        .find(|child| {
            child.kind == "ui.EnumOption"
                && matches!(
                    child.props.get("label"),
                    Some(tilolang_runtime::Value::Text(text)) if text == label
                )
        })
        .unwrap_or_else(|| panic!("the field offers the case {label:?}: {:?}", field.children))
        .key
        .clone()
}

/// The trigger form is filled where no window shows its menu: the case field's case is pressed
/// (the field takes it through its binding), the app id is typed into the form's field, and Add
/// trigger puts the trigger in the draft, with the app the form holds.
#[test]
fn the_trigger_form_fills_and_adds_a_trigger() {
    let (_host, mut view) = window();
    for _ in 0..8 {
        view.poll_patches().expect("the window draws");
    }
    let new = labelled(view.tree(), "ui.Button", "New shortcut");
    view.press(&new).expect("a draft starts");
    for _ in 0..16 {
        view.poll_patches().expect("the window draws");
    }
    let field = labelled(view.tree(), "ui.EnumField", "What starts it");
    let started = case_of(view.find(&field).expect("the case field"), "App opened");
    let outcome = view.press(&started).expect("the case is pressed");
    assert!(
        matches!(outcome, tilolang_runtime::InvokeOutcome::Done(_)),
        "the field takes the pressed case: {outcome:?}"
    );
    for _ in 0..16 {
        view.poll_patches().expect("the window draws");
    }
    // The form asks for the app now, the case it took.
    let root = view.tree().clone();
    let input = root
        .of_kind("ui.TextInput")
        .into_iter()
        .find(|node| {
            matches!(
                node.props.get("placeholder"),
                Some(tilolang_runtime::Value::Text(text)) if text == "so.tilo.notes"
            )
        })
        .expect("the app-opened form takes an app id")
        .key
        .clone();
    view.edit(&input, &tilolang_runtime::Value::text("so.tilo.chat"))
        .expect("the form holds the app");
    view.press(&labelled(view.tree(), "ui.Button", "Add trigger"))
        .expect("the trigger is added");
    for _ in 0..16 {
        view.poll_patches().expect("the window draws");
    }
    let row = view
        .tree()
        .of_kind("ui.ListRow")
        .into_iter()
        .find(|row| {
            matches!(
                row.props.get("title"),
                Some(tilolang_runtime::Value::Text(text)) if text == "An app opens"
            )
        })
        .expect("the draft lists its trigger");
    assert_eq!(
        row.props.get("secondary"),
        Some(&tilolang_runtime::Value::text("so.tilo.chat")),
        "the trigger is for the app the form held"
    );
}
