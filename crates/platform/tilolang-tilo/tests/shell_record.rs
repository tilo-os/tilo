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

const IDS: &str = include_str!("../../../../apps/tl/shell/app.ids");
const APP: &str = "so.tilo.shell";

/// The shell package, as the session compiles it: every file of the package together, in path
/// order, read from the package's directory: a list kept here goes stale as files are added
/// (it once lacked `traditional.tl`), and the test then compiles a package nothing ships.
fn source() -> String {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/shell");
    let mut joined = String::new();
    for path in tilolang::source::source_paths(&dir).unwrap() {
        joined.push_str(&std::fs::read_to_string(path).unwrap());
        joined.push('\n');
    }
    joined
}

/// Ada's workspace with the shell, compiled from every file of the package as the session compiles
/// it, with the ids lock committed beside it.
fn workspace() -> Replica {
    let source = source();
    // The shell `use`s the apps its places read (`discussion`, `notes`, `tasks`, `chat`), so
    // they compile first, as the session compiles them.
    let (mut replica, _) = Replica::new(
        "ada",
        AutoCommit::new(),
        &[
            (
                include_str!("../../../../apps/tl/discussion/app.tl"),
                include_str!("../../../../apps/tl/discussion/app.ids"),
            ),
            (
                include_str!("../../../../apps/tl/chat/app.tl"),
                include_str!("../../../../apps/tl/chat/app.ids"),
            ),
            (
                include_str!("../../../../apps/tl/notes/app.tl"),
                include_str!("../../../../apps/tl/notes/app.ids"),
            ),
            (
                crate::fixtures::TASKS_SOURCE,
                include_str!("../../../../apps/tl/tasks/app.ids"),
            ),
        ],
    );
    // It calls the session's actions (`use so.tilo.session as session`) and reads the device's
    // and Settings' published resources (the quick settings): packages the platform describes,
    // loaded as the session loads them.
    let session = tilolang_tilo::catalogue::described(
        "so.tilo.session",
        "Session",
        include_str!("../../../../apps/tl/session/surface.toml"),
    )
    .expect("the session's description reads");
    let mut imports: Vec<_> = [
        "so.tilo.lang.discussion",
        "so.tilo.lang.chat",
        "so.tilo.lang.notes",
        "so.tilo.lang.tasks",
    ]
    .iter()
    .map(|app| replica.host.package(app).unwrap().meta)
    .collect();
    for meta in std::iter::once(session).chain(
        tilolang_tilo::platform_apps::platform_packages()
            .unwrap()
            .into_iter()
            .filter(|meta| meta.app != "so.tilo.session"),
    ) {
        replica.host.runtime().load_foreign(meta.clone()).unwrap();
        imports.push(meta);
    }
    let lock = tilolang_tilo::tilolang::IdsLock::parse(IDS).unwrap();
    let compiled = tilolang_tilo::tilolang::compile_package(&source, &lock, &imports)
        .unwrap_or_else(|diagnostics| {
            panic!(
                "{}",
                diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.render("shell"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        });
    replica.host.load_compiled(&compiled).unwrap();
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
    // What the session publishes on the desk: the time, who is here and the workspaces. The
    // views read it as the session writes it, so the test writes it the same way.
    let desk_ty = meta
        .roots
        .iter()
        .find(|ty| meta.types.iter().any(|t| &&t.id == ty && t.name == "Desk"))
        .cloned();
    if let Some(ty) = desk_ty {
        let root = replica.host.root_record(APP, &ty).unwrap();
        let meta = replica.host.package(APP).unwrap().meta;
        let clock = crate::fixtures::field_id(&meta, "Desk", "clock");
        replica
            .host
            .runtime()
            .apply_origin(
                "session",
                vec![OriginWrite::Set {
                    target: root,
                    fields: vec![(clock, Value::Text("4:28 PM".to_owned()))],
                }],
                None,
            )
            .expect("the session's clock is written on the desk");
    }
    // The device's and Settings' readings the desk's views read: published as the session
    // publishes them, an empty reading until a service says more. A view that reads a
    // resource with no reading at all traps as the desk opens.
    for (resource, ty) in tilolang_tilo::platform_apps::platform_roots() {
        let target = tilolang_tilo::runtime::ResourceRef::parse_id(resource, ty).unwrap();
        replica.host.runtime().set_reading(&target, "{}").unwrap();
    }
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
    let mut r = workspace();
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
    let mut r = workspace();
    r.commit();
    choose(
        &r,
        "apply",
        vec![shell_ref(&r), pick(&r, "Layout", "macos")],
    );
    r.commit();
    assert!(is(&r, "preset", "Layout", "macos"));
    assert!(is(&r, "dock_showing", "Showing", "no_overlap"));
    assert!(is(&r, "dock_fit", "Fit", "floating"));
    assert!(flag(&r, "dock_zoom"), "macOS's Dock magnifies");

    undo(&mut r);
    assert!(is(&r, "preset", "Layout", "tilo"), "the preset goes back");
    assert!(is(&r, "dock_showing", "Showing", "always"));
    assert!(!flag(&r, "dock_zoom"));
}

// ----- What a person sees: the presets, and when the chrome shows ---------------------------

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
    choose(r, "apply", vec![shell_ref(r), pick(r, "Layout", preset)]);
    r.commit();
}

#[test]
fn choosing_a_preset_changes_the_layout_the_person_sees() {
    let mut r = workspace();
    r.commit();
    // The default: a bar at the top with its parts, and a dock at the bottom along its
    // horizontal run.
    let default = desk_view(&r);
    let runs = nodes(&default, "ui.DragRun");
    let card = |view: &tilolang_runtime::ViewInstance| format!("{:?}", view.tree());

    // The full-width classic bar, always shown: no dock, so no `ui.DragRun` on the desk at
    // all — the bar's own run is there, the dock's is not.
    use_preset(&mut r, "windows");
    assert!(is(&r, "preset", "Layout", "windows"));
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

    // The macOS-like one: a bar with the app's menus and a dock, both translucent (the menus
    // themselves: `the_macos_bar_carries_the_focused_windows_own_commands`).
    use_preset(&mut r, "macos");
    assert!(is(&r, "bar_finish", "Finish", "translucent"));
    assert!(is(&r, "dock_showing", "Showing", "no_overlap"));

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
    assert!(!runs.is_empty(), "the dock and the bar are runs");
}

/// The bar's material is the theme's own, chosen by the person's record: the glass of the
/// translucent style, the calm opaque surface of the solid one, on the same chrome.
#[test]
fn the_bars_material_is_the_one_its_style_names() {
    /// The material the bar's chrome takes in the view the person sees, as the theme reads it.
    fn material(r: &Replica) -> String {
        let view = desk_view(r);
        let bar = nodes(&view, "ui.Chrome")
            .into_iter()
            .find(|node| matches!(node.props.get("name"), Some(Value::Text(name)) if name == "Top bar"))
            .unwrap_or_else(|| panic!("the desk shows a bar: {:?}", view.tree()));
        format!("{:?}", bar.props.get("material"))
    }
    let mut r = workspace();
    r.commit();
    assert!(
        material(&r).contains("calm"),
        "a fresh shell's bar is calm: {}",
        material(&r)
    );
    let translucent = pick(&r, "Finish", "translucent");
    let record = shell_ref(&r);
    choose(&r, "set_bar_finish", vec![record, translucent]);
    r.commit();
    assert!(
        material(&r).contains("glass"),
        "a translucent bar is the theme's glass: {}",
        material(&r)
    );
    let solid = pick(&r, "Finish", "solid");
    let record = shell_ref(&r);
    choose(&r, "set_bar_finish", vec![record, solid]);
    r.commit();
    assert!(
        material(&r).contains("calm"),
        "and back to calm: {}",
        material(&r)
    );
}

/// The charge's control reads the pack's own state: a pack taking charge (or waiting for it)
/// shows the charging battery and says so, and a pack that drains shows the plain one.
#[test]
fn a_charging_pack_shows_the_charging_battery() {
    /// Publish `reading` for the pack, and nothing for the rest of the device.
    fn charge(r: &mut Replica, reading: &str) {
        for (resource, ty) in tilolang_tilo::platform_apps::platform_roots() {
            let target = tilolang_tilo::runtime::ResourceRef::parse_id(resource, ty).unwrap();
            let reading = if resource == "power:battery" {
                reading
            } else {
                "{}"
            };
            r.host.runtime().set_reading(&target, reading).unwrap();
        }
        r.commit();
    }
    /// Whether the bar shows the charging battery's glyph.
    fn charging_glyph(view: &tilolang_runtime::ViewInstance) -> bool {
        let tree = format!("{:?}", view.tree());
        tree.contains("battery-charging") || tree.contains("battery_charging")
    }
    let mut r = workspace();
    charge(
        &mut r,
        r#"{"present": true, "percentage": 61, "state": "charging", "on_battery": false}"#,
    );
    let taking = desk_view(&r);
    assert!(
        says(&taking, "61%, charging"),
        "the charge says it takes charge: {:?}",
        taking.tree()
    );
    assert!(
        charging_glyph(&taking),
        "a charging pack shows the charging battery: {:?}",
        taking.tree()
    );
    charge(
        &mut r,
        r#"{"present": true, "percentage": 61, "state": "pending-charge", "on_battery": true}"#,
    );
    assert!(
        charging_glyph(&desk_view(&r)),
        "a pack waiting for its charger shows it too"
    );
    charge(
        &mut r,
        r#"{"present": true, "percentage": 61, "state": "discharging", "on_battery": true}"#,
    );
    let draining = desk_view(&r);
    assert!(
        says(&draining, "61%") && !says(&draining, "charging"),
        "a pack that drains says the charge alone: {:?}",
        draining.tree()
    );
    assert!(
        !charging_glyph(&draining),
        "and shows the plain battery: {:?}",
        draining.tree()
    );
}

/// The macOS bar carries the focused window's own commands: one menu named by the window, the
/// actions its resource publishes in it (the notes app's "Delete" and "Tag…"), each item running
/// the app's action, and never a menu the shell made up.
#[test]
fn the_macos_bar_carries_the_focused_windows_own_commands() {
    fn under<'a>(node: &'a tilolang_runtime::UiNode, out: &mut Vec<&'a tilolang_runtime::UiNode>) {
        for child in &node.children {
            out.push(child);
            under(child, out);
        }
    }
    let mut r = workspace();
    r.commit();
    r.host.root_of("so.tilo.lang.notes").unwrap();
    r.commit();
    scenario(&r, "so.tilo.lang.notes", "a week of notes");
    r.commit();
    use_preset(&mut r, "macos");
    // The arrangement is the session's: the scenario writes it in this host, and nothing of it
    // goes to the workspace.
    let arrangement = scenario(&r, APP, "a canvas of spaces");
    let meta = r.host.package(APP).unwrap().meta;
    let desk = meta
        .views
        .iter()
        .find(|v| v.name == "desk_view")
        .expect("the desk's view")
        .id
        .clone();
    let mut macos = r
        .host
        .runtime()
        .open_view(&Principal::new("ada"), &desk, vec![arrangement])
        .unwrap();
    // A desktop's output: 1280 by 800, where the bar has room for the window's commands.
    macos.set_space("", tilolang_runtime::Space::from_rem(80.0, 50.0));
    macos.poll_patches().unwrap();
    let card = |view: &tilolang_runtime::ViewInstance| format!("{:?}", view.tree());
    // The menu the bar names the window in front by: a button of the bar that holds commands,
    // the shell filling them from the declarations of the resource the window shows (`subject`).
    // The bar carries the shell's own menus too (the workspace, the preset the desk shows).
    let bar: Vec<tilolang_runtime::UiNode> = nodes(&macos, "ui.MenuButton")
        .into_iter()
        .filter(|menu| menu.props.get("role") == Some(&Value::Text("bar".to_owned())))
        .filter(|menu| menu.props.contains_key("subject"))
        .collect();
    assert_eq!(
        bar.len(),
        1,
        "the bar holds the window's one menu: {}",
        card(&macos)
    );
    let mut items = Vec::new();
    under(bar.last().unwrap(), &mut items);
    let commands: Vec<String> = items
        .iter()
        .filter(|i| i.kind == "ui.MenuItem" && i.events.contains_key("choose"))
        .filter_map(|i| i.props.get("label").cloned())
        .filter_map(|label| match label {
            Value::Text(label) => Some(label),
            _ => None,
        })
        .collect();
    for command in ["Delete", "Tag…"] {
        assert!(
            commands.iter().any(|label| label == command),
            "the window's menu runs the app's {command}: {commands:?}",
        );
    }
    // No menu is the shell's own: the bar names the window's commands, and the app's menus stand
    // in its view, where the app declares them (`File`, its imports and its export).
    let bar_menus: Vec<String> = nodes(&macos, "ui.MenuButton")
        .into_iter()
        .filter(|menu| menu.props.get("role") == Some(&Value::Text("bar".to_owned())))
        .filter_map(|menu| match menu.props.get("label") {
            Some(Value::Text(label)) => Some(label.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !bar_menus.iter().any(|label| label == "File"),
        "no menu the shell made up: {bar_menus:?}"
    );
}

/// Every surface that names a window reads the session's one answer: the window's own title and
/// its app's own name, never an app id.
#[test]
fn a_window_is_named_by_its_window_and_app_never_an_id() {
    let mut r = workspace();
    r.host.root_of("so.tilo.lang.notes").unwrap();
    r.commit();
    scenario(&r, "so.tilo.lang.notes", "a week of notes");
    r.commit();
    let arrangement = scenario(&r, APP, "a reference lands beside its place");
    let meta = r.host.package(APP).unwrap().meta;
    let front = meta
        .views
        .iter()
        .find(|v| v.name == "front_window")
        .expect("the front window's view")
        .id
        .clone();
    let view = r
        .host
        .runtime()
        .open_view(
            &Principal::new("ada"),
            &front,
            vec![arrangement, Value::Bool(false)],
        )
        .unwrap();
    let names: Vec<String> = nodes(&view, "ui.WindowFrame")
        .iter()
        .filter_map(|frame| match frame.props.get("name") {
            Some(Value::Text(name)) => Some(name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        vec!["design — Chat".to_owned()],
        "a window reads as its own title and its app: {names:?}"
    );
    let tree = format!("{:?}", view.tree());
    assert!(
        !tree.contains("so.tilo.lang.chat\""),
        "the desk never shows the id it opens a place by: {tree}"
    );
}

#[test]
fn a_bar_that_hides_until_reached_for_shows_when_the_pointer_comes_to_the_edge() {
    let mut r = workspace();
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
    let mut r = workspace();
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

/// The desk's quick settings stand over the desk with every tile, read from the device's and
/// Settings' published resources: here none is published, so each reads as absent (off, empty).
#[test]
fn the_quick_settings_stand_over_the_desk() {
    let mut r = workspace();
    let root = r.host.root_record(APP, "4jk2lkpt").unwrap();
    let meta = r.host.package(APP).unwrap().meta;
    let id = crate::fixtures::field_id(&meta, "Desk", "here");
    r.host
        .runtime()
        .apply_origin(
            "session",
            vec![OriginWrite::Set {
                target: root,
                fields: vec![(id, Value::text("quick-settings"))],
            }],
            None,
        )
        .expect("the desk's surface is the session's to write");
    for (resource, ty) in tilolang_tilo::platform_apps::platform_roots() {
        let target = tilolang_tilo::runtime::ResourceRef::parse_id(resource, ty).unwrap();
        r.host.runtime().set_reading(&target, "{}").unwrap();
    }
    r.commit();
    let view = desk_view(&r);
    for words in [
        "Quick settings",
        "Bluetooth",
        "Dark mode",
        "Do not disturb",
        "Lock",
    ] {
        assert!(
            says(&view, words),
            "the panel shows {words}: {:?}",
            view.tree()
        );
    }
}

/// The overview reads the workspace's strips in one chain over the desk's windows: each strip
/// once, top to bottom, and a window's menu offers the strips of its workspace it is not on.
/// The chain is a list in an order the index answers, sized by the run its filter chose, so a
/// desk whose windows this host wrote and has not indexed yet still pictures them.
#[test]
fn the_overviews_strips_are_read_once_each_in_the_order_they_stand() {
    /// Every `ui.Section`'s title, depth first: the overview's strips.
    fn walk(node: &tilolang_runtime::UiNode, titles: &mut Vec<String>) {
        if node.kind == "ui.Section"
            && let Some(Value::Text(title)) = node.props.get("title")
        {
            titles.push(title.clone());
        }
        for child in &node.children {
            walk(child, titles);
        }
    }
    /// Every menu item's label under `node`.
    fn under(node: &tilolang_runtime::UiNode, items: &mut Vec<String>) {
        for child in &node.children {
            if child.kind == "ui.MenuItem"
                && let Some(Value::Text(label)) = child.props.get("label")
            {
                items.push(label.clone());
            }
            under(child, items);
        }
    }
    let mut r = workspace();
    r.commit();
    scenario(&r, APP, "the overview");
    r.commit();
    let view = desk_view(&r);
    let mut titles = Vec::new();
    walk(view.tree(), &mut titles);
    let strips: Vec<&str> = titles
        .iter()
        .map(String::as_str)
        .filter(|title| ["Drafts", "Money", "Mail"].contains(title))
        .collect();
    assert_eq!(
        strips,
        ["Drafts", "Money", "Mail"],
        "the workspace's strips stand top to bottom: {titles:?}"
    );
    // The window in front is on Drafts: its menu carries the other two strips of its
    // workspace, by their names, and not the strip it stands on.
    let menus = nodes(&view, "ui.MenuButton");
    let menu = menus
        .iter()
        .find(|menu| menu.props.get("label") == Some(&Value::Text("This window".to_owned())))
        .unwrap_or_else(|| panic!("a window's place carries its own menu: {:?}", view.tree()));
    let mut items: Vec<String> = Vec::new();
    under(menu, &mut items);
    for item in ["Move to Money", "Move to Mail", "Move to Home"] {
        assert!(
            items.iter().any(|held| held == item),
            "the window's menu offers {item}: {items:?}"
        );
    }
    assert!(
        !items.iter().any(|held| held == "Move to Drafts"),
        "a window is not offered the strip it stands on: {items:?}"
    );
}

/// The session writes what the screen says of its edges, as it does every turn: one fact on
/// the shell's own record, through the origin write the session's door applies
/// (`SessionLink::publish_edges`), which the host then commits.
fn set_screen(r: &mut Replica, field: &str, on: bool) {
    // What the screen says is the desk's reading, which the session writes on the desk: the
    // person's shell record keeps their own settings, not the screen's momentary facts.
    let root = r.host.root_record(APP, "4jk2lkpt").unwrap();
    let meta = r.host.package(APP).unwrap().meta;
    let id = crate::fixtures::field_id(&meta, "Desk", field);
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
    use tilolang_tilo::instance::record::HostHistoryEdit;

    let edit = HistoryEdit::Undo {
        steps: None,
        run: None,
        by: None,
        to: None,
        step: None,
    };
    let payload = serde_json::to_vec(&edit).unwrap();
    let action: HostHistoryEdit = serde_json::from_slice(&payload).unwrap();
    // The history edit is on the workspace root, which a tree path names `room`.
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

/// Ada runs the scenario `name` of `app`, as a shot starts from it, and gets what it returns.
fn scenario(r: &Replica, app: &str, name: &str) -> Value {
    let meta = r.host.package(app).unwrap().meta;
    let action = meta
        .scenarios
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("{app} has no scenario {name:?}"))
        .action
        .clone();
    match r
        .host
        .runtime()
        .invoke_action(&Principal::new("ada"), (action.as_str(), vec![]))
        .unwrap()
    {
        InvokeOutcome::Done(value) => value,
        other => panic!("{name}: {other:?}"),
    }
}

#[test]
fn a_canvas_place_naming_a_resource_and_view_composes_that_view_and_a_window_keeps_its_well() {
    let mut r = workspace();
    r.host.root_of("so.tilo.lang.notes").unwrap();
    r.commit();
    scenario(&r, "so.tilo.lang.notes", "a week of notes");
    r.commit();
    let meta = r.host.package(APP).unwrap().meta;
    // The arrangement is the session's (`origin: session.arrangement`): the scenario writes it
    // in this host, as the session's reading would, and nothing of it goes to the workspace.
    let arrangement = scenario(&r, APP, "a canvas of spaces");
    let desk = meta
        .views
        .iter()
        .find(|v| v.name == "desk_view")
        .expect("the desk's view")
        .id
        .clone();
    let view = r
        .host
        .runtime()
        .open_view(&Principal::new("ada"), &desk, vec![arrangement])
        .unwrap();
    let tree = format!("{:?}", view.tree());
    // Each note's place is the notes app's own editor, drawn inline (its body's input, which
    // only the editor has, once per note), and no well stands for it.
    assert_eq!(
        tree.matches("Write something").count(),
        6,
        "the notes' editor is composed in each note's place: {tree}"
    );
    let wells = nodes(&view, "ui.WindowWell");
    assert_eq!(
        wells.len(),
        1,
        "only the terminal's place keeps a well: {wells:#?}"
    );
    assert_eq!(
        wells[0].props.get("place"),
        Some(&Value::Text("c-terminal".to_owned()))
    );
}

#[test]
fn resources_standing_as_content_stand_on_the_spaces_canvas() {
    let mut r = workspace();
    r.host.root_of("so.tilo.lang.notes").unwrap();
    r.commit();
    scenario(&r, "so.tilo.lang.notes", "a week of notes");
    r.commit();
    let meta = r.host.package(APP).unwrap().meta;
    let arrangement = scenario(&r, APP, "resources standing as content");
    let desk = meta
        .views
        .iter()
        .find(|v| v.name == "desk_view")
        .expect("the desk's view")
        .id
        .clone();
    let view = r
        .host
        .runtime()
        .open_view(&Principal::new("ada"), &desk, vec![arrangement])
        .unwrap();
    let tree = format!("{:?}", view.tree());
    // The scenario's strip stands on the canvas, so its places do: four notes as their editor,
    // and the terminal in its well.
    assert_eq!(
        tree.matches("Write something").count(),
        4,
        "each note stands as its editor: {tree}"
    );
    let wells = nodes(&view, "ui.WindowWell");
    assert_eq!(wells.len(), 1, "{wells:#?}");
    assert_eq!(
        wells[0].props.get("place"),
        Some(&Value::Text("x-terminal".to_owned()))
    );
}

// ----- A notice's line, as the banner says it ------------------------------------------------

/// A notice the session wrote on the desk, and the banner pointed at it: `key`'s title, `body`
/// and sender, as `session.notice` carries them.
fn notice_shown(r: &Replica, key: &str, body: &str, origin: &str) {
    let desk = r.host.root_record(APP, "4jk2lkpt").unwrap();
    let meta = r.host.package(APP).unwrap().meta;
    r.host
        .runtime()
        .apply_origin(
            "session",
            vec![
                OriginWrite::Insert {
                    owner: desk.clone(),
                    field: "3vvnnztw".to_owned(),
                    ty: "zk7pzrt2".to_owned(),
                    fields: vec![
                        ("key".to_owned(), Value::text(key)),
                        ("title".to_owned(), Value::text("You joined Lounge")),
                        ("body".to_owned(), Value::text(body)),
                        ("origin_app".to_owned(), Value::text(origin)),
                        ("kind".to_owned(), Value::text("info")),
                        ("seen".to_owned(), Value::Bool(false)),
                    ],
                },
                OriginWrite::Set {
                    target: desk,
                    fields: vec![(
                        crate::fixtures::field_id(&meta, "Desk", "banner"),
                        Value::text(key),
                    )],
                },
            ],
            None,
        )
        .expect("the session writes its own notice on the desk");
}

/// What the desk's banner says under its title, as the desk lays it out: the message the banner
/// carries on a roomy desk, else the row's secondary line on a compact one.
fn banner_line_of(r: &Replica) -> String {
    let mut view = desk_view(r);
    // A desk with room for the toolkit's banner: the width a display's window has.
    view.set_space("", tilolang_runtime::Space::from_rem(90.0, 50.0));
    view.poll_patches().unwrap();
    if let Some(banner) = nodes(&view, "ui.Banner").first()
        && let Some(Value::Text(message)) = banner.props.get("message")
    {
        return message.clone();
    }
    let row = nodes(&view, "ui.ListRow")
        .into_iter()
        .find(|row| row.props.contains_key("secondary"))
        .unwrap_or_else(|| panic!("the desk shows the notice: {:?}", view.tree()));
    match row.props.get("secondary") {
        Some(Value::Text(secondary)) => secondary.clone(),
        other => panic!("the notice's line is text: {other:?}"),
    }
}

/// The banner's line is every part the notice has, joined once: the session's own notice — one
/// with no app behind it — says what it says and trails no separator, an app's notice says what
/// it says and then who sent it, and one that says nothing is its sender alone. A part that is
/// not there is not a part, never an empty one joined in.
#[test]
fn a_banners_line_joins_only_the_parts_the_notice_has() {
    let mut r = workspace();
    r.commit();
    notice_shown(
        &r,
        "n-session",
        "It is open now, with everything its people keep in it.",
        "",
    );
    r.commit();
    assert_eq!(
        banner_line_of(&r),
        "It is open now, with everything its people keep in it.",
        "a notice with no sender trails no separator"
    );

    notice_shown(&r, "n-app", "From Ada", "Mail");
    r.commit();
    assert_eq!(
        banner_line_of(&r),
        "From Ada — Mail",
        "what it says, then who sent it"
    );

    notice_shown(&r, "n-empty", "", "Mail");
    r.commit();
    assert_eq!(
        banner_line_of(&r),
        "Mail",
        "a notice that says nothing is its sender alone"
    );
}
