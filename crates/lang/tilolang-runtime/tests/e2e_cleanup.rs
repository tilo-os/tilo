//! The cleanup action of one package invoking another package's action: it asks, then replays.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, RunOutcome, Runtime, Value};

    const TODO: &str = include_str!("../../tilolang/tests/fixtures/todo.tl");
    const CLEANUP: &str = include_str!("../../tilolang/tests/fixtures/cleanup.tl");

    struct World {
        rt: Runtime,
        auth: TestAuthority,
        todo: App,
        cleanup: App,
        cleanup_id: tilolang_runtime::PackageId,
        list: ResourceRef,
        dispatch: Dispatch,
    }

    fn world() -> World {
        let todo = App::compile(TODO, &[]);
        let cleanup = App::compile(CLEANUP, &[todo.meta().clone()]);
        let auth = TestAuthority::new();
        let rt = Runtime::new(AmStore::new(), auth.clone()).unwrap();
        todo.load(&rt);
        let cleanup_id = cleanup.load(&rt);
        let dispatch = Dispatch::new();
        rt.set_foreign_invoker(dispatch.clone());
        let list = rt.create("TodoList").unwrap();
        World {
            rt,
            auth,
            todo,
            cleanup,
            cleanup_id,
            list,
            dispatch,
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

    tilo_test_env::scenario! {
        fn the_cleanup_asks_is_declined_or_is_trusted;

        phase the_action_asks_for_each_foreign_removal_and_replays_without_repeating_work;
        {
            let w = world();
            w.add("a", true);
            w.add("b", false);
            w.add("c", true);

            let InvokeOutcome::Waits { run, .. } = w.start() else {
                panic!("a foreign action from an untrusted package asks");
            };
            assert_eq!(w.dispatch.asked().0, w.todo.action("remove"));
            assert_eq!(w.titles(), ["a", "b", "c"], "asking changes nothing");

            let RunOutcome::Waits { .. } = w.dispatch.allow(&w.rt, &run) else {
                panic!("the second finished item asks too");
            };
            assert_eq!(w.titles(), ["b", "c"], "the first removal ran once");
            assert_eq!(w.removes(), 1);

            let out = w.dispatch.allow(&w.rt, &run);
            assert_eq!(out, RunOutcome::Done(Value::Int(2)));
            assert_eq!(w.titles(), ["b"]);
            assert_eq!(
                w.removes(),
                2,
                "replays answered from the journal never ran an action again"
            );
        }

        phase a_declined_removal_leaves_the_item_and_ends_the_run;
        {
            let w = world();
            w.add("a", true);
            let InvokeOutcome::Waits { run, .. } = w.start() else {
                panic!("expected a wait");
            };
            let error = decline(&w.rt, &run);
            assert!(
                matches!(error, tilolang_runtime::RuntimeError::Denied(_)),
                "{error}"
            );
            assert_eq!(w.titles(), ["a"], "the declined run changes nothing");
        }

        phase trusting_the_cleanup_package_runs_it_without_asking;
        {
            let w = world();
            w.add("a", true);
            w.add("b", false);
            w.auth.trust(&w.cleanup_id);
            assert_eq!(w.start(), InvokeOutcome::Done(Value::Int(1)));
            assert_eq!(w.titles(), ["b"]);
            assert_eq!(w.dispatch.len(), 0, "no call needed a person");
        }
    }

    #[test]
    fn the_cleanup_view_button_asks_before_it_removes() {
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
        // The action is the cleanup's own, so it runs up to the foreign removal it asks about:
        // the run waits, and the dispatch is asked for the removal.
        let InvokeOutcome::Waits { .. } = view.press(&button).unwrap() else {
            panic!("the action asks before it removes");
        };
        assert_eq!(w.dispatch.asked().0, w.todo.action("remove"));
        assert_eq!(w.titles(), ["a"], "asking changes nothing");
    }
}
