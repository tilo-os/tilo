//! Runs: an action that calls another action journals the call, an Ask suspends the run,
//! resolving replays it from the journal, and the journaled calls are not executed again.

#[cfg(test)]
mod cases {
    use std::cell::Cell;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::{
        InvokeOutcome, ModelHost, Pending, Resolution, RunOutcome, Runtime, Store, Value,
    };

    fn incs(w: &World, c: &tilolang_runtime::ResourceRef) -> usize {
        w.rt.history(&c.doc)
            .unwrap()
            .iter()
            .filter(|n| n.action == "inc")
            .count()
    }

    fn world() -> (World, tilolang_runtime::ResourceRef) {
        let w = World::new();
        w.rt.load_package(&stranger_wasm(), stranger_meta())
            .unwrap();
        let c = w.counter();
        (w, c)
    }

    fn run(w: &World, action: &str, args: Vec<Value>) -> InvokeOutcome {
        w.rt.invoke_action(&w.me, (action.to_owned(), args))
            .unwrap()
    }

    /// The run every call of `inc` in `c`'s document was made in.
    fn invocations(w: &World, c: &tilolang_runtime::ResourceRef) -> Vec<Option<String>> {
        w.rt.history(&c.doc)
            .unwrap()
            .into_iter()
            .filter(|n| n.action == "inc")
            .map(|n| n.invocation)
            .collect()
    }

    #[test]
    fn an_action_calling_its_own_modules_actions_is_one_action_of_the_workspace() {
        let w = World::new();
        let c = w.counter();
        assert_eq!(
            run(&w, "twice", vec![Value::resource(&c)]),
            InvokeOutcome::Done(Value::Int(2))
        );
        let calls = invocations(&w, &c);
        assert_eq!(calls.len(), 2, "each call is a change of its own");
        assert_eq!(
            calls[0], calls[1],
            "every call ran under the one invocation of the action that called it"
        );
        assert!(calls[0].is_some(), "a run names its invocation");
    }

    #[test]
    fn an_action_calling_another_apps_actions_is_one_action_of_the_workspace() {
        let (w, c) = world();
        w.auth.trust(&Runtime::module_hash(&stranger_wasm()));
        assert_eq!(
            run(&w, "ask", vec![Value::resource(&c)]),
            InvokeOutcome::Done(Value::Int(2))
        );
        let calls = invocations(&w, &c);
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0], calls[1],
            "the other app's actions are steps of the one action that called them"
        );
        assert!(
            w.rt.history(&c.doc)
                .unwrap()
                .iter()
                .all(|n| n.action != "ask"),
            "the callee's change stays the callee's, inside the caller's action"
        );
    }

    #[test]
    fn an_action_that_calls_two_actions_and_then_fails_leaves_nothing() {
        let (w, c) = world();
        w.auth.trust(&Runtime::module_hash(&stranger_wasm()));
        let boom =
            w.rt.invoke_action(&w.me, ("boom".to_owned(), vec![Value::resource(&c)]));
        assert!(boom.is_err(), "the action trapped after both calls");
        assert!(
            invocations(&w, &c).is_empty(),
            "nothing of a run that failed anywhere is left"
        );
        // The calls it made did run, and did not stay: the next run's are the only changes.
        assert_eq!(
            run(&w, "ask", vec![Value::resource(&c)]),
            InvokeOutcome::Done(Value::Int(2))
        );
        assert_eq!(invocations(&w, &c).len(), 2);
    }

    #[test]
    fn a_run_that_waits_for_an_answer_goes_on_under_an_invocation_of_its_own() {
        let (w, c) = world();
        let InvokeOutcome::Suggested(first) = run(&w, "ask", vec![Value::resource(&c)]) else {
            panic!("the stranger's call asks first");
        };
        let RunOutcome::Suspended(second) =
            w.rt.resolve(&first.run, &w.me, Resolution::Approve)
                .unwrap()
        else {
            panic!("the second call asks too");
        };
        w.rt.resolve(&second.run, &w.me, Resolution::Approve)
            .unwrap();
        let calls = invocations(&w, &c);
        assert_eq!(calls.len(), 2);
        assert_ne!(
            calls[0], calls[1],
            "the wait split the run: what it does on from the answer is an action of its own"
        );
    }

    #[test]
    fn an_ask_suspends_and_resolving_replays_without_repeating_work() {
        let (w, c) = world();
        let args = vec![Value::resource(&c)];

        let got = run(&w, "ask", args);
        let InvokeOutcome::Suggested(first) = got else {
            panic!("the first foreign invoke must ask: {got:?}");
        };
        assert_eq!(first.action, "inc");
        assert_eq!(incs(&w, &c), 0);

        let RunOutcome::Suspended(second) =
            w.rt.resolve(&first.run, &w.me, Resolution::Approve)
                .unwrap()
        else {
            panic!("the second foreign invoke must ask too");
        };
        assert_eq!(second.run, first.run);
        assert_eq!(
            incs(&w, &c),
            1,
            "the replay answered the first invoke from the journal instead of running it again"
        );

        let out =
            w.rt.resolve(&second.run, &w.me, Resolution::Approve)
                .unwrap();
        assert_eq!(out, RunOutcome::Done(Value::Int(2)));
        assert_eq!(incs(&w, &c), 2);
    }

    #[test]
    fn a_declined_step_is_journaled_and_the_run_goes_on() {
        let (w, c) = world();
        let InvokeOutcome::Suggested(first) = run(&w, "ask", vec![Value::resource(&c)]) else {
            panic!("expected a suggestion");
        };
        let RunOutcome::Suspended(second) =
            w.rt.resolve(&first.run, &w.me, Resolution::Decline)
                .unwrap()
        else {
            panic!("the run continues to its second invoke");
        };
        let out =
            w.rt.resolve(&second.run, &w.me, Resolution::Approve)
                .unwrap();
        assert_eq!(out, RunOutcome::Done(Value::Int(1)));
        assert_eq!(incs(&w, &c), 1);
    }

    #[test]
    fn trusting_the_author_lets_the_run_without_asking() {
        let (w, c) = world();
        let stranger = Runtime::module_hash(&stranger_wasm());
        w.auth.trust(&stranger);
        assert_eq!(
            run(&w, "ask", vec![Value::resource(&c)]),
            InvokeOutcome::Done(Value::Int(2))
        );
    }

    #[test]
    fn a_suspended_run_survives_a_restart() {
        let (w, c) = world();
        let InvokeOutcome::Suggested(first) = run(&w, "ask", vec![Value::resource(&c)]) else {
            panic!("expected a suggestion");
        };
        let store = w.store.clone();
        drop(w);

        let rt = replica(
            &store,
            &[
                (counter_wasm(), counter_meta()),
                (stranger_wasm(), stranger_meta()),
            ],
        );
        let me = me();
        let RunOutcome::Suspended(second) =
            rt.resolve(&first.run, &me, Resolution::Approve).unwrap()
        else {
            panic!("expected the second suggestion");
        };
        let out = rt.resolve(&second.run, &me, Resolution::Approve).unwrap();
        assert_eq!(out, RunOutcome::Done(Value::Int(2)));
    }

    struct CountingModel(Rc<Cell<u32>>);

    impl ModelHost for CountingModel {
        fn call(&self, kind: &str, input: &Value) -> Result<Value, String> {
            self.0.set(self.0.get().saturating_add(1));
            Ok(Value::text(format!(
                "{kind}:{}",
                input.as_text().unwrap_or("?")
            )))
        }
    }

    #[test]
    fn model_calls_are_journaled_and_not_repeated_on_replay() {
        let (w, c) = world();
        let calls = Rc::new(Cell::new(0));
        w.rt.set_model_host(CountingModel(Rc::clone(&calls)));
        let InvokeOutcome::Suggested(s) = run(&w, "m", vec![Value::resource(&c)]) else {
            panic!("expected a suggestion");
        };
        assert_eq!(calls.get(), 1);
        let out = w.rt.resolve(&s.run, &w.me, Resolution::Approve).unwrap();
        assert_eq!(out, RunOutcome::Done(Value::text("classify:hello")));
        assert_eq!(calls.get(), 1, "the replay used the journaled answer");
    }

    #[test]
    fn the_journal_is_persisted_through_the_store() {
        let (w, c) = world();
        let InvokeOutcome::Suggested(s) = run(&w, "ask", vec![Value::resource(&c)]) else {
            panic!("expected a suggestion");
        };
        let mut store = w.store;
        let journal = store.load_journal(&s.run).unwrap().unwrap();
        assert_eq!(journal.action.as_deref(), Some("ask"));
        assert!(journal.entries.is_empty());
        let Pending::Ask(pending) = journal.pending.unwrap() else {
            panic!("the run waits for a person");
        };
        assert_eq!(pending.action, "inc");
    }
}
