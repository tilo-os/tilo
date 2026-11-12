//! What a resource was made from, and what links to it: `created(r)?.from` and `linked(r)`.

#[cfg(test)]
mod cases {
    use crate::common::*;
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
            let board = rt.create_root("Board").unwrap();
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

        /// The titles `action` answers for the links of `note`.
        fn titles_of(&self, action: &str, note: &ResourceRef) -> Vec<String> {
            texts_of(self.call(action, vec![Value::resource(note)]))
        }

        /// Runs the scenario `name`, which answers the resource it names as its result.
        fn scenario(&self, name: &str) -> ResourceRef {
            let action = self
                .app
                .meta()
                .scenarios
                .iter()
                .find(|scenario| scenario.name == name)
                .unwrap_or_else(|| panic!("no scenario {name}"))
                .action
                .clone();
            returned_ref(
                self.rt
                    .invoke_action(&me(), (action.as_str(), vec![]))
                    .unwrap(),
            )
        }

        fn titled(&self, title: &str) -> ResourceRef {
            let out = self.call("note_titled", vec![self.board_arg(), Value::text(title)]);
            match out {
                InvokeOutcome::Done(Value::Opt(Some(inner))) => match *inner {
                    Value::Ref(r) => std::rc::Rc::unwrap_or_clone(r.target),
                    other => panic!("not a note: {other:?}"),
                },
                other => panic!("not a note: {other:?}"),
            }
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

    /// A link says how it links as a case of `host.LinkHow`, so code compares it with
    /// `.field` or `.mention` and never with a word.
    #[test]
    fn a_link_says_how_as_a_case_of_the_hosts_enum() {
        let l = Links::new();
        let source = l.add_note("A");
        l.call_from("add_note", vec![l.board_arg(), Value::text("B")], &source);
        // An item whose field names the note.
        returned_ref(l.call(
            "item_about",
            vec![l.board_arg(), Value::resource(&source), Value::text("I")],
        ));
        // A note whose text mentions it.
        l.note_about(&source, "M");
        assert_eq!(l.titles_of("naming", &source), vec!["I".to_owned()]);
        assert_eq!(l.titles_of("mentioning", &source), vec!["M".to_owned()]);
        // A note made from it names it in no field and mentions it nowhere.
        assert!(l.titles_of("naming", &l.titled("B")).is_empty());
        assert!(l.titles_of("mentioning", &l.titled("B")).is_empty());
    }

    /// The word the index keeps a link's how under is the case `host.LinkHow` names, so a link
    /// read from the index is the case code compares.
    #[test]
    fn a_links_how_word_is_the_case_it_is_read_as() {
        use tilolang_runtime::store::LinkHow;

        let meta = tilolang::host_meta();
        let declared = meta
            .enums
            .iter()
            .find(|e| e.id == tilolang::LINK_HOW_TYPE)
            .expect("the host declares LinkHow");
        assert_eq!(
            declared
                .variants
                .iter()
                .map(|v| v.id.as_str())
                .collect::<Vec<_>>(),
            vec!["created_from", "field", "mention"]
        );
        for how in [LinkHow::CreatedFrom, LinkHow::Field, LinkHow::Mention] {
            assert!(
                declared.variants.iter().any(|v| v.id == how.word()),
                "`{}` is a case of the host's `LinkHow`",
                how.word()
            );
        }
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

    /// A scenario makes a resource from another through the same path an invocation made from it
    /// takes: the change it makes records what it made from, and what links to the source names
    /// the resource made from it.
    #[test]
    fn a_scenario_makes_a_resource_from_another() {
        let l = Links::new();
        let made = l.scenario("a note made from another");
        assert_eq!(l.made_from(&made), "A");
        // What links to the source: the note made from it, and nothing it says itself.
        let source = l.titled("A");
        assert_eq!(l.made_from(&source), "");
        assert_eq!(l.links_of(&source), vec!["created_from B".to_owned()]);
        assert!(l.links_of(&made).is_empty());
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

    /// A page applies its action from the resource it shows: the button it draws carries the
    /// `.made_from(n)` the view wrote, so what a press makes is made from `n` — the same record
    /// a scenario's `made from` leaves, and the same thread a comment on a task joins.
    #[test]
    fn an_action_a_page_applies_from_its_resource_is_made_from_it() {
        let l = Links::new();
        let page = l.add_note("A");
        let view =
            l.rt.open_view(
                &me(),
                &l.app.view("note_page"),
                vec![Value::resource(&page)],
            )
            .unwrap();
        let buttons = of_kind(view.tree(), "ui.Button");
        let button = buttons.first().expect("the page draws its button");
        assert!(matches!(
            view.press(&button.key).unwrap(),
            InvokeOutcome::Done(_)
        ));
        let made = l.titled("under");
        assert_eq!(l.made_from(&made), "A");
        assert_eq!(l.links_of(&page), vec!["created_from under".to_owned()]);
    }
}
