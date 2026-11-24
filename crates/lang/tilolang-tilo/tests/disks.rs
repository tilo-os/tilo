//! Disks end to end: this computer's drives over the device's `disks` reading, its map, its
//! health, its partitions and the verbs that open, erase, measure and eject them.

use std::cell::RefCell;
use std::rc::Rc;

use tilolang::IdsLock;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

const SOURCE: &str = include_str!("../../../../apps/tl/disks/app.tl");

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

fn reading() -> ResourceRef {
    ResourceRef::parse_id("disks", "so.tilo.device#Disks").expect("a resource id")
}

/// What the device publishes: one healthy system drive with two partitions, and a failing
/// external drive without a partition table.
const READING: &str = r#"{
  "serving": true,
  "read_error": null,
  "drives": [
    {"name": "nvme0n1", "model": "Samsung SSD", "serial": "S1", "transport": "nvme",
     "kind": "disk", "size_bytes": 1000000000000, "rotational": false, "removable": false,
     "read_only": false, "fstype": null, "mountpoint": null,
     "partitions": [
       {"name": "nvme0n1p1", "label": "boot", "fstype": "vfat", "part_type_name": "EFI System",
        "size_bytes": 500000000, "used_bytes": 200000000, "mountpoint": "/boot/efi", "uuid": "u1"},
       {"name": "nvme0n1p2", "label": "root", "fstype": "btrfs",
        "part_type_name": "Linux filesystem", "size_bytes": 900000000000,
        "used_bytes": 400000000000, "mountpoint": "/", "uuid": "u2"}
     ],
     "health": {"supported": true, "failing": false, "warnings": [], "temperature_c": 40,
                "power_on_hours": 1200, "power_cycles": 300, "percent_used": 4,
                "available_spare": 100, "bytes_written": 9000000000, "bytes_read": 12000000000,
                "media_errors": 0, "bad_sectors": 0, "self_test": "success"}},
    {"name": "sda", "model": "USB Disk", "serial": "S2", "transport": "usb", "kind": "disk",
     "size_bytes": 64000000000, "rotational": true, "removable": true, "read_only": false,
     "fstype": "exfat", "mountpoint": null, "partitions": [],
     "health": {"supported": true, "failing": true, "warnings": ["temperature"],
                "temperature_c": 60, "power_on_hours": 9000, "power_cycles": 20,
                "percent_used": 90, "available_spare": 5, "bytes_written": 1000000000,
                "bytes_read": 2000000000, "media_errors": 3, "bad_sectors": 2,
                "self_test": "failed"}}
  ],
  "benchmark": null,
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

struct Installed {
    host: Host,
    root: ResourceRef,
    view: String,
    actions: Vec<(String, String)>,
    sent: Sent,
}

impl Installed {
    fn new() -> Self {
        let host = Host::new(me(), || None).expect("a host");
        let metas = tilolang_tilo::platform_apps::platform_packages()
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
        host.runtime().set_invoker(sent.clone());
        host.runtime()
            .set_reading(&reading(), READING)
            .expect("the reading is set");
        // The device publishes a `Disks` too (its drives' reading), so the app's own is named by
        // its id.
        let disks = package
            .meta
            .types
            .iter()
            .find(|ty| ty.name == "Disks")
            .expect("the app's root type")
            .id
            .clone();
        let root = host.runtime().create_root(&disks).expect("the app's root");
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

    fn root_value(&self) -> Value {
        Value::resource(&self.root)
    }
}

fn words(node: &UiNode, into: &mut Vec<String>) {
    if let Some(Value::Text(text)) = node.props.get("value") {
        into.push(text.clone());
    }
    if let Some(Value::Text(title)) = node.props.get("title") {
        into.push(title.clone());
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

fn contains(view: &ViewInstance, wanted: &str) -> bool {
    shown(view).iter().any(|text| text.contains(wanted))
}

fn has(view: &ViewInstance, wanted: &str) -> bool {
    shown(view).iter().any(|text| text == wanted)
}

#[test]
fn the_drive_list_names_the_drives_and_their_health() {
    let app = Installed::new();
    let window = app.window();
    assert!(contains(&window, "Samsung SSD"));
    assert!(contains(&window, "USB Disk"));
    assert!(contains(&window, "Predicted to fail"));
}

#[test]
fn a_chosen_drive_shows_its_partitions_and_unallocated_space() {
    let app = Installed::new();
    let window = app.window();
    window.set_view_state("chosen", Value::text("nvme0n1"));
    for wanted in [
        "How it is divided",
        "nvme0n1p1 boot",
        "Unallocated",
        "Open at /",
    ] {
        assert!(
            contains(&window, wanted),
            "{wanted} shows: {:?}",
            shown(&window)
        );
    }
}

#[test]
fn the_health_tab_shows_what_the_drive_reports() {
    let app = Installed::new();
    let window = app.window();
    window.set_view_state("chosen", Value::text("nvme0n1"));
    window.set_view_state("tab", Value::text("health"));
    for wanted in [
        "Verdict",
        "Temperature",
        "Life used",
        "Media errors",
        "Last self-test",
    ] {
        assert!(
            has(&window, wanted) || contains(&window, wanted),
            "{wanted} shows"
        );
    }
}

#[test]
fn the_partitions_tab_offers_each_partition_its_verbs() {
    let app = Installed::new();
    let window = app.window();
    window.set_view_state("chosen", Value::text("nvme0n1"));
    window.set_view_state("tab", Value::text("partitions"));
    let words = shown(&window);
    for wanted in ["nvme0n1p1", "nvme0n1p2", "Close", "Erase…"] {
        assert!(
            words.iter().any(|text| text.contains(wanted)),
            "{wanted} shows: {words:?}"
        );
    }
}

#[test]
fn a_drive_is_named_in_the_workspace() {
    let app = Installed::new();
    // The buttons are the reading's verbs; each names the device it is called on.
    let id = app
        .actions
        .iter()
        .find(|(name, _)| name == "name_drive")
        .map(|(_, id)| id.clone())
        .expect("the rename verb");
    app.host
        .runtime()
        .invoke_action(
            &me(),
            (
                id.as_str(),
                vec![app.root_value(), Value::text("S1"), Value::text("System")],
            ),
        )
        .expect("the rename runs");
    let window = app.window();
    assert!(contains(&window, "System"), "{:?}", shown(&window));
}

#[test]
fn erasing_asks_for_the_filesystem_and_then_asks_the_device() {
    let app = Installed::new();
    // The confirmation calls `format_device`, which the window's Erase answers with.
    app.call(
        "format_device",
        vec![Value::text("sda"), Value::text("ext4")],
    );
    let asked = app.sent.asked("format");
    assert_eq!(asked[1], Value::text("sda"));
    assert_eq!(asked[2], Value::text("ext4"));
}

#[test]
fn a_failing_drive_says_so_and_offers_its_eject() {
    let app = Installed::new();
    let window = app.window();
    window.set_view_state("chosen", Value::text("sda"));
    for wanted in ["Eject", "Measure", "Erase…", "Predicted to fail"] {
        assert!(
            has(&window, wanted) || contains(&window, wanted),
            "{wanted} shows: {:?}",
            shown(&window)
        );
    }
}
