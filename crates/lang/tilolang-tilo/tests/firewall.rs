//! Firewall end to end: the window over the device's `firewall` reading, the shared
//! configuration each device applies, and the verbs that hand it to firewalld.

use std::cell::RefCell;
use std::rc::Rc;

use tilolang::IdsLock;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

const SOURCE: &str = include_str!("../../../../apps/tl/firewall/app.tl");

/// What this device's firewalld publishes: two zones, services and rules, one boot's worth of
/// dropped traffic, and nothing pending.
const READING: &str = r#"{
  "serving": true,
  "read_error": null,
  "problem": null,
  "enabled": true,
  "panic_mode": false,
  "default_zone": "public",
  "zones": [
    {
      "kind": "public",
      "target": "default",
      "interfaces": ["wlan0", "eth0"],
      "services": ["ssh", "mdns"],
      "rules": [
        {"service": "https", "port": "", "proto": "tcp", "verdict": "allow", "source": "192.168.1.0/24", "enabled": true},
        {"service": "", "port": "3389", "proto": "tcp", "verdict": "deny", "source": "", "enabled": false}
      ],
      "masquerade": false
    },
    {
      "kind": "work",
      "target": "reject",
      "interfaces": [],
      "services": ["ipp"],
      "rules": [],
      "masquerade": false
    }
  ],
  "blocked": [
    {"verdict": "DROP", "protocol": "tcp", "port": "5900", "interface": "wlan0",
     "packets": 184, "sources": 23, "last_seen_ms": 1760000000000,
     "buckets": [18, 29, 41, 27, 12, 9, 20, 28, 0, 0, 0, 0], "window_minutes": 60}
  ],
  "blocked_total": 184,
  "blocked_window_minutes": 60,
  "blocked_read_at_ms": 1760000000000,
  "blocked_error": null,
  "blocked_note": null,
  "pending": []
}"#;

/// A computer whose firewalld is not running.
const MISSING: &str = r#"{
  "serving": false,
  "read_error": "firewalld is not running. Start it, then try again.",
  "problem": null,
  "enabled": false,
  "panic_mode": false,
  "default_zone": "public",
  "zones": [],
  "blocked": [],
  "blocked_total": 0,
  "blocked_window_minutes": 0,
  "blocked_read_at_ms": 0,
  "blocked_error": null,
  "blocked_note": null,
  "pending": []
}"#;

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

fn reading() -> ResourceRef {
    ResourceRef::parse_id("firewall", "so.tilo.device#Firewall").expect("a resource id")
}

/// What the app asked of the device's verbs.
#[derive(Clone, Default)]
struct Sent(Rc<RefCell<Vec<Invocation>>>);

impl Sent {
    fn all(&self) -> Vec<Invocation> {
        self.0.borrow().clone()
    }

    /// The last verb `name` was asked on the reading, with its arguments after the resource.
    fn asked(&self, name: &str) -> Vec<Value> {
        self.all()
            .into_iter()
            .rev()
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

struct Installed {
    host: Host,
    root: ResourceRef,
    view: String,
    actions: Vec<(String, String)>,
    roles: Vec<(String, Option<String>)>,
    sent: Sent,
}

impl Installed {
    fn new(reading_json: &str) -> Self {
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
        let roles = package
            .meta
            .actions
            .iter()
            .map(|action| (action.name.clone(), action.role.clone()))
            .collect();
        let root_type = package
            .meta
            .types
            .iter()
            .find(|ty| ty.name == "Firewall")
            .expect("the root type")
            .id
            .clone();
        let sent = Sent::default();
        host.runtime().set_invoker(sent.clone());
        host.runtime()
            .set_reading(&reading(), reading_json)
            .expect("the reading is set");
        let root = host
            .runtime()
            .create_root(&root_type)
            .expect("the app's root");
        let app = Self {
            host,
            root,
            view,
            actions,
            roles,
            sent,
        };
        // Opening the app reads this device's firewalld into an empty configuration.
        let _ = app.call("import_device", vec![app.root_value()]);
        app
    }

    fn role(&self, name: &str) -> Option<&str> {
        self.roles
            .iter()
            .find(|(asked, _)| asked == name)
            .and_then(|(_, role)| role.as_deref())
    }

    fn call(&self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        let id = self
            .actions
            .iter()
            .find(|(asked, _)| asked == name)
            .map_or_else(
                || Err(format!("no action {name}")),
                |(_, id)| Ok(id.clone()),
            )?;
        match self
            .host
            .runtime()
            .invoke_action(&me(), (id.as_str(), args))
            .map_err(|why| why.to_string())?
        {
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
}

/// Every text, title, badge and label under `node`, in order.
fn words(node: &UiNode, into: &mut Vec<String>) {
    for key in [
        "value", "title", "text", "label", "action", "primary", "message",
    ] {
        if let Some(Value::Text(text)) = node.props.get(key) {
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
        || shown(view).iter().any(|text| text.contains(wanted))
}

/// The first node under `node` whose words satisfy `wanted`.
fn find<'a>(node: &'a UiNode, wanted: &dyn Fn(&str) -> bool) -> Option<&'a UiNode> {
    let holds = |node: &UiNode| {
        let mut said = Vec::new();
        words(node, &mut said);
        said.iter().any(|text| wanted(text))
    };
    if holds(node) {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, wanted))
}

/// The nodes that declare the key `key`.
fn with_key<'a>(node: &'a UiNode, key: &str, into: &mut Vec<&'a UiNode>) {
    if node.props.get("key") == Some(&Value::text(key)) {
        into.push(node);
    }
    for child in &node.children {
        with_key(child, key, into);
    }
}

/// How many times the app asked the device to apply a configuration.
fn applies(app: &Installed) -> usize {
    app.sent
        .all()
        .into_iter()
        .filter(|invocation| {
            matches!(
                invocation,
                Invocation::Action { name, .. } if name == "apply"
            )
        })
        .count()
}

#[test]
fn the_window_lists_the_zones_and_their_rules() {
    let app = Installed::new(READING);
    let window = app.window();
    let words = shown(&window);
    for wanted in ["Public", "Work", "HTTPS", "Port 3389", "Allow", "Deny"] {
        assert!(
            words.iter().any(|text| text == wanted),
            "{wanted} shows: {words:?}"
        );
    }
}

#[test]
fn the_zone_page_reads_the_active_zone_and_its_details() {
    let app = Installed::new(READING);
    let window = app.window();
    let words = shown(&window);
    for wanted in [
        "Allowed services",
        "Allow rules",
        "Deny rules",
        "Interfaces",
        "wlan0, eth0",
    ] {
        assert!(has(&window, wanted), "{wanted} shows: {words:?}");
    }
    assert_eq!(
        words.iter().filter(|text| text.as_str() == "2").count(),
        1,
        "two services, two rules and one deny are counted: {words:?}"
    );
}

#[test]
fn opening_brings_in_the_devices_configuration() {
    let app = Installed::new(READING);
    app.call("import_device", vec![app.root_value()])
        .expect("the configuration is read");
    let window = app.window();
    for wanted in ["Public", "Work", "HTTPS", "Port 3389"] {
        assert!(has(&window, wanted), "{wanted} shows after the import");
    }
}

#[test]
fn adding_a_rule_applies_the_new_configuration_to_the_device() {
    let app = Installed::new(READING);
    app.call("import_device", vec![app.root_value()]).unwrap();
    app.call(
        "add_rule",
        vec![
            app.root_value(),
            Value::text("public"),
            Value::text(""),
            Value::Bool(false),
            Value::text(""),
            Value::text("5900"),
            Value::text("tcp"),
            Value::text("deny"),
            Value::text("10.0.0.0/8"),
        ],
    )
    .expect("the rule is added");
    let asked = app.sent.asked("apply");
    let config = match &asked[1] {
        Value::Text(text) => text.clone(),
        other => panic!("the configuration is JSON text, not {other:?}"),
    };
    assert!(
        config.contains(r#""port":"5900""#),
        "the new rule reaches the device: {config}"
    );
    assert!(
        config.contains(r#""source":"10.0.0.0/8""#),
        "its source reaches the device: {config}"
    );
    assert!(
        config.contains(r#""verdict":"deny""#),
        "its verdict reaches the device: {config}"
    );
}

#[test]
fn an_invalid_port_is_refused_with_its_reason() {
    let app = Installed::new(READING);
    app.call("import_device", vec![app.root_value()]).unwrap();
    let refused = app.call(
        "add_rule",
        vec![
            app.root_value(),
            Value::text("public"),
            Value::text(""),
            Value::Bool(false),
            Value::text(""),
            Value::text("70000"),
            Value::text("tcp"),
            Value::text("allow"),
            Value::text(""),
        ],
    );
    let refused = refused.expect_err("a port outside 1 to 65535 is refused");
    assert!(
        refused.contains("not a port or a port range"),
        "the reason names the port: {refused}"
    );
    assert!(
        app.sent.all().iter().all(|invocation| !matches!(
            invocation,
            Invocation::Action { name, .. } if name == "apply"
        )),
        "nothing was applied"
    );
}

#[test]
fn an_invalid_source_is_refused_with_its_reason() {
    let app = Installed::new(READING);
    app.call("import_device", vec![app.root_value()]).unwrap();
    let refused = app.call(
        "add_rule",
        vec![
            app.root_value(),
            Value::text("public"),
            Value::text(""),
            Value::Bool(true),
            Value::text("https"),
            Value::text(""),
            Value::text("tcp"),
            Value::text("allow"),
            Value::text("203.0.113.0/33"),
        ],
    );
    let refused = refused.expect_err("a prefix past /32 is refused");
    assert!(
        refused.contains("not an IPv4 address or network"),
        "the reason names the prefix: {refused}"
    );
}

#[test]
fn the_traffic_cutting_verbs_are_destructive() {
    let app = Installed::new(READING);
    for name in ["stop", "enter_panic", "delete_rule"] {
        assert_eq!(app.role(name), Some("destructive"), "{name} is destructive");
    }
}

#[test]
fn reading_dropped_traffic_asks_the_device_for_the_last_hour() {
    let app = Installed::new(READING);
    app.call("load_blocked", vec![]).expect("the read starts");
    let asked = app.sent.asked("read-blocked");
    match &asked[0] {
        Value::Ref(r) => assert_eq!(r.target.as_ref(), &reading(), "the verb is the reading's"),
        other => panic!("the verb's resource is a reference, not {other:?}"),
    }
    assert_eq!(asked[1], Value::Int(60), "the window is an hour");
}

#[test]
fn the_window_shows_what_the_kernel_dropped() {
    let app = Installed::new(READING);
    let window = app.window();
    let words = shown(&window);
    for wanted in ["184", "Drop TCP port 5900", "from 23 addresses"] {
        assert!(has(&window, wanted), "{wanted} shows: {words:?}");
    }
}

#[test]
fn the_services_page_offers_each_service_its_switch() {
    let app = Installed::new(READING);
    let window = app.window();
    let words = shown(&window);
    for wanted in ["SSH", "HTTPS", "Samba", "Cockpit"] {
        assert!(
            words.iter().any(|text| text == wanted),
            "{wanted} shows: {words:?}"
        );
    }
    assert!(
        words.iter().any(|text| text == "Allowed"),
        "a service the zone allows reads as allowed: {words:?}"
    );
    assert!(
        words.iter().any(|text| text == "Blocked"),
        "one it does not reads as blocked: {words:?}"
    );
}

#[test]
fn a_refused_apply_says_what_the_device_said() {
    let refused = READING.replace(
        "\"problem\": null",
        "\"problem\": \"firewalld refused the configuration: ZONE_CONFLICT\"",
    );
    let app = Installed::new(&refused);
    let window = app.window();
    assert!(
        has(&window, "firewalld refused the configuration"),
        "the window says why the rules are not in force: {:?}",
        shown(&window)
    );
}

#[test]
fn a_computer_without_a_running_firewall_says_so_and_how_to_try_again() {
    let app = Installed::new(MISSING);
    let window = app.window();
    let words = shown(&window);
    assert!(
        words.iter().any(|text| text == "Firewalld isn't available"),
        "{words:?}"
    );
    assert!(
        words
            .iter()
            .any(|text| text == "firewalld is not running. Start it, then try again."),
        "{words:?}"
    );
    assert!(words.iter().any(|text| text == "Try again"), "{words:?}");
}

#[test]
fn the_delete_key_acts_on_the_chosen_rule_and_undo_brings_it_back() {
    let app = Installed::new(READING);
    let window = app.window();
    let mut deleting = Vec::new();
    with_key(window.tree(), "Delete", &mut deleting);
    let chosen = deleting
        .into_iter()
        .find(|node| find(node, &|text| text.contains("3389")).is_some())
        .unwrap_or_else(|| panic!("the 3389 rule's Delete key: {:?}", shown(&window)));
    let before = applies(&app);
    window
        .fire(&chosen.key, "press")
        .expect("the Delete key deletes");
    assert!(
        applies(&app) > before,
        "the rule is handed to firewalld at once, without a dialog"
    );
    let window = app.window();
    assert!(
        !has(&window, "Port 3389"),
        "the rule is gone: {:?}",
        shown(&window)
    );
    app.host
        .store()
        .undo_last(Some(&me()))
        .expect("the change is taken back");
    let window = app.window();
    assert!(
        has(&window, "Port 3389"),
        "undo brings the rule back: {:?}",
        shown(&window)
    );
}

#[test]
fn a_stored_change_reaches_this_devices_firewall_without_a_press() {
    let app = Installed::new(READING);
    let rule = {
        let zones = app
            .host
            .runtime()
            .read(&app.root, "zones")
            .expect("the zones read");
        let Value::List(zones) = zones else {
            panic!("zones are a list: {zones:?}")
        };
        let public = zones
            .iter()
            .find_map(|zone| {
                let Value::Ref(zone) = zone else { return None };
                let held = std::rc::Rc::unwrap_or_clone(zone.target.clone());
                let kind = app.host.runtime().read(&held, "kind").expect("kind");
                if kind == Value::text("public") {
                    Some(held)
                } else {
                    None
                }
            })
            .expect("the public zone");
        let rules = app
            .host
            .runtime()
            .read(&public, "rules")
            .expect("the rules read");
        let Value::List(rules) = rules else {
            panic!("rules are a list: {rules:?}")
        };
        rules
            .iter()
            .find_map(|rule| {
                let Value::Ref(rule) = rule else { return None };
                let held = std::rc::Rc::unwrap_or_clone(rule.target.clone());
                let port = app.host.runtime().read(&held, "port").expect("port");
                (port == Value::text("3389")).then_some(held)
            })
            .expect("the 3389 rule")
    };
    // A change made elsewhere, or an undo here, leaves the workspace's configuration different
    // from what this device holds; the window hands it to this device's firewalld.
    app.call(
        "toggle_rule",
        vec![
            app.root_value(),
            Value::text("public"),
            Value::text(rule.id_string()),
            Value::Bool(true),
        ],
    )
    .expect("the rule is enabled");
    let window = app.window();
    let mut timers = Vec::new();
    with_kind(window.tree(), "ui.Timer", &mut timers);
    assert_eq!(
        timers.len(),
        1,
        "the device does not hold the stored change"
    );
    let before = applies(&app);
    window.fire(&timers[0], "tick").expect("the clock strikes");
    assert!(
        applies(&app) > before,
        "the stored change is applied to this device"
    );
}

/// The keys of the nodes of `kind`, in order.
fn with_kind(node: &UiNode, kind: &str, into: &mut Vec<String>) {
    if node.kind == kind {
        into.push(node.key.clone());
    }
    for child in &node.children {
        with_kind(child, kind, into);
    }
}
