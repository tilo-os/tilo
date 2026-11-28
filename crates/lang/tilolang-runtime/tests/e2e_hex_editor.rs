//! The Hex Editor (`apps/tl/hex-editor/app.tl`) end to end: a file read as bytes, the edits a
//! person typed, a pattern found and replaced, the numbers the inspector reads, and the file
//! written back. The device's work is the real engine (`crates/engines/hex-engine`), so the test
//! goes through the same answers a session gives.

#[cfg(test)]
mod cases {
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::FileValue;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/hex-editor/app.tl");

    struct Desk {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        device: Files,
        clipboard: Clipboard,
        directory: tempfile::TempDir,
    }

    impl Desk {
        fn new() -> Self {
            // The app's code is trusted and its own outside work granted, as a person's press of
            // its buttons is.
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let device = Files::default();
            rt.set_blobs(device.clone());
            let clipboard = Clipboard::default();
            rt.set_clipboard(clipboard.clone());
            rt.set_engine(DeviceEngine::new(hex_engine::HexEngine::new(), &device));
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
                clipboard,
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
            match self.rt.read(&desk, "cursor").unwrap() {
                Value::Int(n) => usize::try_from(n).unwrap_or(0),
                other => panic!("cursor reads {other:?}"),
            }
        }

        fn edited(&self, d: &ResourceRef) -> usize {
            match self.rt.read(d, "edited").unwrap() {
                Value::Int(n) => usize::try_from(n).unwrap_or(0),
                other => panic!("edited reads {other:?}"),
            }
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

    impl Reads for Desk {
        fn rt(&self) -> &Runtime {
            &self.rt
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
        if node.kind == "ui.Text"
            && let Some(Value::Text(text)) = node.props.get("value")
        {
            out.push(text.clone());
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
            assert!((s.number(&d, "length") - 4.0).abs() < f64::EPSILON);
            assert_eq!(s.bytes(&d), [0x00, 0x11, 0x22, 0x33]);
            assert_eq!(s.rows(&d), ["00112233"]);
            assert!((s.number(&s.desk(&d), "cursor") - 0.0).abs() < f64::EPSILON);
        }

        phase a_file_another_app_hands_over_opens_from_its_address;
        {
            let s = Desk::new();
            let file = s.device.keeping("handed.bin", "application/octet-stream", &[0xDE, 0xAD]);
            let d = s.opened_file(file);
            assert_eq!(s.text(&d, "name"), "handed.bin");
            assert!((s.number(&d, "length") - 2.0).abs() < f64::EPSILON);
            assert_eq!(s.bytes(&d), [0xDE, 0xAD]);
        }

        phase a_new_empty_buffer_is_a_buffer_with_no_bytes;
        {
            let s = Desk::new();
            let d = s.new_buffer();
            assert_eq!(s.text(&d, "name"), "Untitled.bin");
            assert!((s.number(&d, "length") - 0.0).abs() < f64::EPSILON);
            assert_eq!(s.edited(&d), 0);
            assert!(s.rows(&d).is_empty());
        }

        phase bytes_a_caller_hands_over_open_as_their_own_file;
        {
            let s = Desk::new();
            let d = returned_ref(s.call(
                "open_bytes",
                vec![
                    Value::resource(&s.root),
                    Value::text("made.txt"),
                    Value::text("YWJj"),
                ],
            ));
            assert_eq!(s.text(&d, "name"), "made.txt");
            assert!((s.number(&d, "length") - 3.0).abs() < f64::EPSILON);
            assert_eq!(s.rows(&d), ["616263"]);
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
            assert!((s.number(&d, "length") - 2.0).abs() < f64::EPSILON);
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
            assert!((s.number(&desk, "first") - 1.0).abs() < f64::EPSILON, "the rows shown follow the cursor");
            s.call("scrolled", vec![Value::resource(&d), Value::Int(30)]);
            let desk = s.desk(&d);
            assert!((s.number(&desk, "first") - 31.0).abs() < f64::EPSILON, "the wheel moves the window");
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
            assert!((s.number(&d, "length") - 2.0).abs() < f64::EPSILON);
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
            assert!((s.number(&desk, "at_match") - 0.0).abs() < f64::EPSILON);
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

        phase cut_takes_the_selection_and_keeps_it_on_the_clipboard;
        {
            let s = Desk::new();
            let path = s.file("cut.bin", &[0x50, 0x4E, 0x47, 0x44]);
            let d = s.open(&path);
            s.press(&d, "Ctrl+a");
            s.press(&d, "Ctrl+x");
            assert!(s.bytes(&d).is_empty(), "cut removed the bytes");
            assert_eq!(s.clipboard.text(), "50 4e 47 44");
            let desk = s.desk(&d);
            assert_eq!(s.text(&desk, "notice"), "Cut 4 bytes");
        }

        phase a_replacement_of_another_length_is_refused_in_overwrite_mode;
        {
            let s = Desk::new();
            let path = s.file("length.bin", &[0xDE, 0xAD, 0x00]);
            let d = s.open(&path);
            s.call("search", vec![Value::resource(&d), Value::text("de ad")]);
            s.call("replace_all", vec![Value::resource(&d), Value::text("be")]);
            assert_eq!(
                s.bytes(&d),
                [0xDE, 0xAD, 0x00],
                "the file keeps its length"
            );
            let desk = s.desk(&d);
            let said = s.text(&desk, "notice");
            assert!(
                said.contains("another length than the match"),
                "the refusal says why: {said}"
            );
            s.call("set_mode", vec![Value::resource(&d), Value::Int(1)]);
            s.call("replace_all", vec![Value::resource(&d), Value::text("be")]);
            assert_eq!(s.bytes(&d), [0xBE, 0x00], "insert mode grows and shrinks");
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

        phase one_match_is_replaced_and_a_text_replacement_is_its_own_utf8;
        {
            let s = Desk::new();
            let path = s.file("one.bin", b"cat, cat; cat");
            let d = s.open(&path);
            s.call("search_kind", vec![Value::resource(&d), Value::Int(1)]);
            s.call("search", vec![Value::resource(&d), Value::text("cat")]);
            s.call("set_mode", vec![Value::resource(&d), Value::Int(1)]);
            // A text search replaces as text: `lynxä` is its letters' UTF-8, not hex digits.
            s.call("replace_one", vec![Value::resource(&d), Value::text("lynxä")]);
            assert_eq!(s.bytes(&d), "lynxä, cat; cat".as_bytes());
            let desk = s.desk(&d);
            assert_eq!(s.text(&desk, "notice"), "Replaced 1 match");
            s.call("replace_all", vec![Value::resource(&d), Value::text("lynxä")]);
            assert_eq!(s.bytes(&d), "lynxä, lynxä; lynxä".as_bytes());
            // A hex search still reads the replacement as hexadecimal pairs.
            s.call("search_kind", vec![Value::resource(&d), Value::Int(0)]);
            s.call("search", vec![Value::resource(&d), Value::text("6c 79")]);
            s.call("replace_all", vec![Value::resource(&d), Value::text("78 78")]);
            assert_eq!(s.bytes(&d), "xxnxä, xxnxä; xxnxä".as_bytes());
        }

        phase the_matches_are_stepped_through_both_ways_and_wrap;
        {
            let s = Desk::new();
            let path = s.file("steps.bin", b"aa-bb-aa-aa");
            let d = s.open(&path);
            s.call("search", vec![Value::resource(&d), Value::text("61 61")]);
            let desk = s.desk(&d);
            let at = |s: &Desk| match s.rt.read(&desk, "at_match").unwrap() {
                Value::Int(n) => usize::try_from(n).unwrap_or(0),
                other => panic!("at_match reads {other:?}"),
            };
            assert_eq!(at(&s), 0);
            s.call("step_match", vec![Value::resource(&d), Value::Bool(false)]);
            assert_eq!((at(&s), s.cursor(&d)), (1, 6));
            s.call("step_match", vec![Value::resource(&d), Value::Bool(false)]);
            assert_eq!((at(&s), s.cursor(&d)), (2, 9));
            s.call("step_match", vec![Value::resource(&d), Value::Bool(false)]);
            assert_eq!((at(&s), s.cursor(&d)), (0, 0), "past the last is the first");
            s.call("step_match", vec![Value::resource(&d), Value::Bool(true)]);
            assert_eq!((at(&s), s.cursor(&d)), (2, 9), "back from the first is the last");
        }

        phase the_end_keys_reach_the_first_and_last_byte;
        {
            let s = Desk::new();
            let path = s.file("ends.bin", &[0u8; 40]);
            let d = s.open(&path);
            s.press(&d, "Ctrl+End");
            assert_eq!(s.cursor(&d), 39);
            s.press(&d, "Ctrl+Home");
            assert_eq!(s.cursor(&d), 0);
        }

        phase a_paste_in_the_text_column_keeps_a_letters_utf8_bytes;
        {
            let s = Desk::new();
            let path = s.file("paste-text.bin", b"ab");
            let d = s.open(&path);
            s.clipboard.put("text/plain", "ä!".as_bytes());
            s.call("set_column", vec![Value::resource(&d), Value::Int(1)]);
            s.press(&d, "Insert");
            s.press(&d, "Ctrl+v");
            assert_eq!(s.bytes(&d), "ä!ab".as_bytes());
            let desk = s.desk(&d);
            assert_eq!(s.text(&desk, "notice"), "Pasted 3 bytes");
            // The same clipboard in the byte column is hexadecimal pairs or nothing.
            s.call("set_column", vec![Value::resource(&d), Value::Int(0)]);
            s.press(&d, "Ctrl+v");
            assert_eq!(
                s.bytes(&d),
                "ä!ab".as_bytes(),
                "a text that is no hexadecimal bytes leaves the file as it was"
            );
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
            assert!((s.number(&d, "length") - before).abs() < f64::EPSILON);
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
            // The window's own controls stand in its bar: Save is the first of them.
            let save = find(window.tree(), "ui.Button").expect("the window's bar has its controls");
            assert_eq!(save.props.get("label"), Some(&Value::text("Save")));
            let grid = find(window.tree(), "ui.HexGrid").expect("the grid stands in the window");
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
            texts(window.tree(), &mut words);
            assert!(
                words.iter().any(|w| w.contains("Offset 0x00000000")),
                "{words:?}"
            );
            assert!(words.iter().any(|w| w == "4 bytes"), "{words:?}");
        }

        phase a_library_row_opens_the_buffer_it_names;
        {
            let s = Desk::new();
            let d = s.new_buffer();
            let mut window = s
                .rt
                .open_view(&me(), &s.app.view("library"), vec![Value::resource(&s.root)])
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            let row = find(window.tree(), "ui.ListRow")
                .unwrap_or_else(|| panic!("the buffer is a row: {}", dump(window.tree())));
            let key = row.key.clone();
            window.fire(&key, "activate").unwrap();
            let opened: Vec<ResourceRef> = window
                .poll_events()
                .into_iter()
                .filter_map(|event| match event {
                    tilolang_runtime::ViewEvent::Open { target, .. } => Some(target),
                    _ => None,
                })
                .collect();
            assert!(
                opened.contains(&d),
                "the row opens its document: {opened:?}"
            );
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
            let row = find(window.tree(), "ui.ListRow").expect("the buffer's row stands in the window");
            assert_eq!(row.props.get("title"), Some(&Value::text("listed.bin")));
            assert_eq!(s.text(&d, "name"), "listed.bin");
        }
    }
}
