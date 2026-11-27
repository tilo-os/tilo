//! The Network pane's policy on open networks: the switch in the pane, and what it says of a
//! saved network that asks for no password while this device asks before joining one.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::path::Path;

use tilolang_runtime::delegate::Invocation;
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

const APP: &str = "so.tilo.settings";

use crate::fixtures::invoker::{Answered as _, Sent};

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
      "profile": "uuid-cafe",
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

struct Pane {
    host: Host,
    view: String,
    sent: Sent,
}

impl Pane {
    /// The Network pane open on a device that reads `network`.
    fn new(network: &str) -> Self {
        Self::pane_of("network", network, MODEM)
    }

    /// The window open on the page `name`, on a device that reads `network` and `modem`.
    fn pane_of(name: &str, network: &str, modem: &str) -> Self {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/settings");
        let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
        let ids = tilolang::IdsLock::parse(
            &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
        )
        .expect("its ids parse");
        let metas = crate::fixtures::settings_packages().expect("the platform's packages read");
        let host = Host::new(me(), || None).expect("a host");
        // The device's own resources are the platform's packages this package names, loaded
        // here as a window's process loads them: what a read of `device.network` answers from.
        for meta in tilolang_tilo::platform::named_platform_packages_of(&source)
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
    ResourceRef::parse_id(resource, format!("so.tilo.device#{name}")).expect("a resource id")
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
    view.press_answering(&pane.host, &me(), &button(view.tree(), "Join by itself"))
        .expect("the switch is pressed");
    assert_eq!(
        pane.sent
            .api_arguments("so.tilo.device", "set-ask-before-open-networks")["on"],
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

/// This device's `network` reading: a work or school network it keeps and is joined to, with the
/// identity and certificate it joins it as, and a cafe it keeps that asks for no password.
const KEPT: &str = r#"{
  "wifi": {
    "available": true,
    "on": true,
    "hardware-blocked": false,
    "networks": [
      {"name": "Home", "signal": 3, "secure": true, "known": true, "connected": true},
      {"name": "Cafe", "signal": 2, "secure": false, "known": true, "connected": false}
    ]
  },
  "wired": null,
  "mobile": {"available": false, "on": false, "profile": null},
  "airplane-mode": false,
  "connectivity": "full",
  "link": null,
  "saved": [
    {
      "$address": "saved-network:uuid-home",
      "name": "Home",
      "profile": "uuid-home",
      "kind": "wifi",
      "autoconnect": true,
      "last-used": null,
      "priority": 0,
      "metered": "automatic",
      "proxy": null,
      "hidden": false,
      "open": false,
      "ipv4": null,
      "wake": null,
      "enterprise": {
        "identity": "ada@campus.example",
        "ca-certificate": "/etc/pki/campus.pem"
      }
    },
    {
      "$address": "saved-network:uuid-cafe",
      "name": "Cafe",
      "profile": "uuid-cafe",
      "kind": "wifi",
      "autoconnect": true,
      "last-used": null,
      "priority": 0,
      "metered": "automatic",
      "proxy": null,
      "hidden": false,
      "open": true,
      "ipv4": null,
      "wake": null,
      "enterprise": null
    }
  ],
  "ask-before-open-networks": false,
  "pending": [],
  "problem": null
}"#;

/// A work or school network this device keeps says who it knows the device as, and its page
/// changes that identity and the certificate it is checked with together: the certificate the
/// device already has travels with the identity typed, so changing one never drops the other.
#[test]
fn a_kept_work_network_is_edited_with_its_identity_and_certificate() {
    let pane = Pane::new(KEPT);
    let mut view = pane.open();
    open_row(&mut view, "Home");
    let words = shown(&view);
    assert!(
        words.iter().any(|text| text == "Work or school identity"),
        "the page says who the network knows this device as: {words:?}"
    );
    assert!(
        words.iter().any(|text| text == "ada@campus.example"),
        "and which identity it joins as: {words:?}"
    );

    let field = view
        .tree()
        .of_kind("ui.TextInput")
        .into_iter()
        .find(|node| {
            node.props.get("placeholder") == Some(&Value::text("The identity the network knows"))
        })
        .expect("the identity field")
        .key
        .clone();
    view.edit(&field, &Value::text("ada@lab.example"))
        .expect("the identity typed");
    view.press_answering(&pane.host, &me(), &button(view.tree(), "Apply identity"))
        .expect("the identity is applied");
    let asked = pane
        .sent
        .api_arguments("so.tilo.device", "set-enterprise-identity");
    assert_eq!(asked["profile"], serde_json::json!("uuid-home"));
    assert_eq!(asked["identity"], serde_json::json!("ada@lab.example"));
    assert_eq!(
        asked["ca_certificate"],
        serde_json::json!("/etc/pki/campus.pem"),
        "the certificate the device keeps stays with the identity typed: {asked:?}"
    );
}

/// A Wi-Fi network this device keeps hands its password to the person's other devices through the
/// kept network's own `share-with-devices`, and a network that asks for no password has none to
/// share, so its page offers none.
#[test]
fn a_kept_wifi_network_shares_its_password_with_the_persons_devices() {
    let pane = Pane::new(KEPT);
    let mut view = pane.open();
    open_row(&mut view, "Known networks");
    open_row(&mut view, "Home");
    assert!(
        shown(&view)
            .iter()
            .any(|text| text == "Share with my devices"),
        "a network kept with a password can be shared: {:?}",
        shown(&view)
    );
    view.press_answering(&pane.host, &me(), &button(view.tree(), "Share"))
        .expect("the network is shared");
    let sent = pane.sent.all();
    let Invocation::Action {
        app, name, target, ..
    } = sent.last().expect("the pane asks the device to share it")
    else {
        panic!("a verb of the device: {:?}", sent.last());
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.device", "share-with-devices"),
        "the kept network's own verb, as the device publishes it"
    );
    assert_eq!(
        target.as_ref().map(ResourceRef::id_string).as_deref(),
        Some("saved-network:uuid-home"),
        "asked on the network kept, not on the radio"
    );
    drop(sent);

    // The devices took the password, and the row says so rather than asking again.
    view.poll_patches().expect("the pane draws the change");
    assert!(
        shown(&view)
            .iter()
            .any(|text| text == "Shared with your other devices"),
        "the row says the password was shared: {:?}",
        shown(&view)
    );

    // The cafe asks for no password: there is nothing to hand to another device.
    open_row(&mut view, "Back");
    open_row(&mut view, "Known networks");
    open_row(&mut view, "Cafe");
    let words = shown(&view);
    assert!(
        words.iter().any(|text| text == "What this device keeps"),
        "the cafe's page shows what is kept of it: {words:?}"
    );
    assert!(
        !words.iter().any(|text| text == "Share with my devices"),
        "an open network has no password to share: {words:?}"
    );
}
