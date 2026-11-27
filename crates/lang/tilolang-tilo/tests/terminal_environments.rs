//! Terminal's environments: the target menu lists the workspace's environments from the reading
//! `tilo.environment` publishes, and its panel makes, renames and grants folders to them through
//! the environment's own verbs.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use crate::fixtures::Sent;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use tilolang_runtime::delegate::Invocation;
use tilolang_tilo::host::Host;
use tilolang_tilo::runtime::{Actor, Value, ViewInstance};

const APP: &str = "so.tilo.terminal";

fn ada() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// The app as it ships, compiled against the platform's packages it reads, and those packages.
fn compiled() -> (tilolang::Compiled, Vec<tilolang::PackageMeta>) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/terminal");
    let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
    )
    .expect("its ids parse");
    let imports = tilolang_tilo::platform::named_platform_packages_of(&source)
        .expect("the platform's packages read");
    let compiled =
        tilolang::compile_package_source(&source, &ids, &imports).expect("the package compiles");
    (compiled, imports)
}

/// The app as it ships, loaded into a host.
fn terminal() -> Host {
    let (compiled, _) = compiled();
    let host = Host::new(ada(), || None).expect("a host");
    host.load_compiled(&compiled).expect("the package loads");
    tilolang_tilo::platform::load(host.runtime(), &compiled.meta.uses);
    host
}

/// The workspace's environments as a device publishes them: the default one, and one called
/// Work with a tool and two folders, one of them only to read.
const ENVIRONMENTS: &str = r#"{"environments": [
    {"record": "default", "name": "Default", "tools": [], "grants": [], "home": "Persistent",
     "runtime": "2026.11"},
    {"record": "work", "name": "Work", "tools": ["ripgrep"],
     "grants": [{"path": "/home/ada/src", "write": true}, {"path": "/data", "write": false}],
     "home": "Persistent", "runtime": "2026.11"}
]}"#;

fn serve_environments(host: &Host) {
    let target = tilolang_tilo::runtime::ResourceRef::parse_id(
        "environments",
        "tilo.environment#Environments".to_owned(),
    )
    .expect("the root is a resource id");
    host.runtime()
        .set_reading(&target, ENVIRONMENTS)
        .expect("the reading is served");
}

/// The window's target menu, on the session a window opens.
fn target_menu(host: &Host) -> ViewInstance {
    let package = host.package(APP).expect("the app is loaded");
    let session = package
        .meta
        .types
        .iter()
        .find(|ty| ty.name == "Session")
        .expect("a session type")
        .id
        .clone();
    let view = package
        .meta
        .views
        .iter()
        .find(|view| view.name == "target_menu")
        .expect("the target menu is declared")
        .id
        .clone();
    let root = host.root_resource(APP, &session).expect("the session");
    let mut view = host
        .open_resource(&root, Some(&view))
        .expect("the menu opens");
    view.poll_patches().expect("the menu draws");
    view
}

/// The key of the control of `kind` whose label reads `label`: the `nth` of them, in the order
/// the view lays them out.
fn labelled(view: &ViewInstance, kind: &str, label: &str, nth: usize) -> String {
    view.tree()
        .of_kind(kind)
        .into_iter()
        .filter(|node| node.props.get("label") == Some(&Value::text(label)))
        .nth(nth)
        .unwrap_or_else(|| panic!("a {kind} labelled {label}, number {nth}"))
        .key
        .clone()
}

fn texts(view: &ViewInstance) -> Vec<String> {
    crate::fixtures::nodes(view.tree())
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The one control of `kind` the view shows.
fn only(view: &ViewInstance, kind: &str) -> String {
    let nodes = view.tree().of_kind(kind);
    let [node] = nodes.as_slice() else {
        panic!("one {kind}, of {}", nodes.len());
    };
    node.key.clone()
}

/// Type `text` into the panel's one field and take the dialog's main act.
fn submit(view: &mut ViewInstance, text: &str) {
    view.edit(&only(view, "ui.TextInput"), &Value::text(text))
        .expect("the field takes it");
    view.poll_patches().expect("it draws");
    view.fire_with(&only(view, "ui.ModalDialog"), "act_primary", None)
        .expect("the dialog acts");
    view.poll_patches().expect("it draws");
}

/// The verb asked last, on which resource, with its arguments.
fn last(sent: &Rc<RefCell<Vec<Invocation>>>) -> (String, String, String, Vec<(String, Value)>) {
    match sent.borrow().last() {
        Some(Invocation::Action {
            app,
            name,
            target,
            args,
            ..
        }) => (
            app.as_str().to_owned(),
            name.clone(),
            target
                .as_ref()
                .map(tilolang_runtime::ResourceRef::id_string)
                .unwrap_or_default(),
            // The environment the verb is called on comes first; what the person gave follows.
            args.iter()
                .filter(|(name, _)| name != "environment")
                .cloned()
                .collect(),
        ),
        other => panic!("a verb was asked: {other:?}"),
    }
}

/// The menu offers each environment of the workspace by its name, and the panel lists what each
/// one has: its tools and its folders, saying which it may only read.
#[test]
fn the_menu_and_the_panel_show_the_workspaces_environments() {
    let host = terminal();
    serve_environments(&host);
    let mut view = target_menu(&host);
    labelled(&view, "ui.MenuItem", "Work", 0);
    assert!(
        view.tree()
            .of_kind("ui.MenuItem")
            .iter()
            .all(|item| item.props.get("label") != Some(&Value::text("Default"))),
        "the default environment is the menu's own first row, never listed twice"
    );

    view.fire_with(
        &labelled(&view, "ui.MenuItem", "Manage environments…", 0),
        "choose",
        None,
    )
    .expect("the panel opens");
    view.poll_patches().expect("it draws");
    let shown = texts(&view).join(" | ");
    for expected in [
        "Work",
        "Tools: ripgrep",
        "Folders: /home/ada/src, /data (read-only)",
        "No tools yet. Run pkg install inside the environment.",
    ] {
        assert!(shown.contains(expected), "`{expected}` is shown: {shown}");
    }
}

/// Making, renaming and granting a folder are the environment's own verbs, on the workspace's
/// environment resource, with what the person typed.
#[test]
fn the_panel_makes_renames_and_grants_through_the_environments_verbs() {
    let host = terminal();
    serve_environments(&host);
    let mut view = target_menu(&host);
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    view.fire_with(
        &labelled(&view, "ui.MenuItem", "Manage environments…", 0),
        "choose",
        None,
    )
    .expect("the panel opens");
    view.poll_patches().expect("it draws");

    submit(&mut view, "Prose");
    let (app, verb, target, args) = last(&sent);
    assert_eq!(
        (app.as_str(), verb.as_str(), target.as_str()),
        ("tilo.environment", "create", "environment")
    );
    assert_eq!(args, vec![("name".to_owned(), Value::text("Prose"))]);

    // The rows stand sorted by name: Default, then Work.
    view.press(&labelled(&view, "ui.Button", "Rename", 1))
        .expect("renaming starts");
    view.poll_patches().expect("it draws");
    submit(&mut view, "Writing");
    let (_, verb, target, args) = last(&sent);
    assert_eq!((verb.as_str(), target.as_str()), ("rename", "environment"));
    assert_eq!(
        args,
        vec![
            ("record".to_owned(), Value::text("work")),
            ("name".to_owned(), Value::text("Writing")),
        ]
    );

    view.press(&labelled(&view, "ui.Button", "Grant folder…", 1))
        .expect("granting starts");
    view.poll_patches().expect("it draws");
    submit(&mut view, "/home/ada/notes");
    let (_, verb, target, args) = last(&sent);
    assert_eq!(
        (verb.as_str(), target.as_str()),
        ("grant-dir", "environment")
    );
    assert_eq!(
        args,
        vec![
            ("record".to_owned(), Value::text("work")),
            ("path".to_owned(), Value::text("/home/ada/notes")),
            ("write".to_owned(), Value::Bool(true)),
        ]
    );
}

/// An empty name makes nothing: the action refuses it in its own words.
#[test]
fn an_unnamed_environment_is_refused_before_anything_is_asked() {
    let host = terminal();
    serve_environments(&host);
    let mut view = target_menu(&host);
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    view.fire_with(
        &labelled(&view, "ui.MenuItem", "Manage environments…", 0),
        "choose",
        None,
    )
    .expect("the panel opens");
    view.poll_patches().expect("it draws");
    let _ = view.fire_with(&only(&view, "ui.ModalDialog"), "act_primary", None);
    assert!(
        sent.borrow().iter().all(|asked| !matches!(
            asked,
            Invocation::Action { name, .. } if name == "create"
        )),
        "nothing is made without a name: {:?}",
        sent.borrow()
    );
}

/// The environments are Terminal's records, whichever program writes them: the surface its
/// package ships declares their kind, so a workspace's store judges their writes once the
/// tilolang Terminal is installed.
#[test]
fn the_package_declares_the_environments_it_keeps() {
    let (compiled, imports) = compiled();
    let bytes = tilolang_tilo::surface::surface_bytes_reading(
        &[&compiled.meta],
        &imports,
        &tilolang_tilo::runtime::SourceWords,
    )
    .expect("the package's surface compiles");
    let surface: serde_json::Value = serde_json::from_slice(&bytes).expect("it is JSON");
    let kinds: Vec<&str> = surface["resources"]
        .as_array()
        .expect("its resources")
        .iter()
        .filter_map(|resource| resource["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"environment"), "{kinds:?}");
}

/// The workspace's own session and where its shell runs, as the device that keeps the workspace's
/// machine names them: an agent's command goes to the root `session`'s record, `root-vgkud5dv`,
/// and the host reads its `target` under `fzio5css` to refuse an agent the device's own shell.
/// The ids lock never changes an id once given; this holds the package to the ids the host reads.
#[test]
fn the_session_keeps_the_ids_the_workspaces_host_reads() {
    let (compiled, _) = compiled();
    let session = compiled
        .meta
        .types
        .iter()
        .find(|ty| ty.name == "Session")
        .expect("a session type");
    assert_eq!(
        session.id, "vgkud5dv",
        "the root session's record is root-vgkud5dv"
    );
    let target = session
        .fields
        .iter()
        .find(|field| field.name == "target")
        .expect("the session says where it runs");
    assert_eq!(target.id, "fzio5css");
}
