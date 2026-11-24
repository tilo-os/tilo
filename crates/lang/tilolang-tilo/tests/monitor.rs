//! System Monitor end to end: the process list over the device's `processes` reading, its
//! arrangement, its inspector and the verbs that end a process, and the machine's readings.

use std::cell::RefCell;
use std::rc::Rc;

use tilolang::IdsLock;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

const SOURCE: &str = include_str!("../../../../apps/tl/monitor/app.tl");

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

fn reading() -> ResourceRef {
    ResourceRef::parse_id("processes", "so.tilo.device#Processes").expect("a resource id")
}

/// What the device publishes: a parent with a child, a stopped process and the machine.
const READING: &str = r#"{
  "serving": true,
  "read_error": null,
  "sampled_ms": 1760000000000,
  "interval_ms": 1500,
  "paused": false,
  "machine": {
    "hostname": "ada-laptop",
    "kernel": "6.17.1",
    "cpu_model": "Ryzen 7",
    "logical_cpus": 2,
    "uptime_secs": 93784,
    "cpu": {"total": 21.5, "per_core": [30.0, 13.0], "history": [12.0, 21.5]},
    "load_average": [0.4, 0.3, 0.2],
    "memory": {"total": 16000000000, "available": 8000000000, "swap_total": 4000000000,
               "swap_free": 4000000000, "history": [40.0, 50.0]},
    "pressure": null,
    "disks": [{"device": "nvme0n1", "mount": "/", "total_bytes": 500000000000,
               "used_bytes": 200000000000, "read_per_sec": 1000.0, "write_per_sec": 2000.0}],
    "disk_read_history": [0.0, 1000.0],
    "disk_write_history": [0.0, 2000.0],
    "network": {"rx_per_sec": 4096.0, "tx_per_sec": 1024.0, "rx_history": [0.0, 4096.0],
                "tx_history": [0.0, 1024.0],
                "interfaces": [{"name": "wlan0", "rx_per_sec": 4096.0, "tx_per_sec": 1024.0}]},
    "gpus": [],
    "batteries": [{"name": "BAT0", "percent": 80.0, "status": "Discharging", "power_watts": 5.0}],
    "thermal": [{"name": "Tctl", "celsius": 45.0}]
  },
  "processes": [
    {"pid": 10, "ppid": 0, "started_at": 100, "name": "systemd", "app_id": null, "user": "root",
     "cpu": 0.5, "memory_bytes": 12000000, "virtual_bytes": 200000000, "threads": 1,
     "state": "sleeping", "run_secs": 90000, "kernel_thread": false, "endable": false,
     "cpu_history": [0.4, 0.5]},
    {"pid": 11, "ppid": 10, "started_at": 110, "name": "sshd", "app_id": null, "user": "root",
     "cpu": 2.0, "memory_bytes": 8000000, "virtual_bytes": 100000000, "threads": 2,
     "state": "running", "run_secs": 80000, "kernel_thread": false, "endable": true,
     "cpu_history": [1.0, 2.0]},
    {"pid": 12, "ppid": 11, "started_at": 120, "name": "bash", "app_id": "so.tilo.terminal",
     "user": "ada", "cpu": 5.0, "memory_bytes": 4000000, "virtual_bytes": 20000000,
     "threads": 1, "state": "sleeping", "run_secs": 1000, "kernel_thread": false,
     "endable": true, "cpu_history": [5.0, 5.0],
     "cmd": "bash --login", "open_files": 12},
    {"pid": 13, "ppid": 10, "started_at": 130, "name": "kworker/0:1", "app_id": null,
     "user": "root", "cpu": 0.0, "memory_bytes": 0, "virtual_bytes": 0, "threads": 1,
     "state": "idle", "run_secs": 90000, "kernel_thread": true, "endable": false,
     "cpu_history": [0.0, 0.0]}
  ],
  "apps": [
    {"app_id": "so.tilo.terminal", "processes": 1, "cpu": 0.1, "memory_bytes": 4000000,
     "endable": true}
  ],
  "pending": [],
  "problem": null
}"#;

const MISSING: &str = r#"{
  "serving": false,
  "read_error": "the system information service is not answering",
  "sampled_ms": 0,
  "interval_ms": 1500,
  "paused": false,
  "machine": {
    "hostname": "", "kernel": "", "cpu_model": null, "logical_cpus": 0, "uptime_secs": 0,
    "cpu": {"total": 0.0, "per_core": [], "history": []},
    "load_average": [],
    "memory": {"total": 0, "available": 0, "swap_total": 0, "swap_free": 0, "history": []},
    "pressure": null, "disks": [], "disk_read_history": [], "disk_write_history": [],
    "network": {"rx_per_sec": 0.0, "tx_per_sec": 0.0, "rx_history": [], "tx_history": [],
                "interfaces": []},
    "gpus": [], "batteries": [], "thermal": []
  },
  "processes": [], "apps": [], "pending": [], "problem": null
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

struct Installed {
    host: Host,
    root: ResourceRef,
    view: String,
    actions: Vec<(String, String)>,
    sent: Sent,
}

impl Installed {
    fn new(reading_json: &str) -> Self {
        let host = Host::new(me(), || None).expect("a host");
        // Only the packages the app `use`s: the whole platform carries the actions package the
        // host already runs, and a host loads one package once.
        let metas = tilolang_tilo::platform_apps::named_platform_packages(SOURCE)
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
        host.runtime()
            .set_reading(&reading(), reading_json)
            .expect("the reading is set");
        let root = host
            .runtime()
            .create_root("Monitor")
            .expect("the app's root");
        Self {
            host,
            root,
            view,
            actions,
            sent,
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

/// The first node under `node` whose words satisfy `wanted`.
fn find<'a>(node: &'a UiNode, wanted: &dyn Fn(&str) -> bool) -> Option<&'a UiNode> {
    let own = ["value", "title", "label", "text", "message", "action"]
        .iter()
        .filter_map(|prop| node.props.get(*prop))
        .filter_map(Value::as_text)
        .any(wanted);
    if own {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, wanted))
}

/// The nodes of `kind`, in order.
fn of_kind<'a>(tree: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
    let mut found = Vec::new();
    fn walk<'a>(node: &'a UiNode, kind: &str, found: &mut Vec<&'a UiNode>) {
        if node.kind == kind {
            found.push(node);
        }
        for child in &node.children {
            walk(child, kind, found);
        }
    }
    walk(tree, kind, &mut found);
    found
}

fn has(view: &ViewInstance, wanted: &str) -> bool {
    shown(view).iter().any(|text| text == wanted)
}

fn contains(view: &ViewInstance, wanted: &str) -> bool {
    shown(view).iter().any(|text| text.contains(wanted))
}

/// Pick `value` for the window's `state`, as the person does, and let the view run again: the
/// window draws what the person picked at its next turn.
fn pick(window: &mut ViewInstance, state: &str, value: Value) {
    window.set_view_state(state, value);
    window.poll_patches().expect("the window runs again");
}

#[test]
fn the_process_list_shows_the_processes_and_what_the_machine_uses() {
    let app = Installed::new(READING);
    let window = app.window();
    let words = shown(&window);
    for wanted in ["systemd", "sshd", "bash"] {
        assert!(
            words.iter().any(|text| text.ends_with(wanted)),
            "{wanted} shows: {words:?}"
        );
    }
    for wanted in ["CPU", "Memory in use", "Network", "Disk"] {
        assert!(
            words.iter().any(|text| text == wanted),
            "{wanted} shows: {words:?}"
        );
    }
    assert!(
        !contains(&window, "kworker/0:1"),
        "kernel threads are hidden by default"
    );
}

#[test]
fn the_tree_indents_children_under_their_parent_and_the_flat_list_does_not() {
    let app = Installed::new(READING);
    let mut window = app.window();
    let words = shown(&window);
    let systemd = words
        .iter()
        .position(|text| text.ends_with("systemd"))
        .expect("a parent");
    let sshd = words
        .iter()
        .position(|text| text.starts_with("  ") && text.ends_with("sshd"))
        .unwrap_or_else(|| panic!("a child, indented: {words:?}"));
    assert!(sshd > systemd, "the child follows its parent: {words:?}");

    pick(&mut window, "tree", Value::Bool(false));
    let words = shown(&window);
    assert!(words.iter().any(|text| text.ends_with("sshd")), "{words:?}");
    assert!(
        !words.iter().any(|text| text.starts_with("  ")),
        "no indentation flat: {words:?}"
    );
}

#[test]
fn kernel_threads_show_when_they_are_asked_for() {
    let app = Installed::new(READING);
    let mut window = app.window();
    pick(&mut window, "kernel", Value::Bool(true));
    assert!(contains(&window, "kworker/0:1"), "{:?}", shown(&window));
}

/// The window's search narrows the list: what the person searches for in the window's bar, with no
/// filter field of the app's own.
#[test]
fn the_filter_narrows_the_list() {
    fn shell(node: &UiNode) -> Option<String> {
        if node.kind == "ui.NavShell" {
            return Some(node.key.clone());
        }
        node.children.iter().find_map(shell)
    }
    let app = Installed::new(READING);
    let mut window = app.window();
    let shell = shell(window.tree()).expect("the window is a shell");
    window
        .fire_with(&shell, "search", Some(&Value::text("sshd")))
        .expect("the window's search reaches the app");
    window.poll_patches().expect("the window runs again");
    let words = shown(&window);
    assert!(words.iter().any(|text| text.ends_with("sshd")), "{words:?}");
    assert!(
        !words.iter().any(|text| text.ends_with("systemd")),
        "{words:?}"
    );
}

#[test]
fn sorting_by_name_orders_the_list_rising() {
    let app = Installed::new(READING);
    let mut window = app.window();
    pick(&mut window, "sort", Value::text("name"));
    pick(&mut window, "descending", Value::Bool(false));
    pick(&mut window, "tree", Value::Bool(false));
    let words = shown(&window);
    let bash = words
        .iter()
        .position(|text| text.ends_with("bash"))
        .expect("bash");
    let systemd = words
        .iter()
        .position(|text| text.ends_with("systemd"))
        .expect("systemd");
    assert!(bash < systemd, "bash stands before systemd: {words:?}");
}

#[test]
fn a_chosen_process_shows_its_facts_and_the_verbs_that_end_it() {
    let app = Installed::new(READING);
    let mut window = app.window();
    pick(&mut window, "chosen", Value::Int(12));
    let words = shown(&window);
    for wanted in [
        "PID",
        "User",
        "Status",
        "Threads",
        "Virtual memory",
        "Running for",
        "Command line",
        "bash --login",
        "Open files",
        "12",
        "End process",
        "Kill process",
    ] {
        assert!(
            words.iter().any(|text| text == wanted),
            "{wanted} shows: {words:?}"
        );
    }
}

#[test]
fn a_persons_cadence_is_their_window_state_and_reaches_the_device() {
    let app = Installed::new(READING);
    let mut window = app.window();
    let item = find(&window.tree(), &|text| text == "Every 3 seconds")
        .expect("the cadence menu");
    window.fire(&item.key, "choose").expect("the cadence is chosen");
    window.poll_patches().expect("the window runs again");
    assert_eq!(
        window.view_state("interval_ms"),
        Some(Value::some(Value::Int(3000))),
        "the cadence is the person's own state, not the app's document"
    );
    let timers = of_kind(window.tree(), "ui.Timer");
    assert_eq!(timers.len(), 1, "the window carries the cadence's clock");
    window.fire(&timers[0].key, "tick").expect("the clock strikes");
    let asked = app.sent.asked("set-interval");
    assert_eq!(asked[1], Value::Int(3000), "the device follows the person");
}

#[test]
fn an_app_from_the_reading_is_offered_its_end() {
    let app = Installed::new(READING);
    let mut window = app.window();
    pick(&mut window, "chosen", Value::Int(12));
    assert!(has(&window, "End app"));
}

#[test]
fn ending_a_process_asks_the_device_for_its_lifetime() {
    let app = Installed::new(READING);
    // The buttons are the reading's verbs; the app names the process by its lifetime.
    app.call("end_app", vec![Value::text("so.tilo.terminal")]);
    assert_eq!(
        app.sent.asked("end-app")[1],
        Value::text("so.tilo.terminal")
    );
}

#[test]
fn the_resources_tab_shows_the_machine_readings() {
    let app = Installed::new(READING);
    let mut window = app.window();
    pick(&mut window, "tab", Value::text("resources"));
    let words = shown(&window);
    for wanted in [
        "Processor",
        "Processor use",
        "Memory",
        "RAM",
        "Network",
        "Network throughput",
        "Disk",
        "Disk throughput",
        "Battery",
        "Temperature",
        "This computer",
        "ada-laptop",
        "1d 2h 3m",
    ] {
        assert!(
            words.iter().any(|text| text == wanted),
            "{wanted} shows: {words:?}"
        );
    }
}

#[test]
fn a_computer_with_no_system_information_service_says_so_and_offers_to_try_again() {
    let app = Installed::new(MISSING);
    let window = app.window();
    let words = shown(&window);
    assert!(
        words.iter().any(|text| text == "Processes unavailable"),
        "{words:?}"
    );
    assert!(words.iter().any(|text| text == "Try again"), "{words:?}");
}

#[test]
fn a_process_uses_what_stands_under_it_when_the_person_asks() {
    let app = Installed::new(READING);
    let window = app.window();
    assert!(has(&window, "2%"), "the parent's own use: {:?}", shown(&window));
    let mut window = app.window();
    pick(&mut window, "with_children", Value::Bool(true));
    assert!(
        has(&window, "7%"),
        "the parent's use with its child's: {:?}",
        shown(&window)
    );
}

#[test]
fn a_failed_end_says_why() {
    let reading = READING.replace(
        r#""problem": null"#,
        r#""problem": "the operating system refused the process signal""#,
    );
    let app = Installed::new(&reading);
    let window = app.window();
    assert!(
        contains(&window, "the operating system refused the process signal"),
        "{:?}",
        shown(&window)
    );
}
