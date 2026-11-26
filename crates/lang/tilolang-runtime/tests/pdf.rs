//! `pdf.picture` and `pdf.save` end to end: the device draws a page and writes a copy, and each
//! answers the `File` the app shows or saves — a `Result`, as the checker states, never a bare
//! file a `.ok(file)` match would trap on.

#[cfg(test)]
mod cases {

    use crate::common::*;
    use tilolang_runtime::blob::FileValue;
    use tilolang_runtime::{AmStore, Runtime, Value};

    const SOURCE: &str = r#"app so.tilo.papers "Papers"

type Shelf {
    words: Text
}

/// One page drawn by the device, read as the file it is.
action draw(s: Shelf, f: File) {
    let said = match pdf.picture(f, 1, 100) {
        .ok(out) => "{out.name} {out.url}"
        .err(why) => why
    }
    s.words = said
}

/// A copy written by the device, read as the file it is.
action keep(s: Shelf, f: File) {
    let said = match pdf.save([f], "") {
        .ok(out) => "{out.name} {out.media}"
        .err(why) => why
    }
    s.words = said
}

/// A page the device cannot draw reads as its words.
action refused(s: Shelf, f: File) {
    let said = match pdf.picture(f, 9, 100) {
        .ok(out) => out.name
        .err(why) => why
    }
    s.words = said
}
"#;

    /// The device's PDF engine: one page, a picture of it, and a copy of it.
    struct Reader;

    impl tilolang_runtime::pdf::PdfHost for Reader {
        fn pages(&self, _: &[u8]) -> Result<String, String> {
            Ok(r#"[{"page":1,"width":612.0,"height":792.0}]"#.to_owned())
        }

        fn outline(&self, _: &[u8]) -> Result<String, String> {
            Ok("[]".to_owned())
        }

        fn text(&self, _: &[u8], _: i64) -> Result<String, String> {
            Ok("[]".to_owned())
        }

        fn links(&self, _: &[u8], _: i64) -> Result<String, String> {
            Ok("[]".to_owned())
        }

        fn fields(&self, _: &[u8]) -> Result<String, String> {
            Ok("[]".to_owned())
        }

        fn markup(&self, _: &[u8]) -> Result<String, String> {
            Ok(r#"{"marks":[],"fields":[]}"#.to_owned())
        }

        fn picture(&self, _: &[u8], page: i64, _: i64) -> Result<Vec<u8>, String> {
            if page == 1 {
                Ok(vec![0x89, b'P', b'N', b'G'])
            } else {
                Err(format!("the document has no page {page}"))
            }
        }

        fn import(&self, _: &[u8]) -> Result<String, String> {
            Err("a document is no page to import".to_owned())
        }

        fn save(&self, _: &[&[u8]], _: &str) -> Result<Vec<u8>, String> {
            Ok(b"%PDF-1.7 a copy".to_vec())
        }
    }

    struct Bench {
        rt: Runtime,
        app: App,
        shelf: tilolang_runtime::ResourceRef,
        me: tilolang_runtime::Actor,
        paper: FileValue,
    }

    impl Bench {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            rt.set_pdf(Reader);
            let store = Files::default();
            let paper = store.keeping("paper.pdf", "application/pdf", b"%PDF-1.7 a paper");
            rt.set_blobs(store);
            let app = App::compile(SOURCE, &[]);
            authority.trust(&app.load(&rt));
            let shelf = rt.create_root("Shelf").unwrap();
            Self {
                rt,
                app,
                shelf,
                me: me(),
                paper,
            }
        }

        fn call(&self, action: &str) -> String {
            let id = self.app.action(action);
            let out = self
                .rt
                .invoke_action(
                    &self.me,
                    (
                        id.as_str(),
                        vec![
                            Value::resource(&self.shelf),
                            Value::file(self.paper.clone()),
                        ],
                    ),
                )
                .unwrap();
            let _ = out;
            match self.rt.read(&self.shelf, "words").unwrap() {
                Value::Text(text) => text,
                other => panic!("the shelf's words read {other:?}"),
            }
        }
    }

    tilo_test_env::scenario! {
        fn pdf_1;

        phase a_page_the_device_draws_is_read_as_the_file_it_is;
        {
            let bench = Bench::new();
            let said = bench.call("draw");
            assert!(
                said.starts_with("page.png tilo-blob://"),
                "a picture answers the file the device kept: {said}"
            );
        }

        phase a_copy_the_device_writes_is_read_as_the_file_it_is;
        {
            let bench = Bench::new();
            let said = bench.call("keep");
            assert_eq!(
                said, "annotated.pdf application/pdf",
                "a save answers the file it wrote"
            );
        }

        phase a_refusal_reads_as_the_device_s_words;
        {
            let bench = Bench::new();
            assert_eq!(bench.call("refused"), "the document has no page 9");
        }
    }
}
