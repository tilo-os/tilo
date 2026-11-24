//! The Typst editor (`apps/tl/typst-editor/app.tl`) end to end: a document typeset, its pages
//! drawn and its words read through the device's engine; what a failed typeset keeps; the
//! outline and the places the window goes to; and its window, with the source and the page.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::clipboard::ClipboardHost;
    use tilolang_runtime::host_work::EngineHost;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/typst-editor/app.tl");

    /// What the device's engine answers a typeset with.
    #[derive(Clone)]
    struct Answer {
        ok: bool,
        pages: Vec<(String, f64, f64)>,
        problems: Vec<(String, String, i64, i64)>,
        pdf: Option<String>,
        trouble: String,
    }

    impl Default for Answer {
        fn default() -> Self {
            Self {
                ok: true,
                pages: vec![
                    ("look-a".to_owned(), 595.0, 842.0),
                    ("look-b".to_owned(), 595.0, 842.0),
                ],
                problems: Vec::new(),
                pdf: Some("tilo-blob://pdf-1".to_owned()),
                trouble: String::new(),
            }
        }
    }

    /// The device's engine: what it was asked, and the answer it gives.
    #[derive(Clone, Default)]
    struct Engine {
        asked: Rc<RefCell<Vec<(String, String)>>>,
        answer: Rc<RefCell<Answer>>,
    }

    impl Engine {
        fn ops(&self) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .map(|(op, _)| op.clone())
                .collect()
        }

        fn inputs_of(&self, op: &str) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .filter(|(asked, _)| asked == op)
                .map(|(_, input)| input.clone())
                .collect()
        }

        fn compile_answer(&self) -> String {
            let answer = self.answer.borrow().clone();
            let (url, name, size) = answer.pdf.as_ref().map_or_else(
                || ("null".to_owned(), "null".to_owned(), "null".to_owned()),
                |url| {
                    (
                        format!("\"{url}\""),
                        "\"document.pdf\"".to_owned(),
                        "10".to_owned(),
                    )
                },
            );
            let trouble = if answer.trouble.is_empty() {
                "null".to_owned()
            } else {
                let said = &answer.trouble;
                format!("\"{said}\"")
            };
            format!(
                r#"{{"ok":{},"elapsed_ms":12,"pdf_url":{url},"pdf_name":{name},"pdf_size":{size},"pdf_trouble":{trouble}}}"#,
                answer.ok
            )
        }

        fn pages_answer(&self) -> String {
            let answer = self.answer.borrow().clone();
            let pages: Vec<String> = answer
                .pages
                .iter()
                .map(|(look, width, height)| {
                    format!(r#"{{"width":{width},"height":{height},"look":"{look}"}}"#)
                })
                .collect();
            format!("[{}]", pages.join(","))
        }

        fn problems_answer(&self) -> String {
            let answer = self.answer.borrow().clone();
            let problems: Vec<String> = answer
                .problems
                .iter()
                .map(|(kind, message, line, column)| {
                    format!(
                        r#"{{"kind":"{kind}","message":"{message}","line":{line},"column":{column},"end_line":{line},"end_column":{column},"hints":[]}}"#
                    )
                })
                .collect();
            format!("[{}]", problems.join(","))
        }
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            self.asked
                .borrow_mut()
                .push((op.to_owned(), input.to_owned()));
            match op {
                "compile" => Ok(self.compile_answer()),
                "pages" => Ok(self.pages_answer()),
                "problems" => Ok(self.problems_answer()),
                "outline" => Ok(
                    r#"[{"line":1,"level":1,"title":"Hello"},{"line":4,"level":2,"title":"More"}]"#
                        .to_owned(),
                ),
                "picture" => {
                    let page = page_of(input);
                    let width = width_of(input);
                    Ok(format!(
                        r#"{{"url":"tilo-blob://page-{page}-{width}","name":"page-{page}.png","media":"image/png","size":3}}"#
                    ))
                }
                "text" => Ok(
                    r#"[{"text":"H","x0":10.0,"x1":20.0,"top":10.0,"bottom":20.0,"line":0},{"text":"i","x0":20.0,"x1":30.0,"top":10.0,"bottom":20.0,"line":0},{"text":"lower","x0":10.0,"x1":30.0,"top":40.0,"bottom":50.0,"line":1}]"#
                        .to_owned(),
                ),
                other => Err(format!("`{other}` is no op this device runs")),
            }
        }

        fn start(&self, _app: &str, op: &str, _input: &str) -> Result<String, String> {
            Err(format!("`{op}` is no op this device runs"))
        }

        fn status(&self, job: &str) -> Result<String, String> {
            Err(format!("no job `{job}`"))
        }

        fn cancel(&self, _job: &str) -> bool {
            false
        }
    }

    fn page_of(input: &str) -> i64 {
        serde_json::from_str::<serde_json::Value>(input)
            .ok()
            .and_then(|ask| ask.get("page").and_then(serde_json::Value::as_i64))
            .unwrap_or(0)
    }

    fn width_of(input: &str) -> i64 {
        serde_json::from_str::<serde_json::Value>(input)
            .ok()
            .and_then(|ask| ask.get("width").and_then(serde_json::Value::as_i64))
            .unwrap_or(0)
    }

    /// The device's content store: the bytes of a file it keeps or hands over.
    #[derive(Clone, Default)]
    struct Device(Rc<RefCell<HashMap<String, Vec<u8>>>>);

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(hash).cloned()
        }

        fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
            Err("this test chooses nothing".to_owned())
        }

        fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
            Err("this test writes no files out".to_owned())
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            let hash = blake3::hash(bytes).to_hex().to_string();
            self.0.borrow_mut().insert(hash.clone(), bytes.to_vec());
            Ok(FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }

        fn thumbnail(&self, _file: &FileValue, _size: u32) -> Result<Option<FileValue>, String> {
            Ok(None)
        }
    }

    /// The person's clipboard: what an action copied.
    #[derive(Clone, Default)]
    struct Board(Rc<RefCell<String>>);

    impl ClipboardHost for Board {
        fn copy(&self, _mime: &str, bytes: &[u8]) -> bool {
            *self.0.borrow_mut() = String::from_utf8_lossy(bytes).into_owned();
            true
        }

        fn paste(&self, _types: &[&str]) -> Option<(String, Vec<u8>)> {
            None
        }
    }

    struct Desk {
        rt: Runtime,
        app: App,
        lib: ResourceRef,
        engine: Engine,
        device: Device,
        board: Board,
    }

    impl Desk {
        fn new() -> Self {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let engine = Engine::default();
            rt.set_engine(engine.clone());
            let device = Device::default();
            device
                .0
                .borrow_mut()
                .insert("pdf-1".to_owned(), b"%PDF-1.7 a document".to_vec());
            rt.set_blobs(device.clone());
            let board = Board::default();
            rt.set_clipboard(board.clone());
            let app = App::compile(SOURCE, &[]);
            app.load(&rt);
            let lib = rt.create_root("Library").unwrap();
            Self {
                rt,
                app,
                lib,
                engine,
                device,
                board,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            let out = self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap();
            assert!(
                matches!(out, InvokeOutcome::Done(_)),
                "{action} is done: {out:?}"
            );
            out
        }

        fn new_document(&self) -> ResourceRef {
            returned_ref(self.call("new_document", vec![Value::resource(&self.lib)]))
        }

        fn text(&self, r: &ResourceRef, field: &str) -> String {
            match self.rt.read(r, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn number(&self, r: &ResourceRef, field: &str) -> i64 {
            match self.rt.read(r, field).unwrap() {
                Value::Int(n) => n,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn float(&self, r: &ResourceRef, field: &str) -> f64 {
            match self.rt.read(r, field).unwrap() {
                Value::Float(n) => n,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn flag(&self, r: &ResourceRef, field: &str) -> bool {
            match self.rt.read(r, field).unwrap() {
                Value::Bool(flag) => flag,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn records(&self, r: &ResourceRef, field: &str) -> Vec<ResourceRef> {
            match self.rt.read(r, field).unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(r) => Some(std::rc::Rc::unwrap_or_clone(r.target)),
                        _ => None,
                    })
                    .collect(),
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn window(&self, d: &ResourceRef) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(&me(), &self.app.view("editor"), vec![Value::resource(d)])
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }

        fn words(&self, d: &ResourceRef, action: &str, mut args: Vec<Value>) -> String {
            args.insert(0, Value::resource(d));
            match self.call(action, args) {
                InvokeOutcome::Done(Value::Text(text)) => text,
                other => panic!("{action} answers text: {other:?}"),
            }
        }

        fn page(&self, d: &ResourceRef, at: usize) -> ResourceRef {
            self.records(d, "pages")
                .into_iter()
                .nth(at)
                .expect("the document has that page")
        }

        /// A `.typ` a person chose, as the device hands it over.
        fn typ_file(&self, name: &str, words: &str) -> Value {
            let hash = blake3::hash(words.as_bytes()).to_hex().to_string();
            self.device
                .0
                .borrow_mut()
                .insert(hash.clone(), words.as_bytes().to_vec());
            Value::file(FileValue::new(
                name,
                "text/plain",
                i64::try_from(words.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }
    }

    #[test]
    fn a_document_typesets_into_its_pages_a_pdf_and_its_outline() {
        let desk = Desk::new();
        let d = desk.new_document();
        assert_eq!(desk.text(&d, "name"), "Untitled.typ");
        assert!(
            desk.flag(&d, "typesets"),
            "the answer says it typesets, and the trouble is {:?}",
            desk.text(&d, "trouble")
        );
        assert_eq!(desk.number(&d, "took_ms"), 12);
        assert_eq!(desk.text(&d, "trouble"), "");
        assert_eq!(
            desk.engine.ops().first().map(String::as_str),
            Some("compile"),
            "a new document is typeset"
        );
        assert!(
            desk.engine.ops().contains(&"outline".to_owned()),
            "its headings are read"
        );
        let pages = desk.records(&d, "pages");
        assert_eq!(pages.len(), 2, "both pages are laid out");
        assert_eq!(desk.text(&pages[0], "look"), "look-a");
        assert_eq!(desk.text(&d, "pdf"), "tilo-blob://pdf-1");
        let headings = desk.records(&d, "headings");
        assert_eq!(headings.len(), 2, "both headings are read");
        assert_eq!(desk.text(&headings[0], "title"), "Hello");
        assert_eq!(desk.number(&headings[1], "level"), 2);
    }

    #[test]
    fn a_typeset_that_fails_keeps_the_page_and_says_why() {
        let desk = Desk::new();
        let d = desk.new_document();
        desk.engine.answer.borrow_mut().ok = false;
        desk.engine.answer.borrow_mut().pages = Vec::new();
        desk.engine.answer.borrow_mut().pdf = None;
        desk.engine.answer.borrow_mut().problems = vec![(
            "error".to_owned(),
            "unknown variable: missing".to_owned(),
            3,
            7,
        )];
        desk.text(&d, "typeset"); // read once, so the helper is used on a field that exists
        desk.call("typeset", vec![Value::resource(&d)]);
        assert!(!desk.flag(&d, "typesets"));
        assert!(desk.flag(&d, "stale"), "the last good page is kept, marked");
        assert_eq!(
            desk.text(&d, "trouble"),
            "unknown variable: missing",
            "the blocking problem is what a person reads"
        );
        assert_eq!(desk.records(&d, "pages").len(), 2, "the pages stay");
        assert_eq!(desk.text(&d, "pdf"), "", "a stale PDF is not exported");
        let problems = desk.records(&d, "problems");
        assert_eq!(problems.len(), 1);
        assert_eq!(desk.number(&problems[0], "line"), 3);
    }

    #[test]
    fn a_page_is_drawn_at_the_width_asked_for_and_kept() {
        let desk = Desk::new();
        let d = desk.new_document();
        let page = desk.page(&d, 0);
        desk.call(
            "draw_page",
            vec![Value::resource(&d), Value::resource(&page), Value::Int(900)],
        );
        assert_eq!(desk.text(&page, "picture"), "tilo-blob://page-1-900");
        assert_eq!(desk.number(&page, "picture_width"), 900);
        assert_eq!(
            desk.text(&page, "thumb"),
            "tilo-blob://page-1-132",
            "the thumbnail is drawn too"
        );
        let asked = desk.engine.inputs_of("picture").len();
        desk.call(
            "draw_page",
            vec![Value::resource(&d), Value::resource(&page), Value::Int(900)],
        );
        assert_eq!(
            desk.engine.inputs_of("picture").len(),
            asked,
            "a page drawn at a width is not drawn again"
        );
    }

    #[test]
    fn an_unchanged_page_keeps_its_picture_and_a_changed_one_is_drawn_again() {
        let desk = Desk::new();
        let d = desk.new_document();
        let first = desk.page(&d, 0);
        let second = desk.page(&d, 1);
        desk.call(
            "draw_page",
            vec![
                Value::resource(&d),
                Value::resource(&first),
                Value::Int(900),
            ],
        );
        desk.call(
            "draw_page",
            vec![
                Value::resource(&d),
                Value::resource(&second),
                Value::Int(900),
            ],
        );
        // A new typeset leaves the first page as it was and changes the second.
        desk.engine.answer.borrow_mut().pages = vec![
            ("look-a".to_owned(), 595.0, 842.0),
            ("look-c".to_owned(), 595.0, 900.0),
        ];
        desk.call("typeset", vec![Value::resource(&d)]);
        let kept = desk.page(&d, 0);
        let changed = desk.page(&d, 1);
        assert_eq!(
            desk.text(&kept, "picture"),
            "tilo-blob://page-1-900",
            "a page that draws the same keeps its picture"
        );
        assert_eq!(
            desk.text(&changed, "picture"),
            "",
            "a page that draws something else is drawn again"
        );
        assert_eq!(desk.float(&changed, "height"), 900.0);
    }

    #[test]
    fn the_window_draws_the_next_page_at_its_own_width() {
        let desk = Desk::new();
        let d = desk.new_document();
        desk.call("draw_next", vec![Value::resource(&d)]);
        let first = desk.page(&d, 0);
        assert_eq!(
            desk.number(&first, "picture_width"),
            900,
            "a document with no zoom draws at the width a page reads at"
        );
        let second = desk.page(&d, 1);
        assert_eq!(desk.text(&second, "picture"), "", "one page a second");
        desk.call("draw_next", vec![Value::resource(&d)]);
        assert_eq!(desk.text(&second, "picture"), "tilo-blob://page-2-900");
    }

    #[test]
    fn zoom_in_out_and_fit_change_the_width_the_pages_are_drawn_at() {
        let desk = Desk::new();
        let d = desk.new_document();
        desk.call("zoom_in", vec![Value::resource(&d)]);
        assert_eq!(desk.number(&d, "shown_width"), 1020);
        desk.call("zoom_out", vec![Value::resource(&d)]);
        assert_eq!(desk.number(&d, "shown_width"), 900);
        desk.call("fit_width", vec![Value::resource(&d), Value::Int(1200)]);
        assert_eq!(desk.number(&d, "shown_width"), 1200);
        desk.call("draw_next", vec![Value::resource(&d)]);
        let first = desk.page(&d, 0);
        assert_eq!(desk.text(&first, "picture"), "tilo-blob://page-1-1200");
        for _ in 0..8 {
            desk.call("zoom_out", vec![Value::resource(&d)]);
        }
        assert_eq!(
            desk.number(&d, "shown_width"),
            240,
            "the zoom stops at a page's smallest readable width"
        );
    }

    #[test]
    fn a_problem_goes_to_its_line_and_the_window_comes_back_to_the_page() {
        let desk = Desk::new();
        let d = desk.new_document();
        desk.engine.answer.borrow_mut().ok = false;
        desk.engine.answer.borrow_mut().problems =
            vec![("warning".to_owned(), "unused variable".to_owned(), 7, 3)];
        desk.call("typeset", vec![Value::resource(&d)]);
        let problem = desk
            .records(&d, "problems")
            .into_iter()
            .next()
            .expect("a problem");
        desk.call("problems", vec![Value::resource(&d)]);
        assert_eq!(desk.text(&d, "showing"), "problems");
        desk.call(
            "go_to_problem",
            vec![Value::resource(&d), Value::resource(&problem)],
        );
        assert_eq!(
            desk.text(&d, "showing"),
            "split",
            "the editor shows the place"
        );
        assert!(
            desk.text(&d, "reveal").starts_with("7:3:"),
            "the editor is asked for the problem's line and column: {}",
            desk.text(&d, "reveal")
        );
        assert_eq!(desk.number(&d, "reveals"), 1);
    }

    #[test]
    fn a_heading_goes_to_its_line_every_time_it_is_asked() {
        let desk = Desk::new();
        let d = desk.new_document();
        let heading = desk
            .records(&d, "headings")
            .into_iter()
            .next()
            .expect("a heading");
        desk.call(
            "go_to_heading",
            vec![Value::resource(&d), Value::resource(&heading)],
        );
        let first = desk.text(&d, "reveal");
        desk.call(
            "go_to_heading",
            vec![Value::resource(&d), Value::resource(&heading)],
        );
        assert_ne!(
            desk.text(&d, "reveal"),
            first,
            "the same heading asked for again shows again"
        );
        assert!(desk.text(&d, "reveal").starts_with("1:1:"));
    }

    #[test]
    fn the_views_switch_between_the_source_the_page_the_problems_and_the_outline() {
        let desk = Desk::new();
        let d = desk.new_document();
        assert_eq!(
            desk.text(&d, "showing"),
            "split",
            "a document opens beside its page"
        );
        desk.call("editor_view", vec![Value::resource(&d)]);
        assert_eq!(desk.text(&d, "showing"), "editor");
        desk.call("preview_view", vec![Value::resource(&d)]);
        assert_eq!(desk.text(&d, "showing"), "preview");
        desk.call("outline", vec![Value::resource(&d)]);
        assert_eq!(desk.text(&d, "showing"), "outline");
        desk.call("outline", vec![Value::resource(&d)]);
        assert_eq!(
            desk.text(&d, "showing"),
            "split",
            "the same button puts it away"
        );
        desk.call("problems", vec![Value::resource(&d)]);
        assert_eq!(desk.text(&d, "showing"), "problems");
        desk.call("side_by_side", vec![Value::resource(&d)]);
        assert_eq!(desk.text(&d, "showing"), "split");
    }

    #[test]
    fn a_typ_file_opens_as_the_document_it_is() {
        let desk = Desk::new();
        let file = desk.typ_file("report.typ", "= Report\n\nBody.\n");
        let d = returned_ref(desk.call("open_typ", vec![Value::resource(&desk.lib), file]));
        assert_eq!(desk.text(&d, "name"), "report.typ");
        assert_eq!(desk.text(&d, "source"), "= Report\n\nBody.\n");
        assert_eq!(
            desk.text(&d, "written"),
            "= Report\n\nBody.\n",
            "a document opened from a file is as written"
        );
        let documents = desk.records(&desk.lib, "documents");
        assert_eq!(documents.len(), 1, "it is the library's one document");
    }

    #[test]
    fn a_document_is_saved_as_the_text_it_holds_and_exported_as_its_pdf() {
        let desk = Desk::new();
        let d = desk.new_document();
        let saved = match desk.call("save_file", vec![Value::resource(&d)]) {
            InvokeOutcome::Done(Value::File(file)) => file,
            other => panic!("a save answers a file: {other:?}"),
        };
        assert_eq!(saved.name, "Untitled.typ");
        assert_eq!(
            String::from_utf8_lossy(
                &desk
                    .device
                    .0
                    .borrow()
                    .get(&saved.hash)
                    .cloned()
                    .unwrap_or_default()
            ),
            desk.text(&d, "source"),
            "the file holds the document's source"
        );
        let pdf = match desk.call("export_pdf", vec![Value::resource(&d)]) {
            InvokeOutcome::Done(Value::File(file)) => file,
            other => panic!("an export answers a file: {other:?}"),
        };
        assert_eq!(pdf.name, "Untitled.pdf");
        assert_eq!(pdf.url(), "tilo-blob://pdf-1", "the typeset's PDF");
    }

    #[test]
    fn a_page_s_words_are_read_whole_and_within_a_selection() {
        let desk = Desk::new();
        let d = desk.new_document();
        let cells = match desk.call("page_words", vec![Value::resource(&d), Value::Int(0)]) {
            InvokeOutcome::Done(Value::List(cells)) => cells,
            other => panic!("a page's words answer a list: {other:?}"),
        };
        assert_eq!(cells.len(), 3, "three placed glyphs");
        assert_eq!(
            desk.words(&d, "page_text", vec![Value::Int(0)]),
            "Hi lower",
            "the whole page's words, a space between its lines"
        );
        assert_eq!(
            desk.words(
                &d,
                "selection_text",
                vec![
                    Value::Int(0),
                    Value::List(vec![Value::Float(0.0), Value::Float(0.0)]),
                    Value::List(vec![Value::Float(1.0), Value::Float(0.03)]),
                ],
            ),
            "Hi",
            "only the words inside the box"
        );
    }

    #[test]
    fn a_copy_asks_the_person_the_runtime_has() {
        let desk = Desk::new();
        let d = desk.new_document();
        // A copy is an outside effect: a runtime with no one to ask refuses it, and a person's
        // press in a window takes the copy.
        let id = desk.app.action("copy_page");
        let out = desk
            .rt
            .invoke_action(
                &me(),
                (id.as_str(), vec![Value::resource(&d), Value::Int(0)]),
            )
            .unwrap();
        assert!(
            matches!(out, InvokeOutcome::Denied),
            "a copy with nobody to ask is refused: {out:?}"
        );
    }

    #[test]
    fn a_document_leaves_the_workspace() {
        let desk = Desk::new();
        let d = desk.new_document();
        assert_eq!(desk.records(&desk.lib, "documents").len(), 1);
        desk.call(
            "close_document",
            vec![Value::resource(&desk.lib), Value::resource(&d)],
        );
        assert!(
            desk.records(&desk.lib, "documents").is_empty(),
            "a closed document leaves the library"
        );
    }

    #[test]
    fn the_window_shows_the_source_the_page_and_what_a_person_can_do() {
        let desk = Desk::new();
        let d = desk.new_document();
        desk.call("draw_next", vec![Value::resource(&d)]);
        desk.call("draw_next", vec![Value::resource(&d)]);
        let window = desk.window(&d);
        let tree = window.tree();
        let sources = of_kind(&tree, "ui.CodeEditor");
        assert_eq!(sources.len(), 1, "the source is edited in a code editor");
        let pictures = of_kind(&tree, "ui.Image");
        assert!(
            pictures.len() >= 3,
            "each page and each thumbnail is a picture: {}",
            pictures.len()
        );
        let overlays = of_kind(&tree, "ui.Drawing");
        assert!(
            overlays.len() >= 2,
            "each page carries the selection overlay over it: {}",
            overlays.len()
        );
    }
}
