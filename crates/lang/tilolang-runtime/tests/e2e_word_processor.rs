//! The Writer (`apps/tl/word-processor`) end to end: a document started from a template, its
//! words, tables, contents, page header and footer, a file opened into it and written out, and a
//! comment anchored to the words it is about.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::rich::RichValue;
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, Value};

    const SOURCE: &str = concat!(
        include_str!("../../../../apps/tl/word-processor/app.tl"),
        "\n",
        include_str!("../../../../apps/tl/word-processor/views.tl"),
    );
    const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");

    /// The workspace package, as far as these apps need it: their `use` of the people a
    /// workspace has. A stand-in carries the name the import names.
    const WORKSPACE: &str = r#"app tilo.workspace "Workspace"

/// One person of the workspace.
type Member {
    name: Text
}

/// The workspace's members, as its apps read them.
type Members {
    people: Collection<Member>
}

root members: Members

default view members_page for Members

row view member_row for Member

view members_page(m: Members) {
    ui.Text(m.people.len().to_text())
}

view member_row(p: Member) {
    ui.ListRow(title: p.name)
}
"#;

    /// The device's content store: the files a test keeps, by the address of their bytes.
    #[derive(Clone, Default)]
    struct Device(Rc<RefCell<HashMap<String, Vec<u8>>>>);

    impl Device {
        fn keeping(&self, name: &str, media: &str, bytes: &[u8]) -> FileValue {
            let hash = digest_of(bytes);
            self.0.borrow_mut().insert(hash.clone(), bytes.to_vec());
            FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            )
        }
    }

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(hash).cloned()
        }

        fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
            Ok(Vec::new())
        }

        fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
            Ok(())
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            Ok(self.keeping(name, media, bytes))
        }
    }

    fn digest_of(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let mut digest = 0xcbf2_9ce4_8422_2325_u64;
        for byte in bytes {
            digest = (digest ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        let mut hex = String::with_capacity(17);
        let _ = write!(hex, "h{digest:016x}");
        hex
    }

    struct Writer {
        rt: Runtime,
        app: App,
        doc: ResourceRef,
        device: Device,
    }

    impl Writer {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let device = Device::default();
            rt.set_blobs(device.clone());
            rt.set_clock(|| "2026-11-24T09:00:00Z".to_owned());
            let workspace = App::compile(WORKSPACE, &[]);
            workspace.load(&rt);
            let discussion = App::compile(DISCUSSION, &[workspace.meta().clone()]);
            discussion.load(&rt);
            rt.create_root("Comments")
                .expect("the discussion's root is the workspace's comments");
            let app = App::compile(
                SOURCE,
                &[workspace.meta().clone(), discussion.meta().clone()],
            );
            app.load(&rt);
            // The Writer the person installed may make its comments in the discussion.
            authority.trust(&Runtime::module_hash(&app.compiled.wasm));
            let doc = rt.create_root("Document").unwrap();
            Self {
                rt,
                app,
                doc,
                device,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
        }

        fn one(&self, action: &str, args: Vec<Value>) {
            self.call(action, args);
        }

        /// Whether the app refuses `action` with these arguments, as a `require` does.
        fn refused(&self, action: &str, args: Vec<Value>) -> bool {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).is_err()
        }

        fn text(&self, field: &str) -> String {
            match self.rt.read(&self.doc, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        /// What the document says as Markdown, through the app's own read.
        fn markdown(&self) -> String {
            match self.call("read_content", vec![Value::resource(&self.doc)]) {
                InvokeOutcome::Done(Value::Text(text)) => text,
                other => panic!("read_content answers text: {other:?}"),
            }
        }

        fn body(&self) -> String {
            match self.rt.read(&self.doc, "body").unwrap() {
                Value::Rich(body) => body.written_text(),
                other => panic!("body reads {other:?}"),
            }
        }

        fn words(words: &str) -> Value {
            Value::Rich(RichValue::from_text(words))
        }

        fn window(&self) -> tilolang_runtime::ViewInstance {
            self.rt
                .open_view(
                    &me(),
                    &self.app.view("writer"),
                    vec![Value::resource(&self.doc)],
                )
                .unwrap()
        }
    }

    /// The key of the one `kind` node in `view` whose `label` reads `label`.
    fn control(view: &tilolang_runtime::ViewInstance, kind: &str, label: &str) -> String {
        let found: Vec<String> = of_kind(view.tree(), kind)
            .into_iter()
            .filter(|node| node.props.get("label") == Some(&Value::text(label)))
            .map(|node| node.key.clone())
            .collect();
        assert_eq!(
            found.len(),
            1,
            "one {kind} reads {label:?}:\n{}",
            dump(view.tree())
        );
        found[0].clone()
    }

    /// What the editor in `view` was asked for: the formatting command carried by its `format`.
    fn asked_of(view: &tilolang_runtime::ViewInstance) -> String {
        let editor = of_kind(view.tree(), "ui.BlockEditor")[0];
        match editor.props.get("format") {
            Some(Value::Text(ask)) => ask.clone(),
            Some(Value::Opt(Some(held))) => match &**held {
                Value::Text(ask) => ask.clone(),
                other => panic!("the editor's format reads {other:?}"),
            },
            other => panic!("the editor's format reads {other:?}"),
        }
    }

    /// How much larger the editor in `view` shows the page.
    fn shown_zoom(view: &tilolang_runtime::ViewInstance) -> f64 {
        let editor = of_kind(view.tree(), "ui.BlockEditor")[0];
        match editor.props.get("zoom") {
            Some(Value::Float(zoom)) => *zoom,
            Some(Value::Opt(Some(held))) => match &**held {
                Value::Float(zoom) => *zoom,
                other => panic!("the editor's zoom reads {other:?}"),
            },
            other => panic!("the editor's zoom reads {other:?}"),
        }
    }

    /// Every text the window shows.
    fn texts(view: &tilolang_runtime::ViewInstance) -> Vec<String> {
        of_kind(view.tree(), "ui.Text")
            .into_iter()
            .filter_map(|text| text.props.get("value").and_then(Value::as_text))
            .map(str::to_owned)
            .collect()
    }

    fn file_of(outcome: InvokeOutcome) -> FileValue {
        match outcome {
            InvokeOutcome::Done(Value::File(file)) => (*file).clone(),
            other => panic!("a file was expected, got {other:?}"),
        }
    }

    #[test]
    fn a_document_starts_from_a_template_and_keeps_its_title_empty() {
        let writer = Writer::new();
        writer.one(
            "new_document",
            vec![Value::resource(&writer.doc), Value::text("letter")],
        );
        assert_eq!(writer.text("title"), "", "the first words name it");
        assert_eq!(writer.text("started"), "letter");
        let body = writer.body();
        assert!(body.contains("Dear [Recipient Name]"), "{body}");
    }

    #[test]
    fn tables_grow_and_shrink_with_their_actions() {
        let writer = Writer::new();
        writer.one(
            "insert_table",
            vec![Value::resource(&writer.doc), Value::Int(2), Value::Int(2)],
        );
        let body = writer.markdown();
        assert!(body.contains("| Column | Column |"), "{body}");
        assert_eq!(body.lines().filter(|l| l.starts_with('|')).count(), 4);
        assert_eq!(
            writer.body().lines().filter(|l| l.starts_with('|')).count(),
            4
        );

        writer.one("add_row", vec![Value::resource(&writer.doc)]);
        assert_eq!(
            writer.body().lines().filter(|l| l.starts_with('|')).count(),
            5
        );
        writer.one(
            "delete_row",
            vec![Value::resource(&writer.doc), Value::Int(2)],
        );
        assert_eq!(
            writer.body().lines().filter(|l| l.starts_with('|')).count(),
            4,
            "the row added is gone again"
        );
    }

    #[test]
    fn the_page_header_and_footer_are_the_documents_own() {
        let writer = Writer::new();
        writer.one(
            "set_page_words",
            vec![
                Value::resource(&writer.doc),
                Value::text("header"),
                Value::text("Q3 report"),
            ],
        );
        writer.one(
            "set_page_words",
            vec![
                Value::resource(&writer.doc),
                Value::text("footer"),
                Value::text("Internal"),
            ],
        );
        assert_eq!(writer.text("header"), "Q3 report");
        assert_eq!(writer.text("footer"), "Internal");

        assert!(
            writer.refused(
                "set_page_words",
                vec![
                    Value::resource(&writer.doc),
                    Value::text("margin"),
                    Value::text("nowhere"),
                ],
            ),
            "a page has a header and a footer, not a margin"
        );
    }

    #[test]
    fn a_table_of_contents_is_a_field_the_headings_fill() {
        let writer = Writer::new();
        writer.one(
            "replace_body",
            vec![
                Value::resource(&writer.doc),
                Value::text("# One\n\nSaid.\n\n## Two\n\nMore."),
            ],
        );
        writer.one("generate_toc", vec![Value::resource(&writer.doc)]);
        assert!(
            writer.markdown().starts_with("[TOC]"),
            "{}",
            writer.markdown()
        );
        assert!(
            writer.refused("generate_toc", vec![Value::resource(&writer.doc)]),
            "a document lists its headings once"
        );
    }

    #[test]
    fn a_file_opens_into_the_document_and_writes_out_again() {
        let writer = Writer::new();
        let file = writer
            .device
            .keeping("notes.txt", "text/plain", b"First line.\n\nSecond line.");
        writer.one(
            "open_document",
            vec![Value::resource(&writer.doc), Value::file(file)],
        );
        assert_eq!(writer.text("title"), "notes");
        assert_eq!(writer.markdown().trim(), "First line.\n\nSecond line.");

        let exported = file_of(writer.call("export_text", vec![Value::resource(&writer.doc)]));
        assert_eq!(exported.name, "notes.txt");
        let markdown = file_of(writer.call("export_content", vec![Value::resource(&writer.doc)]));
        assert_eq!(markdown.name, "notes.md");
    }

    #[test]
    fn the_page_zooms_from_the_toolbar_and_the_band_says_how_large_it_reads() {
        let writer = Writer::new();
        let mut view = writer.window();
        assert!(
            (shown_zoom(&view) - 1.0).abs() < f64::EPSILON,
            "a page opens at its own size"
        );

        let larger = control(&view, "ui.ToolbarButton", "Zoom in");
        view.press(&larger).unwrap();
        view.poll_patches().unwrap();
        assert!((shown_zoom(&view) - 1.25).abs() < f64::EPSILON);
        assert!(
            texts(&view).iter().any(|text| text == "125%"),
            "the band names the size the page reads at: {:?}",
            texts(&view)
        );

        let smaller = control(&view, "ui.ToolbarButton", "Zoom out");
        view.press(&smaller).unwrap();
        view.press(&smaller).unwrap();
        view.poll_patches().unwrap();
        assert!(
            (shown_zoom(&view) - 0.8).abs() < f64::EPSILON,
            "two steps back from 125%"
        );

        // The page's menu names the sizes it is set at, and choosing one sets it.
        let menu = view.tree().clone();
        let half = of_kind(&menu, "ui.MenuItem")
            .into_iter()
            .find(|item| item.props.get("label") == Some(&Value::text("150%")))
            .unwrap_or_else(|| panic!("the menu names each size:\n{}", dump(&menu)));
        let half = half.key.clone();
        view.fire(&half, "choose").unwrap();
        view.poll_patches().unwrap();
        assert!((shown_zoom(&view) - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn the_band_counts_the_sheets_the_page_fills() {
        let writer = Writer::new();
        let mut view = writer.window();
        assert!(
            !texts(&view).iter().any(|text| text.ends_with(" pages")),
            "one sheet says nothing: {:?}",
            texts(&view)
        );
        let editor = of_kind(view.tree(), "ui.BlockEditor")[0].key.clone();
        view.fire_with(&editor, "page_count", Some(&Value::Int(3)))
            .unwrap();
        view.poll_patches().unwrap();
        assert!(
            texts(&view).iter().any(|text| text == "3 pages"),
            "the band counts what the editor reports: {:?}",
            texts(&view)
        );
        // The sheet the caret is on joins the count, and follows the caret.
        view.fire_with(&editor, "page_shown", Some(&Value::Int(2)))
            .unwrap();
        view.poll_patches().unwrap();
        assert!(
            texts(&view).iter().any(|text| text == "Page 2 of 3"),
            "the band says which sheet of how many: {:?}",
            texts(&view)
        );
        view.fire_with(&editor, "page_shown", Some(&Value::Int(3)))
            .unwrap();
        view.poll_patches().unwrap();
        assert!(
            texts(&view).iter().any(|text| text == "Page 3 of 3"),
            "{:?}",
            texts(&view)
        );
    }

    #[test]
    fn the_panels_ask_the_editor_for_spacing_and_a_pictures_shape() {
        let writer = Writer::new();
        let mut view = writer.window();

        // The paragraph panel's line spacing, as the paragraph's own command.
        let styles = control(&view, "ui.ToolbarButton", "Styles");
        view.press(&styles).unwrap();
        view.poll_patches().unwrap();
        let spaced = control(&view, "ui.Chip", "1.5");
        view.press(&spaced).unwrap();
        view.poll_patches().unwrap();
        assert_eq!(asked_of(&view), "spacing:1.5:1");

        // The Text panel's picture inspector: each control names the picture change it makes.
        let text = control(&view, "ui.ToolbarButton", "Text");
        view.press(&text).unwrap();
        view.poll_patches().unwrap();
        let larger = control(&view, "ui.Chip", "Larger");
        view.press(&larger).unwrap();
        view.poll_patches().unwrap();
        assert_eq!(asked_of(&view), "picture:size:up:2");
        let wrapped = control(&view, "ui.Chip", "Left");
        view.press(&wrapped).unwrap();
        view.poll_patches().unwrap();
        assert_eq!(asked_of(&view), "picture:wrap:left:3");
    }

    #[test]
    fn a_comment_is_the_discussions_and_anchored_to_the_words_it_is_about() {
        let writer = Writer::new();
        writer.one(
            "open_document",
            vec![
                Value::resource(&writer.doc),
                Value::file(writer.device.keeping(
                    "plan.md",
                    "text/markdown",
                    b"# Plan\n\nWe open on Monday.\n",
                )),
            ],
        );
        let InvokeOutcome::Done(Value::Ref(comment)) = writer.call(
            "comment_on",
            vec![
                Value::resource(&writer.doc),
                Writer::words("Which Monday?"),
                Value::text("\u{b6}2"),
                Value::Int(0),
                Value::Int(2),
            ],
        ) else {
            panic!("the comment is the answer");
        };
        match writer.rt.read(&comment.target, "text").unwrap() {
            Value::Rich(text) => assert_eq!(text.written_text(), "Which Monday?"),
            other => panic!("the comment's text reads {other:?}"),
        }
        let Value::Rich(body) = writer.rt.read(&writer.doc, "body").unwrap() else {
            panic!("the body is rich text");
        };
        assert!(
            format!("{body:?}").contains(&comment.target.id_string()),
            "the words carry the comment's mark: {body:?}"
        );
        let InvokeOutcome::Done(Value::Ref(plain)) = writer.call(
            "comment_on",
            vec![Value::resource(&writer.doc), Writer::words("Looks good.")],
        ) else {
            panic!("a comment on the whole document is the answer too");
        };
        assert_ne!(plain.target, comment.target);
    }

    #[test]
    fn the_window_search_replaces_what_it_found() {
        let writer = Writer::new();
        writer.one(
            "replace_body",
            vec![
                Value::resource(&writer.doc),
                Value::text("Monday stands. Monday goes.\n\nMonday stays."),
            ],
        );
        let mut view = writer.window();
        let shell = of_kind(view.tree(), "ui.NavShell")[0].key.clone();
        view.fire_with(&shell, "search", Some(&Value::text("Monday")))
            .unwrap();
        view.poll_patches().unwrap();

        let replacement = of_kind(view.tree(), "ui.TextInput")
            .into_iter()
            .find(|field| field.props.get("placeholder") == Some(&Value::text("The replacement")))
            .unwrap_or_else(|| {
                panic!(
                    "the search line takes a replacement:\n{}",
                    dump(view.tree())
                )
            })
            .key
            .clone();
        view.edit(&replacement, &Value::text("Tuesday")).unwrap();
        view.poll_patches().unwrap();
        let all = control(&view, "ui.Button", "Replace all");
        view.press(&all).unwrap();
        view.poll_patches().unwrap();
        assert_eq!(
            writer.markdown().trim(),
            "Tuesday stands. Tuesday goes.\n\nTuesday stays.",
            "every passage the search found is written over"
        );
        assert!(
            of_kind(view.tree(), "ui.Button")
                .iter()
                .any(|button| button.props.get("label") == Some(&Value::text("Replace"))),
            "the line writes one over too"
        );
    }

    #[test]
    fn a_column_added_then_a_row_keeps_the_table() {
        let writer = Writer::new();
        writer.one(
            "insert_table",
            vec![Value::resource(&writer.doc), Value::Int(2), Value::Int(2)],
        );
        writer.one("add_column", vec![Value::resource(&writer.doc)]);
        writer.one("add_row", vec![Value::resource(&writer.doc)]);
        let body = writer.markdown();
        assert_eq!(
            body.lines().filter(|l| l.starts_with('|')).count(),
            5,
            "the table still stands, its head, its rule and the rows: {body}"
        );
        assert_eq!(
            body.lines()
                .filter(|l| l.starts_with('|'))
                .map(|l| l.matches('|').count())
                .collect::<Vec<_>>(),
            [4, 4, 4, 4, 4],
            "every row holds the column added: {body}"
        );
    }

    /// The document's body as the field holds it.
    fn rich_body(writer: &Writer) -> RichValue {
        match writer.rt.read(&writer.doc, "body").unwrap() {
            Value::Rich(body) => body,
            other => panic!("body reads {other:?}"),
        }
    }

    #[test]
    fn a_cell_is_written_and_a_replace_keeps_the_comments_around_it() {
        let writer = Writer::new();
        writer.one(
            "open_document",
            vec![
                Value::resource(&writer.doc),
                Value::file(writer.device.keeping(
                    "plan.md",
                    "text/markdown",
                    b"# Plan\n\nWe open on Monday at nine.\n",
                )),
            ],
        );
        let InvokeOutcome::Done(Value::Ref(comment)) = writer.call(
            "comment_on",
            vec![
                Value::resource(&writer.doc),
                Writer::words("Which Monday?"),
                Value::text("\u{b6}2"),
                Value::Int(11),
                Value::Int(17),
            ],
        ) else {
            panic!("the comment is the answer");
        };
        writer.one(
            "insert_table",
            vec![Value::resource(&writer.doc), Value::Int(1), Value::Int(2)],
        );
        // One cell is written: its words change, the rest of the table and the words above
        // stay as they were, the comment's mark with them.
        writer.one(
            "write_cell",
            vec![
                Value::resource(&writer.doc),
                Value::Int(2),
                Value::Int(2),
                Value::text("Ada | Lovelace"),
            ],
        );
        let body = writer.markdown();
        assert!(
            body.contains("| Column | Column |") && body.contains(" Ada \\| Lovelace |"),
            "the cell holds the words, its bar escaped: {body}"
        );
        assert!(
            format!("{:?}", rich_body(&writer)).contains(&comment.target.id_string()),
            "writing a cell leaves the comment where it was"
        );

        // Replacing every "Monday" keeps the comment over what replaced it.
        writer.one(
            "replace_text",
            vec![
                Value::resource(&writer.doc),
                Value::text("Monday"),
                Value::text("Tuesday"),
                Value::Bool(true),
            ],
        );
        let rich = rich_body(&writer);
        let said = rich
            .blocks
            .iter()
            .find(|block| block.text.contains("Tuesday"))
            .unwrap_or_else(|| panic!("the words were replaced: {rich:?}"));
        let mark = said
            .marks
            .iter()
            .find(|mark| mark.name == "comment")
            .unwrap_or_else(|| panic!("the comment's mark stays: {said:?}"));
        let marked: String = said
            .text
            .chars()
            .skip(mark.start)
            .take(mark.end - mark.start)
            .collect();
        assert_eq!(
            marked, "Tuesday",
            "the mark covers the words that replaced its own"
        );
        assert!(rich.blocks.iter().any(|block| block.text.contains("Ada")));

        // Rows and columns come and go by place; the last one never does.
        writer.one(
            "add_column",
            vec![Value::resource(&writer.doc), Value::Int(1)],
        );
        assert!(
            writer.markdown().contains("| Column |  | Column |"),
            "{}",
            writer.markdown()
        );
        writer.one(
            "delete_column",
            vec![Value::resource(&writer.doc), Value::Int(2)],
        );
        writer.one("delete_table", vec![Value::resource(&writer.doc)]);
        assert!(!writer.markdown().contains('|'), "{}", writer.markdown());
        assert!(writer.markdown().contains("Tuesday at nine"));
    }

    fn some(value: Value) -> Value {
        Value::Opt(Some(Box::new(value)))
    }

    #[test]
    fn named_styles_are_the_documents_and_changed_made_copied_and_deleted() {
        let writer = Writer::new();
        let doc = || Value::resource(&writer.doc);
        writer.one("append_paragraph", vec![doc(), Value::text("A line.")]);
        writer.one(
            "set_style_look",
            vec![
                doc(),
                Value::text("Quote"),
                some(Value::text("serif")),
                some(Value::Float(14.0)),
                some(Value::Bool(true)),
            ],
        );
        let looks = rich_body(&writer).styles.expect("the table is kept");
        let quote = looks.get("Quote").expect("the quote's look");
        assert_eq!(
            quote.get("font"),
            Some(&tilo_doc::AttrVal::Str("serif".into()))
        );
        assert_eq!(quote.get("size"), Some(&tilo_doc::AttrVal::Float(14.0)));
        assert_eq!(quote.get("bold"), Some(&tilo_doc::AttrVal::Bool(true)));
        assert_eq!(writer.body().trim(), "A line.", "the words are not touched");

        writer.one(
            "new_style",
            vec![doc(), Value::text("Aside"), Value::text("Quote")],
        );
        writer.one("duplicate_style", vec![doc(), Value::text("Aside")]);
        let looks = rich_body(&writer).styles.expect("the table is kept");
        assert_eq!(
            looks.get("Aside"),
            looks.get("Quote"),
            "a new style starts as its source"
        );
        assert_eq!(looks.get("Aside copy"), looks.get("Aside"));
        assert!(writer.refused("new_style", vec![doc(), Value::text("Aside")]));

        writer.one("delete_style", vec![doc(), Value::text("Aside copy")]);
        let looks = rich_body(&writer).styles.expect("the table is kept");
        assert!(!looks.contains_key("Aside copy"));
        assert!(
            writer.refused("delete_style", vec![doc(), Value::text("Heading 1")]),
            "a style every document has stays"
        );
        assert!(writer.refused(
            "set_style_look",
            vec![doc(), Value::text("Nowhere"), some(Value::text("serif"))],
        ));

        // The panel sets the style the person chose.
        let mut view = writer.window();
        let styles = control(&view, "ui.ToolbarButton", "Styles");
        view.press(&styles).unwrap();
        view.poll_patches().unwrap();
        let aside = control(&view, "ui.Chip", "Aside");
        view.press(&aside).unwrap();
        view.poll_patches().unwrap();
        let mono = control(&view, "ui.Chip", "Mono");
        view.press(&mono).unwrap();
        view.poll_patches().unwrap();
        let looks = rich_body(&writer).styles.expect("the table is kept");
        assert_eq!(
            looks.get("Aside").and_then(|look| look.get("font")),
            Some(&tilo_doc::AttrVal::Str("mono".into()))
        );
        assert_eq!(
            asked_of(&view),
            "style:Aside:1",
            "choosing a style sets the paragraph the caret is in in it"
        );
    }

    #[test]
    fn the_windows_toolbar_and_outline_are_there() {
        let writer = Writer::new();
        let window = writer
            .rt
            .open_view(
                &me(),
                &writer.app.view("writer"),
                vec![Value::resource(&writer.doc)],
            )
            .unwrap();
        let shown = crate::common::dump(window.tree());
        for expected in ["Bold", "Table of contents", "Outline", "File"] {
            assert!(shown.contains(expected), "no {expected} in\n{shown}");
        }
    }
}
