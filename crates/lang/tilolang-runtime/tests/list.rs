//! A collection: inserts, moves and removes keep item identity and patch by key.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{Patch, Value};

    fn world() -> (World, tilolang_runtime::ResourceRef) {
        let w = World::new();
        w.rt.load_package(&list_wasm(), list_meta()).unwrap();
        let l = w.rt.create("Bag").unwrap();
        (w, l)
    }

    fn add(w: &World, list: &tilolang_runtime::ResourceRef, s: &str) {
        w.rt.invoke_action(&w.me, ("add", vec![Value::resource(list), Value::text(s)]))
            .unwrap();
    }

    #[test]
    fn insert_then_move_then_remove() {
        let (w, list) = world();
        for label in ["a", "b", "c"] {
            add(&w, &list, label);
        }
        assert_eq!(labels(&w.rt, &list), ["a", "b", "c"]);

        let mut view =
            w.rt.open_view(&w.me, "vl", vec![Value::resource(&list)])
                .unwrap();
        let keys = |view: &tilolang_runtime::ViewInstance| -> Vec<String> {
            view.tree()
                .children
                .iter()
                .map(|child| child.key.clone())
                .collect()
        };
        let before = keys(&view);
        assert_eq!(before.len(), 3);

        let first = item(&w.rt, &list, "c");
        w.rt.invoke_action(&w.me, ("move_first", vec![Value::resource(&list), first]))
            .unwrap();
        assert_eq!(labels(&w.rt, &list), ["c", "a", "b"]);
        let patches = view.poll_patches().unwrap();
        assert_eq!(
            patches,
            vec![Patch::Move {
                parent: vec!["col".into()],
                key: before[2].clone(),
                to: 0
            }],
            "a move keeps the item's identity: only its position changes"
        );

        let second = item(&w.rt, &list, "b");
        w.rt.invoke_action(&w.me, ("drop", vec![Value::resource(&list), second]))
            .unwrap();
        assert_eq!(labels(&w.rt, &list), ["c", "a"]);
        let patches = view.poll_patches().unwrap();
        assert_eq!(
            patches,
            vec![Patch::Remove {
                parent: vec!["col".into()],
                key: before[1].clone()
            }]
        );

        add(&w, &list, "d");
        let patches = view.poll_patches().unwrap();
        assert!(
            matches!(patches.as_slice(), [Patch::Insert { index: 2, .. }]),
            "{patches:?}"
        );
        assert_eq!(labels(&w.rt, &list), ["c", "a", "d"]);
    }

    #[test]
    fn an_item_from_another_collection_is_refused() {
        let (w, list) = world();
        let other = w.rt.create("Bag").unwrap();
        add(&w, &list, "a");
        add(&w, &other, "z");
        let zed = item(&w.rt, &other, "z");
        let err =
            w.rt.invoke_action(&w.me, ("drop", vec![Value::resource(&list), zed]))
                .unwrap_err();
        assert!(err.to_string().contains("does not belong"), "{err}");
        assert_eq!(labels(&w.rt, &list), ["a"]);
    }
}
