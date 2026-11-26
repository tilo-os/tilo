//! A record a field holds: `Clocks` made without a `Stopwatch` holds one of its own, built from
//! the stopwatch's defaults, in the clocks' own document. It reads as it would start before
//! anyone made the root, the same on every device that made it, and to every member who reads
//! the clocks.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::ids::MemberId;
    use tilolang_runtime::{Actor, AmStore, ResourceRef, Runtime, Value};

    const CLOCKS: &str = r#"app so.tilo.clocks "Clocks"

type Stopwatch {
    elapsed: Int = 0
    laps: List<Int>
}

type Clocks {
    stopwatch: Stopwatch
}

root Clocks

type Shelf {
    clocks: Collection<Clocks>
}

action shelve(s: Shelf) -> Clocks {
    s.clocks.insert(Clocks())
}

action unshelve(s: Shelf, c: Clocks) {
    s.clocks.remove(c)
}

default view clocks for Clocks

action lap(c: Clocks, at: Int) {
    c.stopwatch.elapsed = at
}

view clocks(c: Clocks) {
    ui.Text("{c.stopwatch.elapsed}")
}
"#;

    fn device(store: &AmStore, app: &App) -> Runtime {
        let rt = Runtime::new(store.clone(), TestAuthority::new()).unwrap();
        app.load(&rt);
        rt
    }

    /// What `who` sees of the clocks on `rt`.
    fn shown(rt: &Runtime, app: &App, who: &Actor, clocks: &ResourceRef) -> String {
        let view = rt
            .open_view(who, &app.view("clocks"), vec![Value::resource(clocks)])
            .unwrap_or_else(|e| panic!("the clocks open: {e}"));
        text_prop(of_kind(view.tree(), "ui.Text")[0], "value")
    }

    fn stopwatch(rt: &Runtime, clocks: &ResourceRef) -> ResourceRef {
        match rt.read(clocks, "stopwatch").unwrap() {
            Value::Ref(r) => (*r.target).clone(),
            other => panic!("the clocks hold a stopwatch, not {other:?}"),
        }
    }

    tilo_test_env::scenario! {
        fn a_record_field_starts_as_a_record_of_its_own;

        phase the_stopwatch_reads_as_it_starts_on_two_devices_and_to_another_member;
        {
            let app = App::compile(CLOCKS, &[]);
            let (mut store_a, mut store_b) = (AmStore::new(), AmStore::new());
            let (a, b) = (device(&store_a, &app), device(&store_b, &app));
            let bo = Actor::from(MemberId::person("bo"));
            let ty = app.compiled.ids.get("Clocks").unwrap().to_owned();
            let unmade = ResourceRef::root(format!("root-{ty}"), ty);

            // Nobody made the root yet: it reads as it would start, to each member.
            assert_eq!(a.read(&stopwatch(&a, &unmade), "elapsed").unwrap(), Value::Int(0));
            assert_eq!(shown(&b, &app, &bo, &unmade), "0");

            // Both devices make the root on their own; the stopwatch each makes is the same one.
            let made_a = a.create_root("Clocks").unwrap();
            let made_b = b.create_root("Clocks").unwrap();
            assert_eq!(made_a, unmade, "the root is where it was read");
            assert_eq!(stopwatch(&a, &made_a), stopwatch(&a, &unmade), "it holds what it read");
            assert_eq!(a.read(&stopwatch(&a, &made_a), "elapsed").unwrap(), Value::Int(0));
            assert_eq!(b.read(&stopwatch(&b, &made_b), "laps").unwrap(), Value::List(Vec::new()));
            store_a.sync_with(&mut store_b).unwrap();
            assert_eq!(stopwatch(&a, &made_a), stopwatch(&b, &made_b), "one stopwatch");
            assert_eq!(shown(&b, &app, &bo, &made_b), "0", "another member reads it too");

            // What one device writes to it reaches the other member on the other device.
            a.invoke_action(
                &me(),
                (app.action("lap").as_str(), vec![Value::resource(&made_a), Value::Int(12)]),
            )
            .unwrap();
            store_a.sync_with(&mut store_b).unwrap();
            assert_eq!(shown(&b, &app, &bo, &made_b), "12");
            let watch = stopwatch(&b, &made_b);
            assert_eq!(watch.doc, made_b.doc, "the stopwatch is kept in the clocks' document");
        }

        phase an_item_made_without_its_record_holds_one_and_takes_it_when_removed;
        {
            let app = App::compile(CLOCKS, &[]);
            let rt = device(&AmStore::new(), &app);
            let shelf = rt.create("Shelf").unwrap();
            let call = |action: &str, args: Vec<Value>| {
                rt.invoke_action(&me(), (app.action(action).as_str(), args))
                    .unwrap_or_else(|e| panic!("{action}: {e}"))
            };
            let clocks = returned_ref(call("shelve", vec![Value::resource(&shelf)]));
            let watch = stopwatch(&rt, &clocks);
            assert_eq!(rt.read(&watch, "elapsed").unwrap(), Value::Int(0));
            call("lap", vec![Value::resource(&clocks), Value::Int(7)]);
            assert_eq!(rt.read(&watch, "elapsed").unwrap(), Value::Int(7));
            call(
                "unshelve",
                vec![Value::resource(&shelf), Value::resource(&clocks)],
            );
            assert!(
                rt.read(&watch, "elapsed").is_err(),
                "the stopwatch goes with the clocks that hold it"
            );
        }
    }
}
