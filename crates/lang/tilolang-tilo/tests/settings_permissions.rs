//! Settings' Permissions pane: this device's privacy as the session publishes it
//! (`session:privacy`) beside the workspaces' grants as the session publishes those
//! (`workspace/access`), each in the words the reading gives.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use crate::fixtures::Sent;
use crate::invoker::Answered as _;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use tilolang_runtime::delegate::Invocation;
use tilolang_tilo::host::Host;
use tilolang_tilo::runtime::{Actor, Value, ViewInstance};

const APP: &str = "so.tilo.settings";

/// The person at the settings window.
fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// The app as it ships, its ids and the platform's packages it reads through.
fn settings() -> Host {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/settings");
    let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
    )
    .expect("its ids parse");
    let metas = crate::fixtures::settings_packages().expect("the platform's packages read");
    let compiled =
        tilolang::compile_package_source(&source, &ids, &metas).expect("the package compiles");
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .expect("a host");
    host.load_compiled(&compiled).expect("the package loads");
    tilolang_tilo::platform::load(host.runtime(), &compiled.meta.uses);
    host
}

/// Serve the reading the session publishes for the root `id` (`session:privacy`,
/// `workspace/access`), as a followed resource hands it to the host.
fn serve(host: &Host, id: &str, ty: &str, reading: &str) {
    let target = tilolang_tilo::runtime::ResourceRef::parse_id(id, ty.to_owned())
        .unwrap_or_else(|| panic!("{id} is a resource id"));
    host.runtime()
        .set_reading(&target, reading)
        .expect("the reading is served");
}

/// The Permissions pane opened as its own view.
fn pane(host: &Host) -> ViewInstance {
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
    view
}

/// The lines the Permissions pane shows, its text as written.
fn line(host: &Host) -> Vec<String> {
    texts(&pane(host))
}

/// The key of the control of `kind` whose label `label` reads, as the pane lays it out.
fn labelled(tree: &tilolang_runtime::UiNode, kind: &str, label: &str) -> String {
    tree.of_kind(kind)
        .into_iter()
        .find(|node| node.props.get("label") == Some(&Value::text(label)))
        .unwrap_or_else(|| panic!("a {kind} labelled {label}"))
        .key
        .clone()
}

/// The form an answer's address takes: the portal's consent resource for it.
const RECORDER_CONSENT: &str = "portal:consent:Laptop\u{1f}so.tilo.recorder\u{1f}camera";

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
        {"$address": "portal:consent:Laptop\u001fso.tilo.recorder\u001fcamera",
         "app": "so.tilo.recorder", "name": "Recorder", "capability": "camera", "allowed": true,
         "standing": "until-revoked", "until_unix_ms": 0, "device": "Laptop",
         "this_device": true},
        {"$address": "portal:consent:Phone\u001fso.tilo.meet\u001fmicrophone",
         "app": "so.tilo.meet", "name": "Meet", "capability": "microphone", "allowed": false,
         "standing": "session", "until_unix_ms": 0, "device": "Phone", "this_device": false}
    ],
    "secrets": {"apps": [
        {"app": "binary:/usr/bin/chromium", "name": "Chromium", "allowed": true, "uses": 42,
         "items": 3},
        {"app": "sandbox:so.tilo.mail", "name": "Mail", "allowed": false, "uses": 7, "items": 1}
    ], "read_error": null},
    "security": {"disk_encryption": "on", "secure_boot": "off", "firewall": "firewalld",
                 "firewall_error": null}
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
        r#"{"known": true, "workspaces": [{"id": "w1", "name": "Home", "grants": [
            {"id": "cap:1", "section": "Apps", "holder": "Photos",
             "allows": "Read and write /photos", "expiry": "Always", "granted_by": "Ada",
             "app": "so.tilo.photos", "path": "/photos", "workspace": "w1",
             "holder_kind": "app", "granted_by_name": "Ada", "granted_by_kind": "you",
             "granted_by_you": true, "device": false, "can_revoke": true}
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
    for expected in [
        "The applications that use saved passwords and keys can't be read here.",
        "What protects this device itself can't be read here.",
    ] {
        assert!(said.contains(expected), "`{expected}` is shown: {said}");
    }
    assert!(
        !said.contains("No application has used saved passwords or keys yet."),
        "saved secrets nobody read are not none used: {said}"
    );
}

/// Neither root was ever published: the pane still draws, and each half says what it cannot say
/// rather than reading the unread resources as a device with no camera and a workspace that gave
/// nothing. A device whose session has not published `session:privacy` (a platform without one)
/// is the case this is: nothing of the root arrived, and none is coming.
#[test]
fn the_pane_draws_when_nothing_was_ever_published() {
    let host = settings();
    let said = line(&host).join(" | ");
    for expected in [
        "This device",
        "This device's privacy can't be read here, so these are unknown rather than unused.",
        "No application has a lasting answer on any of your devices.",
        "Workspace access",
        "Workspace access can't be checked",
    ] {
        assert!(said.contains(expected), "`{expected}` is shown: {said}");
    }
    assert!(
        !said.contains("No workspace here gives anything beyond its own documents."),
        "a workspace list nothing published is not said to have given nothing: {said}"
    );
}

/// The window over a root nothing published draws the shared row that says so, in place of the
/// pane's fields, and the reading that answers replaces it: the same handling the pane's own half
/// above relies on, through the app's `read_pane`.
#[test]
fn the_window_says_a_root_nothing_published_could_not_be_read() {
    let host = settings();
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .views
        .iter()
        .find(|view| view.name == "settings_window")
        .expect("the window is declared")
        .id
        .clone();
    let root = host.root_of(APP).expect("the app's root");
    let mut view = host
        .runtime()
        .open_view(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            &id,
            vec![tilolang_tilo::runtime::Value::resource(&root)],
        )
        .expect("the window opens");
    view.poll_patches().expect("the window draws");
    let said = texts(&view).join(" | ");
    assert!(
        said.contains("Appearance could not be read"),
        "the pane's row says the appearance is unknown: {said}"
    );
    assert!(
        said.contains("Nothing publishes it here"),
        "with why nothing is known of it: {said}"
    );

    // The reading answers: the pane draws what it says.
    serve(
        &host,
        "settings/appearance",
        "so.tilo.settings.sections#Appearance",
        r#"{"mode": "dark"}"#,
    );
    view.poll_patches().expect("the window draws again");
    let said = texts(&view).join(" | ");
    assert!(
        !said.contains("could not be read"),
        "the row gives way to the pane: {said}"
    );
}

/// The session publishes the workspaces' grants beside this device's privacy in every test here.
fn serve_access(host: &Host) {
    serve(
        host,
        "workspace/access",
        "tilo.workspace#Access",
        r#"{"known": true, "workspaces": []}"#,
    );
}

/// Each answer is a resource of the session's portal, and taking it back runs the consent
/// resource's own `forget`: the app that answered, on the address the answer carries.
#[test]
fn the_pane_takes_back_an_answer_on_its_own_resource() {
    let host = settings();
    serve(&host, "session:privacy", "so.tilo.device#Privacy", PRIVACY);
    serve_access(&host);
    let view = pane(&host);
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    view.press_answering(
        &host,
        &me(),
        &labelled(view.tree(), "ui.Button", "Take back"),
    )
    .expect("the answer is taken back");

    let sent = sent.borrow();
    let [
        Invocation::Action {
            app,
            name,
            target,
            args,
            ..
        },
    ] = sent.as_slice()
    else {
        panic!("one answer's own verb: {sent:?}");
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.device", "forget"),
        "the consent's own verb, as the device surface publishes it"
    );
    assert!(
        args.iter().all(|(_, value)| match value {
            Value::Ref(held) => held.target.id_string() == RECORDER_CONSENT,
            _ => false,
        }),
        "the consent itself is the only argument, the call's address: {args:?}"
    );
    assert_eq!(
        target
            .as_ref()
            .map(tilolang_tilo::runtime::ResourceRef::id_string),
        Some(RECORDER_CONSENT.to_owned()),
        "the first answer the pane lists"
    );
}

/// One argument of a sent call.
fn arg<'a>(args: &'a [(String, Value)], wanted: &str) -> Option<&'a Value> {
    args.iter()
        .find(|(named, _)| named == wanted)
        .map(|(_, value)| value)
}

/// The applications that use the saved passwords and keys are listed with what they keep, and
/// the pane stops one, lets it use the store again, or forgets it, each through the reading's
/// own verb with the application's identity.
#[test]
fn the_pane_lists_and_changes_the_saved_secrets() {
    let host = settings();
    serve(&host, "session:privacy", "so.tilo.device#Privacy", PRIVACY);
    serve_access(&host);
    let view = pane(&host);
    let said = texts(&view).join(" | ");
    for expected in [
        "Saved passwords and keys",
        "Chromium",
        "3 saved, used 42 times",
        "Mail",
        "1 saved, used 7 times",
        "Allowed",
        "Refused",
    ] {
        assert!(said.contains(expected), "`{expected}` is shown: {said}");
    }

    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    // The first application may use the store: the pane offers stopping it, and forgetting it.
    view.press_answering(&host, &me(), &labelled(view.tree(), "ui.Button", "Stop"))
        .expect("the application is stopped");
    let sent = sent.borrow();
    let [
        Invocation::Action {
            app, name, args, ..
        },
    ] = sent.as_slice()
    else {
        panic!("one verb of the reading: {sent:?}");
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.device", "set-secrets-access"),
        "the reading's own verb, as its surface publishes it"
    );
    assert_eq!(
        arg(args, "app"),
        Some(&Value::text("binary:/usr/bin/chromium"))
    );
    assert_eq!(arg(args, "allowed"), Some(&Value::Bool(false)));
    drop(sent);

    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    view.press_answering(&host, &me(), &labelled(view.tree(), "ui.Button", "Forget"))
        .expect("the application is forgotten");
    let sent = sent.borrow();
    let [
        Invocation::Action {
            app, name, args, ..
        },
    ] = sent.as_slice()
    else {
        panic!("one verb of the reading: {sent:?}");
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.device", "forget-secrets")
    );
    assert_eq!(
        arg(args, "app"),
        Some(&Value::text("binary:/usr/bin/chromium"))
    );
}

/// The device's own protection is said in the row's words: whether its disk is encrypted, its
/// firmware verifies what it boots and a firewall is in force, and when one cannot be read the
/// pane says so rather than showing it off.
#[test]
fn the_pane_says_what_protects_this_device() {
    let host = settings();
    serve(&host, "session:privacy", "so.tilo.device#Privacy", PRIVACY);
    serve_access(&host);
    let said = line(&host).join(" | ");
    for expected in [
        "Device protection",
        "Encrypted",
        "Disabled",
        "Active, firewalld",
    ] {
        assert!(said.contains(expected), "`{expected}` is shown: {said}");
    }

    // Read, and nothing protects it: each row says its own off, and a firewall that could not be
    // read says so with why rather than as off.
    let host = settings();
    serve(
        &host,
        "session:privacy",
        "so.tilo.device#Privacy",
        r#"{"capabilities": [], "answers": [],
            "secrets": {"apps": [], "read_error": "the store is locked"},
            "security": {"disk_encryption": "off", "secure_boot": "on",
                         "firewall": "unknown",
                         "firewall_error": "firewalld is not running"}}"#,
    );
    serve_access(&host);
    let said = line(&host).join(" | ");
    for expected in [
        "Not encrypted",
        "Enabled",
        "Can't read the firewall state: firewalld is not running",
        "The saved-secrets service can't be reached, so its applications can't be listed.",
    ] {
        assert!(said.contains(expected), "`{expected}` is shown: {said}");
    }
    assert!(
        !said.contains("Not active"),
        "a firewall nobody could read is not said to be off: {said}"
    );
}

/// The host as a session has it when the Apps pane draws: Settings, and the file explorer whose
/// access list the pane draws the files with.
fn settings_with_files() -> Host {
    let host = settings();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/files");
    let source = tilolang::PackageSource::read_dir(&dir).expect("the files package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the files package's ids"),
    )
    .expect("its ids parse");
    let metas = crate::fixtures::settings_packages().expect("the platform's packages read");
    let compiled = tilolang::compile_package_source(&source, &ids, &metas)
        .unwrap_or_else(|diagnostics| panic!("the files package compiles: {diagnostics:#?}"));
    host.load_compiled(&compiled)
        .expect("the files package loads");
    tilolang_tilo::platform::load(host.runtime(), &compiled.meta.uses);
    host
}

/// The Apps pane opened as its own view.
fn apps_pane(host: &Host) -> ViewInstance {
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .views
        .iter()
        .find(|view| view.name == "apps_access_pane")
        .expect("the Apps pane is declared")
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
    view
}

/// What the session publishes of a workspace's apps and their outside capabilities.
const OUTSIDE: &str = r#"{
    "known": true,
    "workspaces": [],
    "outside": [{
        "workspace": "w1", "workspace_name": "Home",
        "apps": [{
            "app": "so.tilo.mail", "name": "Mail",
            "capabilities": [
                {"capability": "imap.search", "allowed": false, "can_change": true},
                {"capability": "smtp.send", "allowed": true, "can_change": true}
            ]
        }]
    }]
}"#;

/// Each declared outside capability reads in plain words on its app's page, with a switch that
/// turns it off through the access root's own action and the app, workspace and capability it
/// names; a capability is never shown as its id.
#[test]
fn the_apps_pane_lists_each_declared_capability_and_turns_one_off() {
    let host = settings_with_files();
    serve(&host, "workspace/access", "tilo.workspace#Access", OUTSIDE);
    let view = apps_pane(&host);
    let said = texts(&view).join(" | ");
    for expected in [
        "Beyond the workspace",
        "Home",
        "Mail",
        "Send email",
        "Search the mail server",
        "Sends beyond the workspace",
        "Stopped: what it queues for this settles as refused",
        "Allowed",
        "Blocked",
    ] {
        assert!(said.contains(expected), "`{expected}` is shown: {said}");
    }
    assert!(
        !said.contains("smtp.send") && !said.contains("imap.search"),
        "a capability reads in words, never as its id: {said}"
    );

    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    let email = labelled(view.tree(), "ui.Toggle", "Allowed");
    view.fire_with_answering(&host, &me(), &email, "check", Some(&Value::Bool(false)))
        .expect("the switch turns the capability off");
    let sent = sent.borrow();
    let [
        Invocation::Action {
            app,
            name,
            target,
            args,
            ..
        },
    ] = sent.as_slice()
    else {
        panic!("one capability's own verb: {sent:?}");
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("tilo.workspace", "set-outside"),
        "the access root's typed action, as the Workspace package publishes it"
    );
    assert_eq!(
        target
            .as_ref()
            .map(tilolang_tilo::runtime::ResourceRef::id_string),
        Some("workspace/access".to_owned()),
        "the call is on the root the page reads"
    );
    let argument = |name: &str| {
        args.iter()
            .find(|(named, _)| named.as_str() == name)
            .map_or_else(
                || panic!("`{name}` is an argument: {args:?}"),
                |(_, value)| value.clone(),
            )
    };
    assert_eq!(argument("workspace"), Value::text("w1"));
    assert_eq!(argument("app"), Value::text("so.tilo.mail"));
    assert_eq!(argument("capability"), Value::text("smtp.send"));
    // A switch's value is the event's own, an option the host hands over as the plain answer
    // its action declares.
    assert_eq!(argument("allowed"), Value::some(Value::Bool(false)));
}

/// A capability turned off elsewhere reaches every open page on the next reading: what the pane
/// shows is the workspace's grant, never a switch's own memory.
#[test]
fn a_capability_turned_off_elsewhere_shows_off_in_every_open_pane() {
    let host = settings_with_files();
    serve(&host, "workspace/access", "tilo.workspace#Access", OUTSIDE);
    let mut first = apps_pane(&host);
    let mut second = apps_pane(&host);
    for shown in [&first, &second] {
        let said = texts(shown).join(" | ");
        assert!(said.contains("Allowed"), "the send goes now: {said}");
    }

    let off = OUTSIDE.replace(
        "\"capability\": \"smtp.send\", \"allowed\": true",
        "\"capability\": \"smtp.send\", \"allowed\": false",
    );
    serve(&host, "workspace/access", "tilo.workspace#Access", &off);
    for shown in [&mut first, &mut second] {
        shown
            .poll_patches()
            .expect("the pane draws the new reading");
        let said = texts(shown).join(" | ");
        assert!(
            !said.contains("Allowed"),
            "both capabilities are off now: {said}"
        );
        assert_eq!(
            said.matches("Blocked").count(),
            2,
            "each of the two reads blocked: {said}"
        );
    }
}
