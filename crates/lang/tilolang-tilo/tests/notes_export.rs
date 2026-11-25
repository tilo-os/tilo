//! Export is the system's, on every resource's menu: an app publishes each format it writes as
//! an export action (`export: "Markdown"`), the one Export… folds them into a submenu, and
//! choosing a format saves the file the action answers where the person says. Notes publishes
//! the note's Markdown and the notebook's zip and one Markdown document, and keeps no export
//! menu of its own.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_render_protocol::patch::local::{End, pair};
use tilo_render_protocol::patch::{self as wire, Level, ToApp, ToUi, Transport as _};
use tilolang_runtime::blob::{FileValue, SaveRequest};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{Actor, InvokeOutcome, UiNode, Value};
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::runtime::ids::MemberId;

const NOTES_APP: &str = "so.tilo.notes";
const LOCALE: &str = include_str!("../../../sdk/tilo-app-window/locales/en-US.ftl");

/// A file the person saved: the name it was offered under, its media type and what it holds.
#[derive(Debug)]
struct Saved {
    name: String,
    media: String,
    content: String,
}

/// A device that keeps the content an action makes and saves files, running `packages`,
/// speaking the platform's English words.
struct Device {
    packages: &'static [(&'static str, &'static str)],
    saved: RefCell<Vec<Saved>>,
    /// The content store: what an action made, by the address it was kept under.
    kept: RefCell<BTreeMap<String, Vec<u8>>>,
    next: Cell<u64>,
}

impl Platform for Device {
    fn load(&self, host: &Host) -> u64 {
        for (source, ids) in self.packages {
            host.load_builtin(source, ids).expect("the package loads");
        }
        0
    }

    fn phrase(&self, id: &str, _: &[(String, String)]) -> String {
        LOCALE
            .lines()
            .find_map(|line| line.strip_prefix(id)?.trim_start().strip_prefix('='))
            .map_or_else(|| id.to_owned(), |words| words.trim().to_owned())
    }

    fn now(&self) -> String {
        "2026-11-24T09:00:00Z".to_owned()
    }

    fn waker(&self) -> Waker {
        std::sync::Arc::new(|| {})
    }

    fn save_file(&self, request: &SaveRequest, content: &[u8]) -> Result<(), String> {
        self.saved.borrow_mut().push(Saved {
            name: request.name.clone(),
            media: request.media.clone(),
            content: String::from_utf8(content.to_vec()).expect("an export is text"),
        });
        Ok(())
    }

    fn read_content(&self, hash: &str) -> Option<Vec<u8>> {
        self.kept.borrow().get(hash).cloned()
    }

    fn keep_content(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        let next = self.next.get().saturating_add(1);
        self.next.set(next);
        let hash = format!("kept-{next}");
        self.kept.borrow_mut().insert(hash.clone(), bytes.to_vec());
        Ok(FileValue::new(
            name,
            media,
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            hash,
        ))
    }
}

/// Ada's window of `app` on a device running `packages`.
fn window(
    app: &str,
    packages: &'static [(&'static str, &'static str)],
) -> (Instance, End, Rc<Device>) {
    let (app_end, far) = pair();
    let device = Rc::new(Device {
        packages,
        saved: RefCell::new(Vec::new()),
        kept: RefCell::new(BTreeMap::new()),
        next: Cell::new(0),
    });
    let mut instance = Instance::new(
        Config {
            app: app.to_owned(),
            me: "ada".into(),
            mode: Mode::Live,
            device: None,
            store: tilolang_tilo::resources::ResourceStore::in_memory(),
            root: RootView::default(),
            sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
            link_home: None,
        },
        Rc::clone(&device) as Rc<dyn Platform>,
        Rc::new(tilolang_tilo::names::Installed),
        Box::new(app_end),
    );
    instance.sync();
    (instance, far, device)
}

/// `action` of `app` run by `who` with `args`: what it answered.
fn call(instance: &Instance, app: &str, who: &str, action: &str, args: Vec<Value>) -> Value {
    let host = instance.host().expect("the host");
    let id = host
        .package(app)
        .expect("the package")
        .meta
        .actions
        .iter()
        .find(|declared| declared.name == action)
        .unwrap_or_else(|| panic!("no action {action}"))
        .id
        .clone();
    match host
        .runtime()
        .invoke_action(&Actor::from(MemberId::person(who)), (id.as_str(), args))
        .unwrap_or_else(|error| panic!("{action}: {error}"))
    {
        InvokeOutcome::Done(value) => value,
        other => panic!("{action}: {other:?}"),
    }
}

fn target(value: &Value) -> ResourceRef {
    match value {
        Value::Ref(made) => (*made.target).clone(),
        other => panic!("a resource: {other:?}"),
    }
}

/// Tell the app what the window did, and let it answer.
fn tell(instance: &mut Instance, far: &mut End, message: ToApp) {
    far.send(wire::encode_batch(&[message]).expect("a batch"))
        .expect("the window tells the app");
    instance.update();
}

/// What the window was told since the last call.
fn told(far: &mut End) -> Vec<ToUi> {
    let mut messages = Vec::new();
    while let Ok(Some(batch)) = far.recv() {
        messages.extend(wire::decode_batch::<ToUi>(&batch).expect("a batch"));
    }
    messages
}

/// The formats the window's bar lists under the system's one Export…: each row's label and the
/// key pressing it fires, in the order the app declares them. The bar keeps the window's
/// resource's actions as its own buttons, and the rows of one run fold into one Export….
fn bar_export_rows(tree: &UiNode) -> Vec<(String, String)> {
    tree.of_kind("ui.Button")
        .into_iter()
        .filter(|row| row.props.get("in_menu") == Some(&Value::Int(1)))
        .filter(|row| row.props.get("group") == Some(&Value::text("Export…")))
        .filter_map(|row| {
            let label = row.props.get("label")?.as_text()?.to_owned();
            Some((label, row.key.clone()))
        })
        .collect()
}

/// Whether `node` presents `item`.
fn presents(node: &UiNode, item: &ResourceRef) -> bool {
    matches!(
        node.props.get("item"),
        Some(Value::Ref(r)) if r.target.id_string() == item.id_string()
    )
}

/// The rows of the menu on `item` under `node`: each one's label, the run it stands in and the
/// key choosing it fires. `None` for a tree that does not show the menu on `item`.
fn menu_rows(node: &UiNode, item: &ResourceRef) -> Option<Vec<(String, String, String)>> {
    if !presents(node, item) {
        return node
            .children
            .iter()
            .find_map(|child| menu_rows(child, item));
    }
    Some(
        node.of_kind("ui.MenuItem")
            .into_iter()
            .filter_map(|row| {
                let label = row.props.get("label")?.as_text()?.to_owned();
                let group = row
                    .props
                    .get("group")
                    .and_then(Value::as_text)
                    .unwrap_or_default()
                    .to_owned();
                Some((label, group, row.key.clone()))
            })
            .collect(),
    )
}

/// The rows of the menu on `item` as the tree row that shows it offers them, from whichever
/// list of the window's page holds it.
fn row_menu(instance: &mut Instance, item: &ResourceRef) -> Vec<(String, String, String)> {
    let view = instance.view(Level::Root).expect("the notebook's window");
    let tree = view.borrow().tree().clone();
    let lists: Vec<String> = ["ui.VirtualList", "ui.TreeView", "list"]
        .iter()
        .flat_map(|kind| tree.of_kind(kind).into_iter().map(|list| list.key.clone()))
        .collect();
    for list in lists {
        let len = view.borrow().list_len(&list).unwrap_or(0);
        for index in 0..len {
            let built = view.borrow_mut().row(&list, index).unwrap_or(None).cloned();
            if let Some(rows) = built.as_ref().and_then(|row| menu_rows(row, item)) {
                return rows;
            }
        }
    }
    panic!("no menu of {} is offered", item.id_string());
}

#[test]
fn a_notes_window_exports_each_format_its_app_publishes() {
    static PACKAGES: &[(&str, &str)] = &[crate::fixtures::DISCUSSION, crate::fixtures::NOTES];
    let (mut instance, mut far, device) = window(NOTES_APP, PACKAGES);
    let host = instance.host().expect("the host");
    let nb = Value::resource(&host.root_of(NOTES_APP).unwrap());
    let note = target(&call(
        &instance,
        NOTES_APP,
        "ada",
        "new_note",
        vec![nb, Value::text("Q3 plan")],
    ));
    call(
        &instance,
        NOTES_APP,
        "ada",
        "add_text",
        vec![Value::resource(&note), Value::text("Ship it.")],
    );
    instance.update();

    // The note's row lists the one format Notes publishes for a note, under the system's one
    // Export…, and no export row of the app's own.
    let rows = row_menu(&mut instance, &note);
    assert_eq!(
        rows.iter()
            .filter(|(_, group, _)| group == "Export…")
            .map(|(label, _, _)| label.clone())
            .collect::<Vec<_>>(),
        ["Markdown"],
        "the note's menu lists the format Notes publishes for it: {rows:?}"
    );
    let key = rows
        .iter()
        .find(|(label, group, _)| label == "Markdown" && group == "Export…")
        .map(|(_, _, key)| key.clone())
        .expect("the note's Markdown row");

    // Choosing it runs the export action and saves the file it answers where the person says,
    // under the file's own name.
    told(&mut far);
    tell(
        &mut instance,
        &mut far,
        ToApp::Fire {
            level: Level::Root,
            key,
            event: "choose".to_owned(),
            value: None,
            gesture: None,
        },
    );
    let mut saved = device.saved.borrow_mut();
    assert_eq!(saved.len(), 1, "one file is saved: {saved:?}");
    let file = saved.pop().unwrap();
    assert_eq!(file.name, "Q3 plan.md");
    assert_eq!(file.media, "text/markdown");
    assert_eq!(file.content, "# Q3 plan\n\nShip it.");
}

#[test]
fn a_notebooks_window_lists_the_formats_it_exports() {
    static PACKAGES: &[(&str, &str)] = &[crate::fixtures::DISCUSSION, crate::fixtures::NOTES];
    let (mut instance, _far, _device) = window(NOTES_APP, PACKAGES);
    let host = instance.host().expect("the host");
    host.root_of(NOTES_APP).expect("the notebook");
    instance.update();
    let view = instance.view(Level::Root).expect("the notebook's window");

    // The notebook's own bar lists both formats it publishes, together under the system's one
    // Export…: the zip of Markdown files and one Markdown document.
    let tree = view.borrow().tree().clone();
    assert_eq!(
        bar_export_rows(&tree)
            .into_iter()
            .map(|(label, _)| label)
            .collect::<Vec<_>>(),
        ["Zip of Markdown files", "One Markdown document"],
        "the notebook's bar lists the formats Notes publishes for it"
    );
}
