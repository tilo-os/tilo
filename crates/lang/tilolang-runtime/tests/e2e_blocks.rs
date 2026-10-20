//! Agent blocks compiled against a loaded package and evaluated by the runtime.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{
        AmStore, BlockOutput, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, Value,
    };

    const TODO: &str = include_str!("../../tilolang/tests/fixtures/todo.tl");

    struct World {
        rt: Runtime,
        auth: SimpleAuthority,
        todo: App,
        list: ResourceRef,
    }

    fn world() -> World {
        let todo = App::compile(TODO, &[]);
        let auth = SimpleAuthority::new();
        let rt = Runtime::new(AmStore::new(), auth.clone()).unwrap();
        todo.load(&rt);
        let list = rt.create("TodoList").unwrap();
        for title in ["milk", "eggs"] {
            let id = todo.action("add");
            rt.invoke_action(
                &me(),
                (
                    id.as_str(),
                    vec![Value::resource(&list), Value::text(title)],
                ),
            )
            .unwrap();
        }
        World {
            rt,
            auth,
            todo,
            list,
        }
    }

    impl World {
        fn block(&self, src: &str) -> tilolang::CompiledBlock {
            tilolang::compile_block(src, &[self.todo.meta().clone()])
                .unwrap_or_else(|d| panic!("{d:#?}"))
        }

        fn list_ref(&self) -> String {
            format!("@todo.TodoList:{}", self.list.id_string())
        }
    }

    #[test]
    fn a_block_that_applies_a_foreign_view_renders_the_view_through_the_runtime() {
        let w = world();
        let block = w.block(&format!("todo.todo_list({})", w.list_ref()));
        let BlockOutput::View(mut view) = w.rt.run_block(&me(), &block).unwrap() else {
            panic!("a view block renders");
        };
        let tree = view.tree();
        assert_eq!(tree.kind, "fragment", "{}", dump(tree));
        assert_eq!(of_kind(tree, "row").len(), 2, "{}", dump(tree));
        assert_eq!(text_prop(of_kind(tree, "heading")[0], "value"), " (2 open)");

        let remove = of_kind(of_kind(tree, "row")[0], "button")[0].key.clone();
        let out = view.press(&remove).unwrap();
        assert!(
            matches!(out, InvokeOutcome::Done(_)),
            "the view's buttons are the todo package's own: {out:?}"
        );
        view.poll_patches().unwrap();
        assert_eq!(of_kind(view.tree(), "row").len(), 1);
    }

    #[test]
    fn a_blocks_own_button_asks_because_the_agent_wrote_it() {
        let w = world();
        let src = format!(
            "column {{\n    text(\"Clean up\")\n    button(\"Add\", on: todo.add({}, \"x\"))\n}}",
            w.list_ref()
        );
        let BlockOutput::View(view) = w.rt.run_block(&me(), &w.block(&src)).unwrap() else {
            panic!("a view block renders");
        };
        let button = of_kind(view.tree(), "button")[0].key.clone();
        let InvokeOutcome::Suggested(s) = view.press(&button).unwrap() else {
            panic!("code from a stranger has no grants");
        };
        assert_eq!(s.action, w.todo.action("add"));
        assert_eq!(s.chain.len(), 1);
    }

    #[test]
    fn an_action_block_is_invoked_or_suggested_through_the_authority() {
        let w = world();
        let src = format!("todo.add({}, \"bread\")", w.list_ref());
        let block = w.block(&src);
        let BlockOutput::Action(out) = w.rt.run_block(&me(), &block).unwrap() else {
            panic!("an action block");
        };
        assert!(matches!(out, InvokeOutcome::Suggested(_)), "{out:?}");

        let module = tilolang_runtime::ModuleHash::of_bytes(&block.wasm);
        w.auth.trust(&module);
        let BlockOutput::Action(out) = w.rt.run_block(&me(), &block).unwrap() else {
            panic!("an action block");
        };
        assert!(matches!(out, InvokeOutcome::Done(Value::Ref(_))), "{out:?}");
        assert_eq!(
            w.rt.project(&w.rt.read(&w.list, "items").unwrap())["count"],
            serde_json::json!(3)
        );
    }

    #[test]
    fn a_data_block_returns_its_value() {
        let w = world();
        let BlockOutput::Data(v) = w.rt.run_block(&me(), &w.block("1 + 2")).unwrap() else {
            panic!("data");
        };
        assert_eq!(v, Value::Int(3));
        let src = w.list_ref();
        let BlockOutput::Data(v) = w.rt.run_block(&me(), &w.block(&src)).unwrap() else {
            panic!("data");
        };
        assert!(
            matches!(v, Value::Ref(ref r) if r.target == w.list),
            "{v:?}"
        );
        let projection = w.rt.project(&v);
        assert_eq!(projection["type"], serde_json::json!("TodoList"));
    }
}
