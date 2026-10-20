//! The notes app (`apps/tl/notes/app.tl`) end to end.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{
        AmStore, EditOutcome, InvokeOutcome, Patch, ResourceRef, Runtime, SimpleAuthority, Space,
        Value, ViewEvent, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/notes/app.tl");

    struct Notes {
        rt: Runtime,
        app: App,
        nb: ResourceRef,
    }

    impl Notes {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            let nb = rt.create("Notebook").unwrap();
            Self { rt, app, nb }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        fn new_note(&self) -> ResourceRef {
            returned_ref(self.call("new_note", vec![Value::resource(&self.nb)]))
        }

        fn count(&self) -> i64 {
            self.rt.project(&self.rt.read(&self.nb, "notes").unwrap())["count"]
                .as_i64()
                .unwrap()
        }
    }

    #[test]
    fn a_new_note_is_untitled() {
        let n = Notes::new();
        let note = n.new_note();
        assert_eq!(n.rt.read(&note, "title").unwrap(), Value::text("Untitled"));
        assert_eq!(n.rt.read(&note, "pinned").unwrap(), Value::Bool(false));
        assert_eq!(n.count(), 1);
    }

    #[test]
    fn pin_tag_and_delete() {
        let n = Notes::new();
        let first = n.new_note();
        let second = n.new_note();
        n.call("toggle_pin", vec![Value::resource(&first)]);
        assert_eq!(n.rt.read(&first, "pinned").unwrap(), Value::Bool(true));
        n.call("toggle_pin", vec![Value::resource(&first)]);
        assert_eq!(n.rt.read(&first, "pinned").unwrap(), Value::Bool(false));

        n.call("tag", vec![Value::resource(&first), Value::text("work")]);
        n.call("tag", vec![Value::resource(&first), Value::text("idea")]);
        n.call("tag", vec![Value::resource(&first), Value::text("work")]);
        let tags = n.rt.read(&first, "tags").unwrap();
        assert_eq!(
            tags,
            Value::List(vec![Value::text("idea"), Value::text("work")]),
            "a set holds each tag once"
        );

        n.call(
            "delete",
            vec![Value::resource(&n.nb), Value::resource(&second)],
        );
        assert_eq!(n.count(), 1);
        n.call(
            "delete",
            vec![Value::resource(&n.nb), Value::resource(&first)],
        );
        assert_eq!(n.count(), 0);
    }

    fn labels(view: &ViewInstance) -> Vec<String> {
        of_kind(view.tree(), "button")
            .iter()
            .map(|b| text_prop(b, "label"))
            .collect()
    }

    fn button(view: &ViewInstance, label: &str) -> String {
        of_kind(view.tree(), "button")
            .into_iter()
            .find(|b| text_prop(b, "label") == label)
            .unwrap_or_else(|| panic!("no button {label} in\n{}", dump(view.tree())))
            .key
            .clone()
    }

    #[test]
    fn the_notebook_lists_its_notes_and_opens_each_in_a_window_of_its_own() {
        let n = Notes::new();
        let mut list =
            n.rt.open_view(&me(), &n.app.view("notebook"), vec![Value::resource(&n.nb)])
                .unwrap();
        assert!(
            of_kind(list.tree(), "input").is_empty(),
            "{}",
            dump(list.tree())
        );
        assert_eq!(of_kind(list.tree(), "page").len(), 1);

        let new = n.app.anchor("notebook", "new");
        assert!(matches!(list.press(&new).unwrap(), InvokeOutcome::Done(_)));
        list.poll_patches().unwrap();
        assert!(
            of_kind(list.tree(), "input").is_empty(),
            "the list does not edit notes: {}",
            dump(list.tree())
        );
        assert!(labels(&list).contains(&"Untitled".to_owned()));

        list.press(&button(&list, "Untitled")).unwrap();
        let events = list.poll_events();
        let [ViewEvent::Open { target, view: None }] = events.as_slice() else {
            panic!("the row opens its note: {events:?}");
        };
        let mut editor = n.rt.open_resource(&me(), target, None).unwrap();
        let title = of_kind(editor.tree(), "input")[0].key.clone();
        assert_eq!(
            editor.edit(&title, &Value::text("Plans")).unwrap(),
            EditOutcome::Applied
        );
        assert!(
            list.is_stale(),
            "the list shows the title the editor changed"
        );
        list.poll_patches().unwrap();
        assert!(
            labels(&list).contains(&"Plans".to_owned()),
            "{:?}",
            labels(&list)
        );

        editor.press(&button(&editor, "Pin")).unwrap();
        editor.poll_patches().unwrap();
        assert!(labels(&editor).contains(&"Unpin".to_owned()));

        list.press(&button(&list, "Show actions")).unwrap();
        list.poll_patches().unwrap();
        list.press(&button(&list, "Delete")).unwrap();
        list.poll_patches().unwrap();
        assert_eq!(n.count(), 0);
        assert!(!labels(&list).contains(&"Plans".to_owned()));
    }

    #[test]
    fn the_note_rows_show_the_body_only_when_there_is_room() {
        let n = Notes::new();
        n.new_note();
        let mut view =
            n.rt.open_view(&me(), &n.app.view("notebook"), vec![Value::resource(&n.nb)])
                .unwrap();
        let roomy = of_kind(view.tree(), "text").len();
        assert_eq!(roomy, 1, "{}", dump(view.tree()));
        assert!(view.set_space("", Space::from_rem(20.0, 40.0)));
        let patches = view.poll_patches().unwrap();
        assert!(
            patches.iter().any(|p| matches!(p, Patch::Remove { .. })),
            "{patches:?}"
        );
        assert!(of_kind(view.tree(), "text").is_empty());
    }

    #[test]
    fn the_filter_and_the_order_are_the_windows() {
        let n = Notes::new();
        for title in ["b", "a"] {
            let note = n.new_note();
            n.call("retitle", vec![Value::resource(&note), Value::text(title)]);
            if title == "a" {
                n.call("toggle_pin", vec![Value::resource(&note)]);
            }
        }
        let mut view =
            n.rt.open_view(&me(), &n.app.view("notebook"), vec![Value::resource(&n.nb)])
                .unwrap();
        let rows = |v: &ViewInstance| -> Vec<String> {
            labels(v).into_iter().filter(|l| l.len() == 1).collect()
        };
        assert_eq!(rows(&view), ["b", "a"]);
        view.press(&n.app.anchor("notebook", "pinned")).unwrap();
        view.poll_patches().unwrap();
        assert_eq!(rows(&view), ["a"]);
        view.press(&n.app.anchor("notebook", "all")).unwrap();
        view.press(&n.app.anchor("notebook", "sort")).unwrap();
        view.poll_patches().unwrap();
        assert_eq!(rows(&view), ["a", "b"]);
    }

    #[test]
    fn concurrent_body_edits_on_two_replicas_converge() {
        let app = App::compile(SOURCE, &[]);
        let (mut store_a, mut store_b) = (AmStore::new(), AmStore::new());
        let rt_a = Runtime::new(store_a.clone(), SimpleAuthority::new()).unwrap();
        let rt_b = Runtime::new(store_b.clone(), SimpleAuthority::new()).unwrap();
        app.load(&rt_a);
        app.load(&rt_b);
        let nb = rt_a.create("Notebook").unwrap();
        let note = returned_ref(
            rt_a.invoke_action(
                &me(),
                (app.action("new_note").as_str(), vec![Value::resource(&nb)]),
            )
            .unwrap(),
        );
        store_a.sync_with(&mut store_b).unwrap();

        let editor = |rt: &Runtime| {
            rt.open_view(
                &me(),
                &app.view("note_editor"),
                vec![Value::resource(&note)],
            )
            .unwrap()
        };
        let (view_a, view_b) = (editor(&rt_a), editor(&rt_b));
        let body = |v: &tilolang_runtime::ViewInstance| of_kind(v.tree(), "input")[1].key.clone();
        view_a.edit(&body(&view_a), &Value::text("Hello")).unwrap();
        store_a.sync_with(&mut store_b).unwrap();

        view_a
            .edit(&body(&view_a), &Value::text("Hello world"))
            .unwrap();
        view_b
            .edit(&body(&view_b), &Value::text("Big Hello"))
            .unwrap();
        store_a.sync_with(&mut store_b).unwrap();

        let (a, b) = (
            rt_a.read(&note, "body").unwrap(),
            rt_b.read(&note, "body").unwrap(),
        );
        assert_eq!(a, b);
        assert_eq!(a, Value::text("Big Hello world"));
    }
}
