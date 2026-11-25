//! A job an app starts outlives its window and its process: the device runs it, records its
//! moves, and once it ends hands the end to a process of the app started with no window, which
//! runs the `done:` the app's start named.

use super::*;

use runtime_portal::process_link::FromProcess;
use tilolang_process::engines;

use crate::engine_host::{DEVICE, UNDER, device, said};

/// An instance numbered by how many `host` ran, of `bundle` on `route` over the workspace `bus`
/// holds, keeping its files in `content`: its window when it shows one, and the workspace.
fn opened(
    host: &mut TilolangProcess,
    bundle: Bundle,
    route: &RouteSpec,
    bus: &Rc<Bus>,
    content: tilo_content::ContentStore,
    shows: bool,
) -> (Option<Window>, Rc<tilolang_process::testing::Workspace>) {
    let broker = Broker::on(bus, route.clone(), "ada");
    let workspace = broker.workspace();
    let mut manager = CollaborationManager::with_peer_transport(broker);
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        resource_type: None,
        handle: Some("ada".to_owned()),
    }));
    let _ = manager.poll();
    let connection = Connection::new(manager).expect("the connection joined its route");
    let store = ResourceStore::new(Rc::new(connection.port(None)));
    let number = u64::try_from(host.instances().count()).unwrap() + 1;
    let spec = Spec::new(
        route.app_id.clone(),
        route.cluster_id.to_string(),
        number,
        "ada",
    );
    let (spec, transport, window, watched): (
        Spec,
        Box<dyn tilo_render_protocol::patch::Transport>,
        _,
        _,
    ) = if shows {
        let (ours, theirs) = Channel::pair().unwrap();
        let watched = ours.as_fd().try_clone_to_owned().unwrap();
        (
            spec,
            Box::new(ChannelTransport::new(ours).unwrap()),
            Some(Window::over(ChannelTransport::new(theirs).unwrap())),
            Some(watched),
        )
    } else {
        (
            spec.showing(Shows::Nothing),
            Box::new(tilolang_process::hosted::NoView),
            None,
            None,
        )
    };
    let kept = tilolang_process::hosted::Kept {
        content: Some(content),
        ..tilolang_process::hosted::Kept::default()
    };
    let hosted = Hosted::new(
        spec,
        connection,
        (store, tilolang_process::hosted::memory_content()),
        Arc::new(bundle),
        &kept,
        transport,
        (&Rc::new(Phrases::new("en-US")), host.formats()),
        wake(),
    );
    host.add(hosted, watched);
    (window, workspace)
}

/// Invoke the app's own `action` on its root in the instance `host` runs, as `ada`.
fn invoke(host: &TilolangProcess, action: &str, args: Vec<Value>) {
    let hosted = host.instances().next().expect("the app runs");
    let id = hosted
        .bundle()
        .meta
        .actions
        .iter()
        .find(|declared| declared.name == action)
        .unwrap_or_else(|| panic!("the app declares {action}"))
        .id
        .clone();
    let running = hosted.instance().host().expect("the instance runs");
    let root = running.root_of(&hosted.spec().app).expect("the app's root");
    let mut called = vec![Value::resource(&root)];
    called.extend(args);
    let actor =
        tilolang_tilo::runtime::Actor::Member(tilolang_tilo::runtime::ids::MemberId::person("ada"));
    running
        .runtime()
        .invoke_action(&actor, (id.as_str(), called))
        .unwrap_or_else(|error| panic!("{action} runs: {error}"));
}

/// The text field `field` of the app's root, as the instance `host` runs reads it.
fn root_text(host: &TilolangProcess, field: &str) -> String {
    let hosted = host.instances().next().expect("the app runs");
    let running = hosted.instance().host().expect("the instance runs");
    let root = running.root_of(&hosted.spec().app).expect("the app's root");
    text_of(&running, &root, field)
}

/// The text field `field` of `at`, as `running` reads it.
fn text_of(
    running: &tilolang_tilo::host::Host,
    at: &tilolang_tilo::runtime::ResourceRef,
    field: &str,
) -> String {
    match running.runtime().read(at, field) {
        Ok(Value::Text(text)) => text,
        other => panic!("{field} reads {other:?}"),
    }
}

/// The text field `field` of the first of the root's `list`, as the instance `host` runs reads
/// it.
fn first_text(host: &TilolangProcess, list: &str, field: &str) -> String {
    let hosted = host.instances().next().expect("the app runs");
    let running = hosted.instance().host().expect("the instance runs");
    let root = running.root_of(&hosted.spec().app).expect("the app's root");
    let Ok(Value::List(items)) = running.runtime().read(&root, list) else {
        panic!("{list} is a list");
    };
    let first = items
        .into_iter()
        .find_map(|item| match item {
            Value::Ref(r) => Some(std::rc::Rc::unwrap_or_clone(r.target)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{list} holds one"));
    text_of(&running, &first, field)
}

/// The moves the process said of its jobs, until one ended: the last.
fn ended(device: &runtime_portal::process_link::Descriptor) -> serde_json::Value {
    loop {
        match said(device) {
            FromProcess::Job { record, .. }
                if matches!(
                    record["state"].as_str(),
                    Some("done" | "failed" | "cancelled")
                ) =>
            {
                return record;
            }
            FromProcess::Job { .. } => {}
            other => panic!("only the job's moves are said: {other:?}"),
        }
    }
}

#[test]
fn a_backup_runs_to_its_end_after_its_window_and_process_closed() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let device = device();
    let folders = tempfile::tempdir().unwrap();
    let (source, destination) = (folders.path().join("Photos"), folders.path().join("Kept"));
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&destination).unwrap();
    std::fs::write(source.join("beach.jpg"), b"the sea").unwrap();

    // The person starts a backup in Backups' window.
    let (app, bundle) = shipped(&shipped_dir().join("backups"));
    let bus = Rc::<Bus>::default();
    let route = RouteSpec::app(&app, uuid::Uuid::new_v4());
    let mut first = TilolangProcess::new(wake());
    let (mut window, _, _) = instance_on(
        &mut first,
        spec(1, "ada", &route),
        bundle.clone(),
        &route,
        Rc::default(),
        &bus,
    );
    settle(&mut first, &mut window);
    let ask = serde_json::json!({
        "source": source.display().to_string(),
        "destination": destination.display().to_string(),
        "policy_hourly": 1, "policy_daily": 1, "policy_weekly": 1, "policy_monthly": 1,
    });
    invoke(
        &first,
        "start_work",
        vec![Value::text("backup"), Value::text(&ask.to_string())],
    );
    settle(&mut first, &mut window);
    // The device is asked to run it, with what the app runs once it ends.
    let FromProcess::EngineStart(job) = said(&device) else {
        panic!("the device is asked to run the backup");
    };
    assert!(job.job.starts_with(UNDER), "{}", job.job);
    assert!(!job.here, "a backup needs nothing of the app's own");
    assert!(
        job.handlers.get("done").is_some_and(|done| !done.is_null()),
        "the start named its end: {}",
        job.handlers
    );
    assert_eq!(root_text(&first, "job"), job.job);

    // The window closes, and the app's process with it.
    drop(window);
    settle_with_no_view(&mut first);
    assert!(first.is_empty(), "the instance ended with its window");
    drop(first);

    // The device's engine host runs it to its end.
    engines::run(&job);
    let ended = ended(&device);
    assert_eq!(ended["state"], "done", "the backup ran: {ended}");
    assert!(
        std::fs::read_dir(&destination).unwrap().next().is_some(),
        "the snapshot was written"
    );
}

#[test]
fn a_print_runs_to_its_end_after_its_window_closed() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let device = device();
    let (app, bundle) = shipped(&shipped_dir().join("print-centre"));
    let bus = Rc::<Bus>::default();
    let route = RouteSpec::app(&app, uuid::Uuid::new_v4());
    let content = tilolang_process::hosted::memory_content();
    let letter = tilolang_process::files::keep(&content, "letter.txt", "text/plain", b"Dear Bo,")
        .expect("the document is kept");

    // The person sends a document to a printer from Print Centre's window, on a computer whose
    // session says it prints.
    Published::install().publish(
        "printers",
        r#"{"serving":true,"queues":[],"jobs":[],"nearby":[]}"#,
    );
    let mut process = TilolangProcess::new(wake());
    let (window, _) = opened(
        &mut process,
        bundle.clone(),
        &route,
        &bus,
        content.clone(),
        true,
    );
    let mut window = window.expect("a window");
    settle(&mut process, &mut window);
    invoke(&process, "absorb_document", vec![Value::file(letter)]);
    invoke(&process, "choose_printer", vec![Value::text("Office")]);
    invoke(&process, "send_to_printer", vec![]);
    settle(&mut process, &mut window);
    // A print reads what the app keeps, so it runs in the app's own process, which the device is
    // told of first and holds until it ends.
    let FromProcess::EngineStart(job) = said(&device) else {
        panic!("the device is told of the print");
    };
    assert!(job.here, "a print runs in the app's own process");
    assert!(
        job.handlers.get("done").is_some_and(|done| !done.is_null()),
        "the start named its end: {}",
        job.handlers
    );
    assert_eq!(
        first_text(&process, "documents", "job"),
        job.job,
        "the print started: {}",
        first_text(&process, "documents", "failed")
    );
    let ended = ended(&device);
    assert_eq!(ended["id"], job.job);

    // The window closes; the process the device held for the print stays.
    drop(window);
    settle_with_no_view(&mut process);
    assert!(
        process.instances().next().is_none(),
        "no instance of the app runs"
    );
    if ended["state"] == "failed" {
        // This machine has no printer: the print says why it failed.
        assert!(ended["detail"].is_object(), "{ended}");
    }
}

const PAINTER: &str = "so.tilo.painter";

/// A page the device paints, and the window that shows how far it got.
const PAINTER_SOURCE: &str = r#"app so.tilo.painter "Painter" (icon: .hash, category: .utility)

type Page {
    job: Text = ""
    words: Text = ""
    painted: Int = 0
}

root Page

default view bar for Page

/// What the device runs once the page is painted.
action finished(p: Page, j: host.Job) {
    p.painted = j.done
}

/// Paint the page: the device runs it.
action paint(p: Page) {
    match engine.start("page.paint", "second", done: j -> finished(p, j)) {
        .ok(job) => {
            p.job = job
        }
        .err(why) => {
            p.words = why
        }
    }
}

/// How far the job has got, as every window of the page shows it.
view bar(p: Page) {
    std.page(p) {
        section("Painting") {
            if let j = engine.job(p.job) {
                if j.state == .done {
                    ui.Text("painted {j.done} of {j.total}")
                } else {
                    ui.Text("painting {j.done} of {j.total}")
                }
            } else {
                ui.Text("not started")
            }
        }
    }
}
"#;

/// The painter's engine: its work needs nothing of the app's own, so the device runs it.
struct Painter;

impl tilolang_tilo::engine::Provider for Painter {
    fn jobs(&self) -> &'static [tilolang_tilo::engine::JobOp] {
        const JOBS: &[tilolang_tilo::engine::JobOp] = &[tilolang_tilo::engine::JobOp::new(
            "page.paint",
            tilolang_tilo::engine::Needs::NOTHING,
        )];
        JOBS
    }

    fn call(&self, op: &str, _input: &str) -> Result<String, String> {
        Err(format!("no call `{op}`"))
    }

    fn start(
        &self,
        _job: &str,
        _op: &str,
        _input: &str,
        _moved: tilolang_tilo::engine::JobSink,
    ) -> Result<(), String> {
        Err("the device's engine host runs it".to_owned())
    }

    fn cancel(&self, _job: &str) -> bool {
        false
    }
}

#[test]
fn a_window_that_did_not_start_a_job_follows_it_from_the_devices_records() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let device = device();
    tilolang_tilo::engine::register(PAINTER, Arc::new(Painter));
    let compiled = tilolang::compile_package(PAINTER_SOURCE, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|diagnostics| panic!("the painter compiles: {diagnostics:?}"));
    let bundle = Bundle::new(&compiled, []);
    let bus = Rc::<Bus>::default();
    bus.wake_on_answers(wake());
    let route = RouteSpec::app(PAINTER, uuid::Uuid::new_v4());

    // Window A starts the job; window B, another window of the page in a process of its own,
    // started nothing.
    let mut first = TilolangProcess::new(wake());
    let (window, _) = opened(
        &mut first,
        bundle.clone(),
        &route,
        &bus,
        tilolang_process::hosted::memory_content(),
        true,
    );
    let mut a = window.expect("window A");
    settle(&mut first, &mut a);
    let mut second = TilolangProcess::new(wake());
    let (window, _) = opened(
        &mut second,
        bundle,
        &route,
        &bus,
        tilolang_process::hosted::memory_content(),
        true,
    );
    let mut b = window.expect("window B");
    settle(&mut second, &mut b);
    invoke(&first, "paint", vec![]);
    settle(&mut first, &mut a);
    let FromProcess::EngineStart(job) = said(&device) else {
        panic!("the device is asked to paint");
    };
    assert!(job.job.starts_with(UNDER), "{}", job.job);
    // What runs once it ends goes to the device as the app's own published action, the job's
    // record left for the device to fill in.
    let done = &job.handlers["done"];
    assert!(
        done["action"]
            .as_str()
            .is_some_and(|action| action.ends_with("finished")),
        "{done}"
    );
    assert!(
        done["resource"]
            .as_str()
            .is_some_and(|resource| resource.contains(PAINTER)),
        "{done}"
    );
    assert_eq!(
        done["arguments"]["j"],
        serde_json::json!({ "$job": null }),
        "{done}"
    );
    let (_, id) = tilo_actions::remote::parse_job_path(&job.job).expect("a job's path");
    let space = tilo_actions::remote::device_record_space(&DEVICE);
    let publish = |state: &str, done: u64| {
        bus.publish_device_record(
            &space,
            tilo_actions::remote::DEVICE_JOBS,
            &id,
            serde_json::json!({
                "id": id, "app": PAINTER, "op": "page.paint", "state": state,
                "detail": { "done": done, "total": 4 },
            }),
        );
    };

    // The device's record of the job moves, and window B, which never held it, shows each move.
    publish("running", 1);
    settle(&mut second, &mut b);
    assert!(shown(&b).contains("painting 1 of 4"), "{}", shown(&b));
    publish("running", 3);
    settle(&mut second, &mut b);
    assert!(shown(&b).contains("painting 3 of 4"), "{}", shown(&b));
    publish("done", 4);
    settle(&mut second, &mut b);
    assert!(shown(&b).contains("painted 4 of 4"), "{}", shown(&b));
    // Window A reads it from the same records.
    settle(&mut first, &mut a);
    assert!(shown(&a).contains("painted 4 of 4"), "{}", shown(&a));
}

const CLIPPER: &str = "so.tilo.clipper";

/// A clip an engine opened from a library makes, and what the app keeps once it is made.
const CLIPPER_SOURCE: &str = r#"app so.tilo.clipper "Clipper" (icon: .hash, category: .utility)

type Clips {
    job: Text = ""
    words: Text = ""
    kept: Text = ""
}

type Made {
    url: Text
}

root Clips

default view bar for Clips

/// What runs once the clip is made: the file it made.
action made(c: Clips, j: host.Job) {
    if j.state == .done {
        match json.decode<Made>(j.detail) {
            .ok(made) => {
                c.kept = made.url
            }
            .err(why) => {
                c.words = why
            }
        }
    }
}

/// Make a clip: the app's engine makes it in the app's own process.
action clip(c: Clips) {
    match engine.start("clip", "\{\}", done: j -> made(c, j)) {
        .ok(job) => {
            c.job = job
        }
        .err(why) => {
            c.words = why
        }
    }
}

view bar(c: Clips) {
    std.page(c) {
        section("Clips") {
            ui.Text(c.kept)
        }
    }
}
"#;

/// The clipper's engine, as a library exports it: each clip is made on a thread of its own after
/// `start` returned, kept through the device the job was handed, and told done with its file.
mod clipper {
    use tilo_engine_loader::{Device, EngineApi, Frames, JobOp, JobSink, Needs};

    fn jobs() -> &'static [JobOp] {
        const JOBS: &[JobOp] = &[JobOp::new("clip", Needs::APP_STATE)];
        JOBS
    }

    fn call(_device: &Device, op: &str, _input: &str) -> Result<String, String> {
        Err(format!("no call `{op}`"))
    }

    fn start(
        device: &Device,
        _job: &str,
        _op: &str,
        _input: &str,
        moved: &JobSink,
    ) -> Result<(), String> {
        let (device, moved) = (*device, *moved);
        std::thread::spawn(move || {
            moved.moved("running", r#"{"done":1,"total":2}"#);
            match device.keep_content("clip.avi", "video/x-msvideo", b"a moving picture") {
                Ok(kept) => moved.moved(
                    "done",
                    &serde_json::json!({ "url": format!("tilo-blob://{}", kept.hash) })
                        .to_string(),
                ),
                Err(why) => moved.moved("failed", &serde_json::json!({ "error": why }).to_string()),
            }
        });
        Ok(())
    }

    fn status(_device: &Device, job: &str) -> Result<String, String> {
        Err(format!("the device records `{job}`"))
    }

    fn stream(
        _device: &Device,
        _op: &str,
        _input: &str,
        _frames: &Frames,
    ) -> Result<String, String> {
        Err("no streams".to_owned())
    }

    fn cancel(_device: &Device, _job: &str) -> bool {
        false
    }

    pub const API: EngineApi = EngineApi {
        app: super::CLIPPER,
        jobs,
        call,
        start,
        status,
        stream,
        cancel,
    };
}

#[test]
fn a_loaded_engines_job_reaches_its_done_after_its_window_closed() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let device = device();
    tilolang_process::loaded_engines::give(clipper::API);
    let compiled = tilolang::compile_package(CLIPPER_SOURCE, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|diagnostics| panic!("the clipper compiles: {diagnostics:?}"));
    let bundle = Bundle::new(&compiled, []);
    let bus = Rc::<Bus>::default();
    let route = RouteSpec::app(CLIPPER, uuid::Uuid::new_v4());
    let content = tilolang_process::hosted::memory_content();

    // The person starts a clip in the app's window.
    let mut process = TilolangProcess::new(wake());
    let (window, _) = opened(&mut process, bundle, &route, &bus, content.clone(), true);
    let mut window = window.expect("a window");
    settle(&mut process, &mut window);
    invoke(&process, "clip", vec![]);
    settle(&mut process, &mut window);

    // The device is told of it first, and the engine runs it in the app's own process.
    let FromProcess::EngineStart(job) = said(&device) else {
        panic!("the device is told of the clip: {}", root_text(&process, "words"));
    };
    assert!(job.job.starts_with(UNDER), "{}", job.job);
    assert!(job.here, "a loaded engine's job runs in the app's own process");
    assert!(
        job.handlers.get("done").is_some_and(|done| !done.is_null()),
        "the start named its end: {}",
        job.handlers
    );
    assert_eq!(root_text(&process, "job"), job.job);

    // The window closes; the engine's moves still reach the device's record, the last with the
    // file the clip made, kept in the app's store.
    drop(window);
    settle_with_no_view(&mut process);
    let ended = ended(&device);
    assert_eq!(ended["id"], job.job, "{ended}");
    assert_eq!(ended["state"], "done", "the clip was made: {ended}");
    let url = ended["detail"]["url"].as_str().expect("the clip's file");
    let hash = url.strip_prefix("tilo-blob://").expect("a content address");
    assert_eq!(
        tilolang_process::files::read(&content, hash).as_deref(),
        Some(&b"a moving picture"[..]),
        "the clip is kept in the app's own store"
    );
}
