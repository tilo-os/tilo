//! What a resource was made from, and what links to it: `created(r)?.from` and `linked(r)`.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::rich::RichValue;
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, Value};

    const SOURCE: &str = include_str!("../../tilolang/tests/fixtures/links.tl");

    struct Links {
        rt: Runtime,
        app: App,
        board: ResourceRef,
    }

    impl Links {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            let board = rt.create("Board").unwrap();
            Self { rt, app, board }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        fn call_from(&self, action: &str, args: Vec<Value>, from: &ResourceRef) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action_from(&me(), (id.as_str(), args), from)
                .unwrap()
        }

        fn board_arg(&self) -> Value {
            Value::resource(&self.board)
        }

        fn add_note(&self, title: &str) -> ResourceRef {
            returned_ref(self.call("add_note", vec![self.board_arg(), Value::text(title)]))
        }

        fn note_about(&self, source: &ResourceRef, title: &str) -> ResourceRef {
            returned_ref(self.call(
                "note_about",
                vec![
                    self.board_arg(),
                    Value::resource(source),
                    Value::text(title),
                ],
            ))
        }

        fn made_from(&self, note: &ResourceRef) -> String {
            text_of(self.call("made_from", vec![Value::resource(note)]))
        }

        fn links_of(&self, note: &ResourceRef) -> Vec<String> {
            texts_of(self.call("links_of", vec![Value::resource(note)]))
        }
    }

    fn text_of(out: InvokeOutcome) -> String {
        match out {
            InvokeOutcome::Done(Value::Text(text)) => text,
            other => panic!("not a text: {other:?}"),
        }
    }

    fn texts_of(out: InvokeOutcome) -> Vec<String> {
        match out {
            InvokeOutcome::Done(Value::List(items)) => items
                .into_iter()
                .map(|item| match item {
                    Value::Text(text) => text,
                    other => panic!("not a text: {other:?}"),
                })
                .collect(),
            other => panic!("not a list: {other:?}"),
        }
    }

    fn rich(text: &str) -> Value {
        Value::Rich(RichValue::from_text(text))
    }

    #[test]
    fn made_from_is_recorded_with_the_change() {
        let l = Links::new();
        let source = l.add_note("A");
        let made =
            returned_ref(l.call_from("add_note", vec![l.board_arg(), Value::text("B")], &source));
        assert_eq!(l.made_from(&made), "A");
        // One made on its own was made from nothing.
        let alone = l.add_note("C");
        assert_eq!(l.made_from(&alone), "");
    }

    #[test]
    fn links_are_made_from_mentioned_and_referred() {
        let l = Links::new();
        let source = l.add_note("A");
        l.call_from("add_note", vec![l.board_arg(), Value::text("B")], &source);
        // An item whose field names the note.
        returned_ref(l.call(
            "item_about",
            vec![l.board_arg(), Value::resource(&source), Value::text("I")],
        ));
        // A note whose text mentions it.
        let mention = l.note_about(&source, "M");
        assert_eq!(
            l.links_of(&source),
            vec![
                "created_from B".to_owned(),
                "field I".to_owned(),
                "mention M".to_owned(),
            ]
        );
        // What links to the source links to nothing itself.
        assert!(l.links_of(&mention).is_empty());
    }

    #[test]
    fn links_follow_the_fields_that_make_them() {
        let l = Links::new();
        let source = l.add_note("A");
        l.call_from("add_note", vec![l.board_arg(), Value::text("B")], &source);
        let item = returned_ref(l.call(
            "item_about",
            vec![l.board_arg(), Value::resource(&source), Value::text("I")],
        ));
        let mention = l.note_about(&source, "M");
        assert_eq!(l.links_of(&source).len(), 3);

        // A field that no longer names it drops the link.
        l.call("clear_about", vec![Value::resource(&item)]);
        assert_eq!(
            l.links_of(&source),
            vec!["created_from B".to_owned(), "mention M".to_owned()]
        );

        // A note dropped while its text still mentions it leaves no link behind.
        l.call("drop_note", vec![l.board_arg(), Value::resource(&mention)]);
        assert_eq!(l.links_of(&source), vec!["created_from B".to_owned()]);

        // Text that no longer mentions it drops the link.
        let other = l.note_about(&source, "N");
        assert_eq!(l.links_of(&source).len(), 2);
        l.call(
            "rewrite",
            vec![Value::resource(&other), rich("nothing here")],
        );
        assert_eq!(l.links_of(&source), vec!["created_from B".to_owned()]);
    }
}
