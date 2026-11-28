//! Settings' VPN section: the profiles' honest transitions, their traffic and last handshake in
//! Activity, Tor routing, and adding a profile through the device's own action.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use tilolang_runtime::delegate::Invocation;
use tilolang_runtime::{Actor, Value};

use crate::fixtures::invoker::{Answered as _, Sent};
use crate::fixtures::{control_under, labelled, texts};
use crate::settings_panes::{invoke, open_window, settings};

/// The person at the settings window.
fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// This device's VPN profiles as its network service publishes them: one connected and carrying
/// traffic, one coming up, and one down after a failed attempt, routed over Tor.
const VPN: &str = r#"{
  "profiles": [
    {
      "id": "uuid-home",
      "name": "Home",
      "kind": "wireguard",
      "connected": true,
      "state": "connected",
      "since": 1700000000000,
      "notice": null,
      "received": 6710886,
      "sent": 2500000,
      "received_rate": 6710886.0,
      "sent_rate": 1200.0,
      "handshake": 34,
      "tor": false,
      "autoconnect": true,
      "all_traffic": true,
      "addresses": ["10.77.0.2/32"],
      "dns": [],
      "mtu": null,
      "endpoint": "home.example:51820",
      "allowed_ips": ["0.0.0.0/0"],
      "peer-public-key": "kP8x2yQ1mZ7vR3nL9fH0sJtW6cB4dE5gA1iU2oXyz0=",
      "keepalive": null
    },
    {
      "id": "uuid-office",
      "name": "Office",
      "kind": "vpn",
      "connected": false,
      "state": "connecting",
      "since": 1700000005000,
      "notice": null,
      "received": 0,
      "sent": 0,
      "received_rate": 0.0,
      "sent_rate": 0.0,
      "handshake": null,
      "tor": true,
      "autoconnect": false,
      "all_traffic": false,
      "addresses": [],
      "dns": [],
      "mtu": null,
      "endpoint": "vpn.office.example:1194",
      "allowed_ips": ["10.20.0.0/16"],
      "peer-public-key": null,
      "keepalive": null
    },
    {
      "id": "uuid-relay",
      "name": "Relay",
      "kind": "wireguard",
      "connected": false,
      "state": "down",
      "since": 1700000009000,
      "notice": "The tunnel is down on this device",
      "received": 0,
      "sent": 0,
      "received_rate": 0.0,
      "sent_rate": 0.0,
      "handshake": null,
      "tor": true,
      "autoconnect": false,
      "all_traffic": false,
      "addresses": [],
      "dns": [],
      "mtu": null,
      "endpoint": "relay.example:51820",
      "allowed_ips": [],
      "peer-public-key": null,
      "keepalive": null
    }
  ],
  "plugins": [],
  "generated-key": null,
  "pending": [],
  "problem": null,
  "tor": false,
  "tor-available": true
}"#;

/// The VPN section over [`VPN`], reached from the Network pane whose Connections list leads to it.
fn vpn_section() -> (tilolang_tilo::host::Host, tilolang_runtime::ViewInstance) {
    let (host, _) = settings();
    crate::fixtures::set_reading(
        &host,
        "network",
        &serde_json::json!({
            "airplane_mode": false,
            "connectivity": "full",
            "portal_uri": null,
            "portal_problem": null,
            "wifi": {"available": true, "on": true, "hardware_blocked": false, "networks": []},
            "ask_before_open_networks": false,
            "wired": null,
            "saved": [],
            "mobile": {"available": false, "on": false, "profile": null},
            "problem": null,
        }),
    );
    crate::fixtures::set_reading(
        &host,
        "tethering",
        &serde_json::json!({"available": false, "hotspot": false, "name": null, "clients": []}),
    );
    crate::fixtures::set_reading(
        &host,
        "vpn",
        &serde_json::from_str(VPN).expect("the reading reads"),
    );
    invoke(&host, "open_pane", vec![Value::text("network")]);
    let mut view = open_window(&host);
    view.fire_answering(
        &host,
        &me(),
        &view
            .tree()
            .of_kind("ui.ListRow")
            .into_iter()
            .find(|node| node.props.get("title") == Some(&Value::text("VPN")))
            .expect("the row the Connections list leads to the VPN by")
            .key
            .clone(),
        "activate",
    )
    .expect("the VPN section opens");
    view.poll_patches().expect("the section draws");
    (host, view)
}

/// The row titled `title` in a view.
fn row_key(view: &tilolang_runtime::ViewInstance, title: &str) -> String {
    view.tree()
        .of_kind("ui.ListRow")
        .into_iter()
        .find(|node| node.props.get("title") == Some(&Value::text(title)))
        .unwrap_or_else(|| {
            panic!(
                "a row titled {title}; the view drew {}",
                crate::fixtures::drawn(view.tree())
            )
        })
        .key
        .clone()
}

/// Every profile says where it stands, a tunnel coming up is not pressed again, and a failed
/// attempt is said on its own row until the person dismisses it.
#[test]
fn a_vpn_profile_says_where_it_stands_and_what_went_wrong() {
    let (host, view) = vpn_section();
    let shown = texts(&view);
    for words in [
        "Connecting…",
        "Could not connect",
        "The tunnel is down on this device",
    ] {
        assert!(
            shown.iter().any(|text| text == words),
            "{words} shows in the VPN section: {shown:?}"
        );
    }
    assert!(
        shown.iter().any(|text| text.starts_with("Connected")),
        "the connected profile says so: {shown:?}"
    );
    assert!(
        shown.iter().any(|text| text.starts_with("Down")),
        "the profile that is down says so: {shown:?}"
    );
    assert!(
        shown.iter().any(|text| text == "Connected · home.example"),
        "the row names the server without its port: {shown:?}"
    );
    assert!(
        !shown
            .iter()
            .any(|text| text == "Connected · home.example:51820"),
        "the port is not in the row: {shown:?}"
    );

    // The tunnel coming up offers no second press.
    let office = view
        .tree()
        .of_kind("ui.ListRow")
        .into_iter()
        .find(|node| node.props.get("title") == Some(&Value::text("Office")))
        .expect("the Office row");
    let office_words: Vec<String> = crate::fixtures::nodes(office)
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !office_words
            .iter()
            .any(|word| word == "On" || word == "Off"),
        "a connecting tunnel offers no press that would start it again: {office_words:?}"
    );

    // The failure is dismissed on the row it is said on.
    let sent = Sent::new();
    host.runtime().set_invoker(sent.clone());
    let dismiss = labelled(view.tree(), "ui.Button", "Dismiss");
    view.press_answering(&host, &me(), &dismiss)
        .expect("the notice is dismissed");
    let Invocation::Action {
        app, name, target, ..
    } = sent.last()
    else {
        panic!("the row's own verb: {:?}", sent.last());
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.device", "dismiss"),
        "the profile's own verb, as the device publishes it"
    );
    assert_eq!(
        (
            target.as_ref().map(|held| held.doc.as_str()),
            target.as_ref().map(|held| held.ty.as_str())
        ),
        (Some("uuid-relay"), Some("so.tilo.device#VpnProfile")),
        "the profile's own row, as the device publishes it"
    );
}

/// Activity shows what each connected tunnel carried, how fast, and how long ago it last
/// handshook; the switcher is part of the section.
#[test]
fn the_activity_view_shows_traffic_and_the_last_handshake() {
    let (host, mut view) = vpn_section();
    let activity = labelled(view.tree(), "ui.Segment", "Activity");
    view.fire_answering(&host, &me(), &activity, "press")
        .expect("Activity is shown");
    view.poll_patches().expect("the view draws");
    let shown = texts(&view);
    for words in [
        "Received",
        "6.7 MB/s · 6.7 MB",
        "Sent",
        "1.2 kB/s · 2.5 MB",
        "Last handshake",
        "34 s ago",
    ] {
        assert!(
            shown.iter().any(|text| text == words),
            "{words} shows in Activity: {shown:?}"
        );
    }
}

/// Tor routing is this device's own action, and the profile's preference is its own edit.
#[test]
fn tor_routing_is_offered_where_the_device_has_it() {
    let (host, mut view) = vpn_section();
    let sent = Sent::new();
    host.runtime().set_invoker(sent.clone());

    // Every connection, from the section's own row.
    let watched = view
        .tree()
        .of_kind("ui.ListRow")
        .into_iter()
        .find(|node| node.props.get("title") == Some(&Value::text("Every connection through Tor")))
        .expect("the Tor row")
        .key
        .clone();
    let _ = watched;
    view.fire_with_answering(
        &host,
        &me(),
        &control_under(view.tree(), "Every connection through Tor", "ui.Toggle"),
        "check",
        Some(&Value::Bool(true)),
    )
    .expect("every connection goes over Tor");
    let asked = sent.last_args_on("so.tilo.device", "set-tor");
    assert_eq!(
        asked
            .iter()
            .find(|(named, _)| named == "on")
            .map(|(_, value)| value.clone()),
        Some(Value::Opt(Some(Box::new(Value::Bool(true))))),
        "the device is asked to route everything over Tor: {asked:?}"
    );

    // One profile's preference, from its page.
    view.fire_answering(&host, &me(), &row_key(&view, "Home"), "activate")
        .expect("the profile's page opens");
    view.poll_patches().expect("the page draws");
    let shown = texts(&view);
    for words in [
        "On this device",
        "This device's profile",
        "uuid-home",
        "Route it over Tor",
    ] {
        assert!(
            shown.iter().any(|text| text == words),
            "{words} shows on the profile's page: {shown:?}"
        );
    }
    view.fire_with_answering(
        &host,
        &me(),
        &control_under(view.tree(), "Route it over Tor", "ui.Toggle"),
        "check",
        Some(&Value::Bool(true)),
    )
    .expect("the profile is routed over Tor");
    let asked = sent.last_args_on("so.tilo.device", "edit");
    assert!(
        asked.iter().any(|(named, value)| named == "tor"
            && *value == Value::Opt(Some(Box::new(Value::Bool(true))))),
        "the profile is edited to route over Tor: {asked:?}"
    );
}

/// A pasted configuration is set up through the device's own import, its secrets never kept by
/// the page.
#[test]
fn a_pasted_configuration_is_set_up_through_the_device() {
    let (host, mut view) = vpn_section();
    let sent = Sent::new();
    host.runtime().set_invoker(sent.clone());
    view.press_answering(
        &host,
        &me(),
        &labelled(view.tree(), "ui.Button", "Add a VPN"),
    )
    .expect("the ways to add one open");
    view.poll_patches().expect("the page draws");
    view.fire_answering(
        &host,
        &me(),
        &row_key(&view, "Paste a configuration"),
        "activate",
    )
    .expect("the paste page opens");
    view.poll_patches().expect("the page draws");

    let name = view
        .tree()
        .of_kind("ui.TextInput")
        .into_iter()
        .find(|node| node.props.get("placeholder") == Some(&Value::text("Name")))
        .expect("the name field")
        .key
        .clone();
    view.edit(&name, &Value::text("Home"))
        .expect("the name is typed");
    let config = view
        .tree()
        .of_kind("ui.TextInput")
        .into_iter()
        .find(|node| node.props.get("placeholder") == Some(&Value::text("The configuration")))
        .expect("the configuration field")
        .key
        .clone();
    view.edit(
        &config,
        &Value::text("[Interface]\nPrivateKey = aGVsbG8=\n"),
    )
    .expect("the configuration is pasted");
    // The configuration typed edits the page's own state; only setting it up asks the device.
    sent.take();
    view.press_answering(
        &host,
        &me(),
        &labelled(view.tree(), "ui.Button", "Set it up"),
    )
    .expect("the profile is set up");
    let Invocation::Action {
        app, name, args, ..
    } = sent.last()
    else {
        panic!("the device's own import: {:?}", sent.last());
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.device", "import-vpn"),
        "the device's own verb, as its surface publishes it"
    );
    assert_eq!(
        args.iter()
            .find(|(named, _)| named == "kind")
            .map(|(_, value)| value.clone()),
        Some(Value::text("wireguard"))
    );
    assert_eq!(
        args.iter()
            .find(|(named, _)| named == "name")
            .map(|(_, value)| value.clone()),
        Some(Value::text("Home"))
    );
    assert!(
        args.iter().any(|(named, value)| named == "text"
            && *value == Value::text("[Interface]\nPrivateKey = aGVsbG8=\n")),
        "the pasted configuration goes to the device, which keeps its secrets: {args:?}"
    );
}

/// The VPN section is found by the words people know it by.
#[test]
fn the_vpn_words_are_settings_own() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/settings");
    let compiled = {
        let (_, compiled) = settings();
        compiled
    };
    let fleet: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("fleet.json")).expect("the fleet entry"),
    )
    .expect("the fleet entry reads");
    let words: Vec<String> = fleet["keywords"]
        .as_array()
        .expect("the fleet entry's words")
        .iter()
        .filter_map(|word| word.as_str().map(str::to_owned))
        .collect();
    for word in ["vpn", "wireguard", "openvpn", "tunnel", "exit node"] {
        assert!(
            words.iter().any(|keyword| keyword == word),
            "search reaches Settings by {word}: {words:?}"
        );
    }
    assert!(
        compiled.meta.keywords.iter().any(|word| word == "vpn"),
        "the app's own words carry VPN"
    );
}
