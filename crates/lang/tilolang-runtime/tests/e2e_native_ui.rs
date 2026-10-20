//! The native vocabulary's values run: an action's presented state, dates and times, the clock
//! of an invocation, scenarios acting as named people, and rows of resources.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, Runtime, SimpleAuthority, Value};

    fn runtime(app: &App) -> Runtime {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        rt
    }

    const PIN: &str = r#"app so.tilo.pins "Pins"

type Note {
    title: Text (title)
    pinned: Bool = false
}

action toggle_pin(n: Note) (label: if n.pinned { "Unpin" } else { "Pin" }, icon: if n.pinned { .bell_off } else { .pin }, on: n.pinned) {
    n.pinned = not n.pinned
}

action rename(n: Note, t: Text) (label: "Rename", on: n.title != "") {
    n.title = t
}
"#;

    #[test]
    fn the_presentation_view_of_an_action_shows_its_subjects_state() {
        let app = App::compile(PIN, &[]);
        let rt = runtime(&app);
        let note = rt.create("Note").unwrap();
        let present = |name: &str| {
            let a = app.meta().actions.iter().find(|a| a.name == name).unwrap();
            let view = a.present.clone().expect("a presentation view");
            let v = rt
                .open_view(&me(), &view, vec![Value::resource(&note)])
                .unwrap();
            let tree = v.tree().clone();
            assert_eq!(tree.kind, "present", "{}", dump(&tree));
            assert!(tree.children.is_empty());
            tree
        };
        let off = present("toggle_pin");
        assert_eq!(text_prop(&off, "label"), "Pin");
        assert_eq!(text_prop(&off, "icon"), "pin");
        assert_eq!(off.props["on"], Value::Int(0));
        rt.invoke_action(
            &me(),
            (
                app.action("toggle_pin").as_str(),
                vec![Value::resource(&note)],
            ),
        )
        .unwrap();
        let on = present("toggle_pin");
        assert_eq!(text_prop(&on, "label"), "Unpin");
        assert_eq!(text_prop(&on, "icon"), "bell-off");
        assert_eq!(on.props["on"], Value::Int(1));
        let rename = present("rename");
        assert_eq!(
            rename.props.keys().collect::<Vec<_>>(),
            ["on"],
            "only what is given dynamically"
        );
        assert!(
            rt.catalogue().iter().all(|s| !s.name.contains('~')),
            "the catalogue never lists it"
        );
    }
}
