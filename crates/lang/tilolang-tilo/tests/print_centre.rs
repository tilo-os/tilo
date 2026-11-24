//! Print Centre end to end: the printers and their jobs over the device's readings, the print
//! form and the scan form, and the engine work each asks for.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilolang::IdsLock;
use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::host_work::EngineHost;
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

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
        self.content.borrow_mut().insert(hash.to_owned(), bytes.to_vec());
    }
}

impl BlobHost for Device {
    fn read(&self, hash: &str) -> Option<Vec<u8>> {
        self.content.borrow().get(hash).cloned()
    }

    fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
        Ok(Vec::new())
    }

    fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
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

#[derive(Clone, Default)]
struct Sent(Rc<RefCell<Vec<Invocation>>>);

impl Sent {
    fn asked(&self, name: &str) -> Vec<Value> {
        self.0
            .borrow()
            .iter()
            .find_map(|invocation| match invocation {
                Invocation::Action {
                    name: asked, args, ..
                } if asked == name => Some(args.iter().map(|(_, value)| value.clone()).collect()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the app asks `{name}`"))
    }
}

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

/// The engine: what it was asked, and the answers it gives.
#[derive(Clone, Default)]
struct Engine {
    asked: Rc<RefCell<Vec<(String, String)>>>,
    polls: Rc<RefCell<u32>>,
}

impl Engine {
    fn ops(&self) -> Vec<String> {
        self.asked.borrow().iter().map(|(op, _)| op.clone()).collect()
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

    fn start(&self, _app: &str, op: &str, input: &str) -> Result<String, String> {
        self.asked
            .borrow_mut()
            .push((op.to_owned(), input.to_owned()));
        match op {
            "print" | "scan" => Ok(format!(r#"{{"job":"{op}-1"}}"#)),
            other => Err(format!("`{other}` is no op this device runs")),
        }
    }

    fn status(&self, job: &str) -> Result<String, String> {
        if !job.starts_with("print-") && !job.starts_with("scan-") {
            return Err(format!("no job `{job}`"));
        }
        let mut polls = self.polls.borrow_mut();
        *polls += 1;
        if *polls == 1 {
            Ok(r#"{"state":"running","url":"","error":""}"#.to_owned())
        } else if job.starts_with("scan-") {
            Ok(r#"{"state":"done","url":"tilo-blob://page1","error":""}"#.to_owned())
        } else {
            Ok(r#"{"state":"done","url":"","error":""}"#.to_owned())
        }
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
        let metas = tilolang_tilo::platform_apps::platform_packages()
            .expect("the platform's packages read");
        for meta in &metas {
            host.runtime().load_foreign(meta.clone()).expect("the device is loaded");
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
        device.holding("page1", b"\x89PNG\r\n\x1a\n% the scanned page\n");
        device.holding("pdf1", b"%PDF-1.4\n% the scanned document\n");
        host.runtime().set_blobs(device);
        host.runtime().set_reading(&printers(), PRINTERS).expect("the printers read");
        host.runtime().set_reading(&scanners(), SCANNERS).expect("the scanners read");
        let engine = Engine::default();
        host.runtime().set_engine(engine.clone());
        let root = host.runtime().create_root("PrintCentre").expect("the app's root");
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
            .map(|(_, id)| id.clone())
            .unwrap_or_else(|| panic!("no action {name}"));
        match self
            .host
            .runtime()
            .invoke_action(&me(), (id.as_str(), args))
            .unwrap_or_else(|why| panic!("{name} runs: {why}"))
        {
            tilolang_runtime::InvokeOutcome::Done(value) => value,
            other => panic!("{name} answers {other:?}"),
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
}

/// The words the page shows: each node's text, its title and the label a described component
/// reads by.
fn words(node: &UiNode, into: &mut Vec<String>) {
    for prop in ["value", "title", "label", "text", "message", "action"] {
        if let Some(Value::Text(text)) = node.props.get(prop) {
            into.push(text.clone());
        }
    }
    for child in &node.children {
        words(child, into);
    }
}

fn shown(view: &ViewInstance) -> Vec<String> {
    let mut out = Vec::new();
    words(view.tree(), &mut out);
    out
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

#[test]
fn the_overview_shows_the_printers_their_supplies_and_what_prints() {
    let app = Installed::new();
    let window = app.window();
    for wanted in ["studio", "office", "Report", "Black cartridge", "Default"] {
        assert!(contains(&window, wanted), "{wanted} shows: {:?}", shown(&window));
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
    for wanted in ["Accepting jobs", "Reachable", "Paused by a person", "Epson Perfection"] {
        assert!(contains(&window, wanted), "{wanted} shows: {:?}", shown(&window));
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
        .find(|button| matches!(button.props.get("label"), Some(Value::Text(text)) if text == "Add"))
        .map(|button| button.key.clone())
        .expect("the found printer's Add button");
    window.press(&add).expect("the button adds the printer");
    let asked = app.sent.asked("add");
    assert!(
        asked
            .iter()
            .any(|value| matches!(value, Value::Text(text) if text == "ipp://studio.local/printer")),
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
    assert!(contains(&window, "A4"), "the printer's paper: {:?}", shown(&window));
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
    app.call("poll_scan", vec![app.root_value()]);
    app.call("poll_scan", vec![app.root_value()]);
    let mut window = app.window();
    pick(&mut window, "place", Value::text("scan"));
    assert!(contains(&window, "Page"), "{:?}", shown(&window));
}

#[test]
fn a_page_is_turned_read_copied_and_removed() {
    let app = Installed::new();
    app.call("scan_page", vec![app.root_value()]);
    app.call("poll_scan", vec![app.root_value()]);
    app.call("poll_scan", vec![app.root_value()]);
    let mut window = app.window();
    pick(&mut window, "place", Value::text("scan"));
    for wanted in ["Turn", "Read its text", "Remove", "Save this page"] {
        assert!(has(&window, wanted), "{wanted} shows: {:?}", shown(&window));
    }
}

#[test]
fn the_scanned_document_is_written_as_one_pdf() {
    let app = Installed::new();
    app.call("scan_page", vec![app.root_value()]);
    app.call("poll_scan", vec![app.root_value()]);
    app.call("poll_scan", vec![app.root_value()]);
    let saved = app.call("document_to_save", vec![app.root_value()]);
    match saved {
        Value::File(file) => assert_eq!(file.media, "application/pdf"),
        other => panic!("a file is saved, not {other:?}"),
    }
    assert!(app.engine.ops().iter().any(|op| op == "pages_pdf"));
}
