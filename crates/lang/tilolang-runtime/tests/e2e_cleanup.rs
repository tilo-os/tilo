//! The cleanup action of one package invoking another package's action: it asks, then replays.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, Resolution, ResourceRef, RunOutcome, Runtime, SimpleAuthority,
        Value,
    };

    const TODO: &str = include_str!("../../tilolang/tests/fixtures/todo.tl");
    const CLEANUP: &str = include_str!("../../tilolang/tests/fixtures/cleanup.tl");

    struct World {
        rt: Runtime,
        auth: SimpleAuthority,
        todo: App,
        cleanup: App,
        cleanup_id: tilolang_runtime::PackageId,
        list: ResourceRef,
    }

    fn world() -> World {
        let todo = App::compile(TODO, &[]);
        let cleanup = App::compile(CLEANUP, &[todo.meta().clone()]);
        let auth = SimpleAuthority::new();
        let rt = Runtime::new(AmStore::new(), auth.clone()).unwrap();
        todo.load(&rt);
        let cleanup_id = cleanup.load(&rt);
        let list = rt.create("TodoList").unwrap();
        World {
            rt,
            auth,
            todo,
            cleanup,
            cleanup_id,
            list,
        }
    }

    impl World {
        fn add(&self, title: &str, done: bool) {
            let id = self.todo.action("add");
            let out = self
                .rt
                .invoke_action(
                    &me(),
                    (
                        id.as_str(),
                        vec![Value::resource(&self.list), Value::text(title)],
                    ),
                )
                .unwrap();
            let item = returned_ref(out);
            if done {
                let toggle = self.todo.action("toggle");
                self.rt
                    .invoke_action(&me(), (toggle.as_str(), vec![Value::resource(&item)]))
                    .unwrap();
            }
        }

        fn titles(&self) -> Vec<String> {
            let Value::List(items) = self.rt.read(&self.list, "items").unwrap() else {
                panic!("items");
            };
            items
                .iter()
                .map(|i| {
                    let Value::Ref(r) = i else { panic!("ref") };
                    self.rt
                        .read(&r.target, "title")
                        .unwrap()
                        .as_text()
                        .unwrap()
                        .to_owned()
                })
                .collect()
        }

        fn removes(&self) -> usize {
            let remove = self.todo.action("remove");
            self.rt
                .history(&self.list.doc)
                .unwrap()
                .iter()
                .filter(|n| n.action == remove)
                .count()
        }

        fn start(&self) -> InvokeOutcome {
            self.rt
                .invoke_action(
                    &me(),
                    (
                        self.cleanup.action("clear_done"),
                        vec![Value::resource(&self.list)],
                    ),
                )
                .unwrap()
        }
    }

    #[test]
    fn the_action_asks_for_each_foreign_removal_and_replays_without_repeating_work() {
        let w = world();
        w.add("a", true);
        w.add("b", false);
        w.add("c", true);

        let InvokeOutcome::Suggested(first) = w.start() else {
            panic!("a foreign action from an untrusted package asks");
        };
        assert_eq!(first.action, w.todo.action("remove"));
        assert_eq!(w.titles(), ["a", "b", "c"], "asking changes nothing");

        let RunOutcome::Suspended(second) =
            w.rt.resolve(&first.run, &me(), Resolution::Approve)
                .unwrap()
        else {
            panic!("the second finished item asks too");
        };
        assert_eq!(w.titles(), ["b", "c"], "the first removal ran once");
        assert_eq!(w.removes(), 1);

        let out =
            w.rt.resolve(&second.run, &me(), Resolution::Approve)
                .unwrap();
        assert_eq!(out, RunOutcome::Done(Value::Int(2)));
        assert_eq!(w.titles(), ["b"]);
        assert_eq!(
            w.removes(),
            2,
            "replays answered from the journal never ran an action again"
        );
    }

    #[test]
    fn a_declined_removal_leaves_the_item_and_the_run_goes_on() {
        let w = world();
        w.add("a", true);
        let InvokeOutcome::Suggested(s) = w.start() else {
            panic!("expected a suggestion");
        };
        let out = w.rt.resolve(&s.run, &me(), Resolution::Decline).unwrap();
        assert!(matches!(out, RunOutcome::Done(Value::Int(1))), "{out:?}");
        assert_eq!(w.titles(), ["a"]);
    }

    #[test]
    fn trusting_the_cleanup_package_runs_it_without_asking() {
        let w = world();
        w.add("a", true);
        w.add("b", false);
        w.auth.trust(&w.cleanup_id);
        assert_eq!(w.start(), InvokeOutcome::Done(Value::Int(1)));
        assert_eq!(w.titles(), ["b"]);
    }

    #[test]
    fn the_cleanup_view_button_runs_the_action() {
        let w = world();
        w.add("a", true);
        let view =
            w.rt.open_view(
                &me(),
                &w.cleanup.view("cleanup"),
                vec![Value::resource(&w.list)],
            )
            .unwrap();
        let button = of_kind(view.tree(), "ui.Button")[0].key.clone();
        let InvokeOutcome::Suggested(s) = view.press(&button).unwrap() else {
            panic!("the action asks before it removes");
        };
        assert_eq!(s.action, w.todo.action("remove"));
        let out = w.rt.resolve(&s.run, &me(), Resolution::Approve).unwrap();
        assert_eq!(out, RunOutcome::Done(Value::Int(1)));
        assert!(w.titles().is_empty());
    }
}
