//! The todo fixture: insert, toggle, move, remove, the keyed rows and the bound inputs.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, Patch, ResourceRef, Runtime, SimpleAuthority, UiNode, Value,
    };

    const SOURCE: &str = include_str!("../../tilolang/tests/fixtures/todo.tl");

    struct Todo {
        rt: Runtime,
        app: App,
        list: ResourceRef,
    }

    impl Todo {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            let list = rt.create("TodoList").unwrap();
            Self { rt, app, list }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        fn add(&self, title: &str) -> ResourceRef {
            let out = self.call("add", vec![Value::resource(&self.list), Value::text(title)]);
            returned_ref(out)
        }

        fn view(&self) -> tilolang_runtime::ViewInstance {
            self.rt
                .open_view(
                    &me(),
                    &self.app.view("todo_list"),
                    vec![Value::resource(&self.list)],
                )
                .unwrap()
        }
    }

    fn rows(tree: &UiNode) -> Vec<&UiNode> {
        of_kind(tree, "row")
    }

    fn titles(tree: &UiNode) -> Vec<String> {
        rows(tree)
            .iter()
            .map(|r| text_prop(of_kind(r, "checkbox")[0], "label"))
            .collect()
    }

    fn heading(tree: &UiNode) -> String {
        text_prop(of_kind(tree, "heading")[0], "value")
    }

    #[test]
    fn items_are_added_toggled_and_counted() {
        let t = Todo::new();
        t.call(
            "rename",
            vec![Value::resource(&t.list), Value::text("Groceries")],
        );
        let milk = t.add("milk");
        t.add("eggs");
        assert_eq!(t.rt.read(&milk, "title").unwrap(), Value::text("milk"));
        assert_eq!(t.rt.read(&milk, "done").unwrap(), Value::Bool(false));

        let mut view = t.view();
        assert_eq!(heading(view.tree()), "Groceries (2 open)");
        assert_eq!(titles(view.tree()), ["milk", "eggs"]);

        t.call("toggle", vec![Value::resource(&milk)]);
        assert_eq!(t.rt.read(&milk, "done").unwrap(), Value::Bool(true));
        let patches = view.poll_patches().unwrap();
        assert!(
            patches.iter().any(
                |p| matches!(p, Patch::SetProp { name, value: Some(Value::Text(v)), .. }
                if name == "value" && v == "Groceries (1 open)")
            ),
            "{patches:?}"
        );
        assert_eq!(heading(view.tree()), "Groceries (1 open)");
    }

    #[test]
    fn rows_are_keyed_by_the_identity_of_their_item() {
        let t = Todo::new();
        let a = t.add("a");
        let b = t.add("b");
        let view = t.view();
        let keys: Vec<&String> = rows(view.tree()).iter().map(|r| &r.key).collect();
        let item_id = |r: &ResourceRef| r.path.last().unwrap().item.clone();
        assert!(keys[0].contains(&item_id(&a)), "{keys:?}");
        assert!(keys[1].contains(&item_id(&b)), "{keys:?}");
        assert_ne!(keys[0], keys[1]);
    }

    #[test]
    fn moving_an_item_moves_its_row_and_touches_nothing_else() {
        let t = Todo::new();
        t.add("a");
        t.add("b");
        let c = t.add("c");
        let mut view = t.view();
        let before: Vec<String> = rows(view.tree()).iter().map(|r| r.key.clone()).collect();

        t.call(
            "move_to",
            vec![Value::resource(&t.list), Value::resource(&c), Value::Int(0)],
        );
        let patches = view.poll_patches().unwrap();
        assert_eq!(
            patches,
            vec![Patch::Move {
                parent: vec![view.tree().key.clone()],
                key: before[2].clone(),
                // The heading is the column's first child, so the first row is at 1.
                to: 1
            }]
        );
        assert_eq!(titles(view.tree()), ["c", "a", "b"]);
    }

    #[test]
    fn pressing_a_rows_remove_button_removes_that_item() {
        let t = Todo::new();
        t.add("a");
        t.add("b");
        t.add("c");
        let mut view = t.view();
        let remove = {
            let row = rows(view.tree())[1];
            of_kind(row, "button")[0].key.clone()
        };
        assert!(matches!(
            view.press(&remove).unwrap(),
            InvokeOutcome::Done(_)
        ));
        let patches = view.poll_patches().unwrap();
        assert!(
            patches.iter().any(|p| matches!(p, Patch::Remove { .. })),
            "{patches:?}"
        );
        assert_eq!(titles(view.tree()), ["a", "c"]);
        assert_eq!(
            t.rt.project(&t.rt.read(&t.list, "items").unwrap())["count"],
            serde_json::json!(2)
        );
    }

    #[test]
    fn editing_the_bound_name_splices_the_text() {
        let t = Todo::new();
        let mut view = t.view();
        let input = of_kind(view.tree(), "input")[0].key.clone();
        for text in ["Errands", "Errands today", "Chores today"] {
            assert_eq!(
                view.edit(&input, &Value::text(text)).unwrap(),
                tilolang_runtime::EditOutcome::Applied
            );
            assert_eq!(t.rt.read(&t.list, "name").unwrap(), Value::text(text));
        }
        view.poll_patches().unwrap();
        assert!(heading(view.tree()).starts_with("Chores today"));
    }

    #[test]
    fn a_bound_checkbox_toggles_the_item() {
        let t = Todo::new();
        let milk = t.add("milk");
        let mut view = t.view();
        let checkbox = of_kind(view.tree(), "checkbox")[0].key.clone();
        view.edit(&checkbox, &Value::Bool(true)).unwrap();
        assert_eq!(t.rt.read(&milk, "done").unwrap(), Value::Bool(true));
        view.poll_patches().unwrap();
        assert!(heading(view.tree()).contains("(0 open)"));
    }
}
