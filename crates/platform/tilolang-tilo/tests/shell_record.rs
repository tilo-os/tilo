//! The shell's configuration is the person's own record: every change of the bar, the dock and
//! the layouts is one action on it, an agent acting for the person runs the same action, and
//! undo takes any of it back — one setting changed, and a whole preset at once.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test: a failure is a panic"
)]

use automerge::AutoCommit;
use tilolang_runtime::{InvokeOutcome, Principal, Value};

use crate::fixtures::Replica;

const FILES: [&str; 5] = [
    include_str!("../../../../apps/tl/shell/app.tl"),
    include_str!("../../../../apps/tl/shell/chrome.tl"),
    include_str!("../../../../apps/tl/shell/docks.tl"),
    include_str!("../../../../apps/tl/shell/layout.tl"),
    include_str!("../../../../apps/tl/shell/system.tl"),
];
const IDS: &str = include_str!("../../../../apps/tl/shell/app.ids");
const APP: &str = "so.tilo.shell";

/// The shell package, as the session compiles it: every file of the package together, in path
/// order.
fn source() -> String {
    let mut joined = String::new();
    for text in FILES {
        joined.push_str(text);
        joined.push('\n');
    }
    joined
}

/// Ada's room with the shell.
fn room() -> Replica {
    let source = source();
    let (replica, _) = Replica::new("ada", AutoCommit::new(), &[(APP, source.as_str())]);
    replica
}

/// Ada runs `action` with `args` on the shell, as pressing it in the settings does.
fn choose(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package(APP).unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("no action {action}"))
        .id
        .clone();
    match r
        .host
        .runtime()
        .invoke_action(&Principal::new("ada"), (id.as_str(), args))
        .unwrap()
    {
        InvokeOutcome::Done(value) => value,
        other => panic!("{action}: {other:?}"),
    }
}

/// What a field of the shell's record holds now, by its name.
fn field(r: &Replica, name: &str) -> Value {
    let meta = r.host.package(APP).unwrap().meta;
    let id = crate::fixtures::field_id(&meta, "Shell", name);
    r.root(APP).get(&id).cloned().unwrap_or(Value::None)
}

/// A case of an enum the record keeps, as the record holds it: its id.
fn case(r: &Replica, ty: &str, variant: &str) -> String {
    let meta = r.host.package(APP).unwrap().meta;
    let id = meta
        .types
        .iter()
        .find(|t| t.name == ty)
        .and_then(|t| t.fields.iter().find(|f| f.name == variant))
        .map_or_else(|| panic!("no {ty}.{variant}"), |f| f.id.clone());
    id
}

fn is(r: &Replica, name: &str, ty: &str, variant: &str) -> bool {
    let held = field(r, name);
    let want = case(r, ty, variant);
    format!("{held:?}").contains(&want)
}

#[test]
fn a_settings_action_changes_the_record_and_undo_restores_it() {
    let mut r = room();
    r.commit();
    assert!(
        is(&r, "bar_finish", "Finish", "solid"),
        "a fresh shell is solid"
    );

    choose(
        &r,
        "set_bar_finish",
        vec![
            Value::resource(&r.host.root_of(APP).unwrap()),
            Value::text(case(&r, "Finish", "translucent")),
        ],
    );
    r.commit();
    assert!(is(&r, "bar_finish", "Finish", "translucent"));
    assert_eq!(
        field(&r, "tuned"),
        Value::Bool(true),
        "a change is the person's"
    );

    undo(&mut r);
    assert!(is(&r, "bar_finish", "Finish", "solid"), "undo puts it back");
    assert_eq!(field(&r, "tuned"), Value::Bool(false));
}

#[test]
fn applying_a_preset_changes_every_setting_it_names_and_undo_restores_them_all() {
    let mut r = room();
    r.commit();
    choose(
        &r,
        "apply",
        vec![
            Value::resource(&r.host.root_of(APP).unwrap()),
            Value::text(case(&r, "Preset", "macos")),
        ],
    );
    r.commit();
    assert!(is(&r, "preset", "Preset", "macos"));
    assert!(is(&r, "dock_showing", "Showing", "no_overlap"));
    assert!(is(&r, "dock_fit", "Fit", "floating"));
    assert_eq!(
        field(&r, "dock_zoom"),
        Value::Bool(true),
        "macOS's Dock magnifies"
    );

    undo(&mut r);
    assert!(is(&r, "preset", "Preset", "tilo"), "the preset goes back");
    assert!(is(&r, "dock_showing", "Showing", "always"));
    assert_eq!(field(&r, "dock_zoom"), Value::Bool(false));
}

/// The history verb every window's Undo runs, applied as Ada: one step back.
fn undo(r: &mut Replica) {
    use collaboration::history_source::verbs::{EDIT_TYPE, HistoryEdit};
    use tilo_actions::{ActionContext, ActionInvoker, EncodedAction};
    use tilolang_tilo::instance::room::HostHistoryEdit;

    let edit = HistoryEdit::Undo {
        steps: None,
        run: None,
        by: None,
        to: None,
        step: None,
    };
    let payload = serde_json::to_vec(&edit).unwrap();
    let action: HostHistoryEdit = serde_json::from_slice(&payload).unwrap();
    let context = ActionContext::for_resource("room", "history", "history")
        .with_invoker(ActionInvoker::actor(r.name.clone()));
    tilo_actions::apply_local_with_context(
        &mut r.doc,
        &action,
        EncodedAction {
            type_id: EDIT_TYPE.to_owned(),
            payload,
        },
        Some(r.name.as_bytes()),
        context,
    )
    .expect("undo is never refused");
    r.doc.commit();
    r.host.sync_from(&r.doc);
}
