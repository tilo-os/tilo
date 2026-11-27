//! Settings' This device's server pane: the web page it serves browsers, read off the server
//! settings the daemon publishes — no domain name means the certificate is one only the person's
//! devices trust, and one that browsers outside the workspace warn about; a domain name replaces
//! that with the name browsers reach it by.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use crate::fixtures::texts;
use serde_json::{Value as Json, json};
use tilolang_runtime::{ResourceRef, ViewInstance};
use tilolang_tilo::host::Host;
use tilolang_tilo::platform::platform_roots;

const APP: &str = "so.tilo.settings";

/// What the session publishes of this device's server settings, as the pane's view names it.
fn publish(host: &Host, fields: &Json) {
    let (published, ty) = platform_roots()
        .into_iter()
        .find(|(_, root)| root == "so.tilo.device#Server")
        .unwrap_or_else(|| panic!("the server settings are one of the platform's resources"));
    let reference = ResourceRef::parse_id(&published, ty).expect("a resource id");
    host.runtime()
        .set_reading(&reference, &fields.to_string())
        .expect("the reading is handed in");
}

/// This device's server settings with the web page `web`.
fn server_settings(web: Json) -> Json {
    json!({
        "configured": true,
        "problem": null,
        "state-root": "/var/lib/tilo-server",
        "web": web,
        "work": {
            "from": "nobody",
            "when": {"charging-only": false, "free-cpu-percent": 0, "free-memory-percent": 0},
        },
        "services": { "app-serving": true },
    })
}

/// The pane the window shows, opened on its own with this device's server settings.
fn pane(web: Json) -> ViewInstance {
    let (host, _compiled) = crate::fixtures::settings_host();
    publish(&host, &server_settings(web));
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .views
        .iter()
        .find(|view| view.name == "device_server_pane")
        .expect("the server pane is declared")
        .id
        .clone();
    let mut view = host
        .runtime()
        .open_view(
            &tilolang_runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            &id,
            Vec::new(),
        )
        .expect("the pane opens");
    // A view that reads a resource of another app draws its waiting skeleton first: the read
    // lands on the next draw.
    for _ in 0..4 {
        view.poll_patches().expect("the pane draws");
        if view.tree().kind != "ui.Skeleton" {
            return view;
        }
    }
    panic!(
        "the pane draws, not its waiting skeleton: {:?}",
        view.tree()
    );
}

/// The line saying a browser outside the workspace warns about the certificate.
const WARNING: &str =
    "Browsers outside your workspace will warn that this server's certificate is not trusted.";

#[test]
fn a_server_with_no_domain_name_says_browsers_outside_its_workspace_warn() {
    let shown = texts(&pane(json!({ "domain": null })));
    assert!(
        shown.iter().any(|text| text == WARNING),
        "the pane warns about the certificate: {shown:?}"
    );
}

#[test]
fn a_server_with_a_domain_name_says_the_name_instead() {
    let shown = texts(&pane(json!({ "domain": "home.example.org" })));
    assert!(
        shown.iter().any(|text| text == "home.example.org"),
        "the pane names the domain browsers reach it by: {shown:?}"
    );
    assert!(
        !shown.iter().any(|text| text == WARNING),
        "a certificate browsers can verify leaves nothing to warn about: {shown:?}"
    );
}
