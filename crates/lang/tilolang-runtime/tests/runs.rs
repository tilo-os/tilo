//! Runs: an action that calls another action journals the call, a call that needs a person's yes
//! waits through the dispatch, answering it replays the run from the journal, and the journaled
//! calls are not executed again.

#[cfg(test)]
mod cases {
    use std::cell::Cell;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::delegate::Invoked;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ModelHost, Pending, RunOutcome, Runtime, SimpleAuthority, Store,
        Value,
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
            let error = decline(&w.rt, &run);
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

    /// An action that reads its collection, calls its own insert for each name it lacks, and
    /// writes again: every call and every write of the run happens once, though the run is run
    /// again from its start after each call.
    const ATLAS: &str = r#"app so.tilo.atlas "Atlas"

type Item {
    name: Text
}

type Board {
    items: Collection<Item>
    taken: Int merge counter
}

root Board

default view board for Board

view board(b: Board) {
    text("{b.items.len()}")
}

action add(b: Board, name: Text) {
    b.items.insert(Item(name: name))
}

action take(b: Board, names: List<Text>) -> Int {
    let known = b.items.count()
    for name in names {
        if not b.items.any(i -> i.name == name) {
            add(b, name)
        }
    }
    b.taken += 1
    return b.items.count() - known
}
"#;

    /// An app that asks the device a question whose answer moves on, and then calls one of two
    /// actions of its own by what the answer said: a run that replays must read the answer it
    /// read the first time, or it takes the other branch and diverges from its journal.
    const POLLED: &str = r#"app so.tilo.polled "Polled"

type Note {
    name: Text
}

type Lib {
    notes: Collection<Note>
    state: Text
}

root Lib

default view lib for Lib

view lib(l: Lib) {
    text("{l.state}")
}

action land(l: Lib, name: Text) {
    l.notes.insert(Note(name: name))
}

action other(l: Lib, name: Text) {
    l.state = "other {name}"
}

action poll(l: Lib) -> Text {
    match engine.call("next", "") {
        .ok(step) => {
            if step == "1" {
                land(l, "one")
                l.state = "one"
            } else {
                other(l, "two")
                l.state = "two"
            }
            l.state
        }
        .err(why) => why
    }
}
"#;

    /// The device work an app asks for: each answer is the count it was asked at, moving on with
    /// every question, as a job's progress does.
    #[derive(Clone, Default)]
    struct Moving(Rc<Cell<u32>>);

    impl tilolang_runtime::host_work::EngineHost for Moving {
        fn call(&self, _app: &str, _op: &str, _input: &str) -> Result<String, String> {
            let n = self.0.get().saturating_add(1);
            self.0.set(n);
            Ok(n.to_string())
        }

        fn start(&self, _app: &str, _op: &str, _input: &str) -> Result<String, String> {
            self.call("", "", "")
        }

        fn status(&self, _job: &str) -> Result<String, String> {
            self.call("", "", "")
        }

        fn cancel(&self, _job: &str) -> bool {
            true
        }
    }

    #[test]
    fn a_run_that_calls_an_action_reads_the_device_answer_it_read() {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        let device = Moving::default();
        rt.set_engine(device.clone());
        let app = App::compile(POLLED, &[]);
        app.load(&rt);
        let lib = rt.create_root("Lib").unwrap();
        let poll = app.action("poll");
        let out = rt
            .invoke_action(&me(), (poll.as_str(), vec![Value::resource(&lib)]))
            .unwrap();
        assert_eq!(
            out,
            InvokeOutcome::Done(Value::text("one")),
            "the branch the first answer chose is the one the run stays on"
        );
        assert_eq!(
            device.0.get(),
            1,
            "the device was asked once, not again for the replay"
        );
        match rt.read(&lib, "notes").unwrap() {
            Value::List(items) => assert_eq!(items.len(), 1, "the call it made once"),
            other => panic!("notes reads {other:?}"),
        }
        assert_eq!(rt.read(&lib, "state").unwrap(), Value::text("one"));
    }

    #[test]
    fn an_action_that_calls_one_of_its_own_runs_each_call_once() {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        let app = App::compile(ATLAS, &[]);
        app.load(&rt);
        let board = rt.create_root("Board").unwrap();
        let take = app.action("take");
        let out = rt
            .invoke_action(
                &me(),
                (
                    take.as_str(),
                    vec![
                        Value::resource(&board),
                        Value::List(vec![Value::text("one"), Value::text("two")]),
                    ],
                ),
            )
            .unwrap();
        assert_eq!(out, InvokeOutcome::Done(Value::Int(2)), "{out:?}");
        match rt.read(&board, "items").unwrap() {
            Value::List(items) => assert_eq!(items.len(), 2, "each call inserted one item once"),
            other => panic!("items reads {other:?}"),
        }
        assert_eq!(
            rt.read(&board, "taken").unwrap(),
            Value::Int(1),
            "the write after the calls happens once"
        );
        let add = app.action("add");
        assert_eq!(
            rt.history(&board.doc)
                .unwrap()
                .iter()
                .filter(|note| note.action == add)
                .count(),
            2,
            "each call is one change, and ten replays make no more"
        );
    }

    /// A device that checks what a design holds: its answer is an action of its own app, which a
    /// person allows before it runs, as a call that reaches outside the caller's package does.
    const DEVICE: &str = r#"app so.tilo.device "Device"

type Desk {
    asked: Int merge counter
}

root Desk

default view desk for Desk

view desk(d: Desk) {
    text("{d.asked}")
}

/// The device's verdict on a design of `left` parts.
action verdict(d: Desk, left: Int) -> Text {
    d.asked += 1
    return "holds {left}"
}
"#;

    /// An action that removes a part and then asks the device about what is left: the run waits
    /// on the device between the two, and the answer goes on from where it asked.
    const TRIM: &str = r#"app so.tilo.trim "Trim"

use so.tilo.device as device

type Part {
    name: Text
}

type Design {
    parts: Collection<Part>
    verdict: Text
    trims: Int merge counter
}

root Design

default view design for Design

view design(g: Design) {
    text("{g.parts.len()}")
}

action add(g: Design, name: Text) {
    g.parts.insert(Part(name: name))
}

action trim(g: Design, d: device.Desk, name: Text) -> Text {
    for held in g.parts {
        if held.name == name { g.parts.remove(held) }
    }
    let said = device.verdict(d, g.parts.count())
    g.verdict = said
    g.trims += 1
    return said
}

/// Removes the parts a filter picks out and the ones `remove_where` matches, then asks.
action prune(g: Design, d: device.Desk, first: Text, second: Text) -> Text {
    for held in g.parts.filter(p -> p.name == first) {
        g.parts.remove(held)
    }
    let gone = g.parts.remove_where(p -> p.name == second)
    let said = device.verdict(d, g.parts.count())
    g.verdict = "{said} after {gone}"
    g.trims += 1
    return said
}
"#;

    struct Trim {
        rt: Runtime,
        trim: App,
        dispatch: Dispatch,
        design: tilolang_runtime::ResourceRef,
        desk: tilolang_runtime::ResourceRef,
    }

    fn trim_world() -> Trim {
        let device = App::compile(DEVICE, &[]);
        let trim = App::compile(TRIM, &[device.meta().clone()]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        device.load(&rt);
        trim.load(&rt);
        let dispatch = Dispatch::new();
        rt.set_foreign_invoker(dispatch.clone());
        let design = rt.create_root("Design").unwrap();
        let desk = rt.create_root("Desk").unwrap();
        let add = trim.action("add");
        for name in ["a", "b", "c"] {
            rt.invoke_action(
                &me(),
                (add.as_str(), vec![Value::resource(&design), Value::text(name)]),
            )
            .unwrap();
        }
        Trim {
            rt,
            trim,
            dispatch,
            design,
            desk,
        }
    }

    impl Trim {
        fn start(&self, name: &str) -> InvokeOutcome {
            self.rt
                .invoke_action(
                    &me(),
                    (
                        self.trim.action("trim").as_str(),
                        vec![
                            Value::resource(&self.design),
                            Value::resource(&self.desk),
                            Value::text(name),
                        ],
                    ),
                )
                .unwrap()
        }

        fn names(&self) -> Vec<String> {
            let Value::List(parts) = self.rt.read(&self.design, "parts").unwrap() else {
                panic!("parts reads a list");
            };
            parts
                .iter()
                .map(|part| {
                    let Value::Ref(r) = part else {
                        panic!("a part is a reference")
                    };
                    self.rt
                        .read(&r.target, "name")
                        .unwrap()
                        .as_text()
                        .unwrap()
                        .to_owned()
                })
                .collect()
        }

        /// The invocation each change the trim made to the design belongs to.
        fn trim_changes(&self) -> Vec<Option<String>> {
            let trim = self.trim.action("trim");
            self.rt
                .history(&self.design.doc)
                .unwrap()
                .into_iter()
                .filter(|note| note.action == trim)
                .map(|note| note.invocation)
                .collect()
        }
    }

    tilo_test_env::scenario! {
        fn an_action_that_removes_and_then_asks_the_device;

        phase the_answer_goes_on_from_the_ask_and_removes_once;
        {
            let w = trim_world();
            let InvokeOutcome::Waits { run, .. } = w.start("b") else {
                panic!("the device's verdict is asked of a person first");
            };
            assert_eq!(
                w.dispatch.asked().1.get(1),
                Some(&Value::Int(2)),
                "the device is asked about what is left after the removal"
            );
            let out = w.dispatch.allow(&w.rt, &run);
            assert_eq!(out, RunOutcome::Done(Value::text("holds 2")));
            assert_eq!(w.names(), ["a", "c"], "the part went once, and no other with it");
            assert_eq!(
                w.rt.read(&w.design, "verdict").unwrap(),
                Value::text("holds 2"),
                "the rest of the body read the answer the device gave"
            );
            assert_eq!(
                w.rt.read(&w.design, "trims").unwrap(),
                Value::Int(1),
                "the rest of the body ran once"
            );
            assert_eq!(
                w.rt.read(&w.desk, "asked").unwrap(),
                Value::Int(1),
                "the device was asked once"
            );
            let changes = w.trim_changes();
            assert!(!changes.is_empty(), "the trim changed the design");
            assert!(
                changes
                    .iter()
                    .all(|invocation| invocation.is_some() && *invocation == changes[0]),
                "every change of the trim is one invocation, one step to undo: {changes:?}"
            );
            // A second trim of what the first left removes from that, and is its own step.
            let InvokeOutcome::Waits { run: next, .. } = w.start("a") else {
                panic!("the second trim asks too");
            };
            assert_eq!(
                w.dispatch.allow(&w.rt, &next),
                RunOutcome::Done(Value::text("holds 1"))
            );
            assert_eq!(w.names(), ["c"]);
            assert_eq!(w.rt.read(&w.design, "trims").unwrap(), Value::Int(2));
        }

        phase parts_a_filter_and_remove_where_pick_go_once_before_the_ask;
        {
            let w = trim_world();
            let prune = w.trim.action("prune");
            let InvokeOutcome::Waits { run, .. } = w
                .rt
                .invoke_action(
                    &me(),
                    (
                        prune.as_str(),
                        vec![
                            Value::resource(&w.design),
                            Value::resource(&w.desk),
                            Value::text("a"),
                            Value::text("c"),
                        ],
                    ),
                )
                .unwrap()
            else {
                panic!("the device's verdict is asked of a person first");
            };
            assert_eq!(
                w.dispatch.allow(&w.rt, &run),
                RunOutcome::Done(Value::text("holds 1"))
            );
            assert_eq!(w.names(), ["b"], "the picked parts went once, and only they");
            assert_eq!(
                w.rt.read(&w.design, "verdict").unwrap(),
                Value::text("holds 1 after 1")
            );
            assert_eq!(w.rt.read(&w.design, "trims").unwrap(), Value::Int(1));
            assert_eq!(w.rt.read(&w.desk, "asked").unwrap(), Value::Int(1));
        }

        phase a_declined_ask_leaves_the_design_as_it_was;
        {
            let w = trim_world();
            let InvokeOutcome::Waits { run, .. } = w.start("b") else {
                panic!("the device's verdict is asked of a person first");
            };
            let error = decline(&w.rt, &run);
            assert!(
                matches!(error, tilolang_runtime::RuntimeError::Denied(_)),
                "{error}"
            );
            assert_eq!(
                w.names(),
                ["a", "b", "c"],
                "the removal made before the ask does not stand once it is declined"
            );
            assert_eq!(w.rt.read(&w.design, "verdict").unwrap(), Value::text(""));
            assert_eq!(w.rt.read(&w.design, "trims").unwrap(), Value::Int(0));
            assert_eq!(w.rt.read(&w.desk, "asked").unwrap(), Value::Int(0));
            assert!(
                w.trim_changes().is_empty(),
                "nothing of the declined trim is in history"
            );
        }
    }

    /// A package whose action makes a resource and answers it, as the discussion makes a comment.
    const SHELF: &str = r#"app so.tilo.shelf "Shelf"

type Note {
    title: Text
}

type Notes {
    items: Collection<Note>
}

root shelf: Notes

default view notes for Notes

view notes(n: Notes) {
    text("{n.items.len()}")
}

action make(all: Notes, title: Text) -> Note {
    require title != "" "Name the note first"
    all.items.insert(Note(title: title))
}
"#;

    /// A package whose action asks the shelf for a note and goes on with the one it answers.
    const DESK: &str = r#"app so.tilo.desk "Desk"

use so.tilo.shelf as shelf

type Pad {
    last: Text
    said: Text
}

root Pad

default view pad for Pad

view pad(p: Pad) {
    text(p.said)
}

action jot(p: Pad, title: Text) -> shelf.Note {
    let n = shelf.make(shelf.shelf, title).made_from(p)
    p.last = n.id()
    p.said = "kept {n.title}"
    n
}
"#;

    struct Jot {
        rt: Runtime,
        auth: SimpleAuthority,
        desk: App,
        dispatch: Dispatch,
        pad: tilolang_runtime::ResourceRef,
    }

    fn jot_world() -> Jot {
        let shelf = App::compile(SHELF, &[]);
        let desk = App::compile(DESK, &[shelf.meta().clone()]);
        let auth = SimpleAuthority::new();
        let rt = Runtime::new(AmStore::new(), auth.clone()).unwrap();
        shelf.load(&rt);
        desk.load(&rt);
        let dispatch = Dispatch::new();
        rt.set_foreign_invoker(dispatch.clone());
        rt.create_root("Notes").unwrap();
        let pad = rt.create_root("Pad").unwrap();
        Jot {
            rt,
            auth,
            desk,
            dispatch,
            pad,
        }
    }

    impl Jot {
        fn jot(&self, title: &str) -> Result<InvokeOutcome, tilolang_runtime::RuntimeError> {
            self.rt.invoke_action(
                &me(),
                (
                    self.desk.action("jot").as_str(),
                    vec![Value::resource(&self.pad), Value::text(title)],
                ),
            )
        }

        /// The note `answer` is, as the caller went on with it.
        fn kept(&self, answer: &Value) -> tilolang_runtime::ResourceRef {
            let Value::Ref(note) = answer else {
                panic!("the call answers the note it made: {answer:?}");
            };
            assert_eq!(
                self.rt.read(&self.pad, "last").unwrap(),
                Value::text(note.target.id_string()),
                "the caller read the id of the note it was answered"
            );
            tilolang_runtime::ResourceRef::clone(&note.target)
        }
    }

    tilo_test_env::scenario! {
        fn an_action_goes_on_with_the_resource_another_packages_action_answers;

        phase a_call_allowed_at_once_answers_the_note_it_made;
        {
            let w = jot_world();
            w.auth.trust(&Runtime::module_hash(&w.desk.compiled.wasm));
            let Ok(InvokeOutcome::Done(answer)) = w.jot("Groceries") else {
                panic!("a trusted call runs at once");
            };
            let note = w.kept(&answer);
            assert_eq!(w.rt.read(&note, "title").unwrap(), Value::text("Groceries"));
            assert_eq!(
                w.rt.read(&w.pad, "said").unwrap(),
                Value::text("kept Groceries")
            );
        }

        phase a_call_that_asks_first_answers_the_note_when_it_is_allowed;
        {
            let w = jot_world();
            let Ok(InvokeOutcome::Waits { run, .. }) = w.jot("Groceries") else {
                panic!("the other package's action is asked about first");
            };
            let RunOutcome::Done(answer) = w.dispatch.allow(&w.rt, &run) else {
                panic!("the run ends once the call is answered");
            };
            let note = w.kept(&answer);
            assert_eq!(w.rt.read(&note, "title").unwrap(), Value::text("Groceries"));
            assert_eq!(
                w.rt.read(&w.pad, "said").unwrap(),
                Value::text("kept Groceries")
            );
        }

        phase a_refused_call_stops_the_caller_with_why;
        {
            let w = jot_world();
            w.auth.trust(&Runtime::module_hash(&w.desk.compiled.wasm));
            let error = w.jot("").expect_err("the callee refused, so the caller cannot go on");
            assert!(error.to_string().contains("Name the note first"), "{error}");
            assert_eq!(w.rt.read(&w.pad, "said").unwrap(), Value::text(""));
        }

        phase a_declined_call_stops_the_caller;
        {
            let w = jot_world();
            let Ok(InvokeOutcome::Waits { run, .. }) = w.jot("Groceries") else {
                panic!("the other package's action is asked about first");
            };
            let error = decline(&w.rt, &run);
            assert!(
                matches!(error, tilolang_runtime::RuntimeError::Denied(_)),
                "{error}"
            );
            assert_eq!(w.rt.read(&w.pad, "said").unwrap(), Value::text(""));
        }
    }
}
