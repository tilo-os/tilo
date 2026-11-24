//! The Hex Editor (`apps/tl/hex-editor/app.tl`) end to end: a file read as bytes, the edits a
//! person typed, a pattern found and replaced, the numbers the inspector reads, and the file
//! written back. The device's work is the real engine (`crates/engines/hex-engine`), so the test
//! goes through the same answers a session gives.

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
        AmStore, InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/hex-editor/app.tl");

    /// The device's content store, as a window lends it, kept for the test's length.
    #[derive(Clone)]
    struct Device {
        store: tilo_content::ContentStore,
        /// The window's scope: the engine's calls run inside it, as a session runs an app's.
        scope: Rc<tilo_wake::WakeScope>,
        saved: Rc<RefCell<HashMap<String, Vec<u8>>>>,
    }

    impl Device {
        fn new() -> Self {
            let store = tilo_content::ContentStore::memory_in_workspace();
            let scope = Rc::new(tilo_wake::WakeScope::new());
            tilo_resource::content::register(&scope.handle(), store.clone());
            Self {
                store,
                scope,
                saved: Rc::default(),
            }
        }
    }

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.store.read(hash)
        }

        fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
            Err("this test chooses nothing".to_owned())
        }

        fn save(&self, request: &SaveRequest, content: &[u8]) -> Result<(), String> {
            self.saved
                .borrow_mut()
                .insert(request.name.clone(), content.to_vec());
            Ok(())
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            let hash = self.store.put(bytes).map_err(|error| error.to_string())?;
            Ok(FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }
    }

    /// The device's engine, answered by the crate the session registers, inside the window's
    /// content store as a session answers it.
    #[derive(Clone)]
    struct Engine {
        engine: Rc<hex_engine::HexEngine>,
        scope: Rc<tilo_wake::WakeScope>,
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            use tilolang_tilo::engine::Provider as _;
            let _window = self.scope.enter();
            self.engine.call(op, input)
        }

        fn start(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            use tilolang_tilo::engine::Provider as _;
            let _window = self.scope.enter();
            self.engine.start(op, input)
        }

        fn status(&self, job: &str) -> Result<String, String> {
            use tilolang_tilo::engine::Provider as _;
            let _window = self.scope.enter();
            self.engine.status(job)
        }

        fn cancel(&self, job: &str) -> bool {
            use tilolang_tilo::engine::Provider as _;
            let _window = self.scope.enter();
            self.engine.cancel(job)
        }
    }

    /// The person's selection, as the device answers it.
    #[derive(Clone, Default)]
    struct Selection(Rc<RefCell<Option<(String, Vec<u8>)>>>);

    impl ClipboardHost for Selection {
        fn copy(&self, mime: &str, bytes: &[u8]) -> bool {
            *self.0.borrow_mut() = Some((mime.to_owned(), bytes.to_vec()));
            true
        }

        fn paste(&self, types: &[&str]) -> Option<(String, Vec<u8>)> {
            let held = self.0.borrow().clone()?;
            types
                .iter()
                .any(|kind| held.0.starts_with(kind))
                .then_some(held)
        }
    }

    struct Desk {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        device: Device,
        directory: tempfile::TempDir,
    }

    impl Desk {
        fn new() -> Self {
            // The app's code is trusted and its own outside work granted, as a person's press of
            // its buttons is.
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let device = Device::new();
            rt.set_blobs(device.clone());
            rt.set_clipboard(Selection::default());
            rt.set_engine(Engine {
                engine: Rc::new(hex_engine::HexEngine::new()),
                scope: device.scope.clone(),
            });
            let app = App::compile(SOURCE, &[]);
            authority.trust(&app.load(&rt));
            for action in ["press", "copy", "cut", "paste", "open_path", "save"] {
                authority.grant_outside(&app.action(action));
            }
            // What those call reaches the device too, as the app's own code is granted it.
            for action in ["chord", "moved", "replaced", "replace_spans"] {
                authority.grant_outside(&app.action(action));
            }
            let root = rt.create_root("Library").unwrap();
            Self {
                rt,
                app,
                root,
                device,
                directory: tempfile::tempdir().unwrap(),
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        /// A file on this device, holding `bytes`, and the path it stands at.
        fn file(&self, name: &str, bytes: &[u8]) -> String {
            let path = self.directory.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path.to_str().unwrap().to_owned()
        }

        fn open(&self, path: &str) -> ResourceRef {
            returned_ref(self.call(
                "open_path",
                vec![Value::resource(&self.root), Value::text(path)],
            ))
        }

        fn opened_file(&self, file: FileValue) -> ResourceRef {
            returned_ref(self.call(
                "open_file",
                vec![Value::resource(&self.root), Value::file(file)],
            ))
        }

        fn new_buffer(&self) -> ResourceRef {
            returned_ref(self.call("new_buffer", vec![Value::resource(&self.root)]))
        }

        fn desk(&self, d: &ResourceRef) -> ResourceRef {
            match self.rt.read(d, "desks").unwrap() {
                Value::List(items) => match items.into_iter().next() {
                    Some(Value::Ref(r)) => Rc::unwrap_or_clone(r.target),
                    other => panic!("a desk is a record, not {other:?}"),
                },
                other => panic!("desks reads {other:?}"),
            }
        }

        fn text(&self, r: &ResourceRef, field: &str) -> String {
            match self.rt.read(r, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn number(&self, r: &ResourceRef, field: &str) -> f64 {
            match self.rt.read(r, field).unwrap() {
                Value::Int(n) => f64::from(i32::try_from(n).unwrap_or(0)),
                Value::Float(n) => n,
                other => panic!("{field} reads {other:?}"),
            }
        }

        /// The bytes as they stand with the edits applied, made a file by the app's own
        /// export: what a save writes.
        fn bytes(&self, d: &ResourceRef) -> Vec<u8> {
            match self.call("save_bytes", vec![Value::resource(d)]) {
                InvokeOutcome::Done(Value::File(file)) => {
                    self.device.store.read(&file.hash).unwrap_or_default()
                }
                other => panic!("save_bytes answers a file: {other:?}"),
            }
        }

        /// The rows the window shows, each as its hexadecimal text.
        fn rows(&self, d: &ResourceRef) -> Vec<String> {
            let desk = self.desk(d);
            match self.rt.read(&desk, "rows").unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .map(|item| match item {
                        Value::Text(text) => text,
                        other => panic!("a row is text, not {other:?}"),
                    })
                    .collect(),
                other => panic!("rows reads {other:?}"),
            }
        }

        fn press(&self, d: &ResourceRef, key: &str) {
            self.call("press", vec![Value::resource(d), Value::text(key)]);
        }

        fn cursor(&self, d: &ResourceRef) -> usize {
            let desk = self.desk(d);
            usize::try_from(self.number(&desk, "cursor") as i64).unwrap_or(0)
        }

        fn edited(&self, d: &ResourceRef) -> usize {
            usize::try_from(self.number(d, "edited") as i64).unwrap_or(0)
        }

        fn window(&self, d: &ResourceRef) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(&me(), &self.app.view("document"), vec![Value::resource(d)])
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }
    }

    /// The first node of `kind` in `node`'s tree.
    fn find<'a>(node: &'a UiNode, kind: &str) -> Option<&'a UiNode> {
        if node.kind == kind {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, kind))
    }

    fn texts(node: &UiNode, out: &mut Vec<String>) {
        if node.kind == "ui.Text" {
            if let Some(Value::Text(text)) = node.props.get("value") {
                out.push(text.clone());
            }
        }
        for child in &node.children {
            texts(child, out);
        }
    }

    tilo_test_env::scenario! {
        fn the_editor_reads_edits_finds_and_saves_a_file;

        phase a_file_named_on_this_device_opens_as_its_own_buffer;
        {
            let s = Desk::new();
            let path = s.file("Bytes Grün.bin", &[0x00, 0x11, 0x22, 0x33]);
            let d = s.open(&path);
            assert_eq!(s.text(&d, "name"), "Bytes Grün.bin");
            assert_eq!(s.text(&d, "path"), path);
            assert_eq!(s.number(&d, "length"), 4.0);
            assert_eq!(s.bytes(&d), [0x00, 0x11, 0x22, 0x33]);
            assert_eq!(s.rows(&d), ["00112233"]);
            assert_eq!(s.number(&s.desk(&d), "cursor"), 0.0);
        }

        phase a_file_another_app_hands_over_opens_from_its_address;
        {
            let s = Desk::new();
            let kept = s.device.keep("handed.bin", "application/octet-stream", &[0xDE, 0xAD]);
            let file = match kept {
                Ok(file) => file,
                Err(why) => panic!("{why}"),
            };
            let d = s.opened_file(file);
            assert_eq!(s.text(&d, "name"), "handed.bin");
            assert_eq!(s.number(&d, "length"), 2.0);
            assert_eq!(s.bytes(&d), [0xDE, 0xAD]);
        }

        phase a_new_empty_buffer_is_a_buffer_with_no_bytes;
        {
            let s = Desk::new();
            let d = s.new_buffer();
            assert_eq!(s.text(&d, "name"), "Untitled.bin");
            assert_eq!(s.number(&d, "length"), 0.0);
            assert_eq!(s.edited(&d), 0);
            assert!(s.rows(&d).is_empty());
        }

        phase typed_hex_digits_overwrite_a_byte_and_advance;
        {
            let s = Desk::new();
            let path = s.file("bytes.bin", &[0x00, 0x11, 0x22, 0x33]);
            let d = s.open(&path);
            s.press(&d, "a");
            assert_eq!(s.bytes(&d), [0xA0, 0x11, 0x22, 0x33], "the high nibble first");
            assert_eq!(s.cursor(&d), 0, "nor moved until the byte is whole");
            s.press(&d, "b");
            assert_eq!(s.bytes(&d), [0xAB, 0x11, 0x22, 0x33]);
            assert_eq!(s.cursor(&d), 1, "and then on to the next");
            assert_eq!(s.edited(&d), 1);
            s.press(&d, "f");
            s.press(&d, "F");
            assert_eq!(s.bytes(&d), [0xAB, 0xFF, 0x22, 0x33]);
        }

        phase insert_mode_splices_bytes_in;
        {
            let s = Desk::new();
            let path = s.file("insert.bin", &[0xAA]);
            let d = s.open(&path);
            s.press(&d, "Insert");
            s.press(&d, "1");
            s.press(&d, "2");
            assert_eq!(s.bytes(&d), [0x12, 0xAA]);
            assert_eq!(s.number(&d, "length"), 2.0);
            assert_eq!(s.cursor(&d), 1);
            s.press(&d, "Backspace");
            assert_eq!(s.bytes(&d), [0xAA], "insert mode takes the byte before the cursor");
        }

        phase the_ascii_column_types_characters;
        {
            let s = Desk::new();
            let path = s.file("text.bin", &[0x00, 0x00, 0x00, 0x00]);
            let d = s.open(&path);
            s.press(&d, "Tab");
            assert_eq!(s.text(&s.desk(&d), "column"), "ascii");
            s.press(&d, "N");
            s.press(&d, "G");
            assert_eq!(s.bytes(&d), [b'N', b'G', 0x00, 0x00]);
            assert_eq!(s.cursor(&d), 2);
        }

        phase the_cursor_moves_and_the_window_follows;
        {
            let s = Desk::new();
            let path = s.file("move.bin", &[0u8; 4096]);
            let d = s.open(&path);
            s.press(&d, "ArrowRight");
            s.press(&d, "ArrowDown");
            assert_eq!(s.cursor(&d), 17, "a row down and a byte on");
            s.press(&d, "End");
            assert_eq!(s.cursor(&d), 31, "the row's last byte");
            s.press(&d, "Home");
            assert_eq!(s.cursor(&d), 16);
            s.press(&d, "PageDown");
            assert_eq!(s.cursor(&d), 16 + 16 * 16);
            let desk = s.desk(&d);
            assert_eq!(s.number(&desk, "first"), 1.0, "the rows shown follow the cursor");
            s.call("scrolled", vec![Value::resource(&d), Value::Int(30)]);
            let desk = s.desk(&d);
            assert_eq!(s.number(&desk, "first"), 31.0, "the wheel moves the window");
            assert_eq!(s.rows(&d).len(), 96, "the rows the window reads ahead");
        }

        phase shift_makes_a_selection_and_delete_takes_it;
        {
            let s = Desk::new();
            let path = s.file("select.bin", &[1, 2, 3, 4, 5]);
            let d = s.open(&path);
            s.press(&d, "Shift+ArrowRight");
            s.press(&d, "Shift+ArrowRight");
            let desk = s.desk(&d);
            match s.rt.read(&desk, "anchor").unwrap() {
                Value::Opt(Some(anchor)) => {
                    assert!(matches!(*anchor, Value::Int(0)), "the selection began at 0");
                }
                other => panic!("anchor reads {other:?}"),
            }
            s.press(&d, "Delete");
            assert_eq!(s.bytes(&d), [4, 5]);
            assert_eq!(s.number(&d, "length"), 2.0);
        }

        phase ctrl_copy_carries_the_bytes_then_paste_puts_them_back;
        {
            let s = Desk::new();
            let path = s.file("copy.bin", &[0x50, 0x4E, 0x47, 0x44]);
            let d = s.open(&path);
            s.press(&d, "Shift+ArrowRight");
            s.press(&d, "Shift+ArrowRight");
            s.call("copy", vec![Value::resource(&d)]);
            let desk = s.desk(&d);
            assert_eq!(s.text(&desk, "notice"), "Copied 3 bytes");
            s.press(&d, "ArrowRight");
            s.press(&d, "Insert");
            s.press(&d, "Ctrl+v");
            assert_eq!(
                s.bytes(&d),
                [0x50, 0x4E, 0x47, 0x50, 0x4E, 0x47, 0x44],
                "the paste spliced the bytes in where the cursor stood"
            );
        }

        phase a_pattern_is_found_in_hex_and_in_text_and_revealed;
        {
            let s = Desk::new();
            let path = s.file("find.bin", b"IHDR ihdr IHDRx");
            let d = s.open(&path);
            s.call(
                "search",
                vec![Value::resource(&d), Value::text("49 48 44 52")],
            );
            let desk = s.desk(&d);
            assert_eq!(s.number(&desk, "at_match"), 0.0);
            assert_eq!(s.cursor(&d), 0, "the first match holds the cursor");
            s.call("search_kind", vec![Value::resource(&d), Value::Int(1)]);
            s.call("search", vec![Value::resource(&d), Value::text("ihdr")]);
            let desk = s.desk(&d);
            match s.rt.read(&desk, "hits").unwrap() {
                Value::List(hits) => assert_eq!(hits.len(), 3),
                other => panic!("hits read {other:?}"),
            }
            s.call("reveal", vec![Value::resource(&d), Value::Int(2)]);
            assert_eq!(s.cursor(&d), 10, "the third match is where it stands");
        }

        phase every_match_is_replaced_at_once;
        {
            let s = Desk::new();
            let path = s.file("replace.bin", b"aa aa aa");
            let d = s.open(&path);
            s.call("search", vec![Value::resource(&d), Value::text("61 61")]);
            s.call("replace_all", vec![Value::resource(&d), Value::text("62 62")]);
            assert_eq!(s.bytes(&d), b"bb bb bb");
            let desk = s.desk(&d);
            assert_eq!(s.text(&desk, "notice"), "Replaced 3 matches");
        }

        phase go_to_takes_hexadecimal_and_decimal_offsets;
        {
            let s = Desk::new();
            let path = s.file("goto.bin", &[0u8; 64]);
            let d = s.open(&path);
            s.call("go_to", vec![Value::resource(&d), Value::text("0x1f")]);
            assert_eq!(s.cursor(&d), 31);
            s.call("go_to", vec![Value::resource(&d), Value::text("12")]);
            assert_eq!(s.cursor(&d), 12);
            s.call("go_to", vec![Value::resource(&d), Value::text("not an offset")]);
            assert_eq!(s.cursor(&d), 12, "an unreadable offset leaves the cursor");
        }

        phase the_inspector_reads_the_bytes_at_the_cursor_and_its_byte_order;
        {
            let s = Desk::new();
            let path = s.file("inspect.bin", &[0x34, 0x12, 0, 0, 0, 0, 0, 0]);
            let d = s.open(&path);
            let desk = s.desk(&d);
            let findings = |s: &Desk| -> Vec<(String, String)> {
                match s.rt.read(&desk, "findings").unwrap() {
                    Value::List(items) => items
                        .into_iter()
                        .map(|item| match item {
                            Value::Text(text) => {
                                let (label, value) = text.split_once('\t').unwrap_or((&text, ""));
                                (label.to_owned(), value.to_owned())
                            }
                            other => panic!("a finding is text, not {other:?}"),
                        })
                        .collect(),
                    other => panic!("findings read {other:?}"),
                }
            };
            let little = findings(&s);
            assert!(
                little
                    .iter()
                    .any(|(label, value)| label == "u16" && value == "4660"),
                "{little:?}"
            );
            s.call("set_endian", vec![Value::resource(&d), Value::Int(1)]);
            let big = findings(&s);
            assert!(
                big.iter()
                    .any(|(label, value)| label == "u16" && value == "13330"),
                "{big:?}"
            );
        }

        phase the_published_verbs_edit_the_bytes_and_refuse_offsets_past_the_end;
        {
            let s = Desk::new();
            let path = s.file("agent.bin", &[0x00, 0x11, 0x22, 0x33]);
            let d = s.open(&path);
            s.call(
                "insert_bytes",
                vec![Value::resource(&d), Value::Int(2), Value::text("AB")],
            );
            assert_eq!(s.bytes(&d), [0x00, 0x11, 0xAB, 0x22, 0x33]);
            s.call(
                "overwrite_bytes",
                vec![Value::resource(&d), Value::Int(0), Value::text("FF")],
            );
            assert_eq!(s.bytes(&d), [0xFF, 0x11, 0xAB, 0x22, 0x33]);
            s.call(
                "delete_bytes",
                vec![Value::resource(&d), Value::Int(5), Value::Int(1)],
            );
            assert_eq!(s.bytes(&d), [0xFF, 0x11, 0xAB, 0x22, 0x33], "past the end changes nothing");
            let before = s.number(&d, "length");
            s.call(
                "insert_bytes",
                vec![Value::resource(&d), Value::Int(9), Value::text("FF")],
            );
            assert_eq!(s.number(&d, "length"), before);
        }

        phase saving_writes_the_file_and_what_was_saved_is_the_base;
        {
            let s = Desk::new();
            let path = s.file("save.bin", &[0x00, 0x11]);
            let d = s.open(&path);
            s.press(&d, "a");
            s.press(&d, "b");
            assert_eq!(s.edited(&d), 1);
            s.call("save", vec![Value::resource(&d)]);
            assert_eq!(std::fs::read(&path).unwrap(), [0xAB, 0x11]);
            assert_eq!(s.edited(&d), 0, "no byte differs from the file");
            let desk = s.desk(&d);
            assert_eq!(s.text(&desk, "notice"), "Saved 2 B");
        }

        phase a_file_with_no_path_on_this_device_is_saved_where_the_person_places_it;
        {
            let s = Desk::new();
            let d = s.new_buffer();
            s.press(&d, "1");
            s.press(&d, "2");
            let made = match s.call("save_bytes", vec![Value::resource(&d)]) {
                InvokeOutcome::Done(Value::File(file)) => (*file).clone(),
                other => panic!("save_bytes answers a file: {other:?}"),
            };
            assert_eq!(made.name, "Untitled.bin");
            assert_eq!(s.device.store.read(&made.hash).unwrap(), [0x12]);
        }

        phase the_window_shows_the_grid_the_bytes_and_the_status;
        {
            let s = Desk::new();
            let path = s.file("window.bin", &[0xDE, 0xAD, 0xBE, 0xEF]);
            let d = s.open(&path);
            let window = s.window(&d);
            let grid = find(&window.tree(), "ui.HexGrid").expect("the grid stands in the window");
            match grid.props.get("rows") {
                Some(Value::List(rows)) => {
                    assert_eq!(rows.first(), Some(&Value::text("DEADBEEF")));
                }
                other => panic!("the grid's rows read {other:?}"),
            }
            match grid.props.get("total") {
                Some(Value::Int(total)) => assert_eq!(*total, 1, "one row of sixteen bytes at most"),
                other => panic!("total reads {other:?}"),
            }
            let mut words = Vec::new();
            texts(&window.tree(), &mut words);
            assert!(
                words.iter().any(|w| w.contains("Offset 0x00000000")),
                "{words:?}"
            );
            assert!(words.iter().any(|w| w == "4 bytes"), "{words:?}");
        }

        phase the_library_lists_the_buffers_a_person_opened;
        {
            let s = Desk::new();
            let path = s.file("listed.bin", &[1, 2, 3]);
            let d = s.open(&path);
            let mut window = s
                .rt
                .open_view(&me(), &s.app.view("library"), vec![Value::resource(&s.root)])
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            let row = find(&window.tree(), "ui.ListRow").expect("the buffer's row stands in the window");
            assert_eq!(row.props.get("title"), Some(&Value::text("listed.bin")));
            assert_eq!(s.text(&d, "name"), "listed.bin");
        }
    }
}
