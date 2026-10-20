//! The counter end to end, limits, rollback and the trust boundary of a module.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use serde_json::json;
    use tilolang_runtime::{
        AmStore, EngineLimits, InvokeOutcome, Patch, Runtime, RuntimeError, SignatureKind,
        SimpleAuthority, Value,
    };

    #[test]
    fn the_counter_end_to_end() {
        let w = World::new();
        let c = w.counter();
        for _ in 0..2 {
            let out =
                w.rt.invoke_action(&w.me, ("inc", vec![Value::resource(&c)]))
                    .unwrap();
            assert_eq!(out, InvokeOutcome::Done(Value::Unit));
        }
        assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(2));

        let notes = w.rt.history(&c.doc).unwrap();
        let incs: Vec<_> = notes.iter().filter(|n| n.action == "inc").collect();
        assert_eq!(incs.len(), 2, "one change per invocation: {notes:?}");
        assert_eq!(incs[0].invoker, "me");
        assert_eq!(incs[0].module, w.counter.0);
        assert!(
            incs[0].chain.is_empty(),
            "a direct call has no code in its chain"
        );
    }

    #[test]
    fn loading_the_same_module_twice_compiles_once() {
        let w = World::new();
        let again = w.rt.load_package(&counter_wasm(), counter_meta()).unwrap();
        assert_eq!(again, w.counter);
        assert_eq!(w.rt.compiled_modules(), 1);
        assert_eq!(Runtime::module_hash(&counter_wasm()), w.counter);
    }

    #[test]
    fn a_view_shows_the_value_and_a_press_patches_only_the_text() {
        let w = World::new();
        let c = w.counter();
        let mut v =
            w.rt.open_view(&w.me, "vc", vec![Value::resource(&c)])
                .unwrap();
        assert_eq!(v.tree().find("txt").unwrap().props["text"], Value::Int(0));
        assert_eq!(
            v.tree().find("btn").unwrap().props["label"],
            Value::text("+1")
        );
        assert!(v.poll_patches().unwrap().is_empty());

        assert!(matches!(v.press("btn").unwrap(), InvokeOutcome::Done(_)));
        let patches = v.poll_patches().unwrap();
        assert_eq!(
            patches,
            vec![Patch::SetProp {
                path: vec!["col".into(), "txt".into()],
                name: "text".into(),
                value: Some(Value::Int(1)),
            }],
            "changing one field must touch only the node that shows it"
        );
        assert!(v.poll_patches().unwrap().is_empty());
    }

    #[test]
    fn a_trapping_action_is_rolled_back() {
        let w = World::new();
        let c = w.counter();
        let err =
            w.rt.invoke_action(&w.me, ("bad", vec![Value::resource(&c)]))
                .unwrap_err();
        assert!(matches!(err, RuntimeError::Trap(_)), "{err}");
        assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(0));
        assert!(
            w.rt.history(&c.doc)
                .unwrap()
                .iter()
                .all(|n| n.action != "bad"),
            "a rolled back action leaves no change"
        );
        w.inc(&c);
        assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(1));
    }

    #[test]
    fn runaway_code_runs_out_of_fuel_and_the_runtime_carries_on() {
        let w = World::with(
            &AmStore::new(),
            EngineLimits {
                fuel_per_call: 200_000,
                max_memory_bytes: 1 << 24,
            },
        );
        let c = w.counter();
        let err =
            w.rt.invoke_action(&w.me, ("spin", vec![Value::resource(&c)]))
                .unwrap_err();
        assert!(err.to_string().contains("fuel"), "{err}");
        w.inc(&c);
        assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(1));
    }

    #[test]
    fn memory_growth_past_the_limit_traps() {
        let w = World::new();
        let c = w.counter();
        let err =
            w.rt.invoke_action(&w.me, ("grow", vec![Value::resource(&c)]))
                .unwrap_err();
        assert!(matches!(err, RuntimeError::Trap(_)), "{err}");
    }

    #[test]
    fn a_handle_the_host_never_issued_traps() {
        let w = World::new();
        let c = w.counter();
        let err =
            w.rt.invoke_action(&w.me, ("forge", vec![Value::resource(&c)]))
                .unwrap_err();
        assert!(err.to_string().contains("invalid handle"), "{err}");
    }

    #[test]
    fn a_view_cannot_mutate() {
        let w = World::new();
        let c = w.counter();
        let err =
            w.rt.open_view(&w.me, "vmut", vec![Value::resource(&c)])
                .err()
                .unwrap();
        assert!(err.to_string().contains("inside an action"), "{err}");
        assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(0));
    }

    #[test]
    fn an_action_may_only_change_its_own_modules_types() {
        let w = World::new();
        let other = Wat::new()
            .lit("fv", "fv")
            .func(
                r#"(func (export "a_poke") (param $c i32)
  (call $tl_res_add_i (local.get $c) (call $L_fv) (i64.const 1)))"#,
            )
            .build();
        w.rt.load_package(
            &other,
            meta(json!({
                "app": "x.other",
                "actions": [{"id": "poke", "name": "poke", "class": "undoable",
                             "params": [{"name": "c", "type": "Counter"}]}],
            })),
        )
        .unwrap();
        let c = w.counter();
        let err =
            w.rt.invoke_action(&w.me, ("poke", vec![Value::resource(&c)]))
                .unwrap_err();
        assert!(err.to_string().contains("own module"), "{err}");
        assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(0));
    }

    #[test]
    fn arguments_are_checked_against_the_declared_types() {
        let w = World::new();
        let err =
            w.rt.invoke_action(&w.me, ("inc", vec![Value::Int(3)]))
                .unwrap_err();
        assert!(matches!(err, RuntimeError::BadArgs(_)), "{err}");
        let err = w.rt.invoke_action(&w.me, ("inc", vec![])).unwrap_err();
        assert!(matches!(err, RuntimeError::BadArgs(_)), "{err}");
        let err = w.rt.invoke_action(&w.me, ("nope", vec![])).unwrap_err();
        assert!(matches!(err, RuntimeError::NotFound(_)), "{err}");
    }

    #[test]
    fn a_module_that_misses_a_declared_export_is_refused() {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        let wasm = Wat::new().func(r#"(func (export "a_x"))"#).build();
        let err = rt
            .load_package(
                &wasm,
                meta(json!({"app": "x.y", "actions": [
                {"id": "x", "name": "x", "class": "read"},
                {"id": "y", "name": "y", "class": "read"}]})),
            )
            .unwrap_err();
        assert!(err.to_string().contains("a_y"), "{err}");
    }

    #[test]
    fn the_catalogue_and_the_projection_are_built_from_the_metadata() {
        let w = World::new();
        let c = w.counter();
        w.inc(&c);
        w.inc(&c);

        let sig =
            w.rt.catalogue()
                .into_iter()
                .find(|s| s.kind == SignatureKind::Action && s.name == "increment")
                .unwrap();
        assert_eq!(sig.text, "action so.tilo.counter.increment(c: Counter)");
        assert_eq!(
            sig.render(),
            "/// Add one to the counter.\naction so.tilo.counter.increment(c: Counter)"
        );

        let p = w.rt.project(&Value::resource(&c));
        assert_eq!(p["type"], json!("Counter"));
        assert_eq!(p["fields"]["value"], json!(2));
        let many = Value::List((0..50).map(Value::Int).collect());
        let p = w.rt.project(&many);
        assert_eq!(p["count"], json!(50));
        assert_eq!(p["items"].as_array().unwrap().len(), 20);
        assert_eq!(p["truncated"], json!(true));
    }

    #[test]
    fn a_view_reruns_only_when_a_space_value_it_read_changes() {
        use tilolang_runtime::Space;
        let w = World::new();
        let c = w.counter();
        let mut v =
            w.rt.open_view(&w.me, "vspace", vec![Value::resource(&c)])
                .unwrap();
        let root = v.tree().clone();
        assert_eq!(
            root.props["min.rem"],
            Value::Float(12.5),
            "unit-tagged names pass through"
        );
        assert_eq!(
            root.props["gap"],
            Value::text("section"),
            "token props pass through"
        );
        assert_eq!(root.props["compact"], Value::Int(0));

        assert!(v.set_space("", Space::from_rem(20.0, 30.0)));
        assert_eq!(
            v.poll_patches().unwrap(),
            vec![Patch::SetProp {
                path: vec!["col".into()],
                name: "compact".into(),
                value: Some(Value::Int(1)),
            }]
        );
        assert!(
            !v.set_space("", Space::from_rem(25.0, 30.0)),
            "same class: no re-run"
        );
        assert!(v.poll_patches().unwrap().is_empty());
        assert!(
            !v.set_space("some.nested", Space::from_rem(90.0, 30.0)),
            "v1 inherits the root's"
        );

        let mut plain =
            w.rt.open_view(&w.me, "vc", vec![Value::resource(&c)])
                .unwrap();
        assert!(
            !plain.set_space("", Space::from_rem(5.0, 5.0)),
            "a view that never read space stays"
        );
        assert!(plain.poll_patches().unwrap().is_empty());
    }
}
