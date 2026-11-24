//! `record_at(r, version)`: a resource's own record as it stood at a version, read from the
//! store's log through the one record API, so a reader compares past states without keeping a
//! copy of them.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, InvokeOutcome, Runtime, SimpleAuthority, Value};

    const SOURCE: &str = r#"
app test.recoat "Recoat" (
    icon: .file,
    category: .utility,
    tagline: "What a record was",
)

type Note (title: body) {
    body: Text = ""
    tags: Collection<Tag>
}

type Tag (title: name) {
    name: Text
}

root notes: Note

default view note for Note

view note(n: Note) {
    ui.Text(n.body)
}

action set_body(n: Note, body: Text) (label: "Write") {
    n.body = body
}

action add_tag(n: Note, name: Text) -> Tag (label: "Tag") {
    n.tags.insert(Tag(name: name))
}

action read_at(n: Note, version: Text) -> Text (label: "Then") {
    record_at(n, version)
}
"#;

    struct World {
        rt: Runtime,
        package: App,
        note: tilolang_runtime::ResourceRef,
    }

    impl World {
        fn new() -> Self {
            let package = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            package.load(&rt);
            let note = rt.create_root("Note").unwrap();
            Self { rt, package, note }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.package.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        fn set_body(&self, body: &str) {
            self.call(
                "set_body",
                vec![Value::resource(&self.note), Value::text(body)],
            );
        }

        fn then(&self, version: &str) -> serde_json::Value {
            match self.call(
                "read_at",
                vec![Value::resource(&self.note), Value::text(version)],
            ) {
                InvokeOutcome::Done(Value::Text(text)) => serde_json::from_str(&text).unwrap(),
                other => panic!("a record, not {other:?}"),
            }
        }

        /// The change id of the version that last wrote `body`.
        fn body_version(&self) -> String {
            let write = self.package.action("set_body");
            let versions = self.rt.versions(&self.note).unwrap();
            versions
                .iter()
                .rev()
                .find(|version| version.action == write)
                .unwrap_or_else(|| panic!("a version wrote the body: {versions:?}"))
                .change
                .clone()
        }
    }

    #[test]
    fn a_record_reads_as_it_stood_at_a_version() {
        let app = World::new();
        app.set_body("first");
        let first = app.body_version();
        app.set_body("second");
        app.call(
            "add_tag",
            vec![Value::resource(&app.note), Value::text("draft")],
        );

        let past = app.then(&first);
        assert_eq!(
            past["fields"]["body"], "first",
            "the body as it stood: {past}"
        );
        assert_eq!(
            past["fields"]["tags"].as_array().map(Vec::len),
            Some(0),
            "a tag added later is not in the past record: {past}"
        );

        let now = app.rt.versions(&app.note).unwrap();
        let latest = now.last().expect("a version").change.clone();
        let current = app.then(&latest);
        assert_eq!(current["fields"]["body"], "second");
        let tags = current["fields"]["tags"].as_array().expect("the tags");
        assert_eq!(tags.len(), 1, "the tag is in the current record: {current}");
        assert_eq!(tags[0]["fields"]["name"], "draft");
        assert!(
            !tags[0]["id"].as_str().unwrap_or_default().is_empty(),
            "a collection item names its id: {current}"
        );
    }
}
