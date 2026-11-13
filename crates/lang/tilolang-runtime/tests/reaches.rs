//! What a view depends on when it reads a record that is not the one it stands on: a root the
//! package names (`root shell: Shell`), read through a function call or inside a `match`, and
//! changed from outside the view, here or by another connection. The view runs again and
//! patches, and a view whose `is_stale` says yes always has something to run. A record a view
//! builds (`DockItem(...)`) is a value of its type wherever it is handed.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, SimpleAuthority, Value};

    /// The desk's shape, as the shell's is: a root view on one root (`arrangement`) that reads
    /// another root (`shell`) through a function call and an `if`, choosing one of several
    /// views by it. The preset the person picks is a field of the shell's record.
    const SOURCE: &str = r#"app so.tilo.desk "Desk"

type Shell {
    preset: Preset = .strips
    tuned: Bool = false
    bar: Bool = true
}

enum Preset {
    strips
    frames
    spaces
    desktop
}

root shell: Shell

default view shell_settings for Shell

view shell_settings(s: Shell) {
    section("Shell") {
        text("a shell")
    }
}

type Arrangement {
    places: Int
}

root arrangement: Arrangement

default view desk_view for arrangement

fn is_traditional(s: Shell) -> Bool {
    return s.preset == .desktop and s.bar
}

action apply(s: Shell, to: Preset) {
    s.preset = to
    s.tuned = false
}

action set_bar(s: Shell, on: Bool) {
    s.bar = on
}

view desk_view(a: Arrangement) {
    stack {
        screen(a)
    }
}

view screen(a: Arrangement) {
    let v = shell
    if is_traditional(shell) {
        text("the desktop")
    } else if v.preset == .frames {
        text("the frames")
    } else if v.preset == .spaces {
        text("the spaces")
    } else {
        text("the strips")
    }
}

view shown(a: Arrangement) {
    let v = shell
    text(match v.preset {
        .frames => "the frames",
        .spaces => "the spaces",
        .desktop => "the desktop",
        .strips => "the strips",
    })
}

/// The shell's dock, in miniature: a view that builds its items (`DockItem(...)`, as the dock
/// builds one for an app running but never pinned) and hands each to another view's record
/// parameter, beside the field reads above.
type DockItem {
    key: Text
    title: Text
    pinned: Bool = true
}

fn pinned_item(i: Int) -> DockItem {
    return DockItem(key: "app{i}", title: "App {i}", pinned: true)
}

fn pinned_items() -> List<DockItem> {
    return [pinned_item(0), pinned_item(1)]
}

view dock_run(a: Arrangement) {
    stack {
        for item in pinned_items() {
            dock_tile(item)
        }
    }
}

view dock_tile(item: DockItem) {
    text(item.title)
}

/// Changes the item it is given: an action's record parameter is a resource.
action unpin(item: DockItem) {
    item.pinned = false
}
"#;

    struct Desk {
        rt: Runtime,
        app: App,
        arrangement: ResourceRef,
    }

    impl Desk {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            let _shell = rt.create_root("Shell").unwrap();
            let arrangement = rt.create_root("Arrangement").unwrap();
            Self {
                rt,
                app,
                arrangement,
            }
        }

        /// The person's shell record, as the package's named root, `root shell: Shell`.
        fn shell(&self) -> ResourceRef {
            let ty = self
                .app
                .compiled
                .meta
                .named_roots
                .iter()
                .find(|root| root.name == "shell")
                .map(|root| root.ty.clone())
                .expect("the package names its shell root");
            ResourceRef::root(format!("root-{ty}"), ty)
        }

        fn call(&self, action: &str, args: Vec<Value>) {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|e| panic!("{action}: {e}"));
        }

        /// The case `variant` of the enum `ty`, as an action argument takes it.
        fn pick(&self, ty: &str, variant: &str) -> Value {
            let ids = &self.app.compiled.ids;
            let id = |name: &str| {
                ids.get(name)
                    .unwrap_or_else(|| panic!("no id {name}"))
                    .to_owned()
            };
            Value::Enum(tilolang_runtime::EnumValue {
                ty: id(ty),
                variant: id(&format!("{ty}.{variant}")),
                fields: std::collections::BTreeMap::new(),
            })
        }

        fn view(&self, name: &str) -> tilolang_runtime::ViewInstance {
            self.rt
                .open_view(
                    &me(),
                    &self.app.view(name),
                    vec![Value::resource(&self.arrangement)],
                )
                .unwrap_or_else(|e| panic!("{name}: {e}"))
        }
    }

    fn texts(view: &tilolang_runtime::ViewInstance) -> Vec<String> {
        of_kind(view.tree(), "ui.Text")
            .into_iter()
            .map(|t| text_prop(t, "value"))
            .collect()
    }

    /// A view whose root reads a field of another root through a function call runs again when
    /// that field changes, and its patches show the other preset.
    #[test]
    fn a_root_field_read_through_a_function_call_reaches_the_view() {
        let d = Desk::new();
        let mut view = d.view("desk_view");
        assert_eq!(texts(&view), ["the strips"]);
        d.call(
            "apply",
            vec![Value::resource(&d.shell()), d.pick("Preset", "desktop")],
        );
        assert!(view.is_stale(), "the shell's preset moved");
        let patches = view.poll_patches().unwrap();
        assert!(!patches.is_empty(), "the screen changes: {patches:?}");
        assert_eq!(texts(&view), ["the desktop"]);
    }

    /// The same read inside a `match` arm.
    #[test]
    fn a_root_field_read_inside_a_match_reaches_the_view() {
        let d = Desk::new();
        let mut view = d.view("shown");
        assert_eq!(texts(&view), ["the strips"]);
        d.call(
            "apply",
            vec![Value::resource(&d.shell()), d.pick("Preset", "spaces")],
        );
        let patches = view.poll_patches().unwrap();
        assert!(!patches.is_empty(), "the screen changes: {patches:?}");
        assert_eq!(texts(&view), ["the spaces"]);
    }

    /// `is_stale` and what there is to run agree: a view that says it is stale patches
    /// something, and a view with nothing to patch says it is not stale.
    #[test]
    fn a_stale_view_always_has_something_to_run() {
        let d = Desk::new();
        let mut view = d.view("desk_view");
        let _ = view.poll_patches().unwrap();
        assert!(!view.is_stale(), "at rest the view is not stale");
        assert!(view.poll_patches().unwrap().is_empty());
        d.call(
            "apply",
            vec![Value::resource(&d.shell()), d.pick("Preset", "frames")],
        );
        assert!(view.is_stale());
        let patches = view.poll_patches().unwrap();
        assert!(
            !patches.is_empty(),
            "stale before the poll, nothing after: the view had something to run"
        );
        assert!(!view.is_stale(), "and it is at rest again");
    }

    /// A change another connection made reaches the view as one the person made here does.
    #[test]
    fn a_field_changed_by_another_connection_reaches_the_view() {
        let (mut store_a, mut store_b) = (AmStore::new(), AmStore::new());
        let app = App::compile(SOURCE, &[]);
        let rt_a = Runtime::new(store_a.clone(), SimpleAuthority::new()).unwrap();
        app.load(&rt_a);
        let _shell = rt_a.create_root("Shell").unwrap();
        let arrangement = rt_a.create_root("Arrangement").unwrap();
        let mut view = rt_a
            .open_view(
                &me(),
                &app.view("desk_view"),
                vec![Value::resource(&arrangement)],
            )
            .unwrap();
        assert_eq!(texts(&view), ["the strips"]);

        // Another connection, over a store of its own, changes the shell's record.
        let rt_b = Runtime::new(store_b.clone(), SimpleAuthority::new()).unwrap();
        app.load(&rt_b);
        rt_b.create_root("Shell").unwrap();
        let ty = app
            .compiled
            .meta
            .named_roots
            .iter()
            .find(|root| root.name == "shell")
            .map(|root| root.ty.clone())
            .expect("the package names its shell root");
        let shell_b = ResourceRef::root(format!("root-{ty}"), ty);
        let apply = app.action("apply");
        let desktop = Value::Enum(tilolang_runtime::EnumValue {
            ty: app.compiled.ids.get("Preset").unwrap().to_owned(),
            variant: app.compiled.ids.get("Preset.desktop").unwrap().to_owned(),
            fields: std::collections::BTreeMap::new(),
        });
        rt_b.invoke_action(
            &me(),
            (apply.as_str(), vec![Value::resource(&shell_b), desktop]),
        )
        .unwrap();

        // The change reaches this connection.
        store_a.sync_with(&mut store_b).unwrap();
        assert!(
            view.is_stale(),
            "the other connection's change reached the view"
        );
        let patches = view.poll_patches().unwrap();
        assert!(!patches.is_empty(), "the screen changes: {patches:?}");
        assert_eq!(texts(&view), ["the desktop"]);
    }

    /// A view that builds a record and hands it to another view's record parameter draws it,
    /// as the shell's dock hands its running items to its tiles.
    #[test]
    fn a_record_built_in_a_view_reaches_a_views_record_parameter() {
        let d = Desk::new();
        let view = d.view("dock_run");
        assert_eq!(texts(&view), ["App 0", "App 1"]);
    }

    /// Only the boundaries that read the changed field run: the screen reads `preset` through
    /// `is_traditional`, so it runs once and the desk view around it stands.
    #[test]
    fn only_the_boundaries_that_read_the_field_run() {
        let d = Desk::new();
        let mut view = d.view("desk_view");
        let opened = view.stats();
        assert_eq!(
            opened.renders,
            u64::try_from(opened.boundaries).unwrap(),
            "every boundary ran at the open"
        );
        d.call(
            "apply",
            vec![Value::resource(&d.shell()), d.pick("Preset", "frames")],
        );
        let patches = view.poll_patches().unwrap();
        assert_eq!(
            view.stats().renders - opened.renders,
            1,
            "the screen read the field and runs once, and nothing else runs: {:?}",
            view.stats()
        );
        assert!(
            !patches.is_empty(),
            "the screen draws the frames: {patches:?}"
        );
        assert_eq!(texts(&view), ["the frames"]);
    }

    /// A record a view builds names no resource, so an action, whose changes must land in one,
    /// refuses it rather than change a copy that is dropped.
    #[test]
    fn an_action_refuses_a_record_a_view_built() {
        let desk = Desk::new();
        let built = Value::Rec(tilolang_runtime::RecordInit {
            ty: desk.app.compiled.ids.get("DockItem").unwrap().to_owned(),
            fields: std::collections::BTreeMap::new(),
        });
        let unpin = desk.app.action("unpin");
        let refused = desk.rt.invoke_action(&me(), (unpin.as_str(), vec![built]));
        assert!(
            refused.is_err(),
            "an action took a record no resource holds: {refused:?}"
        );
    }
}
