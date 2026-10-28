//! `RichText`: tilo-doc's rich text as a tilolang type, with references to people and
//! resources, merged by splices and marks.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::rich::{Reference, RichValue};
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, Value};

    const SOURCE: &str = include_str!("../../tilolang/tests/fixtures/richtext.tl");

    struct Room {
        rt: Runtime,
        app: App,
        board: ResourceRef,
    }

    impl Room {
        fn on(store: &AmStore, board: Option<&ResourceRef>) -> Self {
            // Every replica runs the same compiled package, as every device does.
            thread_local! {
                static COMPILED: tilolang::Compiled = App::compile(SOURCE, &[]).compiled;
            }
            let app = App {
                compiled: COMPILED.with(Clone::clone),
            };
            let rt = Runtime::new(store.clone(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            let board = board
                .cloned()
                .unwrap_or_else(|| rt.create("Board").unwrap());
            Self { rt, app, board }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        fn done(&self, action: &str, args: Vec<Value>) -> Value {
            match self.call(action, args) {
                InvokeOutcome::Done(v) => v,
                other => panic!("{action}: {other:?}"),
            }
        }

        fn task(&self, title: &str) -> ResourceRef {
            returned_ref(self.call(
                "add_task",
                vec![Value::resource(&self.board), Value::text(title)],
            ))
        }

        fn say(&self, text: &str) -> ResourceRef {
            returned_ref(self.call("say", vec![Value::resource(&self.board), Value::text(text)]))
        }

        fn body(&self, post: &ResourceRef) -> RichValue {
            match self.rt.read(post, "body").unwrap() {
                Value::Rich(r) => r,
                other => panic!("body reads as {other:?}"),
            }
        }

        fn int(&self, action: &str, post: &ResourceRef) -> i64 {
            match self.done(action, vec![Value::resource(post)]) {
                Value::Int(i) => i,
                Value::Opt(Some(v)) => v.as_int().unwrap(),
                other => panic!("{action} answered {other:?}"),
            }
        }
    }

    #[test]
    fn plain_text_is_paragraphs_and_the_excerpt_is_its_first_line_with_text() {
        let b = Room::on(&AmStore::new(), None);
        let post = b.say("\nHello there\nsecond line");
        assert_eq!(b.body(&post).plain_text(), "\nHello there\nsecond line");
        assert_eq!(
            b.done("excerpt_of", vec![Value::resource(&post)]),
            Value::text("Hello there")
        );
        assert_eq!(b.int("task_count", &post), 0);
    }

    #[test]
    fn an_empty_message_is_not_posted() {
        let b = Room::on(&AmStore::new(), None);
        let count = || match b.done("post_count", vec![Value::resource(&b.board)]) {
            Value::Int(i) => i,
            Value::Opt(Some(v)) => v.as_int().unwrap(),
            other => panic!("{other:?}"),
        };
        let post = |text: &str| {
            b.call(
                "post",
                vec![
                    Value::resource(&b.board),
                    Value::Rich(RichValue::from_text(text)),
                ],
            );
        };
        post("  \n ");
        assert_eq!(count(), 0, "an empty message says nothing");
        post("hi");
        assert_eq!(count(), 1);
    }

    #[test]
    fn references_to_a_person_and_a_resource_are_marked_runs() {
        let b = Room::on(&AmStore::new(), None);
        let task = b.task("Fix the build");
        let post = returned_ref(b.call(
            "take",
            vec![Value::resource(&b.board), Value::resource(&task)],
        ));
        let body = b.body(&post);
        assert_eq!(body.plain_text(), "@me will look at Fix the build.");
        let refs = body.references();
        assert_eq!(refs.len(), 2, "{refs:?}");
        assert_eq!(refs[0], Reference::Person("me".into()));
        let Some(at) = refs[1].as_resource() else {
            panic!("{refs:?}")
        };
        assert_eq!(
            (
                at.app.as_str(),
                at.reference.kind.as_str(),
                at.reference.id.as_str()
            ),
            (
                "so.tilo.lang.richtext",
                "tl.so.tilo.lang.richtext.Task",
                format!("tl:so.tilo.lang.richtext:{}", task.id_string()).as_str()
            ),
            "a resource is named by the catalogue's reference to it"
        );
        assert_eq!(
            tilolang_runtime::rich::resource_id(&refs[1]),
            Some(task.id_string().as_str())
        );
        // The marks are tilo-doc's `mention` marks over the runs.
        let block = &body.blocks[0];
        let runs: Vec<&str> = block
            .marks
            .iter()
            .filter(|m| m.name == "mention")
            .map(|m| {
                let start = block.text.char_indices().nth(m.start).map_or(0, |(i, _)| i);
                let end = block
                    .text
                    .char_indices()
                    .nth(m.end)
                    .map_or(block.text.len(), |(i, _)| i);
                block.text.get(start..end).unwrap_or_default()
            })
            .collect();
        assert_eq!(runs, ["@me", "Fix the build"]);
        assert_eq!(b.int("people_count", &post), 1);
        assert_eq!(b.int("task_count", &post), 1);
    }

    #[test]
    fn text_stored_with_an_older_reference_form_still_refers_to_its_resource() {
        let b = Room::on(&AmStore::new(), None);
        let task = b.task("Fix the build");
        let post = b.say("placeholder");
        // As the first rich text wrote a resource: its published id as the mark's value.
        let mut old = RichValue::from_text("see ");
        old.blocks[0].text.push_str("Fix the build");
        old.blocks[0].marks.push(tilo_doc::document::MarkSpan {
            start: 4,
            end: 17,
            name: "mention".into(),
            value: tilo_doc::document::MarkValue::Str(format!(
                "tl:so.tilo.lang.richtext:{}",
                task.id_string()
            )),
        });
        b.call("edit", vec![Value::resource(&post), Value::Rich(old)]);
        let refs = b.body(&post).references();
        assert_eq!(refs.len(), 1, "{refs:?}");
        assert_eq!(
            tilolang_runtime::rich::resource_id(&refs[0]),
            Some(task.id_string().as_str())
        );
        assert_eq!(b.int("task_count", &post), 1, "it still reads as the task");
    }

    #[test]
    fn two_writers_edits_with_references_merge_as_splices_and_marks() {
        let (mut sa, mut sb) = (AmStore::new(), AmStore::new());
        let a = Room::on(&sa, None);
        let task = a.task("Ship it");
        let post = a.say("Plan for today");
        sa.sync_with(&mut sb).unwrap();
        let b = Room::on(&sb, Some(&a.board));
        let first_id = a.body(&post).blocks[0].id.clone();
        assert!(!first_id.is_empty());
        assert_eq!(b.body(&post).plain_text(), "Plan for today");

        // At once: one writer adds a line, the other a reference in the first one.
        a.call(
            "append",
            vec![Value::resource(&post), Value::text("Then lunch")],
        );
        b.call(
            "refer",
            vec![Value::resource(&post), Value::resource(&task)],
        );
        sa.sync_with(&mut sb).unwrap();

        let (ra, rb) = (a.body(&post), b.body(&post));
        assert_eq!(ra, rb, "both replicas read the same");
        assert_eq!(ra.plain_text(), "Plan for today see Ship it\nThen lunch");
        assert_eq!(
            ra.blocks[0].id, first_id,
            "the paragraph was edited in place"
        );
        assert_eq!(ra.references().len(), 1, "the reference survived the merge");
        assert_eq!(a.int("task_count", &post), 1);
    }

    #[test]
    fn concurrent_typing_in_one_paragraph_keeps_both() {
        let (mut sa, mut sb) = (AmStore::new(), AmStore::new());
        let a = Room::on(&sa, None);
        let post = a.say("Hello");
        sa.sync_with(&mut sb).unwrap();
        let b = Room::on(&sb, Some(&a.board));
        let edit = |w: &Room, text: &str| {
            w.call(
                "edit",
                vec![
                    Value::resource(&post),
                    Value::Rich(RichValue::from_text(text)),
                ],
            );
        };
        edit(&a, "Hello world");
        edit(&b, "Big Hello");
        sa.sync_with(&mut sb).unwrap();
        assert_eq!(a.body(&post), b.body(&post));
        assert_eq!(a.body(&post).plain_text(), "Big Hello world");
    }

    #[test]
    fn anchors_resolve_to_the_paragraphs_they_cite() {
        let b = Room::on(&AmStore::new(), None);
        let post = b.say("Intro\nDetails\nOutro");
        let body = b.body(&post);
        assert_eq!(
            body.at_anchor("¶2").map(|p| p.text.as_str()),
            Some("Details")
        );
        let id = &body.blocks[2].id;
        assert_eq!(body.anchor_of(id).as_deref(), Some("¶3"));
    }
}
