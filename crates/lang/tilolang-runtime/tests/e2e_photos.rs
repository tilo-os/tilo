//! The Photos app (`apps/tl/photos/app.tl`) end to end: a folder import that says what it read,
//! a picture imported twice that stays one photo, the words a device reads into a picture, and a
//! copy of it on the clipboard.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::clipboard::ClipboardHost;
    use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
    use tilolang_runtime::{InvokeOutcome, ResourceRef, Runtime, Value, ViewInstance};

    const SOURCE: &str = include_str!("../../../../apps/tl/photos/app.tl");

    /// The picture the tests import: a 64 × 64 PNG, real bytes the device's codec decodes.
    fn picture() -> Vec<u8> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(concat!(
                "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAYAAACqaXHeAAAAPElEQVR42u3OMQEAAAgDoC251a3g",
                "LwSgOXLVkiRJkiRJkiRJkiRJkiRJkiRJkiRJkiRJkiRJkiRJkiRJ0iN1AAG5eZ4hAAAAAElFTkSu",
                "QmCC"
            ))
            .expect("the fixture is base64")
    }

    /// The device's content store: what a chosen file holds, and what a save asked for.
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
            Ok(())
        }

        /// This device's preview of a picture is the picture itself: a real host shrinks it.
        fn thumbnail(&self, file: &FileValue, _size: u32) -> Result<Option<FileValue>, String> {
            Ok(Some(file.clone()))
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            // The name tells two kept pictures apart even when their bytes are the same.
            let hash = format!("h{}-{}", name.len(), bytes.len());
            self.0.borrow_mut().insert(hash.clone(), bytes.to_vec());
            Ok(FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }
    }

    /// What the person copied: the media type and bytes of the last copy.
    #[derive(Clone, Default)]
    struct Clipboard(Rc<RefCell<Option<(String, Vec<u8>)>>>);

    impl ClipboardHost for Clipboard {
        fn copy(&self, mime: &str, bytes: &[u8]) -> bool {
            *self.0.borrow_mut() = Some((mime.to_owned(), bytes.to_vec()));
            true
        }

        fn paste(&self, _types: &[&str]) -> Option<(String, Vec<u8>)> {
            None
        }
    }

    /// The host's dispatch and the device's image model in one: the vision service's actions are
    /// answered here, and any other call the workspace wants the person's yes for waits as the
    /// question they are shown, as the session holds it until they answer.
    #[derive(Clone, Default)]
    struct Vision {
        asked: Rc<RefCell<Vec<String>>>,
        /// The call the workspace asked the person about and they have not answered yet: what
        /// the session delivers, admitted, once they say yes.
        question: Rc<RefCell<Option<(String, Vec<Value>)>>>,
    }

    impl Vision {
        /// The call the person is being asked about, as the session delivers it once they say yes.
        fn question(&self) -> (String, Vec<Value>) {
            self.question
                .borrow_mut()
                .take()
                .expect("the workspace asked the person about a call")
        }
    }

    impl Invoker for Vision {
        fn invoke(&self, invocation: &Invocation) -> Invoked {
            let Invocation::Action {
                app,
                action,
                name,
                args,
                ..
            } = invocation
            else {
                return Invoked::Failed("this device changes nothing".to_owned());
            };
            if app.as_str() != "so.tilo.vision" {
                let values = args.iter().map(|(_, value)| value.clone()).collect();
                *self.question.borrow_mut() = Some((action.clone(), values));
                return Invoked::Pending(format!("asked to {name}"));
            }
            self.asked.borrow_mut().push(name.clone());
            match name.as_str() {
                "ocr" => Invoked::Done(Value::text("HARBOUR CAFE, OPEN 7 TO 4")),
                // Every picture given is one face's: the model answers with the groups it found.
                "faces" => {
                    let Invocation::Action { args, .. } = invocation else {
                        return Invoked::Failed("this device changes nothing".to_owned());
                    };
                    let images = args
                        .iter()
                        .find(|(name, _)| name == "images")
                        .map(|(_, value)| value.clone())
                        .unwrap_or(Value::List(Vec::new()));
                    Invoked::Done(Value::List(vec![images]))
                }
                other => Invoked::Failed(format!("this device answers no {other}")),
            }
        }
    }

    fn model() -> ResourceRef {
        ResourceRef::parse_id("vision:vision.image-model", "so.tilo.vision#ImageModel")
            .expect("a resource id")
    }

    fn analysis() -> ResourceRef {
        ResourceRef::parse_id("vision:vision.analysis", "so.tilo.vision#Analysis")
            .expect("a resource id")
    }

    struct Gallery {
        rt: Runtime,
        app: App,
        lib: ResourceRef,
        device: Device,
        clipboard: Clipboard,
        vision: Vision,
    }

    impl Gallery {
        fn new() -> Self {
            let rt = Runtime::new(
                tilolang_runtime::AmStore::new(),
                tilolang_runtime::SimpleAuthority::new(),
            )
            .unwrap();
            // The device's image model is the platform's own package, read as an app reads it.
            let package = tilolang_tilo::platform_apps::platform_packages()
                .expect("the platform packages read")
                .into_iter()
                .find(|package| package.app == "so.tilo.vision")
                .expect("vision is a platform package");
            rt.load_foreign(package.clone()).expect("vision loads");
            rt.set_reading(&model(), r#"{"available":true,"detail":""}"#)
                .expect("a reading of the image model");
            rt.set_reading(&analysis(), r#"{"phase":"idle","completed":0,"total":0}"#)
                .expect("a reading of the analysis");
            let device = Device::default();
            rt.set_blobs(device.clone());
            let clipboard = Clipboard::default();
            rt.set_clipboard(clipboard.clone());
            let vision = Vision::default();
            rt.set_foreign_invoker(vision.clone());
            let app = App::compile(SOURCE, std::slice::from_ref(&package));
            app.load(&rt);
            let lib = rt.create_root("Library").unwrap();
            Self {
                rt,
                app,
                lib,
                device,
                clipboard,
                vision,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> tilolang_runtime::InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|why| panic!("{action}: {why}"))
        }

        /// Runs `action` as the person does where the workspace asks them first: the window shows
        /// what it would do, they say yes, and the session delivers the call admitted. Answers what
        /// the action came to.
        fn call_and_approve(&self, action: &str, args: Vec<Value>) -> Value {
            let mut outcome = self.call(action, args);
            let mut asked = false;
            loop {
                match outcome {
                    InvokeOutcome::Asked(_) => {
                        asked = true;
                        let (call, values) = self.vision.question();
                        outcome = self
                            .rt
                            .invoke_admitted(&me(), (call.as_str(), values), None)
                            .unwrap_or_else(|why| panic!("{action}, the person said yes: {why}"));
                    }
                    InvokeOutcome::Waits { run, .. } => {
                        let (call, values) = self.vision.question();
                        let answer = match self
                            .rt
                            .invoke_admitted(&me(), (call.as_str(), values), None)
                            .unwrap_or_else(|why| panic!("{action}, the person said yes: {why}"))
                        {
                            InvokeOutcome::Done(value) => Invoked::Done(value),
                            other => Invoked::Failed(format!("{other:?}")),
                        };
                        outcome = self
                            .rt
                            .answer_call(&run, answer)
                            .unwrap_or_else(|why| panic!("{action}'s run takes the answer: {why}"))
                            .into();
                    }
                    InvokeOutcome::Done(value) => {
                        assert!(asked, "{action} did not ask the person first");
                        return value;
                    }
                    other => panic!("{action}, the person said yes, came to {other:?}"),
                }
            }
        }

        /// The picture a person chose, as the device hands it over.
        fn chosen(&self, name: &str) -> Value {
            let kept = self
                .device
                .keep(name, "image/png", &picture())
                .expect("the picture is kept");
            Value::file(kept)
        }

        /// A file that is not a picture.
        fn not_a_picture(&self, name: &str) -> Value {
            let kept = self
                .device
                .keep(name, "text/plain", b"notes")
                .expect("the notes are kept");
            Value::file(kept)
        }

        /// The items of a collection field, as the records they are.
        fn records(&self, field: &str) -> Vec<ResourceRef> {
            self.records_of(&self.lib, field)
        }

        fn records_of(&self, owner: &ResourceRef, field: &str) -> Vec<ResourceRef> {
            match self.rt.read(owner, field).unwrap() {
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

        /// The texts of a `Set<Text>` field: the ids a record names.
        fn ids_of(&self, owner: &ResourceRef, field: &str) -> Vec<String> {
            match self.rt.read(owner, field).unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| item.as_text().map(str::to_owned))
                    .collect(),
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn photos(&self) -> Vec<ResourceRef> {
            match self.rt.read(&self.lib, "photos").unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(r) => Some(std::rc::Rc::unwrap_or_clone(r.target)),
                        _ => None,
                    })
                    .collect(),
                other => panic!("photos reads {other:?}"),
            }
        }

        fn number(&self, field: &str) -> i64 {
            match self.rt.read(&self.lib, field).unwrap() {
                Value::Int(n) => n,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn open(&self, view: &str, args: Vec<Value>) -> ViewInstance {
            let mut opened = self
                .rt
                .open_view(&me(), &self.app.view(view), args)
                .unwrap_or_else(|why| panic!("{view}: {why}"));
            opened.set_lazy_rows(false).unwrap();
            opened
        }

        fn texts(&self, view: &ViewInstance) -> Vec<String> {
            of_kind(view.tree(), "ui.Text")
                .into_iter()
                .map(|node| text_prop(node, "value"))
                .collect()
        }
    }

    tilo_test_env::scenario! {
        fn photos_imports_reads_and_copies_a_picture;

        phase a_folder_import_says_what_it_read;
        {
            let g = Gallery::new();
            g.call(
                "import_dropped",
                vec![
                    Value::resource(&g.lib),
                    Value::List(vec![g.chosen("Harbour.png"), g.not_a_picture("Notes.txt")]),
                ],
            );
            assert_eq!(g.number("import_found"), 2);
            assert_eq!(g.number("import_done"), 1);
            assert_eq!(g.photos().len(), 1, "one picture became a photo");
            let page = g.open("library", vec![Value::resource(&g.lib)]);
            let banner = of_kind(page.tree(), "ui.Banner")
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("the window shows no import report: {}", dump(page.tree())));
            assert_eq!(
                banner.props.get("title").and_then(Value::as_text),
                Some("Imported 1 of 2"),
                "the window says how many arrived"
            );
            assert_eq!(
                banner.props.get("message").and_then(Value::as_text),
                Some("Not pictures: Notes.txt is not a picture"),
                "and which file was not a picture"
            );
            g.call("dismiss_import", vec![Value::resource(&g.lib)]);
            assert_eq!(g.number("import_found"), 0, "the report can be taken down");
        }

        phase a_picture_imported_again_stays_one_photo;
        {
            let g = Gallery::new();
            let file = g.chosen("Harbour.png");
            g.call(
                "import_photo",
                vec![Value::resource(&g.lib), file.clone()],
            );
            g.call("import_photo", vec![Value::resource(&g.lib), file]);
            assert_eq!(g.photos().len(), 1, "the same picture is one photo");
        }

        phase the_words_read_from_a_picture_are_the_library_s;
        {
            let g = Gallery::new();
            let photo = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            g.call("recognize_text", vec![Value::resource(&photo)]);
            assert_eq!(
                g.rt.read(&photo, "recognized").unwrap(),
                Value::text("HARBOUR CAFE, OPEN 7 TO 4"),
                "the device read the picture's words"
            );
            assert_eq!(g.vision.asked.borrow().as_slice(), ["ocr"]);
            let details = g.open(
                "details_fields",
                vec![Value::resource(&g.lib), Value::resource(&photo)],
            );
            let texts = g.texts(&details);
            let sections: Vec<String> = of_kind(details.tree(), "ui.Section")
                .into_iter()
                .filter_map(|s| s.props.get("title").and_then(Value::as_text).map(str::to_owned))
                .collect();
            assert!(
                sections.iter().any(|t| t == "Text in the picture"),
                "the details name the words read: {sections:?}"
            );
            assert!(
                texts.iter().any(|t| t == "HARBOUR CAFE, OPEN 7 TO 4"),
                "the details show what was read: {texts:?}"
            );
        }

        phase a_face_found_is_named_into_a_person;
        {
            let g = Gallery::new();
            let first = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            let second = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Quay.png")],
            ));
            // A scan reaches the device's model, so the workspace asks the person first.
            g.call_and_approve("scan_faces", vec![Value::resource(&g.lib)]);
            let suggestions = g.records("suggestions");
            assert_eq!(suggestions.len(), 1, "the face the model found is one suggestion");
            let suggestion = &suggestions[0];
            assert_eq!(
                g.rt.read(suggestion, "sent").unwrap(),
                Value::Bool(false),
                "an unnamed face has not been sent to the workspace"
            );
            // Until a name is approved the face is the scanner's alone: another member neither
            // lists nor opens it.
            let other = tilolang_runtime::Actor::from(tilolang_runtime::ids::MemberId::person("bo"));
            assert!(g.rt.readers_admit(&me(), suggestion));
            assert!(
                !g.rt.readers_admit(&other, suggestion),
                "another member does not read a face nobody named"
            );
            assert_eq!(g.ids_of(suggestion, "photos").len(), 2);
            // A scan again finds the same face, and does not suggest it twice.
            g.call_and_approve("scan_faces", vec![Value::resource(&g.lib)]);
            assert_eq!(g.records("suggestions").len(), 1);
            // Naming it makes the person, and puts the photos on them.
            g.call(
                "approve_suggestion",
                vec![
                    Value::resource(&g.lib),
                    Value::resource(suggestion),
                    Value::text("Ava"),
                ],
            );
            assert_eq!(
                g.rt.read(suggestion, "sent").unwrap(),
                Value::Bool(true),
                "an approved face is the workspace's"
            );
            assert!(
                g.rt.readers_admit(&other, suggestion),
                "the workspace reads a face whose name it approved"
            );
            let people = g.records("people");
            assert_eq!(people.len(), 1, "the name made a person");
            assert_eq!(g.rt.read(&people[0], "name").unwrap(), Value::text("Ava"));
            assert_eq!(g.ids_of(&people[0], "photos").len(), 2);
            for photo in [&first, &second] {
                let people = match g.rt.read(photo, "people").unwrap() {
                    Value::List(items) => items,
                    other => panic!("people reads {other:?}"),
                };
                assert!(people.contains(&Value::text("Ava")), "the photo is on Ava");
            }
            // The people page shows the person the face became.
            let mut page = g.open("library", vec![Value::resource(&g.lib)]);
            let people = of_kind(page.tree(), "ui.NavItem")
                .into_iter()
                .find(|item| item.props.get("key") == Some(&Value::text("people")))
                .unwrap_or_else(|| panic!("no People place: {}", dump(page.tree())))
                .key
                .clone();
            page.fire(&people, "select").unwrap();
            page.poll_patches().unwrap();
            let names: Vec<String> = of_kind(page.tree(), "ui.ListRow")
                .into_iter()
                .filter_map(|row| row.props.get("title").and_then(Value::as_text).map(str::to_owned))
                .collect();
            assert!(names.iter().any(|name| name == "Ava"), "{names:?}");
        }

        phase copying_a_photo_puts_its_picture_on_the_clipboard;
        {
            let g = Gallery::new();
            let photo = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            g.call_and_approve("copy_photo", vec![Value::resource(&photo)]);
            let copied = g.clipboard.0.borrow().clone();
            let (mime, bytes) = copied.expect("the picture was copied");
            assert_eq!(mime, "image/png");
            assert_eq!(bytes, picture(), "the picture's own bytes are what was copied");
        }
    }
}
