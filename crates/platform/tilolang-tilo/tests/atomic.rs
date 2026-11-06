//! One action and every action it calls are one action of the workspace: every change of the
//! run names the one invocation, and a run that fails anywhere leaves nothing of itself in the
//! record or in what the host would commit.

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use tilo_actions::ActionContext;
use tilolang_runtime::meta::{FieldKind, Meta};
use tilolang_runtime::store::{FieldSpec, FieldValue, Store as _};
use tilolang_runtime::{Principal, Value};
use tilolang_tilo::action::{DynAction, invocation_id};
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

/// A host over a fresh record, running `NESTED`, and the tally it counts.
fn workspace() -> (Host, AutoCommit, tilolang_tilo::runtime::ResourceRef, Meta) {
    let mut doc = AutoCommit::new();
    let host = Host::new(&doc, Principal::new("ada"), || None).unwrap();
    let package = host.load_builtin(NESTED, "").unwrap();
    let tally = host.root_of(APP).unwrap();
    // The root itself is a change of the record, committed before any action runs.
    for created in host.take_committed() {
        apply(&host, &mut doc, &created.action);
    }
    (host, doc, tally, package.meta)
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
        &Principal::new("ada"),
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

/// Apply `action` to `doc` as the record does, in the run it was committed in
/// ([`tilolang_tilo::instance::record::InstanceRecord`]): its context names the run's
/// invocation.
fn apply(host: &Host, doc: &mut AutoCommit, action: &DynAction) {
    let mut context = ActionContext::for_resource(
        action.resources().first().copied().unwrap_or("workspace"),
        &action.kind,
        &action.kind,
    );
    if let Some(run) = &action.authorship.invocation {
        context = context.with_source_invocation(Some(invocation_id(run)));
    }
    tilo_actions::apply_local_with_context(
        doc,
        action,
        action.encode().unwrap(),
        Some(b"ada"),
        context,
    )
    .unwrap();
    doc.commit();
    host.sync_from(doc);
}

#[test]
fn an_action_and_the_actions_it_calls_are_one_commit() {
    let (host, mut doc, tally, meta) = workspace();
    assert!(run(&host, &meta, "twice", &tally));
    let committed = host.take_committed();
    assert_eq!(committed.len(), 2, "a change per call");
    let runs: Vec<Option<String>> = committed
        .iter()
        .map(|committed| committed.action.authorship.invocation.clone())
        .collect();
    assert_eq!(runs[0], runs[1], "both calls ran in the one action");
    let run = runs[0].clone().expect("a run names its invocation");

    let before = doc.get_heads();
    for committed in &committed {
        apply(&host, &mut doc, &committed.action);
    }
    let change = DocumentMutation::Incremental(doc.save_after(&before));
    let commit = collab_types::WorkspaceCommit::of_mutation(
        "workspace",
        APP,
        "tally",
        &collab_types::CommitAuthor {
            principal: Some("ada".to_owned()),
            device: [1; 32],
        },
        &change,
    )
    .unwrap()
    .expect("the calls are committed as one action");
    assert_eq!(commit.parts.len(), 1, "one record, one part");
    assert_eq!(commit.parts[0].changes.len(), 2, "both calls are in it");
    assert_eq!(
        commit.invocation,
        Some(invocation_id(&run).0),
        "the commit names the run the changes ran in"
    );
}

#[test]
fn a_run_that_fails_after_its_calls_leaves_nothing() {
    let (host, mut doc, tally, meta) = workspace();
    let before = doc.get_heads();
    assert!(!run(&host, &meta, "boom", &tally), "the action failed");
    assert!(
        host.take_committed().is_empty(),
        "nothing of a run that failed anywhere is queued to commit"
    );
    assert_eq!(
        doc.get_heads(),
        before,
        "and nothing of it is in the record"
    );
    assert_eq!(value(&host, &tally, &meta), 0);
}
