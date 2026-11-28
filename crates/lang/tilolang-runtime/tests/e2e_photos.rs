//! The Photos app (`apps/tl/photos/app.tl`) end to end: a folder import that runs as a job on the
//! device, says how far it has got, can be stopped and tried again, and brings what it read into
//! the library when it ends; a picture imported twice that stays one photo; the words a device
//! reads into a picture; faces scanned batch by batch behind a switch, paused, and deleted; and a
//! copy of a picture on the clipboard.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::BlobHost;
    use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
    use tilolang_runtime::{
        EnumValue, InvokeOutcome, RecordInit, ResourceRef, Runtime, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/photos/app.tl");

    /// The picture a camera took as `name`, as the tests import it: a 64 × 64 PNG, real bytes the
    /// device's codec decodes, each name its own colour, as two photos are two pictures.
    fn picture(name: &str) -> Vec<u8> {
        use tilo_image_codec::{DecodedImage, Encoding};
        let shade = name.bytes().fold(0u8, u8::wrapping_add);
        // Its top left corner dark, so a turned or cropped picture is other pixels.
        let mut image = DecodedImage::filled(64, 64, [200, 120, shade, 255]);
        image.rgba[..4].copy_from_slice(&[0, 0, 0, 255]);
        tilo_image_codec::encode_now(&image, Encoding::Png).expect("the fixture encodes")
    }

    /// The call the workspace asked the person about and they have not answered: its app and
    /// what it was asked with.
    type Asked = Option<(String, Vec<Value>)>;

    /// The host's dispatch and the device's image model in one: the vision service's actions are
    /// answered here, and any other call the workspace wants the person's yes for waits as the
    /// question they are shown, as the session holds it until they answer.
    #[derive(Clone, Default)]
    struct Vision {
        asked: Rc<RefCell<Vec<String>>>,
        /// The language each read-the-text call named.
        ocr_language: Rc<RefCell<Vec<String>>>,
        /// Whether the model refuses every call, as a device without it does.
        refuse: Rc<RefCell<bool>>,
        /// The type of the record `tags` answers with, and its fields' ids, as the vision package
        /// declares them.
        shown: Rc<RefCell<(String, std::collections::BTreeMap<String, String>)>>,
        /// The call the workspace asked the person about and they have not answered yet: what
        /// the session delivers, admitted, once they say yes.
        question: Rc<RefCell<Asked>>,
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
            if *self.refuse.borrow() {
                return Invoked::Failed("the model is not on this device".to_owned());
            }
            match name.as_str() {
                "ocr" => {
                    let Invocation::Action { args, .. } = invocation else {
                        return Invoked::Failed("this device changes nothing".to_owned());
                    };
                    let language = args
                        .iter()
                        .find(|(name, _)| name == "language")
                        .and_then(|(_, value)| match value {
                            Value::Opt(Some(inner)) => inner.as_text().map(str::to_owned),
                            Value::Text(text) => Some(text.clone()),
                            _ => None,
                        })
                        .unwrap_or_default();
                    self.ocr_language.borrow_mut().push(language);
                    Invoked::Done(Value::text("HARBOUR CAFE, OPEN 7 TO 4"))
                }
                // What the model says one picture shows, as the record the surface declares.
                "shows" => {
                    let shown = self.shown.borrow();
                    let (ty, ids) = (&shown.0, &shown.1);
                    let field = |name: &str| {
                        ids.get(name)
                            .unwrap_or_else(|| panic!("Shown declares {name}"))
                            .clone()
                    };
                    Invoked::Done(Value::Rec(RecordInit {
                        ty: ty.clone(),
                        fields: [
                            (field("description"), Value::text("A dog beside a bicycle")),
                            (field("text"), Value::text("42 North Street")),
                            (
                                field("objects"),
                                Value::List(vec![Value::text("dog"), Value::text("bicycle")]),
                            ),
                            (field("pets"), Value::List(vec![Value::text("dog")])),
                        ]
                        .into_iter()
                        .collect(),
                    }))
                }
                // The analysis run's own verbs: the app asks the device, which does the work.
                "start" | "pause" | "resume" | "retry" | "cancel" => Invoked::Done(Value::Unit),
                // Every picture given is one face's: the model answers with the groups it found.
                "faces" => {
                    let Invocation::Action { args, .. } = invocation else {
                        return Invoked::Failed("this device changes nothing".to_owned());
                    };
                    let images = args
                        .iter()
                        .find(|(name, _)| name == "images")
                        .map_or(Value::List(Vec::new()), |(_, value)| value.clone());
                    Invoked::Done(Value::List(vec![images]))
                }
                other => Invoked::Failed(format!("this device answers no {other}")),
            }
        }
    }

    /// The words a field holds.
    fn text_of(value: &Value) -> String {
        value
            .as_text()
            .unwrap_or_else(|| panic!("the value reads as text: {value:?}"))
            .to_owned()
    }

    /// The picture the window's viewer draws, by the address of its bytes.
    fn image_of(view: &ViewInstance) -> String {
        of_kind(view.tree(), "ui.ImageViewer")
            .into_iter()
            .find_map(|node| match node.props.get("image") {
                Some(Value::Picture(src)) => Some(src.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the window draws no picture:\n{}", dump(view.tree())))
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
        device: Files,
        clipboard: Clipboard,
        vision: Vision,
        engine: DeviceEngine,
    }

    impl Gallery {
        /// The import started last, and the pictures it was asked to read, by name and address.
        fn last_import(&self) -> (String, Vec<(String, String)>) {
            let started = self.engine.last_started();
            let ask: serde_json::Value =
                serde_json::from_str(&started.input).expect("the ask is JSON");
            let files = ask["files"]
                .as_array()
                .expect("the ask names its pictures")
                .iter()
                .map(|f| {
                    (
                        f["name"].as_str().unwrap().to_owned(),
                        f["url"].as_str().unwrap().to_owned(),
                    )
                })
                .collect();
            (started.job, files)
        }

        fn new() -> Self {
            let rt = Runtime::new(
                tilolang_runtime::AmStore::new(),
                crate::common::TestAuthority::new(),
            )
            .unwrap();
            // The device's image model is the platform's own package, read as an app reads it.
            let package = tilolang_tilo::platform::platform_packages()
                .expect("the platform packages read")
                .into_iter()
                .find(|package| package.app == "so.tilo.vision")
                .expect("vision is a platform package");
            rt.load_foreign(package.clone()).expect("vision loads");
            rt.set_reading(&model(), r#"{"available":true,"detail":""}"#)
                .expect("a reading of the image model");
            rt.set_reading(&analysis(), r#"{"phase":"idle","completed":0,"total":0}"#)
                .expect("a reading of the analysis");
            let device = Files::default().previewing(|_, file, _| Some(file.clone()));
            rt.set_blobs(device.clone());
            let clipboard = Clipboard::default();
            rt.set_clipboard(clipboard.clone());
            let shown = package
                .types
                .iter()
                .find(|ty| ty.name == "Shown")
                .expect("the vision package declares what a picture shows");
            let vision = Vision {
                shown: Rc::new(RefCell::new((
                    shown.id.clone(),
                    shown
                        .fields
                        .iter()
                        .map(|field| (field.name.clone(), field.id.clone()))
                        .collect(),
                ))),
                ..Vision::default()
            };
            rt.set_foreign_invoker(vision.clone());
            let engine = DeviceEngine::calls(
                |op, _| Err(format!("`{op}` is no op this device answers now")),
                &device,
            )
            .for_app("so.tilo.photos")
            .starting(&["import"]);
            rt.set_engine(engine.clone());
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
                engine,
            }
        }

        /// Runs `action` as the person, who says yes if the workspace asks them first: whether it
        /// ran, rather than being refused.
        fn attempt(&self, action: &str, args: Vec<Value>) -> bool {
            let id = self.app.action(action);
            match self.rt.invoke_action(&me(), (id.as_str(), args)) {
                Ok(InvokeOutcome::Done(_)) => true,
                Ok(InvokeOutcome::Asked(_)) => {
                    let (call, values) = self.vision.question();
                    matches!(
                        self.rt
                            .invoke_admitted(&me(), (call.as_str(), values), None),
                        Ok(InvokeOutcome::Done(_))
                    )
                }
                _ => false,
            }
        }

        /// A field of the library as text.
        fn text(&self, field: &str) -> String {
            self.rt
                .read(&self.lib, field)
                .unwrap()
                .as_text()
                .unwrap_or_default()
                .to_owned()
        }

        /// This person's face scan.
        fn scan(&self) -> ResourceRef {
            self.records("face_scans")
                .into_iter()
                .next()
                .expect("the person has a face scan")
        }

        /// The labels of a view's buttons and progress bars.
        fn labels(view: &ViewInstance) -> Vec<String> {
            let mut labels: Vec<String> = of_kind(view.tree(), "ui.Button")
                .into_iter()
                .map(|node| text_prop(node, "label"))
                .collect();
            labels.extend(
                of_kind(view.tree(), "ui.ProgressBar")
                    .into_iter()
                    .map(|node| text_prop(node, "label")),
            );
            labels
        }

        fn call(&self, action: &str, args: Vec<Value>) -> tilolang_runtime::InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|why| panic!("{action}: {why}"))
        }

        /// Runs `action` as the person's yes delivers it, where the window asked them first
        /// because the action leaves the device. Answers what the action came to.
        fn approve(&self, action: &str, args: Vec<Value>) -> Value {
            let id = self.app.action(action);
            match self
                .rt
                .invoke_admitted(&me(), (id.as_str(), args), None)
                .unwrap_or_else(|why| panic!("{action}: {why}"))
            {
                InvokeOutcome::Done(value) => value,
                other => panic!("{action}, the person said yes, came to {other:?}"),
            }
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
                    other @ InvokeOutcome::Denied => {
                        panic!("{action}, the person said yes, came to {other:?}")
                    }
                }
            }
        }

        /// The picture a person chose, as the device hands it over.
        fn chosen(&self, name: &str) -> Value {
            let kept = self
                .device
                .keep(name, "image/png", &picture(name))
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

        fn texts(view: &ViewInstance) -> Vec<String> {
            of_kind(view.tree(), "ui.Text")
                .into_iter()
                .map(|node| text_prop(node, "value"))
                .collect()
        }

        /// The case `name` of the app's `Change`, as a value: what a batch edit says about a
        /// field.
        fn change(&self, name: &str) -> Value {
            let ids = &self.app.compiled.ids;
            Value::Enum(EnumValue {
                ty: ids
                    .get("Change")
                    .expect("the app declares Change")
                    .to_owned(),
                variant: ids
                    .get(&format!("Change.{name}"))
                    .unwrap_or_else(|| panic!("Change has no case {name}"))
                    .to_owned(),
                fields: std::collections::BTreeMap::new(),
            })
        }

        /// Edits the details of the photos `ids`, naming only the fields the test changes: every
        /// other field is left as each photo has it.
        fn edit_selected(&self, ids: &[&ResourceRef], given: &[(&str, Value)]) -> InvokeOutcome {
            let mut args = vec![
                ("lib", Value::resource(&self.lib)),
                (
                    "ids",
                    Value::List(ids.iter().map(|id| Value::text(id.id_string())).collect()),
                ),
            ];
            args.extend(given.iter().map(|(name, value)| (*name, value.clone())));
            let filled = self.app.args(&self.rt, "edit_selected", &args);
            let id = self.app.action("edit_selected");
            self.rt
                .invoke_action(&me(), (id.as_str(), filled))
                .unwrap_or_else(|why| panic!("edit_selected: {why}"))
        }

        /// The key of the control of `kind` whose `prop` says `text`.
        fn control(view: &ViewInstance, kind: &str, prop: &str, text: &str) -> String {
            of_kind(view.tree(), kind)
                .into_iter()
                .find(|node| node.props.get(prop).and_then(Value::as_text) == Some(text))
                .unwrap_or_else(|| panic!("no {kind} with {prop} `{text}`:\n{}", dump(view.tree())))
                .key
                .clone()
        }

        /// The titles of the picture cards the view shows, in order.
        fn cards(view: &ViewInstance) -> Vec<String> {
            of_kind(view.tree(), "ui.MediaCard")
                .into_iter()
                .map(|card| text_prop(card, "title"))
                .collect()
        }
    }

    tilo_test_env::scenario! {
        fn photos_imports_reads_and_copies_a_picture;

        phase a_folder_import_runs_as_a_job_and_says_what_it_read;
        {
            let g = Gallery::new();
            g.call(
                "import_folder",
                vec![
                    Value::resource(&g.lib),
                    Value::List(vec![
                        g.chosen("Harbour.png"),
                        g.chosen("Quay-at-dusk.png"),
                        g.not_a_picture("Notes.txt"),
                    ]),
                ],
            );
            // The device is asked to read the two pictures; the notes are no picture.
            let (job, files) = g.last_import();
            assert_eq!(
                files.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
                ["Harbour.png", "Quay-at-dusk.png"]
            );
            assert_eq!(g.text("import_job"), job, "the library holds the job under way");
            assert_eq!(g.number("import_found"), 3);
            assert!(g.photos().is_empty(), "nothing arrives before the job reads it");
            // The window reads how far the job has got from its record, as the device pushes it.
            g.engine.moves(&g.rt, &job, "running",
                &serde_json::json!({
                    "job": job, "state": "running", "found": 2, "done": 1,
                    "photos": [], "issues": [],
                }),
            );
            let page = g.open("library", vec![Value::resource(&g.lib)]);
            let labels = Gallery::labels(&page);
            assert!(
                labels.iter().any(|l| l == "Importing 1 of 2"),
                "the window shows the job's progress: {labels:?}"
            );
            assert!(labels.iter().any(|l| l == "Stop"), "and a way to stop it: {labels:?}");
            // The job ends: its `done:` runs as the person who started it, and what it read joins
            // the library; what it could not read is kept to try again.
            let (harbour, quay) = (&files[0].1, &files[1].1);
            g.engine.moves(&g.rt, &job, "done",
                &serde_json::json!({
                    "job": job, "state": "done", "found": 2, "done": 2,
                    "photos": [{
                        "name": "Harbour.png", "picture": harbour, "thumb": "",
                        "folder": "Trip", "captured_at": "2026:07:12 18:16:00",
                        "latitude": 53.5, "longitude": 10.0,
                    }],
                    "issues": [{
                        "name": "Quay-at-dusk.png", "picture": quay, "path": "Trip/Quay-at-dusk.png",
                        "why": "Could not read this image", "retry": true,
                    }],
                }),
            );
            let photos = g.photos();
            assert_eq!(photos.len(), 1, "the picture the device read became a photo");
            assert_eq!(
                g.rt.read(&photos[0], "captured_at").unwrap(),
                Value::text("2026:07:12 18:16:00")
            );
            assert_eq!(g.rt.read(&photos[0], "folder").unwrap(), Value::text("Trip"));
            assert_eq!(g.number("import_done"), 1);
            assert_eq!(g.text("import_job"), "", "no import is under way any more");
            assert_eq!(g.records("unread").len(), 1, "the picture it could not read is kept");
            let page = g.open("library", vec![Value::resource(&g.lib)]);
            let banner = of_kind(page.tree(), "ui.Banner")
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("the window shows no import report: {}", dump(page.tree())));
            assert_eq!(
                banner.props.get("title").and_then(Value::as_text),
                Some("Imported 1 of 3"),
                "the window says how many arrived"
            );
            assert_eq!(
                banner.props.get("message").and_then(Value::as_text),
                Some("Notes.txt is not a picture, Quay-at-dusk.png: Could not read this image"),
                "and what could not be read, and why"
            );
            assert!(Gallery::labels(&page).iter().any(|l| l == "Try again"));
            // Each file that did not become a photo is a row of its own, saying why.
            let problems: Vec<String> = of_kind(page.tree(), "ui.ListRow")
                .into_iter()
                .filter_map(|row| row.props.get("title").and_then(Value::as_text).map(str::to_owned))
                .collect();
            assert!(
                problems.iter().any(|p| p == "Quay-at-dusk.png: Could not read this image"),
                "the import's problems are listed one file at a time: {problems:?}"
            );

            // Trying again asks the device for the picture that failed, and nothing else.
            g.call("retry_import", vec![Value::resource(&g.lib)]);
            let (again, files) = g.last_import();
            assert_eq!(files, [("Quay-at-dusk.png".to_owned(), quay.clone())]);
            // Stopping it asks the device to stop, and the job ends saying so.
            g.call("stop_import", vec![Value::resource(&g.lib)]);
            assert_eq!(
                g.engine.cancelled().as_slice(),
                std::slice::from_ref(&again)
            );
            g.engine.moves(&g.rt, &again, "cancelled",
                &serde_json::json!({
                    "job": again, "state": "stopped", "found": 1, "done": 0,
                    "photos": [], "issues": [],
                }),
            );
            let page = g.open("library", vec![Value::resource(&g.lib)]);
            let banner = of_kind(page.tree(), "ui.Banner").into_iter().next().unwrap();
            assert_eq!(
                banner.props.get("title").and_then(Value::as_text),
                Some("Import stopped: 0 of 1 imported")
            );
            assert_eq!(g.photos().len(), 1);

            // Reading a folder again hands the device the photos the library holds there.
            g.call(
                "reindex",
                vec![Value::resource(&g.lib), Value::text("Trip")],
            );
            let (_, files) = g.last_import();
            assert_eq!(files, [("Harbour.png".to_owned(), harbour.clone())]);
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
            g.call(
                "recognize_text",
                vec![Value::resource(&g.lib), Value::resource(&photo)],
            );
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
            let texts = Gallery::texts(&details);
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
            // Recognition is off until the person turns it on: no scan reaches the model.
            assert!(
                !g.attempt("scan_faces", vec![Value::resource(&g.lib)]),
                "a scan with recognition off is refused"
            );
            assert!(g.vision.asked.borrow().is_empty());
            g.call("set_recognition", vec![Value::resource(&g.lib), Value::Bool(true)]);
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
            // A scan again goes on with the photos it has not read: there are none, so it asks the
            // model nothing and suggests nobody twice.
            g.call_and_approve("scan_faces", vec![Value::resource(&g.lib)]);
            assert_eq!(g.vision.asked.borrow().len(), 1);
            assert_eq!(g.records("suggestions").len(), 1);
            assert_eq!(g.rt.read(&g.scan(), "state").unwrap(), Value::text(""));
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
            // Deleting this device's face data keeps the people already named.
            g.call("delete_face_data", vec![Value::resource(&g.lib)]);
            assert_eq!(g.records("people").len(), 1);
            assert_eq!(g.records("suggestions").len(), 1, "a named face is the workspace's");
            assert!(g.ids_of(&g.scan(), "scanned").is_empty());
        }

        phase a_face_set_aside_is_hidden_and_can_come_back;
        {
            let g = Gallery::new();
            g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            );
            g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Quay.png")],
            );
            g.call("set_recognition", vec![Value::resource(&g.lib), Value::Bool(true)]);
            g.call_and_approve("scan_faces", vec![Value::resource(&g.lib)]);
            let suggestion = g
                .records("suggestions")
                .into_iter()
                .next()
                .expect("the face the model found");
            // Setting a face aside keeps it: the person sees it again when they ask, and the
            // photos are not read into a second suggestion meanwhile.
            g.call(
                "hide_suggestion",
                vec![Value::resource(&g.lib), Value::resource(&suggestion)],
            );
            assert_eq!(g.records("suggestions").len(), 1, "a hidden face is kept");
            assert_eq!(
                g.rt.read(&suggestion, "hidden").unwrap(),
                Value::Bool(true),
                "the face is out of the list"
            );
            g.call_and_approve("scan_faces", vec![Value::resource(&g.lib)]);
            assert_eq!(g.records("suggestions").len(), 1, "and not offered twice");
            // The people page keeps it out of sight until the person asks for the hidden ones.
            let mut page = g.open("library", vec![Value::resource(&g.lib)]);
            let people = of_kind(page.tree(), "ui.NavItem")
                .into_iter()
                .find(|item| item.props.get("key") == Some(&Value::text("people")))
                .unwrap_or_else(|| panic!("no People place: {}", dump(page.tree())))
                .key
                .clone();
            page.fire(&people, "select").unwrap();
            page.poll_patches().unwrap();
            assert!(
                !Gallery::labels(&page).iter().any(|l| l == "Approve"),
                "a hidden face is not in the list: {:?}",
                Gallery::labels(&page)
            );
            let show = Gallery::control(&page, "ui.Button", "label", "Show hidden faces");
            page.fire(&show, "press").unwrap();
            page.poll_patches().unwrap();
            assert!(
                Gallery::labels(&page).iter().any(|l| l == "Show again"),
                "the hidden face is shown: {:?}",
                Gallery::labels(&page)
            );
            // Bring it back, and it is a face to name again.
            g.call(
                "restore_suggestion",
                vec![Value::resource(&g.lib), Value::resource(&suggestion)],
            );
            assert_eq!(
                g.rt.read(&suggestion, "hidden").unwrap(),
                Value::Bool(false),
                "the face is back in the list"
            );
        }

        phase a_face_scan_goes_batch_by_batch_and_pauses;
        {
            let g = Gallery::new();
            // Thirteen pictures: the model reads twelve at a time. Each name is its own length,
            // so each picture is its own.
            for at in 0..13 {
                let name = format!("{}.png", "p".repeat(at + 1));
                g.call("import_photo", vec![Value::resource(&g.lib), g.chosen(&name)]);
            }
            assert_eq!(g.photos().len(), 13);
            g.call("set_recognition", vec![Value::resource(&g.lib), Value::Bool(true)]);
            g.call_and_approve("scan_faces", vec![Value::resource(&g.lib)]);
            let scan = g.scan();
            assert_eq!(g.ids_of(&scan, "scanned").len(), 12, "one batch of twelve was read");
            assert_eq!(g.rt.read(&scan, "state").unwrap(), Value::text("running"));
            let people = g.open("face_recognition", vec![Value::resource(&g.lib)]);
            assert!(
                Gallery::labels(&people).iter().any(|l| l == "Scanned 12 of 13 photos"),
                "the people page shows how far the scan has got: {:?}",
                Gallery::labels(&people)
            );
            // Paused, the scan reads nothing more.
            g.call("pause_scan", vec![Value::resource(&g.lib)]);
            assert_eq!(g.rt.read(&scan, "state").unwrap(), Value::text("paused"));
            g.call_and_approve("scan_step", vec![Value::resource(&g.lib)]);
            assert_eq!(g.vision.asked.borrow().len(), 1, "a paused scan asks the model nothing");
            let people = g.open("face_recognition", vec![Value::resource(&g.lib)]);
            assert!(Gallery::texts(&people).iter().any(|t| t == "Scanning is paused at 12 of 13 photos."));
            // Going on reads the photo left.
            g.call_and_approve("scan_faces", vec![Value::resource(&g.lib)]);
            assert_eq!(g.ids_of(&scan, "scanned").len(), 13);
            assert_eq!(g.vision.asked.borrow().len(), 2);
            assert_eq!(g.records("suggestions").len(), 2);
            // The switch off ends the scan, and a scan with it off is refused.
            g.call("set_recognition", vec![Value::resource(&g.lib), Value::Bool(false)]);
            assert_eq!(g.rt.read(&scan, "state").unwrap(), Value::text(""));
            assert!(!g.attempt("scan_faces", vec![Value::resource(&g.lib)]));
            g.call_and_approve("scan_step", vec![Value::resource(&g.lib)]);
            assert_eq!(g.vision.asked.borrow().len(), 2, "recognition off reads nothing");
            // Deleting the face data takes the faces nobody named and what the scan had read.
            g.call("delete_face_data", vec![Value::resource(&g.lib)]);
            assert!(g.records("suggestions").is_empty());
            assert!(g.ids_of(&scan, "scanned").is_empty());
            // The face scan is its person's alone.
            let other = tilolang_runtime::Actor::from(tilolang_runtime::ids::MemberId::person("bo"));
            assert!(!g.rt.readers_admit(&other, &scan));
        }

        phase a_photo_the_model_cannot_read_says_so;
        {
            let g = Gallery::new();
            let photo = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            *g.vision.refuse.borrow_mut() = true;
            for action in ["describe_photo", "tag_photo", "analyze_photo"] {
                g.call_and_approve(
                    action,
                    vec![Value::resource(&g.lib), Value::resource(&photo)],
                );
                assert_eq!(
                    g.rt.read(&g.lib, "notice").unwrap(),
                    Value::text("the model is not on this device"),
                    "{action}'s failure is said"
                );
            }
        }

        phase text_is_read_in_the_language_the_library_names;
        {
            let g = Gallery::new();
            let photo = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            // The language the reader is given is the library's, and it is checked.
            assert!(!g.attempt(
                "set_text_language",
                vec![Value::resource(&g.lib), Value::text("english!")],
            ));
            g.call(
                "set_text_language",
                vec![Value::resource(&g.lib), Value::text("deu")],
            );
            assert_eq!(g.rt.read(&g.lib, "text_language").unwrap(), Value::text("deu"));
            g.call(
                "recognize_text",
                vec![Value::resource(&g.lib), Value::resource(&photo)],
            );
            assert_eq!(
                g.vision.ocr_language.borrow().as_slice(),
                &["deu".to_owned()][..],
                "the device reads the picture's text in German"
            );
            assert_eq!(
                g.rt.read(&photo, "recognized").unwrap(),
                Value::text("HARBOUR CAFE, OPEN 7 TO 4")
            );
        }

        phase a_copy_the_clipboard_refuses_says_so;
        {
            let g = Gallery::new();
            let photo = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            g.clipboard.refuse(true);
            g.call_and_approve(
                "copy_photo",
                vec![Value::resource(&g.lib), Value::resource(&photo)],
            );
            assert_eq!(
                g.rt.read(&g.lib, "notice").unwrap(),
                Value::text("The picture could not be copied to the clipboard."),
                "a refused copy is said, not dropped"
            );
            g.clipboard.refuse(false);
            g.call_and_approve(
                "copy_photo",
                vec![Value::resource(&g.lib), Value::resource(&photo)],
            );
            assert!(g.clipboard.held().is_some(), "the picture is on the clipboard");
            assert_eq!(
                g.rt.read(&g.lib, "notice").unwrap(),
                Value::text("The picture is on the clipboard.")
            );
        }

        phase copying_a_photo_puts_its_picture_on_the_clipboard;
        {
            let g = Gallery::new();
            let photo = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            g.call_and_approve(
                "copy_photo",
                vec![Value::resource(&g.lib), Value::resource(&photo)],
            );
            let copied = g.clipboard.held();
            let (mime, bytes) = copied.expect("the picture was copied");
            assert_eq!(mime, "image/png");
            assert_eq!(bytes, picture("Harbour.png"), "the picture's own bytes are what was copied");
        }

        phase many_photos_details_say_mixed_and_change_only_what_is_edited;
        {
            let g = Gallery::new();
            let dawn = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            let dunes = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Dunes.png")],
            ));
            let pines = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Pine.png")],
            ));
            g.call("tag", vec![Value::resource(&dawn), Value::text("sea")]);
            g.call("tag", vec![Value::resource(&dunes), Value::text("sand")]);
            g.call("tag", vec![Value::resource(&pines), Value::text("fog")]);
            // One photo alone has a folder, so the selection differs in it.
            g.edit_selected(
                &[&dunes],
                &[
                    ("folder", g.change("set")),
                    ("folder_text", Value::some(Value::text("Trips"))),
                ],
            );
            // The person selects the three in the grid.
            let mut page = g.open("library", vec![Value::resource(&g.lib)]);
            let select = Gallery::control(&page, "ui.Button", "label", "Select");
            page.press(&select).unwrap();
            page.poll_patches().unwrap();
            let cards: Vec<String> = of_kind(page.tree(), "ui.MediaCard")
                .into_iter()
                .map(|card| card.key.clone())
                .collect();
            assert_eq!(cards.len(), 3, "the library shows the three photos");
            for card in cards {
                page.fire(&card, "activate").unwrap();
            }
            page.poll_patches().unwrap();
            let edit = Gallery::control(&page, "ui.Button", "label", "Edit details");
            page.press(&edit).unwrap();
            page.poll_patches().unwrap();
            // The editor says where the photos differ, and where none of them holds a value.
            let texts = Gallery::texts(&page);
            assert!(
                texts.iter().any(|text| text == "Details of 3 photos"),
                "the editor names the selection: {texts:?}"
            );
            assert!(
                texts.iter().any(|text| text == "Tags: Mixed"),
                "the photos' tags differ: {texts:?}"
            );
            assert!(
                texts.iter().any(|text| text == "Folder: Mixed"),
                "the photos' folders differ: {texts:?}"
            );
            assert!(
                texts.iter().any(|text| text == "Place: None"),
                "none of the photos has a place: {texts:?}"
            );
            assert!(
                texts.iter().any(|text| text == "People: None"),
                "none of the photos names a person: {texts:?}"
            );
            // The person adds one tag and saves, changing nothing else.
            let add = Gallery::control(&page, "ui.TextInput", "placeholder", "Tags to add, separated by commas");
            page.edit(&add, &Value::text("holiday")).unwrap();
            let save = Gallery::control(&page, "ui.Button", "label", "Save");
            page.press(&save).unwrap();
            page.poll_patches().unwrap();
            for (photo, own) in [(&dawn, "sea"), (&dunes, "sand"), (&pines, "fog")] {
                let tags = g.ids_of(photo, "tags");
                assert!(tags.contains(&own.to_owned()), "{photo:?} keeps {own}: {tags:?}");
                assert!(tags.contains(&"holiday".to_owned()), "{photo:?} gains the tag: {tags:?}");
            }
            assert_eq!(
                g.rt.read(&dunes, "folder").unwrap(),
                Value::text("Trips"),
                "a field the person did not touch keeps what each photo has"
            );
            assert!(
                !Gallery::texts(&page).iter().any(|text| text == "Details of 3 photos"),
                "saving closes the editor:\n{}",
                dump(page.tree())
            );
        }

        phase saving_a_batch_edit_with_nothing_typed_changes_nothing;
        {
            let g = Gallery::new();
            let harbour = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            let dunes = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Dunes.png")],
            ));
            let mut page = g.open("library", vec![Value::resource(&g.lib)]);
            let select = Gallery::control(&page, "ui.Button", "label", "Select");
            page.press(&select).unwrap();
            page.poll_patches().unwrap();
            let cards: Vec<String> = of_kind(page.tree(), "ui.MediaCard")
                .into_iter()
                .map(|card| card.key.clone())
                .collect();
            for card in cards {
                page.fire(&card, "activate").unwrap();
            }
            page.poll_patches().unwrap();
            let edit = Gallery::control(&page, "ui.Button", "label", "Edit details");
            page.press(&edit).unwrap();
            page.poll_patches().unwrap();
            // Save with every field left alone says what is missing, and the editor stays open.
            let save = Gallery::control(&page, "ui.Button", "label", "Save");
            let refused = page.press(&save);
            assert!(
                refused.is_err(),
                "an edit with no field filled in is refused: {refused:?}"
            );
            page.poll_patches().unwrap();
            assert!(
                Gallery::texts(&page).iter().any(|text| text == "Details of 2 photos"),
                "the editor stays open to say why:\n{}",
                dump(page.tree())
            );
            for photo in [&harbour, &dunes] {
                assert_eq!(g.rt.read(photo, "folder").unwrap(), Value::text(""));
                assert_eq!(g.rt.read(photo, "location").unwrap(), Value::text(""));
                assert!(g.ids_of(photo, "tags").is_empty());
            }
        }

        phase a_batch_edit_gives_every_photo_the_same_place_and_people;
        {
            let g = Gallery::new();
            let harbour = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            let quay = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Quay.png")],
            ));
            let ava = returned_ref(g.call(
                "new_person",
                vec![
                    Value::resource(&g.lib),
                    Value::text("Ava"),
                    Value::List(vec![Value::text(harbour.id_string())]),
                ],
            ));
            g.edit_selected(
                &[&harbour, &quay],
                &[
                    ("place", g.change("set")),
                    ("place_name", Value::some(Value::text("Harbour"))),
                    ("latitude", Value::Float(48.5)),
                    ("longitude", Value::Float(-3.2)),
                    ("people", g.change("add")),
                    ("person_names", Value::List(vec![Value::text("Ava")])),
                ],
            );
            for photo in [&harbour, &quay] {
                assert_eq!(
                    g.rt.read(photo, "location").unwrap(),
                    Value::text("Harbour"),
                    "every selected photo takes the place"
                );
                assert_eq!(g.rt.read(photo, "latitude").unwrap(), Value::Float(48.5));
                assert_eq!(g.rt.read(photo, "longitude").unwrap(), Value::Float(-3.2));
            }
            assert_eq!(
                g.ids_of(&ava, "photos").len(),
                2,
                "the person the photos name has both on their page"
            );
            // Taking everyone off every photo takes them off the person too.
            g.edit_selected(&[&harbour, &quay], &[("people", g.change("clear"))]);
            assert!(g.ids_of(&ava, "photos").is_empty(), "the person keeps nobody");
            // Clearing the place empties what each photo's place was.
            g.edit_selected(&[&harbour, &quay], &[("place", g.change("clear"))]);
            for photo in [&harbour, &quay] {
                assert_eq!(g.rt.read(photo, "location").unwrap(), Value::text(""));
                assert_eq!(g.rt.read(photo, "latitude").unwrap(), Value::Float(0.0));
                assert_eq!(g.rt.read(photo, "longitude").unwrap(), Value::Float(0.0));
            }
        }

        phase one_photo_s_coordinates_are_typed_in_its_details;
        {
            let g = Gallery::new();
            let photo = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            let mut details = g.open(
                "details_fields",
                vec![Value::resource(&g.lib), Value::resource(&photo)],
            );
            let latitude = Gallery::control(&details, "ui.NumberInput", "label", "Latitude");
            details.edit(&latitude, &Value::Float(48.5)).unwrap();
            assert_eq!(
                g.rt.read(&photo, "latitude").unwrap(),
                Value::Float(48.5),
                "a typed latitude is the photo's"
            );
            details.poll_patches().unwrap();
            let longitude = Gallery::control(&details, "ui.NumberInput", "label", "Longitude");
            details.edit(&longitude, &Value::Float(-3.2)).unwrap();
            assert_eq!(g.rt.read(&photo, "longitude").unwrap(), Value::Float(-3.2));
        }

        phase the_timeline_opens_a_month_and_a_day;
        {
            let g = Gallery::new();
            let july12 = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            let july20 = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Dunes.png")],
            ));
            let august = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Pine.png")],
            ));
            for (photo, when) in [
                (&july12, "2026-07-12T05:42Z"),
                (&july20, "2026-07-20T17:10Z"),
                (&august, "2026-08-03T07:15Z"),
            ] {
                g.edit_selected(
                    &[photo],
                    &[
                        ("captured_at", g.change("set")),
                        ("captured_text", Value::some(Value::text(when))),
                    ],
                );
            }
            let mut page = g.open("library", vec![Value::resource(&g.lib)]);
            let timeline = of_kind(page.tree(), "ui.NavItem")
                .into_iter()
                .find(|item| item.props.get("key") == Some(&Value::text("timeline")))
                .unwrap_or_else(|| panic!("no Timeline place:\n{}", dump(page.tree())))
                .key
                .clone();
            page.fire(&timeline, "select").unwrap();
            page.poll_patches().unwrap();
            assert_eq!(
                Gallery::cards(&page),
                ["Pine.png", "Dunes.png", "Harbour.png"],
                "the timeline shows every photo, newest first"
            );
            // The person opens July.
            Gallery::control(&page, "ui.MenuButton", "label", "All months");
            let july = of_kind(page.tree(), "ui.MenuItem")
                .into_iter()
                .find(|item| text_prop(item, "label") == "2026-07")
                .unwrap_or_else(|| panic!("no July in the months menu:\n{}", dump(page.tree())))
                .key
                .clone();
            page.fire(&july, "choose").unwrap();
            page.poll_patches().unwrap();
            assert_eq!(
                Gallery::cards(&page),
                ["Dunes.png", "Harbour.png"],
                "only July's photos are shown"
            );
            // And then a day of it.
            Gallery::control(&page, "ui.MenuButton", "label", "Every day");
            let day = of_kind(page.tree(), "ui.MenuItem")
                .into_iter()
                .find(|item| text_prop(item, "label") == "2026-07-20")
                .unwrap_or_else(|| panic!("no day in the days menu:\n{}", dump(page.tree())))
                .key
                .clone();
            page.fire(&day, "choose").unwrap();
            page.poll_patches().unwrap();
            assert_eq!(
                Gallery::cards(&page),
                ["Dunes.png"],
                "only that day's photo is shown"
            );
        }

        phase a_photographs_edits_are_recorded_and_drawn_without_writing_it;
        {
            let g = Gallery::new();
            let photo = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            let original = text_of(&g.rt.read(&photo, "picture").unwrap());
            g.call("rotate_right", vec![Value::resource(&photo)]);
            assert_eq!(g.rt.read(&photo, "rotation").unwrap(), Value::Int(1));
            let turned = text_of(&g.rt.read(&photo, "edited").unwrap());
            assert_ne!(turned, original, "the device drew the photograph turned");
            assert_eq!(
                g.rt.read(&photo, "picture").unwrap(),
                Value::text(original.clone()),
                "the photograph itself is never written"
            );
            // A crop narrows what the person kept, and a turn after it keeps the crop: the turn
            // is of the photograph, not of the picture the window draws.
            g.call(
                "crop_photo",
                vec![
                    Value::resource(&photo),
                    Value::Float(0.0),
                    Value::Float(0.0),
                    Value::Float(0.5),
                    Value::Float(0.5),
                ],
            );
            assert_eq!(g.rt.read(&photo, "rotation").unwrap(), Value::Int(1));
            let cropped = text_of(&g.rt.read(&photo, "edited").unwrap());
            assert_ne!(cropped, turned);
            let drawn_pixels = g.device.bytes_at(&cropped);
            let drawn_image = tilo_image_codec::decode_upright_now(&drawn_pixels, None)
                .expect("what the device drew is a picture");
            assert_eq!(
                (drawn_image.width, drawn_image.height),
                (32, 32),
                "the window's half of the photograph is what the device drew"
            );
            let drawn = g.rt.read(&photo, "crop").unwrap();
            assert!(
                matches!(drawn, Value::List(ref parts) if parts.len() == 4),
                "the part they kept is recorded: {drawn:?}"
            );
            g.call("rotate_left", vec![Value::resource(&photo)]);
            assert_eq!(g.rt.read(&photo, "rotation").unwrap(), Value::Int(0));
            assert_eq!(
                g.rt.read(&photo, "crop").unwrap(),
                drawn,
                "turning keeps the part they kept"
            );
            // The window shows the edit, and the photograph as it came beside it.
            let mut page = g.open("library", vec![Value::resource(&g.lib)]);
            let card = of_kind(page.tree(), "ui.MediaCard")
                .into_iter()
                .next()
                .expect("the library shows the photograph")
                .key
                .clone();
            page.fire(&card, "activate").unwrap();
            page.poll_patches().unwrap();
            let shown = image_of(&page);
            assert_eq!(
                shown,
                text_of(&g.rt.read(&photo, "edited").unwrap()),
                "the viewer draws the edit"
            );
            let before = Gallery::control(&page, "ui.Button", "label", "Before");
            page.press(&before).unwrap();
            page.poll_patches().unwrap();
            assert_eq!(
                image_of(&page),
                original,
                "before shows the photograph as it came"
            );
            assert!(
                Gallery::texts(&page).iter().any(|text| text == "Before the edits"),
                "the window says which it shows"
            );
            // Putting it back leaves the photograph as it came, and nothing edited.
            g.call("reset_edits", vec![Value::resource(&photo)]);
            assert_eq!(g.rt.read(&photo, "edited").unwrap(), Value::text(""));
            assert_eq!(g.rt.read(&photo, "rotation").unwrap(), Value::Int(0));
            assert_eq!(g.rt.read(&photo, "crop").unwrap(), Value::List(Vec::new()));
        }

        phase an_album_draft_saves_or_says_another_member_changed_it;
        {
            let g = Gallery::new();
            let harbour = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            let mut page = g.open("library", vec![Value::resource(&g.lib)]);
            // The person starts an album, names it, and chooses the photo in the library.
            let new_album = Gallery::control(&page, "ui.Button", "label", "New album");
            page.press(&new_album).unwrap();
            page.poll_patches().unwrap();
            let name = Gallery::control(&page, "ui.TextInput", "placeholder", "Album name");
            page.edit(&name, &Value::text("Harbour days")).unwrap();
            let card = of_kind(page.tree(), "ui.MediaCard")
                .into_iter()
                .next()
                .expect("the draft shows the library")
                .key
                .clone();
            page.fire(&card, "activate").unwrap();
            page.poll_patches().unwrap();
            let save = Gallery::control(&page, "ui.Button", "label", "Save album");
            page.press(&save).unwrap();
            page.poll_patches().unwrap();
            let albums = g.records("albums");
            assert_eq!(albums.len(), 1, "saving the draft starts the album");
            assert_eq!(g.rt.read(&albums[0], "name").unwrap(), Value::text("Harbour days"));
            assert_eq!(g.ids_of(&albums[0], "photos"), vec![harbour.id_string()]);
            assert!(
                !Gallery::texts(&page).iter().any(|t| t == "Choose the photos for this album."),
                "saving closes the draft:\n{}",
                dump(page.tree())
            );

            // A draft is its person's alone until it is saved: nobody else watches it being made.
            g.call("start_album_draft", vec![Value::resource(&g.lib), Value::text("")]);
            let drafts = g.records("album_drafts");
            assert_eq!(drafts.len(), 1, "the draft is open");
            let other = tilolang_runtime::Actor::from(tilolang_runtime::ids::MemberId::person("bo"));
            assert!(g.rt.readers_admit(&me(), &drafts[0]));
            assert!(!g.rt.readers_admit(&other, &drafts[0]), "another member does not read it");
            g.call("cancel_album_draft", vec![Value::resource(&g.lib)]);
            assert!(g.records("album_drafts").is_empty(), "cancelling closes it");

            // A draft of that album, while someone else deletes it: saving says so, and the
            // album stays deleted rather than coming back from the draft.
            g.call("start_album_draft", vec![Value::resource(&g.lib), Value::text(albums[0].id_string())]);
            g.call("remove_album", vec![Value::resource(&g.lib), Value::resource(&albums[0])]);
            g.call("save_album", vec![Value::resource(&g.lib)]);
            assert!(g.records("albums").is_empty(), "a change made first is not overwritten");
            assert_eq!(
                g.text("notice"),
                "Someone else changed this album. Review it and save again."
            );

            // A photo taken out of the library while the draft names it is the same news.
            g.call("start_album_draft", vec![Value::resource(&g.lib), Value::text("")]);
            g.call("album_draft_toggle", vec![Value::resource(&g.lib), Value::resource(&harbour)]);
            g.call("remove", vec![Value::resource(&g.lib), Value::resource(&harbour)]);
            g.call("save_album", vec![Value::resource(&g.lib)]);
            assert!(g.records("albums").is_empty());
            assert_eq!(
                g.text("notice"),
                "Someone else changed this album. Review it and save again."
            );
        }

        phase the_analysis_page_names_the_model_and_the_photos_it_failed;
        {
            let g = Gallery::new();
            let photo = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            g.rt.set_reading(
                &analysis(),
                &serde_json::json!({
                    "phase": "complete", "total": 3, "completed": 3, "succeeded": 1,
                    "failed": 1, "remaining": 0, "current": "",
                    "model": "florence-2", "revision": "0123456789abcdef",
                    "progress": 1.0,
                    "failures": [{
                        "resource": format!("tl:so.tilo.photos:{}", photo.id_string()),
                        "reason": "The picture changed before analysis finished",
                    }],
                })
                .to_string(),
            )
            .unwrap();
            let page = g.open("analysis_page", vec![Value::resource(&g.lib)]);
            let texts = Gallery::texts(&page);
            assert!(texts.iter().any(|t| t == "Finished"), "{texts:?}");
            assert!(texts.iter().any(|t| t == "3 of 3 finished, 1 failed"), "{texts:?}");
            let properties: Vec<(String, String)> = of_kind(page.tree(), "ui.Property")
                .into_iter()
                .map(|node| (text_prop(node, "label"), text_prop(node, "value")))
                .collect();
            assert!(
                properties.contains(&("Model".to_owned(), "florence-2".to_owned())),
                "the model that read the pictures is named: {properties:?}"
            );
            assert!(
                properties.contains(&("Revision".to_owned(), "0123456789".to_owned())),
                "and the revision it read them at: {properties:?}"
            );
            let rows: Vec<(String, String)> = of_kind(page.tree(), "ui.ListRow")
                .into_iter()
                .map(|node| (text_prop(node, "title"), text_prop(node, "secondary")))
                .collect();
            assert!(
                rows.contains(&(
                    "Harbour.png".to_owned(),
                    "The picture changed before analysis finished".to_owned()
                )),
                "the photo it could not read is listed, with why: {rows:?}"
            );
            // The row opens the photo it is about, so a person looks at what failed.
            let row = of_kind(page.tree(), "ui.ListRow")
                .into_iter()
                .find(|node| text_prop(node, "title") == "Harbour.png")
                .expect("the failed photo's row")
                .key
                .clone();
            let opened = page
                .fire_with(&row, "activate", None)
                .unwrap_or_else(|why| panic!("the failed row opens its photo: {why}"));
            assert!(matches!(opened, InvokeOutcome::Done(_)), "{opened:?}");
            // Retrying asks the device to read only the pictures that failed.
            let page = page;
            let retry = Gallery::control(&page, "ui.Button", "label", "Retry failed photos");
            page.press(&retry).unwrap();
            assert_eq!(g.vision.asked.borrow().as_slice(), ["retry"]);
        }

        phase a_card_whose_picture_is_on_another_device_says_so;
        {
            let g = Gallery::new();
            let photo = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            // The bytes go from this device while the record stays: what a second device holds
            // before the picture has arrived.
            g.device.send_all_away();
            g.call("check_picture", vec![Value::resource(&photo)]);
            assert_eq!(
                g.rt.read(&photo, "picture_state").unwrap(),
                Value::text("waiting"),
                "the picture is not here"
            );
            let page = g.open("library", vec![Value::resource(&g.lib)]);
            let card = of_kind(page.tree(), "ui.MediaCard")
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("the library's card: {}", dump(page.tree())));
            // The thumbnail this device kept stands in while the photograph arrives.
            let thumb = match g.rt.read(&photo, "thumb").unwrap() {
                Value::Text(thumb) => thumb,
                other => panic!("the thumbnail reads {other:?}"),
            };
            assert!(!thumb.is_empty(), "the import kept a thumbnail");
            let drawn = match card.props.get("picture") {
                Some(Value::Picture(picture) | Value::Text(picture)) => Some(picture.clone()),
                Some(Value::Opt(Some(inner))) => match &**inner {
                    Value::Picture(picture) | Value::Text(picture) => Some(picture.clone()),
                    other => panic!("the card's picture reads {other:?}"),
                },
                other => panic!("the card's picture reads {other:?}"),
            };
            assert_eq!(
                drawn.as_deref(),
                Some(thumb.as_str()),
                "the card draws the thumbnail rather than nothing"
            );
            let meta: Vec<String> = match card.props.get("meta") {
                Some(Value::List(items)) => items
                    .iter()
                    .filter_map(|item| item.as_text().map(str::to_owned))
                    .collect(),
                other => panic!("the card's meta reads {other:?}"),
            };
            assert!(
                meta.contains(&"Arriving from another device".to_owned()),
                "the card says where its picture is: {meta:?}"
            );
        }

        phase what_a_photo_shows_is_read_in_one_call_and_shown;
        {
            let g = Gallery::new();
            let photo = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Harbour.png")],
            ));
            g.approve(
                "analyze_photo",
                vec![Value::resource(&g.lib), Value::resource(&photo)],
            );
            assert_eq!(g.vision.asked.borrow().as_slice(), ["shows"]);
            assert_eq!(
                g.rt.read(&photo, "description").unwrap(),
                Value::text("A dog beside a bicycle")
            );
            assert_eq!(
                g.rt.read(&photo, "recognized").unwrap(),
                Value::text("42 North Street")
            );
            let mut objects = g.ids_of(&photo, "objects");
            objects.sort();
            assert_eq!(objects, ["bicycle", "dog"]);
            assert_eq!(g.ids_of(&photo, "pets"), ["dog"]);
            // The photo's page says what it shows, the objects and the pets apart.
            let details = g.open(
                "details_fields",
                vec![Value::resource(&g.lib), Value::resource(&photo)],
            );
            assert!(
                Gallery::texts(&details).iter().any(|t| t == "A dog beside a bicycle"),
                "the details say what it shows: {:?}",
                Gallery::texts(&details)
            );
            let chips: Vec<String> = of_kind(details.tree(), "ui.Chip")
                .into_iter()
                .map(|chip| text_prop(chip, "label"))
                .collect();
            assert!(chips.iter().any(|c| c == "dog"), "{chips:?}");
            assert!(chips.iter().any(|c| c == "bicycle"), "{chips:?}");
            // The library's search button is the system's search, scoped to the app.
            let page = g.open("library", vec![Value::resource(&g.lib)]);
            assert!(
                !of_kind(page.tree(), "ui.SearchButton").is_empty(),
                "the window searches through the system's search"
            );
        }

        phase a_photo_whose_bytes_are_elsewhere_says_it_is_arriving;
        {
            let g = Gallery::new();
            let photo = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.chosen("Away.png")],
            ));
            assert_eq!(g.rt.read(&photo, "picture_state").unwrap(), Value::text("here"));
            // Another device holds the bytes for now: only the photo's record reached this one.
            let url = text_of(&g.rt.read(&photo, "picture").unwrap());
            let hash = url
                .strip_prefix("tilo-blob://")
                .expect("the picture's address")
                .to_owned();
            g.device.send_away(&hash);
            g.call("check_picture", vec![Value::resource(&photo)]);
            assert_eq!(
                g.rt.read(&photo, "picture_state").unwrap(),
                Value::text("waiting")
            );
            let mut page = g.open("library", vec![Value::resource(&g.lib)]);
            let card = of_kind(page.tree(), "ui.MediaCard")
                .into_iter()
                .next()
                .expect("the library shows the photo")
                .key
                .clone();
            page.fire(&card, "activate").unwrap();
            page.poll_patches().unwrap();
            let waiting = of_kind(page.tree(), "ui.EmptyState")
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("the window shows no waiting state:\n{}", dump(page.tree())));
            assert_eq!(text_prop(waiting, "title"), "Waiting for the photo");
            assert_eq!(
                text_prop(waiting, "message"),
                "It is still arriving from another device."
            );
            // The bytes arrive, and the same window draws the picture.
            g.device.arrive(&hash);
            g.call("check_picture", vec![Value::resource(&photo)]);
            page.poll_patches().unwrap();
            assert_eq!(g.rt.read(&photo, "picture_state").unwrap(), Value::text("here"));
            assert!(
                of_kind(page.tree(), "ui.ImageViewer").len() == 1,
                "the picture is drawn:\n{}",
                dump(page.tree())
            );

            // A picture that is here but is no picture the app opens says so.
            let notes = returned_ref(g.call(
                "import_photo",
                vec![Value::resource(&g.lib), g.not_a_picture("Notes.txt")],
            ));
            g.call("check_picture", vec![Value::resource(&notes)]);
            assert_eq!(
                g.rt.read(&notes, "picture_state").unwrap(),
                Value::text("unreadable")
            );
            let note_page = g.open(
                "photo",
                vec![Value::resource(&notes)],
            );
            let failed = of_kind(note_page.tree(), "ui.EmptyState")
                .into_iter()
                .next()
                .unwrap_or_else(|| {
                    panic!("the page draws no error state:\n{}", dump(note_page.tree()))
                });
            assert_eq!(text_prop(failed, "title"), "Could not open this photo");
            assert_eq!(
                failed.props.get("error"),
                Some(&Value::Bool(true)),
                "and shows it as the failure it is"
            );
        }
    }
}
