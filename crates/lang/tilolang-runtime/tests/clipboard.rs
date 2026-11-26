//! `clipboard.copy`, `clipboard.copy_image`, `clipboard.paste` and `clipboard.paste_image` end to
//! end: an app's action reaches the person's selection, and what pastes comes back as the values
//! the language has for it.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::clipboard::ClipboardHost;
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, Value};

    const SOURCE: &str = r#"app so.tilo.clipboard "Clipboard"

type Board {
    text: Text
}

action type_in(b: Board, text: Text) {
    b.text = text
}

action copy(b: Board) uses clipboard {
    clipboard.copy(b.text)
}

action paste(b: Board) uses clipboard {
    b.text = clipboard.paste() ?? b.text
}

action copy_picture(b: Board, f: File) uses clipboard {
    clipboard.copy_image(f)
}

action paste_picture(b: Board) uses clipboard {
    let image = clipboard.paste_image()
    if let picture = image {
        b.text = picture.name
    }
}
"#;

    /// What the selection holds: a media type and its bytes.
    type Held = Option<(String, Vec<u8>)>;

    /// The person's selection, as the device answers it: what a copy leaves there and a paste
    /// reads off it.
    #[derive(Clone, Default)]
    struct Selection(Rc<RefCell<Held>>);

    impl ClipboardHost for Selection {
        fn copy(&self, mime: &str, bytes: &[u8]) -> bool {
            *self.0.borrow_mut() = Some((mime.to_owned(), bytes.to_vec()));
            true
        }

        fn paste(&self, types: &[&str]) -> Option<(String, Vec<u8>)> {
            let held = self.0.borrow().clone()?;
            types.iter().any(|wanted| *wanted == held.0).then_some(held)
        }
    }

    /// The device's content store, of what a test kept there: a file's hash is its bytes.
    #[derive(Clone, Default)]
    struct Device(Rc<RefCell<HashMap<String, Vec<u8>>>>);

    impl Device {
        fn keeps(&self, bytes: &[u8]) -> String {
            let hash = digest_of(bytes);
            self.0.borrow_mut().insert(hash.clone(), bytes.to_vec());
            hash
        }
    }

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(hash).cloned()
        }

        fn choose(&self, _: &FileRequest) -> Result<Vec<FileValue>, String> {
            Ok(Vec::new())
        }

        fn save(&self, _: &SaveRequest, _: &[u8]) -> Result<(), String> {
            Err("this test writes no files out".to_owned())
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            let hash = self.keeps(bytes);
            Ok(FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            ))
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

    struct Bench {
        rt: Runtime,
        app: App,
        board: ResourceRef,
        clipboard: Selection,
        device: Device,
    }

    impl Bench {
        fn new() -> Self {
            // The app's own code is trusted and its copies are granted, as a person's press of
            // its copy button is: an action that copies reaches outside Tilo, which the policy
            // allows for the app it is written in.
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let device = Device::default();
            let clipboard = Selection::default();
            rt.set_blobs(device.clone());
            rt.set_clipboard(clipboard.clone());
            let app = App::compile(SOURCE, &[]);
            authority.trust(&app.load(&rt));
            for action in ["copy", "copy_picture"] {
                authority.grant_outside(&app.action(action));
            }
            let board = rt.create_root("Board").unwrap();
            Self {
                rt,
                app,
                board,
                clipboard,
                device,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        fn text(&self) -> String {
            match self.rt.read(&self.board, "text").unwrap() {
                Value::Text(text) => text,
                other => panic!("the board's text reads {other:?}"),
            }
        }

        fn set_text(&self, text: &str) {
            self.call(
                "type_in",
                vec![Value::resource(&self.board), Value::text(text)],
            );
        }
    }

    #[test]
    fn an_action_copies_text_onto_the_person_s_selection() {
        let board = Bench::new();
        board.set_text("Lisbon in May");
        board.call("copy", vec![Value::resource(&board.board)]);
        assert_eq!(
            board.clipboard.0.borrow().clone(),
            Some(("text/plain".to_owned(), b"Lisbon in May".to_vec())),
            "the action's text is the person's copy"
        );
    }

    #[test]
    fn a_paste_answers_what_the_person_copied_elsewhere() {
        let board = Bench::new();
        board.set_text("old");
        *board.clipboard.0.borrow_mut() = Some(("text/plain".to_owned(), b"Porto".to_vec()));
        board.call("paste", vec![Value::resource(&board.board)]);
        assert_eq!(board.text(), "Porto");
    }

    #[test]
    fn an_image_is_copied_as_its_bytes_and_pastes_back_as_a_file_the_device_kept() {
        let board = Bench::new();
        let png = [0x89, b'P', b'N', b'G', 0, 1, 2, 3];
        let hash = board.device.keeps(&png);
        let file = Value::file(FileValue::new("fox.png", "image/png", 8, hash));
        board.call("copy_picture", vec![Value::resource(&board.board), file]);
        assert_eq!(
            board.clipboard.0.borrow().clone(),
            Some(("image/png".to_owned(), png.to_vec())),
            "an image is offered as the bytes the file holds, under its media type"
        );

        // What another app copied as an image pastes as a file this device keeps.
        *board.clipboard.0.borrow_mut() = Some(("image/jpeg".to_owned(), b"\xff\xd8jpeg".to_vec()));
        board.call("paste_picture", vec![Value::resource(&board.board)]);
        assert_eq!(board.text(), "pasted.jpg");
    }

    #[test]
    fn copying_a_file_that_is_no_image_fails_rather_than_copying_nothing() {
        let board = Bench::new();
        let hash = board.device.keeps(b"# Notes");
        let file = Value::file(FileValue::new("Notes.md", "text/markdown", 7, hash));
        let id = board.app.action("copy_picture");
        let failed = board.rt.invoke_action(
            &me(),
            (id.as_str(), vec![Value::resource(&board.board), file]),
        );
        assert!(
            failed.is_err(),
            "a copy of no image says why: `Notes.md` is text/markdown"
        );
        assert_eq!(board.clipboard.0.borrow().clone(), None, "nothing copied");
    }
}
