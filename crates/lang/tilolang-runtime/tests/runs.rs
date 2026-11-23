//! Runs: an action that calls another action journals the call, a call that needs a person's yes
//! waits through the dispatch, answering it replays the run from the journal, and the journaled
//! calls are not executed again.

#[cfg(test)]
mod cases {
    use std::cell::Cell;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::delegate::Invoked;
    use tilolang_runtime::{InvokeOutcome, ModelHost, Pending, RunOutcome, Runtime, Store, Value};

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

    tilo_test_env::scenario! {
        fn an_action_calling_actions_is_one_action;

        phase an_action_calling_its_own_modules_actions_is_one_action_of_the_workspace;
        {
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

        phase an_action_that_calls_two_actions_and_then_fails_leaves_nothing;
        {
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

        phase a_run_keeps_its_journal_across_the_answers;
        {
            let (w, c) = world();
            let InvokeOutcome::Waits { run, .. } = run(&w, "ask", vec![Value::resource(&c)]) else {
                panic!("the stranger's call asks first");
            };
            let RunOutcome::Waits { run: second, .. } = w.dispatch.allow(&w.rt, &run) else {
                panic!("the second call asks too");
            };
            assert_eq!(second, run, "the same run waits on the second call");
            w.dispatch.allow(&w.rt, &run);
            assert_eq!(incs(&w, &c), 2, "each call ran once");
        }
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

    tilo_test_env::scenario! {
        fn a_run_waits_on_an_asked_call_and_answers_it;

        phase an_asked_call_waits_and_the_answer_resumes_without_repeating_work;
        {
            let (w, c) = world();
            let args = vec![Value::resource(&c)];

            let got = run(&w, "ask", args);
            let InvokeOutcome::Waits { run, call } = got else {
                panic!("the first foreign invoke must ask: {got:?}");
            };
            assert_eq!(w.dispatch.len(), 1, "the call went through the dispatch");
            assert_eq!(w.dispatch.asked().0, "inc");
            assert_eq!(incs(&w, &c), 0);

            let RunOutcome::Waits { call: next, .. } = w.dispatch.allow(&w.rt, &run) else {
                panic!("the second foreign invoke must ask too");
            };
            assert_eq!(next, format!("call-2"));
            assert_eq!(call, "call-1");
            assert_eq!(
                incs(&w, &c),
                1,
                "the replay answered the first invoke from the journal instead of running it again"
            );

            let out = w.dispatch.allow(&w.rt, &run);
            assert_eq!(out, RunOutcome::Done(Value::Int(2)));
            assert_eq!(incs(&w, &c), 2);
        }

        phase a_declined_call_ends_the_run_and_takes_back_its_writes;
        {
            let (w, c) = world();
            let InvokeOutcome::Waits { run, .. } = run(&w, "ask", vec![Value::resource(&c)]) else {
                panic!("expected a wait");
            };
            let error = w.dispatch.decline(&w.rt, &run);
            assert!(
                matches!(error, tilolang_runtime::RuntimeError::Denied(_)),
                "{error}"
            );
            assert_eq!(incs(&w, &c), 0, "nothing of the declined run stands");
            let mut store = w.store;
            assert!(
                store.load_journal(&run).unwrap().unwrap().finished,
                "the run ended"
            );
        }

        phase trusting_the_author_lets_the_run_without_asking;
        {
            let (w, c) = world();
            let stranger = Runtime::module_hash(&stranger_wasm());
            w.auth.trust(&stranger);
            assert_eq!(
                run(&w, "ask", vec![Value::resource(&c)]),
                InvokeOutcome::Done(Value::Int(2))
            );
            assert_eq!(w.dispatch.len(), 0, "no call needed a person");
        }

        phase a_waiting_run_survives_a_restart;
        {
            let (w, c) = world();
            let InvokeOutcome::Waits { run, .. } = run(&w, "ask", vec![Value::resource(&c)]) else {
                panic!("expected a wait");
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
            // The first call was asked before the restart, so its invocation is gone with the
            // host that asked it; its answer is known here. The run goes on to its second call,
            // which the new host's dispatch is asked about.
            let dispatch = Dispatch::new();
            rt.set_foreign_invoker(dispatch.clone());
            rt.invoke_action(&me(), ("inc", vec![Value::resource(&c)]))
                .unwrap();
            let RunOutcome::Waits { .. } = rt.answer_call(&run, Invoked::Done(Value::Unit)).unwrap()
            else {
                panic!("expected the second call to ask");
            };
            assert_eq!(dispatch.asked().0, "inc");
            let out = dispatch.allow(&rt, &run);
            assert_eq!(out, RunOutcome::Done(Value::Int(2)));
        }

        phase the_journal_is_persisted_through_the_store;
        {
            let (w, c) = world();
            let InvokeOutcome::Waits { run, call } = run(&w, "ask", vec![Value::resource(&c)]) else {
                panic!("expected a wait");
            };
            let mut store = w.store;
            let journal = store.load_journal(&run).unwrap().unwrap();
            assert_eq!(journal.action.as_deref(), Some("ask"));
            assert!(journal.entries.is_empty());
            let Some(Pending::Call {
                call: waiting,
                action,
            }) = journal.pending
            else {
                panic!("the run waits on a call");
            };
            assert_eq!(waiting, call);
            assert_eq!(action, "inc");
        }
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
        let InvokeOutcome::Waits { run, .. } = run(&w, "m", vec![Value::resource(&c)]) else {
            panic!("expected a wait");
        };
        assert_eq!(calls.get(), 1);
        let out = w.dispatch.allow(&w.rt, &run);
        assert_eq!(out, RunOutcome::Done(Value::text("classify:hello")));
        assert_eq!(calls.get(), 1, "the replay used the journaled answer");
    }
}
