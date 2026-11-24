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
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, Value};

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
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
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

        fn words(&self, words: &str) -> Value {
            Value::Rich(RichValue::from_text(words))
        }
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
