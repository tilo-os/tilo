//! The browser app (`apps/tl/browser/app.tl`) end to end: what a page downloads and what the
//! person does about it, a whole form answered in one ask, a saved address filled into a form a
//! site asked, the addresses the settings edit, and a sign-in turned down.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use serde_json::{Value as Json, json};
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::files::{Entry, FileTarget, FilesHost};
    use tilolang_runtime::host_work::{EngineHost, Push};
    use tilolang_runtime::page::PageHost;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, RunOutcome, Runtime, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/browser/app.tl");
    /// Files' published place to show: its own action, which the Browser calls rather than
    /// reaching the device's files itself.
    const FILES_SOURCE: &str = include_str!("../../tilolang/tests/fixtures/file_explorer.tl");
    /// Print Centre's published print: its own action, which the Browser calls with the PDF it
    /// has rather than printing it itself.
    const PRINT_CENTRE_SOURCE: &str = include_str!("../../tilolang/tests/fixtures/print_centre.tl");
    /// What the engine wrote for a page it printed.
    const PRINTED_PDF: &[u8] = b"%PDF-1.7\n% a page printed from the Browser\n";

    /// The page a window holds, as the test plays it: every ask it was given, and what it last
    /// reported of itself.
    #[derive(Clone, Default)]
    struct Page {
        asked: Rc<RefCell<Vec<Json>>>,
        asks: Rc<RefCell<Json>>,
    }

    impl Page {
        fn ops(&self) -> Vec<String> {
            self.asked
                .borrow()
                .iter()
                .filter_map(|ask| ask.get("op").and_then(Json::as_str).map(str::to_owned))
                .collect()
        }

        fn last(&self) -> Json {
            self.asked.borrow().last().cloned().expect("an ask")
        }

        fn asked_ops(&self) -> Vec<Json> {
            self.asked.borrow().clone()
        }

        fn answer_asks(&self, asks: Json) {
            *self.asks.borrow_mut() = asks;
        }

        /// Forget what was asked so far, so what a step asks is the whole of what it did.
        fn forget(&self) {
            self.asked.borrow_mut().clear();
        }
    }

    impl PageHost for Page {
        fn call(&self, request: &str) -> Result<String, String> {
            let ask: Json = serde_json::from_str(request).unwrap_or(Json::Null);
            let op = ask
                .get("op")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_owned();
            self.asked.borrow_mut().push(ask);
            if op == "asks" {
                return Ok(self.asks.borrow().to_string());
            }
            // The engine stops presenting the page: its next report of itself says so.
            if op == "exit_fullscreen" {
                let mut asks = self.asks.borrow_mut();
                if let Some(fields) = asks.as_object_mut() {
                    fields.insert("fullscreen".to_owned(), Json::Bool(false));
                }
            }
            Ok(json!({"ok": true}).to_string())
        }
    }

    /// The device's files, as a download's open asks them: what was opened, in order.
    #[derive(Clone, Default)]
    struct Files {
        opened: Rc<RefCell<Vec<String>>>,
    }

    impl FilesHost for Files {
        fn list(&self, _path: &str) -> Result<Vec<Entry>, String> {
            Ok(Vec::new())
        }

        fn stat(&self, _path: &str) -> Result<Option<Entry>, String> {
            Ok(None)
        }

        /// The PDF the engine wrote for a page it printed, and nothing else.
        fn read(&self, path: &str) -> Result<Vec<u8>, String> {
            if std::path::Path::new(path)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
            {
                Ok(PRINTED_PDF.to_vec())
            } else {
                Err("the test keeps no files".to_owned())
            }
        }

        fn remove(&self, _path: &str) -> Result<bool, String> {
            Ok(true)
        }

        fn watch(&self, _path: &str, _run: &str) -> Result<String, String> {
            Err("the test watches no folder".to_owned())
        }

        fn open(&self, file: FileTarget<'_>) -> Result<(), String> {
            if let FileTarget::Path(path) = file {
                self.opened.borrow_mut().push(path.to_owned());
            }
            Ok(())
        }
    }

    /// The device's content store, and the file the person picks when a control asks: the
    /// package an extension install takes, and the bytes a file the Browser hands on holds.
    #[derive(Clone, Default)]
    struct Device {
        content: Rc<RefCell<std::collections::HashMap<String, Vec<u8>>>>,
        file: Rc<RefCell<Option<FileValue>>>,
        /// The downloads the app started, as the engine was asked: the op and its input.
        started: Rc<RefCell<Vec<(String, Json)>>>,
        /// Where each job stands, as the engine tells it, by the job's id.
        jobs: Rc<RefCell<std::collections::HashMap<String, Json>>>,
        /// The jobs the app stopped, in order.
        cancelled: Rc<RefCell<Vec<String>>>,
        /// Where the device pushes what each job says of itself: the window's record of it.
        record: Rc<RefCell<Option<std::sync::Arc<dyn Push>>>>,
    }

    impl Device {
        /// The job `job` now stands as `state` says.
        fn job_stands(&self, job: &str, state: Json) {
            let mut standing = json!({"state": "running", "done": 0, "total": 0, "path": "",
                "name": "", "error": ""});
            for (field, value) in state.as_object().expect("a job's fields") {
                standing[field] = value.clone();
            }
            if let Some(record) = self.record.borrow().as_ref() {
                record.state(job, &standing.to_string());
            }
            self.jobs.borrow_mut().insert(job.to_owned(), standing);
        }
    }

    // The device's engine, as the Browser's downloads reach it: each start is a job of its own.
    impl EngineHost for Device {
        fn call(&self, _app: &str, op: &str, _input: &str) -> Result<String, String> {
            Err(format!("`{op}` is no op this device answers"))
        }

        fn start(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
            if op != "download" {
                return Err(format!("`{op}` is no op this device starts"));
            }
            let mut started = self.started.borrow_mut();
            started.push((
                op.to_owned(),
                serde_json::from_str(input).expect("the ask is JSON"),
            ));
            let job = format!("browser-{}", started.len());
            drop(started);
            self.job_stands(&job, json!({}));
            Ok(json!({ "job": job }).to_string())
        }

        fn start_held(
            &self,
            app: &str,
            op: &str,
            input: &str,
            push: std::sync::Arc<dyn Push>,
        ) -> Result<String, String> {
            *self.record.borrow_mut() = Some(push);
            self.start(app, op, input)
        }

        fn stream(
            &self,
            _app: &str,
            _op: &str,
            _input: &str,
            _push: std::sync::Arc<dyn Push>,
        ) -> Result<String, String> {
            Err("this device shows no moving picture".to_owned())
        }

        fn status(&self, job: &str) -> Result<String, String> {
            self.jobs
                .borrow()
                .get(job)
                .map(Json::to_string)
                .ok_or_else(|| format!("no job {job}"))
        }

        fn cancel(&self, job: &str) -> bool {
            self.cancelled.borrow_mut().push(job.to_owned());
            true
        }
    }

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.content.borrow().get(hash).cloned()
        }

        fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
            Ok(self.file.borrow().clone().into_iter().collect())
        }

        fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
            Err("this test writes no files out".to_owned())
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            // The content's own address: the same bytes are one file, as a content store keeps
            // them.
            let hash = format!("h{}", bytes.len());
            self.content
                .borrow_mut()
                .insert(hash.clone(), bytes.to_vec());
            Ok(FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }
    }

    struct Browser {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        page: Page,
        files: Files,
        device: Device,
        print_centre: App,
        opened: ResourceRef,
    }

    impl Browser {
        fn new() -> Self {
            Self::built(true)
        }

        /// A Browser nobody vouched for: a call it makes into another app's action asks the
        /// person first, as one from an app the workspace has not trusted does.
        fn untrusted() -> Self {
            Self::built(false)
        }

        fn built(trusted: bool) -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let page = Page::default();
            rt.set_page(page.clone());
            let files = Files::default();
            rt.set_device_files(files.clone());
            let device = Device::default();
            rt.set_blobs(device.clone());
            rt.set_engine(device.clone());
            rt.set_clock(|| "2026-11-24T09:00:00Z".to_owned());
            // The Browser shows where a download stands by asking Files, whose own action knows
            // the device's files, and prints a page by asking Print Centre, which has the
            // printers.
            let files_app = App::compile(FILES_SOURCE, &[]);
            files_app.load(&rt);
            let print_centre = App::compile(PRINT_CENTRE_SOURCE, &[]);
            print_centre.load(&rt);
            let app = App::compile(
                SOURCE,
                &[files_app.meta().clone(), print_centre.meta().clone()],
            );
            let loaded = app.load(&rt);
            if trusted {
                authority.trust(&loaded);
            }
            let root = rt.create_root("Browser").unwrap();
            let made = Browser::call(&app, &rt, "new_page", &[("b", Value::resource(&root))])
                .expect("the browser opens a page");
            let opened = returned_ref(made);
            Self {
                rt,
                app,
                root,
                page,
                files,
                device,
                print_centre,
                opened,
            }
        }

        /// The file the person picks when a control asks: `name`'s bytes, in this device's
        /// content store under `hash`.
        fn chooses(&self, name: &str, hash: &str) {
            *self.device.file.borrow_mut() =
                Some(FileValue::new(name, "application/octet-stream", 2048, hash));
        }

        fn call(
            app: &App,
            rt: &Runtime,
            action: &str,
            given: &[(&str, Value)],
        ) -> Result<InvokeOutcome, tilolang_runtime::RuntimeError> {
            let id = app.action(action);
            let args = app.args(rt, action, given);
            rt.invoke_action(&me(), (id.as_str(), args))
        }

        fn run(&self, action: &str, given: &[(&str, Value)]) -> InvokeOutcome {
            Self::call(&self.app, &self.rt, action, given)
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
        }

        fn page(&self, action: &str, given: &[(&str, Value)]) {
            let mut all = vec![("p", Value::resource(&self.opened))];
            all.extend_from_slice(given);
            self.run(action, &all);
        }

        fn view(&self, name: &str, args: Vec<Value>) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(&me(), &self.app.view(name), args)
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }

        fn page_view(&self) -> ViewInstance {
            self.view("page_view", vec![Value::resource(&self.opened)])
        }

        fn read(&self, at: &ResourceRef, field: &str) -> Value {
            self.rt.read(at, field).unwrap()
        }

        fn text(&self, at: &ResourceRef, field: &str) -> String {
            self.read(at, field)
                .as_text()
                .unwrap_or_else(|| panic!("{field} is text"))
                .to_owned()
        }
    }

    fn button_key(view: &ViewInstance, label: &str) -> String {
        of_kind(view.tree(), "ui.Button")
            .into_iter()
            .find(|b| matches!(b.props.get("label"), Some(Value::Text(l)) if l == label))
            .unwrap_or_else(|| panic!("no button {label} in\n{}", dump(view.tree())))
            .key
            .clone()
    }

    /// The key of the window's own plain control named `label`, not the action of the same name
    /// the app publishes into its window's action menu: a view's control says it is plain.
    fn plain_button(view: &ViewInstance, label: &str) -> Option<String> {
        of_kind(view.tree(), "ui.Button")
            .into_iter()
            .find(|b| {
                b.props.get("label") == Some(&Value::text(label))
                    && b.props.get("role") == Some(&Value::text("plain"))
            })
            .map(|b| b.key.clone())
    }

    fn row_secondary(view: &ViewInstance, title: &str) -> String {
        let row = of_kind(view.tree(), "ui.ListRow")
            .into_iter()
            .find(|r| r.props.get("title") == Some(&Value::text(title)))
            .unwrap_or_else(|| panic!("no row {title} in\n{}", dump(view.tree())));
        match row.props.get("secondary") {
            Some(Value::Text(text)) => text.clone(),
            other => panic!("{title}'s secondary is {other:?}"),
        }
    }

    /// Every line of text the window shows, in view order.
    fn view_texts(view: &ViewInstance) -> Vec<String> {
        of_kind(view.tree(), "ui.Text")
            .into_iter()
            .filter_map(|node| match node.props.get("value") {
                Some(Value::Text(text)) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn text_inputs(view: &ViewInstance) -> Vec<String> {
        of_kind(view.tree(), "ui.TextInput")
            .into_iter()
            .map(|node| node.key.clone())
            .collect()
    }

    #[test]
    fn the_window_search_finds_in_the_open_page_and_clearing_it_takes_the_find_away() {
        let b = Browser::new();
        let view = b.page_view();
        let shell = of_kind(view.tree(), "ui.NavShell")[0].key.clone();
        view.fire_with(&shell, "search", Some(&Value::text("harbour")))
            .unwrap();
        assert!(
            b.page.ops().iter().any(|op| op == "find_in_page"),
            "the words are found in the page: {:?}",
            b.page.ops()
        );
        assert_eq!(b.page.last()["text"], Json::from("harbour"));
        assert_eq!(
            b.text(&b.opened, "find_text"),
            "harbour",
            "the words are kept for the next and previous match"
        );

        view.fire_with(&shell, "search", Some(&Value::text("")))
            .unwrap();
        assert!(
            b.page.ops().iter().any(|op| op == "stop_finding"),
            "clearing the search leaves the page as it was: {:?}",
            b.page.ops()
        );
        assert_eq!(b.text(&b.opened, "find_text"), "");
    }

    /// A download the person saved in a page is the Browser's own job: the call names the
    /// handle the window keeps the request under (never the site's cookies), the row reads how
    /// far the job has come, and Cancel and Cancel every download stop the job.
    #[test]
    fn a_saved_download_is_a_job_the_row_follows_and_cancel_stops() {
        let b = Browser::new();
        b.page(
            "download",
            &[
                ("url", Value::text("https://example.org/report.pdf")),
                ("name", Value::text("report.pdf")),
                ("request", Value::text("0f3c")),
            ],
        );
        let started = b.device.started.borrow().clone();
        assert_eq!(
            started,
            [(
                "download".to_owned(),
                json!({"url": "https://example.org/report.pdf", "name": "report.pdf",
                    "request": "0f3c"})
            )],
            "the engine is asked with the handle alone"
        );
        assert!(
            b.page.ops().is_empty(),
            "the page's engine fetches nothing: {:?}",
            b.page.ops()
        );

        b.device
            .job_stands("browser-1", json!({"done": 2048, "total": 8192}));
        let view = b.page_view();
        assert_eq!(row_secondary(&view, "report.pdf"), "2.0 KB of 8.1 KB");
        let bar = of_kind(view.tree(), "ui.ProgressBar");
        assert_eq!(
            bar.len(),
            1,
            "a running download of known size shows how far it is"
        );

        let cancel = button_key(&view, "Cancel");
        view.fire(&cancel, "press").unwrap();
        assert_eq!(b.device.cancelled.borrow().as_slice(), ["browser-1"]);

        let all = button_key(&b.page_view(), "Cancel every download");
        b.page_view().fire(&all, "press").unwrap();
        assert_eq!(
            b.device.cancelled.borrow().as_slice(),
            ["browser-1", "browser-1"],
            "Cancel every download stops each download's job"
        );
    }

    /// A download that failed says why in the job's words. Retry asks the page what a request
    /// for the address carries now, so the retry is as signed in as the page; the window's call
    /// that follows starts a new job, which the download keeps.
    #[test]
    fn a_failed_download_says_why_and_a_retry_is_a_new_job_as_signed_in_as_the_page() {
        let b = Browser::new();
        b.page(
            "download",
            &[("url", Value::text("https://example.org/files/map.pdf"))],
        );
        b.device.job_stands(
            "browser-1",
            json!({"state": "failed", "error": "example.org no longer has this file (404)"}),
        );
        let view = b.page_view();
        assert_eq!(
            row_secondary(&view, "map.pdf"),
            "example.org no longer has this file (404)",
            "a typed address is named by its last part"
        );
        let retry = button_key(&view, "Retry");
        view.fire(&retry, "press").unwrap();
        assert_eq!(b.page.ops(), ["download_again"], "the page is asked again");
        assert_eq!(b.page.last()["url"], "https://example.org/files/map.pdf");
        assert_eq!(b.page.last()["job"], "browser-1");
        assert_eq!(
            b.device.started.borrow().len(),
            1,
            "nothing starts before the page answers"
        );

        // The window's call once the page answered: the job it ended as, and a fresh handle.
        let again = [
            ("url", Value::text("https://example.org/files/map.pdf")),
            ("request", Value::text("5e6f")),
            ("again", Value::text("browser-1")),
        ];
        b.page("download", &again);
        let started = b.device.started.borrow().clone();
        assert_eq!(started.len(), 2, "a retry starts the download again");
        assert_eq!(started[1].1["request"], "5e6f", "signed in, as the page is");
        let downloads = b.rt.project(&b.read(&b.root, "downloads"));
        assert_eq!(
            downloads["items"][0]["fields"]["job"].as_str(),
            Some("browser-2"),
            "the download keeps its new job"
        );
        assert_eq!(downloads["items"].as_array().map(Vec::len), Some(1));

        // A second answer for the same retry starts nothing more.
        b.page("download", &again);
        assert_eq!(b.device.started.borrow().len(), 2);
        let downloads = b.rt.project(&b.read(&b.root, "downloads"));
        assert_eq!(downloads["items"].as_array().map(Vec::len), Some(1));
    }

    /// A download an extension started is the page engine's to save, and the one list shows it
    /// with the Browser's own: how far it came, then Open and Show in Files where it was saved.
    #[test]
    fn an_extensions_download_is_in_the_one_list_with_its_progress() {
        let b = Browser::new();
        let seen = |state: &str, received: i64, path: &str| {
            vec![
                ("id", Value::Int(4)),
                ("state", Value::text(state)),
                ("name", Value::text("kept.bin")),
                ("url", Value::text("https://example.org/kept.bin")),
                ("path", Value::text(path)),
                ("received", Value::Int(received)),
                ("total", Value::Int(8192)),
                ("error", Value::text("")),
            ]
        };
        b.page("note_extension_download", &seen("progress", 2048, ""));
        let view = b.page_view();
        assert_eq!(row_secondary(&view, "kept.bin"), "2.0 KB of 8.1 KB");
        assert_eq!(of_kind(view.tree(), "ui.ProgressBar").len(), 1);
        assert!(
            plain_button(&view, "Cancel").is_none(),
            "the extension that started it is the one that stops it"
        );

        b.page(
            "note_extension_download",
            &seen("complete", 8192, "/home/ada/Downloads/kept.bin"),
        );
        let view = b.page_view();
        assert_eq!(
            row_secondary(&view, "kept.bin"),
            "Saved to /home/ada/Downloads/kept.bin"
        );
        let downloads = b.rt.project(&b.read(&b.root, "downloads"));
        assert_eq!(
            downloads["items"].as_array().map(Vec::len),
            Some(1),
            "one download, however often it moved"
        );
        let open = button_key(&view, "Open");
        view.fire(&open, "press").unwrap();
        assert_eq!(
            b.files.opened.borrow().as_slice(),
            ["/home/ada/Downloads/kept.bin"]
        );
        assert!(plain_button(&view, "Show in Files").is_some());
        assert!(
            b.device.started.borrow().is_empty(),
            "the Browser fetches nothing of it"
        );
    }

    /// A finished download offers Open and Show in Files, at the path its job saved it to.
    #[test]
    fn a_finished_download_opens_where_its_job_saved_it() {
        let b = Browser::new();
        b.page(
            "download",
            &[
                ("url", Value::text("https://example.org/r?id=3")),
                ("name", Value::text("report.pdf")),
            ],
        );
        b.device.job_stands(
            "browser-1",
            json!({"state": "done", "done": 8192, "total": 8192,
                "path": "/home/ada/Downloads/report 2.pdf", "name": "report 2.pdf"}),
        );
        let view = b.page_view();
        assert_eq!(
            row_secondary(&view, "report 2.pdf"),
            "Saved to /home/ada/Downloads/report 2.pdf"
        );
        assert!(
            plain_button(&view, "Cancel").is_none(),
            "a finished download has nothing to stop"
        );
        let open = button_key(&view, "Open");
        view.fire(&open, "press").unwrap();
        assert_eq!(
            b.files.opened.borrow().as_slice(),
            ["/home/ada/Downloads/report 2.pdf"]
        );
        assert!(plain_button(&view, "Show in Files").is_some());
    }

    /// A download is the Browser's, not its page's: another window shows it, still going.
    #[test]
    fn a_download_outlives_the_page_it_came_from() {
        let b = Browser::new();
        b.page(
            "download",
            &[
                ("url", Value::text("https://example.org/big.iso")),
                ("name", Value::text("big.iso")),
                ("request", Value::text("77aa")),
            ],
        );
        b.device
            .job_stands("browser-1", json!({"done": 1_000_000, "total": 0}));
        let made = Browser::call(
            &b.app,
            &b.rt,
            "new_page",
            &[("b", Value::resource(&b.root))],
        )
        .expect("another window opens");
        let other = b.view("page_view", vec![Value::resource(&returned_ref(made))]);
        assert_eq!(row_secondary(&other, "big.iso"), "1.0 MB downloaded");
        assert!(
            plain_button(&other, "Cancel").is_some(),
            "the other window can stop it"
        );
    }

    #[test]
    fn a_whole_form_is_answered_in_one_ask() {
        let b = Browser::new();
        let answers = Value::Map(vec![
            (Value::text("name"), Value::text("Ada Lovelace")),
            (Value::text("city"), Value::text("London")),
        ]);
        b.page(
            "fill_page_form",
            &[
                ("form", Value::text("checkout-1a2b")),
                ("answers", answers),
                ("submit", Value::Bool(true)),
            ],
        );
        assert_eq!(b.page.ops(), ["fill_form"]);
        let ask = b.page.last();
        assert_eq!(ask["form"].as_str(), Some("checkout-1a2b"));
        assert_eq!(ask["answers"]["name"].as_str(), Some("Ada Lovelace"));
        assert_eq!(ask["answers"]["city"].as_str(), Some("London"));
        assert_eq!(ask["submit"].as_bool(), Some(true));
    }

    #[test]
    fn a_saved_address_answers_the_questions_a_form_asks_by_their_names() {
        let b = Browser::new();
        b.run(
            "remember_form",
            &[
                ("b", Value::resource(&b.root)),
                ("origin", Value::text("https://shop.example.org")),
                ("signature", Value::text("checkout-1a2b")),
                ("label", Value::text("Delivery address")),
                ("page", Value::text("/checkout")),
                (
                    "questions",
                    Value::List(vec![
                        Value::text("full_name\ttext\tFull name\ttrue\t"),
                        Value::text("city\ttext\tCity\ttrue\t"),
                        Value::text("nickname\ttext\tNickname\tfalse\t"),
                    ]),
                ),
                ("whole", Value::Bool(true)),
            ],
        );
        let address = returned_ref(b.run("add_address", &[("b", Value::resource(&b.root))]));
        let view = b.view("address_page", vec![Value::resource(&address)]);
        let inputs = text_inputs(&view);
        for (at, text) in inputs.iter().zip([
            "Home",
            "Ada Lovelace",
            "Ada",
            "Lovelace",
            "12 Analytical Way",
            "London",
            "London",
            "N1 9AB",
            "United Kingdom",
        ]) {
            view.edit(at, &Value::text(text)).unwrap();
        }
        assert_eq!(b.text(&address, "city"), "London");

        b.page(
            "fill_address",
            &[
                ("form", Value::text("checkout-1a2b")),
                ("a", Value::resource(&address)),
            ],
        );
        assert_eq!(b.page.ops(), ["fill_form"]);
        let ask = b.page.last();
        assert_eq!(ask["form"].as_str(), Some("checkout-1a2b"));
        assert_eq!(ask["answers"]["full_name"].as_str(), Some("Ada Lovelace"));
        assert_eq!(ask["answers"]["city"].as_str(), Some("London"));
        assert!(
            ask["answers"].get("nickname").is_none(),
            "a question an address answers nothing for is left alone: {ask}"
        );
    }

    /// A file input is answered with the paths a person or an agent chose there, through the
    /// page's own validation of what it accepts.
    #[test]
    fn a_file_input_is_filled_with_the_paths_it_was_given() {
        let b = Browser::new();
        b.page(
            "fill_page_files",
            &[
                ("element", Value::text("d1")),
                (
                    "paths",
                    Value::List(vec![
                        Value::text("/home/ada/picture.png"),
                        Value::text("/home/ada/second.png"),
                    ]),
                ),
            ],
        );
        assert_eq!(b.page.ops(), ["fill_files"]);
        let ask = b.page.last();
        assert_eq!(ask["element"].as_str(), Some("d1"));
        let paths: Vec<&str> = ask["paths"]
            .as_array()
            .expect("the paths are an array")
            .iter()
            .filter_map(Json::as_str)
            .collect();
        assert_eq!(paths, ["/home/ada/picture.png", "/home/ada/second.png"]);
    }

    /// The page's sound and its own back and forward pages are read into its record, and the
    /// person's mute and history asks reach the engine.
    #[test]
    fn a_pages_sound_and_its_own_history_are_read_and_asked_for() {
        let b = Browser::new();
        b.page.answer_asks(json!({
            "muted": true,
            "back": [{"id": 2, "url": "https://example.org/earlier", "title": "Earlier"}],
            "forward": [{"id": 3, "url": "https://example.org/later", "title": ""}],
        }));
        b.page("refresh_page", &[]);
        assert!(
            matches!(b.read(&b.opened, "muted"), Value::Bool(true)),
            "the page's sound is muted"
        );
        let back = b.rt.project(&b.read(&b.opened, "back"));
        assert_eq!(
            back["items"][0]["fields"]["url"].as_str(),
            Some("https://example.org/earlier")
        );
        assert_eq!(
            back["items"][0]["fields"]["title"].as_str(),
            Some("Earlier")
        );
        let after = b.rt.project(&b.read(&b.opened, "forward"));
        assert_eq!(
            after["items"][0]["fields"]["title"].as_str(),
            Some("https://example.org/later"),
            "a page the engine has no title for is named by its address"
        );

        b.page("mute", &[("muted", Value::Bool(false))]);
        assert_eq!(b.page.ops().last().map(String::as_str), Some("mute"));
        let ask = b.page.last();
        assert_eq!(ask["muted"].as_bool(), Some(false), "the mute ask: {ask}");

        b.page("read_back_history", &[]);
        assert_eq!(b.page.ops().last().map(String::as_str), Some("history"));
        assert_eq!(b.page.last()["direction"].as_str(), Some("back"));

        b.page("read_forward_history", &[]);
        assert_eq!(b.page.last()["direction"].as_str(), Some("forward"));

        b.page("go_history_entry", &[("entry", Value::Int(2))]);
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("history_entry")
        );
        assert_eq!(b.page.last()["entry"].as_i64(), Some(2));
    }

    /// A page that goes fullscreen shows alone with the way out over it, and pressing the way
    /// out asks the engine to leave: the page's record leaves fullscreen, and its window shows
    /// its own controls again.
    #[test]
    fn a_page_in_fullscreen_shows_the_way_out_and_leaving_asks_the_engine() {
        let b = Browser::new();
        b.page.answer_asks(json!({"fullscreen": false}));
        b.page("refresh_page", &[]);
        let window = b.page_view();
        assert!(
            of_kind(window.tree(), "ui.Fullscreen").is_empty(),
            "at rest the page shows its own controls:\n{}",
            dump(window.tree())
        );

        b.page.answer_asks(json!({"fullscreen": true}));
        b.page("refresh_page", &[]);
        assert!(
            matches!(b.read(&b.opened, "fullscreen"), Value::Bool(true)),
            "the page fills the screen"
        );
        let window = b.page_view();
        let presented = of_kind(window.tree(), "ui.Fullscreen")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("the page is presented:\n{}", dump(window.tree())));
        assert!(
            matches!(presented.props.get("showing"), Some(Value::Bool(true))),
            "it presents the page: {:?}",
            presented.props
        );
        assert!(
            presented.children.len() == 1 && presented.children[0].kind == "ui.WebView",
            "the page alone is presented:\n{}",
            dump(window.tree())
        );

        window.fire(&presented.key, "exit").unwrap();
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("exit_fullscreen"),
            "the way out asks the engine to leave"
        );
        assert!(
            matches!(b.read(&b.opened, "fullscreen"), Value::Bool(false)),
            "the record leaves fullscreen"
        );
        // The engine's own report of itself agrees, so what the window shows and what the page
        // says stay one thing.
        b.page("refresh_page", &[]);
        let window = b.page_view();
        assert!(
            of_kind(window.tree(), "ui.Fullscreen").is_empty(),
            "the page shows its own controls again:\n{}",
            dump(window.tree())
        );
    }

    /// What a site's connection and permissions are is read into the page, and the person's
    /// clearing, blocking and asking again reach the engine for this site and this document.
    #[test]
    fn a_site_is_read_and_its_data_and_permissions_are_acted_on() {
        let b = Browser::new();
        b.page(
            "went_to",
            &[("url", Value::text("https://example.org/private"))],
        );
        b.page.answer_asks(json!({
            "site": {
                "origin": "https://example.org",
                "generation": 4,
                "blocked": 7,
                "protected": true,
                "security": "secure",
                "certificate": 0,
                "issuer": "Example CA",
                "subject": "example.org",
                "permissions": [{"kind": "camera", "state": "ask"}],
                "can_reset": true,
            },
        }));
        b.page("refresh_page", &[]);
        let view = b.page_view();
        assert_eq!(
            row_secondary(&view, "https://example.org"),
            "Secure connection"
        );
        assert_eq!(row_secondary(&view, "Requests blocked"), "7");
        assert_eq!(row_secondary(&view, "Certificate"), "Example CA");
        assert!(
            view_texts(&view).contains(&"Camera: asks first".to_owned()),
            "a permission reads as words:\n{}",
            dump(view.tree())
        );

        b.page("read_site", &[]);
        assert_eq!(b.page.ops().last().map(String::as_str), Some("site_status"));
        assert_eq!(
            b.page.last()["origin"].as_str(),
            Some("https://example.org")
        );

        b.page("clear_site_data", &[]);
        assert_eq!(b.page.ops().last().map(String::as_str), Some("site_action"));
        assert_eq!(b.page.last()["doing"].as_str(), Some("clear"));
        assert_eq!(b.page.last()["generation"].as_i64(), Some(4));

        b.page("block_site_permissions", &[]);
        assert_eq!(b.page.last()["doing"].as_str(), Some("revoke"));

        b.page("ask_site_permissions_again", &[]);
        assert_eq!(b.page.last()["doing"].as_str(), Some("reset"));

        b.run(
            "set_site_protection",
            &[
                ("b", Value::resource(&b.root)),
                ("origin", Value::text("https://example.org")),
                ("enabled", Value::Bool(false)),
            ],
        );
        b.page("sync_site_exceptions", &[]);
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("site_exceptions")
        );
        let ask = b.page.last();
        let origins: Vec<&str> = ask["origins"]
            .as_array()
            .expect("the origins are an array")
            .iter()
            .filter_map(Json::as_str)
            .collect();
        assert_eq!(origins, ["https://example.org"]);
    }

    /// A name of the engine's the Browser has no words for reads as a plain fact: a site's
    /// connection, a login form and a permission never show their token in a window.
    #[test]
    fn a_name_the_engine_grows_reads_as_words_and_never_as_its_token() {
        let b = Browser::new();
        b.page(
            "went_to",
            &[("url", Value::text("https://example.org/private"))],
        );
        b.page.answer_asks(json!({
            "site": {
                "origin": "https://example.org",
                "generation": 1,
                "blocked": 0,
                "protected": true,
                "security": "quantum",
                "certificate": 0,
                "issuer": "",
                "subject": "example.org",
                "permissions": [{"kind": "usb", "state": "allowed"}],
                "can_reset": false,
            },
        }));
        b.page("refresh_page", &[]);
        let view = b.page_view();
        assert_eq!(
            row_secondary(&view, "https://example.org"),
            "Unknown connection"
        );
        assert!(
            view_texts(&view).contains(&"Something else: allowed".to_owned()),
            "a permission the Browser knows no word for says so:\n{}",
            dump(view.tree())
        );

        b.page(
            "remember_credential",
            &[
                ("did", Value::text("discover")),
                ("success", Value::Bool(true)),
                ("message", Value::text("")),
                (
                    "forms",
                    Value::text(r#"[{"form":"1","username":"","kind":"sso"}]"#),
                ),
                ("accounts", Value::text("[]")),
                ("capture", Value::text("")),
                ("username", Value::text("")),
                ("late", Value::Bool(false)),
            ],
        );
        let view = b.page_view();
        assert_eq!(
            row_secondary(&view, "Form"),
            "Form",
            "a form kind with no words is a form:\n{}",
            dump(view.tree())
        );
    }

    /// The person's history stays within its bound: the oldest visits go and the newest stay.
    #[test]
    fn the_history_stays_within_its_bound() {
        let b = Browser::new();
        for (at, name) in [
            ("2026-11-24T09:00:00Z", "a"),
            ("2026-11-24T10:00:00Z", "b"),
            ("2026-11-24T11:00:00Z", "c"),
        ] {
            b.run(
                "visit",
                &[
                    ("b", Value::resource(&b.root)),
                    ("url", Value::text(format!("https://example.org/{name}"))),
                    ("title", Value::text(name)),
                    ("at", Value::text(at)),
                ],
            );
        }
        b.run(
            "trim_history",
            &[("b", Value::resource(&b.root)), ("most", Value::Int(2))],
        );
        let visits = b.rt.project(&b.read(&b.root, "visits"));
        let urls: Vec<&str> = visits["items"]
            .as_array()
            .expect("visits are a list")
            .iter()
            .filter_map(|visit| visit["fields"]["url"].as_str())
            .collect();
        assert_eq!(urls.len(), 2, "two visits stay: {urls:?}");
        assert!(
            !urls.contains(&"https://example.org/a"),
            "the oldest goes: {urls:?}"
        );
        assert!(urls.contains(&"https://example.org/c"), "the newest stays");
    }

    #[test]
    fn a_sign_in_turned_down_sends_no_credentials() {
        let b = Browser::new();
        b.page(
            "cancel_sign_in",
            &[
                ("id", Value::Int(7)),
                ("asker", Value::text("proxy.example")),
            ],
        );
        assert_eq!(b.page.ops(), ["answer_sign_in"]);
        let ask = b.page.last();
        assert_eq!(ask["id"].as_i64(), Some(7));
        assert_eq!(ask["asker"].as_str(), Some("proxy.example"));
        assert!(ask["username"].is_null(), "no user name is sent: {ask}");
        assert!(ask["password"].is_null(), "no password is sent: {ask}");
    }

    /// A private window settles the page's privacy before its window reads anything, and what it
    /// opens joins no history the next window reads.
    #[test]
    fn a_private_window_keeps_nothing_of_what_it_opens() {
        let b = Browser::new();
        let made = returned_ref(b.run("new_private_page", &[("b", Value::resource(&b.root))]));
        assert_eq!(b.read(&made, "private"), Value::Bool(true));

        b.run("opened_page", &[("p", Value::resource(&made))]);
        assert_eq!(
            b.page.ops(),
            ["private", "asks"],
            "the privacy is settled before the page is read"
        );

        b.run(
            "went_to",
            &[
                ("p", Value::resource(&made)),
                ("url", Value::text("https://example.org/private")),
            ],
        );
        let visits = b.rt.project(&b.read(&b.root, "visits"));
        assert_eq!(
            visits["items"].as_array().map(Vec::len),
            Some(0),
            "a private page's addresses join no history: {visits}"
        );

        let view = b.view("page_view", vec![Value::resource(&made)]);
        assert!(
            !of_kind(view.tree(), "ui.Banner").is_empty(),
            "the window says it is private:\n{}",
            dump(view.tree())
        );

        // An ordinary page says nothing on the same window.
        let open = returned_ref(b.run("new_page", &[("b", Value::resource(&b.root))]));
        let view = b.view("page_view", vec![Value::resource(&open)]);
        assert!(
            of_kind(view.tree(), "ui.Banner").is_empty(),
            "an ordinary page shows no private notice"
        );
    }

    /// The extensions' toolbar buttons are read from the page, a press reaches the engine with
    /// the extension it stands for, and one the extension dimmed takes no press.
    #[test]
    fn the_extensions_toolbar_buttons_are_read_and_pressed_through_the_page() {
        let b = Browser::new();
        b.page(
            "remember_extensions",
            &[
                (
                    "installed",
                    Value::text(
                        r#"[{"id":"abcdefghijklmnopabcdefghijklmnop","name":"Reader","version":"1.0","manifest_version":3,"description":"","permissions":[],"enabled":true}]"#,
                    ),
                ),
                ("message", Value::text("")),
            ],
        );
        b.page.answer_asks(json!({
            "extension_buttons": [
                {"id":"abcdefghijklmnopabcdefghijklmnop","title":"","badge":"3","enabled":true,"popup":false},
                {"id":"ponmlkjihgfedcbaponmlkjihgfedcba","title":"Dimmer","badge":"","enabled":false,"popup":false},
            ],
        }));
        b.page("refresh_page", &[]);
        let view = b.page_view();
        // A button with no title of its own is named by its extension, with its badge.
        view.fire(&button_key(&view, "Reader · 3"), "press")
            .unwrap();
        assert_eq!(
            b.page.last()["op"].as_str(),
            Some("extension_click"),
            "the engine is asked to press the extension"
        );
        assert_eq!(
            b.page.last()["extension"].as_str(),
            Some("abcdefghijklmnopabcdefghijklmnop")
        );

        // The dimmed one does not answer a press, and its refusal says why.
        let Value::List(items) = b.read(&b.opened, "buttons") else {
            panic!("the buttons are a list");
        };
        let Value::Ref(dimmer) = items.get(1).expect("the dimmed button") else {
            panic!("a button is a resource");
        };
        let refused = Browser::call(
            &b.app,
            &b.rt,
            "click_extension",
            &[
                ("p", Value::resource(&b.opened)),
                ("b", Value::resource(&dimmer.target)),
            ],
        );
        assert!(refused.is_err(), "a press on a dimmed button is refused");
    }

    /// A notification an extension showed is drawn on the page with its buttons, and what the
    /// person presses reaches the engine with the extension, its notification and the button.
    #[test]
    fn an_extensions_notification_is_shown_and_answered_on_the_page() {
        let b = Browser::new();
        b.page.answer_asks(json!({
            "extension_notices": [{
                "id": "abcdefghijklmnopabcdefghijklmnop",
                "notification": "n1",
                "title": "Saved for later",
                "message": "Three pages kept",
                "buttons": ["Open", "Later"],
            }],
        }));
        b.page("refresh_page", &[]);
        let view = b.page_view();
        assert!(
            of_kind(view.tree(), "ui.Banner")
                .iter()
                .any(|banner| banner.props.get("title") == Some(&Value::text("Saved for later"))),
            "the notification is a banner on the page:\n{}",
            dump(view.tree())
        );

        view.fire(&button_key(&view, "Later"), "press").unwrap();
        assert_eq!(
            b.page.last()["op"].as_str(),
            Some("extension_notice_chosen"),
            "the engine hears which button was pressed"
        );
        assert_eq!(
            b.page.last()["extension"].as_str(),
            Some("abcdefghijklmnopabcdefghijklmnop")
        );
        assert_eq!(b.page.last()["notification"].as_str(), Some("n1"));
        assert_eq!(
            b.page.last()["button"].as_str(),
            Some("1"),
            "the second button is the second one"
        );

        let view = b.page_view();
        view.fire(&button_key(&view, "Dismiss"), "press").unwrap();
        assert_eq!(
            b.page.last()["op"].as_str(),
            Some("extension_notice_chosen")
        );
        assert!(
            b.page.last()["button"].is_null(),
            "taking it away presses no button: {}",
            b.page.last()
        );
    }

    /// A saved download is shown in Files by calling Files' own action: the Browser names the
    /// place and Files, the app that knows the device's files, shows it. The Browser never reads
    /// a folder itself.
    #[test]
    fn a_saved_download_is_shown_in_files_through_its_own_action() {
        let authority = TestAuthority::new();
        let rt = Runtime::new(AmStore::new(), authority).unwrap();
        rt.set_clock(|| "2026-11-24T09:00:00Z".to_owned());
        let files = App::compile(FILES_SOURCE, &[]);
        files.load(&rt);
        let print_centre = App::compile(PRINT_CENTRE_SOURCE, &[]);
        print_centre.load(&rt);
        // The Browser is not trusted with another app's actions, so its call to Files asks.
        let app = App::compile(SOURCE, &[files.meta().clone(), print_centre.meta().clone()]);
        app.load(&rt);
        let files_root = rt.create_root("Files").expect("the Files root");
        let dispatch = Dispatch::new();
        rt.set_foreign_invoker(dispatch.clone());

        let out = Browser::call(
            &app,
            &rt,
            "reveal_download",
            &[("path", Value::text("/home/ada/Downloads/report.pdf"))],
        )
        .unwrap_or_else(|error| panic!("the download is shown: {error:?}"));
        let InvokeOutcome::Waits { run, .. } = out else {
            panic!("the call to Files asks first: {out:?}");
        };
        let (action, args) = dispatch.asked();
        assert_eq!(
            action,
            files.action("show"),
            "Files' own action, by name, not a second path of its own"
        );
        assert!(
            args.contains(&Value::text("/home/ada/Downloads/report.pdf")),
            "the call names the download: {args:?}"
        );

        let RunOutcome::Done(_) = dispatch.allow(&rt, &run) else {
            panic!("Files shows the place");
        };
        assert_eq!(
            rt.read(&files_root, "place").unwrap().as_text(),
            Some("/home/ada/Downloads/report.pdf"),
            "Files shows the download's place"
        );

        // Nothing to show for a download that was never saved.
        let refused = Browser::call(&app, &rt, "reveal_download", &[("path", Value::text(""))]);
        assert!(refused.is_err(), "a download with no path shows nothing");
    }

    /// A page is written as a PDF and handed to Print Centre through Print Centre's own action,
    /// as a file the workspace's content store holds; a print that failed says why in the
    /// person's words.
    #[test]
    fn a_printed_page_is_handed_to_print_centre_through_its_own_action() {
        let b = Browser::untrusted();
        let dispatch = Dispatch::new();
        b.rt.set_foreign_invoker(dispatch.clone());
        let print_root =
            b.rt.create_root("PrintCentre")
                .expect("the Print Centre root");

        // A page with an address of its own is what there is to print.
        b.run(
            "navigate",
            &[
                ("p", Value::resource(&b.opened)),
                ("url", Value::text("https://example.org/report")),
            ],
        );
        b.page.forget();

        // The page's own menu offers Print, which asks the engine for the page as a PDF.
        let view = b.page_view();
        let row = of_kind(view.tree(), "ui.MenuItem")
            .into_iter()
            .find(|row| row.props.get("label") == Some(&Value::text("Print")))
            .unwrap_or_else(|| panic!("the page's menu offers Print: {}", dump(view.tree())));
        view.fire(&row.key, "choose").expect("Print is chosen");
        assert_eq!(b.page.ops(), ["print"]);

        // The action itself asks for the same.
        let out = b.run("print_page", &[("p", Value::resource(&b.opened))]);
        assert!(
            matches!(out, InvokeOutcome::Done(_)),
            "the page is asked for its PDF: {out:?}"
        );
        assert_eq!(b.page.ops(), ["print", "print"]);

        // The engine wrote the PDF; the window hands it on to Print Centre, which is not an app
        // the Browser is trusted with the actions of, so its call asks.
        let out = Browser::call(
            &b.app,
            &b.rt,
            "note_print",
            &[
                ("p", Value::resource(&b.opened)),
                ("ok", Value::Bool(true)),
                (
                    "path",
                    Value::text("/home/ada/.local/share/tilo/chromium/print/Example page.pdf"),
                ),
                ("name", Value::text("Example page.pdf")),
                ("message", Value::text("")),
            ],
        )
        .expect("the PDF is handed over");
        let InvokeOutcome::Waits { run, .. } = out else {
            panic!("the call to Print Centre asks first: {out:?}");
        };
        let (action, args) = dispatch.asked();
        assert_eq!(
            action,
            b.print_centre.action("print_file"),
            "Print Centre's own action, by name, not a second path of its own"
        );
        let printed = args
            .iter()
            .find_map(|value| match value {
                Value::File(file) => Some(file.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the call hands Print Centre a file: {args:?}"));
        assert_eq!(printed.name, "Example page.pdf");
        assert_eq!(printed.media, "application/pdf");
        assert_eq!(
            b.device.read(&printed.hash).as_deref(),
            Some(PRINTED_PDF),
            "Print Centre is handed the PDF's own bytes"
        );

        let RunOutcome::Done(_) = dispatch.allow(&b.rt, &run) else {
            panic!("Print Centre takes the document");
        };
        assert_eq!(
            b.rt.read(&print_root, "printed").unwrap().as_text(),
            Some("Example page.pdf"),
            "Print Centre has the page as its document"
        );

        // A print the engine could not write says what happened, in the person's words.
        b.run(
            "note_print",
            &[
                ("p", Value::resource(&b.opened)),
                ("ok", Value::Bool(false)),
                ("path", Value::text("")),
                ("name", Value::text("")),
                (
                    "message",
                    Value::text("The page could not be written as a PDF"),
                ),
            ],
        );
        let view = b.page_view();
        assert!(
            of_kind(view.tree(), "ui.Banner")
                .iter()
                .any(|banner| banner.props.get("message")
                    == Some(&Value::text("The page could not be written as a PDF"))),
            "the failure shows: {}",
            dump(view.tree())
        );
    }

    /// The extensions this device keeps are read into the Browser's record, installed from the
    /// package the person names, turned on and off and removed through their rows, and the
    /// engine's own answer is shown in the person's words.
    #[test]
    fn extensions_are_read_installed_turned_on_and_removed_through_the_page() {
        let b = Browser::new();
        b.page("list_extensions", &[]);
        assert_eq!(b.page.ops(), ["extension_list"]);

        b.page(
            "remember_extensions",
            &[
                (
                    "installed",
                    Value::text(
                        r#"[{"id":"abcdefghijklmnopabcdefghijklmnop","name":"Reader","version":"1.2.3","manifest_version":3,"description":"Reads pages","permissions":["storage","tabs"],"enabled":true}]"#,
                    ),
                ),
                ("message", Value::text("")),
            ],
        );
        let view = b.page_view();
        assert_eq!(row_secondary(&view, "Reader"), "On (1.2.3)");

        b.page(
            "install_extension",
            &[(
                "file",
                Value::file(FileValue::new(
                    "reader.crx",
                    "application/octet-stream",
                    2048,
                    "d".repeat(64),
                )),
            )],
        );
        assert_eq!(
            b.page.last()["op"].as_str(),
            Some("extension_install"),
            "the engine is asked to install the package the person chose"
        );
        assert_eq!(
            b.page.last()["file"].as_str(),
            Some(format!("tilo-blob://{}", "d".repeat(64)).as_str()),
            "the package is named by the address of its bytes, never a host path"
        );

        // Turning it off names the extension and says which way, and the record shows it off
        // without waiting for the engine to answer.
        let view = b.page_view();
        view.fire(&button_key(&view, "Turn the extension off"), "press")
            .unwrap();
        assert_eq!(
            b.page.ops(),
            [
                "extension_list",
                "extension_install",
                "extension_enable",
                "extension_list"
            ],
            "turning it off reaches the engine and the record is read again"
        );
        let turn_off = &b.page.asked_ops()[2];
        assert_eq!(
            turn_off["extension"].as_str(),
            Some("abcdefghijklmnopabcdefghijklmnop")
        );
        assert_eq!(turn_off["enabled"].as_bool(), Some(false));
        assert_eq!(row_secondary(&b.page_view(), "Reader"), "Off (1.2.3)");

        let view = b.page_view();
        view.fire(&button_key(&view, "Remove"), "press").unwrap();
        assert_eq!(
            b.page.ops().into_iter().skip(4).collect::<Vec<_>>(),
            ["extension_remove", "extension_list"]
        );
        assert_eq!(
            b.page.asked_ops()[4]["extension"].as_str(),
            Some("abcdefghijklmnopabcdefghijklmnop")
        );

        // What the engine read of a package it could not install is shown as the person's words.
        b.page(
            "note_extension",
            &[
                ("ok", Value::Bool(false)),
                ("kind", Value::text("damaged")),
                ("message", Value::text("the package is damaged: signature")),
                ("id", Value::text("")),
            ],
        );
        assert_eq!(
            b.text(&b.root, "extension_notice"),
            "The package is damaged, or signed for another extension"
        );
        b.page(
            "note_extension",
            &[
                ("ok", Value::Bool(true)),
                ("kind", Value::text("")),
                ("message", Value::text("")),
                ("id", Value::text("abcdefghijklmnopabcdefghijklmnop")),
            ],
        );
        assert_eq!(b.text(&b.root, "extension_notice"), "");
    }

    /// The system's own chooser is how a package arrives: the person presses Install from a
    /// file, picks one, and the engine is handed the address of its bytes and asked again what
    /// is installed.
    #[test]
    fn an_extension_installs_from_the_package_the_person_picks() {
        let b = Browser::new();
        b.chooses("reader.crx", &"e".repeat(64));
        let view = b.page_view();
        view.fire(&button_key(&view, "Install from a file"), "press")
            .unwrap();
        assert_eq!(
            b.page.ops(),
            ["extension_install", "extension_list"],
            "the picked package reaches the engine and the record is read again"
        );
        assert_eq!(
            b.page.asked_ops()[0]["file"].as_str(),
            Some(format!("tilo-blob://{}", "e".repeat(64)).as_str()),
            "the engine is handed the address of the package's bytes"
        );

        // A chooser the person dismisses installs nothing.
        let empty = Browser::new();
        let view = empty.page_view();
        view.fire(&button_key(&view, "Install from a file"), "press")
            .unwrap();
        assert_eq!(
            empty.page.ops(),
            ["extension_list"],
            "nothing was picked, so nothing is installed"
        );
    }

    /// A Chrome Web Store item installs from its page's address, from a link pasted into the
    /// settings, or from its id; a page that is no store item offers no install.
    #[test]
    fn a_store_extension_installs_from_its_page_or_its_id() {
        const ITEM: &str = "abcdefghijklmnopabcdefghijklmnop";
        let address = format!("https://chromewebstore.google.com/detail/reader/{ITEM}");
        let b = Browser::new();
        b.page("went_to", &[("url", Value::text(address.as_str()))]);
        let view = b.page_view();
        view.fire(&button_key(&view, "Add to Browser"), "press")
            .unwrap();
        assert_eq!(
            b.page.ops(),
            ["extension_install_store", "extension_list"],
            "the store item reaches the engine, which fetches its package"
        );
        assert_eq!(
            b.page.asked_ops()[0]["item"].as_str(),
            Some(address.as_str())
        );

        // The id alone names the same item.
        b.page("install_store_extension", &[("said", Value::text(ITEM))]);
        assert_eq!(
            b.page.last()["op"].as_str(),
            Some("extension_install_store")
        );
        assert_eq!(b.page.last()["item"].as_str(), Some(ITEM));
        let refused = Browser::call(
            &b.app,
            &b.rt,
            "install_store_extension",
            &[
                ("p", Value::resource(&b.opened)),
                ("said", Value::text("  ")),
            ],
        );
        assert!(refused.is_err(), "no item named, nothing installed");

        // A look-alike host, and a page that is no store item, offer no install.
        for other in [
            "https://example.org/",
            format!("https://chromewebstore.google.com.evil.example/detail/reader/{ITEM}").as_str(),
        ] {
            b.page("went_to", &[("url", Value::text(other))]);
            let view = b.page_view();
            assert!(
                of_kind(view.tree(), "ui.Button")
                    .iter()
                    .all(|b| b.props.get("label") != Some(&Value::text("Add to Browser"))),
                "{other} offers no install"
            );
        }
    }

    /// Checking for updates asks the store and says what came of it: what was updated, that all
    /// is current, and which extension could not be checked.
    #[test]
    fn an_update_check_reports_what_it_installed() {
        const ITEM: &str = "abcdefghijklmnopabcdefghijklmnop";
        let installed = |version: &str| {
            Value::text(format!(
                r#"[{{"id":"{ITEM}","name":"Reader","version":"{version}","manifest_version":3,"description":"Reads pages","permissions":["storage"],"enabled":true}}]"#
            ))
        };
        let b = Browser::new();
        b.page(
            "remember_extensions",
            &[
                ("installed", installed("1.2.3")),
                ("message", Value::text("")),
            ],
        );
        b.page("update_extensions", &[]);
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("extension_update_check"),
            "the store is asked for newer versions"
        );

        // What the engine found: one extension updated, and the inventory it answered with.
        b.page(
            "note_extension_updates",
            &[
                ("checked", Value::Int(1)),
                (
                    "updated",
                    Value::text(format!(
                        r#"[{{"id":"{ITEM}","name":"Reader","from":"1.2.3","version":"1.4.0"}}]"#
                    )),
                ),
                ("failed", Value::text("")),
            ],
        );
        assert_eq!(
            b.text(&b.root, "extension_notice"),
            "“Reader” updated to 1.4.0"
        );
        b.page(
            "remember_extensions",
            &[
                ("installed", installed("1.4.0")),
                ("message", Value::text("")),
            ],
        );
        assert_eq!(
            row_secondary(&b.page_view(), "Reader"),
            "On (1.4.0)",
            "the record shows the version that was installed"
        );

        let report = |checked: i64, updated: &str, failed: &str| {
            b.page(
                "note_extension_updates",
                &[
                    ("checked", Value::Int(checked)),
                    ("updated", Value::text(updated)),
                    ("failed", Value::text(failed)),
                ],
            );
            b.text(&b.root, "extension_notice")
        };
        assert_eq!(report(1, "[]", ""), "The extensions are up to date");
        assert_eq!(
            report(0, "[]", ""),
            "No extension from the Chrome Web Store is installed"
        );
        assert_eq!(
            report(1, "[]", "Reader: the store could not be reached"),
            "The Chrome Web Store could not be checked: Reader: the store could not be reached"
        );
    }

    /// Turning an extension on reaches the running engine and shows at once: the record says so
    /// before the window has restarted, and the inventory that follows agrees.
    #[test]
    fn turning_an_extension_on_takes_effect_without_a_restart() {
        const ITEM: &str = "abcdefghijklmnopabcdefghijklmnop";
        let on = |enabled: bool| {
            Value::text(format!(
                r#"[{{"id":"{ITEM}","name":"Reader","version":"1.2.3","manifest_version":3,"description":"Reads pages","permissions":["storage"],"enabled":{enabled}}}]"#
            ))
        };
        let b = Browser::new();
        b.page(
            "remember_extensions",
            &[("installed", on(false)), ("message", Value::text(""))],
        );
        let view = b.page_view();
        assert_eq!(row_secondary(&view, "Reader"), "Off (1.2.3)");
        view.fire(&button_key(&view, "Turn the extension on"), "press")
            .unwrap();
        assert_eq!(
            b.page.ops(),
            ["extension_enable", "extension_list"],
            "the engine is told, and the record is read again"
        );
        assert_eq!(
            b.page.asked_ops()[0]["op"].as_str(),
            Some("extension_enable")
        );
        assert_eq!(b.page.asked_ops()[0]["extension"].as_str(), Some(ITEM));
        assert_eq!(b.page.asked_ops()[0]["enabled"].as_bool(), Some(true));
        assert_eq!(
            row_secondary(&b.page_view(), "Reader"),
            "On (1.2.3)",
            "the record shows it on with no restart"
        );
        b.page(
            "remember_extensions",
            &[("installed", on(true)), ("message", Value::text(""))],
        );
        assert_eq!(row_secondary(&b.page_view(), "Reader"), "On (1.2.3)");
    }

    /// The logins the engine keeps for a site are found, captured, saved, filled and forgotten
    /// through the engine's own credential store: the app names the site and the form, and the
    /// password never enters it.
    #[test]
    fn a_login_is_found_saved_filled_and_forgotten_through_the_page() {
        let b = Browser::new();
        b.page(
            "went_to",
            &[("url", Value::text("https://example.org/login"))],
        );
        a_login_is_discovered_and_saved(&b);
        the_vaults_logins_are_listed_and_filled(&b);
        a_login_that_cannot_be_saved_keeps_its_capture(&b);
    }

    /// The engine's `remember_credential` answer, naming what it did and what it holds.
    fn answered(did: &str, capture: &str, accounts: &str) -> Vec<(&'static str, Value)> {
        vec![
            ("did", Value::text(did)),
            ("success", Value::Bool(true)),
            ("message", Value::text("")),
            (
                "forms",
                Value::text(r#"[{"form":"1","username":"ada","kind":"current-password"}]"#),
            ),
            ("accounts", Value::text(accounts)),
            ("capture", Value::text(capture)),
            ("username", Value::text("ada")),
            ("late", Value::Bool(false)),
        ]
    }

    /// The page's login is found, captured and saved under the vault's name.
    fn a_login_is_discovered_and_saved(b: &Browser) {
        b.page("discover_logins", &[]);
        assert_eq!(b.page.ops(), ["credential"]);
        let ask = b.page.last();
        assert_eq!(ask["credential"].as_str(), Some("discover"));
        assert_eq!(ask["origin"].as_str(), Some("https://example.org"));
        assert!(
            ask.get("document").is_none(),
            "the window names the document the page is at: {ask}"
        );

        // What the engine's store answered reaches the page's record.
        b.page("remember_credential", &answered("discover", "", "[]"));
        let view = b.page_view();
        assert_eq!(
            row_secondary(&view, "ada"),
            "Sign-in form",
            "the form the engine found is on the page"
        );

        // The person takes the login from that form; the engine answers with a capture.
        view.fire(&button_key(&view, "Save this login"), "press")
            .unwrap();
        assert_eq!(b.page.last()["credential"].as_str(), Some("capture"));
        assert_eq!(b.page.last()["form"].as_str(), Some("1"));
        b.page("remember_credential", &answered("capture", "7", "[]"));

        // Saving names the capture, and the capture is spent once the engine has it.
        let view = b.page_view();
        view.fire(&button_key(&view, "Save the login"), "press")
            .unwrap();
        assert_eq!(b.page.last()["credential"].as_str(), Some("save"));
        assert_eq!(b.page.last()["capture"].as_str(), Some("7"));
        b.page("remember_credential", &answered("save", "", "[]"));
        assert_eq!(b.text(&b.opened, "capture"), "", "the capture is spent");
    }

    /// The vault's logins for the site are listed, one fills once allowed, and one is deleted.
    fn the_vaults_logins_are_listed_and_filled(b: &Browser) {
        b.page("list_saved_logins", &[]);
        assert_eq!(b.page.last()["credential"].as_str(), Some("list"));
        b.page(
            "remember_credential",
            &answered(
                "list",
                "",
                r#"[{"account":"vault.item:a","username":"ada@example.org","title":"example.org","own":true},
                    {"account":"vault.item:b","username":"ada.home","title":"Shop","own":false}]"#,
            ),
        );
        let view = b.page_view();
        assert_eq!(
            row_secondary(&view, "ada@example.org"),
            "Saved by Browser in your vault"
        );
        assert_eq!(
            row_secondary(&view, "ada.home"),
            "In your vault: fills once you allow it"
        );
        view.fire(&button_key(&view, "Fill"), "press").unwrap();
        assert_eq!(b.page.last()["credential"].as_str(), Some("fill"));
        assert_eq!(b.page.last()["form"].as_str(), Some("1"));
        assert_eq!(b.page.last()["account"].as_str(), Some("vault.item:a"));

        // A login the person has not allowed yet is asked for, and nothing is filled: the page's
        // record says why and keeps no secret.
        b.page(
            "remember_credential",
            &[
                ("did", Value::text("fill")),
                ("success", Value::Bool(false)),
                (
                    "message",
                    Value::text("Browser asked you first to use this login"),
                ),
                ("forms", Value::text("[]")),
                ("accounts", Value::text("[]")),
                ("capture", Value::text("")),
                ("username", Value::text("")),
                ("late", Value::Bool(false)),
            ],
        );
        assert_eq!(
            b.text(&b.opened, "login_notice"),
            "Browser asked you first to use this login",
            "the person is told the login waits for them"
        );

        view.fire(&button_key(&view, "Delete"), "press").unwrap();
        assert_eq!(b.page.last()["credential"].as_str(), Some("delete"));
        assert_eq!(b.page.last()["account"].as_str(), Some("vault.item:a"));
    }

    /// A login the engine could not save leaves the capture for another try and says why.
    fn a_login_that_cannot_be_saved_keeps_its_capture(b: &Browser) {
        b.page("remember_credential", &answered("capture", "9", "[]"));
        b.page(
            "remember_credential",
            &[
                ("did", Value::text("save")),
                ("success", Value::Bool(false)),
                ("message", Value::text("Capture expired; capture again")),
                ("forms", Value::text("[]")),
                ("accounts", Value::text("[]")),
                ("capture", Value::text("")),
                ("username", Value::text("")),
                ("late", Value::Bool(false)),
            ],
        );
        assert_eq!(
            b.text(&b.opened, "capture"),
            "9",
            "the capture is kept for another try"
        );
        assert_eq!(
            b.text(&b.opened, "login_notice"),
            "Capture expired; capture again",
            "the engine's own words are shown"
        );
    }

    /// Another browser's bookmarks come in with their folders: a flat entry's folder and a
    /// tree's nested folder both reach the library as the path the bookmark stands under.
    #[test]
    fn imported_bookmarks_keep_their_folders_and_their_tree() {
        let b = Browser::new();
        b.run(
            "import_bookmarks",
            &[
                ("b", Value::resource(&b.root)),
                (
                    "bundle",
                    Value::text(
                        r#"{"bookmarks":[
                            {"url":"https://example.org/a","title":"A","folder":"Work"},
                            {"title":"Reading","children":[
                                {"url":"https://example.org/b","title":"B"}
                            ]}
                        ]}"#,
                    ),
                ),
            ],
        );
        let made = b.rt.project(&b.read(&b.root, "bookmarks"));
        let items = made["items"].as_array().expect("the bookmarks are a list");
        let folder = |url: &str| {
            items
                .iter()
                .find(|item| item["fields"]["url"].as_str() == Some(url))
                .unwrap_or_else(|| panic!("no bookmark for {url}: {made}"))["fields"]["folder"]
                .as_str()
                .map(str::to_owned)
        };
        assert_eq!(folder("https://example.org/a").as_deref(), Some("Work"));
        assert_eq!(folder("https://example.org/b").as_deref(), Some("Reading"));
    }

    /// A saved download opens in the app that opens its kind, through the system's one open,
    /// and one that was never saved is refused before anything is opened.
    /// The engine's own developer tools are asked for by the page's action.
    /// A page the page opened in a window of its own becomes a page of the browser.
    #[test]
    fn a_popup_a_page_opened_becomes_a_page_of_the_browser() {
        let b = Browser::new();
        b.page(
            "open_from_page",
            &[
                ("url", Value::text("https://example.org/sign-in")),
                ("popup", Value::Bool(true)),
            ],
        );
        let pages = b.rt.project(&b.read(&b.root, "pages"));
        let urls: Vec<&str> = pages["items"]
            .as_array()
            .expect("pages are a list")
            .iter()
            .filter_map(|page| page["fields"]["url"].as_str())
            .collect();
        assert!(
            urls.contains(&"https://example.org/sign-in"),
            "the popup is kept as a page: {urls:?}"
        );
    }

    #[test]
    fn developer_tools_are_asked_for() {
        let b = Browser::new();
        b.page("developer_tools", &[]);
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("developer_tools")
        );
    }

    /// What the window hands over when the page's history is read is written into the same page
    /// record the window draws, so the view moves while a person looks.
    #[test]
    fn the_pages_it_can_go_to_reach_the_record() {
        let b = Browser::new();
        b.page(
            "remember_history",
            &[
                ("direction", Value::text("back")),
                (
                    "entries",
                    Value::text(
                        r#"[{"id":2,"url":"https://example.org/earlier","title":"Earlier"}]"#,
                    ),
                ),
            ],
        );
        let back = b.rt.project(&b.read(&b.opened, "back"));
        assert_eq!(
            back["items"][0]["fields"]["title"].as_str(),
            Some("Earlier")
        );
    }

    #[test]
    fn a_saved_download_opens_through_the_systems_open() {
        let b = Browser::new();
        let refused = Browser::call(
            &b.app,
            &b.rt,
            "open_download",
            &[
                ("name", Value::text("report.pdf")),
                ("path", Value::text("")),
            ],
        );
        assert!(refused.is_err(), "a download with no path is refused");
        assert!(b.files.opened.borrow().is_empty(), "nothing was opened");

        b.run(
            "open_download",
            &[
                ("name", Value::text("report.pdf")),
                ("path", Value::text("/home/ada/report.pdf")),
            ],
        );
        assert_eq!(
            b.files.opened.borrow().as_slice(),
            ["/home/ada/report.pdf"],
            "the system opened exactly the download's path"
        );
    }

    #[test]
    fn a_bookmark_is_renamed_and_put_in_a_folder() {
        let b = Browser::new();
        let bookmark = b.rt.create("Bookmark").unwrap();
        b.run(
            "name_bookmark",
            &[
                ("k", Value::resource(&bookmark)),
                ("title", Value::text("A guide")),
            ],
        );
        b.run(
            "move_bookmark",
            &[
                ("k", Value::resource(&bookmark)),
                ("folder", Value::text("Work/Reading")),
            ],
        );
        assert_eq!(b.text(&bookmark, "title"), "A guide");
        assert_eq!(b.text(&bookmark, "folder"), "Work/Reading");

        // The bookmark's own window edits both, through the fields the controls bind.
        let view = b.view("bookmark_page", vec![Value::resource(&bookmark)]);
        let inputs = text_inputs(&view);
        view.edit(&inputs[0], &Value::text("Reading list")).unwrap();
        view.edit(&inputs[1], &Value::text("")).unwrap();
        assert_eq!(b.text(&bookmark, "title"), "Reading list");
        assert_eq!(b.text(&bookmark, "folder"), "");
    }

    #[test]
    fn a_page_with_a_video_offers_picture_in_picture_and_its_view_asks_to_float_it() {
        let b = Browser::new();
        // A page that reports no video offers nothing to float. (The app's published action of
        // the same name stands in the window's action menu whatever the page shows: this is the
        // page window's own control.)
        b.page("refresh_page", &[]);
        let view = b.page_view();
        assert!(
            plain_button(&view, "Picture in picture").is_none(),
            "no picture-in-picture control without a video:\n{}",
            dump(view.tree())
        );

        // The engine says what video the page shows: where it is in the viewport, how big the
        // picture is and what it is called.
        b.page.answer_asks(json!({
            "media": {
                "element": "video:1",
                "viewport": [0.1, 0.2, 0.5, 0.3],
                "intrinsic": [1280, 720],
                "label": "A talk on maps",
            },
        }));
        b.page("refresh_page", &[]);
        assert!(
            matches!(b.read(&b.opened, "media"), Value::Opt(Some(_))),
            "the page's record keeps the video the engine reported"
        );

        let view = b.page_view();
        let float = plain_button(&view, "Picture in picture")
            .expect("the page's control, once it shows a video");
        let webview = of_kind(view.tree(), "ui.WebView").remove(0);
        assert_eq!(
            webview.props.get("pip"),
            Some(&Value::Bool(false)),
            "the view floats nothing before the person asks"
        );

        // Pressing the control asks the window to float the page's video.
        view.fire(&float, "press").unwrap();
        assert_eq!(b.read(&b.opened, "pip"), Value::Bool(true));
        let view = b.page_view();
        let webview = of_kind(view.tree(), "ui.WebView").remove(0);
        assert_eq!(
            webview.props.get("pip"),
            Some(&Value::Bool(true)),
            "the page's view asks the window to float its video:\n{}",
            dump(view.tree())
        );
        // The page's menu offers the way back too.
        assert!(
            of_kind(view.tree(), "ui.MenuItem")
                .iter()
                .any(|row| row.props.get("label")
                    == Some(&Value::text("Return from picture in picture"))),
            "the menu offers the way back:\n{}",
            dump(view.tree())
        );

        // Leaving picture in picture puts the video back in the page.
        let back = plain_button(&view, "Return from picture in picture")
            .expect("the control, while the video floats");
        view.fire(&back, "press").unwrap();
        assert_eq!(b.read(&b.opened, "pip"), Value::Bool(false));
        let view = b.page_view();
        let webview = of_kind(view.tree(), "ui.WebView").remove(0);
        assert_eq!(
            webview.props.get("pip"),
            Some(&Value::Bool(false)),
            "the view stops floating the video"
        );
        assert_eq!(
            plain_button(&view, "Picture in picture"),
            Some(float.clone()),
            "the same control asks again"
        );

        // The person closed the picture's own window instead: the shell returned it, the page
        // hears that, and its control offers the float again.
        view.fire(&float, "press").unwrap();
        assert_eq!(b.read(&b.opened, "pip"), Value::Bool(true));
        let view = b.page_view();
        let webview = of_kind(view.tree(), "ui.WebView").remove(0);
        view.fire(&webview.key, "picture_in_picture_closed")
            .expect("the page hears the shell return its picture");
        assert_eq!(b.read(&b.opened, "pip"), Value::Bool(false));
        let view = b.page_view();
        let webview = of_kind(view.tree(), "ui.WebView").remove(0);
        assert_eq!(
            webview.props.get("pip"),
            Some(&Value::Bool(false)),
            "the view stops floating a picture the shell returned"
        );
        assert!(
            plain_button(&view, "Picture in picture").is_some(),
            "the control offers the float again:\n{}",
            dump(view.tree())
        );
    }

    /// An installed extension's side panel opens beside the page from the page's menu, in a page
    /// of its own, and closes: its own record, its own Close, and the engine told each time.
    #[test]
    fn an_extensions_side_panel_opens_beside_the_page_and_closes() {
        let b = Browser::new();
        let id = "abcdefghijklmnopabcdefghijklmnop";
        b.page(
            "remember_extensions",
            &[
                (
                    "installed",
                    Value::text(format!(
                        r#"[{{"id":"{id}","name":"Reader","version":"1.0","manifest_version":3,"description":"","permissions":["sidePanel"],"enabled":true,"keyword":"","side_panel":true}}]"#
                    )),
                ),
                ("message", Value::text("")),
            ],
        );

        // The page's menu offers the extension's side panel, and choosing it asks the extension.
        let view = b.page_view();
        let offered = of_kind(view.tree(), "ui.MenuItem")
            .into_iter()
            .find(|node| {
                matches!(node.props.get("label"), Some(Value::Text(label)) if label.contains("side panel"))
            })
            .unwrap_or_else(|| panic!("no side panel row in\n{}", dump(view.tree())));
        let offered = offered.key.clone();
        view.fire(&offered, "choose").unwrap();
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("open_side_panel")
        );
        assert_eq!(b.page.last()["extension"].as_str(), Some(id));

        // Its page shows beside the page the person browsed, as the window's second page.
        let url = format!("chrome-extension://{id}/panel.html");
        b.page(
            "remember_panel",
            &[
                ("open", Value::Bool(true)),
                ("kind", Value::text("side")),
                ("id", Value::text(id)),
                ("url", Value::text(&url)),
                ("interactive", Value::Bool(true)),
            ],
        );
        let view = b.page_view();
        let panels: Vec<_> = of_kind(view.tree(), "ui.WebView")
            .into_iter()
            .filter(|node| node.props.get("surface") == Some(&Value::text("panel")))
            .collect();
        assert_eq!(
            panels.len(),
            1,
            "one panel beside the page:\n{}",
            dump(view.tree())
        );
        assert_eq!(panels[0].props.get("title"), Some(&Value::text("Reader")));
        let close = button_key(&view, "Close");
        view.fire(&close, "press").unwrap();
        assert_eq!(b.page.ops().last().map(String::as_str), Some("panel_close"));

        // The window said it went: the page no longer draws it.
        b.page(
            "remember_panel",
            &[
                ("open", Value::Bool(false)),
                ("kind", Value::text("")),
                ("id", Value::text("")),
                ("url", Value::text("")),
                ("interactive", Value::Bool(false)),
            ],
        );
        let view = b.page_view();
        assert!(
            of_kind(view.tree(), "ui.WebView")
                .into_iter()
                .all(|node| node.props.get("surface") != Some(&Value::text("panel"))),
            "the panel is gone:\n{}",
            dump(view.tree())
        );
    }

    /// Typing an extension's keyword and a space in the address hands what follows to the
    /// extension: its rows are drawn, choosing one or Enter runs it, and anything else is an
    /// address.
    #[test]
    fn an_extensions_keyword_offers_its_rows_and_runs_what_is_chosen() {
        let b = Browser::new();
        let id = "abcdefghijklmnopabcdefghijklmnop";
        b.page(
            "remember_extensions",
            &[
                (
                    "installed",
                    Value::text(format!(
                        r#"[{{"id":"{id}","name":"Reader","version":"1.0","manifest_version":3,"description":"","permissions":[],"enabled":true,"keyword":"kw","side_panel":false}}]"#
                    )),
                ),
                ("message", Value::text("")),
            ],
        );
        let view = b.page_view();
        let address = text_inputs(&view)
            .first()
            .cloned()
            .expect("an address field");
        view.fire_with(&address, "changed", Some(&Value::text("kw ca")))
            .unwrap();
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("address_typed")
        );
        let ask = b.page.last();
        assert_eq!(ask["text"].as_str(), Some("kw ca"));
        assert_eq!(ask["extension"].as_str(), Some(id));
        assert_eq!(ask["name"].as_str(), Some("Reader"));
        assert_eq!(ask["rest"].as_str(), Some("ca"));

        // The extension offered what Enter does and two suggestions: they are rows under the
        // address, and choosing one sends its content to the extension.
        b.page(
            "remember_omnibox",
            &[
                ("id", Value::text(id)),
                ("name", Value::text("Reader")),
                ("text", Value::text("ca")),
                (
                    "rows",
                    Value::text(r#"[{"title":"Look up ca","content":"ca"},{"title":"cat","content":"cat"}]"#),
                ),
            ],
        );
        let view = b.page_view();
        let rows = of_kind(view.tree(), "ui.ListRow");
        let cat = rows
            .iter()
            .find(|node| node.props.get("title") == Some(&Value::text("cat")))
            .unwrap_or_else(|| panic!("no suggestion row in\n{}", dump(view.tree())));
        let cat = cat.key.clone();
        view.fire(&cat, "activate").unwrap();
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("address_entered")
        );
        assert_eq!(b.page.last()["text"].as_str(), Some("cat"));

        // Enter goes to the extension with what follows its keyword, never to the address.
        let view = b.page_view();
        view.fire_with(&address, "submit", Some(&Value::text("kw cats")))
            .unwrap();
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("address_entered")
        );
        assert_eq!(b.page.last()["text"].as_str(), Some("cats"));

        // Anything else is an address to go to (a bare site opens over https), and leaving the address tells the extension.
        let view = b.page_view();
        view.fire_with(&address, "submit", Some(&Value::text("example.org")))
            .unwrap();
        assert_eq!(b.page.ops().last().map(String::as_str), Some("navigate"));
        assert_eq!(b.page.last()["url"].as_str(), Some("https://example.org"));
        let view = b.page_view();
        view.fire(&address, "blur").unwrap();
        assert_eq!(
            b.page.ops().last().map(String::as_str),
            Some("address_left")
        );
    }

    /// An extension asking the person to sign in has its page shown in the page's window, named
    /// by the site they sign in on; turning it down answers the extension with no credentials.
    #[test]
    fn an_extensions_sign_in_shows_its_page_and_turning_it_down_closes_it() {
        let b = Browser::new();
        let id = "abcdefghijklmnopabcdefghijklmnop";
        b.page(
            "remember_panel",
            &[
                ("open", Value::Bool(true)),
                ("kind", Value::text("sign-in")),
                ("id", Value::text(id)),
                (
                    "url",
                    Value::text("https://accounts.example.org/o/oauth?client=1"),
                ),
                ("interactive", Value::Bool(true)),
            ],
        );
        let view = b.page_view();
        let panels: Vec<_> = of_kind(view.tree(), "ui.WebView")
            .into_iter()
            .filter(|node| node.props.get("surface") == Some(&Value::text("panel")))
            .collect();
        assert_eq!(
            panels.len(),
            1,
            "the sign-in page is shown:\n{}",
            dump(view.tree())
        );
        assert_eq!(
            panels[0].props.get("title"),
            Some(&Value::text("accounts.example.org")),
            "the person sees whose page asks them to sign in"
        );
        let close = button_key(&view, "Close");
        view.fire(&close, "press").unwrap();
        assert_eq!(b.page.ops().last().map(String::as_str), Some("panel_close"));
    }

    /// The address field of a page's window: the one that asks for an address or a search.
    fn address_field(view: &ViewInstance) -> String {
        of_kind(view.tree(), "ui.TextInput")
            .into_iter()
            .find(|f| f.props.get("key") == Some(&Value::text("l")))
            .unwrap_or_else(|| panic!("no address field in\n{}", dump(view.tree())))
            .key
            .clone()
    }

    /// The rows the address bar offers, top to bottom: each one's title and what it says under it.
    fn offered(view: &ViewInstance) -> Vec<(String, String)> {
        let Some(section) = of_kind(view.tree(), "ui.Section")
            .into_iter()
            .find(|s| text_prop(s, "title") == "Suggestions")
        else {
            return Vec::new();
        };
        of_kind(section, "ui.ListRow")
            .into_iter()
            .map(|r| (text_prop(r, "title"), text_prop(r, "secondary")))
            .collect()
    }

    /// Types `typed` into the address field and lets the typing pause: the bar offers nothing
    /// for it until the pause's timer, counting from what was typed, ticks.
    fn type_address(view: &mut ViewInstance, typed: &str) {
        let field = address_field(view);
        view.edit(&field, &Value::text(typed)).unwrap();
        view.poll_patches().unwrap();
        let timer = of_kind(view.tree(), "ui.Timer")
            .into_iter()
            .find(|t| t.props.get("restart") == Some(&Value::text(typed)))
            .unwrap_or_else(|| panic!("no pause counted from {typed:?} in\n{}", dump(view.tree())))
            .key
            .clone();
        view.fire(&timer, "tick").unwrap();
        view.poll_patches().unwrap();
    }

    fn visit_at(b: &Browser, clock: &Rc<RefCell<String>>, at: &str, url: &str, title: &str) {
        *clock.borrow_mut() = at.to_owned();
        b.run(
            "visit",
            &[
                ("b", Value::resource(&b.root)),
                ("url", Value::text(url)),
                ("title", Value::text(title)),
                ("at", Value::text(at)),
            ],
        );
    }

    /// The pages the address bar offers come in the order the person opens them, most often and
    /// most lately first, as the system ranks their history; and what it offers follows the
    /// typing only once the typing pauses.
    #[test]
    fn the_address_bar_offers_the_pages_opened_most_often_and_lately_once_the_typing_pauses() {
        let b = Browser::new();
        let clock = Rc::new(RefCell::new(String::new()));
        let reading = Rc::clone(&clock);
        b.rt.set_clock(move || reading.borrow().clone());
        // Opened once two months ago, first in the history; opened six times ten days ago; and
        // opened once this morning.
        visit_at(
            &b,
            &clock,
            "2026-09-25T09:00:00Z",
            "https://example.org/old",
            "Old notes",
        );
        for _ in 0..6 {
            visit_at(
                &b,
                &clock,
                "2026-11-14T09:00:00Z",
                "https://example.org/daily",
                "Daily",
            );
        }
        visit_at(
            &b,
            &clock,
            "2026-11-24T08:00:00Z",
            "https://example.org/today",
            "Today",
        );
        *clock.borrow_mut() = "2026-11-24T09:00:00Z".to_owned();

        let mut view = b.page_view();
        let field = address_field(&view);
        view.edit(&field, &Value::text("exam")).unwrap();
        view.poll_patches().unwrap();
        assert!(
            offered(&view).is_empty(),
            "nothing is offered for a key while the typing goes on: {:?}",
            offered(&view)
        );
        type_address(&mut view, "example.org/");
        assert_eq!(
            offered(&view),
            [
                ("Daily".to_owned(), "example.org/daily".to_owned()),
                ("Today".to_owned(), "example.org/today".to_owned()),
                ("Old notes".to_owned(), "example.org/old".to_owned()),
                (
                    "Search for “example.org/”".to_owned(),
                    "duckduckgo.com".to_owned()
                ),
            ],
            "the pages lead an address being typed, and the search follows"
        );
    }

    /// The address bar offers the person's bookmarks, the pages this browser opened and the ones
    /// another browser's history brought over, each page once, with the search engine's row
    /// first for words that name no address; choosing a row opens it and empties the bar.
    /// A browser with a bookmark, two pages it visited and another browser's history, for the
    /// bar's words and addresses to draw on.
    fn address_bar_world() -> Browser {
        let b = Browser::new();
        b.run(
            "bookmark",
            &[
                ("b", Value::resource(&b.root)),
                ("url", Value::text("https://cooking.example.com/kitchen")),
                ("title", Value::some(Value::text("Kitchen tools"))),
            ],
        );
        for (url, title) in [
            ("https://www.example.org/kitchen-diary", "Kitchen diary"),
            ("https://cooking.example.com/kitchen", "Kitchen tools page"),
        ] {
            b.run(
                "visit",
                &[
                    ("b", Value::resource(&b.root)),
                    ("url", Value::text(url)),
                    ("title", Value::text(title)),
                    ("at", Value::text("2026-11-24T09:00:00Z")),
                ],
            );
        }
        b.run(
            "import_history",
            &[
                ("b", Value::resource(&b.root)),
                (
                    "bundle",
                    Value::text(
                        r#"{"browser":"firefox","visits":[
                            {"url":"https://imported.example.net/kitchen","title":"Imported kitchen","last":1760000000,"count":4},
                            {"url":"https://imported.example.net/garden","title":"Garden","last":1760000000,"count":9}
                        ]}"#,
                    ),
                ),
            ],
        );
        b
    }

    #[test]
    fn the_address_bar_offers_bookmarks_imported_pages_and_the_search_and_each_row_opens() {
        let b = address_bar_world();

        let mut view = b.page_view();
        type_address(&mut view, "kitchen");
        assert_eq!(
            offered(&view),
            [
                (
                    "Search for “kitchen”".to_owned(),
                    "duckduckgo.com".to_owned()
                ),
                (
                    "Kitchen tools".to_owned(),
                    "cooking.example.com/kitchen".to_owned()
                ),
                (
                    "Kitchen diary".to_owned(),
                    "example.org/kitchen-diary".to_owned()
                ),
                (
                    "Imported kitchen".to_owned(),
                    "imported.example.net/kitchen".to_owned()
                ),
            ],
            "words lead with the search; a bookmarked page is offered once, as the bookmark"
        );

        // A page another browser's history holds is opened like any other.
        let row = |view: &ViewInstance, title: &str| {
            of_kind(view.tree(), "ui.ListRow")
                .into_iter()
                .find(|r| r.props.get("title") == Some(&Value::text(title)))
                .unwrap_or_else(|| panic!("no row {title} in\n{}", dump(view.tree())))
                .key
                .clone()
        };
        view.fire(&row(&view, "Imported kitchen"), "activate")
            .unwrap();
        assert_eq!(b.page.last()["op"], "navigate");
        assert_eq!(b.page.last()["url"], "https://imported.example.net/kitchen");
        assert_eq!(
            b.text(&b.opened, "url"),
            "https://imported.example.net/kitchen"
        );
        view.poll_patches().unwrap();
        assert!(offered(&view).is_empty(), "choosing a row empties the bar");

        // The search row asks the person's search engine for the words.
        type_address(&mut view, "kitchen scales");
        view.fire(&row(&view, "Search for “kitchen scales”"), "activate")
            .unwrap();
        assert_eq!(
            b.page.last()["url"],
            "https://duckduckgo.com/?q=kitchen+scales"
        );

        // An address being typed leads with the pages it names, and offers the search after them.
        type_address(&mut view, "imported.example.net/g");
        assert_eq!(
            offered(&view),
            [
                (
                    "Garden".to_owned(),
                    "imported.example.net/garden".to_owned()
                ),
                (
                    "Search for “imported.example.net/g”".to_owned(),
                    "duckduckgo.com".to_owned()
                ),
            ]
        );

        // Enter opens what was typed: words as the search engine's answer to them.
        let field = address_field(&view);
        view.edit(&field, &Value::text("best knives")).unwrap();
        view.poll_patches().unwrap();
        view.fire_with(&field, "submit", Some(&Value::text("best knives")))
            .unwrap();
        assert_eq!(
            b.page.last()["url"],
            "https://duckduckgo.com/?q=best+knives"
        );
    }
}
