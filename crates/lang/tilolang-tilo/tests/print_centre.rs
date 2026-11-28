//! Print Centre end to end: the printers and their jobs over the device's readings, the print
//! form and the scan form, and the engine work each asks for.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilolang::IdsLock;
use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest, Saved};
use tilolang_runtime::delegate::Invocation;
use tilolang_runtime::host_work::{EngineHost, JobHandlers};
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

use crate::fixtures::invoker::{Answered as _, Sent};

const SOURCE: &str = include_str!("../../../../apps/tl/print-centre/app.tl");

/// The device the test runs on: its content store, of the content it was handed and the files
/// it kept. The engine answers with addresses into it, and the app names the files there.
#[derive(Clone, Default)]
struct Device {
    content: Rc<RefCell<HashMap<String, Vec<u8>>>>,
}

impl Device {
    /// `bytes` held at `hash`, as content the engine's answers name.
    fn holding(&self, hash: &str, bytes: &[u8]) {
        self.content
            .borrow_mut()
            .insert(hash.to_owned(), bytes.to_vec());
    }
}

impl BlobHost for Device {
    fn read(&self, hash: &str) -> Option<Vec<u8>> {
        self.content.borrow().get(hash).cloned()
    }

    fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
        Ok(Vec::new())
    }

    fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<Saved, String> {
        Err("this test writes no files out".to_owned())
    }

    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        // The content's own address: the same bytes are one file, as a content store keeps them.
        let mut digest = 0xcbf2_9ce4_8422_2325_u64;
        for byte in bytes {
            digest = (digest ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        let hash = format!("k{digest:x}");
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

/// The device's PDF engine for the print form: a file of `pages` pages, each drawn as the
/// document's own bytes. Every other read is one this test does not do.
struct Pdf {
    pages: usize,
}

impl tilolang_runtime::pdf::PdfHost for Pdf {
    fn pages(&self, _bytes: &[u8]) -> Result<String, String> {
        let rows: Vec<serde_json::Value> = (1..=self.pages)
            .map(|page| serde_json::json!({"page": page, "width": 612.0, "height": 792.0}))
            .collect();
        Ok(serde_json::Value::Array(rows).to_string())
    }

    fn outline(&self, _bytes: &[u8]) -> Result<String, String> {
        Err("this test reads no contents".to_owned())
    }

    fn text(&self, _bytes: &[u8], _page: i64) -> Result<String, String> {
        Err("this test reads no text".to_owned())
    }

    fn links(&self, _bytes: &[u8], _page: i64) -> Result<String, String> {
        Err("this test reads no links".to_owned())
    }

    fn fields(&self, _bytes: &[u8]) -> Result<String, String> {
        Err("this test reads no form".to_owned())
    }

    fn markup(&self, _bytes: &[u8]) -> Result<String, String> {
        Err("this test reads no markup".to_owned())
    }

    fn picture(&self, bytes: &[u8], _page: i64, _width: i64) -> Result<Vec<u8>, String> {
        Ok(bytes.to_vec())
    }

    fn import(&self, _bytes: &[u8]) -> Result<String, String> {
        Err("this test imports no page".to_owned())
    }

    fn save(&self, _sources: &[&[u8]], _request: &str) -> Result<Vec<u8>, String> {
        Err("this test writes no PDF".to_owned())
    }
}

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

fn printers() -> ResourceRef {
    ResourceRef::parse_id("printers", "so.tilo.device#Printers").expect("a resource id")
}

fn scanners() -> ResourceRef {
    ResourceRef::parse_id("scanners", "so.tilo.device#Scanners").expect("a resource id")
}

const PRINTERS: &str = r#"{
  "serving": true,
  "queues": [
    {"name": "studio", "state": "ready", "default": true, "accepting": true, "reason": null,
     "supplies": [{"name": "Black cartridge", "color": "black", "level": 80}],
     "reachable": true, "media": ["A4", "Letter"], "default_media": "A4"},
    {"name": "office", "state": "paused", "default": false, "accepting": false,
     "reason": "Paused by a person", "supplies": [], "reachable": true,
     "media": [], "default_media": null}
  ],
  "jobs": [
    {"id": "studio-7", "title": "Report", "owner": "ada", "size_bytes": 2048,
     "submitted": "today", "destination": "studio", "state": "printing", "progress": 40},
    {"id": "studio-6", "title": "Invoice", "owner": "ada", "size_bytes": 1024,
     "submitted": "yesterday", "destination": "studio", "state": "printed", "progress": null},
    {"id": "studio-5", "title": "Lost", "owner": "ada", "size_bytes": 512,
     "submitted": "yesterday", "destination": "studio", "state": "failed", "progress": null}
  ],
  "nearby": [{"uri": "ipp://studio.local/printer", "name": "Studio printer", "location": "Desk"}],
  "pending": [],
  "problem": null
}"#;

const SCANNERS: &str = r#"{
  "available": true,
  "found": [{"device": "epson2:libusb:001:004", "description": "Epson Perfection"}],
  "pending": [],
  "problem": null
}"#;

/// The engine: what it was asked, the answers it gives, and what a job's start named as it moves
/// and ends.
#[derive(Clone, Default)]
struct Engine {
    asked: Rc<RefCell<Vec<(String, String)>>>,
    handlers: Rc<RefCell<Vec<JobHandlers>>>,
    /// How the last job stands, as the device says it: the job's detail while it runs.
    state: Rc<RefCell<String>>,
}

/// A job's record as the device that holds it says it stands.
fn record(id: &str, state: &str, detail: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"id": id, "op": "scan", "state": state, "detail": detail})
}

impl Engine {
    fn ops(&self) -> Vec<String> {
        self.asked
            .borrow()
            .iter()
            .map(|(op, _)| op.clone())
            .collect()
    }
}

impl EngineHost for Engine {
    fn call(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
        self.asked
            .borrow_mut()
            .push((op.to_owned(), input.to_owned()));
        match op {
            "options" => Ok(r#"{"page_sizes":[{"key":"A4","label":"A4","default":true}],"colour_modes":[{"key":"Gray","label":"Grayscale","default":false}],"duplex_modes":[],"orientations":[]}"#.to_owned()),
            "ocr" => Ok(r#"{"text":"Hello from the page"}"#.to_owned()),
            "pages_pdf" => Ok(r#"{"url":"tilo-blob://pdf1"}"#.to_owned()),
            other => Err(format!("`{other}` is no op this device runs")),
        }
    }

    fn start(
        &self,
        _app: &str,
        op: &str,
        input: &str,
        handlers: &JobHandlers,
    ) -> Result<String, String> {
        self.asked
            .borrow_mut()
            .push((op.to_owned(), input.to_owned()));
        match op {
            "print" | "scan" => {
                self.handlers.borrow_mut().push(handlers.clone());
                Ok(format!("{op}-1"))
            }
            other => Err(format!("`{other}` is no op this device runs")),
        }
    }

    fn job(&self, job: &str) -> Result<Option<serde_json::Value>, String> {
        if !job.starts_with("print-") && !job.starts_with("scan-") {
            return Err(format!("no job `{job}`"));
        }
        Ok(tilolang_engine::record_of(job, &self.state.borrow()))
    }

    fn cancel(&self, job: &str) -> bool {
        job.starts_with("print-") || job.starts_with("scan-")
    }
}

struct Installed {
    host: Host,
    root: ResourceRef,
    view: String,
    actions: Vec<(String, String)>,
    sent: Sent,
    engine: Engine,
}

impl Installed {
    fn new() -> Self {
        let host = Host::new(me(), || None).expect("a host");
        // Only the packages the app `use`s: the whole platform carries the actions package the
        // host already runs, and a host loads one package once.
        let metas = tilolang_tilo::platform::named_platform_packages(SOURCE)
            .expect("the platform's packages read");
        for meta in &metas {
            host.runtime()
                .load_foreign(meta.clone())
                .expect("the device is loaded");
        }
        let compiled = tilolang::compile_package(SOURCE, &IdsLock::new(), &metas)
            .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
        let package = host.load_compiled(&compiled).expect("the app loads");
        let view = package
            .meta
            .views
            .iter()
            .find(|view| view.name == "window")
            .expect("the window view")
            .id
            .clone();
        let actions = package
            .meta
            .actions
            .iter()
            .map(|action| (action.name.clone(), action.id.clone()))
            .collect();
        let sent = Sent::default();
        host.runtime().set_foreign_invoker(sent.clone());
        // What the engine's answers name: the chosen document's bytes, the scanned page's, and
        // the pages it wrote as one PDF.
        let device = Device::default();
        device.holding("h1", b"%PDF-1.4\n% the document being printed\n");
        // A real 2×2 picture, so a crop of it is a picture the codec reads and writes.
        device.holding(
            "page1",
            &[
                0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
                0x44, 0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00,
                0x00, 0x72, 0xb6, 0x0d, 0x24, 0x00, 0x00, 0x00, 0x11, 0x49, 0x44, 0x41, 0x54, 0x78,
                0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x84, 0x19, 0x60, 0x0c, 0x00, 0x47, 0xca,
                0x07, 0xf9, 0x67, 0x59, 0x6e, 0xb7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
                0xae, 0x42, 0x60, 0x82,
            ],
        );
        device.holding("pdf1", b"%PDF-1.4\n% the scanned document\n");
        host.runtime().set_blobs(device);
        host.runtime()
            .set_reading(&printers(), PRINTERS)
            .expect("the printers read");
        host.runtime()
            .set_reading(&scanners(), SCANNERS)
            .expect("the scanners read");
        let engine = Engine::default();
        host.runtime().set_engine(engine.clone());
        let root = host
            .runtime()
            .create_root("PrintCentre")
            .expect("the app's root");
        Self {
            host,
            root,
            view,
            actions,
            sent,
            engine,
        }
    }

    fn call(&self, name: &str, args: Vec<Value>) -> Value {
        let id = self
            .actions
            .iter()
            .find(|(asked, _)| asked == name)
            .map_or_else(|| panic!("no action {name}"), |(_, id)| id.clone());
        match crate::fixtures::invoker::invoke_confirmed(&self.host, &me(), (id.as_str(), args))
            .unwrap_or_else(|why| panic!("{name} runs: {why}"))
        {
            tilolang_runtime::InvokeOutcome::Done(value) => value,
            other => panic!("{name} answers {other:?}"),
        }
    }

    fn try_call(&self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        let id = self
            .actions
            .iter()
            .find(|(asked, _)| asked == name)
            .map_or_else(
                || Err(format!("no action {name}")),
                |(_, id)| Ok(id.clone()),
            )?;
        match crate::fixtures::invoker::invoke_confirmed(&self.host, &me(), (id.as_str(), args))? {
            tilolang_runtime::InvokeOutcome::Done(value) => Ok(value),
            other => Err(format!("{name} answers {other:?}")),
        }
    }

    fn window(&self) -> ViewInstance {
        self.host
            .runtime()
            .open_view(&me(), &self.view, vec![Value::resource(&self.root)])
            .expect("the window opens")
    }

    fn root_value(&self) -> Value {
        Value::resource(&self.root)
    }

    /// The device runs what the job's start named, now that it stands as `record` says: the
    /// handler it kept, filled with the job. The start kept one job at a time in these tests.
    fn moves(
        &self,
        handler: fn(&JobHandlers) -> Option<serde_json::Value>,
        record: &serde_json::Value,
    ) {
        let kept = {
            let kept = self.engine.handlers.borrow();
            handler(kept.last().expect("a start came before its job moved"))
                .expect("the start named this handler")
        };
        *self.engine.state.borrow_mut() = record["detail"].to_string();
        let call = self
            .host
            .runtime()
            .job_call(&kept, record)
            .expect("the handler's call");
        crate::fixtures::invoker::invoke_confirmed(&self.host, &me(), call)
            .expect("the handler runs");
    }
}

fn has(view: &ViewInstance, wanted: &str) -> bool {
    shown(view).iter().any(|text| text == wanted)
}

/// Pick `value` for the window's `state`, as the person does, and let the view run again: the
/// window draws what the person picked at its next turn.
fn pick(window: &mut ViewInstance, state: &str, value: Value) {
    window.set_view_state(state, value);
    window.poll_patches().expect("the window runs again");
}

fn contains(view: &ViewInstance, wanted: &str) -> bool {
    shown(view).iter().any(|text| text.contains(wanted))
}

/// The window's clock strikes once, as its timer does when the work it waits on is done.
fn tick(window: &mut ViewInstance) {
    let keys: Vec<String> = window
        .tree()
        .of_kind("ui.Timer")
        .into_iter()
        .map(|timer| timer.key.clone())
        .collect();
    assert!(
        !keys.is_empty(),
        "the window holds a timer: {:?}",
        shown(window)
    );
    for key in keys {
        window.fire(&key, "tick").expect("the clock strikes");
    }
    window.poll_patches().expect("the window runs again");
}

/// Every word the page shows.
fn shown(view: &ViewInstance) -> Vec<String> {
    crate::fixtures::texts_props(
        view,
        &[
            "value",
            "title",
            "label",
            "text",
            "message",
            "action",
            "secondary",
            "description",
            "primary",
        ],
    )
}

#[test]
fn the_overview_shows_the_printers_their_supplies_and_what_prints() {
    let app = Installed::new();
    let window = app.window();
    for wanted in ["studio", "office", "Report", "Black cartridge", "Default"] {
        assert!(
            contains(&window, wanted),
            "{wanted} shows: {:?}",
            shown(&window)
        );
    }
}

#[test]
fn the_queue_shows_the_active_jobs_and_the_history_the_finished_ones() {
    let app = Installed::new();
    let mut window = app.window();
    pick(&mut window, "place", Value::text("queue"));
    assert!(contains(&window, "Report"));
    assert!(!contains(&window, "Invoice"), "a printed job is history");

    pick(&mut window, "place", Value::text("history"));
    assert!(contains(&window, "Invoice"));
    assert!(contains(&window, "Lost"));
}

#[test]
fn diagnostics_names_how_each_printer_stands_and_its_scanners() {
    let app = Installed::new();
    let mut window = app.window();
    pick(&mut window, "place", Value::text("diagnostics"));
    for wanted in [
        "Accepting jobs",
        "Reachable",
        "Paused by a person",
        "Epson Perfection",
        "Printers responding",
        "2 of 2",
        "The printer answered the latest status check.",
        "Not accepting new documents",
        "This printer does not report supply levels.",
    ] {
        assert!(
            contains(&window, wanted),
            "{wanted} shows: {:?}",
            shown(&window)
        );
    }
}

#[test]
fn found_printers_are_offered_to_add() {
    let app = Installed::new();
    let mut window = app.window();
    pick(&mut window, "place", Value::text("discover"));
    assert!(has(&window, "Add"));
    assert!(contains(&window, "Studio printer"));
    let add = window
        .tree()
        .of_kind("ui.Button")
        .into_iter()
        .find(
            |button| matches!(button.props.get("label"), Some(Value::Text(text)) if text == "Add"),
        )
        .map(|button| button.key.clone())
        .expect("the found printer's Add button");
    window
        .press_answering(&app.host, &me(), &add)
        .expect("the button adds the printer");
    let asked = app.sent.asked_values("add");
    assert!(
        asked.iter().any(
            |value| matches!(value, Value::Text(text) if text == "ipp://studio.local/printer")
        ),
        "the printer it found is the one it adds: {asked:?}"
    );
}

#[test]
fn the_print_form_offers_the_printer_and_what_it_reports() {
    let app = Installed::new();
    let mut window = app.window();
    pick(&mut window, "place", Value::text("print"));
    assert!(contains(&window, "Choose a document"));
    assert!(contains(&window, "studio"));
}

#[test]
fn choosing_a_document_on_a_printer_asks_the_engine_for_its_options() {
    let app = Installed::new();
    app.call(
        "choose_printer",
        vec![app.root_value(), Value::text("studio")],
    );
    app.call(
        "absorb_document",
        vec![
            app.root_value(),
            Value::file(tilolang_runtime::blob::FileValue::new(
                "Report.pdf",
                "application/pdf",
                2048,
                "h1",
            )),
        ],
    );
    let asked = app.engine.asked.borrow().clone();
    assert!(asked.iter().any(|(op, _)| op == "options"), "{asked:?}");
    let mut window = app.window();
    pick(&mut window, "place", Value::text("print"));
    assert!(
        contains(&window, "A4"),
        "the printer's paper: {:?}",
        shown(&window)
    );
}

/// A document another app hands over is what this window prints, and the person's own window
/// opens on the print form so they choose the printer and how it prints there. What sends the
/// window there is their own document (`readers: author`), never a field of the shared record,
/// so it reaches no other member's window.
#[test]
fn a_file_another_app_hands_over_becomes_the_document_to_print() {
    let app = Installed::new();
    app.call(
        "print_file",
        vec![
            app.root_value(),
            Value::file(tilolang_runtime::blob::FileValue::new(
                "Example page.pdf",
                "application/pdf",
                2048,
                "h1",
            )),
        ],
    );
    assert!(
        app.engine.ops().iter().any(|op| op == "options"),
        "the printer's options are asked for the document it was handed: {:?}",
        app.engine.ops()
    );
    let window = app.window();
    assert_eq!(place_of(&window), Value::text("print"));
    assert!(
        contains(&window, "Example page.pdf"),
        "{:?}",
        shown(&window)
    );
    assert!(
        contains(&window, "studio"),
        "the printer the document prints on is offered: {:?}",
        shown(&window)
    );
    // The person's own choice of place takes the window back from the handed document: a place
    // they open from the launcher, as one they pick in the sidebar.
    assert!(window.show_place("history").expect("the history opens"));
    let window = app.window();
    assert_eq!(place_of(&window), Value::text("history"));
    assert!(
        !contains(&window, "Example page.pdf"),
        "{:?}",
        shown(&window)
    );
}

/// The place the window's sidebar shows chosen.
fn place_of(view: &ViewInstance) -> Value {
    view.tree()
        .of_kind("ui.NavList")
        .first()
        .and_then(|list| list.props.get("selected"))
        .cloned()
        .unwrap_or(Value::Unit)
}

#[test]
fn printing_asks_the_engine_with_the_form() {
    let app = Installed::new();
    app.call(
        "absorb_document",
        vec![
            app.root_value(),
            Value::file(tilolang_runtime::blob::FileValue::new(
                "Report.pdf",
                "application/pdf",
                2048,
                "h1",
            )),
        ],
    );
    app.call("send_to_printer", vec![app.root_value()]);
    let asked = app.engine.asked.borrow().clone();
    let (_, input) = asked
        .iter()
        .find(|(op, _)| op == "print")
        .unwrap_or_else(|| panic!("the engine prints: {asked:?}"));
    assert!(input.contains("studio"), "{input}");
    assert!(input.contains("h1"), "{input}");
}

#[test]
fn a_scan_keeps_the_page_the_scanner_answered() {
    let app = Installed::new();
    app.call("scan_page", vec![app.root_value()]);
    assert!(app.engine.ops().iter().any(|op| op == "scan"));
    app.moves(
        |handlers| handlers.done.clone(),
        &record(
            "scan-1",
            "done",
            serde_json::json!({"url": "tilo-blob://page1"}),
        ),
    );
    let mut window = app.window();
    pick(&mut window, "place", Value::text("scan"));
    assert!(contains(&window, "Page"), "{:?}", shown(&window));
}

#[test]
fn a_page_is_turned_read_copied_and_removed() {
    let app = Installed::new();
    app.call("scan_page", vec![app.root_value()]);
    app.moves(
        |handlers| handlers.done.clone(),
        &record(
            "scan-1",
            "done",
            serde_json::json!({"url": "tilo-blob://page1"}),
        ),
    );
    let mut window = app.window();
    pick(&mut window, "place", Value::text("scan"));
    for wanted in ["Turn", "Read its text", "Remove", "Save this page"] {
        assert!(has(&window, wanted), "{wanted} shows: {:?}", shown(&window));
    }
    // Reading the page's text is a step the page says it is doing until the text is there.
    let read = buttons(&window, "Read its text").first().map_or_else(
        || panic!("the page's read: {:?}", shown(&window)),
        |button| button.key.clone(),
    );
    window
        .fire(&read, "press")
        .expect("the press starts reading");
    window.poll_patches().expect("the window runs again");
    assert!(
        contains(&window, "Updating recognized text…"),
        "the page says it is reading: {:?}",
        shown(&window)
    );
    tick(&mut window);
    let window = app.window();
    assert!(
        contains(&window, "Hello from the page"),
        "the text the engine read is shown: {:?}",
        shown(&window)
    );
}

#[test]
fn the_scanned_document_is_written_as_one_pdf() {
    let app = Installed::new();
    app.call("scan_page", vec![app.root_value()]);
    app.moves(
        |handlers| handlers.done.clone(),
        &record(
            "scan-1",
            "done",
            serde_json::json!({"url": "tilo-blob://page1"}),
        ),
    );
    let saved = app.call("document_to_save", vec![app.root_value()]);
    match saved {
        Value::File(file) => assert_eq!(file.media, "application/pdf"),
        other => panic!("a file is saved, not {other:?}"),
    }
    assert!(app.engine.ops().iter().any(|op| op == "pages_pdf"));
}

/// The first scanned page of the person's document.
fn first_page(app: &Installed) -> ResourceRef {
    let scans = app
        .host
        .runtime()
        .read(&app.root, "scans")
        .expect("the scans read");
    let Value::List(scans) = scans else {
        panic!("scans are a list: {scans:?}")
    };
    let scan = scans
        .first()
        .and_then(|scan| match scan {
            Value::Ref(scan) => Some(std::rc::Rc::unwrap_or_clone(scan.target.clone())),
            _ => None,
        })
        .expect("the scanned document");
    let pages = app
        .host
        .runtime()
        .read(&scan, "pages")
        .expect("the pages read");
    let Value::List(pages) = pages else {
        panic!("pages are a list: {pages:?}")
    };
    pages
        .first()
        .and_then(|page| match page {
            Value::Ref(page) => Some(std::rc::Rc::unwrap_or_clone(page.target.clone())),
            _ => None,
        })
        .expect("a scanned page")
}

/// The pictures the window draws.
fn pictures(tree: &UiNode) -> Vec<&UiNode> {
    fn walk<'a>(node: &'a UiNode, found: &mut Vec<&'a UiNode>) {
        if node.kind == "ui.Image" {
            found.push(node);
        }
        for child in &node.children {
            walk(child, found);
        }
    }
    let mut found = Vec::new();
    walk(tree, &mut found);
    found
}

#[test]
fn a_scanned_page_is_cropped_to_the_part_kept_and_its_text_is_stale() {
    let app = Installed::new();
    app.call("scan_page", vec![app.root_value()]);
    app.moves(
        |handlers| handlers.done.clone(),
        &record(
            "scan-1",
            "done",
            serde_json::json!({"url": "tilo-blob://page1"}),
        ),
    );
    let page = first_page(&app);
    app.call("recognize", vec![Value::resource(&page)]);
    let mut window = app.window();
    pick(&mut window, "place", Value::text("scan"));
    assert!(
        contains(&window, "Hello from the page"),
        "{:?}",
        shown(&window)
    );
    let before = pictures(window.tree())
        .first()
        .and_then(|node| node.props.get("src").cloned())
        .expect("the page's picture");
    app.call(
        "crop_page",
        vec![
            app.root_value(),
            Value::resource(&page),
            Value::Float(0.0),
            Value::Float(0.0),
            Value::Float(0.5),
            Value::Float(0.5),
        ],
    );
    let mut window = app.window();
    pick(&mut window, "place", Value::text("scan"));
    let after = pictures(window.tree())
        .first()
        .and_then(|node| node.props.get("src").cloned())
        .expect("the cropped page's picture");
    assert_ne!(before, after, "the page is the cropped picture");
    assert!(
        !contains(&window, "Hello from the page"),
        "the text read off the whole page no longer describes it: {:?}",
        shown(&window)
    );
}

#[test]
fn a_new_scanned_document_clears_the_pages_and_is_refused_while_scanning() {
    let app = Installed::new();
    app.call("scan_page", vec![app.root_value()]);
    app.moves(
        |handlers| handlers.done.clone(),
        &record(
            "scan-1",
            "done",
            serde_json::json!({"url": "tilo-blob://page1"}),
        ),
    );
    app.call("new_scanned_document", vec![app.root_value()]);
    let mut window = app.window();
    pick(&mut window, "place", Value::text("scan"));
    assert!(!contains(&window, "Save this page"), "{:?}", shown(&window));
    // The change is the person's own: history takes it back.
    app.host
        .store()
        .undo_last(Some(&me()))
        .expect("the change is taken back");
    let mut window = app.window();
    pick(&mut window, "place", Value::text("scan"));
    assert!(contains(&window, "Save this page"), "{:?}", shown(&window));
    // A page being scanned is never thrown away.
    app.call("scan_page", vec![app.root_value()]);
    let refused = app.try_call("new_scanned_document", vec![app.root_value()]);
    assert!(
        refused.is_err(),
        "a scan in flight refuses the reset: {refused:?}"
    );
    let mut window = app.window();
    pick(&mut window, "place", Value::text("scan"));
    assert!(contains(&window, "Save this page"), "{:?}", shown(&window));
}

/// The buttons labelled `label` the window shows.
fn buttons<'a>(window: &'a ViewInstance, label: &str) -> Vec<&'a UiNode> {
    window
        .tree()
        .of_kind("ui.Button")
        .into_iter()
        .filter(
            |button| matches!(button.props.get("label"), Some(Value::Text(text)) if text == label),
        )
        .collect()
}

/// What the app asked of the device's printers under the action `name`, by its tilolang name or
/// the verb the device publishes it as.
fn printers_asked(app: &Installed, name: &str) -> Vec<Value> {
    let verb = name.replace('_', "-");
    app.sent
        .0
        .borrow()
        .iter()
        .find_map(|invocation| match invocation {
            Invocation::Action {
                name: asked, args, ..
            } if *asked == name || *asked == verb => {
                Some(args.iter().map(|(_, value)| value.clone()).collect())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("the app asks `{name}`"))
}

// Print Centre is where this computer's printers are kept: a person picks the paper a printer
// uses unless told otherwise, cancels what still waits (and only that), reads why a job failed
// beside it, and sees what the printer service refused.
#[test]
fn printers_their_paper_queue_and_failures_are_kept_in_one_place() {
    let app = Installed::new();
    let mut window = app.window();

    // The paper a printer uses: the one it uses now is chosen, and another is one press away.
    let letter = window
        .tree()
        .of_kind("ui.Chip")
        .into_iter()
        .find(|chip| matches!(chip.props.get("label"), Some(Value::Text(text)) if text == "Letter"))
        .map_or_else(
            || panic!("the studio printer offers Letter: {:?}", shown(&window)),
            |chip| chip.key.clone(),
        );
    window
        .press_answering(&app.host, &me(), &letter)
        .expect("the chip sets the paper");
    let asked = printers_asked(&app, "set_media");
    assert!(
        asked
            .iter()
            .any(|value| matches!(value, Value::Text(text) if text == "Letter"))
            && asked
                .iter()
                .any(|value| matches!(value, Value::Text(text) if text == "studio")),
        "Letter is set on studio: {asked:?}"
    );

    // What still prints can be cancelled; what printed or failed cannot, and the failed one can
    // be tried again. Cancelling asks first: the document is taken out of the printer's queue,
    // which is not a change this workspace can take back.
    pick(&mut window, "place", Value::text("queue"));
    let cancel = buttons(&window, "Cancel").first().map_or_else(
        || panic!("the printing job can be cancelled: {:?}", shown(&window)),
        |button| button.key.clone(),
    );
    window
        .fire(&cancel, "press")
        .expect("the button asks first");
    window.poll_patches().expect("the question draws");
    assert!(
        !app.sent.names().iter().any(|name| name == "cancel-job"),
        "nothing is cancelled before the person's yes"
    );
    assert!(
        contains(&window, "Report will be removed from the printer."),
        "the question names the document: {:?}",
        shown(&window)
    );
    let question = window.tree().of_kind("ui.ModalDialog").first().map_or_else(
        || panic!("the question: {:?}", shown(&window)),
        |question| question.key.clone(),
    );
    window
        .fire_with_answering(&app.host, &me(), &question, "act_primary", None)
        .expect("the yes cancels the job");
    let asked = printers_asked(&app, "cancel_job");
    assert!(
        asked
            .iter()
            .any(|value| matches!(value, Value::Text(text) if text == "studio-7")),
        "the printing job is the one cancelled: {asked:?}"
    );
    pick(&mut window, "place", Value::text("history"));
    assert!(
        buttons(&window, "Cancel").is_empty(),
        "nothing finished is cancelled"
    );
    assert_eq!(
        buttons(&window, "Retry").len(),
        1,
        "the failed job is tried again"
    );
    assert_eq!(
        buttons(&window, "Print again").len(),
        1,
        "the document that printed is offered again"
    );

    // The printer says why the job failed, and the service says what it refused.
    let failing = PRINTERS
        .replace(
            r#""accepting": true, "reason": null"#,
            r#""accepting": true, "reason": "Out of paper""#,
        )
        .replace(
            r#""problem": null"#,
            r#""problem": "studio did not take the job""#,
        );
    app.host
        .runtime()
        .set_reading(&printers(), &failing)
        .expect("the printers read");
    window.poll_patches().expect("the window runs again");
    assert!(
        contains(&window, "studio - Out of paper"),
        "{:?}",
        shown(&window)
    );
    assert!(
        contains(&window, "studio did not take the job"),
        "{:?}",
        shown(&window)
    );
}

#[test]
fn a_long_documents_pages_are_stepped_through_in_the_form() {
    let app = Installed::new();
    app.host.runtime().set_pdf(Pdf { pages: 3 });
    app.call(
        "absorb_document",
        vec![
            app.root_value(),
            Value::file(tilolang_runtime::blob::FileValue::new(
                "Long report.pdf",
                "application/pdf",
                2048,
                "h1",
            )),
        ],
    );
    let mut window = app.window();
    pick(&mut window, "place", Value::text("print"));
    assert!(
        contains(&window, "Preparing document preview"),
        "the form says the preview is being drawn: {:?}",
        shown(&window)
    );
    tick(&mut window);
    let mut window = app.window();
    assert!(contains(&window, "Page 1 of 3"), "{:?}", shown(&window));
    assert!(
        contains(&window, "The original document is unchanged."),
        "{:?}",
        shown(&window)
    );
    let next = buttons(&window, "Next").first().map_or_else(
        || panic!("the form's Next: {:?}", shown(&window)),
        |button| button.key.clone(),
    );
    window
        .press_answering(&app.host, &me(), &next)
        .expect("the press shows the next page");
    window.poll_patches().expect("the window runs again");
    assert!(contains(&window, "Page 2 of 3"), "{:?}", shown(&window));
    let previous = buttons(&window, "Previous").first().map_or_else(
        || panic!("the form's Previous: {:?}", shown(&window)),
        |button| button.key.clone(),
    );
    window
        .press_answering(&app.host, &me(), &previous)
        .expect("the press shows the page before");
    window.poll_patches().expect("the window runs again");
    assert!(contains(&window, "Page 1 of 3"), "{:?}", shown(&window));
}

#[test]
fn the_scan_defaults_are_the_ones_the_controls_offer() {
    let app = Installed::new();
    // A resolution the controls do not offer is refused, and the default stands.
    app.call("set_resolution", vec![app.root_value(), Value::Int(200)]);
    app.call("scan_page", vec![app.root_value()]);
    let asked = app.engine.asked.borrow().clone();
    let (_, input) = asked
        .iter()
        .find(|(op, _)| op == "scan")
        .unwrap_or_else(|| panic!("the engine scans: {asked:?}"));
    assert!(input.contains(r#""resolution":300"#), "{input}");
    // One the controls offer is what the scanner is asked for.
    app.call("set_resolution", vec![app.root_value(), Value::Int(600)]);
    app.call("scan_page", vec![app.root_value()]);
    let asked = app.engine.asked.borrow().clone();
    let last = asked
        .iter()
        .rfind(|(op, _)| op == "scan")
        .unwrap_or_else(|| panic!("the engine scans: {asked:?}"));
    assert!(last.1.contains(r#""resolution":600"#), "{}", last.1);
}

/// The field of the print form that takes the pages to print.
fn pages_field(window: &ViewInstance) -> String {
    window
        .tree()
        .of_kind("ui.TextInput")
        .into_iter()
        .find(|field| field.props.get("placeholder") == Some(&Value::text("1-3")))
        .map_or_else(
            || panic!("the pages field: {:?}", shown(window)),
            |field| field.key.clone(),
        )
}

#[test]
fn a_page_range_is_checked_before_the_document_leaves_the_window() {
    let app = Installed::new();
    app.call(
        "absorb_document",
        vec![
            app.root_value(),
            Value::file(tilolang_runtime::blob::FileValue::new(
                "Report.pdf",
                "application/pdf",
                2048,
                "h1",
            )),
        ],
    );
    let mut window = app.window();
    pick(&mut window, "place", Value::text("print"));
    let field = pages_field(&window);
    window
        .edit(&field, &Value::text("12-3"))
        .expect("the field takes the range");
    window.poll_patches().expect("the window runs again");
    assert!(
        contains(&window, "A page range cannot end before it starts."),
        "the form says why the range cannot print: {:?}",
        shown(&window)
    );
    let print = buttons(&window, "Print")
        .first()
        .map_or_else(|| panic!("the Print button"), |button| button.key.clone());
    window
        .press_answering(&app.host, &me(), &print)
        .expect("the press runs");
    assert!(
        !app.engine.ops().iter().any(|op| op == "print"),
        "a range nobody can print never reaches the printer: {:?}",
        app.engine.ops()
    );
    let window = app.window();
    assert!(
        contains(&window, "A page range cannot end before it starts."),
        "{:?}",
        shown(&window)
    );

    // A range the printer takes is the range it is asked for.
    let mut window = app.window();
    pick(&mut window, "place", Value::text("print"));
    let field = pages_field(&window);
    window
        .edit(&field, &Value::text("1-3,5"))
        .expect("the field takes the range");
    window.poll_patches().expect("the window runs again");
    let print = buttons(&window, "Print")
        .first()
        .map_or_else(|| panic!("the Print button"), |button| button.key.clone());
    window
        .press_answering(&app.host, &me(), &print)
        .expect("the press prints");
    let asked = app.engine.asked.borrow().clone();
    let (_, input) = asked
        .iter()
        .find(|(op, _)| op == "print")
        .unwrap_or_else(|| panic!("the engine prints: {asked:?}"));
    assert!(input.contains(r#""pages":"1-3,5""#), "{input}");
}

#[test]
fn a_finished_document_is_printed_again_from_the_history() {
    let app = Installed::new();
    let mut window = app.window();
    pick(&mut window, "place", Value::text("history"));
    let again = buttons(&window, "Print again").first().map_or_else(
        || panic!("the printed document: {:?}", shown(&window)),
        |button| button.key.clone(),
    );
    window
        .press_answering(&app.host, &me(), &again)
        .expect("the press prints it again");
    let asked = printers_asked(&app, "retry_job");
    assert!(
        asked
            .iter()
            .any(|value| matches!(value, Value::Text(text) if text == "studio-6")),
        "the document that printed is the one printed again: {asked:?}"
    );
}

#[test]
fn a_captured_page_counts_towards_its_document() {
    let app = Installed::new();
    app.call("scan_page", vec![app.root_value()]);
    app.moves(
        |handlers| handlers.done.clone(),
        &record(
            "scan-1",
            "done",
            serde_json::json!({"url": "tilo-blob://page1"}),
        ),
    );
    let mut window = app.window();
    pick(&mut window, "place", Value::text("scan"));
    assert!(
        contains(
            &window,
            "1 page captured. New scans are added to this document."
        ),
        "{:?}",
        shown(&window)
    );
}

#[test]
fn a_printer_is_checked_and_the_search_is_said_while_it_runs() {
    let app = Installed::new();
    let window = app.window();
    let check = buttons(&window, "Check printer").first().map_or_else(
        || panic!("the printer's check: {:?}", shown(&window)),
        |button| button.key.clone(),
    );
    window
        .press_answering(&app.host, &me(), &check)
        .expect("the check asks the device");
    assert!(
        app.sent.names().iter().any(|name| name == "find"),
        "the printer is read again: {:?}",
        app.sent.names()
    );
    let searching = PRINTERS.replace(r#""pending": []"#, r#""pending": ["find"]"#);
    app.host
        .runtime()
        .set_reading(&printers(), &searching)
        .expect("the printers read");
    let mut window = app.window();
    window.poll_patches().expect("the window runs again");
    assert!(
        contains(&window, "Looking for printers on your network."),
        "{:?}",
        shown(&window)
    );
}

/// What the system's History says each change of the print and scan defaults did, in English and
/// in German: the choice each made, never the action's name alone.
#[test]
fn history_says_which_default_each_change_chose_in_the_readers_language() {
    let app = crate::fixtures::Pane::install_with_ids(
        &me(),
        SOURCE,
        include_str!("../../../../apps/tl/print-centre/app.ids"),
    )
    .reading(&printers(), PRINTERS)
    .reading(&scanners(), SCANNERS)
    .rooted("PrintCentre");
    let root = app.root_value();
    app.call("set_two_sided", vec![root.clone(), Value::Bool(true)]);
    app.call("set_two_sided", vec![root.clone(), Value::Bool(false)]);
    app.call("set_resolution", vec![root.clone(), Value::Int(600)]);
    app.call(
        "set_scan_colour",
        vec![root.clone(), Value::text("grayscale")],
    );
    app.call(
        "set_scan_source",
        vec![root.clone(), Value::text("feeder-duplex")],
    );
    app.call("set_ocr_language", vec![root, Value::text("deu")]);
    let Ok(Value::Ref(defaults)) = app.host.runtime().read(app.root(), "defaults") else {
        panic!("the defaults are a record of their own");
    };
    let german = include_str!("../../../../apps/tl/print-centre/locales/de-DE.ftl");
    let english = [
        "Set printing to both sides from now on",
        "Set printing to one side from now on",
        "Set scanning to 600 dpi from now on",
        "Set scanning to grayscale from now on",
        "Set scanning both sides from the feeder from now on",
        "Set text recognition to German from now on",
    ];
    let in_german = [
        "Stellte den Druck ab jetzt auf beidseitig um",
        "Stellte den Druck ab jetzt auf einseitig um",
        "Stellte das Scannen ab jetzt auf 600 dpi um",
        "Stellte das Scannen ab jetzt auf Graustufen um",
        "Stellte das Scannen ab jetzt auf beidseitig aus dem Einzug um",
        "Stellte die Texterkennung ab jetzt auf Deutsch um",
    ];
    for (locale, words, wanted) in [("en-US", "", english), ("de-DE", german, in_german)] {
        let mut said = app.history(&defaults.target, locale, words);
        let said = said.split_off(said.len().saturating_sub(wanted.len()));
        assert_eq!(said.len(), wanted.len(), "{said:?}");
        for (said, wanted) in said.iter().zip(wanted) {
            assert!(said.starts_with(wanted), "{wanted:?} in {locale}: {said:?}");
        }
    }
}

/// A printer's reason is the code its device's state reasons carry, said in the reader's
/// language, with the device's own words kept beside it; a code this version has no words for
/// is said as unknown, with the words that came.
#[test]
fn a_printers_reason_reads_in_the_readers_language() {
    let cases = [
        (
            "out-of-paper",
            "media-empty-error",
            "Add paper to continue.",
            "Legen Sie Papier ein, um fortzufahren.",
        ),
        (
            "jam",
            "media-jam-error",
            "Clear the paper jam, then resume the printer.",
            "Beheben Sie den Papierstau und setzen Sie den Drucker dann fort.",
        ),
        (
            "cover-open",
            "cover-open-error",
            "Close the printer cover.",
            "Schließen Sie die Abdeckung des Druckers.",
        ),
        (
            "supply-low",
            "toner-low",
            "A supply is running low; printing can usually continue for now.",
            "Ein Betriebsmittel geht zur Neige; der Druck kann meist vorerst weiterlaufen.",
        ),
        (
            "supply-empty",
            "toner-empty",
            "Replace the empty supply to continue.",
            "Ersetzen Sie das leere Betriebsmittel, um fortzufahren.",
        ),
        (
            "offline",
            "offline-report",
            "Check that the printer is awake and connected.",
            "Prüfen Sie, ob der Drucker eingeschaltet und verbunden ist.",
        ),
        (
            "unknown",
            "some-new-device-error",
            "The printer reported a condition that may need attention.",
            "Der Drucker meldet einen Zustand, der Aufmerksamkeit brauchen kann.",
        ),
    ];
    let german = tilolang::messages::module_catalogue(include_str!(
        "../../../../apps/tl/print-centre/locales/de-DE.ftl"
    ));
    let ids = include_str!("../../../../apps/tl/print-centre/app.ids");
    for (locale, catalogs) in [
        ("en-US", &[("en-US", "")][..]),
        ("de-DE", &[("en-US", ""), ("de-DE", german.as_str())][..]),
    ] {
        tilo_ui_base::i18n::I18nManager::install("en-US", locale, catalogs).expect("the words");
        for (code, reported, english, auf_deutsch) in cases {
            let wanted = if locale == "en-US" {
                english
            } else {
                auf_deutsch
            };
            let reading = PRINTERS.replace(
                r#""reason": null"#,
                &format!(r#""reason": "{reported}", "reason_code": "{code}""#),
            );
            let app = crate::fixtures::Pane::install_with_ids(&me(), SOURCE, ids)
                .reading(&printers(), &reading)
                .reading(&scanners(), SCANNERS)
                .rooted("PrintCentre");
            app.host.set_words(crate::words::Toolkit);
            let window = app.window();
            assert!(
                contains(&window, wanted),
                "{code} reads {wanted:?} in {locale}: {:?}",
                shown(&window)
            );
            assert!(
                contains(&window, reported),
                "the device's own words stay beside {code} in {locale}: {:?}",
                shown(&window)
            );
        }
    }
}
