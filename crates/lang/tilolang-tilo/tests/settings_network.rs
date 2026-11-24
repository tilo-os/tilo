//! The Network pane's policy on open networks: the switch in the pane, and what it says of a
//! saved network that asks for no password while this device asks before joining one.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::path::Path;

use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

const APP: &str = "so.tilo.settings";

/// This device's `network` reading: two radios, a network it keeps that asks for no password,
/// and whether it asks before joining one.
const NETWORK: &str = r#"{
  "wifi": {
    "available": true,
    "on": true,
    "hardware-blocked": false,
    "networks": [
      {"name": "Home", "signal": 3, "secure": true, "known": true, "connected": true}
    ]
  },
  "wired": null,
  "mobile": {"available": false, "on": false, "profile": null},
  "airplane-mode": false,
  "connectivity": "full",
  "link": null,
  "saved": [
    {
      "name": "Cafe",
      "id": "uuid-cafe",
      "kind": "wifi",
      "autoconnect": true,
      "last-used": null,
      "priority": 0,
      "metered": "automatic",
      "proxy": null,
      "hidden": false,
      "open": true,
      "ipv4": null
    }
  ],
  "ask-before-open-networks": false,
  "pending": [],
  "problem": null
}"#;

/// [`NETWORK`] as this device reads it while it asks before joining an open network.
fn asking() -> String {
    NETWORK.replace(
        r#""ask-before-open-networks": false"#,
        r#""ask-before-open-networks": true"#,
    )
}

const MODEM: &str = r#"{"available": false, "modems": [], "pending": [], "problem": null}"#;

/// The VPN profiles the Network pane's Connections section counts.
const VPN: &str =
    r#"{"profiles": [], "plugins": [], "generated-key": null, "pending": [], "problem": null}"#;

const TETHERING: &str =
    r#"{"available": false, "hotspot": false, "band": "automatic", "clients": []}"#;

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// What the pane asked of the device.
#[derive(Clone, Default)]
struct Sent(std::rc::Rc<std::cell::RefCell<Vec<Invocation>>>);

impl Sent {
    /// The arguments of the last `name` asked, by name, as the device's action API takes them:
    /// the resource it is asked on and the options it leaves out are not among them.
    fn asked(&self, name: &str) -> serde_json::Map<String, serde_json::Value> {
        self.0
            .borrow()
            .iter()
            .rev()
            .find_map(|invocation| match invocation {
                Invocation::Action {
                    app, name: asked, ..
                } if asked == name => {
                    assert_eq!(app.as_str(), "so.tilo.device", "the device's own verb");
                    Some(invocation.api_arguments())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("the pane asks `{name}`"))
    }
}

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

struct Pane {
    host: Host,
    view: String,
    sent: Sent,
}

impl Pane {
    /// The Network pane open on a device that reads `network`.
    fn new(network: &str) -> Self {
        Self::pane("network", network, MODEM)
    }

    /// The window open on the page `name`, on a device that reads `network` and `modem`.
    fn pane(name: &str, network: &str, modem: &str) -> Self {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/settings");
        let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
        let ids = tilolang::IdsLock::parse(
            &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
        )
        .expect("its ids parse");
        let metas = tilolang_tilo::platform_apps::platform_packages()
            .expect("the platform's packages read");
        let host = Host::new(me(), || None).expect("a host");
        // The device's own resources are the platform's packages this package names, loaded
        // here as a window's process loads them: what a read of `device.network` answers from.
        for meta in tilolang_tilo::platform_apps::named_platform_packages_of(&source)
            .expect("the platform's packages read")
        {
            host.runtime()
                .load_foreign(meta)
                .expect("the device is loaded");
        }
        let compiled =
            tilolang::compile_package_source(&source, &ids, &metas).expect("the package compiles");
        host.load_compiled(&compiled).expect("the package loads");
        host.root_of(APP).expect("the app's root");
        let _ = name;
        let view = compiled
            .meta
            .views
            .iter()
            .find(|view| view.name == "network_pane")
            .expect("the Network pane is declared")
            .id
            .clone();
        let sent = Sent::default();
        host.runtime().set_foreign_invoker(sent.clone());
        host.runtime()
            .set_reading(&reading("network", "Network"), network)
            .expect("the network reading is set");
        host.runtime()
            .set_reading(&reading("modem", "Modem"), modem)
            .expect("the modem reading is set");
        host.runtime()
            .set_reading(&reading("tethering", "Tethering"), TETHERING)
            .expect("the tethering reading is set");
        host.runtime()
            .set_reading(&reading("vpn", "Vpn"), VPN)
            .expect("the vpn reading is set");
        Self { host, view, sent }
    }

    fn open(&self) -> ViewInstance {
        let mut view = self
            .host
            .runtime()
            .open_view(&me(), &self.view, Vec::new())
            .expect("the pane opens");
        view.poll_patches().expect("the pane draws");
        view
    }

    /// Show the reading `network` again, as the device publishes it after a change.
    fn reads(&self, network: &str) {
        self.host
            .runtime()
            .set_reading(&reading("network", "Network"), network)
            .expect("the network reading is set");
    }
}

fn reading(resource: &str, name: &str) -> ResourceRef {
    ResourceRef::parse_id(resource, &format!("so.tilo.device#{name}")).expect("a resource id")
}

/// Every text the node carries, whatever it is called, under `node` in order.
fn words(node: &UiNode, into: &mut Vec<String>) {
    into.extend(node.props.values().filter_map(|value| match value {
        Value::Text(text) => Some(text.clone()),
        _ => None,
    }));
    for child in &node.children {
        words(child, into);
    }
}

fn shown(view: &ViewInstance) -> Vec<String> {
    let mut out = Vec::new();
    words(view.tree(), &mut out);
    out
}

/// The key of the button the pane labels `label`.
fn button(tree: &UiNode, label: &str) -> String {
    tree.of_kind("ui.Button")
        .into_iter()
        .find(|node| node.props.get("label") == Some(&Value::text(label)))
        .unwrap_or_else(|| panic!("a button labelled {label}"))
        .key
        .clone()
}

/// The row the pane titles `title`.
fn row(tree: &UiNode, title: &str) -> String {
    tree.of_kind("ui.ListRow")
        .into_iter()
        .find(|node| node.props.get("title") == Some(&Value::text(title)))
        .unwrap_or_else(|| panic!("a row titled {title}"))
        .key
        .clone()
}

/// Press the row `title`, which opens what it shows.
fn open_row(view: &mut ViewInstance, title: &str) {
    let key = row(view.tree(), title);
    view.fire(&key, "activate")
        .unwrap_or_else(|why| panic!("the {title} row opens what it shows: {why}"));
    view.poll_patches().expect("what it shows draws");
}

/// This device joins the network it keeps by itself; pressing the switch asks it to ask first,
/// on the network resource the device publishes.
#[test]
fn the_pane_asks_before_joining_an_open_network() {
    let pane = Pane::new(NETWORK);
    let mut view = pane.open();
    let words = shown(&view);
    assert!(
        words
            .iter()
            .any(|text| text == "Joins the ones it keeps by itself"),
        "the pane says what it does now: {words:?}, reads {:?}",
        view.foreign_reads()
            .iter()
            .map(ResourceRef::id_string)
            .collect::<Vec<_>>()
    );
    view.press(&button(view.tree(), "Join by itself"))
        .expect("the switch is pressed");
    assert_eq!(
        pane.sent.asked("set-ask-before-open-networks")["on"],
        serde_json::json!(true),
        "the pane asks the device to ask first"
    );

    // The device now asks: the pane shows it, without the window being reopened.
    pane.reads(&asking());
    view.poll_patches().expect("the pane draws the change");
    let words = shown(&view);
    assert!(
        words
            .iter()
            .any(|text| text == "Asks before joining one this device keeps"),
        "the pane says this device asks first: {words:?}"
    );
    assert!(
        view.tree()
            .of_kind("ui.Button")
            .iter()
            .any(|node| node.props.get("label") == Some(&Value::text("Ask first"))),
        "the switch reads the state: {words:?}"
    );
}

/// A network this device keeps that asks for no password waits for the person while this device
/// asks first: it says so, and offers no way to make it join by itself.
#[test]
fn an_open_network_this_device_keeps_waits_for_the_person() {
    let pane = Pane::new(&asking());
    let mut view = pane.open();
    open_row(&mut view, "Known networks");
    let words = shown(&view);
    assert!(
        words.iter().any(|text| text == "Asks first"),
        "the open network waits for the person: {words:?}"
    );
    assert!(
        !words
            .iter()
            .any(|text| text == "Stop joining by itself" || text == "Join by itself"),
        "it offers no way to make it join by itself: {words:?}"
    );

    // With the policy off, this device's own setting stands and the person changes it again.
    let pane = Pane::new(NETWORK);
    let mut view = pane.open();
    open_row(&mut view, "Known networks");
    let words = shown(&view);
    assert!(
        words.iter().any(|text| text == "Joins by itself"),
        "it joins as its own setting says: {words:?}"
    );
    assert!(
        words.iter().any(|text| text == "Stop joining by itself"),
        "and the person may stop it: {words:?}"
    );
}
