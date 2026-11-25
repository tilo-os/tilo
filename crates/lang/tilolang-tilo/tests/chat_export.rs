//! Export is the system's, on every resource's menu, beside Copy link: no app writes its own.
//! Choosing it saves the resource as a file where the person says. A Chat conversation saves as
//! the Markdown Chat says it exports as (its messages in order, each under who wrote it and
//! when), a note in Notes as the Markdown format it publishes, and a resource that holds no
//! document, a voice channel, as everything it holds in JSON.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_render_protocol::patch::local::{End, pair};
use tilo_render_protocol::patch::{self as wire, Level, ToApp, ToUi, Transport as _};
use tilolang_runtime::blob::{FileValue, SaveRequest};
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{Actor, InvokeOutcome, UiNode, Value};
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::runtime::ids::MemberId;

const CHAT_APP: &str = "so.tilo.chat";
const NOTES_APP: &str = "so.tilo.notes";
const LOCALE: &str = include_str!("../../../sdk/tilo-app-window/locales/en-US.ftl");

/// A file the person saved: the name it was offered under, its media type and what it holds.
#[derive(Debug)]
struct Saved {
    name: String,
    media: String,
    content: String,
}

/// A device that saves files, running `packages`, speaking the platform's English words.
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

fn rich(text: &str) -> Value {
    Value::Rich(RichValue::from_text(text))
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

/// Whether `node` presents `item`.
fn presents(node: &UiNode, item: &ResourceRef) -> bool {
    matches!(
        node.props.get("item"),
        Some(Value::Ref(r)) if r.target.id_string() == item.id_string()
    )
}

/// The menu of the node presenting `item` under `node`: its commands' labels, and the key of
/// the one labelled `label`.
fn menu_on(node: &UiNode, item: &ResourceRef, label: &str) -> Option<(Vec<String>, String)> {
    if presents(node, item) {
        let commands = node.of_kind("ui.MenuItem");
        let key = commands
            .iter()
            .find(|command| command.props.get("label") == Some(&Value::text(label)))?
            .key
            .clone();
        let labels = commands
            .iter()
            .filter_map(|command| command.props.get("label")?.as_text().map(str::to_owned))
            .collect();
        return Some((labels, key));
    }
    node.children
        .iter()
        .find_map(|child| menu_on(child, item, label))
}

/// The menu of the row or place presenting `item`, opened as the person opens it at whichever
/// level shows it, its rows asked for as the window asks: the level, the commands' labels, and
/// the key of the one labelled `label`.
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
        let tree = view.borrow().tree().clone();
        if let Some((labels, key)) = menu_on(&tree, item, label) {
            return (level, labels, key);
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
                    },
                );
                let built = view.borrow_mut().row(&list, index).unwrap().cloned();
                if let Some((labels, key)) = built.and_then(|row| menu_on(&row, item, label)) {
                    return (level, labels, key);
                }
            }
        }
    }
    panic!("no menu of {} offers {label}", item.id_string());
}

/// Choose Export… on the menu of `item`, and read back the one file it saved and the menu's
/// commands.
fn export(
    instance: &mut Instance,
    far: &mut End,
    device: &Device,
    item: &ResourceRef,
) -> (Saved, Vec<String>) {
    let (level, menu, key) = open_menu(instance, far, item, "Export…");
    assert_eq!(
        menu.iter()
            .rev()
            .take(2)
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["Copy link", "Export…"],
        "the system's commands close the menu, after the app's own: {menu:?}"
    );
    assert_eq!(
        menu.iter().filter(|label| *label == "Export…").count(),
        1,
        "the system's Export stands once: {menu:?}"
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
    let mut saved = device.saved.borrow_mut();
    assert_eq!(saved.len(), 1, "one file is saved: {saved:?}");
    (saved.pop().unwrap(), menu)
}

/// Choose the format `format` the system's Export… lists on the menu of `item`, and read back
/// the one file it saved and the menu's commands.
fn export_format(
    instance: &mut Instance,
    far: &mut End,
    device: &Device,
    item: &ResourceRef,
    format: &str,
) -> (Saved, Vec<String>) {
    let (level, menu, key) = open_menu(instance, far, item, format);
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
    let mut saved = device.saved.borrow_mut();
    assert_eq!(saved.len(), 1, "one file is saved: {saved:?}");
    (saved.pop().unwrap(), menu)
}

#[test]
fn a_channels_menu_exports_its_messages_as_markdown_in_order() {
    static PACKAGES: &[(&str, &str)] = &[crate::fixtures::CHAT];
    let (mut instance, mut far, device) = window(CHAT_APP, PACKAGES);
    let host = instance.host().expect("the host");
    let all = Value::resource(&host.root_of(CHAT_APP).unwrap());
    let design = call(
        &instance,
        CHAT_APP,
        "ada",
        "new_channel",
        vec![all, Value::text("design")],
    );
    call(
        &instance,
        CHAT_APP,
        "ben",
        "send",
        vec![design.clone(), rich("Shipping at four.")],
    );
    call(
        &instance,
        CHAT_APP,
        "ada",
        "send",
        vec![design.clone(), rich("Two lines:\nthis one too.")],
    );
    call(
        &instance,
        CHAT_APP,
        "ada",
        "relay",
        vec![
            design.clone(),
            rich("Morning."),
            Value::text("Robin"),
            Value::text("Discord"),
        ],
    );
    instance.update();
    let (file, menu) = export(&mut instance, &mut far, &device, &target(&design));
    assert!(
        !menu
            .iter()
            .any(|label| label.contains("Export") && label != "Export…"),
        "Chat writes no export of its own beside the system's: {menu:?}"
    );
    assert_eq!(file.name, "design.md");
    assert_eq!(file.media, "text/markdown");
    let text = file.content;
    assert!(text.starts_with("# #design\n\n"), "{text}");
    // Each message opens its own line, in the order they were said, under the name it shows
    // with and the time it was written; a message's later lines stand indented under it.
    let opens: Vec<&str> = text.lines().filter(|line| line.starts_with("- ")).collect();
    assert_eq!(opens.len(), 3, "{text}");
    let at = "(2026-";
    assert!(
        opens[0].starts_with(&format!("- **ben** {at}"))
            && opens[0].ends_with("): Shipping at four."),
        "{text}"
    );
    assert!(
        opens[1].starts_with(&format!("- **ada** {at}")) && opens[1].ends_with("): Two lines:"),
        "the person exporting is named as anyone else is, in a file they hand on: {text}"
    );
    assert!(text.contains("\n  this one too.\n"), "{text}");
    assert!(
        opens[2].starts_with("- **Robin**") && opens[2].ends_with(": Morning."),
        "a relayed message stands under the name it was written with elsewhere: {text}"
    );
}

#[test]
fn a_notes_menu_exports_its_body() {
    static PACKAGES: &[(&str, &str)] = &[crate::fixtures::DISCUSSION, crate::fixtures::NOTES];
    let (mut instance, mut far, device) = window(NOTES_APP, PACKAGES);
    let host = instance.host().expect("the host");
    let notebook = Value::resource(&host.root_of(NOTES_APP).unwrap());
    let note = target(&call(
        &instance,
        NOTES_APP,
        "ada",
        "new_note",
        vec![
            notebook,
            Value::text("Plan"),
            Value::text("Buy **milk**.\n\nCall Ben."),
            Value::none(),
            Value::none(),
        ],
    ));
    instance.update();
    let (file, _menu) = export_format(&mut instance, &mut far, &device, &note, "Markdown");
    assert_eq!(file.name, "Plan.md");
    assert_eq!(file.media, "text/markdown");
    assert!(
        file.content.starts_with("# Plan\n\n"),
        "the note's title heads its body: {}",
        file.content
    );
    assert!(
        file.content.contains("Buy **milk**.") && file.content.contains("Call Ben."),
        "the body keeps its marks: {}",
        file.content
    );
}

#[test]
fn a_resource_without_a_document_exports_what_it_holds_as_json() {
    static PACKAGES: &[(&str, &str)] = &[crate::fixtures::CHAT];
    let (mut instance, mut far, device) = window(CHAT_APP, PACKAGES);
    let host = instance.host().expect("the host");
    let all = Value::resource(&host.root_of(CHAT_APP).unwrap());
    let voice = target(&call(
        &instance,
        CHAT_APP,
        "ada",
        "new_voice_channel",
        vec![all, Value::text("Standup")],
    ));
    instance.update();
    let (file, _) = export(&mut instance, &mut far, &device, &voice);
    assert_eq!(file.name, "Standup.json");
    assert_eq!(file.media, "application/json");
    let read: serde_json::Value = serde_json::from_str(&file.content).expect("the export is JSON");
    assert_eq!(read["fields"]["name"], "Standup", "{read:#}");
    assert_eq!(
        read["item"],
        format!("tl:{CHAT_APP}:{}", voice.id_string()),
        "it names itself as the workspace publishes it: {read:#}"
    );
    assert_eq!(read["label"], "Standup", "{read:#}");
}
