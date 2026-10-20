//! The notes app (`apps/tl/notes/app.tl`) end to end.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{
        AmStore, EditOutcome, InvokeOutcome, Patch, ResourceRef, Runtime, SimpleAuthority, Space,
        Value,
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

        fn open(&self) -> Value {
            self.rt.read(&self.nb, "open").unwrap()
        }

        fn count(&self) -> i64 {
            self.rt.project(&self.rt.read(&self.nb, "notes").unwrap())["count"]
                .as_i64()
                .unwrap()
        }
    }

    #[test]
    fn a_new_note_is_untitled_and_open() {
        let n = Notes::new();
        assert_eq!(n.open(), Value::none());
        let note = n.new_note();
        assert_eq!(n.rt.read(&note, "title").unwrap(), Value::text("Untitled"));
        assert_eq!(n.rt.read(&note, "pinned").unwrap(), Value::Bool(false));
        assert_eq!(n.count(), 1);
        let Value::Opt(Some(open)) = n.open() else {
            panic!("the new note is open");
        };
        assert!(matches!(*open, Value::Ref(ref r) if r.target == note));
    }

    #[test]
    fn open_pin_tag_and_delete() {
        let n = Notes::new();
        let first = n.new_note();
        let second = n.new_note();
        n.call(
            "open_note",
            vec![Value::resource(&n.nb), Value::resource(&first)],
        );
        let Value::Opt(Some(open)) = n.open() else {
            panic!("a note is open");
        };
        assert!(matches!(*open, Value::Ref(ref r) if r.target == first));

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
        assert!(
            matches!(n.open(), Value::Opt(Some(_))),
            "deleting another note keeps the editor open"
        );
        n.call(
            "delete",
            vec![Value::resource(&n.nb), Value::resource(&first)],
        );
        assert_eq!(n.count(), 0);
        assert_eq!(
            n.open(),
            Value::none(),
            "deleting the open note closes the editor"
        );
    }

    #[test]
    fn the_main_view_lists_notes_and_opens_the_editor() {
        let n = Notes::new();
        let mut view =
            n.rt.open_view(&me(), &n.app.view("main"), vec![Value::resource(&n.nb)])
                .unwrap();
        assert!(
            of_kind(view.tree(), "input").is_empty(),
            "{}",
            dump(view.tree())
        );
        assert_eq!(of_kind(view.tree(), "page").len(), 1);

        let new = n.app.anchor("note_list", "new");
        assert!(matches!(view.press(&new).unwrap(), InvokeOutcome::Done(_)));
        view.poll_patches().unwrap();
        assert_eq!(
            of_kind(view.tree(), "input").len(),
            2,
            "{}",
            dump(view.tree())
        );
        let buttons: Vec<String> = of_kind(view.tree(), "button")
            .iter()
            .map(|b| text_prop(b, "label"))
            .collect();
        assert!(buttons.contains(&"Untitled".to_owned()), "{buttons:?}");
        assert!(buttons.contains(&"Pin".to_owned()), "{buttons:?}");

        let title = of_kind(view.tree(), "input")[0].key.clone();
        assert_eq!(
            view.edit(&title, &Value::text("Plans")).unwrap(),
            EditOutcome::Applied
        );
        view.poll_patches().unwrap();
        let labels: Vec<String> = of_kind(view.tree(), "button")
            .iter()
            .map(|b| text_prop(b, "label"))
            .collect();
        assert!(labels.contains(&"Plans".to_owned()), "{labels:?}");

        let pin = of_kind(view.tree(), "button")
            .into_iter()
            .find(|b| text_prop(b, "label") == "Pin")
            .unwrap()
            .key
            .clone();
        view.press(&pin).unwrap();
        view.poll_patches().unwrap();
        let labels: Vec<String> = of_kind(view.tree(), "button")
            .iter()
            .map(|b| text_prop(b, "label"))
            .collect();
        assert!(labels.contains(&"Unpin".to_owned()), "{labels:?}");

        let delete = n.app.anchor("editor", "delete");
        let node_key = of_kind(view.tree(), "button")
            .into_iter()
            .find(|b| b.key.starts_with(&delete))
            .unwrap()
            .key
            .clone();
        view.press(&node_key).unwrap();
        view.poll_patches().unwrap();
        assert!(of_kind(view.tree(), "input").is_empty());
        assert_eq!(n.count(), 0);
    }

    #[test]
    fn the_note_rows_show_the_body_only_when_there_is_room() {
        let n = Notes::new();
        let note = n.new_note();
        n.call(
            "open_note",
            vec![Value::resource(&n.nb), Value::resource(&note)],
        );
        let mut view =
            n.rt.open_view(
                &me(),
                &n.app.view("note_list"),
                vec![Value::resource(&n.nb)],
            )
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
                &app.view("editor"),
                vec![Value::resource(&nb), Value::resource(&note)],
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
