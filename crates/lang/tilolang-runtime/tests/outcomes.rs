//! Action parameter defaults run when the action is invoked (convergence design 1.1): an
//! invocation that leaves the argument out gets the default, computed on the invoker's state
//! and recorded as the argument; an explicit argument wins.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use serde_json::json;
    use tilolang_runtime::{
        AmStore, FlowOutcome, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, Value,
    };

    const SOURCE: &str = r#"app so.tilo.checks "Checks"

type Todo {
    title: Text (title)
    done: Bool = false
}

/// Checks or unchecks an item.
action check(item: Todo, done: Bool = not item.done) (label: if done { "Check" } else { "Uncheck" }, icon: .check, on: item.done) {
    item.done = done
}

action check_twice(item: Todo) {
    check(item)
    check(item)
}

flow tick(item: Todo) {
    check(item)
}

view item(item: Todo) {
    stack {
        button(check(item)) as tick
        button(check(item, done: false)) as clear
    }
}
"#;

    struct World {
        rt: Runtime,
        app: App,
        item: ResourceRef,
    }

    impl World {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            let item = rt.create("Todo").unwrap();
            Self { rt, app, item }
        }

        fn done(&self) -> Value {
            self.rt.read(&self.item, "done").unwrap()
        }

        fn check(&self, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action("check");
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        /// The arguments each change of the item recorded, in order, for the action `name`.
        fn recorded(&self, name: &str) -> Vec<serde_json::Value> {
            let id = self.app.action(name);
            self.rt
                .history(&self.item.doc)
                .unwrap()
                .into_iter()
                .filter(|note| note.action == id)
                .map(|note| note.args)
                .collect()
        }
    }

    #[test]
    fn a_left_out_argument_is_computed_when_invoked_and_recorded() {
        let w = World::new();
        w.check(vec![Value::resource(&w.item)]);
        assert_eq!(w.done(), Value::Bool(true));
        w.check(vec![Value::resource(&w.item)]);
        assert_eq!(
            w.done(),
            Value::Bool(false),
            "the default reads the state then"
        );
        let recorded = w.recorded("check");
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].get(1), Some(&json!(true)), "{recorded:?}");
        assert_eq!(recorded[1].get(1), Some(&json!(false)), "{recorded:?}");
    }

    #[test]
    fn an_explicit_argument_wins() {
        let w = World::new();
        w.check(vec![Value::resource(&w.item), Value::Bool(false)]);
        assert_eq!(
            w.done(),
            Value::Bool(false),
            "not flipped: `done: false` was asked"
        );
        w.check(vec![Value::resource(&w.item), Value::Bool(true)]);
        w.check(vec![Value::resource(&w.item), Value::Bool(true)]);
        assert_eq!(
            w.done(),
            Value::Bool(true),
            "checking twice leaves it checked"
        );
        assert_eq!(w.recorded("check")[0].get(1), Some(&json!(false)));
    }

    #[test]
    fn the_runtime_completes_the_arguments_a_caller_left_out() {
        let w = World::new();
        let id = w.app.action("check");
        let args =
            w.rt.complete_args(&id, vec![Some(Value::resource(&w.item)), None])
                .unwrap();
        assert_eq!(args[1], Value::Bool(true));
        let explicit =
            w.rt.complete_args(
                &id,
                vec![Some(Value::resource(&w.item)), Some(Value::Bool(false))],
            )
            .unwrap();
        assert_eq!(explicit[1], Value::Bool(false));
        assert!(
            w.rt.complete_args(&id, vec![None, None]).is_err(),
            "the subject has no default"
        );
    }

    #[test]
    fn a_button_invokes_with_the_default_of_when_it_is_pressed() {
        let w = World::new();
        let v =
            w.rt.open_view(&me(), &w.app.view("item"), vec![Value::resource(&w.item)])
                .unwrap();
        let (tick, clear) = (w.app.anchor("item", "tick"), w.app.anchor("item", "clear"));
        assert!(matches!(v.press(&tick).unwrap(), InvokeOutcome::Done(_)));
        assert_eq!(w.done(), Value::Bool(true));
        assert!(matches!(v.press(&tick).unwrap(), InvokeOutcome::Done(_)));
        assert_eq!(
            w.done(),
            Value::Bool(false),
            "pressed again, on the new state"
        );
        assert!(matches!(v.press(&clear).unwrap(), InvokeOutcome::Done(_)));
        assert_eq!(
            w.done(),
            Value::Bool(false),
            "an explicit `done: false` wins over the default, which would have checked it"
        );
        let recorded = w.recorded("check");
        let dones: Vec<_> = recorded.iter().map(|args| args.get(1).cloned()).collect();
        assert_eq!(
            dones,
            [Some(json!(true)), Some(json!(false))],
            "the last press changed nothing, so it left no change behind"
        );
    }

    #[test]
    fn a_flow_and_an_action_compute_it_where_they_call() {
        let w = World::new();
        let flow = w.app.flow("tick");
        let out =
            w.rt.run_flow(&me(), &flow, vec![Value::resource(&w.item)])
                .unwrap();
        assert!(matches!(out, FlowOutcome::Done(_)), "{out:?}");
        assert_eq!(w.done(), Value::Bool(true));
        assert_eq!(w.recorded("check")[0].get(1), Some(&json!(true)));
        let twice = w.app.action("check_twice");
        w.rt.invoke_action(&me(), (twice.as_str(), vec![Value::resource(&w.item)]))
            .unwrap();
        assert_eq!(
            w.done(),
            Value::Bool(true),
            "each inlined call reads the state its turn sees: unchecked, then checked again"
        );
    }

    #[test]
    fn the_label_reads_the_default_for_the_subject() {
        let w = World::new();
        let present = || {
            let view = w
                .app
                .meta()
                .actions
                .iter()
                .find(|a| a.name == "check")
                .and_then(|a| a.present.clone())
                .expect("a presentation view");
            let v =
                w.rt.open_view(&me(), &view, vec![Value::resource(&w.item)])
                    .unwrap();
            v.tree().clone()
        };
        assert_eq!(text_prop(&present(), "label"), "Check");
        w.check(vec![Value::resource(&w.item)]);
        assert_eq!(text_prop(&present(), "label"), "Uncheck");
        assert_eq!(present().props["on"], Value::Int(1));
    }

    #[test]
    fn the_catalogue_shows_the_default() {
        let w = World::new();
        let check =
            w.rt.catalogue()
                .into_iter()
                .find(|s| s.name == "check")
                .expect("listed");
        assert!(
            check.text.contains("done: Bool = not item.done"),
            "{}",
            check.text
        );
    }
}
