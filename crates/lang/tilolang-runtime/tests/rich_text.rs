//! `RichText`: tilo-doc's rich text as a tilolang type, with references to people and
//! resources, merged by splices and marks.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::rich::{Reference, RichValue};
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, Value};

    const SOURCE: &str = include_str!("../../tilolang/tests/fixtures/richtext.tl");

    struct Workspace {
        rt: Runtime,
        app: App,
        board: ResourceRef,
    }

    impl Workspace {
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

        fn say_text(&self, text: &str) -> ResourceRef {
            returned_ref(self.call(
                "say_text",
                vec![Value::resource(&self.board), Value::text(text)],
            ))
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
        let b = Workspace::on(&AmStore::new(), None);
        let post = b.say_text("\nHello there\nsecond line");
        assert_eq!(b.body(&post).written_text(), "\nHello there\nsecond line");
        assert_eq!(
            b.done("excerpt_of", vec![Value::resource(&post)]),
            Value::text("Hello there")
        );
        assert_eq!(b.int("task_count", &post), 0);
    }

    #[test]
    fn a_store_that_cannot_tell_what_its_reader_holds_reads_no_title() {
        // The bare store does not know who reads: the excerpt names the person as written and
        // the task only as being there, never by the title the text was written with.
        let b = Workspace::on(&AmStore::new(), None);
        let task = b.task("Fix the build");
        let post = returned_ref(b.call(
            "take",
            vec![Value::resource(&b.board), Value::resource(&task)],
        ));
        assert_eq!(
            b.done("excerpt_of", vec![Value::resource(&post)]),
            Value::text("@me will look at Not shared with you.")
        );
    }

    #[test]
    fn an_empty_message_is_not_posted() {
        let b = Workspace::on(&AmStore::new(), None);
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
        let b = Workspace::on(&AmStore::new(), None);
        let task = b.task("Fix the build");
        let post = returned_ref(b.call(
            "take",
            vec![Value::resource(&b.board), Value::resource(&task)],
        ));
        let body = b.body(&post);
        assert_eq!(body.written_text(), "@me will look at Fix the build.");
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
        let b = Workspace::on(&AmStore::new(), None);
        let task = b.task("Fix the build");
        let post = b.say_text("placeholder");
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
    fn a_time_in_rich_text_is_a_run_marked_with_the_moment() {
        let b = Workspace::on(&AmStore::new(), None);
        let post = returned_ref(b.call(
            "schedule",
            vec![
                Value::resource(&b.board),
                Value::text("2026-10-29T09:30:00Z"),
            ],
        ));
        let body = b.body(&post);
        assert_eq!(body.written_text(), "Standup at 2026-10-29T09:30:00Z");
        let block = &body.blocks[0];
        let time = block
            .marks
            .iter()
            .find(|m| m.name == tilo_doc::inline_time::MARK)
            .expect("a time mark");
        assert_eq!((time.start, time.end), (11, 31));
        let tilo_doc::document::MarkValue::Str(value) = &time.value else {
            panic!("{time:?}")
        };
        assert_eq!(
            tilo_doc::inline_time::InlineTime::of_mark(value).map(|t| t.at),
            Some("2026-10-29T09:30:00Z".to_owned())
        );
    }

    #[test]
    fn two_writers_edits_with_references_merge_as_splices_and_marks() {
        let (mut sa, mut sb) = (AmStore::new(), AmStore::new());
        let a = Workspace::on(&sa, None);
        let task = a.task("Ship it");
        let post = a.say_text("Plan for today");
        sa.sync_with(&mut sb).unwrap();
        let b = Workspace::on(&sb, Some(&a.board));
        let first_id = a.body(&post).blocks[0].id.clone();
        assert!(!first_id.is_empty());
        assert_eq!(b.body(&post).written_text(), "Plan for today");

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
        assert_eq!(ra.written_text(), "Plan for today see Ship it\nThen lunch");
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
        let a = Workspace::on(&sa, None);
        let post = a.say_text("Hello");
        sa.sync_with(&mut sb).unwrap();
        let b = Workspace::on(&sb, Some(&a.board));
        let edit = |w: &Workspace, text: &str| {
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
        assert_eq!(a.body(&post).written_text(), "Big Hello world");
    }

    #[test]
    fn a_markdown_document_is_a_rich_text_and_back() {
        let b = Workspace::on(&AmStore::new(), None);
        let markdown = concat!(
            "# Groceries\n",
            "\n",
            "- [ ] oat milk\n",
            "- [x] eggs\n",
            "\n",
            "Text with **bold**, `code` and [a link](https://example.test).\n",
            "\n",
            "```rust\n",
            "fn main() {}\n",
            "```\n",
            "\n",
            "| a | b |\n",
            "| --- | --- |\n",
            "| 1 | 2 |\n",
            "\n",
            "![a fox](tilo-blob://fox)\n",
            "\n",
            "> remember the bags\n",
            "\n",
            "---\n",
        );
        let post = returned_ref(b.call(
            "import_body",
            vec![Value::resource(&b.board), Value::text(markdown)],
        ));
        let body = b.body(&post);
        let kinds: Vec<&str> = body
            .blocks
            .iter()
            .map(|block| block.kind.as_str())
            .collect();
        assert_eq!(
            kinds,
            [
                "heading",
                "list_item",
                "list_item",
                "paragraph",
                "paragraph",
                "paragraph",
                "paragraph",
                "quote",
                "divider",
            ],
            "a fence, a table and a picture are paragraphs: {kinds:?}"
        );
        assert!(
            body.blocks[3]
                .marks
                .iter()
                .any(|mark| mark.name == "strong"),
            "bold is a mark: {:?}",
            body.blocks[3].marks
        );
        assert_eq!(
            b.done("markdown_of", vec![Value::resource(&post)],)
                .as_text(),
            Some(markdown),
            "the document writes as the Markdown it was read from"
        );
    }

    #[test]
    fn a_paragraph_projecting_a_resource_is_a_block_of_its_own_under_the_text() {
        use tilolang_runtime::Store as _;

        let store = AmStore::new();
        let b = Workspace::on(&store, None);
        let post = b.say_text("Standup");
        let task = b.task("Fix the build");
        b.done(
            "embed",
            vec![Value::resource(&post), Value::resource(&task)],
        );
        let body = b.body(&post);
        assert_eq!(
            body.blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>(),
            ["Standup", "Fix the build"],
            "the post says what it said, and the card it projects stands under it"
        );
        let card = body.blocks.get(1).expect("the card");
        assert!(
            card.marks.iter().any(|mark| {
                mark.name == tilolang_runtime::rich::PROJECTION
                    && mark.start == 0
                    && mark.end == card.text.chars().count()
            }),
            "the whole paragraph projects it: {:?}",
            card.marks
        );
        assert_eq!(
            body.projections()
                .iter()
                .filter(|projection| projection.as_resource().is_some())
                .count(),
            1,
            "what the post projects is the task: {body:?}"
        );
        // What links to the task says so: the post projects it.
        let linked = store.linked(&task).expect("the task's links");
        assert!(
            linked
                .iter()
                .any(|link| link.how == tilolang_runtime::store::LinkHow::Mention),
            "the post that embeds it, and how: {linked:?}"
        );
    }

    #[test]
    fn a_line_break_before_a_card_opens_its_place_and_keeps_an_empty_line_a_person_left() {
        let b = Workspace::on(&AmStore::new(), None);
        let task = b.task("Fix the build");
        let texts = |post: &ResourceRef| {
            b.body(post)
                .blocks
                .iter()
                .map(|block| block.text.clone())
                .collect::<Vec<String>>()
        };
        let post = returned_ref(b.call(
            "embed_under_line",
            vec![Value::resource(&b.board), Value::resource(&task)],
        ));
        assert_eq!(
            texts(&post),
            ["What to take to the market:", "Fix the build"],
            "the break ends the paragraph, so the card stands where the next line starts"
        );
        // A card of the task, and nothing between it and the line it stands under.
        let body = b.body(&post);
        assert_eq!(body.blocks.len(), 2, "no empty paragraph: {body:?}");
        assert!(
            !body.blocks[1].id.is_empty(),
            "the card is stored as a block"
        );
        assert_eq!(
            body.projections().len(),
            1,
            "the card projects the task: {body:?}"
        );
        let blank = returned_ref(b.call(
            "embed_after_blank_line",
            vec![Value::resource(&b.board), Value::resource(&task)],
        ));
        assert_eq!(
            texts(&blank),
            ["What to take to the market:", "", "Fix the build"],
            "an empty line a person typed stands, and the card comes under it"
        );
    }

    #[test]
    fn anchors_resolve_to_the_paragraphs_they_cite() {
        let b = Workspace::on(&AmStore::new(), None);
        let post = b.say_text("Intro\nDetails\nOutro");
        let body = b.body(&post);
        assert_eq!(
            body.at_anchor("¶2").map(|p| p.text.as_str()),
            Some("Details")
        );
        let id = &body.blocks[2].id;
        assert_eq!(body.anchor_of(id).as_deref(), Some("¶3"));
    }

    #[test]
    fn a_page_names_the_page_it_links_to_by_the_path_it_stands_at() {
        let b = Workspace::on(&AmStore::new(), None);
        let task = b.task("Fix the build");
        let post = returned_ref(b.call(
            "take",
            vec![Value::resource(&b.board), Value::resource(&task)],
        ));
        let markdown = match b.done(
            "linked_markdown",
            vec![Value::resource(&post), Value::text("Under/")],
        ) {
            Value::Text(text) => text,
            other => panic!("an export: {other:?}"),
        };
        assert!(
            markdown.contains("[Fix the build](<Under/Task 1.md>)"),
            "a reference to a task is the link to the page the code named for it: {markdown}"
        );
        assert!(
            markdown.contains("@me will look at "),
            "a person is no page of the bundle, and reads as the run it is: {markdown}"
        );
    }

    #[test]
    fn a_link_to_a_page_of_the_bundle_reads_as_the_reference_to_it() {
        let b = Workspace::on(&AmStore::new(), None);
        let task = b.task("Fix the build");
        let said = |markdown: &str| {
            returned_ref(b.call(
                "post_linked",
                vec![
                    Value::resource(&b.board),
                    Value::text(markdown),
                    Value::resource(&task),
                    Value::text("Under/Task 1.md"),
                ],
            ))
        };
        let made = said("See [Fix the build](<Under/Task 1.md>).");
        assert_eq!(b.int("task_count", &made), 1, "the link is the reference");
        let refs = b.body(&made).references();
        assert_eq!(
            tilolang_runtime::rich::resource_id(&refs[0]),
            Some(task.id_string().as_str())
        );
        // A link the bundle does not hold stays a link: a page names its own pages, not the web.
        let outside = said("See [the site](<https://example.test/>).");
        assert_eq!(b.int("task_count", &outside), 0);
        // A page alone in its paragraph is the card of it, which is how an embed is written.
        let card = said("[Fix the build](<Under/Task 1.md>)");
        let held = b.body(&card);
        assert_eq!(
            held.blocks[0]
                .marks
                .iter()
                .filter(|mark| mark.name == tilolang_runtime::rich::PROJECTION)
                .count(),
            1,
            "{:?}",
            held.blocks[0].marks
        );
    }

    #[test]
    fn a_map_the_code_fills_as_it_runs_answers_what_was_put_in_it() {
        let b = Workspace::on(&AmStore::new(), None);
        let task = b.task("Fix the build");
        let post = returned_ref(b.call(
            "take",
            vec![Value::resource(&b.board), Value::resource(&task)],
        ));
        let asked = |key: &str| b.done("page_of", vec![Value::resource(&post), Value::text(key)]);
        let found = match asked("Fix the build") {
            Value::Opt(Some(held)) => held.as_text().map(str::to_owned),
            other => panic!("a page: {other:?}"),
        };
        assert_eq!(
            found.as_deref(),
            Some(task.id_string().as_str()),
            "the map answers what it was put under the title"
        );
        assert!(
            matches!(asked("Nothing like it"), Value::Opt(None)),
            "a key no task was put under holds nothing"
        );
    }
}
