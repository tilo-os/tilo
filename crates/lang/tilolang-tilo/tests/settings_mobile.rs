//! The Mobile data pane: the operators a modem reaches and choosing one, and the codes an
//! operator answers, each through the modem resource's own actions.

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

const TETHERING: &str = r#"{"available": false, "hotspot": false, "name": null, "band": "automatic", "clients": [], "pending": [], "problem": null}"#;

/// The VPN profiles the Network page's Connections section counts.
const VPN: &str =
    r#"{"profiles": [], "plugins": [], "generated-key": null, "pending": [], "problem": null}"#;

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

fn reading(resource: &str, name: &str) -> ResourceRef {
    ResourceRef::parse_id(resource, &format!("so.tilo.device#{name}")).expect("a resource id")
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
    /// The Mobile data pane open on a device that reads `network` and `modem`.
    fn new(network: &str, modem: &str) -> Self {
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

    /// The Network pane, on its Mobile data page.
    fn open(&self) -> ViewInstance {
        let mut view = self
            .host
            .runtime()
            .open_view(&me(), &self.view, Vec::new())
            .expect("the pane opens");
        view.poll_patches().expect("the pane draws");
        // The Mobile data row is titled by the modem this device has.
        let key = row(view.tree(), "Phone modem");
        view.fire(&key, "activate")
            .expect("the Mobile data row opens its page");
        view.poll_patches().expect("the page draws");
        view
    }

    /// Show the reading `modem` again, as the device publishes it after a change.
    fn reads(&self, modem: &str) {
        self.host
            .runtime()
            .set_reading(&reading("modem", "Modem"), modem)
            .expect("the modem reading is set");
    }
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

/// The key of the row the pane titles `title`.
fn row(tree: &UiNode, title: &str) -> String {
    tree.of_kind("ui.ListRow")
        .into_iter()
        .find(|node| node.props.get("title") == Some(&Value::text(title)))
        .unwrap_or_else(|| panic!("a row titled {title}"))
        .key
        .clone()
}

/// The code's field, as the pane lays it out.
fn code_field(view: &ViewInstance) -> String {
    view.tree()
        .of_kind("ui.TextInput")
        .into_iter()
        .find(|node| node.props.get("placeholder") == Some(&Value::text("A code, such as *100#")))
        .unwrap_or_else(|| panic!("the code's field: {:?}", shown(view)))
        .key
        .clone()
}

/// This device's mobile data, with a modem that reaches two operators.
const MOBILE: &str = r#"{
  "wifi": {"available": false, "on": false, "hardware-blocked": false, "networks": []},
  "wired": null,
  "mobile": {
    "available": true,
    "on": true,
    "profile": {
      "id": "uuid-mobile",
      "name": "Mobile data",
      "apn": null,
      "roaming": false,
      "metered": "automatic",
      "autoconnect": true
    }
  },
  "airplane-mode": false,
  "connectivity": "full",
  "link": null,
  "saved": [],
  "ask-before-open-networks": true,
  "pending": [],
  "problem": null
}"#;

/// The modem, its SIM, and the operators a scan found.
const MODEM_SCANNED: &str = r#"{
  "available": true,
  "modems": [
    {
      "path": "/mm/modem0",
      "name": "Phone modem",
      "state": "registered",
      "failure": null,
      "lock": "none",
      "retries": {"pin": null, "puk": null},
      "pin-enabled": false,
      "signal": 60,
      "technology": "lte",
      "operator": "Stadtnetz",
      "operators": [
        {"code": "26201", "name": "Stadtnetz", "state": "current"},
        {"code": "26202", "name": "Feldnetz", "state": "available"}
      ],
      "code": null,
      "registration": "home",
      "network-type": "5g-4g-3g",
      "network-types": ["5g-4g-3g"],
      "sims": [
        {
          "path": "/mm/sim0",
          "slot": 1,
          "active": true,
          "operator": "Stadtnetz",
          "label": "Stadtnetz",
          "kind": "physical"
        }
      ],
      "numbers": [],
      "received": 0,
      "sent": 0
    }
  ],
  "pending": [],
  "problem": null
}"#;

/// [`MODEM_SCANNED`] with what the network answered a code, which waits for the person.
fn modem_asking() -> String {
    MODEM_SCANNED.replace(
        r#""code": null"#,
        r#""code": {"text": "1 balance, 2 bundle", "awaiting": true}"#,
    )
}

/// A modem's operators are listed with the one in use, and choosing one by hand, or letting the
/// modem choose, is the modem resource's own actions.
#[test]
fn the_mobile_pane_lists_the_operators_and_chooses_one() {
    let pane = Pane::new(MOBILE, MODEM_SCANNED);
    let mut view = pane.open();
    let words = shown(&view);
    for wanted in [
        "The operator",
        "Stadtnetz",
        "Feldnetz",
        "In use",
        "In reach",
    ] {
        assert!(
            words.iter().any(|text| text == wanted),
            "{wanted} shows: {words:?}"
        );
    }
    let feldnetz = row(view.tree(), "Feldnetz");
    view.fire(&feldnetz, "activate")
        .expect("the operator is chosen by its row");
    assert_eq!(
        pane.sent.asked("select-operator"),
        serde_json::json!({"modem": "/mm/modem0", "operator": "26202"})
            .as_object()
            .expect("an object")
            .clone(),
        "the modem stands on the operator whose row was pressed"
    );

    view.press(&button(view.tree(), "Choose automatically"))
        .expect("the rest is chosen for it");
    assert_eq!(
        pane.sent.asked("select-operator"),
        serde_json::json!({"modem": "/mm/modem0"})
            .as_object()
            .expect("an object")
            .clone(),
        "and the modem chooses when no operator is named"
    );

    view.press(&button(view.tree(), "Look for operators"))
        .expect("the operators are looked for again");
    assert_eq!(
        pane.sent.asked("scan-operators"),
        serde_json::json!({"modem": "/mm/modem0"})
            .as_object()
            .expect("an object")
            .clone()
    );
}

/// A code is sent to the operator, its answer shown, the question it asks answered, and the
/// session ended: each through the modem resource's action.
#[test]
fn the_mobile_pane_sends_a_code_and_shows_what_the_network_answers() {
    let pane = Pane::new(MOBILE, MODEM_SCANNED);
    let mut view = pane.open();
    view.edit(&code_field(&view), &Value::text("*100#"))
        .expect("the code is typed");
    view.press(&button(view.tree(), "Send the code"))
        .expect("the code is sent");
    assert_eq!(
        pane.sent.asked("send-code"),
        serde_json::json!({"modem": "/mm/modem0", "code": "*100#"})
            .as_object()
            .expect("an object")
            .clone()
    );

    // The network asked what the person wants next.
    pane.reads(&modem_asking());
    view.poll_patches().expect("the pane draws the answer");
    let words = shown(&view);
    assert!(
        words.iter().any(|text| text == "1 balance, 2 bundle"),
        "the network's answer shows: {words:?}"
    );
    let answer = view
        .tree()
        .of_kind("ui.TextInput")
        .into_iter()
        .find(|node| node.props.get("placeholder") == Some(&Value::text("The answer it asks for")))
        .expect("the answer's field")
        .key
        .clone();
    view.edit(&answer, &Value::text("2"))
        .expect("the answer is typed");
    view.press(&button(view.tree(), "Send the answer"))
        .expect("the answer is sent");
    assert_eq!(
        pane.sent.asked("answer-code"),
        serde_json::json!({"modem": "/mm/modem0", "answer": "2"})
            .as_object()
            .expect("an object")
            .clone()
    );

    view.press(&button(view.tree(), "End the session"))
        .expect("the session ends");
    assert_eq!(
        pane.sent.asked("cancel-code"),
        serde_json::json!({"modem": "/mm/modem0"})
            .as_object()
            .expect("an object")
            .clone()
    );
}
