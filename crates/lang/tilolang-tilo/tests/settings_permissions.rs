//! Settings' Permissions pane: this device's privacy as the session publishes it
//! (`session:privacy`) beside the workspaces' grants as the session publishes those
//! (`workspace/access`), each in the words the reading gives.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::path::Path;

use tilolang_tilo::host::Host;
use tilolang_tilo::runtime::{Actor, ViewInstance};

const APP: &str = "so.tilo.settings";

/// The app as it ships, its ids and the platform's packages it reads through.
fn settings() -> Host {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/settings");
    let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
    )
    .expect("its ids parse");
    let metas =
        tilolang_tilo::platform_apps::platform_packages().expect("the platform's packages read");
    let compiled =
        tilolang::compile_package_source(&source, &ids, &metas).expect("the package compiles");
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .expect("a host");
    host.load_compiled(&compiled).expect("the package loads");
    tilolang_tilo::platform_apps::load(&host, &compiled.meta.uses);
    host
}

/// Serve the reading the session publishes for the root `id` (`session:privacy`,
/// `workspace/access`), as a followed resource hands it to the host.
fn serve(host: &Host, id: &str, ty: &str, reading: &str) {
    let target = tilolang_tilo::runtime::ResourceRef::parse_id(id, ty.to_owned())
        .unwrap_or_else(|| panic!("{id} is a resource id"));
    host.runtime()
        .serve_reading(&target, reading)
        .expect("the reading is served");
}

/// The lines the Permissions pane shows, its text as written.
fn line(host: &Host) -> Vec<String> {
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .views
        .iter()
        .find(|view| view.name == "permissions_pane")
        .expect("the Permissions pane is declared")
        .id
        .clone();
    let mut view = host
        .runtime()
        .open_view(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            &id,
            Vec::new(),
        )
        .expect("the pane opens");
    view.poll_patches().expect("the pane draws");
    texts(&view)
}

/// The texts a view shows: its text as written, and its rich text flattened.
fn texts(view: &ViewInstance) -> Vec<String> {
    crate::fixtures::nodes(view.tree())
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            tilolang_tilo::runtime::Value::Text(text) => Some(text.clone()),
            tilolang_tilo::runtime::Value::Rich(rich) => Some(rich.written_text()),
            _ => None,
        })
        .collect()
}

/// What the session publishes of this machine's privacy: a camera that is blocked and in use, a
/// microphone that is muted, a screen nothing uses, a location no one can watch, and two of the
/// person's answers.
const PRIVACY: &str = r#"{
    "capabilities": [
        {"capability": "camera", "present": true, "switch": "software-blocked", "muted": false,
         "mute_known": false, "watched": true,
         "in_use": [{"app": "so.tilo.recorder", "name": "Recorder"}]},
        {"capability": "microphone", "present": true, "switch": "allowed", "muted": true,
         "mute_known": true, "watched": true, "in_use": []},
        {"capability": "screen", "present": true, "switch": "unavailable", "muted": false,
         "mute_known": false, "watched": true, "in_use": []},
        {"capability": "location", "present": true, "switch": "unavailable", "muted": false,
         "mute_known": false, "watched": false, "in_use": []}
    ],
    "answers": [
        {"app": "so.tilo.recorder", "name": "Recorder", "capability": "camera", "allowed": true,
         "standing": "until-revoked", "until_unix_ms": 0, "device": "Laptop",
         "this_device": true},
        {"app": "so.tilo.meet", "name": "Meet", "capability": "microphone", "allowed": false,
         "standing": "session", "until_unix_ms": 0, "device": "Phone", "this_device": false}
    ]
}"#;

/// Each device says what blocks it and what uses it, in its own words, and each answer names the
/// application, the device it stands on and how long it lasts.
#[test]
fn the_pane_says_what_each_device_is_doing_and_what_was_answered() {
    let host = settings();
    serve(&host, "session:privacy", "so.tilo.device#Privacy", PRIVACY);
    // The pane reads both roots, as the session publishes both: a root with no reading at all is
    // one its read refuses, never an empty one.
    serve(
        &host,
        "workspace/access",
        "tilo.workspace#Access",
        r#"{"known": false, "workspaces": []}"#,
    );
    let shown = line(&host);
    let said = shown.join(" | ");
    for expected in [
        "Camera",
        "Blocked in software",
        "In use by Recorder",
        "Microphone",
        "Muted",
        "Screen",
        "No application is using it now",
        "Location",
        "The system asks you each time an application wants it, so this list cannot say who uses it",
        "Recorder",
        "Camera, on this device · until you take it back",
        "Meet",
        "May not use the microphone, on Phone · until this session ends",
        "Allowed",
        "Refused",
    ] {
        assert!(said.contains(expected), "`{expected}` is shown: {said}");
    }
    assert!(
        !said.contains("so.tilo.recorder"),
        "an application is never shown by its id: {said}"
    );
}

/// The workspaces' grants are listed with what each allows, for how long, and who gave it; a
/// workspace that gave nothing says so, and one that could not be read is not said to have given
/// nothing.
#[test]
fn the_pane_lists_the_workspaces_grants_and_never_confuses_absent_with_unread() {
    let host = settings();
    serve(&host, "session:privacy", "so.tilo.device#Privacy", PRIVACY);
    serve(
        &host,
        "workspace/access",
        "tilo.workspace#Access",
        r#"{"known": true, "workspaces": [{"name": "Home", "grants": [
            {"id": "cap:1", "section": "Apps", "holder": "Photos",
             "allows": "Read and write /photos", "expiry": "Always", "granted_by": "Ada"}
        ]}]}"#,
    );
    let said = line(&host).join(" | ");
    for expected in [
        "Workspace access",
        "Home",
        "Photos",
        "Apps",
        "Read and write /photos · Always · given by Ada",
    ] {
        assert!(said.contains(expected), "`{expected}` is shown: {said}");
    }

    // Read, and nothing given.
    let host = settings();
    serve(&host, "session:privacy", "so.tilo.device#Privacy", PRIVACY);
    serve(
        &host,
        "workspace/access",
        "tilo.workspace#Access",
        r#"{"known": true, "workspaces": []}"#,
    );
    let said = line(&host).join(" | ");
    assert!(
        said.contains("No workspace here gives anything beyond its own documents."),
        "{said}"
    );

    // Not read at all: it cannot be checked, which is not the same as nothing given.
    let host = settings();
    serve(&host, "session:privacy", "so.tilo.device#Privacy", PRIVACY);
    serve(
        &host,
        "workspace/access",
        "tilo.workspace#Access",
        r#"{"known": false, "workspaces": []}"#,
    );
    let said = line(&host).join(" | ");
    assert!(
        said.contains("Workspace access can't be checked"),
        "an unread workspace list is said so: {said}"
    );
    assert!(
        !said.contains("No workspace here gives anything beyond its own documents."),
        "and never as nothing given: {said}"
    );
}

/// A reading that names no device at all is said to be unknown, never unused: a host that
/// publishes the record and can see none of its devices says so.
#[test]
fn a_device_read_nothing_of_is_unknown_rather_than_unused() {
    let host = settings();
    serve(
        &host,
        "session:privacy",
        "so.tilo.device#Privacy",
        r#"{"capabilities": [], "answers": []}"#,
    );
    serve(
        &host,
        "workspace/access",
        "tilo.workspace#Access",
        r#"{"known": false, "workspaces": []}"#,
    );
    let said = line(&host).join(" | ");
    assert!(
        said.contains("This device's privacy can't be read here"),
        "{said}"
    );
    assert!(
        !said.contains("No application is using it now"),
        "nothing read is not nothing used: {said}"
    );
}
