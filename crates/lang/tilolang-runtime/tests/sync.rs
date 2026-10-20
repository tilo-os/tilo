//! Two replicas of the reference store converge after `sync_with`.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, Patch, Value};

    fn pair(
        packages: &[(Vec<u8>, tilolang_runtime::Meta)],
    ) -> (
        AmStore,
        AmStore,
        tilolang_runtime::Runtime,
        tilolang_runtime::Runtime,
    ) {
        let (a, b) = (AmStore::new(), AmStore::new());
        let (ra, rb) = (replica(&a, packages), replica(&b, packages));
        (a, b, ra, rb)
    }

    #[test]
    fn two_replicas_each_increment_once_and_both_read_two() {
        let (mut a, mut b, ra, rb) = pair(&[(counter_wasm(), counter_meta())]);
        let me = me();
        let c = ra.create("Counter").unwrap();
        a.sync_with(&mut b).unwrap();
        assert_eq!(
            rb.read(&c, "value").unwrap(),
            Value::Int(0),
            "the document arrived"
        );

        let mut view_b = rb.open_view(&me, "vc", vec![Value::resource(&c)]).unwrap();
        ra.invoke_action(&me, ("inc", vec![Value::resource(&c)]))
            .unwrap();
        rb.invoke_action(&me, ("inc", vec![Value::resource(&c)]))
            .unwrap();
        assert_eq!(ra.read(&c, "value").unwrap(), Value::Int(1));
        assert_eq!(rb.read(&c, "value").unwrap(), Value::Int(1));

        a.sync_with(&mut b).unwrap();
        assert_eq!(ra.read(&c, "value").unwrap(), Value::Int(2));
        assert_eq!(rb.read(&c, "value").unwrap(), Value::Int(2));

        assert!(view_b.is_stale(), "incoming changes reach views");
        assert_eq!(
            view_b.poll_patches().unwrap(),
            vec![Patch::SetProp {
                path: vec!["col".into(), "txt".into()],
                name: "text".into(),
                value: Some(Value::Int(2)),
            }]
        );
    }

    #[test]
    fn concurrent_text_splices_merge() {
        let (mut a, mut b, ra, rb) = pair(&[(note_wasm(), note_meta())]);
        let me = me();
        let n = ra.create("Note").unwrap();
        let set = |rt: &tilolang_runtime::Runtime, s: &str| {
            rt.invoke_action(&me, ("set_body", vec![Value::resource(&n), Value::text(s)]))
                .unwrap();
        };
        set(&ra, "Hello");
        a.sync_with(&mut b).unwrap();

        set(&ra, "Hello world");
        set(&rb, "Big Hello");
        a.sync_with(&mut b).unwrap();

        let (ta, tb) = (ra.read(&n, "body").unwrap(), rb.read(&n, "body").unwrap());
        assert_eq!(ta, tb);
        assert_eq!(ta, Value::text("Big Hello world"), "both splices survive");
    }

    #[test]
    fn concurrent_collection_insert_and_move_converge() {
        let (mut a, mut b, ra, rb) = pair(&[(list_wasm(), list_meta())]);
        let me = me();
        let l = ra.create("List").unwrap();
        let add = |rt: &tilolang_runtime::Runtime, s: &str| {
            rt.invoke_action(&me, ("add", vec![Value::resource(&l), Value::text(s)]))
                .unwrap();
        };
        add(&ra, "a");
        add(&ra, "b");
        a.sync_with(&mut b).unwrap();
        assert_eq!(labels(&rb, &l), ["a", "b"]);

        add(&ra, "c");
        let b_item = item(&rb, &l, "b");
        rb.invoke_action(&me, ("move_first", vec![Value::resource(&l), b_item]))
            .unwrap();
        a.sync_with(&mut b).unwrap();

        assert_eq!(labels(&ra, &l), labels(&rb, &l));
        assert_eq!(labels(&ra, &l), ["b", "a", "c"]);
    }

    #[test]
    fn concurrent_moves_of_the_same_item_converge_to_one_place() {
        let (mut a, mut b, ra, rb) = pair(&[(list_wasm(), list_meta())]);
        let me = me();
        let l = ra.create("List").unwrap();
        for s in ["a", "b", "c"] {
            ra.invoke_action(&me, ("add", vec![Value::resource(&l), Value::text(s)]))
                .unwrap();
        }
        a.sync_with(&mut b).unwrap();
        for rt in [&ra, &rb] {
            let c = item(rt, &l, "c");
            rt.invoke_action(&me, ("move_first", vec![Value::resource(&l), c]))
                .unwrap();
        }
        a.sync_with(&mut b).unwrap();
        assert_eq!(labels(&ra, &l), labels(&rb, &l));
        let order = labels(&ra, &l);
        assert_eq!(order.len(), 3, "no duplicates: {order:?}");
        assert_eq!(order[0], "c");
    }

    #[test]
    fn syncing_a_store_with_itself_is_refused() {
        let a = AmStore::new();
        let mut x = a.clone();
        let mut y = a;
        assert!(x.sync_with(&mut y).is_err());
    }
}
