//! The shortcuts a person builds, as the window and the actions change them: a draft starts empty
//! and is named once, its steps add into blocks with readable ids and move deeper and back out,
//! its inputs are declared once, a shortcut opened from the library or a copy of it keeps what it
//! came with, and a shortcut imported from outside starts nothing until it is saved.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::path::Path;

use serde_json::json;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{ResourceRef, Value, ViewInstance};
use tilolang_tilo::host::Host;

use crate::fixtures::{Replica, Workspace, target};

const APP: &str = "so.tilo.shortcuts";

/// What answers Shortcuts' calls of the session's action registry, as the device's dispatch
/// would: the catalogue offers no action here, or says why it could not be read, and the checker
/// reads a draft that has neither a problem nor a grant.
#[derive(Default)]
struct Answers {
    /// Why the catalogue could not be read, for a test of that line; empty for a catalogue read.
    failure: String,
}

/// The invocation record `actions.call` answers: a call that ended, its output the checker's
/// answer for a draft with neither a problem nor a grant.
fn answered_call() -> Value {
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("Invocation.id".to_owned(), Value::text("call-1"));
    fields.insert(
        "Invocation.status".to_owned(),
        Value::Enum(tilolang_runtime::EnumValue {
            ty: "tilo.invocations#Status".to_owned(),
            variant: "Status.ended".to_owned(),
            fields: std::collections::BTreeMap::new(),
        }),
    );
    fields.insert(
        "Invocation.output".to_owned(),
        Value::some(Value::Json(Box::new(json!({
            "grant": "[]",
            "problems": [],
            "permissions": [],
        })))),
    );
    fields.insert("Invocation.started_ms".to_owned(), Value::Int(1));
    Value::Rec(tilolang_runtime::value::RecordInit {
        ty: "tilo.invocations#Invocation".to_owned(),
        fields,
    })
}

/// The `Catalogue` record the action registry answers with, the offers and the failure beside them.
fn catalogue(failure: &str) -> Value {
    let mut fields = std::collections::BTreeMap::new();
    fields.insert(
        "actions-catalogue-offers".to_owned(),
        Value::List(Vec::new()),
    );
    fields.insert("actions-catalogue-failure".to_owned(), Value::text(failure));
    Value::Rec(tilolang_runtime::value::RecordInit {
        ty: "actions-catalogue".to_owned(),
        fields,
    })
}

impl Invoker for Answers {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        let Invocation::Action { app, name, .. } = invocation else {
            return Invoked::Refused("this test answers actions alone".to_owned());
        };
        if app == tilolang::ACTIONS_APP {
            return match name.as_str() {
                "catalogue" => Invoked::Done(catalogue(&self.failure)),
                // The checker's calls: the inspect and choices addresses answer the invocation
                // record whose output is a clean draft.
                "call" => Invoked::Done(answered_call()),
                _ => Invoked::Refused("this test calls no action by address".to_owned()),
            };
        }
        if app == APP {
            return match name.as_str() {
                // The checker finds nothing to fix and nothing to allow.
                "inspect" => Invoked::Done(Value::Json(Box::new(json!({
                    "grant": "[]",
                    "problems": [],
                    "permissions": [],
                })))),
                // An argument may be bound to nothing the test checks.
                "choices" => Invoked::Done(Value::List(Vec::new())),
                _ => Invoked::Done(Value::Unit),
            };
        }
        Invoked::Refused(format!("this test answers no app {app}"))
    }
}

const SHORTCUTS: (&str, &str) = (
    include_str!("../../../../apps/tl/shortcuts/app.tl"),
    include_str!("../../../../apps/tl/shortcuts/app.ids"),
);

/// Ada's workspace with Shortcuts, and the workbench it keeps.
fn workbench() -> (Replica, ResourceRef) {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[SHORTCUTS]);
    ada.host.runtime().set_foreign_invoker(Answers::default());
    let root = ada.root(APP);
    ada.commit();
    (ada, root)
}

/// The resources a collection holds, in the order the document keeps them.
fn items(ada: &Replica, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
    match ada
        .host
        .runtime()
        .read(at, field)
        .unwrap_or_else(|error| panic!("{field} reads: {error}"))
    {
        Value::List(items) => items.iter().map(target).collect(),
        other => panic!("{field} is a list: {other:?}"),
    }
}

/// The text `field` holds.
fn text(ada: &Replica, at: &ResourceRef, field: &str) -> String {
    match ada.host.runtime().read(at, field) {
        Ok(Value::Text(text)) => text,
        other => panic!("{field} is text: {other:?}"),
    }
}

/// A draft's program record.
fn program(ada: &Replica, draft: &ResourceRef) -> ResourceRef {
    match ada.host.runtime().read(draft, "program") {
        Ok(Value::Ref(program)) => target(&Value::Ref(program)),
        other => panic!("a draft's program: {other:?}"),
    }
}

/// The ids of a program's top-level steps, as the editor reads them: in their block's order.
fn step_ids(ada: &Replica, draft: &ResourceRef) -> Vec<String> {
    let program = program(ada, draft);
    items(ada, &program, "steps")
        .iter()
        .map(|step| text(ada, step, "step_id"))
        .collect()
}

/// The case of the enum type `ty` whose name is `name`, as a value an action takes.
fn case(ada: &Replica, ty: &str, name: &str) -> Value {
    let meta = &ada.host.package(APP).expect("the app is loaded").meta;
    let found = meta
        .enums
        .iter()
        .find(|declared| declared.name == ty)
        .unwrap_or_else(|| panic!("the {ty} enum"));
    let variant = found
        .variants
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("no {ty} case {name}"));
    Value::Enum(tilolang_runtime::EnumValue {
        ty: found.id.clone(),
        variant: variant.id.clone(),
        fields: std::collections::BTreeMap::new(),
    })
}

/// A draft in a new workbench, made through the action New shortcut presses.
fn draft(ada: &Replica, root: &ResourceRef) -> ResourceRef {
    ada.call(APP, "new_draft", vec![Value::resource(root)]);
    items(ada, root, "drafts")
        .into_iter()
        .last()
        .expect("the draft New shortcut made")
}

/// New shortcut starts a draft with nothing in it, the window opens that draft, and a second
/// press is named so the two can be told apart.
#[test]
fn a_draft_starts_empty_and_is_named_once() {
    let (ada, root) = workbench();
    assert!(
        items(&ada, &root, "drafts").is_empty(),
        "a fresh workbench is empty"
    );
    assert!(items(&ada, &root, "saved").is_empty());

    ada.call(APP, "new_draft", vec![Value::resource(&root)]);
    let drafts = items(&ada, &root, "drafts");
    assert_eq!(drafts.len(), 1);
    assert_eq!(text(&ada, &drafts[0], "name"), "New shortcut");
    assert_eq!(
        text(&ada, &root, "opened"),
        text(&ada, &drafts[0], "shortcut_id"),
        "the window opens the draft the action made"
    );
    assert!(
        step_ids(&ada, &drafts[0]).is_empty(),
        "a draft starts with no steps"
    );

    ada.call(APP, "new_draft", vec![Value::resource(&root)]);
    let names: Vec<String> = items(&ada, &root, "drafts")
        .iter()
        .map(|draft| text(&ada, draft, "name"))
        .collect();
    assert_eq!(names, ["New shortcut", "New shortcut 2"]);

    ada.call(
        APP,
        "discard",
        vec![Value::resource(&root), Value::resource(&drafts[0])],
    );
    let names: Vec<String> = items(&ada, &root, "drafts")
        .iter()
        .map(|draft| text(&ada, draft, "name"))
        .collect();
    assert_eq!(names, ["New shortcut 2"], "Discard drafts the one it names");
}

/// Steps add into blocks with a readable id each, an `if` holds the step under it, and the step
/// lifts back out after it.
#[test]
fn steps_add_into_blocks_and_nest_and_lift() {
    let (ada, root) = workbench();
    let draft = draft(&ada, &root);
    for head in ["branch", "each", "branch"] {
        let block = case(&ada, "Head", head);
        ada.call(
            APP,
            "add_block",
            vec![Value::resource(&root), Value::resource(&draft), block],
        );
    }
    assert_eq!(
        step_ids(&ada, &draft),
        ["if", "for_each", "if2"],
        "each step is named by what it does, once"
    );

    let program = program(&ada, &draft);
    let steps = items(&ada, &program, "steps");
    let each = steps[1].clone();
    ada.call(
        APP,
        "nest_step",
        vec![
            Value::resource(&root),
            Value::resource(&draft),
            Value::resource(&each),
        ],
    );
    let parent = match ada.host.runtime().read(&each, "parent") {
        Ok(Value::Opt(Some(parent))) => target(&parent),
        other => panic!("the nested step's parent: {other:?}"),
    };
    assert_eq!(parent, steps[0], "the step moved into the if above it");

    ada.call(
        APP,
        "lift_step",
        vec![
            Value::resource(&root),
            Value::resource(&draft),
            Value::resource(&each),
        ],
    );
    match ada.host.runtime().read(&each, "parent") {
        Ok(Value::Opt(None)) => {}
        other => panic!("the step lifted back out: {other:?}"),
    }

    ada.call(
        APP,
        "move_step",
        vec![
            Value::resource(&root),
            Value::resource(&draft),
            Value::resource(&steps[2]),
            Value::Bool(true),
        ],
    );
    assert_eq!(
        step_ids(&ada, &draft),
        ["if", "if2", "for_each"],
        "a step moves over the one above it"
    );

    ada.call(
        APP,
        "remove_step",
        vec![
            Value::resource(&root),
            Value::resource(&draft),
            Value::resource(&steps[2]),
        ],
    );
    assert_eq!(step_ids(&ada, &draft), ["if", "for_each"]);
}

/// A step's policy is set through the action the editor's own control calls, and kept where the
/// run reads it.
#[test]
fn a_steps_policy_is_set_through_the_action() {
    let (ada, root) = workbench();
    let draft = draft(&ada, &root);
    ada.call(
        APP,
        "add_block",
        vec![
            Value::resource(&root),
            Value::resource(&draft),
            case(&ada, "Head", "branch"),
        ],
    );
    let program = program(&ada, &draft);
    let step = items(&ada, &program, "steps").remove(0);
    ada.call(
        APP,
        "set_policy",
        vec![
            Value::resource(&root),
            Value::resource(&draft),
            Value::resource(&step),
            case(&ada, "Policy", "retry"),
        ],
    );
    match ada.host.runtime().read(&step, "policy") {
        Ok(Value::Enum(value)) => {
            let meta = &ada.host.package(APP).unwrap().meta;
            let ty = meta
                .enums
                .iter()
                .find(|ty| ty.id == value.ty)
                .expect("the policy enum");
            let name = ty
                .variants
                .iter()
                .find(|variant| variant.id == value.variant)
                .expect("the policy case")
                .name
                .clone();
            assert_eq!(name, "retry");
        }
        other => panic!("the step's policy: {other:?}"),
    }
}

/// An input is declared once: a second of the same name changes nothing and says why.
#[test]
fn an_input_is_declared_once() {
    let (ada, root) = workbench();
    let draft = draft(&ada, &root);
    let text_type = case(&ada, "InputType", "text");
    ada.call(
        APP,
        "add_input",
        vec![
            Value::resource(&root),
            Value::resource(&draft),
            Value::text("folder"),
            text_type.clone(),
        ],
    );
    ada.call(
        APP,
        "add_input",
        vec![
            Value::resource(&root),
            Value::resource(&draft),
            Value::text("folder"),
            text_type,
        ],
    );
    let program = program(&ada, &draft);
    assert_eq!(items(&ada, &program, "inputs").len(), 1);
}

/// The JSON of a saved shortcut with one key chord, as the library hands one to the app.
fn saved_json(id: &str, chord: &str) -> String {
    format!(
        "{{\"id\":\"{id}\",\"name\":\"Tidy notes\",\"steps\":[{{\"id\":\"list\",\"step\":\"invoke\",\
         \"action\":{{\"app\":\"so.tilo.files\",\"kind\":\"\",\"action\":\"list\"}},\
         \"resource\":{{\"fixed\":\"\"}}}}],\
         \"triggers\":[{{\"trigger\":\"keyboard\",\"chord\":\"{chord}\"}}]}}"
    )
}

/// The chords of a draft's triggers, in the order the document keeps them.
fn chords(ada: &Replica, draft: &ResourceRef) -> Vec<String> {
    let program = program(ada, draft);
    items(ada, &program, "triggers")
        .iter()
        .map(|trigger| text(ada, trigger, "chord"))
        .collect()
}

/// Editing a saved shortcut opens the shortcut itself, triggers and all, and a copy of it keeps
/// what the saved one came with under a key of its own.
#[test]
fn an_opened_or_duplicated_shortcut_keeps_its_triggers() {
    let (ada, root) = workbench();
    ada.call(
        APP,
        "open_shortcut",
        vec![
            Value::resource(&root),
            Value::text(saved_json("saved-1", "Super+Shift+N")),
        ],
    );
    let opened = items(&ada, &root, "drafts").remove(0);
    assert_eq!(text(&ada, &opened, "shortcut_id"), "saved-1");
    assert_eq!(
        chords(&ada, &opened),
        ["Super+Shift+N"],
        "a shortcut opened to change keeps the trigger it came with"
    );
    match ada.host.runtime().read(&opened, "replaces_saved") {
        Ok(Value::Bool(true)) => {}
        other => panic!("the draft replaces the saved shortcut: {other:?}"),
    }

    ada.call(
        APP,
        "duplicate_shortcut",
        vec![
            Value::resource(&root),
            Value::text(saved_json("saved-1", "Super+Shift+N")),
        ],
    );
    let drafts = items(&ada, &root, "drafts");
    let copy = drafts.last().expect("the copy").clone();
    assert_ne!(
        text(&ada, &copy, "shortcut_id"),
        "saved-1",
        "the copy is a shortcut of its own"
    );
    assert_eq!(text(&ada, &copy, "name"), "Tidy notes copy");
    assert_eq!(
        chords(&ada, &copy),
        ["Super+Shift+N"],
        "the copy keeps the trigger"
    );
}

/// Duplicate makes a variant of the draft it copies: a new draft with the draft's steps, named
/// after it, opening beside it.
#[test]
fn duplicating_a_draft_makes_a_variant_named_after_it() {
    let (ada, root) = workbench();
    let draft = draft(&ada, &root);
    ada.call(
        APP,
        "add_block",
        vec![
            Value::resource(&root),
            Value::resource(&draft),
            case(&ada, "Head", "branch"),
        ],
    );
    ada.call(
        APP,
        "duplicate_draft",
        vec![Value::resource(&root), Value::resource(&draft)],
    );
    let drafts = items(&ada, &root, "drafts");
    assert_eq!(drafts.len(), 2, "Duplicate starts a variant");
    let copy = drafts.last().expect("the copy").clone();
    assert_eq!(text(&ada, &copy, "name"), "New shortcut copy");
    assert_eq!(step_ids(&ada, &copy), ["if"], "the variant keeps the steps");
    assert_ne!(
        text(&ada, &copy, "shortcut_id"),
        text(&ada, &draft, "shortcut_id"),
        "the variant is a shortcut of its own"
    );
}

/// A shortcut imported from outside starts nothing until a person saves it: its steps are kept
/// and its triggers are not armed.
#[test]
fn an_imported_shortcut_arms_nothing() {
    let (ada, root) = workbench();
    ada.call(
        APP,
        "draft_shortcut",
        vec![
            Value::resource(&root),
            Value::text(saved_json("outside-1", "Super+Shift+N")),
        ],
    );
    let imported = items(&ada, &root, "drafts").remove(0);
    assert!(
        chords(&ada, &imported).is_empty(),
        "an imported shortcut arms no trigger"
    );
    assert_eq!(
        step_ids(&ada, &imported),
        ["list"],
        "its steps are kept for the person to review"
    );
}

/// The window as it ships, its draft opened by the control the walk presses.
fn window() -> (Host, ViewInstance) {
    window_answering(Answers::default())
}

/// The window on a host whose registry answers as `answers` says.
fn window_answering(answers: Answers) -> (Host, ViewInstance) {
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
        tilolang_runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .expect("a host");
    host.load_compiled(&compiled).expect("the package loads");
    tilolang_tilo::platform::load(host.runtime(), &compiled.meta.uses);
    host.runtime().set_foreign_invoker(answers);
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
                Some(Value::Text(text)) if text == label
            )
        })
        .unwrap_or_else(|| panic!("a {kind} labelled {label}"))
        .key
        .clone()
}

/// Every text the window draws.
fn shown(view: &ViewInstance) -> Vec<String> {
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

/// Run one of the app's actions as the person, as the window's own timer would when it comes due.
fn act(host: &Host, action: &str, args: Vec<Value>) {
    let meta = &host.package(APP).expect("the app is loaded").meta;
    let id = crate::fixtures::action_id(meta, action);
    let actor = tilolang_runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"));
    match host.runtime().invoke_action(&actor, (id.as_str(), args)) {
        Ok(tilolang_runtime::InvokeOutcome::Done(_)) => {}
        other => panic!("{action}: {other:?}"),
    }
}

/// Poll the window until `done` holds, for up to two seconds: what an action did shows in the
/// frame after it.
fn wait_until(view: &mut ViewInstance, done: impl Fn(&ViewInstance) -> bool) {
    // A thousand looks, a moment apart: two seconds of frames.
    for _ in 0..1000 {
        view.poll_patches().expect("the window draws");
        if done(view) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!(
        "the window never drew what the test waits for: {:?}",
        shown(view)
    );
}

/// Add step opens the picker, which says plainly when no app offers actions: the person is not
/// left with an empty sheet and no words.
#[test]
fn the_picker_says_when_no_app_offers_actions() {
    let (host, mut view) = window();
    for _ in 0..8 {
        view.poll_patches().expect("the window draws");
    }
    let new = labelled(view.tree(), "ui.Button", "New shortcut");
    view.press(&new).expect("a draft starts");
    for _ in 0..8 {
        view.poll_patches().expect("the window draws");
    }
    let add = labelled(view.tree(), "ui.Button", "Add step");
    view.press(&add).expect("the picker opens");
    // The picker opens at once and the catalogue is read a moment later: it says what it is
    // doing, then what it found. The window host holds that moment's timer; the test takes it.
    for _ in 0..2 {
        view.poll_patches().expect("the window draws");
    }
    let lines = shown(&view);
    assert!(
        lines
            .iter()
            .any(|line| line == "Reading the actions your apps offer…"),
        "the picker says it is reading the catalogue: {lines:?}"
    );
    let root = host.root_of(APP).expect("the workbench");
    act(&host, "read_picker", vec![Value::resource(&root)]);
    wait_until(&mut view, |view| {
        shown(view)
            .iter()
            .any(|line| line == "No app offers actions here yet.")
    });
    let lines = shown(&view);
    assert!(
        lines
            .iter()
            .any(|line| line == "No app offers actions here yet."),
        "the picker says no app offers actions: {lines:?}"
    );
    for head in [
        "If",
        "Repeat with each",
        "Set variable",
        "Ask for a value",
        "Ask the agent once",
        "Start a task",
    ] {
        assert!(
            lines.iter().any(|line| line == head),
            "the picker offers the block {head}: {lines:?}"
        );
    }
}

/// Review and save opens the sheet that says what saving would allow, with its answers on it.
#[test]
fn the_draft_offers_its_save_review() {
    let (_host, mut view) = window();
    for _ in 0..8 {
        view.poll_patches().expect("the window draws");
    }
    let new = labelled(view.tree(), "ui.Button", "New shortcut");
    view.press(&new).expect("a draft starts");
    for _ in 0..8 {
        view.poll_patches().expect("the window draws");
    }
    let review = labelled(view.tree(), "ui.Button", "Review and save…");
    view.press(&review).expect("the review opens");
    wait_until(&mut view, |view| {
        shown(view).iter().any(|line| line == "Save this shortcut?")
    });
    let lines = shown(&view);
    assert!(
        lines.iter().any(|line| line == "Cancel"),
        "the review can be put away: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "Save shortcut" || line == "Close"),
        "the review says how it ends: {lines:?}"
    );
}

/// A catalogue that could not be read says so where its actions would stand, with the dispatch's
/// own words beside it, rather than leaving the picker empty.
#[test]
fn the_picker_says_when_the_actions_could_not_be_read() {
    let (host, mut view) = window_answering(Answers {
        failure: "the action registry is not connected".to_owned(),
    });
    for _ in 0..8 {
        view.poll_patches().expect("the window draws");
    }
    let new = labelled(view.tree(), "ui.Button", "New shortcut");
    view.press(&new).expect("a draft starts");
    for _ in 0..8 {
        view.poll_patches().expect("the window draws");
    }
    let add = labelled(view.tree(), "ui.Button", "Add step");
    view.press(&add).expect("the picker opens");
    for _ in 0..2 {
        view.poll_patches().expect("the window draws");
    }
    let root = host.root_of(APP).expect("the workbench");
    act(&host, "read_picker", vec![Value::resource(&root)]);
    wait_until(&mut view, |view| {
        shown(view)
            .iter()
            .any(|line| line == "The actions your apps offer could not be read")
    });
    let lines = shown(&view);
    assert!(
        lines
            .iter()
            .any(|line| line == "the action registry is not connected"),
        "the picker says why: {lines:?}"
    );
    for head in ["If", "Repeat with each", "Set variable"] {
        assert!(
            lines.iter().any(|line| line == head),
            "the blocks still stand: {head}: {lines:?}"
        );
    }
}

/// Duplicate stands beside the draft it copies, in the window the person works in: the control
/// the window draws is there to press.
#[test]
fn the_open_draft_offers_duplicate() {
    let (_host, mut view) = window();
    for _ in 0..8 {
        view.poll_patches().expect("the window draws");
    }
    let new = labelled(view.tree(), "ui.Button", "New shortcut");
    view.press(&new).expect("a draft starts");
    for _ in 0..8 {
        view.poll_patches().expect("the window draws");
    }
    labelled(view.tree(), "ui.Button", "Duplicate");
    for label in ["Review and save…", "Discard"] {
        labelled(view.tree(), "ui.Button", label);
    }
}
