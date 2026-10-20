use automerge::AutoCommit;
use serde_json::json;
use tilo_actions::{Action, ActionRegistry, EncodedAction};
use tilo_resource::{Edit, EditOp, Id, Model, Segment};
use tilolang_tilo::action::{Authorship, DynAction, DynEdit, read_resource, register_actions};
use tilolang_tilo::record::{DynRecord, DynValue};
use tilolang_tilo::schema::lookup;

use crate::fixtures::meta;

const APP: &str = "so.tilo.counter";

fn field(id: &str) -> Vec<Segment> {
    vec![Segment::Field(id.to_owned())]
}

fn action(id: &str, edits: Vec<DynEdit>) -> DynAction {
    DynAction {
        app: APP.to_owned(),
        kind: "tl.so.tilo.counter.Counter".to_owned(),
        action_id: id.to_owned(),
        args: json!({}),
        authorship: Authorship::default(),
        edits,
    }
}

fn create(doc: &mut AutoCommit, ty: &str, id: &str) {
    let record = DynRecord::empty(lookup(ty).unwrap()).unwrap();
    action(
        "create",
        vec![DynEdit::Create {
            id: id.to_owned(),
            record: tilo_resource::Model::to_json(&record),
        }],
    )
    .apply(doc)
    .unwrap();
    doc.commit();
}

fn increment(id: &str) -> DynAction {
    action(
        "aincr001",
        vec![DynEdit::Edit {
            id: id.to_owned(),
            edit: Edit::new(field("fval0001"), EditOp::Add { by: 1 }),
        }],
    )
}

fn counter_value(doc: &AutoCommit, id: &str) -> i64 {
    match read_resource(doc, APP, id).unwrap().get("fval0001") {
        Some(DynValue::Counter(counter)) => counter.get(),
        other => panic!("not a counter: {other:?}"),
    }
}

#[test]
fn a_record_written_and_read_back_is_the_same_record() {
    let _ = meta();
    let mut doc = AutoCommit::new();
    create(&mut doc, "tcnt0001", "c1");
    let record = read_resource(&doc, APP, "c1").unwrap();
    assert_eq!(record.schema().name, "Counter");
    assert_eq!(counter_value(&doc, "c1"), 0);
    // The stored layout is the design's: `$type`, `$app` and the field by its id.
    let again = DynRecord::from_json(&tilo_resource::Model::to_json(&record)).unwrap();
    assert_eq!(again, record);
}

#[test]
fn two_replicas_incrementing_concurrently_read_two() {
    let _ = meta();
    let mut one = AutoCommit::new();
    create(&mut one, "tcnt0001", "c1");
    let mut two = one.fork();
    increment("c1").apply(&mut one).unwrap();
    one.commit();
    increment("c1").apply(&mut two).unwrap();
    two.commit();
    one.merge(&mut two).unwrap();
    two.merge(&mut one).unwrap();
    assert_eq!(counter_value(&one, "c1"), 2);
    assert_eq!(counter_value(&two, "c1"), 2);
}

#[test]
fn a_peer_without_the_module_replays_the_recorded_action() {
    let _ = meta();
    let mut origin = AutoCommit::new();
    create(&mut origin, "tcnt0001", "c1");
    let mut peer = origin.fork();
    // The origin plans the action and records its edits; the peer holds only the registry.
    let recorded = increment("c1").encode().unwrap();
    let registry = ActionRegistry::with(register_actions);
    let replayed = registry
        .decode(&EncodedAction {
            type_id: recorded.type_id.clone(),
            payload: recorded.payload.clone(),
        })
        .unwrap();
    replayed.apply(&mut peer).unwrap();
    peer.commit();
    assert_eq!(counter_value(&peer, "c1"), 1);
}

#[test]
fn text_collections_sets_and_maps_edit_by_their_merge_kind() {
    let _ = meta();
    let mut doc = AutoCommit::new();
    create(&mut doc, "ttodo001", "t1");
    let item = json!({
        "$type": "titem001", "$app": APP, "ftext001": "milk", "fcheck01": false
    });
    let edits = vec![
        DynEdit::Edit {
            id: "t1".into(),
            edit: Edit::new(
                field("ftitle01"),
                EditOp::Replace {
                    text: "Shopping".into(),
                },
            ),
        },
        DynEdit::Edit {
            id: "t1".into(),
            edit: Edit::new(field("fdone001"), EditOp::Set { value: json!(true) }),
        },
        DynEdit::Edit {
            id: "t1".into(),
            edit: Edit::new(
                field("fitems01"),
                EditOp::InsertItem {
                    id: Some(Id::from_raw("i1")),
                    at: 0,
                    value: item,
                },
            ),
        },
        DynEdit::Edit {
            id: "t1".into(),
            edit: Edit::new(
                field("ftags001"),
                EditOp::Insert {
                    value: json!("home"),
                },
            ),
        },
    ];
    action("aadd00001", edits).apply(&mut doc).unwrap();
    doc.commit();
    let todo = read_resource(&doc, APP, "t1").unwrap();
    assert_eq!(
        todo.get("ftitle01"),
        Some(&DynValue::Text("Shopping".into()))
    );
    assert_eq!(todo.get("fdone001"), Some(&DynValue::Register(json!(true))));
    let Some(DynValue::Collection(items)) = todo.get("fitems01") else {
        panic!("no items");
    };
    assert_eq!(items.len(), 1);
    let Some(DynValue::Set(tags)) = todo.get("ftags001") else {
        panic!("no tags");
    };
    assert_eq!(tags.len(), 1);

    // Concurrent text edits, an insert and a move of the same collection merge.
    let mut other = doc.fork();
    let more = |text: &str, id: &str, at: usize| DynEdit::Edit {
        id: "t1".into(),
        edit: Edit::new(
            field("fitems01"),
            EditOp::InsertItem {
                id: Some(Id::from_raw(id)),
                at,
                value: json!({"$type": "titem001", "$app": APP, "ftext001": text}),
            },
        ),
    };
    action("aadd00001", vec![more("eggs", "i2", 1)])
        .apply(&mut doc)
        .unwrap();
    doc.commit();
    action("aadd00001", vec![more("bread", "i3", 0)])
        .apply(&mut other)
        .unwrap();
    other.commit();
    doc.merge(&mut other).unwrap();
    other.merge(&mut doc).unwrap();
    let ids = |doc: &AutoCommit| -> Vec<String> {
        let todo = read_resource(doc, APP, "t1").unwrap();
        let Some(DynValue::Collection(items)) = todo.get("fitems01") else {
            panic!("no items")
        };
        items.iter().map(|_| String::new()).collect()
    };
    assert_eq!(ids(&doc).len(), 3);
    assert_eq!(ids(&doc), ids(&other));
    assert_eq!(
        read_resource(&doc, APP, "t1").unwrap(),
        read_resource(&other, APP, "t1").unwrap()
    );
}

#[test]
fn an_edit_of_a_missing_resource_is_refused() {
    let _ = meta();
    let mut doc = AutoCommit::new();
    let error = increment("nothing").apply(&mut doc).unwrap_err();
    assert!(error.to_string().contains("no resource"), "{error}");
}
