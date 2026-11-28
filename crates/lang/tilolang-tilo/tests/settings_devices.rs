//! Settings' Devices and access pane: what may act as the person, read off the devices resource
//! and the MCP server's, with the verbs that name a key and take one back reaching the resources
//! those keep.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use crate::fixtures::Sent;
use crate::fixtures::invoker::Answered as _;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use crate::fixtures::texts;
use crate::fixtures::{control_under, labelled};
use tilolang_runtime::delegate::Invocation;
use tilolang_runtime::{Actor, ResourceRef, Value, ViewInstance};
use tilolang_tilo::host::Host;

/// The person at the settings window.
fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

const APP: &str = "so.tilo.settings";

/// One key that may act as the person, as the devices resource lists it.
fn key(id: &str, label: &str, kind: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "label": label,
        "kind": kind,
        "agent": "",
        "current": false,
        "revoked": false,
        "online": false,
        "nearby": false,
        "fingerprint": "AA BB CC DD EE FF",
        "issued_ms": 1_760_000_000_000_i64,
        "expires_ms": null,
        "last_active_ms": 1_760_003_600_000_i64,
        "workspace": "",
    })
}

/// The devices resource as the session publishes it: this computer, one of the person's other
/// computers, an agent's grant and one key the person took back.
fn devices_reading() -> String {
    let mut this = key("01".repeat(32).as_str(), "Ada's laptop", "device");
    this["current"] = serde_json::json!(true);
    let mut other = key("02".repeat(32).as_str(), "Ada's desktop", "device");
    other["online"] = serde_json::json!(true);
    other["nearby"] = serde_json::json!(true);
    let mut agent = key("03".repeat(32).as_str(), "Claude Code", "agent");
    agent["agent"] = serde_json::json!("Claude Code");
    agent["online"] = serde_json::json!(true);
    let mut gone = key("04".repeat(32).as_str(), "An old script", "script");
    gone["revoked"] = serde_json::json!(true);
    serde_json::json!({
        "devices": [
            {"id": "01".repeat(32), "label": "Ada's laptop", "online": true, "nearby": false,
             "local": true, "display": true, "wakeable": false, "audio_output": true,
             "seen_ms": 1_760_003_600_000_i64,
             "storage": []},
            {"id": "02".repeat(32), "label": "Ada's desktop", "online": true, "nearby": true,
             "local": false, "display": true, "wakeable": true, "audio_output": false,
             "seen_ms": 1_760_003_000_000_i64,
             "storage": []},
        ],
        "access": [this, other, agent, gone],
        "playing": null,
        "chosen": null,
        "sounds": [],
        "routes": [],
        "asks": [],
        "signed_in": true,
        "workspaces": [{"key": "w1", "name": "Studio"}],
        "refused": "",
    })
    .to_string()
}

/// The MCP server as its host publishes it: the agent's grant, working in one workspace, and one
/// the person stopped.
fn server_reading() -> String {
    serde_json::json!({
        "endpoint": "https://mcp.example",
        "grants": [
            {
                "id": "03".repeat(32),
                "agent": "Claude Code",
                "reach": {"kind": "workspace", "id": "w1", "name": "Studio"},
                "granted_ms": 1_760_000_000_000_i64,
                "last_ms": 1_760_003_600_000_i64,
                "sessions": [{"name": "Claude Code #1", "started_ms": 1_760_003_000_000_i64}],
                "browser_pages": false,
                "stopped": false,
            },
            {
                "id": "05".repeat(32),
                "agent": "A stopped agent",
                "reach": {"kind": "account"},
                "granted_ms": 1_760_000_000_000_i64,
                "sessions": [],
                "browser_pages": true,
                "stopped": true,
            },
        ],
        "workspaces": [{"id": "w1", "name": "Studio", "made_ms": 1_760_000_000_000_i64,
                        "open": "open"}],
    })
    .to_string()
}

fn devices() -> ResourceRef {
    ResourceRef::parse_id("session:devices", "so.tilo.device#Devices").expect("a resource id")
}

fn server() -> ResourceRef {
    ResourceRef::parse_id("mcp.server", "tilo.mcp-server#Server").expect("a resource id")
}

fn identity() -> ResourceRef {
    ResourceRef::parse_id("settings/identity", "so.tilo.settings.sections#Identity")
        .expect("a resource id")
}

/// The app as it ships, its window opened on its own pane drawing.
struct Pane {
    host: Host,
    view: ViewInstance,
}

fn invite() -> ResourceRef {
    ResourceRef::parse_id("session:invite", "so.tilo.device#Invite").expect("a resource id")
}

fn pane() -> Pane {
    pane_with_ticket("")
}

/// The pane on a device whose invite reading carries `ticket`: the code a new computer joins
/// with, as the daemon printed it (empty until one is asked for).
fn pane_with_ticket(ticket: &str) -> Pane {
    pane_with(ticket, &serde_json::json!({ "colour": null }))
}

/// The pane on a device whose invite reading carries `ticket`, the person's identity settings
/// reading `identity`.
fn pane_with(ticket: &str, identity_reading: &serde_json::Value) -> Pane {
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
    host.runtime()
        .set_reading(&devices(), &devices_reading())
        .expect("the devices reading is set");
    host.runtime()
        .set_reading(&server(), &server_reading())
        .expect("the server reading is set");
    host.runtime()
        .set_reading(&identity(), &identity_reading.to_string())
        .expect("the identity reading is set");
    host.runtime()
        .set_reading(
            &invite(),
            &serde_json::json!({ "workspace": "w1", "ticket": ticket }).to_string(),
        )
        .expect("the invite reading is set");
    let view = devices_pane(&host);
    Pane { host, view }
}

/// The Devices pane as its own view: a host without a window has no size to lay the window out
/// in, so what the pane draws is read from the pane.
fn devices_pane(host: &Host) -> ViewInstance {
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .views
        .iter()
        .find(|view| view.name == "devices_pane")
        .expect("the Devices pane is declared")
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

/// The key of the text field whose placeholder `placeholder` reads.
fn field(tree: &tilolang_runtime::UiNode, placeholder: &str) -> String {
    tree.of_kind("ui.TextInput")
        .into_iter()
        .find(|node| node.props.get("placeholder") == Some(&Value::text(placeholder)))
        .unwrap_or_else(|| panic!("a text field reading {placeholder:?}"))
        .key
        .clone()
}

/// What the pane asked of the resources that keep the person's keys, from now on.
fn watching(pane: &Pane) -> Rc<RefCell<Vec<Invocation>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    pane.host.runtime().set_invoker(Sent(Rc::clone(&seen)));
    seen
}

/// The verb `name` was asked of `app`, with its named arguments.
fn asked(seen: &Rc<RefCell<Vec<Invocation>>>, app: &str, name: &str) -> Vec<(String, Value)> {
    seen.borrow()
        .iter()
        .find_map(|invocation| match invocation {
            Invocation::Action {
                app: asked_app,
                name: asked_name,
                args,
                ..
            } if asked_app.as_str() == app && asked_name == name => Some(args.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{app}#{name}: {:?}", seen.borrow()))
}

/// Every text a value holds, the fields of a record included.
fn words_of(value: &Value, into: &mut Vec<String>) {
    match value {
        Value::Text(text) => into.push(text.clone()),
        Value::Rec(record) => {
            for field in record.fields.values() {
                words_of(field, into);
            }
        }
        Value::List(items) | Value::Tuple(items) => {
            for item in items {
                words_of(item, into);
            }
        }
        Value::Opt(Some(inner)) => words_of(inner, into),
        _ => {}
    }
}

/// What the pane shows of the keys that may act as the person, and how each reaches this device.
#[test]
fn the_devices_pane_lists_what_may_act_as_the_person() {
    let pane = pane();
    let shown = texts(&pane.view);
    assert!(shown.iter().any(|line| line == "Claude Code"), "{shown:?}");
    assert!(
        shown.iter().any(|line| line == "Only in Studio"),
        "the agent's reach, as its grant names it: {shown:?}"
    );
    assert!(
        shown.iter().any(|line| line == "Working now"),
        "a grant with a session open works now: {shown:?}"
    );
    assert!(
        shown.iter().any(|line| line == "Ada's desktop"),
        "the person's other computer: {shown:?}"
    );
    assert!(
        shown.iter().any(|line| line == "Online, on your network"),
        "it reaches this device directly: {shown:?}"
    );
    assert!(
        shown.iter().any(|line| line == "This computer"),
        "the device the pane is on is marked: {shown:?}"
    );
    assert!(
        shown.iter().any(|line| line == "AA BB CC DD EE FF"),
        "a key reads by its fingerprint: {shown:?}"
    );
    assert!(
        shown
            .iter()
            .any(|line| line == "Removed; every device of yours refuses it"),
        "a key the person took back says so: {shown:?}"
    );
    assert!(
        shown.iter().any(|line| line == "Directly, on your network"),
        "the mesh says how the devices reach each other: {shown:?}"
    );
    assert!(
        shown
            .iter()
            .any(|line| line == "Asks before operating web pages in Browser"),
        "a grant says whether it operates web pages: {shown:?}"
    );
}

/// Removing a key asks the devices resource with the reason the person gave, and never writes the
/// record beside it.
#[test]
fn removing_a_key_asks_the_devices_resource_with_its_reason() {
    let pane = pane();
    let seen = watching(&pane);
    let lost = labelled(
        pane.view.tree(),
        "ui.MenuItem",
        "Remove Ada's desktop — it was lost",
    );
    pane.view
        .fire_answering(&pane.host, &me(), &lost, "choose")
        .expect("the reason is chosen");
    let args = asked(&seen, "so.tilo.device", "revoke");
    let arg = |wanted: &str| {
        args.iter()
            .find(|(named, _)| named == wanted)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(arg("device"), Some(Value::text("02".repeat(32).as_str())));
    let reason = match arg("reason") {
        Some(Value::Opt(Some(reason))) => (*reason).clone(),
        other => other.expect("a revocation says why"),
    };
    assert_eq!(
        reason,
        Value::text("lost"),
        "the reason the person gave reaches the record that keeps it"
    );
}

/// The person's card opens Settings at `settings://devices_access`, which search runs as the
/// app's `open_pane`: the window shows Devices and access, where the key is taken back.
#[test]
fn the_persons_card_opens_the_pane_that_takes_a_key_back() {
    let pane = pane();
    let open_pane = pane
        .host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .actions
        .iter()
        .find(|action| action.name == "open_pane")
        .expect("the app publishes open_pane")
        .id
        .clone();
    for page in ["devices_access", "devices_sync"] {
        pane.host
            .runtime()
            .invoke_action(
                &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
                (open_pane.as_str(), vec![Value::text(page)]),
            )
            .expect("the page opens");
        let root = pane.host.root_of(APP).expect("the app's root");
        assert_eq!(
            pane.host.runtime().read(&root, "pane").expect("a pane"),
            Value::text("devices"),
            "{page} is Devices and access"
        );
    }
    let seen = watching(&pane);
    let stolen = labelled(
        pane.view.tree(),
        "ui.MenuItem",
        "Remove Ada's desktop — it was stolen",
    );
    pane.view
        .fire_answering(&pane.host, &me(), &stolen, "choose")
        .expect("the reason is chosen");
    let args = asked(&seen, "so.tilo.device", "revoke");
    assert!(
        args.iter()
            .any(|(named, value)| named == "device"
                && *value == Value::text("02".repeat(32).as_str())),
        "the key is taken back: {args:?}"
    );
}

/// Naming a key types the new name and Save hands it to the devices resource.
#[test]
fn naming_a_key_hands_its_name_to_the_devices_resource() {
    let mut window = pane();
    let name_it = labelled(window.view.tree(), "ui.MenuItem", "Name Ada's desktop…");
    window
        .view
        .fire_answering(&window.host, &me(), &name_it, "choose")
        .expect("the field opens");
    window.view.poll_patches().expect("the field draws");
    let field = field(window.view.tree(), "The name Ada's desktop should read by");
    window
        .view
        .edit(&field, &Value::text("The studio desktop"))
        .expect("the name is typed");
    let seen = watching(&window);
    let save = labelled(window.view.tree(), "ui.Button", "Save name");
    window
        .view
        .press_answering(&window.host, &me(), &save)
        .expect("the name is saved");
    let args = asked(&seen, "so.tilo.device", "rename");
    let arg = |wanted: &str| {
        args.iter()
            .find(|(named, _)| named == wanted)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(arg("device"), Some(Value::text("02".repeat(32).as_str())));
    assert_eq!(arg("name"), Some(Value::text("The studio desktop")));
}

/// Stopping an agent and letting it go on are the MCP server's own verbs, named by its grant and
/// its session.
#[test]
fn stopping_an_agent_asks_the_server_by_its_grant_and_session() {
    let pane = pane();
    let seen = watching(&pane);
    let stop = labelled(pane.view.tree(), "ui.Button", "Stop Claude Code #1");
    pane.view
        .press_answering(&pane.host, &me(), &stop)
        .expect("the agent is stopped");
    let args = asked(&seen, "tilo.mcp-server", "stop");
    let reference = args
        .iter()
        .find_map(|(named, value)| (named == "session").then_some(value.clone()))
        .expect("a stop names the session");
    let mut named = Vec::new();
    words_of(&reference, &mut named);
    assert!(
        named.contains(&"03".repeat(32)) && named.contains(&"Claude Code #1".to_owned()),
        "the stop names the grant and the session: {named:?}"
    );
}

/// Adding a device asks the device's daemon for the one-use code a new computer joins with: the
/// daemon prints it, and the invite reading carries it here.
#[test]
fn adding_a_device_asks_for_the_code_a_new_computer_joins_with() {
    let pane = pane();
    let seen = watching(&pane);
    let add = labelled(pane.view.tree(), "ui.Button", "Add a device");
    pane.view
        .press_answering(&pane.host, &me(), &add)
        .expect("the code is asked for");
    asked(&seen, "so.tilo.device", "issue-device-ticket");
}

/// The code the daemon printed stands in the page, with the control that copies it, so a person
/// hands it to the computer they are adding.
#[test]
fn the_new_devices_code_shows_with_the_control_that_copies_it() {
    let pane = pane_with_ticket("tilo1abcdef");
    let shown = texts(&pane.view);
    assert!(
        shown.iter().any(|line| line.contains("tilo1abcdef")),
        "the code stands where the person reads it: {shown:?}"
    );
    labelled(pane.view.tree(), "ui.Button", "Copy code");
}

/// A device that is nobody's yet offers the one thing it can do, and lists nothing as acting as a
/// person.
#[test]
fn a_device_that_is_nobodys_lists_nothing_as_acting_as_you() {
    let pane = pane();
    let mut reading: serde_json::Value = serde_json::from_str(&devices_reading()).unwrap();
    reading["signed_in"] = serde_json::json!(false);
    reading["access"] = serde_json::json!([]);
    reading["devices"] = serde_json::json!([]);
    pane.host
        .runtime()
        .set_reading(&devices(), &reading.to_string())
        .expect("the reading is set");
    let view = devices_pane(&pane.host);
    let shown = texts(&view);
    assert!(
        shown
            .iter()
            .any(|line| line.contains("This device is not one of yours yet")),
        "{shown:?}"
    );
    assert!(
        !shown.iter().any(|line| line == "Claude Code"),
        "nothing acts as anyone yet: {shown:?}"
    );
}

/// A person who left the backup of their identity for later finds it on Devices, beside who they
/// are: the backup is the devices resource's own verb, the one setup and an agent ask for, and the
/// reminder they turned off turns on again there, as an edit of their identity settings, which
/// the session's reminder reads. Once the identity is backed up, there is nothing to remind of.
#[test]
fn devices_offers_the_backup_and_turns_its_reminder_back_on() {
    let pane = pane_with(
        "",
        &serde_json::json!({ "colour": null, "backup_reminders": false, "backed_up": false }),
    );
    let seen = watching(&pane);
    pane.view
        .press_answering(
            &pane.host,
            &me(),
            &labelled(pane.view.tree(), "ui.Button", "Back up"),
        )
        .expect("the backup is asked for");
    asked(&seen, "so.tilo.device", "back-up-identity");

    seen.borrow_mut().clear();
    let switch = control_under(pane.view.tree(), "Remind me to back up", "ui.Toggle");
    assert_eq!(
        pane.view.bound_value(&switch),
        Some(Value::Bool(false)),
        "the switch shows the reminders as the person left them"
    );
    pane.view
        .edit(&switch, &Value::Bool(true))
        .expect("the reminders turn on");
    let edits = seen.borrow().clone();
    let [
        Invocation::Edit {
            app,
            target,
            field,
            value,
            ..
        },
    ] = edits.as_slice()
    else {
        panic!("one field edit: {edits:?}");
    };
    assert_eq!(app.as_str(), "so.tilo.settings.sections");
    assert_eq!(target.id_string(), "settings/identity");
    assert_eq!(field.as_str(), "backup_reminders");
    assert_eq!(*value, Value::Bool(true));

    let backed_up = pane_with(
        "",
        &serde_json::json!({ "colour": null, "backup_reminders": true, "backed_up": true }),
    );
    let shown = texts(&backed_up.view);
    assert!(
        shown.iter().any(|line| line.starts_with("Backed up.")),
        "{shown:?}"
    );
    assert!(
        !shown.iter().any(|line| line == "Remind me to back up"),
        "a backed-up identity is reminded of nothing: {shown:?}"
    );
}
