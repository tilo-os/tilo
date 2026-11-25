//! Settings' Storage pane: what the device holds of each workspace it mounts, read from the
//! `storage` resource, and the actions that keep a workspace here or free the files it fetched.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

const APP: &str = "so.tilo.settings";

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// The app as it ships, with the platform's packages it reads through and the fleet's native ones
/// (the migration engine its account rows ask).
fn settings() -> (Host, tilolang::Compiled) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/settings");
    let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
    )
    .expect("its ids parse");
    let metas =
        crate::fixtures::settings_packages().expect("the platform's and the fleet's packages read");
    let compiled =
        tilolang::compile_package_source(&source, &ids, &metas).expect("the package compiles");
    let host = Host::new(me(), || None).expect("a host");
    host.load_compiled(&compiled).expect("the package loads");
    tilolang_tilo::platform_apps::load(&host, &compiled.meta.uses);
    (host, compiled)
}

/// The device's `storage` reading, as the session hands it in.
fn storage() -> ResourceRef {
    ResourceRef::parse_id("storage", "so.tilo.device#Storage").expect("a resource id")
}

/// The person's devices, as the session publishes them.
fn devices() -> ResourceRef {
    ResourceRef::parse_id("session:devices", "so.tilo.device#Devices").expect("a resource id")
}

/// The pane the window shows.
fn pane(host: &Host) -> String {
    let root = host.root_of(APP).expect("the app's root");
    match host.runtime().read(&root, "pane").expect("a pane") {
        Value::Text(text) => text,
        other => panic!("not text: {other:?}"),
    }
}

/// Run the app-level action `name` with `args`, as a search row does from outside the window.
fn invoke(host: &Host, name: &str, args: Vec<Value>) -> Value {
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .actions
        .iter()
        .find(|action| action.name == name)
        .unwrap_or_else(|| panic!("the app publishes no `{name}`"))
        .id
        .clone();
    match host
        .runtime()
        .invoke_action(&me(), (id.as_str(), args))
        .unwrap_or_else(|why| panic!("{name}: {why}"))
    {
        tilolang_runtime::InvokeOutcome::Done(value) => value,
        other => panic!("{name}: {other:?}"),
    }
}

/// The content pane opened as its own view, showing the pane the window is on.
fn open_storage_pane(host: &Host) -> ViewInstance {
    invoke(host, "open_pane", vec![Value::text("storage")]);
    assert_eq!(pane(host), "storage");
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .views
        .iter()
        .find(|view| view.name == "storage_pane")
        .expect("the pane is a view")
        .id
        .clone();
    let mut view = host
        .runtime()
        .open_view(&me(), &id, Vec::new())
        .expect("the pane opens");
    view.poll_patches().expect("the pane draws");
    view
}

/// Every text under `node` that the pane shows, in order.
fn texts(node: &UiNode, into: &mut Vec<String>) {
    for value in node.props.values() {
        match value {
            Value::Text(text) => into.push(text.clone()),
            Value::Rich(rich) => into.push(rich.written_text()),
            _ => {}
        }
    }
    for child in &node.children {
        texts(child, into);
    }
}

fn shown(view: &ViewInstance) -> Vec<String> {
    let mut out = Vec::new();
    texts(view.tree(), &mut out);
    out
}

fn has(view: &ViewInstance, wanted: &str) -> bool {
    shown(view).iter().any(|text| text == wanted)
}

fn contains(view: &ViewInstance, wanted: &str) -> bool {
    shown(view).iter().any(|text| text.contains(wanted))
}

/// The menu item under the row's own menu, labelled `label`.
fn menu_item(view: &ViewInstance, row: &str, label: &str) -> String {
    let wanted = format!("More for {row}");
    let button = view
        .tree()
        .of_kind("ui.MenuButton")
        .into_iter()
        .find(|node| node.props.get("label") == Some(&Value::text(wanted.clone())))
        .unwrap_or_else(|| {
            let menus: Vec<_> = view
                .tree()
                .of_kind("ui.MenuButton")
                .iter()
                .map(|node| node.props.get("label").cloned())
                .collect();
            panic!("no menu for {row}: {menus:?}")
        });
    let items = button.of_kind("ui.MenuItem");
    items
        .iter()
        .find(|item| item.props.get("label") == Some(&Value::text(label.to_owned())))
        .map_or_else(
            || {
                let labels: Vec<_> = items
                    .iter()
                    .map(|item| item.props.get("label").cloned())
                    .collect();
                panic!("no {label} in {row}'s menu: {labels:?}")
            },
            |item| item.key.clone(),
        )
}

/// One invocation as it was sent, with its arguments by name.
#[derive(Clone, Default)]
struct Sent(Rc<RefCell<Vec<Invocation>>>);

impl Sent {
    /// The last invocation that ran `name` on the storage resource, its arguments by name.
    fn asked(&self, name: &str) -> Vec<(String, Value)> {
        self.0
            .borrow()
            .iter()
            .rev()
            .find_map(|invocation| match invocation {
                Invocation::Action {
                    name: asked,
                    args,
                    target,
                    ..
                } if asked == name => {
                    let target = target
                        .as_ref()
                        .unwrap_or_else(|| panic!("{name} names the resource it acts on"));
                    assert_eq!(
                        target.id_string(),
                        storage().id_string(),
                        "{name} acts on the storage reading"
                    );
                    Some(args.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("the app asks `{name}`: {:?}", self.0.borrow()))
    }

    /// The value of the argument `wanted`.
    fn argument(&self, name: &str, wanted: &str) -> Value {
        self.asked(name)
            .into_iter()
            .find(|(named, _)| named == wanted)
            .unwrap_or_else(|| panic!("{name} is given `{wanted}`"))
            .1
    }
}

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

/// What the device publishes: one workspace kept here with files only here and fetched files,
/// and one whose files this device holds none of, how it fetches and keeps fetched files, and the
/// synced files whose copy here can be freed.
const READING: &str = r#"{
  "disk": {"total_bytes": 512000000000, "free_bytes": 256000000000},
  "categories": [],
  "largest": [],
  "cache_bytes": 0,
  "scanned": true,
  "read_error": null,
  "pending": [],
  "problem": null,
  "residency": [
    {"workspace": "studio", "kept_here": true, "pinned_bytes": 0, "replicated_bytes": 4000,
     "only_here_bytes": 900, "only_here_files": 2, "cache_bytes": 3000},
    {"workspace": "archive", "kept_here": false, "pinned_bytes": 0, "replicated_bytes": 0,
     "only_here_bytes": 0, "only_here_files": 0, "cache_bytes": 0}
  ],
  "residency_error": null,
  "cache_policy": {"prefetch": true, "budget_bytes": 10000000000},
  "reclaimable": [
    {"workspace": "studio", "path": "/films/rough-cut.mov", "bytes": 2000},
    {"workspace": "studio", "path": "/films/audio.wav", "bytes": 1000}
  ],
  "reclaimable_bytes": 3000,
  "sync_error": null
}"#;

/// What the person's other device last said it holds: both of the workspaces, and when it was
/// last with a host.
const DEVICES: &str = r#"{
  "devices": [
    {"id": "self", "label": "This one", "online": true, "nearby": false, "local": true,
     "display": false, "wakeable": false, "audio_output": false, "seen_ms": 0, "storage": []},
    {"id": "laptop", "label": "Ada's laptop", "online": true, "nearby": false, "local": false,
     "display": false, "wakeable": false, "audio_output": false, "seen_ms": 1764000000000,
     "storage": [
       {"workspace": "studio", "kept_here": false, "pinned_bytes": 0,
        "replicated_bytes": 4000, "only_here_bytes": 0, "only_here_files": 0, "cache_bytes": 0},
       {"workspace": "archive", "kept_here": false, "pinned_bytes": 0,
        "replicated_bytes": 2000, "only_here_bytes": 0, "only_here_files": 0, "cache_bytes": 0}
     ]}
  ],
  "playing": null,
  "chosen": null,
  "sounds": [],
  "routes": [],
  "asks": [],
  "signed_in": true,
  "workspaces": [],
  "refused": ""
}"#;

fn installed(reading: &str) -> (Host, ViewInstance) {
    let (host, _) = settings();
    host.runtime()
        .set_reading(&storage(), reading)
        .expect("the reading is set");
    host.runtime()
        .set_reading(&devices(), DEVICES)
        .expect("the devices are set");
    let view = open_storage_pane(&host);
    (host, view)
}

#[test]
fn the_pane_shows_what_this_device_holds_of_each_workspace() {
    let (_host, view) = installed(READING);

    assert!(
        has(&view, "Where workspace files are kept"),
        "{:?}",
        shown(&view)
    );
    assert!(has(&view, "studio"), "{:?}", shown(&view));
    assert!(has(&view, "archive"), "{:?}", shown(&view));
    assert!(
        contains(&view, "On no other device: 900 B in 2 files"),
        "the only copy is the point of the split: {:?}",
        shown(&view)
    );
    assert!(
        contains(&view, "Also on another device: 4 KB"),
        "{:?}",
        shown(&view)
    );
    assert!(
        contains(&view, "Fetched files, safe to free: 3 KB"),
        "{:?}",
        shown(&view)
    );
    assert!(has(&view, "900 B only here"), "{:?}", shown(&view));
    assert!(has(&view, "Kept here"), "{:?}", shown(&view));
    assert!(
        contains(&view, "nothing of it is stored here."),
        "a workspace held only elsewhere says so: {:?}",
        shown(&view)
    );
    // Which of the person's devices hold each workspace, this one by its own word.
    assert!(
        contains(
            &view,
            "Kept on this device, Ada's laptop - Also on another device: 4 KB"
        ),
        "both devices hold the studio: {:?}",
        shown(&view)
    );
    assert!(
        contains(
            &view,
            "Kept on Ada's laptop - nothing of it is stored here."
        ),
        "the archive is only on the laptop: {:?}",
        shown(&view)
    );
    // The other devices' copies: how much each holds, and when it last said so.
    assert!(has(&view, "Your other devices"), "{:?}", shown(&view));
    assert!(has(&view, "Ada's laptop"), "{:?}", shown(&view));
    assert!(
        contains(&view, "It holds studio: 4 KB, archive: 2 KB"),
        "{:?}",
        shown(&view)
    );
    assert!(
        contains(&view, "last seen"),
        "the reading says when the device last said so: {:?}",
        shown(&view)
    );
}

#[test]
fn a_workspace_is_kept_here_or_its_fetched_files_freed_on_the_storage_resource() {
    let (host, view) = installed(READING);
    let sent = Sent::default();
    host.runtime().set_invoker(sent.clone());

    let free = menu_item(&view, "studio", "Free 3 KB");
    view.fire(&free, "choose")
        .expect("freeing the fetched files");
    let asked = sent.asked("free-workspace-cache");
    assert_eq!(
        asked
            .iter()
            .find(|(named, _)| named == "workspace")
            .map(|(_, value)| value),
        Some(&Value::text("studio")),
        "{asked:?}"
    );
    assert_eq!(
        sent.argument("free-workspace-cache", "workspace"),
        Value::text("studio")
    );

    // A workspace kept here is offered the end of keeping it, and the other the start.
    let stop = menu_item(&view, "studio", "Stop keeping here");
    view.fire(&stop, "choose").expect("stopping keeping it");
    assert_eq!(
        sent.argument("keep-here", "workspace"),
        Value::text("studio")
    );
    assert_eq!(sent.argument("keep-here", "keep"), Value::Bool(false));

    let keep = menu_item(&view, "archive", "Keep here");
    view.fire(&keep, "choose").expect("keeping it");
    assert_eq!(
        sent.argument("keep-here", "workspace"),
        Value::text("archive")
    );
    assert_eq!(sent.argument("keep-here", "keep"), Value::Bool(true));

    // Nothing fetched is offered for freeing: the archive holds no cache, so its menu has no
    // free entry, while the studio's has one.
    let view = open_storage_pane(&host);
    let archive = menu_labels(&view, "archive");
    assert!(
        !archive.iter().any(|label| label.starts_with("Free ")),
        "no fetched file to free: {archive:?}"
    );
    let studio = menu_labels(&view, "studio");
    assert!(
        studio.iter().any(|label| label == "Free 3 KB"),
        "the fetched files of the studio are offered: {studio:?}"
    );
}

/// The labels of the items under `row`'s own menu.
fn menu_labels(view: &ViewInstance, row: &str) -> Vec<String> {
    let wanted = format!("More for {row}");
    let button = view
        .tree()
        .of_kind("ui.MenuButton")
        .into_iter()
        .find(|node| node.props.get("label") == Some(&Value::text(wanted.clone())))
        .unwrap_or_else(|| panic!("no menu for {row}"));
    button
        .of_kind("ui.MenuItem")
        .iter()
        .filter_map(|item| match item.props.get("label") {
            Some(Value::Text(label)) => Some(label.clone()),
            _ => None,
        })
        .collect()
}

/// The node under `view` whose kind is `kind` and whose `prop` is `value`.
fn node_with(view: &ViewInstance, kind: &str, prop: &str, value: &str) -> UiNode {
    let found = view
        .tree()
        .of_kind(kind)
        .into_iter()
        .find(|node| node.props.get(prop) == Some(&Value::text(value.to_owned())))
        .unwrap_or_else(|| {
            let shown: Vec<_> = view
                .tree()
                .of_kind(kind)
                .into_iter()
                .map(|node| node.props.get(prop).cloned())
                .collect();
            panic!("a {kind} whose {prop} is {value}: {shown:?}")
        });
    UiNode::clone(found)
}

/// The dropdown the pane captions `caption`.
fn dropdown(view: &ViewInstance, caption: &str) -> String {
    node_with(view, "ui.Dropdown", "caption", caption).key
}

/// The button under `row` labelled `label`.
fn row_button(view: &ViewInstance, row: &str, label: &str) -> String {
    let listed = node_with(view, "ui.ListRow", "title", row);
    listed
        .of_kind("ui.Button")
        .into_iter()
        .find(|button| button.props.get("label") == Some(&Value::text(label.to_owned())))
        .unwrap_or_else(|| {
            let labels: Vec<_> = listed
                .of_kind("ui.Button")
                .into_iter()
                .map(|button| button.props.get("label").cloned())
                .collect();
            panic!("a {label} button under {row}: {labels:?}")
        })
        .key
        .clone()
}

#[test]
fn fetched_files_are_fetched_ahead_or_held_to_a_space_the_person_chooses() {
    let (host, view) = installed(READING);
    let sent = Sent::default();
    host.runtime().set_invoker(sent.clone());

    // The switch shows what the daemon does now, and turning it off asks the storage resource.
    let prefetch = node_with(&view, "ui.Toggle", "label", "Prefetch files");
    assert!(
        matches!(prefetch.props.get("shown"), Some(Value::Bool(true))),
        "prefetching is on as the daemon reports it: {:?}",
        prefetch.props.get("shown")
    );
    view.fire_with(&prefetch.key, "check", Some(&Value::Bool(false)))
        .expect("prefetching is turned off");
    assert_eq!(
        sent.argument("set-prefetch", "on"),
        Value::some(Value::Bool(false)),
        "the switch's own answer reaches the resource"
    );

    // The space shows the one chosen, and a choice is the action's.
    let space = dropdown(&view, "Space for fetched files");
    assert_eq!(
        view.find(&space).expect("the space").props.get("value"),
        Some(&Value::text("10 GB")),
        "the ten gigabytes the daemon holds fetched files to"
    );
    view.fire_with(&space, "change", Some(&Value::text("25 GB")))
        .expect("the space is chosen");
    assert_eq!(
        sent.argument("set-cache-size", "size"),
        Value::text("25 GB")
    );

    // A device that fetches nothing ahead says so on the same switch.
    let off = READING.replace("\"prefetch\": true", "\"prefetch\": false");
    let (_host, view) = installed(&off);
    let prefetch = node_with(&view, "ui.Toggle", "label", "Prefetch files");
    assert!(
        matches!(prefetch.props.get("shown"), Some(Value::Bool(false))),
        "prefetching is off as the daemon reports it: {:?}",
        prefetch.props.get("shown")
    );
}

#[test]
fn a_device_whose_files_cannot_be_read_offers_no_prefetch_switch() {
    let reading = READING.replace(
        r#""cache_policy": {"prefetch": true, "budget_bytes": 10000000000},"#,
        r#""cache_policy": null,"#,
    );
    let reading = reading.replace(
        r#""sync_error": null"#,
        r#""sync_error": "No workspace is mounted on this device""#,
    );
    let (_host, view) = installed(&reading);
    assert!(
        view.tree()
            .of_kind("ui.Toggle")
            .iter()
            .all(|node| node.props.get("label") != Some(&Value::text("Prefetch files"))),
        "no switch for what cannot be read: {:?}",
        shown(&view)
    );
    assert!(
        has(&view, "No workspace is mounted on this device"),
        "{:?}",
        shown(&view)
    );
}

#[test]
fn the_fetched_files_that_can_be_freed_are_freed_one_by_one_or_all_at_once() {
    let (host, view) = installed(READING);
    let sent = Sent::default();
    host.runtime().set_invoker(sent.clone());

    // Each file the device may free is listed with the workspace it is in and its size.
    assert!(has(&view, "/films/rough-cut.mov"), "{:?}", shown(&view));
    assert!(contains(&view, "studio - 2 KB"), "{:?}", shown(&view));
    assert!(contains(&view, "studio - 1 KB"), "{:?}", shown(&view));

    let one = row_button(&view, "/films/rough-cut.mov", "Free");
    view.fire(&one, "press").expect("the file is freed");
    assert_eq!(
        sent.argument("free-file", "workspace"),
        Value::text("studio")
    );
    assert_eq!(
        sent.argument("free-file", "path"),
        Value::text("/films/rough-cut.mov")
    );

    let all = row_button(&view, "Free fetched files", "Free 3 KB");
    view.fire(&all, "press").expect("every file is freed");
    let freeing_all = sent.asked("free-synced-files");
    assert!(
        freeing_all.iter().all(|(_, value)| matches!(
            value,
            Value::Ref(target) if target.target.id_string() == storage().id_string()
        )),
        "freeing every file names no file, only the storage resource: {freeing_all:?}"
    );
}

#[test]
fn a_device_that_cannot_read_what_it_holds_says_why() {
    let reading = READING.replace(
        "\"residency_error\": null",
        "\"residency_error\": \"No workspace is mounted on this device\"",
    );
    let (_host, view) = installed(&reading);
    assert!(
        has(&view, "No workspace is mounted on this device"),
        "{:?}",
        shown(&view)
    );
}

/// The key of the button the pane shows labelled `label`.
fn button(view: &ViewInstance, label: &str) -> String {
    view.tree()
        .of_kind("ui.Button")
        .into_iter()
        .find(|button| button.props.get("label") == Some(&Value::text(label)))
        .unwrap_or_else(|| panic!("the pane offers a `{label}` button: {:?}", shown(view)))
        .key
        .clone()
}

/// The tools a person manages disks and backups in open their own app through the session, as
/// any other app is opened: the pane sends `open-app` for Disks and for Backups, in the order it
/// offers them, and says what each tool holds.
#[test]
fn the_pane_opens_disks_and_backups_through_the_session() {
    let (host, _) = settings();
    let sent = Sent::default();
    host.runtime().set_foreign_invoker(sent.clone());
    let view = open_storage_pane(&host);

    let disks = button(&view, "Open Disks");
    view.press(&disks).expect("the Disks row opens Disks");
    let backups = button(&view, "Open Backups");
    view.press(&backups).expect("the Backups row opens Backups");

    let opened: Vec<String> = sent
        .0
        .borrow()
        .iter()
        .filter_map(|invocation| match invocation {
            Invocation::Action { name, args, .. } if name == "open-app" => {
                args.first().and_then(|(_, app)| match app {
                    Value::Text(app) => Some(app.clone()),
                    _ => None,
                })
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        opened,
        ["so.tilo.disks", "so.tilo.backups"],
        "each row asks the session to open its app: {:?}",
        sent.0.borrow()
    );

    for expected in [
        "Storage tools",
        "Disks",
        "Partitions, formatting, drive health, and removable media",
        "Open Disks",
        "Backups",
        "Backup history, schedules, retention, and restore",
        "Open Backups",
        "Use Backups to protect files before making storage changes.",
    ] {
        assert!(
            has(&view, expected),
            "`{expected}` is shown: {:?}",
            shown(&view)
        );
    }
}
