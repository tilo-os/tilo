//! Firewall end to end: the window over the device's `firewall` reading, the shared
//! configuration each device applies, and the verbs that hand it to firewalld.

use crate::fixtures::Pane;

use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};

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

fn installed(reading_json: &str) -> Pane {
    let app = Pane::install(&me(), SOURCE)
        .delegating()
        .reading(&reading(), reading_json)
        .rooted("Firewall");
    // Opening the app reads this device's firewalld into an empty configuration.
    let _ = app.try_call("import_device", vec![app.root_value()]);
    app
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
fn applies(app: &Pane) -> usize {
    app.sent
        .names()
        .into_iter()
        .filter(|name| name == "apply")
        .count()
}

#[test]
fn the_window_lists_the_zones_and_their_rules() {
    let app = installed(READING);
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
    let app = installed(READING);
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
    let app = installed(READING);
    app.try_call("import_device", vec![app.root_value()])
        .expect("the configuration is read");
    let window = app.window();
    for wanted in ["Public", "Work", "HTTPS", "Port 3389"] {
        assert!(has(&window, wanted), "{wanted} shows after the import");
    }
}

#[test]
fn adding_a_rule_applies_the_new_configuration_to_the_device() {
    let app = installed(READING);
    app.try_call("import_device", vec![app.root_value()])
        .unwrap();
    app.try_call(
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
    let app = installed(READING);
    app.try_call("import_device", vec![app.root_value()])
        .unwrap();
    let refused = app.try_call(
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
        !app.sent.names().iter().any(|name| name == "apply"),
        "nothing was applied"
    );
}

#[test]
fn an_invalid_source_is_refused_with_its_reason() {
    let app = installed(READING);
    app.try_call("import_device", vec![app.root_value()])
        .unwrap();
    let refused = app.try_call(
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
    let app = installed(READING);
    for name in ["stop", "enter_panic", "delete_rule"] {
        assert_eq!(app.role(name), Some("destructive"), "{name} is destructive");
    }
}

#[test]
fn reading_dropped_traffic_asks_the_device_for_the_last_hour() {
    let app = installed(READING);
    app.try_call("load_blocked", vec![])
        .expect("the read starts");
    let asked = app.sent.asked("read-blocked");
    match &asked[0] {
        Value::Ref(r) => assert_eq!(r.target.as_ref(), &reading(), "the verb is the reading's"),
        other => panic!("the verb's resource is a reference, not {other:?}"),
    }
    assert_eq!(asked[1], Value::Int(60), "the window is an hour");
}

#[test]
fn the_window_shows_what_the_kernel_dropped() {
    let app = installed(READING);
    let window = app.window();
    let words = shown(&window);
    for wanted in ["184", "Drop TCP port 5900", "from 23 addresses"] {
        assert!(has(&window, wanted), "{wanted} shows: {words:?}");
    }
}

#[test]
fn the_services_page_offers_each_service_its_switch() {
    let app = installed(READING);
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
    let app = installed(&refused);
    let window = app.window();
    assert!(
        has(&window, "firewalld refused the configuration"),
        "the window says why the rules are not in force: {:?}",
        shown(&window)
    );
}

#[test]
fn a_computer_without_a_running_firewall_says_so_and_how_to_try_again() {
    let app = installed(MISSING);
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

/// The Delete key deletes the rule it names, and the press asks first: handing the configuration
/// to firewalld changes the device, which undo cannot take back.
#[test]
fn the_delete_key_asks_first_and_the_yes_deletes_the_chosen_rule() {
    let app = installed(READING);
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
        .expect("the Delete key asks");
    assert_eq!(
        applies(&app),
        before,
        "nothing is handed to firewalld before the person's yes"
    );
    // The person answers the confirm: the delete runs on their yes, and the rule is gone.
    let args = app.sent.asked("delete_rule");
    let outcome = app
        .host
        .runtime()
        .invoke_approved_through(&me(), None, (app.action_id("delete_rule"), args), None)
        .expect("the yes deletes");
    assert!(
        matches!(outcome, tilolang_runtime::InvokeOutcome::Done(_)),
        "{outcome:?}"
    );
    assert!(
        applies(&app) > before,
        "the yes hands the rule to firewalld"
    );
    let window = app.window();
    assert!(
        !has(&window, "Port 3389"),
        "the rule is gone: {:?}",
        shown(&window)
    );
}

#[test]
fn a_stored_change_reaches_this_devices_firewall_without_a_press() {
    let app = installed(READING);
    let rule = {
        let zones = app
            .host
            .runtime()
            .read(app.root(), "zones")
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
    app.try_call(
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

/// The nodes of `kind`, in order.
fn of_kind<'a>(node: &'a UiNode, kind: &str, into: &mut Vec<&'a UiNode>) {
    if node.kind == kind {
        into.push(node);
    }
    for child in &node.children {
        of_kind(child, kind, into);
    }
}

/// The first place of the window's list called `label`, as the list drew it.
fn place<'a>(node: &'a UiNode, label: &str) -> Option<&'a UiNode> {
    let mut places = Vec::new();
    of_kind(node, "ui.NavItem", &mut places);
    places
        .into_iter()
        .find(|place| place.props.get("label") == Some(&Value::text(label)))
}

/// Where `word` stands in what the window shows.
fn at(words: &[String], word: &str) -> usize {
    words
        .iter()
        .position(|text| text == word)
        .unwrap_or_else(|| panic!("{word} shows: {words:?}"))
}

/// A computer whose zones come over in the order firewalld keeps them, which the list does not:
/// work before public, and work the active one.
const WORK_FIRST: &str = r#"{
  "serving": true,
  "read_error": null,
  "problem": null,
  "enabled": true,
  "panic_mode": false,
  "default_zone": "work",
  "zones": [
    {
      "kind": "work",
      "target": "default",
      "interfaces": [],
      "services": ["ipp"],
      "rules": [],
      "masquerade": false
    },
    {
      "kind": "public",
      "target": "default",
      "interfaces": ["wlan0"],
      "services": ["ssh"],
      "rules": [],
      "masquerade": false
    }
  ],
  "blocked": [],
  "blocked_total": 0,
  "blocked_window_minutes": 60,
  "blocked_read_at_ms": 0,
  "blocked_error": null,
  "blocked_note": null,
  "pending": []
}"#;

#[test]
fn the_zones_read_least_trusted_first_and_the_active_one_says_so() {
    let app = installed(WORK_FIRST);
    let window = app.window();
    let mut places = Vec::new();
    of_kind(window.tree(), "ui.NavItem", &mut places);
    let labels: Vec<Value> = places
        .iter()
        .filter_map(|place| place.props.get("label").cloned())
        .collect();
    assert_eq!(
        labels,
        vec![Value::text("Public"), Value::text("Work")],
        "the public zone stands before work whatever order firewalld hands them over in"
    );
    let work = place(window.tree(), "Work").expect("the work place");
    assert_eq!(
        work.props.get("secondary"),
        Some(&Value::text("Active")),
        "the active zone's row says so: {:?}",
        work.props
    );
    let public = place(window.tree(), "Public").expect("the public place");
    assert_ne!(
        public.props.get("secondary"),
        Some(&Value::text("Active")),
        "a zone that is not active does not: {:?}",
        public.props
    );
}

#[test]
fn panic_mode_and_a_stopped_firewall_say_what_they_mean() {
    let guarded = WORK_FIRST
        .replace("\"enabled\": true", "\"enabled\": false")
        .replace("\"panic_mode\": false", "\"panic_mode\": true");
    let app = installed(&guarded);
    let window = app.window();
    let words = shown(&window);
    for wanted in [
        "Panic mode is on",
        "All incoming and outgoing network traffic is dropped.",
        "The firewall is stopped",
        "No rules are enforced until you turn it on.",
    ] {
        assert!(has(&window, wanted), "{wanted} shows: {words:?}");
    }
}

#[test]
fn the_rules_are_one_list_sorted_by_any_of_its_columns() {
    let app = installed(READING);
    let window = app.window();
    let words = shown(&window);
    assert!(
        at(&words, "Allow") < at(&words, "Deny"),
        "rules that allow come before ones that deny: {words:?}"
    );
    let mut strips = Vec::new();
    of_kind(window.tree(), "ui.ToggleGroup", &mut strips);
    let sorting = strips
        .into_iter()
        .find(|strip| strip.props.get("caption") == Some(&Value::text("Sort by")))
        .unwrap_or_else(|| panic!("the rule list's sort control: {words:?}"));
    // Sorting by the rule's protocol, then turning the order around: HTTPS (TCP) before
    // Port 3389 (TCP) reads the same, so the proof is the action list turned around below.
    window
        .fire_with(&sorting.key, "change", Some(&Value::Int(0)))
        .expect("the sort is chosen");
    let mut toggles = Vec::new();
    of_kind(window.tree(), "ui.Toggle", &mut toggles);
    let descending = toggles
        .into_iter()
        .find(|toggle| toggle.props.get("label") == Some(&Value::text("Descending")))
        .unwrap_or_else(|| panic!("the sort's direction: {:?}", shown(&window)));
    window
        .fire_with(&descending.key, "check", Some(&Value::Bool(true)))
        .expect("the direction is turned");
    let window = app.window();
    let words = shown(&window);
    assert!(
        at(&words, "Deny") < at(&words, "Allow"),
        "largest first puts the rules that deny first: {words:?}"
    );
}

#[test]
fn choosing_a_zone_in_the_list_opens_its_page() {
    let app = installed(READING);
    let window = app.window();
    let work = place(window.tree(), "Work").expect("the work place");
    window
        .fire(&work.key, "select")
        .expect("the place is chosen");
    let window = app.window();
    let words = shown(&window);
    assert!(
        has(
            &window,
            "For work networks where you mostly trust the other computers."
        ),
        "the page says what work is for: {words:?}"
    );
    assert!(
        has(&window, "Make active zone"),
        "work is not the active zone, so the page offers to make it one: {words:?}"
    );
    assert!(
        !has(&window, "wlan0, eth0"),
        "the public zone's details are not shown on work's page: {words:?}"
    );
}

/// The rule in `zone` that matches `port`, by its id.
fn rule_of(app: &Pane, zone: &str, port: &str) -> String {
    let zones = app
        .host
        .runtime()
        .read(app.root(), "zones")
        .expect("the zones read");
    let Value::List(zones) = zones else {
        panic!("zones are a list: {zones:?}")
    };
    let held = zones
        .iter()
        .find_map(|entry| {
            let Value::Ref(entry) = entry else {
                return None;
            };
            let held = std::rc::Rc::unwrap_or_clone(entry.target.clone());
            let kind = app.host.runtime().read(&held, "kind").expect("kind");
            (kind == Value::text(zone)).then_some(held)
        })
        .unwrap_or_else(|| panic!("the {zone} zone"));
    let rules = app
        .host
        .runtime()
        .read(&held, "rules")
        .expect("the rules read");
    let Value::List(rules) = rules else {
        panic!("rules are a list: {rules:?}")
    };
    rules
        .iter()
        .find_map(|rule| {
            let Value::Ref(rule) = rule else { return None };
            let rule = std::rc::Rc::unwrap_or_clone(rule.target.clone());
            let matched = app.host.runtime().read(&rule, "port").expect("port");
            (matched == Value::text(port)).then_some(rule.id_string())
        })
        .unwrap_or_else(|| panic!("the rule for port {port}"))
}

#[test]
fn a_switch_round_trips_through_the_configuration_to_the_device() {
    let app = installed(READING);
    app.try_call("import_device", vec![app.root_value()])
        .unwrap();
    let rule = rule_of(&app, "public", "3389");
    app.try_call(
        "toggle_rule",
        vec![
            app.root_value(),
            Value::text("public"),
            Value::text(rule),
            Value::Bool(true),
        ],
    )
    .expect("the rule is turned on");
    let asked = app.sent.asked("apply");
    let config = match &asked[asked.len() - 1] {
        Value::Text(text) => text.clone(),
        other => panic!("the configuration is JSON text, not {other:?}"),
    };
    assert!(
        config.contains(r#"{"service":"","port":"3389","proto":"tcp","verdict":"deny","source":"","enabled":true}"#),
        "the switched rule reaches the device: {config}"
    );
    app.try_call(
        "allow_service",
        vec![
            app.root_value(),
            Value::text("public"),
            Value::text("ssh"),
            Value::Bool(false),
        ],
    )
    .expect("ssh stops being allowed");
    let asked = app.sent.last_asked("apply");
    let config = match &asked[asked.len() - 1] {
        Value::Text(text) => text.clone(),
        other => panic!("the configuration is JSON text, not {other:?}"),
    };
    assert!(
        !config.contains(r#""services":["mdns","ssh"]"#),
        "the service the person stopped allowing is no longer let through: {config}"
    );
}

#[test]
fn the_firewalls_controls_are_in_the_window_header() {
    let app = installed(READING);
    let window = app.window();
    let words = shown(&window);
    for wanted in ["Stop", "Panic mode", "Apply", "Add rule", "Read firewalld"] {
        assert!(has(&window, wanted), "{wanted} is offered: {words:?}");
    }
}

#[test]
fn the_switches_say_what_they_turn() {
    let app = installed(READING);
    let window = app.window();
    let mut toggles = Vec::new();
    of_kind(window.tree(), "ui.Toggle", &mut toggles);
    let names: Vec<String> = toggles
        .iter()
        .filter_map(|toggle| match toggle.props.get("accessible_name") {
            Some(Value::Text(name)) => Some(name.clone()),
            _ => None,
        })
        .collect();
    for wanted in [
        "Enable the HTTPS rule",
        "Enable the Port 3389 rule",
        "Allow SSH",
        "Allow HTTPS",
    ] {
        assert!(
            names.iter().any(|name| name == wanted),
            "{wanted} is spoken by its switch: {names:?}"
        );
    }
}

#[test]
fn the_window_says_what_it_did_with_a_rule_and_the_active_zone() {
    let app = installed(READING);
    app.try_call("import_device", vec![app.root_value()])
        .unwrap();
    let _ = app.host.runtime().take_said();
    app.try_call("make_active", vec![app.root_value(), Value::text("work")])
        .expect("work becomes the active zone");
    assert_eq!(
        app.host
            .runtime()
            .read(app.root(), "default_zone")
            .expect("the active zone"),
        Value::text("work"),
        "the configuration names work as the active zone"
    );
    let said = app.host.runtime().take_said();
    assert_eq!(
        said.iter()
            .map(|said| said.text.as_str())
            .collect::<Vec<_>>(),
        ["Made Work the active zone"],
        "{said:?}"
    );
    app.try_call(
        "add_rule",
        vec![
            app.root_value(),
            Value::text("work"),
            Value::text(""),
            Value::Bool(true),
            Value::text("HTTPS"),
            Value::text(""),
            Value::text("tcp"),
            Value::text("allow"),
            Value::text(""),
        ],
    )
    .expect("a rule is added");
    let said = app.host.runtime().take_said();
    assert_eq!(
        said.iter()
            .map(|said| said.text.as_str())
            .collect::<Vec<_>>(),
        ["Added a rule for HTTPS"],
        "{said:?}"
    );
}
