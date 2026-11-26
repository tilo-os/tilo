//! `clipboard.copy`, `clipboard.copy_image`, `clipboard.paste` and `clipboard.paste_image` end to
//! end: an app's action reaches the person's selection, and what pastes comes back as the values
//! the language has for it.

#[cfg(test)]
mod cases {
    use crate::common::*;
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

    /// The clipboard app on a runtime whose clipboard and files are the device's: its own code
    /// is trusted and its copies granted, as a person's press of its copy button is.
    struct Bench {
        rt: Runtime,
        app: App,
        board: ResourceRef,
        clipboard: Clipboard,
        files: Files,
    }

    impl Bench {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let (files, clipboard) = (Files::default(), Clipboard::default());
            rt.set_blobs(files.clone());
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
                files,
            }
        }

        fn call(
            &self,
            action: &str,
            arg: Option<Value>,
        ) -> Result<InvokeOutcome, tilolang_runtime::RuntimeError> {
            let id = self.app.action(action);
            let args = std::iter::once(Value::resource(&self.board))
                .chain(arg)
                .collect();
            self.rt.invoke_action(&me(), (id.as_str(), args))
        }

        fn text(&self) -> String {
            match self.rt.read(&self.board, "text").unwrap() {
                Value::Text(text) => text,
                other => panic!("the board's text reads {other:?}"),
            }
        }
    }

    /// Text and pictures go onto the person's selection and come back from it; a file that is no
    /// picture is refused rather than copying nothing.
    #[test]
    fn copies_and_pastes_through_the_person_s_selection() {
        let board = Bench::new();
        board
            .call("type_in", Some(Value::text("Lisbon in May")))
            .unwrap();
        board.call("copy", None).unwrap();
        assert_eq!(
            board.clipboard.held(),
            Some(("text/plain".to_owned(), b"Lisbon in May".to_vec())),
            "the action's text is the person's copy"
        );

        board.clipboard.put("text/plain", b"Porto");
        board.call("paste", None).unwrap();
        assert_eq!(
            board.text(),
            "Porto",
            "a paste answers what was copied elsewhere"
        );

        let png = [0x89, b'P', b'N', b'G', 0, 1, 2, 3];
        let fox = board.files.keeping("fox.png", "image/png", &png);
        board.call("copy_picture", Some(Value::file(fox))).unwrap();
        assert_eq!(
            board.clipboard.held(),
            Some(("image/png".to_owned(), png.to_vec())),
            "an image is offered as the bytes the file holds, under its media type"
        );

        // What another app copied as an image pastes as a file this device keeps.
        board.clipboard.put("image/jpeg", b"\xff\xd8jpeg");
        board.call("paste_picture", None).unwrap();
        assert_eq!(board.text(), "pasted.jpg");

        let notes = board.files.keeping("Notes.md", "text/markdown", b"# Notes");
        board.clipboard.put("text/plain", b"kept");
        assert!(
            board
                .call("copy_picture", Some(Value::file(notes)))
                .is_err(),
            "a copy of no image says why: `Notes.md` is text/markdown"
        );
        assert_eq!(board.clipboard.text(), "kept", "nothing copied");
    }
}
