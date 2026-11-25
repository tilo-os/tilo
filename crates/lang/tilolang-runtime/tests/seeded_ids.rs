//! What a root starts with is the same records on every replica that makes it: a device that
//! shows its own making of a person's root names the records the workspace's making holds, so an
//! action it hands the workspace about one of them finds it there.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, Value};

    const SOURCE: &str = r#"app so.tilo.seeded "Seeded"

type Bar {
    parts: Collection<Part>
}

type Part {
    name: Text
}

initial action seed(b: Bar) {
    b.parts.insert(Part(name: "menu"))
    b.parts.insert(Part(name: "clock"))
}

action add(b: Bar, name: Text) {
    b.parts.insert(Part(name: name))
}
"#;

    fn parts(rt: &Runtime, bar: &ResourceRef) -> Vec<ResourceRef> {
        match rt.read(bar, "parts").unwrap() {
            Value::List(items) => items
                .into_iter()
                .map(|item| match item {
                    Value::Ref(r) => ResourceRef::clone(&r.target),
                    other => panic!("not a part: {other:?}"),
                })
                .collect(),
            other => panic!("not parts: {other:?}"),
        }
    }

    /// A device running `app`, the one package both devices installed, with the root made.
    fn device(app: &App) -> (Runtime, ResourceRef, String) {
        let authority = TestAuthority::new();
        let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
        authority.trust(&app.load(&rt));
        let bar = rt.create_root("Bar").unwrap();
        let add = app.action("add");
        (rt, bar, add)
    }

    #[test]
    fn two_makings_of_a_root_start_with_the_same_records() {
        let app = App::compile(SOURCE, &[]);
        let (laptop, bar, add) = device(&app);
        let (phone, theirs, _) = device(&app);
        let made = parts(&laptop, &bar);
        assert_eq!(made.len(), 2, "the initial action's parts");
        assert_eq!(
            made,
            parts(&phone, &theirs),
            "both makings name the same parts"
        );

        // What a person adds later is theirs alone: two devices adding one each add two.
        for (rt, at) in [(&laptop, &bar), (&phone, &theirs)] {
            rt.invoke_action(
                &me(),
                (
                    add.as_str(),
                    vec![Value::resource(at), Value::text("people")],
                ),
            )
            .unwrap();
        }
        assert_ne!(
            parts(&laptop, &bar)[2],
            parts(&phone, &theirs)[2],
            "a later insert is named afresh"
        );
    }
}
