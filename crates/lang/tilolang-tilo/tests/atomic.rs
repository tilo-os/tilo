//! One action and every action it calls are one run of the workspace: every change of the run
//! names the one invocation, and a run that fails anywhere leaves nothing of itself in the
//! store.

use tilolang_runtime::meta::{FieldKind, Meta};
use tilolang_runtime::store::{FieldSpec, FieldValue, Store as _};
use tilolang_runtime::{Actor, Value};
use tilolang_tilo::host::Host;

/// Two actions that call `bump` twice, one of them failing after the calls.
const NESTED: &str = r#"app test.nested "Nested"

type Tally {
  value: Int merge counter
}

root Tally

default view tally for Tally

view tally(t: Tally) {
  std.page(t) {
    ui.Text("{t.value}")
  }
}

action bump(t: Tally) {
  t.value += 1
}

action twice(t: Tally) {
  bump(t)
  bump(t)
}

action boom(t: Tally) {
  bump(t)
  bump(t)
  t.value += 1 / (t.value - t.value)
}
"#;

const APP: &str = "test.nested";

/// A host over a store of its own, running `NESTED`, and the tally it counts.
fn workspace() -> (Host, tilolang_tilo::runtime::ResourceRef, Meta) {
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .unwrap();
    let package = host.load_builtin(NESTED, "").unwrap();
    let tally = host.root_of(APP).unwrap();
    (host, tally, package.meta)
}

fn run(
    host: &Host,
    meta: &Meta,
    action: &str,
    tally: &tilolang_tilo::runtime::ResourceRef,
) -> bool {
    let id = meta
        .actions
        .iter()
        .find(|declared| declared.name == action)
        .unwrap()
        .id
        .clone();
    match host.runtime().invoke_action(
        &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        (id.as_str(), vec![Value::resource(tally)]),
    ) {
        Ok(_) => true,
        Err(error) => {
            eprintln!("run {action} failed: {error:?}");
            false
        }
    }
}

fn value(host: &Host, tally: &tilolang_tilo::runtime::ResourceRef, meta: &Meta) -> i64 {
    let spec = FieldSpec {
        id: super::fixtures::field_id(meta, "Tally", "value"),
        kind: FieldKind::Counter,
    };
    match host.store().read(tally, &spec).unwrap() {
        FieldValue::Counter(value) => value,
        other => panic!("the tally's value is {other:?}"),
    }
}

tilo_test_env::scenario! {
    fn a_run_and_its_calls_commit_together_or_not_at_all;

    phase an_action_and_the_actions_it_calls_are_one_run;
    {
        let (host, tally, meta) = workspace();
        assert!(run(&host, &meta, "twice", &tally));
        // A change per call, each naming the one invocation the run is, and the root's making.
        let versions = host.store().versions(&tally).unwrap();
        let calls: Vec<_> = versions
            .iter()
            .filter(|version| version.action != tilolang_runtime::store::CREATE_ACTION)
            .collect();
        assert_eq!(calls.len(), 2, "a change per call: {versions:#?}");
        let runs: Vec<Option<String>> = calls
            .iter()
            .map(|version| version.gesture.clone())
            .collect();
        assert_eq!(runs[0], runs[1], "both calls ran in the one action");
        assert!(runs[0].is_some(), "the run names its invocation");
        assert_eq!(value(&host, &tally, &meta), 2, "both calls counted");
    }

    phase a_run_that_fails_after_its_calls_leaves_nothing;
    {
        let (host, tally, meta) = workspace();
        let before = host.store().versions(&tally).unwrap();
        assert!(!run(&host, &meta, "boom", &tally), "the action failed");
        assert_eq!(
            host.store().versions(&tally).unwrap(),
            before,
            "nothing of a run that failed anywhere is in the store"
        );
        assert_eq!(value(&host, &tally, &meta), 0);
        // A call that ended while a run waited on it is a change of its own: it outlives the run.
        let mut store = host.store().clone();
        store.begin_run("waits on a call");
        store.begin_run("the call");
        assert!(run(&host, &meta, "bump", &tally));
        store.end_run("the call").unwrap();
        store.abort_run("waits on a call");
        assert_eq!(value(&host, &tally, &meta), 1, "the call's own change stands");
    }
}
