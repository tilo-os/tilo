//! What an invocation read that a replay cannot compute again: the clock and zone are recorded
//! where the invocation reads them, kept beside an action's change and in a run's journal, so a
//! replay answers from the record instead of the host.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::run::{Clock, JournalEntry};
    use tilolang_runtime::{
        AmStore, InvokeOutcome, Resolution, ResourceRef, RunOutcome, Runtime, SimpleAuthority,
        Store, Value,
    };

    /// Two hours ahead of UTC: their midnight is 22:00 UTC.
    const ZONE: i64 = 2 * 3600;
    const ZONE_NAME: &str = "Europe/Berlin";

    const NOTE: &str = r#"app so.tilo.stamp "Stamp"

type Note {
    text: Text
    at: Time = Time("2026-01-01T00:00:00Z")
}

action stamp(n: Note) uses time {
    n.at = now
}

action retitle(n: Note, text: Text) {
    n.text = text
}
"#;

    const DEMO: &str = r#"app so.tilo.demo "Demo"

type Board {
    marks: Int merge counter
}

action mark(b: Board) {
    b.marks += 1
}
"#;

    /// The runner's action reads the clock, then invokes another package's action, which asks.
    const RUNNER: &str = r#"app so.tilo.runner "Runner"

use so.tilo.demo as demo

action start(b: demo.Board) -> Time uses time {
    let started = now
    demo.mark(b)
    return started
}
"#;

    struct HostClock {
        now: Rc<RefCell<String>>,
    }

    impl HostClock {
        fn new(now: &str) -> Self {
            Self {
                now: Rc::new(RefCell::new(now.to_owned())),
            }
        }

        fn set(&self, now: &str) {
            now.clone_into(&mut self.now.borrow_mut());
        }

        fn install(&self, rt: &Runtime) {
            let reading = Rc::clone(&self.now);
            rt.set_clock(move || reading.borrow().clone());
            rt.set_zone(move || ZONE);
            rt.set_zone_name(move || Some(ZONE_NAME.to_owned()));
        }
    }

    fn runtime(clock: &HostClock) -> (Runtime, AmStore) {
        let store = AmStore::new();
        let rt = Runtime::new(store.clone(), SimpleAuthority::new()).unwrap();
        clock.install(&rt);
        (rt, store)
    }

    #[test]
    fn an_actions_journal_is_kept_beside_its_change() {
        let clock = HostClock::new("2026-06-30T09:00:00Z");
        let (rt, _store) = runtime(&clock);
        let app = App::compile(NOTE, &[]);
        app.load(&rt);
        let note = rt.create("Note").unwrap();
        let stamp = app.action("stamp");
        rt.invoke_action(&me(), (stamp.as_str(), vec![Value::resource(&note)]))
            .unwrap();

        let history = rt.history(&note.doc).unwrap();
        let note_made = history.last().expect("the change it made");
        assert_eq!(note_made.at.as_deref(), Some("2026-06-30T09:00:00Z"));
        assert_eq!(
            note_made.effects,
            [JournalEntry::Clock(Clock {
                now: "2026-06-30T09:00:00Z".to_owned(),
                zone: Some(ZONE_NAME.to_owned()),
                offset: ZONE,
            })],
            "the moment and zone the change was made of are kept with it"
        );

        // An action that reads no clock records none: nothing of its is replayed differently.
        let retitle = app.action("retitle");
        rt.invoke_action(
            &me(),
            (
                retitle.as_str(),
                vec![Value::resource(&note), Value::text("renamed")],
            ),
        )
        .unwrap();
        let history = rt.history(&note.doc).unwrap();
        assert!(history.last().unwrap().effects.is_empty());
    }

    #[test]
    fn a_run_resumed_the_next_day_reads_the_moment_it_started() {
        let clock = HostClock::new("2026-06-30T09:00:00Z");
        let (rt, mut store) = runtime(&clock);
        let demo = App::compile(DEMO, &[]);
        let runner = App::compile(RUNNER, &[demo.meta().clone()]);
        demo.load(&rt);
        runner.load(&rt);
        let board = rt.create("Board").unwrap();

        let out = rt
            .invoke_action(
                &me(),
                (runner.action("start"), vec![Value::resource(&board)]),
            )
            .unwrap();
        let InvokeOutcome::Suggested(first) = out else {
            panic!("the other package's action must ask: {out:?}");
        };

        // The moment the run started is in its journal before it goes on.
        let journal = store
            .load_journal(&first.run)
            .unwrap()
            .expect("a suspended run's journal");
        assert_eq!(
            journal.entries,
            [JournalEntry::Clock(Clock {
                now: "2026-06-30T09:00:00Z".to_owned(),
                zone: Some(ZONE_NAME.to_owned()),
                offset: ZONE,
            })]
        );

        // Tomorrow, the run resumed reads the moment it started, not the host's.
        clock.set("2026-07-01T15:30:00Z");
        let out = rt.resolve(&first.run, &me(), Resolution::Approve).unwrap();
        assert_eq!(
            out,
            RunOutcome::Done(Value::text("2026-06-30T09:00:00Z")),
            "a resumed run sees the clock it first read"
        );
        assert_eq!(marks(&rt, &board), 1, "the step it approved ran once");
    }

    /// The writer writes its note, then invokes another package's action, which asks.
    const WRITER: &str = r#"app so.tilo.writer "Writer"

use so.tilo.demo as demo

type Doc {
    text: Text
}

action begin(d: Doc, b: demo.Board) {
    d.text = "opened"
    demo.mark(b)
    d.text = "done"
}
"#;

    #[test]
    fn a_call_waited_on_keeps_what_the_caller_wrote_and_what_the_call_wrote() {
        let clock = HostClock::new("2026-06-30T09:00:00Z");
        let (rt, mut store) = runtime(&clock);
        let demo = App::compile(DEMO, &[]);
        let writer = App::compile(WRITER, &[demo.meta().clone()]);
        demo.load(&rt);
        writer.load(&rt);
        let doc = rt.create("Doc").unwrap();
        let board = rt.create("Board").unwrap();
        let (doc_changes, board_changes) = (
            rt.history(&doc.doc).unwrap().len(),
            rt.history(&board.doc).unwrap().len(),
        );

        let out = rt
            .invoke_action(
                &me(),
                (
                    writer.action("begin"),
                    vec![Value::resource(&doc), Value::resource(&board)],
                ),
            )
            .unwrap();
        let InvokeOutcome::Suggested(first) = out else {
            panic!("the other package's action must ask: {out:?}");
        };
        // What it wrote before the call is a change of its own, made before the call runs, so
        // the call works on the record: the first word is there, the call has not run.
        assert_eq!(text(&rt, &doc), "opened");
        assert_eq!(marks(&rt, &board), 0, "the call waits on the person");
        let journal = store
            .load_journal(&first.run)
            .unwrap()
            .expect("a suspended run's journal");
        assert_eq!(
            journal.entries,
            [JournalEntry::Wrote { item: None }],
            "the run wrote once, and its journal says so"
        );

        let out = rt.resolve(&first.run, &me(), Resolution::Approve).unwrap();
        assert_eq!(out, RunOutcome::Done(Value::Unit));
        // The call's change stands, and the caller's replay did not write the first word again
        // or take the call back: one change each, not two of the first.
        assert_eq!(marks(&rt, &board), 1, "the call ran once");
        assert_eq!(text(&rt, &doc), "done");
        assert_eq!(
            rt.history(&board.doc).unwrap().len(),
            board_changes + 1,
            "the call is one change, made once"
        );
        assert_eq!(
            rt.history(&doc.doc).unwrap().len(),
            doc_changes + 2,
            "what it wrote before the call and after it are a change each"
        );
    }

    fn text(rt: &Runtime, note: &ResourceRef) -> String {
        match rt.read(note, "text").unwrap() {
            Value::Text(t) => t,
            other => panic!("text is {other:?}"),
        }
    }

    fn marks(rt: &Runtime, board: &ResourceRef) -> i64 {
        match rt.read(board, "marks").unwrap() {
            Value::Int(n) => n,
            other => panic!("marks is {other:?}"),
        }
    }
}
