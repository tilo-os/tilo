use automerge::AutoCommit;
use serde_json::json;
use tilo_actions::{Action, ActionRegistry};
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
    doc.merge(&mut other).unwrap();
    other.merge(&mut doc).unwrap();
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
