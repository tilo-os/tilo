//! A window of a tilolang app on a device that saves files, and the rows the system's one
//! Export… puts in it: what every app's export test opens and reads.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_render_protocol::patch::local::{End, pair};
use tilo_render_protocol::patch::{self as wire, ToApp, Transport as _};
use tilolang_runtime::blob::{FileValue, SaveRequest};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{Actor, InvokeOutcome, UiNode, Value};
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::runtime::ids::MemberId;

const LOCALE: &str = include_str!("../../../../sdk/tilo-app-window/locales/en-US.ftl");

/// A device running `packages`, speaking the platform's English words, that keeps the content an
/// action makes and the names of the files an export saves.
pub struct Device {
    packages: &'static [(&'static str, &'static str)],
    pub saved: RefCell<Vec<String>>,
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

    fn save_file(&self, request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
        self.saved.borrow_mut().push(request.name.clone());
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
pub fn window(
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
pub fn call(instance: &Instance, app: &str, who: &str, action: &str, args: Vec<Value>) -> Value {
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

/// Tell the app what the window did, and let it answer.
pub fn tell(instance: &mut Instance, far: &mut End, message: ToApp) {
    far.send(wire::encode_batch(&[message]).expect("a batch"))
        .expect("the window tells the app");
    instance.update();
}

/// The formats the window lists under the system's one Export…, in the order the app declares
/// them. A window without a title line keeps its actions in the bar, as buttons of its overflow;
/// one with a title puts them behind the title's "…", as menu rows. Either way the rows of one
/// run fold into one Export….
pub fn export_formats(tree: &UiNode) -> Vec<String> {
    fn walk(node: &UiNode, out: &mut Vec<String>) {
        let a_row = matches!(node.kind.as_str(), "ui.Button" | "ui.MenuItem");
        if a_row
            && node.props.get("group") == Some(&Value::text("Export…"))
            && let Some(label) = node.props.get("label").and_then(Value::as_text)
        {
            out.push(label.to_owned());
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

/// Whether `node` presents `item`.
fn presents(node: &UiNode, item: &ResourceRef) -> bool {
    matches!(
        node.props.get("item"),
        Some(Value::Ref(r)) if r.target.id_string() == item.id_string()
    )
}

/// The rows of the menu on `item` under `node`: each one's label and the run it stands in.
/// `None` for a tree that does not show the menu on `item`.
fn menu_rows(node: &UiNode, item: &ResourceRef) -> Option<Vec<(String, String)>> {
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
                Some((label, group))
            })
            .collect(),
    )
}

/// The formats the menu on `item` lists under Export…, as the row that presents it offers them:
/// asked for as the window asks, from whichever level and list shows it.
pub fn row_export_formats(
    instance: &mut Instance,
    far: &mut End,
    item: &ResourceRef,
) -> Vec<String> {
    for level in [
        tilo_render_protocol::patch::Level::Detail,
        tilo_render_protocol::patch::Level::Root,
    ] {
        let Some(view) = instance.view(level) else {
            continue;
        };
        let tree = view.borrow().tree().clone();
        let lists: Vec<String> = ["ui.VirtualList", "ui.TreeView", "list"]
            .iter()
            .flat_map(|kind| tree.of_kind(kind).into_iter().map(|list| list.key.clone()))
            .collect();
        for list in lists {
            let len = view.borrow().list_len(&list).unwrap_or(0);
            for index in 0..len {
                let Some(row) = view.borrow().row_key(&list, index) else {
                    continue;
                };
                tell(
                    instance,
                    far,
                    ToApp::Rows {
                        level,
                        list: list.clone(),
                        start: 0,
                        end: u32::try_from(len).unwrap(),
                        open: None,
                        menu: Some(row),
                        store: 1,
                    },
                );
                let built = view.borrow_mut().row(&list, index).unwrap().cloned();
                if let Some(rows) = built.as_ref().and_then(|row| menu_rows(row, item)) {
                    return rows
                        .into_iter()
                        .filter(|(_, group)| group == "Export…")
                        .map(|(label, _)| label)
                        .collect();
                }
            }
        }
    }
    panic!("no menu of {} is offered", item.id_string());
}
