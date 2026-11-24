//! Containers end to end: the window over the device's `containers` reading, the verbs it runs on
//! it, the draft a person (or an agent) fills, and the pull the app's engine answers.

use std::cell::RefCell;
use std::rc::Rc;

use tilolang::IdsLock;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::files::{Entry, FileTarget, FilesHost, TrashEntry};
use tilolang_runtime::host_work::EngineHost;
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

const SOURCE: &str = include_str!("../../../../apps/tl/containers/app.tl");
const APP: &str = "so.tilo.containers";

/// What the device publishes: one running container with logs, one stopped, an image, a volume
/// and a distrobox.
const READING: &str = r#"{
  "serving": true,
  "connection": "Podman 5.4, rootless",
  "live": true,
  "containers": [
    {
      "id": "9f3c1a7be4d2c0118af26b5d4e9f01a2",
      "name": "web-frontend",
      "image": "docker.io/library/nginx:1.27-alpine",
      "state": "running",
      "status_detail": "Up 4 hours",
      "command": "nginx -g 'daemon off;'",
      "created": "4 hours ago",
      "ports": [{"host_port": 8080, "container_port": 80, "protocol": "tcp"}],
      "mounts": [{"source": "site", "destination": "/usr/share/nginx/html", "read_only": true}],
      "env": [{"name": "TZ", "value": "UTC"}],
      "labels": [{"name": "io.tilo.cluster", "value": "dev-stack"}],
      "stats": {
        "available": true,
        "cpu_percent": 0.07,
        "mem_bytes": 18874368,
        "mem_limit_bytes": 268435456,
        "net_rx_bytes": 4812344,
        "net_tx_bytes": 19004221,
        "block_read_bytes": 1204224,
        "block_write_bytes": 90112,
        "pids": 5,
        "fetched_unix_ms": 1760000000000
      },
      "logs": ["GET / 200", "WARN upstream slow", "ERROR upstream gone"],
      "logs_error": null,
      "logs_fetched_unix_ms": 1760000000000
    },
    {
      "id": "e0b3a7c9f2d14e6b8c0a3f5d7e9b1c2d",
      "name": "build-runner",
      "image": "ghcr.io/northwind/ci-runner:latest",
      "state": "exited",
      "status_detail": "Exited (0) 2 hours ago",
      "command": "/runner/entrypoint.sh",
      "created": "yesterday",
      "ports": [],
      "mounts": [],
      "env": [],
      "labels": [],
      "stats": {"available": false, "cpu_percent": 0.0, "mem_bytes": 0, "mem_limit_bytes": 0,
                "net_rx_bytes": 0, "net_tx_bytes": 0, "block_read_bytes": 0,
                "block_write_bytes": 0, "pids": 0, "fetched_unix_ms": null},
      "logs": [],
      "logs_error": null,
      "logs_fetched_unix_ms": null
    }
  ],
  "images": [
    {"id": "img1", "repository": "docker.io/library/nginx", "tag": "1.27-alpine",
     "size_bytes": 50000000, "created": "a week ago", "in_use_by": 1}
  ],
  "volumes": [
    {"name": "pgdata", "driver": "local", "mountpoint": "/home/ada/.local/share/containers/volumes/pgdata/_data",
     "created": "a week ago", "in_use_by": 1}
  ],
  "distroboxes": [
    {"name": "toolbox", "image": "fedora:41", "state": "running", "status_detail": "Up 2 hours"}
  ],
  "pending": [],
  "problem": null
}"#;

/// A computer with no podman.
const MISSING: &str = r#"{
  "serving": false,
  "connection": "Podman didn't respond: podman is not on this computer",
  "live": false,
  "containers": [], "images": [], "volumes": [], "distroboxes": [],
  "pending": [], "problem": null
}"#;

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

fn reading() -> ResourceRef {
    ResourceRef::parse_id("containers", "so.tilo.device#Containers").expect("a resource id")
}

/// What the app asked of the device's verbs.
#[derive(Clone, Default)]
struct Sent(Rc<RefCell<Vec<Invocation>>>);

impl Sent {
    fn all(&self) -> Vec<Invocation> {
        self.0.borrow().clone()
    }

    /// The verb `name` was asked on the reading, with its arguments after the resource.
    fn asked(&self, name: &str) -> Vec<Value> {
        self.all()
            .into_iter()
            .find_map(|invocation| match invocation {
                Invocation::Action {
                    name: asked, args, ..
                } if asked == name => Some(args.into_iter().map(|(_, value)| value).collect()),
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

/// What the app opened as a shell.
#[derive(Clone, Default)]
struct Shells(Rc<RefCell<Vec<(String, String)>>>);

impl FilesHost for Shells {
    fn list(&self, _path: &str) -> Result<Vec<Entry>, String> {
        Err("this test reads no disk".to_owned())
    }

    fn stat(&self, _path: &str) -> Result<Option<Entry>, String> {
        Err("this test reads no disk".to_owned())
    }

    fn read(&self, _path: &str) -> Result<Vec<u8>, String> {
        Err("this test reads no disk".to_owned())
    }

    fn watch(&self, _path: &str, _run: &str) -> Result<String, String> {
        Err("this test watches nothing".to_owned())
    }

    fn trash(&self, _path: &str) -> Result<TrashEntry, String> {
        Err("this test trashes nothing".to_owned())
    }

    fn open(&self, _file: FileTarget<'_>) -> Result<(), String> {
        Err("this test opens nothing".to_owned())
    }

    fn container_shell(&self, container: &str, engine: &str) -> Result<(), String> {
        self.0
            .borrow_mut()
            .push((container.to_owned(), engine.to_owned()));
        Ok(())
    }
}

/// The app's engine: one pull job, answered running first and done after.
#[derive(Clone, Default)]
struct Pulls {
    asked: Rc<RefCell<Vec<(String, String)>>>,
    polls: Rc<RefCell<u32>>,
}

impl EngineHost for Pulls {
    fn call(&self, app: &str, op: &str, _input: &str) -> Result<String, String> {
        Err(format!("`{op}` is no op this device runs for {app}"))
    }

    fn start(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
        self.asked
            .borrow_mut()
            .push((app.to_owned(), format!("{op} {input}")));
        Ok(r#"{"job":"pull-1"}"#.to_owned())
    }

    fn status(&self, job: &str) -> Result<String, String> {
        if job != "pull-1" {
            return Err(format!("no job `{job}` runs on this device"));
        }
        let mut polls = self.polls.borrow_mut();
        *polls += 1;
        if *polls == 1 {
            Ok(r#"{"state":"running","lines":["Copying blob nginx"],"error":""}"#.to_owned())
        } else {
            Ok(r#"{"state":"done","lines":["Copying blob nginx","Done"],"error":""}"#.to_owned())
        }
    }

    fn cancel(&self, job: &str) -> bool {
        job == "pull-1"
    }
}

struct Installed {
    host: Host,
    root: ResourceRef,
    view: String,
    actions: Vec<(String, String)>,
    sent: Sent,
    shells: Shells,
    pulls: Pulls,
}

impl Installed {
    fn new(reading_json: &str) -> Self {
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
        host.runtime().set_invoker(sent.clone());
        host.runtime().set_reading(&reading(), reading_json).expect("the reading is set");
        let root = host.root_of(APP).expect("the app's root");
        Self {
            host,
            root,
            view,
            actions,
            sent,
            shells: Shells::default(),
            pulls: Pulls::default(),
        }
    }

    fn with_engine(self) -> Self {
        self.host.runtime().set_engine(self.pulls.clone());
        self.host.runtime().set_device_files(self.shells.clone());
        self
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
        self.window_showing(&[])
    }

    /// The window with the view state `states` names, drawn with every row: what a person sees
    /// rather than what a scroll would have to reach.
    fn window_showing(&self, states: &[(&str, &str)]) -> ViewInstance {
        let mut window = self
            .host
            .runtime()
            .open_view(&me(), &self.view, vec![Value::resource(&self.root)])
            .expect("the window opens");
        window
            .set_lazy_rows(false)
            .expect("the window draws its rows");
        for (state, value) in states {
            window.set_view_state(state, Value::text(*value));
        }
        window.poll_patches().expect("the window answers");
        window
    }

    fn root_value(&self) -> Value {
        Value::resource(&self.root)
    }
}

/// Every text under `node` that the window shows, in order.
fn words(node: &UiNode, into: &mut Vec<String>) {
    for prop in ["value", "title", "label", "message", "action"] {
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

#[test]
fn the_window_lists_the_places_and_what_the_engine_holds() {
    let app = Installed::new(READING);
    let window = app.window();
    let words = shown(&window);
    for wanted in ["Containers", "Images", "Volumes", "Distrobox", "web-frontend", "build-runner"] {
        assert!(words.iter().any(|text| text == wanted), "{wanted} shows: {words:?}");
    }
    assert_eq!(
        words.iter().filter(|text| text.as_str() == "Containers").count(),
        2,
        "the window's title and the place name it: {words:?}"
    );
}

#[test]
fn a_chosen_container_shows_its_logs_with_their_severity() {
    let app = Installed::new(READING);
    let window = app.window_showing(&[("chosen", "9f3c1a7be4d2c0118af26b5d4e9f01a2")]);
    let words = shown(&window);
    for wanted in ["GET / 200", "WARN upstream slow", "ERROR upstream gone", "Error", "Warning"] {
        assert!(words.iter().any(|text| text == wanted), "{wanted} shows: {words:?}");
    }
}

#[test]
fn a_running_container_offers_stop_restart_and_pause() {
    let app = Installed::new(READING);
    let window = app.window_showing(&[("chosen", "9f3c1a7be4d2c0118af26b5d4e9f01a2")]);
    for wanted in ["Stop", "Restart", "Pause", "Remove"] {
        assert!(has(&window, wanted), "{wanted} shows");
    }
}

#[test]
fn a_stopped_container_offers_start() {
    let app = Installed::new(READING);
    let window = app.window_showing(&[("chosen", "e0b3a7c9f2d14e6b8c0a3f5d7e9b1c2d")]);
    assert!(has(&window, "Start"));
    assert!(!has(&window, "Stop"));
}

#[test]
fn the_usage_tab_shows_what_the_container_used() {
    let app = Installed::new(READING);
    let window = app.window_showing(&[
        ("chosen", "9f3c1a7be4d2c0118af26b5d4e9f01a2"),
        ("tab", "stats"),
    ]);
    let words = shown(&window);
    assert!(words.iter().any(|text| text == "Processor"), "{words:?}");
    assert!(words.iter().any(|text| text == "Received"), "{words:?}");
    assert!(words.iter().any(|text| text == "Processes"), "{words:?}");
}

#[test]
fn the_details_tab_shows_ports_mounts_environment_and_labels() {
    let app = Installed::new(READING);
    let window = app.window_showing(&[
        ("chosen", "9f3c1a7be4d2c0118af26b5d4e9f01a2"),
        ("tab", "inspect"),
    ]);
    let words = shown(&window);
    for wanted in ["80/tcp", "8080", "/usr/share/nginx/html", "TZ", "io.tilo.cluster"] {
        assert!(words.iter().any(|text| text == wanted), "{wanted} shows: {words:?}");
    }
}

#[test]
fn the_images_place_shows_the_images_and_their_verbs() {
    let app = Installed::new(READING);
    let window = app.window_showing(&[("place", "images")]);
    let words = shown(&window);
    for wanted in ["docker.io/library/nginx:1.27-alpine", "Pull again", "Add a tag", "Pull an image"] {
        assert!(words.iter().any(|text| text == wanted), "{wanted} shows: {words:?}");
    }
}

#[test]
fn the_volumes_place_shows_the_volumes() {
    let app = Installed::new(READING);
    let window = app.window_showing(&[("place", "volumes")]);
    assert!(has(&window, "pgdata"));
    assert!(has(&window, "New volume"));
}

#[test]
fn the_distrobox_place_offers_each_box_its_shell() {
    let app = Installed::new(READING);
    let window = app.window_showing(&[("place", "distrobox")]);
    assert!(has(&window, "toolbox"));
    assert!(has(&window, "Enter"));
}

#[test]
fn a_computer_without_podman_says_so_and_how_to_try_again() {
    let app = Installed::new(MISSING);
    let window = app.window();
    let words = shown(&window);
    assert!(words.iter().any(|text| text == "Podman isn't available"), "{words:?}");
    assert!(words.iter().any(|text| text == "Try again"), "{words:?}");
}

#[test]
fn the_draft_is_filled_a_field_at_a_time_and_creates_the_container() {
    let app = Installed::new(READING);
    for (field, text) in [
        ("name", "web"),
        ("image", "nginx:alpine"),
        ("ports", "8080:80"),
        ("env", "TZ=UTC"),
        ("volumes", "site:/usr/share/nginx/html"),
        ("command", "nginx -g 'daemon off;'"),
    ] {
        app.call("fill", vec![app.root_value(), Value::text(field), Value::text(text)]);
    }
    app.call("create_container", vec![app.root_value()]);
    let asked = app.sent.asked("create");
    let Value::Ref(on) = &asked[0] else {
        panic!("the verb is called on a resource: {asked:?}")
    };
    assert_eq!(&*on.target, &reading(), "the verb is the reading's");
    let words: Vec<String> = asked[1..]
        .iter()
        .map(|value| match value {
            Value::Text(text) => text.clone(),
            other => panic!("a create argument is text, not {other:?}"),
        })
        .collect();
    assert_eq!(
        words,
        [
            "web",
            "nginx:alpine",
            "8080:80",
            "TZ=UTC",
            "site:/usr/share/nginx/html",
            "nginx -g 'daemon off;'",
        ]
    );
    // The form is cleared when the engine took the container.
    let draft = match app.host.runtime().read(&app.root, "draft").expect("the draft reads") {
        Value::Ref(r) => std::rc::Rc::unwrap_or_clone(r.target),
        other => panic!("the draft is a record, not {other:?}"),
    };
    assert_eq!(
        app.host.runtime().read(&draft, "image").expect("the image field"),
        Value::text("")
    );
}

#[test]
fn a_volume_is_made_and_removed_by_its_verbs() {
    let app = Installed::new(READING);
    app.call("make_volume", vec![app.root_value(), Value::text("cache")]);
    assert_eq!(app.sent.asked("create-volume")[1], Value::text("cache"));
    app.call(
        "prompt_run",
        vec![Value::text("remove-volume:cache"), Value::text("")],
    );
    assert_eq!(app.sent.asked("remove-volume")[1], Value::text("cache"));
}

#[test]
fn an_image_is_tagged_and_removed_by_its_verbs() {
    let app = Installed::new(READING);
    app.call(
        "tag_image",
        vec![app.root_value(), Value::text("nginx:alpine"), Value::text("web:edge")],
    );
    let tagged = app.sent.asked("tag");
    assert_eq!(tagged[1], Value::text("nginx:alpine"));
    assert_eq!(tagged[2], Value::text("web:edge"));
    app.call(
        "prompt_run",
        vec![Value::text("remove-image:nginx:alpine"), Value::text("")],
    );
    assert_eq!(app.sent.asked("remove-image")[1], Value::text("nginx:alpine"));
}

#[test]
fn a_container_is_removed_only_after_the_confirmation() {
    let app = Installed::new(READING);
    app.call(
        "confirm_run",
        vec![Value::text("container:9f3c1a7be4d2c0118af26b5d4e9f01a2")],
    );
    assert_eq!(
        app.sent.asked("remove")[1],
        Value::text("9f3c1a7be4d2c0118af26b5d4e9f01a2")
    );
}

#[test]
fn a_shell_opens_inside_the_container_and_a_distrobox() {
    let app = Installed::new(READING).with_engine();
    app.call(
        "open_shell",
        vec![Value::text("9f3c1a7be4d2c0118af26b5d4e9f01a2")],
    );
    app.call("enter_box", vec![Value::text("toolbox")]);
    assert_eq!(
        app.shells.0.borrow().clone(),
        [
            ("9f3c1a7be4d2c0118af26b5d4e9f01a2".to_owned(), "podman".to_owned()),
            ("toolbox".to_owned(), "distrobox".to_owned()),
        ]
    );
}

#[test]
fn a_pull_runs_in_the_apps_engine_and_its_output_shows() {
    let app = Installed::new(READING).with_engine();
    app.call("begin_pull", vec![app.root_value(), Value::text("nginx:alpine")]);
    let asked = app.pulls.asked.borrow().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].0, "so.tilo.containers");
    assert!(asked[0].1.starts_with("pull "), "{asked:?}");
    assert!(asked[0].1.contains("nginx:alpine"), "{asked:?}");

    app.call("poll_pull", vec![app.root_value()]);
    let window = app.window();
    assert!(has(&window, "Copying blob nginx"), "{:?}", shown(&window));

    app.call("poll_pull", vec![app.root_value()]);
    let window = app.window();
    let words = shown(&window);
    assert!(words.iter().any(|text| text == "Done"), "{words:?}");
    assert!(words.iter().any(|text| text == "The image is ready."), "{words:?}");
}
