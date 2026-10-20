//! Flows: an Ask suspends the run, resolving replays it from the journal, and the journaled
//! results are not executed again.

mod common;

use std::cell::Cell;
use std::rc::Rc;

use common::*;
use tilolang_runtime::{FlowOutcome, ModelHost, Resolution, Runtime, Store, Value};

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

#[test]
fn a_flow_invoking_its_own_modules_actions_runs_straight_through() {
    let w = World::new();
    let c = w.counter();
    let out =
        w.rt.run_flow(&w.me, "twice", vec![Value::resource(&c)])
            .unwrap();
    assert_eq!(out, FlowOutcome::Done(Value::Int(2)));
    assert_eq!(incs(&w, &c), 2);
}

#[test]
fn an_ask_suspends_and_resolving_replays_without_repeating_work() {
    let (w, c) = world();
    let args = vec![Value::resource(&c)];

    let FlowOutcome::Suspended(first) = w.rt.run_flow(&w.me, "ask", args).unwrap() else {
        panic!("the first foreign invoke must ask");
    };
    assert_eq!(first.action, "inc");
    assert_eq!(incs(&w, &c), 0);

    let FlowOutcome::Suspended(second) =
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
    assert_eq!(out, FlowOutcome::Done(Value::Int(2)));
    assert_eq!(incs(&w, &c), 2);
}

#[test]
fn a_declined_step_is_journaled_and_the_flow_goes_on() {
    let (w, c) = world();
    let FlowOutcome::Suspended(first) =
        w.rt.run_flow(&w.me, "ask", vec![Value::resource(&c)])
            .unwrap()
    else {
        panic!("expected a suggestion");
    };
    let FlowOutcome::Suspended(second) =
        w.rt.resolve(&first.run, &w.me, Resolution::Decline)
            .unwrap()
    else {
        panic!("the flow continues to its second invoke");
    };
    let out =
        w.rt.resolve(&second.run, &w.me, Resolution::Approve)
            .unwrap();
    assert_eq!(out, FlowOutcome::Done(Value::Int(1)));
    assert_eq!(incs(&w, &c), 1);
}

#[test]
fn trusting_the_author_lets_the_flow_run_without_asking() {
    let (w, c) = world();
    let stranger = Runtime::module_hash(&stranger_wasm());
    w.auth.trust(&stranger);
    let out =
        w.rt.run_flow(&w.me, "ask", vec![Value::resource(&c)])
            .unwrap();
    assert_eq!(out, FlowOutcome::Done(Value::Int(2)));
}

#[test]
fn a_suspended_run_survives_a_restart() {
    let (w, c) = world();
    let FlowOutcome::Suspended(first) =
        w.rt.run_flow(&w.me, "ask", vec![Value::resource(&c)])
            .unwrap()
    else {
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
    let FlowOutcome::Suspended(second) = rt.resolve(&first.run, &me, Resolution::Approve).unwrap()
    else {
        panic!("expected the second suggestion");
    };
    let out = rt.resolve(&second.run, &me, Resolution::Approve).unwrap();
    assert_eq!(out, FlowOutcome::Done(Value::Int(2)));
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
    let FlowOutcome::Suspended(s) =
        w.rt.run_flow(&w.me, "m", vec![Value::resource(&c)])
            .unwrap()
    else {
        panic!("expected a suggestion");
    };
    assert_eq!(calls.get(), 1);
    let out = w.rt.resolve(&s.run, &w.me, Resolution::Approve).unwrap();
    assert_eq!(out, FlowOutcome::Done(Value::text("classify:hello")));
    assert_eq!(calls.get(), 1, "the replay used the journaled answer");
}

#[test]
fn the_journal_is_persisted_through_the_store() {
    let (w, c) = world();
    let FlowOutcome::Suspended(s) =
        w.rt.run_flow(&w.me, "ask", vec![Value::resource(&c)])
            .unwrap()
    else {
        panic!("expected a suggestion");
    };
    let mut store = w.store.clone();
    let journal = store.load_journal(&s.run).unwrap().unwrap();
    assert_eq!(journal.flow.as_deref(), Some("ask"));
    assert!(journal.entries.is_empty());
    assert_eq!(journal.pending.unwrap().action, "inc");
}
