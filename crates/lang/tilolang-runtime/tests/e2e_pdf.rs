//! The PDF reader (`apps/tl/pdf`) end to end: a PDF opened into its pages, the markup the file
//! already carries taken over, its words read into runs a person selects and copies, its links,
//! finding words in a page, and pages duplicated, deleted and saved.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::clipboard::ClipboardHost;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, PdfHost, ResourceRef, Runtime, Space, UiNode, Value,
    };

    const SOURCE: &str = concat!(
        include_str!("../../../../apps/tl/pdf/app.tl"),
        "\n",
        include_str!("../../../../apps/tl/pdf/reader.tl"),
    );

    /// A PDF's bytes, as far as the test reads them: only that they are there.
    const PDF: &[u8] = b"%PDF-1.4 the test's document";

    /// One glyph of a page's text, at its place in the page's display space.
    fn glyph(text: &str, line: u32, x: f32, y: f32, w: f32) -> String {
        format!(
            r#"{{"text":"{text}","line":{line},"x":{x},"y":{y},"w":{w},"h":12.0,"placed":false}}"#
        )
    }

    /// A word of a page, each of its letters a glyph, at `x` on `line`.
    fn word(text: &str, line: u32, x: f32) -> String {
        let mut glyphs = Vec::new();
        let mut at = x;
        for letter in text.chars() {
            glyphs.push(glyph(&letter.to_string(), line, at, 72.0, 8.0));
            at += 8.0;
        }
        // The engine places the space after a word, as the file does not draw one.
        glyphs.push(glyph(" ", line, at, 72.0, 4.0));
        glyphs.join(",")
    }

    /// The device's PDF engine, as the tests' documents hold it: two pages, their words and one
    /// page's links, a contents entry, and a highlight the file itself carries.
    struct Engine;

    impl PdfHost for Engine {
        fn pages(&self, _bytes: &[u8]) -> Result<String, String> {
            Ok(r#"[{"page":1,"width":612.0,"height":792.0},{"page":2,"width":612.0,"height":792.0}]"#
                .to_owned())
        }

        fn outline(&self, _bytes: &[u8]) -> Result<String, String> {
            Ok(r#"[{"title":"Start","page":1,"level":0}]"#.to_owned())
        }

        fn text(&self, _bytes: &[u8], page: i64) -> Result<String, String> {
            match page {
                1 => Ok(format!(
                    "[{},{}]",
                    word("Hello", 0, 72.0),
                    word("world", 0, 120.0)
                )),
                2 => Ok(format!("[{}]", word("Second", 0, 72.0))),
                other => Err(format!("the document has no page {other}")),
            }
        }

        fn links(&self, _bytes: &[u8], page: i64) -> Result<String, String> {
            match page {
                1 => Ok(concat!(
                    r#"[{"rect":{"x":72.0,"y":72.0,"w":48.0,"h":12.0},"kind":"page","to":2},"#,
                    r#"{"rect":{"x":72.0,"y":100.0,"w":80.0,"h":12.0},"kind":"uri","to":"https://example.com"}]"#,
                )
                .to_owned()),
                _ => Ok("[]".to_owned()),
            }
        }

        fn fields(&self, _bytes: &[u8]) -> Result<String, String> {
            // One field the document locks and one it needs, so a reader is told of both.
            Ok(concat!(
                r#"[{"name":"Signed by","kind":"text","value":"A. Ferrer","checked":false,"export":"","options":[],"page":1,"rect":[72.0,700.0,180.0,14.0],"read_only":true,"required":false},"#,
                r#"{"name":"Date","kind":"text","value":"","checked":false,"export":"","options":[],"page":1,"rect":[72.0,680.0,120.0,14.0],"read_only":false,"required":true}]"#,
            )
            .to_owned())
        }

        fn markup(&self, bytes: &[u8]) -> Result<String, String> {
            if bytes.is_empty() {
                return Ok(r#"{"marks":[],"fields":[]}"#.to_owned());
            }
            Ok(concat!(
                r##"{"marks":[{"page":0,"index":0,"name":"m1","kind":"highlight","color":"#ffff00","##,
                r##""contents":"","author":"Ada","created":"","modified":"","rect":[72.0,72.0,48.0,12.0],"##,
                r##""quads":[[72.0,72.0,120.0,72.0,72.0,84.0,120.0,84.0]],"strokes":[],"size":null,"##,
                r##""in_reply_to":null,"reply":false,"hidden":false,"readable":true}],"fields":[]}"##,
            )
            .to_owned())
        }

        fn picture(&self, _bytes: &[u8], _page: i64, _width: i64) -> Result<Vec<u8>, String> {
            Ok(b"\x89PNG\r\n\x1a\n".to_vec())
        }

        fn import(&self, _bytes: &[u8]) -> Result<String, String> {
            Ok(r#"{"kind":"pdf","pages":[{"page":1,"width":612.0,"height":792.0}]}"#.to_owned())
        }

        fn save(&self, sources: &[&[u8]], request: &str) -> Result<Vec<u8>, String> {
            assert_eq!(
                sources.first(),
                Some(&PDF),
                "the document's own bytes are saved"
            );
            assert!(request.contains("\"pages\""), "a save names the pages");
            Ok(b"%PDF-1.4 saved".to_vec())
        }
    }

    /// The device's content store and its clipboard.
    #[derive(Clone, Default)]
    struct Device {
        files: Rc<RefCell<HashMap<String, Vec<u8>>>>,
        clip: Rc<RefCell<String>>,
    }

    impl Device {
        /// `bytes` kept as the file `name`, as a person choosing it answers.
        fn keeping(&self, name: &str, media: &str, bytes: &[u8]) -> FileValue {
            let hash = digest_of(bytes);
            self.files.borrow_mut().insert(hash.clone(), bytes.to_vec());
            FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            )
        }

        fn copied(&self) -> String {
            self.clip.borrow().clone()
        }
    }

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.files.borrow().get(hash).cloned()
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

    impl ClipboardHost for Device {
        fn copy(&self, _mime: &str, bytes: &[u8]) -> bool {
            *self.clip.borrow_mut() = String::from_utf8_lossy(bytes).into_owned();
            true
        }

        fn paste(&self, _types: &[&str]) -> Option<(String, Vec<u8>)> {
            None
        }
    }

    /// The address a content store names `bytes` by: the test's stand-in for the store's hash.
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

    struct Reader {
        rt: Runtime,
        app: App,
        lib: ResourceRef,
        device: Device,
    }

    impl Reader {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            rt.set_pdf(Engine);
            let device = Device::default();
            rt.set_blobs(device.clone());
            rt.set_clipboard(device.clone());
            rt.set_clock(|| "2026-11-24T09:00:00Z".to_owned());
            let app = App::compile(SOURCE, &[]);
            authority.trust(&app.load(&rt));
            for action in ["copy_selected", "copy_page"] {
                authority.grant_outside(&app.action(action));
            }
            let lib = rt.create_root("Library").unwrap();
            Self {
                rt,
                app,
                lib,
                device,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
        }

        fn open(&self) -> ResourceRef {
            let file = self.device.keeping("Report.pdf", "application/pdf", PDF);
            returned_ref(self.call(
                "open_pdf",
                vec![Value::resource(&self.lib), Value::file(file)],
            ))
        }

        /// The `at`th page of `doc`, counted from 0, as the resource its actions take.
        fn page(&self, doc: &ResourceRef, at: usize) -> ResourceRef {
            let pages = self.rt.read(doc, "pages").unwrap();
            let projected = self.rt.project(&pages);
            let id = projected["items"][at]["id"]
                .as_str()
                .unwrap_or_else(|| panic!("no page {at} in {}", projected));
            let ty = self
                .app
                .meta()
                .types
                .iter()
                .find(|t| t.name == "Page")
                .unwrap()
                .id
                .clone();
            ResourceRef::parse_id(id, ty).unwrap()
        }

        fn count(&self, at: &ResourceRef, field: &str) -> usize {
            let held = self.rt.read(at, field).unwrap();
            self.rt.project(&held)["count"].as_u64().unwrap_or(0) as usize
        }

        fn texts(&self, at: &ResourceRef, field: &str) -> Vec<String> {
            match self.rt.read(at, field).unwrap() {
                Value::List(items) => items
                    .iter()
                    .map(|item| match item {
                        Value::Text(text) => text.clone(),
                        other => panic!("{field} holds {other:?}"),
                    })
                    .collect(),
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn text(&self, at: &ResourceRef, field: &str) -> String {
            match self.rt.read(at, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn window(&self, view: &str, args: Vec<Value>) -> tilolang_runtime::ViewInstance {
            self.rt
                .open_view(&me(), &self.app.view(view), args)
                .unwrap()
        }
    }

    fn reader_text(window: &tilolang_runtime::ViewInstance) -> String {
        fn go(node: &UiNode, out: &mut String) {
            for name in ["label", "text", "value", "title"] {
                if let Some(Value::Text(text)) = node.props.get(name) {
                    out.push_str(text);
                    out.push('\n');
                }
            }
            for child in &node.children {
                go(child, out);
            }
        }
        let mut out = String::new();
        go(window.tree(), &mut out);
        out
    }

    #[test]
    fn opening_a_pdf_lays_out_its_pages_and_takes_its_own_markup_over() {
        let reader = Reader::new();
        let doc = reader.open();
        assert_eq!(
            reader.count(&doc, "pages"),
            2,
            "two pages stand in the file"
        );
        assert_eq!(reader.count(&doc, "outlines"), 1, "its contents are read");
        reader.call("adopt_markup", vec![Value::resource(&doc)]);
        assert!(
            reader
                .text(&doc, "imported_source")
                .ends_with(&digest_of(PDF).clone()),
            "the source it took its markup over from is remembered; trouble={:?}",
            reader.text(&doc, "trouble")
        );
        assert_eq!(
            reader.count(&doc, "marks"),
            1,
            "the highlight the file carried is taken over; the reader says {:?}",
            reader.text(&doc, "trouble")
        );
    }

    #[test]
    fn a_page_reads_its_words_as_runs_and_as_search_words() {
        let reader = Reader::new();
        let doc = reader.open();
        let first = reader.page(&doc, 0);
        reader.call(
            "read_page",
            vec![Value::resource(&doc), Value::resource(&first)],
        );
        assert_eq!(reader.text(&first, "words"), "Hello world");
        assert_eq!(
            reader.texts(&first, "run_words"),
            ["Hello".to_owned(), "world".to_owned()],
            "each word a run a person selects"
        );
        let runs = match reader.rt.read(&first, "runs").unwrap() {
            Value::List(items) => items.len(),
            other => panic!("runs read {other:?}"),
        };
        assert_eq!(runs, 8, "four numbers for each of two runs");
        assert_eq!(
            reader.count(&doc, "links"),
            2,
            "the page's two links are read"
        );
    }

    #[test]
    fn selecting_runs_copies_their_words() {
        let reader = Reader::new();
        let doc = reader.open();
        let first = reader.page(&doc, 0);
        reader.call(
            "read_page",
            vec![Value::resource(&doc), Value::resource(&first)],
        );

        reader.call(
            "copy_selected",
            vec![
                Value::resource(&doc),
                Value::resource(&first),
                Value::Int(0),
                Value::Int(1),
            ],
        );
        assert_eq!(reader.device.copied(), "Hello world");

        reader.call(
            "copy_selected",
            vec![
                Value::resource(&doc),
                Value::resource(&first),
                Value::Int(1),
                Value::Int(1),
            ],
        );
        assert_eq!(reader.device.copied(), "world", "a selection of one run");

        reader.call(
            "copy_page",
            vec![Value::resource(&doc), Value::resource(&first)],
        );
        assert_eq!(reader.device.copied(), "Hello world");
    }

    #[test]
    fn finding_counts_the_pages_and_the_words_it_finds() {
        let reader = Reader::new();
        let doc = reader.open();
        for at in 0..2 {
            let page = reader.page(&doc, at);
            reader.call(
                "read_page",
                vec![Value::resource(&doc), Value::resource(&page)],
            );
        }
        let found = reader.call("find_in", vec![Value::resource(&doc), Value::text("world")]);
        let InvokeOutcome::Done(value) = found else {
            panic!("find_in returns its pages");
        };
        let Value::List(pages) = value else {
            panic!("find_in answers a list");
        };
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0], Value::text("Page 1: 1"));
    }

    /// The window's search is the find in a PDF: the words the person searches for in the
    /// window's bar are counted above the pages, with the pages they stand on, and the reader
    /// draws no search field of its own.
    #[test]
    fn the_windows_search_finds_in_the_pdf() {
        let reader = Reader::new();
        let doc = reader.open();
        for at in 0..2 {
            let page = reader.page(&doc, at);
            reader.call(
                "read_page",
                vec![Value::resource(&doc), Value::resource(&page)],
            );
        }
        let mut window = reader.window("reader", vec![Value::resource(&doc)]);
        assert!(
            of_kind(window.tree(), "ui.TextInput")
                .iter()
                .all(|field| !matches!(
                    field.props.get("placeholder"),
                    Some(Value::Text(placeholder)) if placeholder.starts_with("Find")
                )),
            "the reader draws no search field of its own:\n{}",
            dump(window.tree())
        );
        let shell = of_kind(window.tree(), "ui.NavShell")[0].key.clone();
        window
            .fire_with(&shell, "search", Some(&Value::text("world")))
            .unwrap();
        window.poll_patches().unwrap();
        let text = reader_text(&window);
        assert!(
            text.contains("1 found in 1 pages") && text.contains("Page 1 (1)"),
            "what the window searched for is counted, with the page it stands on:\n{text}"
        );
    }

    #[test]
    fn duplicate_and_delete_change_the_pages_the_saved_copy_holds() {
        let reader = Reader::new();
        let doc = reader.open();
        let first = reader.page(&doc, 0);
        let copy = returned_ref(reader.call(
            "duplicate_page",
            vec![Value::resource(&doc), Value::resource(&first)],
        ));
        assert_eq!(reader.count(&doc, "pages"), 3);
        reader.call(
            "delete_page",
            vec![Value::resource(&doc), Value::resource(&copy)],
        );
        assert_eq!(reader.count(&doc, "pages"), 2);

        let saved = reader.call("save_copy", vec![Value::resource(&doc)]);
        let InvokeOutcome::Done(Value::File(file)) = saved else {
            panic!("a save answers the file it wrote");
        };
        assert_eq!(file.media, "application/pdf");
    }

    #[test]
    fn a_copy_is_saved_as_a_pdf() {
        let reader = Reader::new();
        let doc = reader.open();
        let saved = reader.call("save_copy", vec![Value::resource(&doc)]);
        let InvokeOutcome::Done(Value::File(file)) = saved else {
            panic!("a save answers the file it wrote: {saved:?}");
        };
        assert_eq!(file.media, "application/pdf");
    }

    #[test]
    fn a_file_without_markup_takes_nothing_over() {
        let reader = Reader::new();
        let file = reader.device.keeping("Empty.pdf", "application/pdf", b"");
        let doc = returned_ref(reader.call(
            "open_pdf",
            vec![Value::resource(&reader.lib), Value::file(file)],
        ));
        assert!(
            reader
                .text(&doc, "imported_source")
                .ends_with(&digest_of(b"").clone())
        );
        assert_eq!(reader.count(&doc, "marks"), 0);
    }

    /// A second file opened while the reader already runs adds its own document, and that
    /// document's window shows it: the same action a shell runs when a person opens another file
    /// in the app it is already showing.
    #[test]
    fn a_pdf_opened_while_the_reader_runs_shows_its_own_document() {
        let reader = Reader::new();
        let first = reader.open();
        let second_file = reader.device.keeping(
            "Later.pdf",
            "application/pdf",
            b"%PDF-1.4 the second document",
        );
        let second = returned_ref(reader.call(
            "open_pdf",
            vec![Value::resource(&reader.lib), Value::file(second_file)],
        ));
        assert_ne!(
            first.id_string(),
            second.id_string(),
            "each file is its own document"
        );
        assert_eq!(
            reader.count(&reader.lib, "documents"),
            2,
            "both documents stand in the library"
        );
        let window = reader.window("reader", vec![Value::resource(&second)]);
        let shown = crate::common::dump(window.tree());
        assert!(shown.contains("Later.pdf"), "the second file:\n{shown}");
    }

    #[test]
    fn a_locked_or_required_field_says_so() {
        let reader = Reader::new();
        let doc = reader.open();
        reader.call("read_fields", vec![Value::resource(&doc)]);
        // The pages are rows built as they scroll into view: a test reads them all.
        let mut window = reader.window("reader", vec![Value::resource(&doc)]);
        window.set_lazy_rows(false).unwrap();
        let said = crate::common::dump(window.tree());
        assert!(
            said.contains("The document locks this field."),
            "the locked field says so:\n{said}"
        );
        assert!(
            said.contains("The document needs this field filled."),
            "the field the document needs says so:\n{said}"
        );
    }

    /// The width each page stands at, in pixels, as the engine draws it.
    fn page_widths(window: &tilolang_runtime::ViewInstance) -> Vec<f64> {
        of_kind(window.tree(), "ui.Image")
            .into_iter()
            .map(|image| match image.props.get("max.dp") {
                Some(Value::Float(width)) => *width,
                other => panic!("a page names its width in pixels: {other:?}"),
            })
            .collect()
    }

    /// Press the toolbar control `label`.
    fn press(window: &mut tilolang_runtime::ViewInstance, label: &str) {
        let control = of_kind(window.tree(), "ui.ToolbarButton")
            .into_iter()
            .find(|button| {
                matches!(button.props.get("label"), Some(Value::Text(said)) if said == label)
            })
            .unwrap_or_else(|| panic!("no {label} control in\n{}", dump(window.tree())));
        window.fire(&control.key.clone(), "press").unwrap();
        window.poll_patches().unwrap();
    }

    #[test]
    fn a_page_fits_the_width_it_is_given_and_stands_whole_in_the_room_it_has() {
        let reader = Reader::new();
        let doc = reader.open();
        // Both pages drawn, as the reader's own trigger draws them: a page with no picture stands
        // as its waiting state.
        for at in 0..2 {
            let page = reader.page(&doc, at);
            reader.call(
                "show_page",
                vec![Value::resource(&doc), Value::resource(&page), Value::Int(800)],
            );
        }
        let mut window = reader.window("reader", vec![Value::resource(&doc)]);
        // The pages are rows built as they scroll into view: a test reads them all.
        window.set_lazy_rows(false).unwrap();
        window.poll_patches().unwrap();
        // The reader's own view reads the room the pages area has, and each page's row the width
        // of the row it stands in: no other view asks for its size.
        let readers = window.space_readers();
        assert_eq!(readers.len(), 2, "the two rows of pages: {readers:?}");
        let rem = 16.0;

        // Fit width: every page fills the width of the row it stands in, at any window size.
        press(&mut window, "Fit width");
        for room in [30.0_f64, 50.0] {
            window.set_space("", Space::from_rem(room, 40.0));
            for key in &readers {
                window.set_space(key, Space::from_rem(room, room));
            }
            window.poll_patches().unwrap();
            let widths = page_widths(&window);
            assert_eq!(widths.len(), 2, "both pages are drawn");
            for width in widths {
                assert!(
                    (width - room * rem).abs() < 0.5,
                    "a page fills the {room} rem it is given: {width} px at {rem} px a rem"
                );
            }
        }

        // Fit page: the whole page stands in the room the column has, at two window sizes.
        press(&mut window, "Fit page");
        for (room, tall) in [(30.0_f64, 30.0_f64), (50.0, 20.0)] {
            window.set_space("", Space::from_rem(room, tall));
            for key in &readers {
                window.set_space(key, Space::from_rem(room, room));
            }
            window.poll_patches().unwrap();
            let widths = page_widths(&window);
            assert_eq!(widths.len(), 2);
            // A page is 612 by 792 points: its height is its width times 792/612.
            let fits = (tall * rem * 612.0 / 792.0).min(room * rem);
            for width in widths {
                assert!(
                    (width - fits).abs() < 0.5,
                    "the whole page stands in {room} by {tall} rem: {width} px, not {fits}"
                );
                assert!(
                    width * 792.0 / 612.0 <= tall * rem + 0.5,
                    "the page is no taller than the column: {width} px wide"
                );
            }
        }

        // The person's own zoom is back the moment they step it: the page is the zoom's size,
        // whatever room it is in.
        press(&mut window, "Fit page");
        press(&mut window, "Zoom in");
        let widths = page_widths(&window);
        // A page is 612 by 792 points, and a point is four thirds of a pixel.
        let zoomed = 612.0 * (4.0 / 3.0) * 1.25;
        for width in widths {
            assert!(
                (width - zoomed).abs() < 0.5,
                "the page is its own zoom again: {width} px, not {zoomed}"
            );
        }
    }

    #[test]
    fn the_reader_opens_the_documents_window() {
        let reader = Reader::new();
        let doc = reader.open();
        let window = reader.window("reader", vec![Value::resource(&doc)]);
        let tree = window.tree();
        let shown = crate::common::dump(tree);
        assert!(shown.contains("Report.pdf"), "its file:\n{shown}");
        assert!(shown.contains("Pages"), "its page rail:\n{shown}");
        // The reader's toolbar is the page's header: it stands in the window's bar, with the
        // actions the window offers beside it.
        let shell = crate::common::of_kind(tree, "ui.NavShell")
            .into_iter()
            .next()
            .expect("the reader's window shell");
        let bar = shell
            .children
            .iter()
            .find(|child| {
                child.kind == "part" && child.props.get("name") == Some(&Value::text("actions"))
            })
            .unwrap_or_else(|| panic!("the window's bar holds the page's header:\n{shown}"));
        let bar_shown = crate::common::dump(bar);
        assert!(
            bar_shown.contains("Zoom in") && bar_shown.contains("Fit width"),
            "the toolbar's controls stand in the window's bar:\n{bar_shown}"
        );
    }

    /// The addresses of the pictures a document holds, in page order.
    fn rail_pictures(reader: &Reader, doc: &ResourceRef) -> Vec<String> {
        let held = reader.rt.read(doc, "pictures").unwrap();
        let projected = reader.rt.project(&held);
        projected["items"]
            .as_array()
            .unwrap_or_else(|| panic!("the pictures project as {projected}"))
            .iter()
            .map(|item| {
                item["fields"]["url"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a picture has no address: {item}"))
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn the_rail_carries_each_pages_picture() {
        let reader = Reader::new();
        let doc = reader.open();
        for at in 0..2 {
            let page = reader.page(&doc, at);
            reader.call(
                "show_page",
                vec![
                    Value::resource(&doc),
                    Value::resource(&page),
                    Value::Int(612),
                ],
            );
        }
        let pictures = rail_pictures(&reader, &doc);
        assert_eq!(pictures.len(), 2, "each page is drawn: {pictures:?}");
        let window = reader.window("reader", vec![Value::resource(&doc)]);
        let tree = window.tree();
        let rail = crate::common::of_kind(tree, "ui.PageSidebar")
            .into_iter()
            .next()
            .expect("the reader's page rail");
        let carried = match rail.props.get("pictures") {
            Some(Value::List(items)) => items
                .iter()
                .map(|item| match item {
                    Value::Text(address) => address.clone(),
                    other => panic!("a page's picture reads {other:?}"),
                })
                .collect::<Vec<_>>(),
            other => panic!("the rail's pictures read {other:?}"),
        };
        assert_eq!(
            carried, pictures,
            "the rail carries each page's picture, in page order"
        );
    }

    /// A list of numbers a resource holds.
    fn floats(reader: &Reader, at: &ResourceRef, field: &str) -> Vec<f64> {
        match reader.rt.read(at, field).unwrap() {
            Value::List(items) => items
                .iter()
                .map(|item| match item {
                    Value::Float(value) => *value,
                    other => panic!("{field} holds {other:?}"),
                })
                .collect(),
            other => panic!("{field} reads {other:?}"),
        }
    }

    /// Every number within a page point of its expected place: what a fraction of the page and
    /// back leaves as its rounding.
    fn near(got: &[f64], want: &[f64]) -> bool {
        got.len() == want.len()
            && got
                .iter()
                .zip(want)
                .all(|(got, want)| (got - want).abs() < 0.01)
    }

    #[test]
    fn marking_a_selection_marks_exactly_its_runs() {
        let reader = Reader::new();
        let doc = reader.open();
        let first = reader.page(&doc, 0);
        reader.call(
            "read_page",
            vec![Value::resource(&doc), Value::resource(&first)],
        );
        let marked = returned_ref(reader.call(
            "mark_selection",
            vec![
                Value::resource(&doc),
                Value::resource(&first),
                Value::text("underline"),
                Value::text("#4ade80"),
                Value::Int(0),
                Value::Int(1),
            ],
        ));
        assert_eq!(reader.text(&marked, "style"), "underline");
        let rects = floats(&reader, &marked, "rects");
        assert!(
            near(&rects, &[72.0, 72.0, 40.0, 12.0, 120.0, 72.0, 40.0, 12.0]),
            "exactly the two runs' boxes: {rects:?}"
        );
        let one = returned_ref(reader.call(
            "mark_selection",
            vec![
                Value::resource(&doc),
                Value::resource(&first),
                Value::text("highlight"),
                Value::text("#facc15"),
                Value::Int(1),
                Value::Int(1),
            ],
        ));
        assert!(
            near(&floats(&reader, &one, "rects"), &[120.0, 72.0, 40.0, 12.0]),
            "one run marks one line"
        );

        // With lines selected, the window offers the four marks of them.
        let mut window = reader.window("reader", vec![Value::resource(&doc)]);
        window.set_lazy_rows(false).unwrap();
        window.set_view_state("sel_from", Value::Int(0));
        window.set_view_state("sel_to", Value::Int(1));
        window.poll_patches().expect("the window runs again");
        let said = crate::common::dump(window.tree());
        for label in ["Highlight", "Underline", "Strike through", "Squiggle"] {
            assert!(
                said.contains(label),
                "{label} marks the selected words:\n{said}"
            );
        }
    }

    /// A whole-number prop of a node, as the view gave it.
    fn whole(node: &UiNode, name: &str) -> i64 {
        match node.props.get(name) {
            Some(Value::Int(value)) => *value,
            other => panic!("{name} of {} is {other:?}", node.kind),
        }
    }

    /// A text prop of a node, as the view gave it.
    fn words(node: &UiNode, name: &str) -> String {
        match node.props.get(name) {
            Some(Value::Text(value)) => value.clone(),
            other => panic!("{name} of {} is {other:?}", node.kind),
        }
    }

    /// A number a resource holds.
    fn number(reader: &Reader, at: &ResourceRef, field: &str) -> f64 {
        match reader.rt.read(at, field).unwrap() {
            Value::Float(value) => value,
            other => panic!("{field} reads {other:?}"),
        }
    }

    #[test]
    fn form_fields_stand_at_their_boxes_and_a_signature_moves_through_the_view() {
        let reader = Reader::new();
        let doc = reader.open();
        reader.call("read_fields", vec![Value::resource(&doc)]);
        reader.call(
            "sign_as",
            vec![Value::resource(&reader.lib), Value::text("Ada")],
        );
        let first = reader.page(&doc, 0);
        let signed = returned_ref(reader.call(
            "place_signature",
            vec![
                Value::resource(&doc),
                Value::resource(&first),
                Value::Float(120.0),
                Value::Float(240.0),
            ],
        ));
        let mut window = reader.window("reader", vec![Value::resource(&doc)]);
        window.set_lazy_rows(false).unwrap();
        let (places, stage) = {
            let tree = window.tree();
            let places: Vec<(String, (i64, i64, i64, i64))> =
                crate::common::of_kind(tree, "ui.StagePin")
                    .into_iter()
                    .map(|node| {
                        (
                            words(node, "name"),
                            (
                                whole(node, "x"),
                                whole(node, "y"),
                                whole(node, "width"),
                                whole(node, "height"),
                            ),
                        )
                    })
                    .collect();
            let stage = crate::common::of_kind(tree, "ui.Stage")
                .into_iter()
                .next()
                .map(|node| node.key.clone());
            (places, stage)
        };
        assert_eq!(
            places,
            vec![
                ("Signed by".to_owned(), (72, 700, 180, 14)),
                ("Date".to_owned(), (72, 680, 120, 14)),
                ("Ada".to_owned(), (120, 240, 218, 74)),
            ],
            "each field and the signature stand where the file puts them"
        );

        // Dragging the signature reports its new point to the view, whose action moves it.
        let stage = stage.expect("the page's places stand in a stage");
        window
            .fire_with(
                &stage,
                "moved",
                Some(&Value::List(vec![
                    Value::text(&signed.id_string()),
                    Value::List(vec![Value::Int(300), Value::Int(400)]),
                ])),
            )
            .expect("the place reports its move to the view");
        assert!(
            (number(&reader, &signed, "x") - 300.0).abs() < 0.01,
            "the signature moved across its page: {}",
            number(&reader, &signed, "x")
        );
        assert!((number(&reader, &signed, "y") - 400.0).abs() < 0.01);
    }

    #[test]
    fn a_signature_moves_by_fractions_of_its_page() {
        let reader = Reader::new();
        let doc = reader.open();
        reader.call(
            "sign_as",
            vec![Value::resource(&reader.lib), Value::text("Ada")],
        );
        let first = reader.page(&doc, 0);
        let signed = returned_ref(reader.call(
            "place_signature",
            vec![
                Value::resource(&doc),
                Value::resource(&first),
                Value::Float(0.0),
                Value::Float(0.0),
            ],
        ));
        reader.call(
            "move_signature",
            vec![
                Value::resource(&doc),
                Value::resource(&signed),
                Value::Float(0.25),
                Value::Float(0.5),
            ],
        );
        assert!(
            (number(&reader, &signed, "x") - 153.0).abs() < 0.01,
            "a quarter across"
        );
        assert!(
            (number(&reader, &signed, "y") - 396.0).abs() < 0.01,
            "halfway down"
        );
    }
}
