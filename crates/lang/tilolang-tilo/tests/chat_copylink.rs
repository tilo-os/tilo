//! Copy link is the system's, on every resource's menu: a Chat message's row offers it, and so
//! does a note's in Notes, though neither app writes one. Choosing it puts on the person's
//! clipboard the link other devices and people open the resource by, minted under the workspace's
//! page as the published `share-link` mints it.

use std::cell::RefCell;
use std::rc::Rc;

use crate::fixtures::target;
use crate::window_reads::told;
use tilo_reference::link::{Link, WebEntries, WebEntry};
use tilo_render_protocol::patch::local::{End, pair};
use tilo_render_protocol::patch::{self as wire, Level, ToApp, ToUi, Transport as _, Words};
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{Actor, InvokeOutcome, UiNode, Value};
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::runtime::ids::MemberId;
use tilolang_tilo::share::LinkHome;

const CHAT_APP: &str = "so.tilo.chat";
const NOTES_APP: &str = "so.tilo.notes";
const SITE: &str = "https://tilo.so/";
const LOCALE: &str = include_str!("../../../sdk/tilo-app-window/locales/en-US.ftl");

/// A device with a clipboard, running `packages`, speaking the platform's English words.
struct Device {
    packages: &'static [(&'static str, &'static str)],
    clipboard: RefCell<Vec<String>>,
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

    fn copy_clipboard(&self, mime: &str, bytes: &[u8]) -> bool {
        assert_eq!(mime, "text/plain", "a link is copied as text");
        self.clipboard
            .borrow_mut()
            .push(String::from_utf8(bytes.to_vec()).expect("a link is text"));
        true
    }
}

/// Ada's window of `app` on a device running `packages`, under the workspace's page.
fn window(
    app: &str,
    packages: &'static [(&'static str, &'static str)],
) -> (Instance, End, Rc<Device>) {
    let (app_end, far) = pair();
    let device = Rc::new(Device {
        packages,
        clipboard: RefCell::new(Vec::new()),
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
            link_home: Some(LinkHome {
                origin: "https://ada.tilo.cloud".to_owned(),
                workspace: "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90".to_owned(),
                host_device: "ab".repeat(32),
                web_entry: SITE.to_owned(),
            }),
        },
        Rc::clone(&device) as Rc<dyn Platform>,
        Rc::new(tilolang_tilo::names::Installed),
        Box::new(app_end),
    );
    instance.sync();
    (instance, far, device)
}

/// `action` of `app` run by Ada with `args`: what it answered.
fn call(instance: &Instance, app: &str, action: &str, args: Vec<Value>) -> Value {
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
        .invoke_action(&Actor::from(MemberId::person("ada")), (id.as_str(), args))
        .unwrap_or_else(|error| panic!("{action}: {error}"))
    {
        InvokeOutcome::Done(value) => value,
        other => panic!("{action}: {other:?}"),
    }
}

/// Tell the app what the window did, and let it answer.
fn tell(instance: &mut Instance, far: &mut End, message: ToApp) {
    far.send(wire::encode_batch(&[message]).expect("a batch"))
        .expect("the window tells the app");
    instance.update();
}

/// The menu command labelled `label` on the node presenting `item` under `node`, by its key.
fn command_on(node: &UiNode, item: &ResourceRef, label: &str) -> Option<String> {
    let presents = matches!(
        node.props.get("item"),
        Some(Value::Ref(r)) if r.target.id_string() == item.id_string()
    );
    if presents
        && let Some(found) = node
            .of_kind("ui.MenuItem")
            .into_iter()
            .find(|command| command.props.get("label") == Some(&Value::text(label)))
    {
        return Some(found.key.clone());
    }
    node.children
        .iter()
        .find_map(|child| command_on(child, item, label))
}

/// The menu of the row presenting `item`, opened as the person opens it at whichever level shows
/// it, its rows asked for as the window asks: the level and the commands' labels, and the key of
/// the one labelled `label`.
fn open_menu(
    instance: &mut Instance,
    far: &mut End,
    item: &ResourceRef,
    label: &str,
) -> (Level, Vec<String>, String) {
    for level in [Level::Detail, Level::Root] {
        let Some(view) = instance.view(level) else {
            continue;
        };
        // A row standing in the tree builds its menu with it.
        let tree = view.borrow().tree().clone();
        if let Some(key) = command_on(&tree, item, label) {
            return (level, labels(&tree, item), key);
        }
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
                if let Some(built) = built
                    && let Some(key) = command_on(&built, item, label)
                {
                    return (level, labels(&built, item), key);
                }
            }
        }
    }
    panic!("no menu of {} offers {label}", item.id_string());
}

/// The labels of the menu of the node presenting `item` under `node`.
fn labels(node: &UiNode, item: &ResourceRef) -> Vec<String> {
    let presents = matches!(
        node.props.get("item"),
        Some(Value::Ref(r)) if r.target.id_string() == item.id_string()
    );
    if presents {
        return node
            .of_kind("ui.MenuItem")
            .into_iter()
            .filter_map(|command| command.props.get("label")?.as_text().map(str::to_owned))
            .collect();
    }
    node.children
        .iter()
        .map(|child| labels(child, item))
        .find(|found| !found.is_empty())
        .unwrap_or_default()
}

/// Choose Copy link on the menu of `item`'s row, and read back the one link it copied.
fn copy_link(instance: &mut Instance, far: &mut End, device: &Device, item: &ResourceRef) -> Link {
    let (level, menu, key) = open_menu(instance, far, item, "Copy link");
    assert_eq!(
        menu.last().map(String::as_str),
        Some("Copy link"),
        "the system's command stands last, after the app's own: {menu:?}"
    );
    told(far);
    tell(
        instance,
        far,
        ToApp::Fire {
            level,
            key,
            event: "choose".to_owned(),
            value: None,
            gesture: None,
        },
    );
    let copied = device.clipboard.borrow().clone();
    let [url] = copied.as_slice() else {
        panic!("one link is copied: {copied:?}");
    };
    assert!(
        told(far).iter().any(|message| matches!(
            message,
            ToUi::Notice { words: Words::Phrase { id, .. }, .. } if id == "tilolang-link-copied"
        )),
        "the window says the link is copied"
    );
    let entries = WebEntries::new(vec![WebEntry::new(SITE).unwrap()]);
    Link::parse(url, &entries).expect("the copied link reads back as the workspace's link")
}

#[test]
fn a_messages_menu_copies_its_link() {
    static PACKAGES: &[(&str, &str)] = &[crate::fixtures::CHAT];
    let (mut instance, mut far, device) = window(CHAT_APP, PACKAGES);
    let host = instance.host().expect("the host");
    let all = Value::resource(&host.root_of(CHAT_APP).unwrap());
    let channel = call(
        &instance,
        CHAT_APP,
        "new_channel",
        vec![all, Value::text("design")],
    );
    let message = target(&call(
        &instance,
        CHAT_APP,
        "send",
        vec![
            channel.clone(),
            Value::Rich(RichValue::from_text("Lunch is at noon.")),
            Value::none(),
        ],
    ));
    let channel = target(&channel);
    tell(
        &mut instance,
        &mut far,
        ToApp::Open {
            resource: wire::ResourceRef {
                id: channel.id_string(),
                ty: channel.ty,
            },
        },
    );
    let link = copy_link(&mut instance, &mut far, &device, &message);
    let names = link.target.resource.expect("the link names the message");
    assert_eq!(names.kind, format!("tl.{CHAT_APP}.Message"));
    assert_eq!(names.id, format!("tl:{CHAT_APP}:{}", message.id_string()));
    assert_eq!(
        link.target
            .app
            .as_ref()
            .map(tilo_reference::link::AppId::as_str),
        Some(CHAT_APP)
    );
}

#[test]
fn a_notes_menu_copies_its_link_too() {
    static PACKAGES: &[(&str, &str)] = &[crate::fixtures::DISCUSSION, crate::fixtures::NOTES];
    let (mut instance, mut far, device) = window(NOTES_APP, PACKAGES);
    let host = instance.host().expect("the host");
    let notebook = Value::resource(&host.root_of(NOTES_APP).unwrap());
    let note = target(&call(
        &instance,
        NOTES_APP,
        "new_note",
        vec![
            notebook,
            Value::text("Plan"),
            Value::text(""),
            Value::none(),
            Value::none(),
        ],
    ));
    instance.update();
    let link = copy_link(&mut instance, &mut far, &device, &note);
    let names = link.target.resource.expect("the link names the note");
    assert_eq!(names.kind, format!("tl.{NOTES_APP}.Note"));
    assert_eq!(names.id, format!("tl:{NOTES_APP}:{}", note.id_string()));
}
