//! The store follows the room record by its changes: a peer's edit of one item reaches the
//! runtime as exactly that item's field, and the mirror is never copied.

use std::cell::RefCell;
use std::rc::Rc;

use automerge::AutoCommit;
use serde_json::{Value as Json, json};
use tilo_resource::{Edit, EditOp, Id, Segment};
use tilolang_runtime::store::{ChangeSummary, FieldKey, Store as _};
use tilolang_runtime::value::Value;
use tilolang_tilo::action::{Authorship, DynAction, DynEdit};
use tilolang_tilo::publish::PlanRequest;
use tilolang_tilo::record::DynValue;

use crate::fixtures::{Replica, TODO, field_id};

const APP: &str = "so.tilo.todo";

/// Ada's room with three items, and Grace's fork of it.
fn room() -> (Replica, Replica, tilolang_runtime::Meta, String) {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(TODO, "")]);
    let todo = packages[0].clone();
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    for text in ["milk", "eggs", "bread"] {
        add(&mut ada, &todo.meta, &root.doc, text);
    }
    let grace = ada.fork("grace", &[(TODO, &todo.ids)]);
    (ada, grace, todo.meta, root.doc)
}

fn add(replica: &mut Replica, meta: &tilolang_runtime::Meta, root: &str, text: &str) {
    let action = meta.actions.iter().find(|a| a.name == "add").unwrap();
    let args = json!({ "text": text }).as_object().cloned().unwrap();
    let planned = replica
        .host
        .plan(&PlanRequest {
            app: APP,
            kind: "tl.so.tilo.todo.TodoList",
            action_id: &action.id,
            resource: Some(root),
            args: &args,
            caller: None,
        })
        .unwrap();
    replica.apply(&planned);
}

fn item_ids(replica: &Replica, meta: &tilolang_runtime::Meta, root: &str) -> Vec<String> {
    let record = replica.host.store().record(root).unwrap();
    match record.get(&field_id(meta, "TodoList", "items")) {
        Some(DynValue::Collection(items)) => {
            items.iter().map(|(id, _)| id.as_str().to_owned()).collect()
        }
        other => panic!("{other:?}"),
    }
}

/// What the store tells a subscriber of `doc`.
fn listen(replica: &Replica, doc: &str) -> Rc<RefCell<Vec<ChangeSummary>>> {
    let heard: Rc<RefCell<Vec<ChangeSummary>>> = Rc::default();
    let sink = Rc::clone(&heard);
    let mut store = replica.host.store().clone();
    store.subscribe(
        &doc.to_owned(),
        Box::new(move |summary| sink.borrow_mut().push(summary.clone())),
    );
    heard
}

/// Grace retypes the text of one item, as the executor commits it.
fn retype(grace: &mut Replica, meta: &tilolang_runtime::Meta, root: &str, item: &str, text: &str) {
    let edit = Edit::new(
        vec![
            Segment::Field(field_id(meta, "TodoList", "items")),
            Segment::Item(Id::from_raw(item)),
            Segment::Field(field_id(meta, "Item", "text")),
        ],
        EditOp::Replace {
            text: text.to_owned(),
        },
    );
    grace.apply(&DynAction {
        app: APP.to_owned(),
        kind: "tl.so.tilo.todo.TodoList".to_owned(),
        action_id: "$edit".to_owned(),
        args: Json::Null,
        authorship: Authorship::default(),
        edits: vec![DynEdit::Edit {
            id: root.to_owned(),
            edit,
        }],
    });
}

#[test]
fn a_peers_edit_of_one_item_reaches_the_runtime_as_that_items_field() {
    let (mut ada, mut grace, meta, root) = room();
    let ids = item_ids(&ada, &meta, &root);
    let heard = listen(&ada, &root);
    retype(&mut grace, &meta, &root, &ids[1], "oat milk");
    ada.sync_with(&mut grace);
    let heard = heard.borrow();
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert_eq!(
        heard[0].fields,
        vec![FieldKey {
            resource: format!("{root}/{}:{}", field_id(&meta, "TodoList", "items"), ids[1]),
            field: field_id(&meta, "Item", "text"),
        }],
        "only the retyped item's text, not the whole record"
    );
}

#[test]
fn an_item_added_by_a_peer_changes_the_collection_only() {
    let (mut ada, mut grace, meta, root) = room();
    let heard = listen(&ada, &root);
    add(&mut grace, &meta, &root, "tea");
    ada.sync_with(&mut grace);
    let heard = heard.borrow();
    let items = field_id(&meta, "TodoList", "items");
    assert!(
        heard.iter().all(|summary| !summary.fields.is_empty()),
        "never the whole document: {heard:?}"
    );
    assert!(
        heard
            .iter()
            .flat_map(|summary| &summary.fields)
            .any(|key| key.resource == root && key.field == items),
        "{heard:?}"
    );
    assert_eq!(item_ids(&ada, &meta, &root).len(), 4);
}

#[test]
fn the_mirror_follows_the_record_without_copying_it() {
    let (mut ada, mut grace, meta, root) = room();
    let ids = item_ids(&ada, &meta, &root);
    retype(&mut grace, &meta, &root, &ids[0], "almond milk");
    ada.sync_with(&mut grace);
    add(&mut ada, &meta, &root, "jam");
    let stats = ada.host.store().stats();
    assert_eq!(stats.full_syncs, 0, "{stats:?}");
    assert!(stats.incremental_syncs >= 2, "{stats:?}");
    // The runtime reads what the record holds.
    let items = tilolang_runtime::value::ResourceRef::root(&root, type_id(&meta, "TodoList"));
    let first = items.item(
        &field_id(&meta, "TodoList", "items"),
        &ids[0],
        type_id(&meta, "Item"),
    );
    assert_eq!(
        ada.host.runtime().read(&first, "text").unwrap(),
        Value::text("almond milk")
    );
    assert_eq!(item_ids(&ada, &meta, &root).len(), 4);
}

#[test]
fn a_trial_run_takes_back_only_its_own_writes() {
    let (mut ada, _grace, meta, root) = room();
    // A press the executor has not committed yet: the runtime sees it, the record does not.
    let view = ada.view(APP);
    let title = crate::fixtures::field_id(&meta, "TodoList", "title");
    let input = find_bound(view.tree(), &title).unwrap();
    view.edit(&input, &Value::text("Groceries")).unwrap();
    // Planning another action for the door writes into the mirror and takes it back.
    let action = meta.actions.iter().find(|a| a.name == "add").unwrap();
    let args = json!({ "text": "tea" }).as_object().cloned().unwrap();
    ada.host
        .plan(&PlanRequest {
            app: APP,
            kind: "tl.so.tilo.todo.TodoList",
            action_id: &action.id,
            resource: Some(&root),
            args: &args,
            caller: None,
        })
        .unwrap();
    assert_eq!(item_ids(&ada, &meta, &root).len(), 3, "the trial is gone");
    let list = tilolang_runtime::value::ResourceRef::root(&root, type_id(&meta, "TodoList"));
    assert_eq!(
        ada.host.runtime().read(&list, "title").unwrap(),
        Value::text("Groceries"),
        "the press is still there"
    );
    ada.commit();
    assert_eq!(
        ada.host.runtime().read(&list, "title").unwrap(),
        Value::text("Groceries")
    );
    assert_eq!(ada.host.store().stats().full_syncs, 0);
}

#[test]
fn what_the_runtime_read_follows_every_change_after() {
    let (mut ada, mut grace, meta, root) = room();
    let ids = item_ids(&ada, &meta, &root);
    let list = tilolang_runtime::value::ResourceRef::root(&root, type_id(&meta, "TodoList"));
    let first = list.item(
        &field_id(&meta, "TodoList", "items"),
        &ids[0],
        type_id(&meta, "Item"),
    );
    let count = |ada: &Replica| match ada.host.runtime().read(&list, "items").unwrap() {
        Value::List(items) => items.len(),
        other => panic!("{other:?}"),
    };
    let text = |ada: &Replica| ada.host.runtime().read(&first, "text").unwrap();
    // Read once, so that the store keeps what it read.
    assert_eq!(text(&ada), Value::text("milk"));
    assert_eq!(count(&ada), 3);

    // A peer retypes the item and adds one.
    retype(&mut grace, &meta, &root, &ids[0], "oat milk");
    add(&mut grace, &meta, &root, "tea");
    ada.sync_with(&mut grace);
    assert_eq!(text(&ada), Value::text("oat milk"));
    assert_eq!(count(&ada), 4);

    // A trial run adds one and takes it back.
    let action = meta.actions.iter().find(|a| a.name == "add").unwrap();
    let args = json!({ "text": "jam" }).as_object().cloned().unwrap();
    ada.host
        .plan(&PlanRequest {
            app: APP,
            kind: "tl.so.tilo.todo.TodoList",
            action_id: &action.id,
            resource: Some(&root),
            args: &args,
            caller: None,
        })
        .unwrap();
    assert_eq!(count(&ada), 4);

    // An addition of its own.
    add(&mut ada, &meta, &root, "jam");
    assert_eq!(count(&ada), 5);
    assert_eq!(text(&ada), Value::text("oat milk"));
}

fn type_id(meta: &tilolang_runtime::Meta, name: &str) -> String {
    meta.types
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
        .clone()
}

fn find_bound(node: &tilolang_runtime::UiNode, field: &str) -> Option<String> {
    if node
        .binding
        .as_ref()
        .and_then(|b| b.field())
        .is_some_and(|(_, f)| f == field)
    {
        return Some(node.key.clone());
    }
    node.children.iter().find_map(|c| find_bound(c, field))
}
