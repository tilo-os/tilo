//! The Photos app (`apps/tl/photos/app.tl`) end to end: a folder import that runs as a job on the
//! device, says how far it has got, can be stopped and tried again, and brings what it read into
//! the library when it ends; a picture imported twice that stays one photo; the words a device
//! reads into a picture; faces scanned batch by batch behind a switch, paused, and deleted; and a
//! copy of a picture on the clipboard.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::clipboard::ClipboardHost;
    use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
    use tilolang_runtime::host_work::{EngineHost, JobHandlers};
    use tilolang_runtime::{EnumValue, InvokeOutcome, ResourceRef, Runtime, Value, ViewInstance};

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

    /// The media type and bytes of what the person copied.
    type Copied = Option<(String, Vec<u8>)>;

    /// What the person copied: the media type and bytes of the last copy.
    #[derive(Clone, Default)]
    struct Clipboard(Rc<RefCell<Copied>>);

    impl ClipboardHost for Clipboard {
        fn copy(&self, mime: &str, bytes: &[u8]) -> bool {
            *self.0.borrow_mut() = Some((mime.to_owned(), bytes.to_vec()));
            true
        }

        fn paste(&self, _types: &[&str]) -> Option<(String, Vec<u8>)> {
            None
        }
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
                        .map_or(Value::List(Vec::new()), |(_, value)| value.clone());
                    Invoked::Done(Value::List(vec![images]))
                }
                other => Invoked::Failed(format!("this device answers no {other}")),
            }
        }
    }

    /// The device's engine for Photos: the jobs it was asked to start, by their input, the jobs it
    /// was asked to stop, and what each start named to run as its job moves, which the test runs
    /// as the device would.
    #[derive(Clone, Default)]
    struct Engine {
        started: Rc<RefCell<Vec<(String, String)>>>,
        cancelled: Rc<RefCell<Vec<String>>>,
        handlers: Rc<RefCell<HashMap<String, JobHandlers>>>,
    }

    impl Engine {
        /// The job last started, and the pictures it was asked to read, by name and address.
        fn last(&self) -> (String, Vec<(String, String)>) {
            let started = self.started.borrow();
            let (job, input) = started.last().expect("a job was started").clone();
            let ask: serde_json::Value = serde_json::from_str(&input).expect("the ask is JSON");
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
            (job, files)
        }

        /// The job's record as the device keeps it, with what the engine said of it (`detail`).
        fn record(job: &str, state: &str, detail: &serde_json::Value) -> serde_json::Value {
            serde_json::json!({
                "id": job, "app": "so.tilo.photos", "op": "import", "state": state,
                "detail": detail,
            })
        }
    }

    impl EngineHost for Engine {
        fn call(&self, _app: &str, op: &str, _input: &str) -> Result<String, String> {
            Err(format!("`{op}` is no op this device answers now"))
        }

        fn start(
            &self,
            app: &str,
            op: &str,
            input: &str,
            handlers: &JobHandlers,
        ) -> Result<String, String> {
            assert_eq!(app, "so.tilo.photos");
            assert_eq!(op, "import");
            let job = format!("devices/ab/jobs/photos-{}", self.started.borrow().len() + 1);
            self.started
                .borrow_mut()
                .push((job.clone(), input.to_owned()));
            self.handlers
                .borrow_mut()
                .insert(job.clone(), handlers.clone());
            Ok(job)
        }

        fn job(&self, job: &str) -> Result<Option<serde_json::Value>, String> {
            Err(format!(
                "the window reads job `{job}` from its record as the device hands it on, never \
                 by asking"
            ))
        }

        fn cancel(&self, job: &str) -> bool {
            self.cancelled.borrow_mut().push(job.to_owned());
            true
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
        engine: Engine,
    }

    impl Gallery {
        /// The job says how far it has got: the device hands its record on to the window.
        fn says(&self, job: &str, detail: &serde_json::Value) {
            self.rt
                .set_job(job, &Engine::record(job, "running", detail));
        }

        /// The job ends as `state`, saying `detail` last: the device hands the record on and
        /// runs what its start named for its end, as the person who started it.
        fn ends(&self, job: &str, state: &str, detail: &serde_json::Value) -> InvokeOutcome {
            let record = Engine::record(job, state, detail);
            self.rt.set_job(job, &record);
            let done = self.engine.handlers.borrow()[job]
                .done
                .clone()
                .expect("the import's start names what runs at its end");
            let call = self.rt.job_call(&done, &record).unwrap();
            self.rt.invoke_action(&me(), call).unwrap()
        }

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
            let engine = Engine::default();
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
            let (job, files) = g.engine.last();
            assert_eq!(
                files.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
                ["Harbour.png", "Quay-at-dusk.png"]
            );
            assert_eq!(g.text("import_job"), job, "the library holds the job under way");
            assert_eq!(g.number("import_found"), 3);
            assert!(g.photos().is_empty(), "nothing arrives before the job reads it");
            // The window reads how far the job has got from its record, as the device pushes it.
            g.says(
                &job,
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
            let finished = g.ends(
                &job,
                "done",
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
            assert!(
                matches!(finished, InvokeOutcome::Done(_)),
                "the job's `done:` ran: {finished:?}"
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

            // Trying again asks the device for the picture that failed, and nothing else.
            g.call("retry_import", vec![Value::resource(&g.lib)]);
            let (again, files) = g.engine.last();
            assert_eq!(files, [("Quay-at-dusk.png".to_owned(), quay.clone())]);
            // Stopping it asks the device to stop, and the job ends saying so.
            g.call("stop_import", vec![Value::resource(&g.lib)]);
            assert_eq!(
                g.engine.cancelled.borrow().as_slice(),
                std::slice::from_ref(&again)
            );
            let stopped = g.ends(
                &again,
                "cancelled",
                &serde_json::json!({
                    "job": again, "state": "stopped", "found": 1, "done": 0,
                    "photos": [], "issues": [],
                }),
            );
            assert!(matches!(stopped, InvokeOutcome::Done(_)), "{stopped:?}");
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
            let (_, files) = g.engine.last();
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
    }
}
