//! The counter fixture, compiled by tilolang and run on two replicas.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, InvokeOutcome, Patch, Runtime, SimpleAuthority, Value};

    const SOURCE: &str = include_str!("../../tilolang/tests/fixtures/counter.tl");

    #[test]
    fn two_replicas_increment_concurrently_then_both_read_two() {
        let app = App::compile(SOURCE, &[]);
        let (mut store_a, mut store_b) = (AmStore::new(), AmStore::new());
        let rt_a = Runtime::new(store_a.clone(), SimpleAuthority::new()).unwrap();
        let rt_b = Runtime::new(store_b.clone(), SimpleAuthority::new()).unwrap();
        app.load(&rt_a);
        app.load(&rt_b);
        let me = me();
        let inc = app.action("increment");

        let c = rt_a.create("Counter").unwrap();
        store_a.sync_with(&mut store_b).unwrap();
        for rt in [&rt_a, &rt_b] {
            let out = rt
                .invoke_action(&me, (inc.as_str(), vec![Value::resource(&c)]))
                .unwrap();
            assert_eq!(out, InvokeOutcome::Done(Value::Unit));
            assert_eq!(rt.read(&c, "value").unwrap(), Value::Int(1));
        }
        store_a.sync_with(&mut store_b).unwrap();
        assert_eq!(rt_a.read(&c, "value").unwrap(), Value::Int(2));
        assert_eq!(rt_b.read(&c, "value").unwrap(), Value::Int(2));
    }

    #[test]
    fn the_view_shows_the_count_and_the_plus_button_bumps_it() {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let me = me();
        let c = rt.create("Counter").unwrap();
        let mut view = rt
            .open_view(&me, &app.view("counter"), vec![Value::resource(&c)])
            .unwrap();
        let shown = |v: &tilolang_runtime::ViewInstance| {
            of_kind(v.tree(), "text")[0].props["value"].clone()
        };
        assert_eq!(shown(&view), Value::Int(0));

        let plus = app.anchor("counter", "plus");
        assert!(view.tree().find(&plus).is_some(), "{}", dump(view.tree()));
        assert!(matches!(view.press(&plus).unwrap(), InvokeOutcome::Done(_)));
        let patches = view.poll_patches().unwrap();
        assert!(
            matches!(patches.as_slice(), [Patch::SetProp { name, value: Some(Value::Int(1)), .. }] if name == "value"),
            "{patches:?}"
        );
        assert_eq!(shown(&view), Value::Int(1));
    }

    #[test]
    fn a_replica_view_follows_incoming_changes() {
        let app = App::compile(SOURCE, &[]);
        let (mut store_a, mut store_b) = (AmStore::new(), AmStore::new());
        let rt_a = Runtime::new(store_a.clone(), SimpleAuthority::new()).unwrap();
        let rt_b = Runtime::new(store_b.clone(), SimpleAuthority::new()).unwrap();
        app.load(&rt_a);
        app.load(&rt_b);
        let me = me();
        let c = rt_a.create("Counter").unwrap();
        store_a.sync_with(&mut store_b).unwrap();
        let mut view_b = rt_b
            .open_view(&me, &app.view("counter"), vec![Value::resource(&c)])
            .unwrap();
        rt_a.invoke_action(
            &me,
            (app.action("increment").as_str(), vec![Value::resource(&c)]),
        )
        .unwrap();
        store_a.sync_with(&mut store_b).unwrap();
        let patches = view_b.poll_patches().unwrap();
        assert!(
            matches!(
                patches.as_slice(),
                [Patch::SetProp {
                    value: Some(Value::Int(1)),
                    ..
                }]
            ),
            "{patches:?}"
        );
    }
}
