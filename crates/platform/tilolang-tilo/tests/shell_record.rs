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
use tilolang_runtime::origin::OriginWrite;
use tilolang_runtime::{InvokeOutcome, Principal, Value};
use tilolang_tilo::record::DynValue;

use crate::fixtures::Replica;

const FILES: [&str; 21] = [
    include_str!("../../../../apps/tl/shell/app.tl"),
    include_str!("../../../../apps/tl/shell/arrangement.tl"),
    include_str!("../../../../apps/tl/shell/arranging.tl"),
    include_str!("../../../../apps/tl/shell/canvas.tl"),
    include_str!("../../../../apps/tl/shell/chrome.tl"),
    include_str!("../../../../apps/tl/shell/composer.tl"),
    include_str!("../../../../apps/tl/shell/desk.tl"),
    include_str!("../../../../apps/tl/shell/docks.tl"),
    include_str!("../../../../apps/tl/shell/launcher.tl"),
    include_str!("../../../../apps/tl/shell/layout.tl"),
    include_str!("../../../../apps/tl/shell/notices.tl"),
    include_str!("../../../../apps/tl/shell/overview.tl"),
    include_str!("../../../../apps/tl/shell/parts.tl"),
    include_str!("../../../../apps/tl/shell/people.tl"),
    include_str!("../../../../apps/tl/shell/presets.tl"),
    include_str!("../../../../apps/tl/shell/rooms.tl"),
    include_str!("../../../../apps/tl/shell/scenarios.tl"),
    include_str!("../../../../apps/tl/shell/session.tl"),
    include_str!("../../../../apps/tl/shell/strips.tl"),
    include_str!("../../../../apps/tl/shell/switcher.tl"),
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

/// Ada's room with the shell, compiled from every file of the package as the session compiles
/// it, with the ids lock committed beside it.
fn room() -> Replica {
    let source = source();
    let (mut replica, _) = Replica::new("ada", AutoCommit::new(), &[(source.as_str(), IDS)]);
    // The person's shell record, made as the session makes it, and the package's migration
    // seeding it: the bar's parts and the dock's items, on a shell never arranged.
    let meta = replica.host.package(APP).unwrap().meta;
    for root in &meta.named_roots {
        replica
            .host
            .root_record(APP, &root.ty)
            .expect("the shell record is made");
    }
    replica.host.migrate();
    replica.commit();
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

/// What a field of the shell's record holds now, by its name, as the record holds it: an
/// enum's case is its id, a flag a `Bool`.
fn field(r: &Replica, name: &str) -> DynValue {
    let meta = r.host.package(APP).unwrap().meta;
    let id = crate::fixtures::field_id(&meta, "Shell", name);
    shell(r)
        .get(&id)
        .cloned()
        .unwrap_or(DynValue::Register(serde_json::Value::Null))
}

/// The `Shell` record as the person holds it now: the package's named root.
fn shell(r: &Replica) -> tilolang_tilo::record::DynRecord {
    let meta = r.host.package(APP).unwrap().meta;
    let ty = meta
        .named_roots
        .iter()
        .find(|root| root.name == "shell")
        .map(|root| root.ty.clone())
        .expect("the shell declares its record as a named root");
    r.host.root_record(APP, &ty).unwrap();
    r.host
        .store()
        .record(&format!("root-{ty}"))
        .expect("the shell record is made")
}

/// A case of an enum the record keeps, as the record holds it: its id.
fn case(r: &Replica, ty: &str, variant: &str) -> String {
    let meta = r.host.package(APP).unwrap().meta;
    meta.enums
        .iter()
        .find(|en| en.name == ty)
        .and_then(|en| en.variants.iter().find(|v| v.name == variant))
        .map_or_else(|| panic!("no {ty}.{variant}"), |v| v.id.clone())
}

/// The `variant` case of the enum `ty`, as an action argument takes it.
fn pick(r: &Replica, ty: &str, variant: &str) -> Value {
    let meta = r.host.package(APP).unwrap().meta;
    let en = meta
        .enums
        .iter()
        .find(|en| en.name == ty)
        .unwrap_or_else(|| panic!("no enum {ty}"));
    let case = en
        .variants
        .iter()
        .find(|v| v.name == variant)
        .unwrap_or_else(|| panic!("no {ty}.{variant}"));
    Value::Enum(tilolang_runtime::value::EnumValue {
        ty: en.id.clone(),
        variant: case.id.clone(),
        fields: std::collections::BTreeMap::new(),
    })
}

/// Whether the enum field `name` holds the case `variant` of `ty` now. A register holds an
/// enum's case as its variant's id, which is what the record keeps.
fn is(r: &Replica, name: &str, ty: &str, variant: &str) -> bool {
    let want = case(r, ty, variant);
    match field(r, name) {
        // A register holds an enum's case as `{"enum": <type id>, "variant": <case id>}`.
        DynValue::Register(serde_json::Value::Object(held)) => {
            held.get("variant").and_then(|v| v.as_str()) == Some(want.as_str())
        }
        DynValue::Register(serde_json::Value::String(held)) => held == want,
        other => panic!("{name} does not hold an enum case: {other:?}"),
    }
}

/// Whether the shell's own record says `flag`.
fn flag(r: &Replica, name: &str) -> bool {
    let meta = r.host.package(APP).unwrap().meta;
    let id = crate::fixtures::field_id(&meta, "Shell", name);
    matches!(
        shell(r).get(&id),
        Some(DynValue::Register(serde_json::Value::Bool(true)))
    )
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
        vec![shell_ref(&r), pick(&r, "Finish", "translucent")],
    );
    r.commit();
    assert!(is(&r, "bar_finish", "Finish", "translucent"));
    assert!(flag(&r, "tuned"), "a change is the person's");

    undo(&mut r);
    assert!(is(&r, "bar_finish", "Finish", "solid"), "undo puts it back");
    assert!(!flag(&r, "tuned"));
}

#[test]
fn applying_a_preset_changes_every_setting_it_names_and_undo_restores_them_all() {
    let mut r = room();
    r.commit();
    choose(
        &r,
        "apply",
        vec![shell_ref(&r), pick(&r, "Preset", "macos")],
    );
    r.commit();
    assert!(is(&r, "preset", "Preset", "macos"));
    assert!(is(&r, "dock_showing", "Showing", "no_overlap"));
    assert!(is(&r, "dock_fit", "Fit", "floating"));
    assert!(flag(&r, "dock_zoom"), "macOS's Dock magnifies");

    undo(&mut r);
    assert!(is(&r, "preset", "Preset", "tilo"), "the preset goes back");
    assert!(is(&r, "dock_showing", "Showing", "always"));
    assert!(!flag(&r, "dock_zoom"));
}

// ----- What a person sees: the presets, and when the chrome shows ---------------------------

/// The `Desk` record the shell's root view reads: the package's unnamed root.
fn desk(r: &Replica) -> Value {
    let root = r
        .host
        .root_of_layer(
            APP,
            &tilolang_tilo::host::RootView {
                root: Some("Desk".to_owned()),
                view: None,
            },
        )
        .expect("the desk's root is made");
    Value::resource(&root)
}

/// The person's shell record: the package's named root, `root shell: Shell`.
fn shell_ref(r: &Replica) -> Value {
    let meta = r.host.package(APP).unwrap().meta;
    let ty = meta
        .named_roots
        .iter()
        .find(|root| root.name == "shell")
        .map(|root| root.ty.clone())
        .expect("the shell declares its record as a named root");
    Value::resource(&r.host.root_record(APP, &ty).unwrap())
}

/// The chrome the session shows (the desk's default view, `desk`), laid out as the person
/// sees it: the bar and the dock over the desk and the person's shell record.
fn desk_view(r: &Replica) -> tilolang_runtime::ViewInstance {
    r.host
        .open_layer(
            APP,
            &tilolang_tilo::host::RootView {
                root: Some("Desk".to_owned()),
                view: None,
            },
        )
        .expect("the desk opens")
}

/// Every node of `view`'s tree that is `kind`, depth first: what a preset's layout is read
/// from (which `ui.DragRun`s it has, on which axis).
fn nodes(view: &tilolang_runtime::ViewInstance, kind: &str) -> Vec<tilolang_runtime::UiNode> {
    fn walk(node: &tilolang_runtime::UiNode, kind: &str, out: &mut Vec<tilolang_runtime::UiNode>) {
        if node.kind == kind {
            out.push(node.clone());
        }
        for child in &node.children {
            walk(child, kind, out);
        }
    }
    let mut out = Vec::new();
    walk(view.tree(), kind, &mut out);
    out
}

/// Whether the view says `words` anywhere: a dock item's or a bar part's name.
fn says(view: &tilolang_runtime::ViewInstance, words: &str) -> bool {
    format!("{:?}", view.tree()).contains(words)
}

/// Ada picks the preset `preset`, as pressing its card in the settings does.
fn use_preset(r: &mut Replica, preset: &str) {
    choose(r, "apply", vec![shell_ref(r), pick(r, "Preset", preset)]);
    r.commit();
}

#[test]
fn choosing_a_preset_changes_the_layout_the_person_sees() {
    let mut r = room();
    r.commit();
    // The default: a bar at the top with its parts, and a dock at the bottom along its
    // horizontal run.
    let default = desk_view(&r);
    let runs = nodes(&default, "ui.DragRun");
    let card = |view: &tilolang_runtime::ViewInstance| format!("{:?}", view.tree());

    // The full-width classic bar, always shown: no dock, so no `ui.DragRun` on the desk at
    // all — the bar's own run is there, the dock's is not.
    use_preset(&mut r, "windows");
    assert!(is(&r, "preset", "Preset", "windows"));
    assert!(!flag(&r, "bar_shown"), "the Windows preset has no top bar");
    assert!(flag(&r, "dock_shown"), "and its dock is the taskbar");
    assert!(is(&r, "dock_fit", "Fit", "full"));
    let windows = card(&desk_view(&r));
    assert!(
        windows.contains("Launcher"),
        "the dock's items show on the desk: {windows}"
    );
    assert!(
        windows.contains("4:28 PM") && windows.contains("Notifications"),
        "the taskbar carries the time and the device's status beside its items: {windows}"
    );

    // The macOS-like one: a bar with the app's menus and a dock, both translucent.
    use_preset(&mut r, "macos");
    assert!(is(&r, "bar_finish", "Finish", "translucent"));
    assert!(is(&r, "dock_showing", "Showing", "no_overlap"));
    let macos = desk_view(&r);
    assert!(
        says(&macos, "File"),
        "the macOS bar carries the app's menus: {}",
        card(&macos)
    );

    // The minimal one: the bar hides until reached for, and the dock is off.
    use_preset(&mut r, "minimal");
    assert!(is(&r, "bar_showing", "Showing", "edge_reveal"));
    assert!(!flag(&r, "dock_shown"));
    // Nothing reached for: no bar at rest.
    let minimal = desk_view(&r);
    assert!(
        !says(&minimal, "Notifications"),
        "the minimal shell has no chrome at rest: {}",
        card(&minimal)
    );

    // And back: the Tilo default, the bar always shown again.
    use_preset(&mut r, "tilo");
    assert!(is(&r, "bar_showing", "Showing", "always"));
    assert!(
        says(&desk_view(&r), "Notifications"),
        "the bar is back at rest"
    );
    assert!(runs.len() >= 1, "the dock and the bar are runs");
}

#[test]
fn a_bar_that_hides_until_reached_for_shows_when_the_pointer_comes_to_the_edge() {
    let mut r = room();
    r.commit();
    use_preset(&mut r, "minimal");
    assert!(is(&r, "bar_showing", "Showing", "edge_reveal"));
    // Nothing reached for yet: the bar is not drawn.
    assert!(!says(&desk_view(&r), "Notifications"));
    // The session says the pointer has come to the top edge (what it publishes when the
    // pointer rests there).
    set_screen(&mut r, "bar_reached", true);
    assert!(
        says(&desk_view(&r), "Notifications"),
        "the bar shows when the pointer comes to its edge"
    );
    // And hides again when it leaves.
    set_screen(&mut r, "bar_reached", false);
    assert!(!says(&desk_view(&r), "Notifications"));
}

#[test]
fn a_bar_that_hides_when_a_window_overlaps_it_hides_only_while_one_does() {
    let mut r = room();
    r.commit();
    use_preset(&mut r, "macos");
    assert!(is(&r, "dock_showing", "Showing", "no_overlap"));
    // Nothing overlaps it: the dock's items show.
    assert!(says(&desk_view(&r), "Launcher"), "the dock shows at rest");
    // A window stands where the dock rides.
    set_screen(&mut r, "dock_covered", true);
    assert!(
        !says(&desk_view(&r), "Launcher"),
        "the dock hides while a window covers it"
    );
    // The window goes: the dock comes back.
    set_screen(&mut r, "dock_covered", false);
    assert!(
        says(&desk_view(&r), "Launcher"),
        "and shows once it is clear"
    );
}

/// The session writes what the screen says of its edges, as it does every turn: one fact on
/// the shell's own record, through the origin write the session's door applies
/// (`SessionLink::publish_edges`), which the host then commits.
fn set_screen(r: &mut Replica, field: &str, on: bool) {
    let meta0 = r.host.package(APP).unwrap().meta;
    let ty = meta0
        .named_roots
        .iter()
        .find(|root| root.name == "shell")
        .map(|root| root.ty.clone())
        .expect("the shell declares its record as a named root");
    let root = r.host.root_record(APP, &ty).unwrap();
    let meta = r.host.package(APP).unwrap().meta;
    let id = crate::fixtures::field_id(&meta, "Shell", field);
    let writes = vec![OriginWrite::Set {
        target: root,
        fields: vec![(id, Value::Bool(on))],
    }];
    r.host
        .runtime()
        .apply_origin("session", writes, None)
        .expect("the screen's facts are the session's to write");
    r.commit();
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
