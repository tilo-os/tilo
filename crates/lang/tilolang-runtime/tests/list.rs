//! A collection: inserts, moves and removes keep item identity and patch by key.

mod common;

use common::*;
use tilolang_runtime::{Patch, Value};

fn world() -> (World, tilolang_runtime::ResourceRef) {
    let w = World::new();
    w.rt.load_package(&list_wasm(), list_meta()).unwrap();
    let l = w.rt.create("List").unwrap();
    (w, l)
}

fn add(w: &World, l: &tilolang_runtime::ResourceRef, s: &str) {
    w.rt.invoke_action(&w.me, ("add", vec![Value::resource(l), Value::text(s)]))
        .unwrap();
}

#[test]
fn insert_then_move_then_remove() {
    let (w, l) = world();
    for s in ["a", "b", "c"] {
        add(&w, &l, s);
    }
    assert_eq!(labels(&w.rt, &l), ["a", "b", "c"]);

    let mut v =
        w.rt.open_view(&w.me, "vl", vec![Value::resource(&l)])
            .unwrap();
    let keys = |v: &tilolang_runtime::ViewInstance| -> Vec<String> {
        v.tree().children.iter().map(|c| c.key.clone()).collect()
    };
    let before = keys(&v);
    assert_eq!(before.len(), 3);

    let c = item(&w.rt, &l, "c");
    w.rt.invoke_action(&w.me, ("move_first", vec![Value::resource(&l), c]))
        .unwrap();
    assert_eq!(labels(&w.rt, &l), ["c", "a", "b"]);
    let patches = v.poll_patches().unwrap();
    assert_eq!(
        patches,
        vec![Patch::Move {
            parent: vec!["col".into()],
            key: before[2].clone(),
            to: 0
        }],
        "a move keeps the item's identity: only its position changes"
    );

    let b = item(&w.rt, &l, "b");
    w.rt.invoke_action(&w.me, ("drop", vec![Value::resource(&l), b]))
        .unwrap();
    assert_eq!(labels(&w.rt, &l), ["c", "a"]);
    let patches = v.poll_patches().unwrap();
    assert_eq!(
        patches,
        vec![Patch::Remove {
            parent: vec!["col".into()],
            key: before[1].clone()
        }]
    );

    add(&w, &l, "d");
    let patches = v.poll_patches().unwrap();
    assert!(
        matches!(patches.as_slice(), [Patch::Insert { index: 2, .. }]),
        "{patches:?}"
    );
    assert_eq!(labels(&w.rt, &l), ["c", "a", "d"]);
}

#[test]
fn an_item_from_another_collection_is_refused() {
    let (w, l) = world();
    let other = w.rt.create("List").unwrap();
    add(&w, &l, "a");
    add(&w, &other, "z");
    let z = item(&w.rt, &other, "z");
    let err =
        w.rt.invoke_action(&w.me, ("drop", vec![Value::resource(&l), z]))
            .unwrap_err();
    assert!(err.to_string().contains("does not belong"), "{err}");
    assert_eq!(labels(&w.rt, &l), ["a"]);
}
