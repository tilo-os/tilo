//! What an origin derives is never saved to the workspace record: the clock, the desk, the device's
//! state are written into the store's unsaved layer, read at once, and gone at the next start,
//! when the origin writes them again. A change of one reaches the views and leaves no change
//! behind to be committed, sent or replayed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

use automerge::AutoCommit;
use tilo_actions::ActionInvoker;
use tilolang_runtime::Principal;
use tilolang_runtime::origin::OriginWrite;
use tilolang_runtime::store::Store as _;
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_tilo::action::DynAction;
use tilolang_tilo::host::Host;
use tilolang_tilo::record::DynValue;

/// A package whose root holds a kind its origin derives (`session.desk`).
const SHELL: &str = r#"app so.tilo.test.shell "Shell"

type Desk (origin: session.desk) {
  clock: Text merge register
  workspace: Text merge register
  workspaces: Collection<Workspace>
}

type Workspace (origin: session.workspace) {
  key: Text merge register
  name: Text merge register
}

root Desk

default view bar for Desk

view bar(d: Desk) {
  row {
    text(d.clock)
    text(d.workspace)
  }
}
"#;

const SHELL_IDS: &str = "";

/// A host over a fresh record, with the shell package loaded (its ids made fresh).
fn host() -> (Host, AutoCommit) {
    let doc = AutoCommit::new();
    let host = Host::new(&doc, Principal::new("ada"), || None).unwrap();
    host.load_builtin(SHELL, SHELL_IDS).unwrap();
    (host, doc)
}

/// The heads of `doc`, as text: equal heads say nothing was written to it.
fn heads(doc: &mut AutoCommit) -> String {
    doc.get_heads()
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Commit through the executor every action the host queued, as the door does for a window.
fn commit(host: &Host, doc: &mut AutoCommit) {
    for committed in host.take_committed() {
        apply(host, doc, &committed.action);
    }
}

fn apply(_host: &Host, doc: &mut AutoCommit, action: &DynAction) {
    let context = tilo_actions::ActionContext::for_resource(
        // The workspace root's path, as the record service names it for an action that wrote
        // no resource.
        action.resources().first().copied().unwrap_or("room"),
        &action.kind,
        &action.kind,
    )
    .with_app("so.tilo.lang", "tilolang")
    .with_invoker(ActionInvoker::actor("ada"));
    tilo_actions::apply_local_with_context(
        doc,
        action,
        action.encode().unwrap(),
        Some(b"ada"),
        context,
    )
    .unwrap();
    doc.commit();
}

/// The field id of `name` in the shell's `Desk` type.
fn desk_field(host: &Host, name: &str) -> String {
    let types = tilolang_tilo::origin::origin_types(
        &host.package("so.tilo.test.shell").unwrap().meta,
        "session",
    );
    types
        .iter()
        .find(|t| t.kind == "session.desk")
        .unwrap()
        .fields
        .iter()
        .find(|(n, _)| n.as_str() == name)
        .unwrap()
        .1
        .clone()
}

/// Write the desk's clock through its origin, as the session does each minute.
fn write_clock(host: &Host, desk: &ResourceRef, clock: &str) {
    host.runtime()
        .apply_origin(
            "session",
            vec![OriginWrite::Set {
                target: desk.clone(),
                fields: vec![(desk_field(host, "clock"), Value::Text(clock.to_owned()))],
            }],
            Some("2026-10-31T12:00:00Z".to_owned()),
        )
        .unwrap();
}

fn clock_of(host: &Host, desk: &ResourceRef) -> Option<DynValue> {
    host.store()
        .record(&desk.doc)
        .unwrap()
        .get(&desk_field(host, "clock"))
        .cloned()
}

/// The type id of `name` in the shell package.
fn type_of(host: &Host, name: &str) -> String {
    host.package("so.tilo.test.shell")
        .unwrap()
        .meta
        .types
        .iter()
        .find(|ty| ty.name == name)
        .unwrap()
        .id
        .clone()
}

#[test]
fn a_record_a_write_made_answers_a_field_the_write_did_not_write() {
    let (host, mut doc) = host();
    let desk = host.root_of("so.tilo.test.shell").unwrap();
    commit(&host, &mut doc);
    // The session's sync makes a workspace in the desk's collection, naming its key alone.
    let made = host
        .runtime()
        .apply_origin(
            "session",
            vec![OriginWrite::Insert {
                owner: desk,
                field: desk_field(&host, "workspaces"),
                ty: type_of(&host, "Workspace"),
                fields: vec![("key".to_owned(), Value::Text("one".to_owned()))],
            }],
            None,
        )
        .unwrap();
    let workspace = made
        .into_iter()
        .flatten()
        .next()
        .expect("the sync made the workspace");
    // A field the write did not write is the record's own: the index names the record at its
    // next derivation, and until then the record answers.
    assert_eq!(
        host.runtime().read(&workspace, "name").unwrap(),
        Value::Text(String::new()),
        "a field the write did not write reads as the record holds it"
    );
    assert_eq!(
        host.runtime().read(&workspace, "key").unwrap(),
        Value::Text("one".to_owned()),
        "and a field it did write reads as it wrote it"
    );
}

#[test]
fn a_record_a_write_made_has_its_type_before_the_index_names_it() {
    let (host, mut doc) = host();
    let desk = host.root_of("so.tilo.test.shell").unwrap();
    commit(&host, &mut doc);
    let made = host
        .runtime()
        .apply_origin(
            "session",
            vec![OriginWrite::Insert {
                owner: desk.clone(),
                field: desk_field(&host, "workspaces"),
                ty: type_of(&host, "Workspace"),
                fields: vec![("key".to_owned(), Value::Text("one".to_owned()))],
            }],
            None,
        )
        .unwrap();
    let workspace = made
        .into_iter()
        .flatten()
        .next()
        .expect("the sync made the workspace");
    // A window opens over the record the write made, before the index names it: the writes name
    // its type, and the record answers.
    assert_eq!(
        host.store().type_of(&workspace).unwrap(),
        type_of(&host, "Workspace"),
        "a record a write just made opens at once"
    );
    // ... and once the writes take it away, neither the record nor its window has a type for it.
    host.runtime()
        .apply_origin(
            "session",
            vec![OriginWrite::Remove {
                owner: desk,
                field: desk_field(&host, "workspaces"),
                item: workspace.clone(),
            }],
            None,
        )
        .unwrap();
    assert!(
        host.store().type_of(&workspace).is_err(),
        "the workspace is gone from the sync: its window says so"
    );
}

#[test]
fn a_root_a_write_just_made_has_its_type_before_the_index_names_it() {
    let (host, _doc) = host();
    // The session derives the shell's desk: a root made in the unsaved layer, before the index
    // names it, and the window over it opens at once.
    let desk = host.root_of("so.tilo.test.shell").unwrap();
    let at = ResourceRef::root(desk.doc, String::new());
    assert_eq!(
        host.store().type_of(&at).unwrap(),
        type_of(&host, "Desk"),
        "a window opens over the root a write just made"
    );
}

#[test]
fn a_session_origin_update_writes_nothing_to_the_record() {
    let (host, mut doc) = host();
    // The desk is a root of a derived kind: made in the unsaved layer, so making it writes
    // nothing to the record either.
    let before_root = heads(&mut doc);
    let desk = host.root_of("so.tilo.test.shell").unwrap();
    commit(&host, &mut doc);
    assert_eq!(
        heads(&mut doc),
        before_root,
        "the derived root is made in the unsaved layer: the record is untouched"
    );

    // A minute turns: the origin writes the clock.
    let before = heads(&mut doc);
    write_clock(&host, &desk, "12:01");
    commit(&host, &mut doc);
    assert_eq!(
        heads(&mut doc),
        before,
        "a derived update is not committed: the record's heads do not move"
    );
    // ... and the shell reads what the origin wrote, at once.
    assert_eq!(
        clock_of(&host, &desk),
        Some(DynValue::Register(serde_json::json!("12:01"))),
        "the view reads the update from the unsaved layer"
    );
}

#[test]
fn a_derived_update_survives_the_record_moving_on() {
    let (host, mut doc) = host();
    let desk = host.root_of("so.tilo.test.shell").unwrap();
    commit(&host, &mut doc);
    write_clock(&host, &desk, "12:01");
    commit(&host, &mut doc);
    assert_eq!(
        clock_of(&host, &desk),
        Some(DynValue::Register(serde_json::json!("12:01")))
    );
    // The record moves on (a pass, another device): the derived write stays in the unsaved
    // layer, where it was, and is not re-derived or re-applied through the record.
    let mut record = doc.fork();
    automerge::transaction::Transactable::put(&mut record, automerge::ROOT, "a-pass", "kept")
        .unwrap();
    record.commit();
    host.sync_from(&record);
    assert_eq!(
        clock_of(&host, &desk),
        Some(DynValue::Register(serde_json::json!("12:01"))),
        "the record moving on leaves the derived write where it was"
    );
}
