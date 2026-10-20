use automerge::AutoCommit;
use serde_json::json;
use tilo_actions::{Action, ActionRegistry};
use tilo_automerge::ApplyIncoming as _;
use tilo_resource::{Edit, EditOp, Id, Model, Segment};
use tilolang_tilo::action::{Authorship, DynAction, DynEdit, read_resource, register_actions};
use tilolang_tilo::record::{DynRecord, DynValue};
use tilolang_tilo::schema::lookup;

use crate::fixtures::{COUNTER, Replica, TODO, field_id};

fn action(app: &str, id: &str, edits: Vec<DynEdit>) -> DynAction {
    DynAction {
        app: app.to_owned(),
        kind: "tl.test".to_owned(),
        action_id: id.to_owned(),
        args: json!({}),
        authorship: Authorship::default(),
        edits,
    }
}

fn field(id: &str) -> Vec<Segment> {
    vec![Segment::Field(id.to_owned())]
}

/// A host with both packages loaded, so their schemas are registered.
fn loaded() -> (tilolang_runtime::Meta, tilolang_runtime::Meta) {
    let (replica, packages) = Replica::new("ada", AutoCommit::new(), &[(COUNTER, ""), (TODO, "")]);
    drop(replica);
    (packages[0].meta.clone(), packages[1].meta.clone())
}

fn create(doc: &mut AutoCommit, app: &str, ty: &str, id: &str) {
    let record = DynRecord::empty(lookup(ty).unwrap()).unwrap();
    action(
        app,
        "create",
        vec![DynEdit::Create {
            id: id.to_owned(),
            record: record.to_json(),
        }],
    )
    .apply(doc)
    .unwrap();
    doc.commit();
}

#[test]
fn a_record_written_and_read_back_is_the_same_record() {
    let (counter, _) = loaded();
    let ty = &counter.types[0].id;
    let mut doc = AutoCommit::new();
    create(&mut doc, &counter.app, ty, "c1");
    let record = read_resource(&doc, &counter.app, "c1").unwrap();
    assert_eq!(record.schema().name, "Counter");
    let again = DynRecord::from_json(&record.to_json()).unwrap();
    assert_eq!(again, record);
}

#[test]
fn text_collections_and_sets_edit_by_their_merge_kind() {
    let (_, todo) = loaded();
    let list = todo.types.iter().find(|t| t.name == "TodoList").unwrap();
    let item = todo.types.iter().find(|t| t.name == "Item").unwrap();
    let (title, items, tags) = (
        field_id(&todo, "TodoList", "title"),
        field_id(&todo, "TodoList", "items"),
        field_id(&todo, "TodoList", "tags"),
    );
    let (text, done) = (
        field_id(&todo, "Item", "text"),
        field_id(&todo, "Item", "done"),
    );
    let mut doc = AutoCommit::new();
    create(&mut doc, &todo.app, &list.id, "t1");
    let item_json = |label: &str| json!({"$type": item.id, "$app": todo.app, text.clone(): label, done.clone(): false});
    let edit = |path: Vec<Segment>, op: EditOp| DynEdit::Edit {
        id: "t1".into(),
        edit: Edit::new(path, op),
    };
    action(
        &todo.app,
        "x",
        vec![
            edit(
                field(&title),
                EditOp::Replace {
                    text: "Shopping".into(),
                },
            ),
            edit(
                field(&items),
                EditOp::InsertItem {
                    id: Some(Id::from_raw("i1")),
                    at: 0,
                    value: item_json("milk"),
                },
            ),
            edit(
                field(&tags),
                EditOp::Insert {
                    value: json!("home"),
                },
            ),
        ],
    )
    .apply(&mut doc)
    .unwrap();
    doc.commit();
    let read = read_resource(&doc, &todo.app, "t1").unwrap();
    assert_eq!(read.get(&title), Some(&DynValue::Text("Shopping".into())));
    let Some(DynValue::Collection(collection)) = read.get(&items) else {
        panic!("no items");
    };
    assert_eq!(collection.len(), 1);
    let Some(DynValue::Set(set)) = read.get(&tags) else {
        panic!("no tags");
    };
    assert_eq!(set.len(), 1);

    // An insert and a concurrent insert elsewhere in the same collection merge.
    let mut other = doc.fork();
    let insert = |label: &str, id: &str, at: usize| {
        action(
            &todo.app,
            "x",
            vec![edit(
                field(&items),
                EditOp::InsertItem {
                    id: Some(Id::from_raw(id)),
                    at,
                    value: item_json(label),
                },
            )],
        )
    };
    insert("eggs", "i2", 1).apply(&mut doc).unwrap();
    doc.commit();
    insert("bread", "i3", 0).apply(&mut other).unwrap();
    other.commit();
    doc.merge_incoming(&mut other).unwrap();
    other.merge_incoming(&mut doc).unwrap();
    let merged = read_resource(&doc, &todo.app, "t1").unwrap();
    assert_eq!(merged, read_resource(&other, &todo.app, "t1").unwrap());
    let Some(DynValue::Collection(collection)) = merged.get(&items) else {
        panic!("no items");
    };
    assert_eq!(collection.len(), 3);
}

#[test]
fn an_edit_of_a_missing_resource_is_refused() {
    let (counter, _) = loaded();
    let value = field_id(&counter, "Counter", "value");
    let mut doc = AutoCommit::new();
    let error = action(
        &counter.app,
        "x",
        vec![DynEdit::Edit {
            id: "nothing".into(),
            edit: Edit::new(field(&value), EditOp::Add { by: 1 }),
        }],
    )
    .apply(&mut doc)
    .unwrap_err();
    assert!(error.to_string().contains("no resource"), "{error}");
}

#[test]
fn the_registry_decodes_the_recorded_action() {
    let (counter, _) = loaded();
    let recorded = action(&counter.app, "x", Vec::new()).encode().unwrap();
    let registry = ActionRegistry::with(register_actions);
    assert!(registry.decode(&recorded).is_ok());
}

const DATED: &str = r#"app so.tilo.dated "Dated"

type Day {
    title: Text (title)
    start: Date = Date("2026-10-24")
    due: Option<Date> = .none
    done_at: Option<Time> = .none
}

root Day

default view day for Day

action finish(d: Day) {
    d.done_at = now
    d.start = Date("2026-10-25")
    d.due = .some(Date("2026-11-01"))
}

view day(d: Day) {
    page {
        text(d.title)
    }
}
"#;

// A Date or a Time is text in the record: a plain register for one, a structured one for an
// option; both read and write through the host, and `now` is the host's clock.
#[test]
fn dates_and_times_are_text_fields_and_now_is_the_hosts_clock() {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(DATED, "")]);
    let meta = packages[0].meta.clone();
    let start = lookup(&meta.types[0].id)
        .unwrap()
        .field(&field_id(&meta, "Day", "start"))
        .unwrap()
        .clone();
    assert_eq!(start.kind, tilolang_tilo::MergeKind::Register);
    assert!(!start.composite, "a Date is stored as plain text");
    ada.host.set_clock(|| "2026-10-21T09:30:00Z".to_owned());
    let day = ada.host.root_of(&meta.app).unwrap();
    ada.commit();
    let read = |r: &Replica, f: &str| r.host.runtime().read(&day, f).unwrap();
    assert_eq!(
        read(&ada, "start"),
        tilolang_runtime::Value::text("2026-10-24")
    );
    let finish = meta.actions.iter().find(|a| a.name == "finish").unwrap();
    ada.host
        .runtime()
        .invoke_action(
            &tilolang_runtime::Principal::new("ada"),
            (
                finish.id.as_str(),
                vec![tilolang_runtime::Value::resource(&day)],
            ),
        )
        .unwrap();
    ada.commit();
    let ids = packages[0].ids.clone();
    let bo = ada.fork("bo", &[(DATED, ids.as_str())]);
    for replica in [&ada, &bo] {
        assert_eq!(
            read(replica, "start"),
            tilolang_runtime::Value::text("2026-10-25")
        );
        assert_eq!(
            read(replica, "due"),
            tilolang_runtime::Value::some(tilolang_runtime::Value::text("2026-11-01"))
        );
        assert_eq!(
            read(replica, "done_at"),
            tilolang_runtime::Value::some(tilolang_runtime::Value::text("2026-10-21T09:30:00Z"))
        );
    }
}

#[test]
fn the_system_clock_answers_iso_utc() {
    assert_eq!(tilolang_tilo::host::iso_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(
        tilolang_tilo::host::iso_utc(951_782_400),
        "2000-02-29T00:00:00Z"
    );
    assert_eq!(
        tilolang_tilo::host::iso_utc(1_792_575_000),
        "2026-10-21T09:30:00Z"
    );
    assert!(tilolang::types::check_time(&tilolang_tilo::host::system_time()).is_ok());
}
