//! The notes app (`apps/tl/notes/app.tl`) end to end.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{
        AmStore, EditOutcome, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, Space, UiNode,
        Value, ViewInstance,
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
            let nb = rt.create("Notes").unwrap();
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
            self.rt.project(&self.rt.read(&self.nb, "items").unwrap())["count"]
                .as_i64()
                .unwrap()
        }
    }

    #[test]
    fn a_new_note_is_untitled() {
        let n = Notes::new();
        let note = n.new_note();
        assert_eq!(
            n.rt.read(&note, "title").unwrap(),
            Value::text(""),
            "its title starts empty: what is typed first names it"
        );
        assert_eq!(n.rt.read(&note, "pinned").unwrap(), Value::Bool(false));
        assert_eq!(n.count(), 1);
    }

    #[test]
    fn pin_tag_and_delete() {
        let n = Notes::new();
        let first = n.new_note();
        let second = n.new_note();
        n.call("pin", vec![Value::resource(&first)]);
        assert_eq!(n.rt.read(&first, "pinned").unwrap(), Value::Bool(true));
        n.call("pin", vec![Value::resource(&first)]);
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

    fn list(view: &ViewInstance) -> &UiNode {
        of_kind(view.tree(), "ui.VirtualList")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("no list in\n{}", dump(view.tree())))
    }

    fn titles(view: &ViewInstance) -> Vec<String> {
        of_kind(view.tree(), "ui.ListRow")
            .iter()
            .map(|r| text_prop(r, "title"))
            .collect()
    }

    /// The resource of the list item whose row has this title.
    fn item_titled(view: &ViewInstance, title: &str) -> ResourceRef {
        of_kind(list(view), "ui.ListItem")
            .into_iter()
            .find(|item| {
                of_kind(item, "ui.ListRow")
                    .iter()
                    .any(|r| text_prop(r, "title") == title)
            })
            .and_then(|item| match item.props.get("item") {
                Some(Value::Ref(r)) => Some(ResourceRef::clone(&r.target)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no item {title} in\n{}", dump(view.tree())))
    }

    /// The node of `kind` whose `prop` is `value`.
    fn node_with<'a>(tree: &'a UiNode, kind: &str, prop: &str, value: &str) -> &'a UiNode {
        of_kind(tree, kind)
            .into_iter()
            .find(|node| node.props.get(prop) == Some(&Value::text(value)))
            .unwrap_or_else(|| panic!("no {kind} with {prop} {value} in\n{}", dump(tree)))
    }

    /// Fire `event` of the node of `kind` whose `prop` is `value`.
    fn fire_on(view: &ViewInstance, kind: &str, prop: &str, value: &str, event: &str) {
        let key = node_with(view.tree(), kind, prop, value).key.clone();
        view.fire(&key, event).unwrap();
    }

    #[test]
    fn the_notebook_lists_its_notes_and_opens_each_in_a_window_of_its_own() {
        let n = Notes::new();
        let mut list =
            n.rt.open_view(&me(), &n.app.view("notebook"), vec![Value::resource(&n.nb)])
                .unwrap();
        // These checks read the whole tree, as a host that draws every row does.
        list.set_lazy_rows(false).unwrap();
        assert_eq!(
            list.tree().kind,
            "ui.NavShell",
            "the window is a shell: {}",
            dump(list.tree())
        );
        assert_eq!(
            text_prop(of_kind(list.tree(), "ui.EmptyState")[0], "title"),
            "No notes yet"
        );
        assert_eq!(
            text_prop(
                node_with(list.tree(), "ui.Button", "label", "New note"),
                "role"
            ),
            "primary",
            "the header holds the primary action"
        );

        // The header's primary action, as the platform applies it.
        let note = n.new_note();
        list.poll_patches().unwrap();
        assert!(
            of_kind(list.tree(), "ui.TextInput").is_empty(),
            "the list does not edit notes: {}",
            dump(list.tree())
        );
        assert_eq!(titles(&list), ["Untitled"]);
        assert_eq!(
            item_titled(&list, "Untitled"),
            note,
            "activating the row opens this note"
        );

        let mut editor = n.rt.open_resource(&me(), &note, None).unwrap();
        assert_eq!(text_prop(editor.tree(), "content_kind"), "document");
        let title = of_kind(editor.tree(), "ui.TextInput")[0].key.clone();
        assert_eq!(
            editor.edit(&title, &Value::text("Plans")).unwrap(),
            EditOutcome::Applied
        );
        assert!(
            list.is_stale(),
            "the list shows the title the editor changed"
        );
        list.poll_patches().unwrap();
        assert_eq!(titles(&list), ["Plans"]);

        n.call("pin", vec![Value::resource(&note)]);
        list.poll_patches().unwrap();
        let row = of_kind(list.tree(), "ui.ListRow")[0];
        assert_eq!(text_prop(row, "mark"), "pin", "{}", dump(row));
        editor.poll_patches().unwrap();

        n.call(
            "delete",
            vec![Value::resource(&n.nb), Value::resource(&note)],
        );
        list.poll_patches().unwrap();
        assert_eq!(n.count(), 0);
        assert!(titles(&list).is_empty());
    }

    #[test]
    fn the_note_rows_carry_the_body_at_every_width() {
        let n = Notes::new();
        let note = n.new_note();
        let editor = n.rt.open_resource(&me(), &note, None).unwrap();
        let body = of_kind(editor.tree(), "ui.TextInput")[1].key.clone();
        editor.edit(&body, &Value::text("Oat milk, eggs")).unwrap();
        let mut view =
            n.rt.open_view(&me(), &n.app.view("notebook"), vec![Value::resource(&n.nb)])
                .unwrap();
        // These checks read the whole tree, as a host that draws every row does.
        view.set_lazy_rows(false).unwrap();
        for width in [90.0, 20.0] {
            view.set_space("", Space::from_rem(width, 40.0));
            view.poll_patches().unwrap();
            let row = of_kind(view.tree(), "ui.ListRow")[0];
            assert_eq!(text_prop(row, "secondary"), "Oat milk, eggs");
            assert!(
                !row.props.contains_key("mark"),
                "an unpinned note has no mark"
            );
        }
    }

    #[test]
    fn the_filter_and_the_order_are_the_windows() {
        let n = Notes::new();
        for title in ["b", "a"] {
            let note = n.new_note();
            n.call("retitle", vec![Value::resource(&note), Value::text(title)]);
            if title == "a" {
                n.call("pin", vec![Value::resource(&note)]);
            }
        }
        let mut view =
            n.rt.open_view(&me(), &n.app.view("notebook"), vec![Value::resource(&n.nb)])
                .unwrap();
        // These checks read the whole tree, as a host that draws every row does.
        view.set_lazy_rows(false).unwrap();
        assert_eq!(titles(&view), ["b", "a"]);
        assert_eq!(
            node_with(view.tree(), "ui.NavItem", "key", "is:pinned").props["count"],
            Value::Int(1)
        );
        fire_on(&view, "ui.NavItem", "key", "is:pinned", "select");
        view.poll_patches().unwrap();
        assert_eq!(titles(&view), ["a"]);
        fire_on(&view, "ui.NavItem", "key", "all", "select");
        view.poll_patches().unwrap();
        fire_on(&view, "ui.MenuItem", "label", "Title", "choose");
        view.poll_patches().unwrap();
        assert_eq!(titles(&view), ["a", "b"]);
        assert_eq!(
            node_with(view.tree(), "ui.MenuItem", "label", "Title").props["checked"],
            Value::Int(1)
        );
    }

    #[test]
    fn concurrent_body_edits_on_two_replicas_converge() {
        let app = App::compile(SOURCE, &[]);
        let (mut store_a, mut store_b) = (AmStore::new(), AmStore::new());
        let rt_a = Runtime::new(store_a.clone(), SimpleAuthority::new()).unwrap();
        let rt_b = Runtime::new(store_b.clone(), SimpleAuthority::new()).unwrap();
        app.load(&rt_a);
        app.load(&rt_b);
        let nb = rt_a.create("Notes").unwrap();
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
        let body =
            |v: &tilolang_runtime::ViewInstance| of_kind(v.tree(), "ui.TextInput")[1].key.clone();
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
