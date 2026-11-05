//! What links to a resource: what was made from it, what mentions it and what names it in a
//! field, read from the index of every app of the workspace, and what a resource was made from.

use automerge::AutoCommit;
use serde_json::json;
use tilo_resource::{Collection, Id, Model as _};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{InvokeOutcome, Principal, Value, ViewInstance};
use tilolang_tilo::action::{DynAction, DynEdit};
use tilolang_tilo::host::Package;
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::PlanRequest;
use tilolang_tilo::record::{DynRecord, DynValue};
use tilolang_tilo::{Authorship, schema};

use crate::fixtures::{Replica, field_id};

const LINKS: &str = include_str!("../../../lang/tilolang/tests/fixtures/links.tl");
const APP: &str = "so.tilo.links";

fn call(replica: &Replica, action: &str, args: Vec<Value>) -> Value {
    let package = replica.host.package(APP).unwrap();
    let id = package
        .meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap()
        .id
        .clone();
    match replica
        .host
        .runtime()
        .invoke_action(&Principal::new(&replica.name), (id.as_str(), args))
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

fn text(v: Value) -> String {
    match v {
        Value::Text(t) => t,
        other => panic!("not text: {other:?}"),
    }
}

fn lines(v: Value) -> Vec<String> {
    match v {
        Value::List(items) => items.into_iter().map(text).collect(),
        other => panic!("not a list: {other:?}"),
    }
}

fn notes(replica: &Replica) -> Vec<ResourceRef> {
    match replica
        .host
        .runtime()
        .read(&root(replica), "notes")
        .unwrap()
    {
        Value::List(items) => items
            .into_iter()
            .map(|item| match item {
                Value::Ref(r) => std::rc::Rc::unwrap_or_clone(r.target),
                other => panic!("not a resource: {other:?}"),
            })
            .collect(),
        other => panic!("not a list: {other:?}"),
    }
}

fn root(replica: &Replica) -> ResourceRef {
    replica.host.root_of(APP).unwrap()
}

/// `action` as the door plans it, made from `from` when one is given.
fn planned(
    replica: &Replica,
    action: &str,
    args: serde_json::Value,
    from: Option<&ResourceRef>,
) -> DynAction {
    let package = replica.host.package(APP).unwrap();
    let meta = &package.meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap()
        .id
        .clone();
    let root = root(replica).id_string();
    replica
        .host
        .plan(&PlanRequest {
            app: APP,
            kind: &meta.kind_of("Board"),
            action_id: &id,
            resource: Some(&root),
            args: args.as_object().unwrap(),
            caller: None,
            from,
        })
        .unwrap()
}

fn board() -> (Replica, Vec<Package>) {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(LINKS, "")]);
    // The root is made on its first reading, as a window opens its app.
    let _ = root(&ada);
    ada.commit();
    (ada, packages)
}

/// Adds a note as the door does, from `from` when one is given, and answers its reference.
fn add_note(replica: &mut Replica, title: &str, from: Option<&ResourceRef>) -> ResourceRef {
    let action = planned(replica, "add_note", json!({"title": title}), from);
    replica.apply(&action);
    replica.commit();
    notes(replica).pop().unwrap()
}

#[test]
fn a_note_made_from_another_says_so() {
    let (mut ada, loaded) = board();
    let source = add_note(&mut ada, "A", None);
    let made = add_note(&mut ada, "B", Some(&source));
    assert_eq!(
        text(call(&ada, "made_from", vec![Value::resource(&made)])),
        "A"
    );
    assert_eq!(
        text(call(&ada, "made_from", vec![Value::resource(&source)])),
        ""
    );
    // It is a fact of the change, so another replica reads it from the record.
    let ben = ada.fork("ben", &[(LINKS, &loaded[0].ids)]);
    ben.host.sync_from(&ben.doc);
    assert_eq!(
        text(call(&ben, "made_from", vec![Value::resource(&made)])),
        "A"
    );
}

#[test]
fn what_links_to_a_note_is_read_from_the_index() {
    let (mut ada, _) = board();
    let source = add_note(&mut ada, "A", None);
    let made = add_note(&mut ada, "B", Some(&source));
    // An item whose field names the note, and a note whose text mentions it.
    let action = planned(
        &ada,
        "item_about",
        json!({"n": source.id_string(), "title": "I"}),
        None,
    );
    ada.apply(&action);
    ada.commit();
    let action = planned(
        &ada,
        "note_about",
        json!({"n": source.id_string(), "title": "M"}),
        None,
    );
    ada.apply(&action);
    ada.commit();
    assert_eq!(
        lines(call(&ada, "links_of", vec![Value::resource(&source)])),
        [
            "created_from B".to_owned(),
            "field I".to_owned(),
            "mention M".to_owned(),
        ]
    );
    // What links to it links to nothing itself.
    assert!(lines(call(&ada, "links_of", vec![Value::resource(&made)])).is_empty());
    // A view of what links to it shows the same, and runs again when one more is made from it.
    let view = ada
        .host
        .runtime()
        .open_view(
            &Principal::new("ada"),
            &ada.host.package(APP).unwrap().view_id("links").unwrap(),
            vec![Value::resource(&source)],
        )
        .unwrap();
    let shown = |view: &ViewInstance| format!("{:?}", view.tree());
    assert!(shown(&view).contains("created_from B"), "{}", shown(&view));
    let mut view = view;
    add_note(&mut ada, "C", Some(&source));
    assert!(
        !view.poll_patches().unwrap().is_empty(),
        "the view runs again when what links to the note changes"
    );
    assert!(shown(&view).contains("created_from C"), "{}", shown(&view));
}

/// A board of `n` notes, made whole in one change as a workspace is seeded.
fn seeded(n: usize) -> (Replica, Vec<Package>) {
    let (mut ada, packages) = board();
    let meta = packages[0].meta.clone();
    let board = root(&ada);
    let board_ty = meta.types.iter().find(|t| t.name == "Board").unwrap();
    let note_ty = meta.types.iter().find(|t| t.name == "Note").unwrap();
    let note_schema = schema::lookup(&note_ty.id).unwrap();
    let mut notes = Collection::default();
    for i in 0..n {
        let mut item = DynRecord::empty(note_schema).unwrap();
        if let Some(DynValue::Text(title)) = item.get_mut(&field_id(&meta, "Note", "title")) {
            *title = format!("Note {i:05}");
        }
        notes.push(Id::from_raw(format!("note-{i:05}")), item);
    }
    let mut record = DynRecord::empty(schema::lookup(&board_ty.id).unwrap()).unwrap();
    if let Some(DynValue::Collection(slot)) = record.get_mut(&field_id(&meta, "Board", "notes")) {
        *slot = notes;
    }
    let create = DynAction {
        app: APP.to_owned(),
        kind: meta.kind_of("Board").to_owned(),
        action_id: "$create".to_owned(),
        args: serde_json::Value::Null,
        authorship: Authorship::default(),
        edits: vec![DynEdit::Create {
            id: board.id_string(),
            record: record.to_json(),
        }],
    };
    ada.apply(&create);
    ada.commit();
    (ada, packages)
}

/// The index and item reads `action` does at the size the workspace is.
fn reads_of(ada: &Replica, action: &str, args: Vec<Value>) -> (u64, u64) {
    let _ = call(ada, action, args.clone());
    let before = ada.host.store().read_stats();
    let _ = call(ada, action, args);
    let after = ada.host.store().read_stats();
    (
        after.index.dirs - before.index.dirs + after.index.pages - before.index.pages
            + after.index.fields
            - before.index.fields,
        after.evaluated - before.evaluated,
    )
}

#[test]
fn what_links_to_a_note_is_read_at_the_same_cost_at_any_size() {
    let work = |n: usize| {
        let (mut ada, _) = seeded(n);
        let source = add_note(&mut ada, "A", None);
        add_note(&mut ada, "B", Some(&source));
        let action = planned(
            &ada,
            "note_about",
            json!({"n": source.id_string(), "title": "M"}),
            None,
        );
        ada.apply(&action);
        ada.commit();
        reads_of(&ada, "links_of", vec![Value::resource(&source)])
    };
    let small = work(1_000);
    let large = work(10_000);
    assert_eq!(
        small, large,
        "what links to a note is read from its own list, not the workspace: {small:?} then {large:?}"
    );
    assert!(small.0 > 0, "the read comes from the index: {small:?}");
}
