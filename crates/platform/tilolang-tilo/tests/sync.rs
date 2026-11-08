//! The store follows the workspace record by its changes: a peer's edit of one item reaches the
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

/// Ada's workspace with three items, and Grace's fork of it.
fn workspace() -> (Replica, Replica, tilolang_runtime::Meta, String) {
    let (ada, grace, meta, root, _) = workspace_of(3);
    (ada, grace, meta, root)
}

/// Ada's workspace with `n` items made in one change, Grace's fork of it, and the package's
/// ids.
fn workspace_of(n: usize) -> (Replica, Replica, tilolang_runtime::Meta, String, String) {
    use tilo_resource::{Collection, Model as _};
    use tilolang_tilo::record::DynRecord;

    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(TODO, "")]);
    let todo = packages[0].clone();
    let meta = todo.meta.clone();
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    if n <= 3 {
        for text in ["milk", "eggs", "bread"].into_iter().take(n) {
            add(&mut ada, &meta, &root.doc, text);
        }
    } else {
        // Made in one change, as a workspace that holds a thousand items is.
        let item = meta.types.iter().find(|held| held.name == "Item").unwrap();
        let schema = tilolang_tilo::schema::lookup(&item.id).unwrap();
        let mut items = Collection::default();
        for i in 0..n {
            let mut record = DynRecord::empty(schema).unwrap();
            if let Some(DynValue::Text(text)) = record.get_mut(&field_id(&meta, "Item", "text")) {
                *text = format!("item {i:04}");
            }
            items.push(Id::from_raw(format!("item-{i:04}")), record);
        }
        let list = meta
            .types
            .iter()
            .find(|held| held.name == "TodoList")
            .unwrap();
        let mut made = DynRecord::empty(tilolang_tilo::schema::lookup(&list.id).unwrap()).unwrap();
        if let Some(DynValue::Collection(slot)) =
            made.get_mut(&field_id(&meta, "TodoList", "items"))
        {
            *slot = items;
        }
        ada.apply(&DynAction {
            app: APP.to_owned(),
            kind: "tl.so.tilo.todo.TodoList".to_owned(),
            action_id: "$create".to_owned(),
            args: Json::Null,
            authorship: Authorship::default(),
            edits: vec![DynEdit::Create {
                id: root.doc.clone(),
                record: made.to_json(),
            }],
        });
    }
    let grace = ada.fork("grace", &[(TODO, &todo.ids)]);
    (ada, grace, meta, root.doc, todo.ids)
}

/// A person typing `text` into the item `item`, as a bound control's edit writes it: the store
/// holds the edit and the host commits it to the record when it next takes what the store
/// queued.
fn typed(ada: &Replica, meta: &tilolang_runtime::Meta, root: &str, item: &str, text: &str) {
    use tilolang_runtime::store::{ChangeNote, Mutation};
    use tilolang_runtime::value::ResourceRef;

    let at = ResourceRef::root(root, type_id(meta, "TodoList")).item(item, type_id(meta, "Item"));
    let mut store = ada.host.store().clone();
    let txn = store.begin().unwrap();
    store
        .apply(
            txn,
            &at,
            &Mutation::SetText {
                field: field_id(meta, "Item", "text"),
                text: text.to_owned(),
            },
        )
        .unwrap();
    store
        .commit(
            txn,
            &ChangeNote {
                action: "$edit".to_owned(),
                invoker: ada.name.clone(),
                ..ChangeNote::default()
            },
        )
        .unwrap();
}

/// What the list's record holds: its title, and its items' texts in order.
fn listed(replica: &Replica, meta: &tilolang_runtime::Meta, root: &str) -> (String, Vec<String>) {
    let record = replica.host.store().record(root).unwrap();
    let title = match record.get(&field_id(meta, "TodoList", "title")) {
        Some(DynValue::Text(text)) => text.clone(),
        other => panic!("{other:?}"),
    };
    let texts = match record.get(&field_id(meta, "TodoList", "items")) {
        Some(DynValue::Collection(items)) => items
            .iter()
            .map(
                |(_, item)| match item.get(&field_id(meta, "Item", "text")) {
                    Some(DynValue::Text(text)) => text.clone(),
                    other => panic!("{other:?}"),
                },
            )
            .collect(),
        other => panic!("{other:?}"),
    };
    (title, texts)
}

/// The texts the `main` view renders, in the order it renders them.
fn shown(tree: &tilolang_runtime::UiNode) -> Vec<String> {
    fn walk(node: &tilolang_runtime::UiNode, out: &mut Vec<String>) {
        if node.kind == "ui.Text"
            && let Some(Value::Text(text)) = node.props.get("value")
        {
            out.push(text.clone());
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
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
            from: None,
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
    let (mut ada, mut grace, meta, root) = workspace();
    let ids = item_ids(&ada, &meta, &root);
    let heard = listen(&ada, &root);
    retype(&mut grace, &meta, &root, &ids[1], "oat milk");
    ada.sync_with(&mut grace);
    let heard = heard.borrow();
    assert_eq!(heard.len(), 1, "{heard:?}");
    // The item's text, the version the edit made of it, and the marks and the change of that
    // field a view reads of it, which the edit moved.
    let mut fields = heard[0].fields.clone();
    fields.sort_by(|left, right| left.field.cmp(&right.field));
    let mut expected = vec![
        FieldKey {
            resource: format!("{root}/{}", ids[1]),
            field: field_id(&meta, "Item", "text"),
        },
        FieldKey {
            resource: format!("{root}/{}", ids[1]),
            field: tilolang_runtime::DOC_VERSIONS.to_owned(),
        },
        FieldKey {
            resource: format!("{root}/{}", ids[1]),
            field: tilolang_runtime::store::Reflected::CREATED.to_owned(),
        },
        FieldKey {
            resource: format!("{root}/{}", ids[1]),
            field: tilolang_runtime::store::Reflected::UPDATED.to_owned(),
        },
        FieldKey {
            resource: format!("{root}/{}", ids[1]),
            field: tilolang_runtime::store::changed_field(&field_id(&meta, "Item", "text")),
        },
    ];
    expected.sort_by(|left, right| left.field.cmp(&right.field));
    assert_eq!(
        fields, expected,
        "only the retyped item, not the whole record"
    );
}

#[test]
fn an_item_added_by_a_peer_changes_the_collection_only() {
    let (mut ada, mut grace, meta, root) = workspace();
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
    let (mut ada, mut grace, meta, root) = workspace();
    let ids = item_ids(&ada, &meta, &root);
    retype(&mut grace, &meta, &root, &ids[0], "almond milk");
    ada.sync_with(&mut grace);
    add(&mut ada, &meta, &root, "jam");
    let stats = ada.host.store().stats();
    assert_eq!(stats.full_syncs, 0, "{stats:?}");
    assert!(stats.incremental_syncs >= 2, "{stats:?}");
    // The runtime reads what the record holds.
    let items = tilolang_runtime::value::ResourceRef::root(&root, type_id(&meta, "TodoList"));
    let first = items.item(&ids[0], type_id(&meta, "Item"));
    assert_eq!(
        ada.host.runtime().read(&first, "text").unwrap(),
        Value::text("almond milk")
    );
    assert_eq!(item_ids(&ada, &meta, &root).len(), 4);
}

#[test]
fn a_trial_run_takes_back_only_its_own_writes() {
    let (mut ada, _grace, meta, root) = workspace();
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
            from: None,
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
    let (mut ada, mut grace, meta, root) = workspace();
    let ids = item_ids(&ada, &meta, &root);
    let list = tilolang_runtime::value::ResourceRef::root(&root, type_id(&meta, "TodoList"));
    let first = list.item(&ids[0], type_id(&meta, "Item"));
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
            from: None,
        })
        .unwrap();
    assert_eq!(count(&ada), 4);

    // An addition of its own.
    add(&mut ada, &meta, &root, "jam");
    assert_eq!(count(&ada), 5);
    assert_eq!(text(&ada), Value::text("oat milk"));
}

#[test]
fn a_peers_change_keeps_every_edit_a_person_has_not_committed_yet() {
    let (mut ada, mut grace, meta, root, ids_lock) = workspace_of(3);
    let ids = item_ids(&ada, &meta, &root);
    let committed = ada.log.len();

    // Ada types into two items and the list's title. The record has none of it: the host has
    // not taken what the store queued.
    typed(&ada, &meta, &root, &ids[0], "oat milk");
    typed(&ada, &meta, &root, &ids[2], "rye");
    let view = ada.view(APP);
    let title = field_id(&meta, "TodoList", "title");
    let input = find_bound(view.tree(), &title).unwrap();
    view.edit(&input, &Value::text("Groceries")).unwrap();
    assert_eq!(
        ada.log.len(),
        committed,
        "nothing of it is the record's yet"
    );

    // Grace's change arrives first: she adds an item and retypes another.
    add(&mut grace, &meta, &root, "tea");
    retype(&mut grace, &meta, &root, &ids[1], "eggs, large");
    ada.sync_with(&mut grace);
    assert_eq!(ada.log.len(), committed, "still none of it is the record's");

    // Every edit is still what the store answers, and Grace's change is there too.
    let (held_title, texts) = listed(&ada, &meta, &root);
    assert_eq!(
        held_title, "Groceries",
        "the person's title survived the sync"
    );
    assert_eq!(
        texts,
        ["oat milk", "eggs, large", "rye", "tea"],
        "every edit a person made survived the sync, beside the peer's change"
    );
    let view = ada.view(APP);
    assert_eq!(
        shown(view.tree()),
        ["oat milk", "eggs, large", "rye", "tea"],
        "the window shows every edit, and the peer's change"
    );

    // The host takes what the store queued: the record holds every edit, and a start from it
    // reads the same.
    ada.commit();
    assert_eq!(
        ada.log.len(),
        committed + 3,
        "one change an edit: {}",
        ada.log.len()
    );
    let restarted = Replica::new("ada", ada.doc.fork(), &[(TODO, &ids_lock)]).0;
    for replica in [&ada, &restarted] {
        let (held_title, texts) = listed(replica, &meta, &root);
        assert_eq!(held_title, "Groceries");
        assert_eq!(texts, ["oat milk", "eggs, large", "rye", "tea"]);
        let view = replica.view(APP);
        assert_eq!(
            shown(view.tree()),
            ["oat milk", "eggs, large", "rye", "tea"]
        );
    }
}

#[test]
fn a_sync_of_one_unrelated_change_takes_no_row_of_a_thousand_away() {
    use tilolang_runtime::Principal;
    use tilolang_runtime::meta::FieldKind;
    use tilolang_runtime::store::{FieldSpec, Splice};
    use tilolang_runtime::value::ResourceRef;

    let (mut ada, mut grace, meta, root, _ids) = workspace_of(1_000);
    let list = ResourceRef::root(&root, type_id(&meta, "TodoList"));
    let rows = FieldSpec {
        id: field_id(&meta, "TodoList", "items"),
        kind: FieldKind::Collection,
    };
    let read = |replica: &Replica| -> Vec<String> {
        let listing = replica.host.store().listing(&list, &rows).unwrap();
        listing.ids()
    };
    let before = read(&ada);
    assert_eq!(before.len(), 1_000, "a listing of a thousand items");

    // Ada adds three items: the store's writes, which the record does not hold yet.
    let add = meta
        .actions
        .iter()
        .find(|action| action.name == "add")
        .unwrap()
        .id
        .clone();
    for text in ["one", "two", "three"] {
        ada.host
            .runtime()
            .invoke_action(
                &Principal::new("ada"),
                (
                    add.as_str(),
                    vec![Value::resource(&list), Value::text(text)],
                ),
            )
            .unwrap();
    }
    let with_hers = read(&ada);
    assert_eq!(with_hers.len(), 1_003, "her items are what the store shows");
    let heard = listen(&ada, &root);

    // Grace's one change touches no row's place: one item's text alone.
    retype(&mut grace, &meta, &root, "item-0500", "retyped");
    ada.sync_with(&mut grace);

    // Nothing left, nothing moved: the listing reads as it did, and nothing was told as a step.
    let after = read(&ada);
    let differs = after
        .iter()
        .zip(&with_hers)
        .position(|(now, then)| now != then);
    assert!(
        after == with_hers,
        "no row of the listing left or moved ({} rows, {} then, first differs at {differs:?}): \
         her items are still there, where they were",
        after.len(),
        with_hers.len()
    );
    let heard = heard.borrow();
    let steps: Vec<&[Splice]> = heard
        .iter()
        .filter_map(|summary| {
            summary.listing(&FieldKey {
                resource: root.clone(),
                field: field_id(&meta, "TodoList", "items"),
            })
        })
        .collect();
    assert!(
        steps.is_empty(),
        "a change of one item's text steps no row: {steps:?} of {heard:?}"
    );
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
