//! Messages end to end: the window over the workspace's people and the person's messaging
//! accounts, the device's messaging engine answering it (history pages, sends, linking, groups),
//! Chat carrying what is written to a member, and the conversations agents hand the app to keep.
//!
//! The device here answers each op the way `tilo_messaging::engine` does, in its words; that
//! engine's own answers over a real broker are `messaging-protocol`'s tests.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use serde_json::json;
use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};

use tilolang_runtime::host_work::{EngineHost, JobHandlers};
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::{Actor, InvokeOutcome, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

use crate::fixtures::{drawn, set_reading};
use crate::invoker::{Sent, press, press_confirmed};

const APP: &str = "so.tilo.messages";
const CHAT: &str = "so.tilo.chat";
/// The person at the window, and the member they write to on Tilo.
const ADA: &str = "ada";
const GRACE: &str = "grace";

fn person(name: &str) -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person(name))
}

/// The package of `apps/tl/<dir>` as it ships, compiled against `metas`.
fn compiled(dir: &str, metas: &[tilolang::PackageMeta]) -> tilolang::Compiled {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../apps/tl")
        .join(dir);
    let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
    )
    .expect("its ids parse");
    tilolang::compile_package_source(&source, &ids, metas)
        .unwrap_or_else(|diagnostics| panic!("{}: {diagnostics:#?}", dir.display()))
}

/// One job the device runs for the app: what it was asked, how it stands, and the handlers its
/// start named.
#[derive(Clone)]
struct Job {
    op: String,
    input: serde_json::Value,
    state: String,
    detail: serde_json::Value,
    handlers: JobHandlers,
}

/// The device's messaging engine as the app reaches it: `call` answers the account states and
/// the cancels now; every other op is a job that waits until the test ends it.
#[derive(Clone, Default)]
struct Device {
    jobs: Rc<RefCell<BTreeMap<String, Job>>>,
    calls: Rc<RefCell<Vec<(String, serde_json::Value)>>>,
    /// Whether the messaging service runs here: without it the state says so.
    serving: Rc<RefCell<bool>>,
    /// The accounts' states, by account id.
    states: Rc<RefCell<BTreeMap<String, String>>>,
    /// The setup phase of each account, by account id: `empty` for none.
    phases: Rc<RefCell<BTreeMap<String, String>>>,
    /// The accounts that keep private groups.
    grouped: Rc<RefCell<Vec<String>>>,
}

impl Device {
    fn serving() -> Self {
        let device = Self::default();
        *device.serving.borrow_mut() = true;
        device
    }

    /// The jobs started for `op`, oldest first, by their paths.
    fn started(&self, op: &str) -> Vec<(String, serde_json::Value)> {
        self.jobs
            .borrow()
            .iter()
            .filter(|(_, job)| job.op == op)
            .map(|(path, job)| (path.clone(), job.input.clone()))
            .collect()
    }

    fn last(&self, op: &str) -> (String, serde_json::Value) {
        self.started(op)
            .pop()
            .unwrap_or_else(|| panic!("the app started `{op}`: {:?}", self.ops()))
    }

    fn ops(&self) -> Vec<String> {
        self.jobs
            .borrow()
            .values()
            .map(|job| job.op.clone())
            .collect()
    }

    fn record(path: &str, job: &Job) -> serde_json::Value {
        json!({"id": path, "op": job.op, "state": job.state, "detail": job.detail})
    }
}

impl EngineHost for Device {
    fn call(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
        if app != APP {
            return Err(format!("the engine is Messages', not {app}'s"));
        }
        let input: serde_json::Value = serde_json::from_str(input).unwrap_or_default();
        self.calls.borrow_mut().push((op.to_owned(), input.clone()));
        if op != "state" {
            return Ok("{}".to_owned());
        }
        let serving = *self.serving.borrow();
        let states = self.states.borrow();
        let grouped = self.grouped.borrow();
        let phases = self.phases.borrow();
        let accounts: Vec<_> = input["accounts"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|account| {
                let id = account["account"].as_str().unwrap_or_default();
                json!({
                    "service": account["service"],
                    "account": id,
                    "state": if serving { states.get(id).map_or("ready", String::as_str) } else { "unavailable" },
                    "phase": phases.get(id).map_or("empty", String::as_str),
                    "linked": false,
                    "linkable": serving,
                    "wake_ms": 0,
                    "groups": serving && grouped.iter().any(|held| held == id),
                })
            })
            .collect();
        Ok(json!({
            "available": serving,
            "first": serving,
            "sims": serving,
            "actor": "",
            "accounts": accounts,
            "routes": input["routes"].as_array().map_or(0, Vec::len)
                .eq(&0).then(Vec::<bool>::new).unwrap_or_else(|| vec![serving]),
        })
        .to_string())
    }

    fn start(
        &self,
        app: &str,
        op: &str,
        input: &str,
        handlers: &JobHandlers,
    ) -> Result<String, String> {
        if app != APP {
            return Err(format!("the engine is Messages', not {app}'s"));
        }
        let mut jobs = self.jobs.borrow_mut();
        let path = format!("devices/this/jobs/{:04}", jobs.len());
        jobs.insert(
            path.clone(),
            Job {
                op: op.to_owned(),
                input: serde_json::from_str(input).unwrap_or_default(),
                state: "running".to_owned(),
                detail: json!({}),
                handlers: handlers.clone(),
            },
        );
        Ok(path)
    }

    fn job(&self, job: &str) -> Result<Option<serde_json::Value>, String> {
        Ok(self
            .jobs
            .borrow()
            .get(job)
            .map(|held| Device::record(job, held)))
    }

    fn cancel(&self, job: &str) -> bool {
        self.jobs.borrow_mut().get_mut(job).is_some_and(|held| {
            "cancelled".clone_into(&mut held.state);
            true
        })
    }
}

/// The device's content store: what an export made, by its name.
#[derive(Clone, Default)]
struct Kept(Rc<RefCell<BTreeMap<String, Vec<u8>>>>);

impl BlobHost for Kept {
    fn read(&self, hash: &str) -> Option<Vec<u8>> {
        self.0.borrow().get(hash).cloned()
    }

    fn choose(&self, _: &FileRequest) -> Result<Vec<FileValue>, String> {
        Err("nobody chooses files here".to_owned())
    }

    fn save(&self, _: &SaveRequest, _: &[u8]) -> Result<(), String> {
        Ok(())
    }

    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        self.0.borrow_mut().insert(name.to_owned(), bytes.to_vec());
        Ok(FileValue::new(
            name,
            media,
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            name,
        ))
    }
}

/// Ada's replica with Contacts' directory, the workspace's accounts and members, Chat and
/// Messages loaded, and the device's engine answering Messages.
struct Window {
    host: Host,
    root: ResourceRef,
    device: Device,
    /// What Messages asked of the platform's packages: the directory's verbs, the session's.
    asked: Sent,
    kept: Kept,
}

/// The account Ada's Signal messages go from, and her `WhatsApp` account.
const SIGNAL: &str = "acct-signal";
const WHATSAPP: &str = "acct-whatsapp";
/// The scope both are confirmed under, and Bo's identity there.
const SCOPE: &str = "scope-1";
const BO_SIGNAL: &str = "+15550100";

fn account(id: &str, service: &str, name: &str) -> serde_json::Value {
    json!({
        "id": id, "kind": "other", "name": name, "address": "", "username": "", "provider": "",
        "status": "connected", "words": "", "enabled": true, "incoming": "", "outgoing": "",
        "identities": [], "calendars": [], "services": {"mail": false, "calendar": false, "contacts": false, "files": false},
        "files_mode": "", "paused": false, "service": service, "scope": SCOPE,
    })
}

fn endpoint(id: &str, service: &str, recipient: &str, revision: &str) -> serde_json::Value {
    json!({"id": id, "service": service, "recipient": recipient, "scope": SCOPE, "revision": revision, "removed": false})
}

fn contact(id: &str, label: &str, endpoints: Vec<serde_json::Value>) -> serde_json::Value {
    json!({"id": id, "label": label, "removed": false, "phones": [], "emails": [], "photo": "", "photo_size": 0, "endpoints": endpoints})
}

impl Window {
    fn new(device: Device) -> Self {
        Self::reading(device, "en-US")
    }

    /// The window as a person reading `locale` sees it.
    fn reading(device: Device, locale: &str) -> Self {
        tilo_ui_base::i18n::I18nManager::install("en-US", locale, &[("en-US", "")])
            .expect("the words");
        let host = Host::new(person(ADA), || None).expect("a host");
        host.set_words(crate::words::Toolkit);
        let platform = tilolang_tilo::platform::platform_packages().expect("the platform packages");
        let chat_source = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/chat/app.tl"),
        )
        .expect("Chat's source");
        let messages_source = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/messages/app.tl"),
        )
        .expect("Messages' source");
        let mut loaded: Vec<String> = Vec::new();
        for source in [&chat_source, &messages_source] {
            for meta in tilolang_tilo::platform::named_platform_packages(source)
                .expect("the platform's packages read")
            {
                if !loaded.contains(&meta.app) {
                    loaded.push(meta.app.clone());
                    host.runtime()
                        .load_foreign(meta)
                        .expect("a platform package loads");
                }
            }
        }
        let chat = compiled("chat", &platform);
        host.load_compiled(&chat).expect("Chat loads");
        let mut metas = platform;
        metas.push(chat.meta);
        let messages = compiled("messages", &metas);
        host.load_compiled(&messages).expect("Messages loads");
        host.runtime().set_engine(device.clone());
        let kept = Kept::default();
        host.runtime().set_blobs(kept.clone());
        host.add_workspace_member(&person(ADA))
            .expect("Ada is a member");
        host.add_workspace_member(&person(GRACE))
            .expect("Grace is a member");
        set_reading(
            &host,
            "accounts",
            &json!({"accounts": [account(SIGNAL, "signal", "Ada's Signal"), account(WHATSAPP, "whatsapp", "Ada's WhatsApp")], "connected": []}),
        );
        let root = host.root_of(APP).expect("the app's root");
        let asked = Sent::default();
        host.runtime().set_foreign_invoker(asked.clone());
        let window = Self {
            host,
            root,
            device,
            asked,
            kept,
        };
        window.directory(vec![
            contact(
                "bo",
                "Bo Diaz",
                vec![endpoint("bo-signal", "signal", BO_SIGNAL, "rev-1")],
            ),
            contact("cy", "Cy Park", vec![]),
        ]);
        window
    }

    fn directory(&self, people: Vec<serde_json::Value>) {
        self.host
            .runtime()
            .set_reading(
                &ResourceRef::parse_id(
                    "contacts:tilo.contacts.identities",
                    "so.tilo.contacts.directory#Identities",
                )
                .expect("the directory's id"),
                &json!({"people": people}).to_string(),
            )
            .expect("the directory reads");
    }

    fn root(&self) -> Value {
        Value::resource(&self.root)
    }

    fn try_call(&self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        let meta = self.host.package(APP).expect("Messages").meta;
        let id = meta
            .actions
            .iter()
            .find(|action| action.name == name)
            .ok_or_else(|| format!("no action {name}"))?
            .id
            .clone();
        match self
            .host
            .runtime()
            .invoke_action(&person(ADA), (id.as_str(), args))
        {
            Ok(InvokeOutcome::Done(value)) => Ok(value),
            Ok(other) => Err(format!("{other:?}")),
            Err(why) => Err(why.to_string()),
        }
    }

    fn call(&self, name: &str, args: Vec<Value>) -> Value {
        self.try_call(name, args)
            .unwrap_or_else(|why| panic!("{name}: {why}"))
    }

    /// The words of each message `found` names, as a caller that follows its answer reads them.
    fn bodies(&self, found: &Value) -> Vec<String> {
        let Value::List(items) = found else {
            panic!("a search answers a list: {found:?}")
        };
        items
            .iter()
            .map(|item| {
                let Value::Ref(message) = item else {
                    panic!("a search answers the messages it found: {item:?}")
                };
                match self.host.runtime().read(&message.target, "body") {
                    Ok(Value::Text(body)) => body,
                    other => panic!("a found message reads its words: {other:?}"),
                }
            })
            .collect()
    }

    fn refused(&self, name: &str, args: Vec<Value>) -> String {
        match self.try_call(name, args) {
            Ok(value) => panic!("{name} ran: {value:?}"),
            Err(why) => why,
        }
    }

    fn window(&self) -> ViewInstance {
        let view = self
            .host
            .package(APP)
            .expect("Messages")
            .meta
            .views
            .iter()
            .find(|view| view.name == "messages_window")
            .expect("the window view")
            .id
            .clone();
        // Opened the way the system opens an app: its root's `opened action` runs as the person,
        // then the window shows.
        let opened = self
            .host
            .runtime()
            .opened_action(&self.root.ty)
            .expect("Messages starts the person's window when it opens");
        match self
            .host
            .runtime()
            .invoke_action(&person(ADA), (opened.as_str(), vec![self.root()]))
        {
            Ok(InvokeOutcome::Done(_)) => {}
            other => panic!("opening Messages: {other:?}"),
        }
        let mut window = self
            .host
            .runtime()
            .open_view(&person(ADA), &view, vec![self.root()])
            .expect("the window opens");
        window.poll_patches().expect("the window answers");
        // The device hands the window every job it reads, as the app's host follows them.
        self.host.runtime().take_wanted_jobs();
        for (path, job) in self.device.jobs.borrow().iter() {
            self.host
                .runtime()
                .set_job(path, &Device::record(path, job));
        }
        window.poll_patches().expect("the window answers");
        window
    }

    /// The job `path` ended as `state` with `detail`; the handler its start named runs.
    fn ends(&self, path: &str, state: &str, detail: serde_json::Value) {
        let (handlers, record) = {
            let mut jobs = self.device.jobs.borrow_mut();
            let job = jobs.get_mut(path).expect("the job runs");
            state.clone_into(&mut job.state);
            job.detail = detail;
            (job.handlers.clone(), Device::record(path, job))
        };
        if let Some(done) = handlers.done {
            let call = self
                .host
                .runtime()
                .job_call(&done, &record)
                .expect("the handler's call");
            match self.host.runtime().invoke_action(&person(ADA), call) {
                Ok(InvokeOutcome::Done(_)) => {}
                other => panic!("the handler of {path} runs: {other:?}"),
            }
        }
    }
}

/// `text` as a person reads it: without the marks that keep a name's direction apart from the
/// sentence around it.
fn plain(text: &str) -> String {
    text.replace(['\u{2068}', '\u{2069}'], "")
}

/// Every text under `node` that the window shows, in order.
fn words(node: &UiNode, into: &mut Vec<String>) {
    for prop in [
        "value",
        "title",
        "label",
        "message",
        "action",
        "description",
        "said",
        "secondary",
        "caption",
        "alt",
        "author",
        "text",
    ] {
        if let Some(Value::Text(text)) = node.props.get(prop) {
            into.push(plain(text));
        }
    }
    // What a choice offers is shown too, as a person opening it reads it.
    if let Some(Value::List(options)) = node.props.get("options") {
        into.extend(options.iter().filter_map(|option| match option {
            Value::Text(text) => Some(plain(text)),
            _ => None,
        }));
    }
    for child in &node.children {
        words(child, into);
    }
}

fn shown(window: &ViewInstance) -> Vec<String> {
    let mut out = Vec::new();
    words(window.tree(), &mut out);
    out
}

fn shows(window: &ViewInstance, wanted: &str) -> bool {
    shown(window).iter().any(|text| text.contains(wanted))
}

#[track_caller]
fn assert_shows(window: &ViewInstance, wanted: &str) {
    assert!(
        shows(window, wanted),
        "the window shows {wanted:?}: {:?}\n{}",
        shown(window),
        drawn(window.tree())
    );
}

#[track_caller]
fn assert_hides(window: &ViewInstance, unwanted: &str) {
    assert!(
        !shows(window, unwanted),
        "the window shows no {unwanted:?}:\n{}",
        drawn(window.tree())
    );
}

fn nodes_of<'a>(node: &'a UiNode, kind: &str, into: &mut Vec<&'a UiNode>) {
    into.extend(node.of_kind(&format!("ui.{kind}")));
}

/// Type `words` into the window's composer, as a person does.
fn type_in(window: &ViewInstance, words: &str) {
    let input = window
        .tree()
        .first_of_kind("ui.RichInput")
        .unwrap_or_else(|| panic!("a composer:\n{}", drawn(window.tree())))
        .key
        .clone();
    window
        .edit(&input, &Value::Rich(RichValue::from_text(words)))
        .expect("the composer takes the words");
}

/// A page of Bo's Signal history as the engine answers it.
fn page(lines: &[(&str, &str, bool, &str)], more: bool) -> serde_json::Value {
    json!({
        "messages": lines.iter().enumerate().zip((1_790_000_000_000_i64..).step_by(60_000)).map(|((at, (id, text, incoming, delivery)), at_ms)| json!({
            "key": id, "text": text, "incoming": incoming, "at_ms": at_ms,
            "at": format!("2026-09-21T13:{:02}:00Z", at), "delivery": delivery,
        })).collect::<Vec<_>>(),
        "more": more,
    })
}

/// The key of the way of writing to `person` that `wanted` says, as the window lists them.
fn choice(window: &Window, person: &str, wanted: impl Fn(&serde_json::Value) -> bool) -> String {
    let listed = window.call("list_routes", vec![window.root(), Value::text(person)]);
    let Value::List(keys) = listed else {
        panic!("the routes are a list: {listed:?}")
    };
    keys.iter()
        .filter_map(|key| match key {
            Value::Text(key) => Some(key.clone()),
            _ => None,
        })
        .find(|key| wanted(&serde_json::from_str(key).expect("a choice is JSON")))
        .unwrap_or_else(|| panic!("a way of writing to {person}: {keys:?}"))
}

/// The words each line of the window's conversation says, in order.
fn shown_said(window: &ViewInstance) -> Vec<String> {
    let mut lines = Vec::new();
    nodes_of(window.tree(), "ConversationEntry", &mut lines);
    lines
        .iter()
        .filter_map(|line| match line.props.get("said") {
            Some(Value::Text(said)) => Some(said.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn with_nobody_yet_the_window_says_how_to_start() {
    let window = Window::new(Device::serving());
    window.directory(vec![]);
    window
        .host
        .remove_workspace_member(&person(GRACE))
        .expect("Grace leaves");
    let shown = window.window();
    assert_shows(&shown, "No conversations yet");
    assert_shows(&shown, "Add people in Contacts to start a conversation.");
    assert_shows(&shown, "Open Contacts");
    window.call("open_contacts", vec![window.root()]);
    let opened = window.asked.last_args("open-app");
    assert!(
        opened
            .iter()
            .any(|(_, value)| *value == Value::text("so.tilo.contacts")),
        "Open Contacts opens Contacts: {opened:?}"
    );
}

#[test]
fn members_come_first_then_the_address_books_people() {
    let window = Window::new(Device::serving());
    let shown = window.window();
    let mut items = Vec::new();
    nodes_of(shown.tree(), "NavItem", &mut items);
    let labels: Vec<String> = items
        .iter()
        .filter_map(|item| match item.props.get("label") {
            Some(Value::Text(label)) => Some(label.clone()),
            _ => None,
        })
        .collect();
    let grace = labels
        .iter()
        .position(|label| label.to_lowercase().contains(GRACE));
    let bo = labels.iter().position(|label| label == "Bo Diaz");
    assert!(
        grace.is_some() && bo.is_some() && grace < bo,
        "Grace, a member, stands before Bo of the address book: {labels:?}"
    );
    assert!(
        !labels
            .iter()
            .any(|label| label.to_lowercase().contains(ADA)),
        "Ada does not write to herself: {labels:?}"
    );
    assert_shows(&shown, "Choose a person");
    assert_shows(
        &shown,
        "Your conversations stay separate for each service and account.",
    );

    // An agent reads the same people, in the same order, each as its id and its name.
    let people = format!("{:?}", window.call("list_people", vec![window.root()]));
    let (member, contact) = (people.find("grace"), people.find("bo\\tBo Diaz"));
    assert!(
        member.is_some() && contact.is_some() && member < contact,
        "{people}"
    );

    // Back to the people from a person's conversation.
    press(&shown, "NavItem", "Bo Diaz");
    window.call("back", vec![window.root()]);
    assert_shows(&window.window(), "Choose a person");
}

/// Bo's Signal conversation: the one confirmed way of writing to him opens, its newest page and
/// then the page before it are read by the oldest message shown, and what is written waits in
/// the draft until the service took it, sent once however often Send is pressed.
#[test]
fn a_confirmed_address_is_read_page_by_page_and_written_to_once() {
    let window = Window::new(Device::serving());
    the_conversation_reads_page_by_page(&window);
    what_the_service_holds_stays_with_the_device(&window);
    let (sending, operation) = a_press_sends_one_operation(&window);
    an_uncertain_send_keeps_the_words(&window, &sending);
    a_send_lands_once_and_reads_again(&window, &operation);
    conflicting_associations_are_said_and_asked_again(&window);
    a_late_page_never_shows(&window);
    a_changed_identity_retires_the_route(&window);
}

/// The one confirmed way of writing to Bo opens on its newest page, and the page before it
/// stands above what is shown.
fn the_conversation_reads_page_by_page(window: &Window) {
    press(&window.window(), "NavItem", "Bo Diaz");
    let (newest, asked) = window.device.last("history");
    assert_eq!(asked["route"]["service"], "signal", "{asked}");
    assert_eq!(asked["route"]["sender"], SIGNAL, "{asked}");
    assert_eq!(asked["route"]["recipient"], BO_SIGNAL, "{asked}");
    assert_eq!(asked["route"]["revision"], "rev-1", "{asked}");
    assert_eq!(asked["route"]["scope"], SCOPE, "{asked}");
    assert_eq!(asked["before"], "", "the newest page first: {asked}");
    let shown = window.window();
    assert_shows(&shown, "Loading messages…");
    assert_shows(&shown, "Signal from Ada's Signal to +15550100");

    window.ends(
        &newest,
        "done",
        page(
            &[
                ("m3", "See you at noon", true, ""),
                ("m4", "Great, booked", false, "read"),
            ],
            true,
        ),
    );
    let shown = window.window();
    assert_eq!(shown_said(&shown), ["See you at noon", "Great, booked"]);
    assert_shows(&shown, "Load older messages");
    assert_hides(&shown, "This is the start of the conversation.");
    let mut lines = Vec::new();
    nodes_of(shown.tree(), "ConversationEntry", &mut lines);
    let delivered: Vec<_> = lines
        .iter()
        .filter_map(|line| line.props.get("delivery").cloned())
        .collect();
    assert_eq!(
        delivered.len(),
        1,
        "only the line Ada wrote says how it went: {delivered:?}"
    );

    press(&shown, "Button", "Load older messages");
    let (older, asked) = window.device.last("history");
    assert_ne!(older, newest);
    assert_eq!(
        asked["before"], "m3",
        "the page before the oldest shown: {asked}"
    );
    window.ends(
        &older,
        "done",
        page(&[("m1", "Lunch this week?", true, "")], false),
    );
    let shown = window.window();
    assert_eq!(
        shown_said(&shown),
        ["Lunch this week?", "See you at noon", "Great, booked"],
        "older lines stand above"
    );
    assert_shows(&shown, "This is the start of the conversation.");
}

/// What the service holds stays with the device: Messages keeps none of it, so neither a search
/// nor an export has a word of it.
fn what_the_service_holds_stays_with_the_device(window: &Window) {
    let found = window.call("search_messages", vec![window.root(), Value::text("noon")]);
    assert_eq!(window.bodies(&found), Vec::<String>::new());
    window.call(
        "export_content",
        vec![window.root(), Value::text("json"), Value::text("")],
    );
    let exported =
        String::from_utf8(window.kept.0.borrow()["messages.json"].clone()).expect("text");
    assert!(!exported.contains("See you at noon"), "{exported}");
}

/// The words wait in the draft, and one press sends one operation, however often Send is
/// pressed.
fn a_press_sends_one_operation(window: &Window) -> (String, String) {
    press(&window.window(), "Button", "Write a message");
    let shown = window.window();
    type_in(&shown, "On my way");
    let shown = window.window();
    assert_shows(&shown, "Draft: On my way");
    press_confirmed(&window.host, &shown, &person(ADA), "Button", "Send");
    let (sending, asked) = window.device.last("send");
    assert_eq!(asked["text"], "On my way", "{asked}");
    assert_eq!(asked["route"]["recipient"], BO_SIGNAL, "{asked}");
    let operation = asked["operation"]
        .as_str()
        .expect("an operation")
        .to_owned();
    assert!(!operation.is_empty());
    let shown = window.window();
    let mut buttons = Vec::new();
    nodes_of(shown.tree(), "Button", &mut buttons);
    let send = buttons
        .iter()
        .find(|node| node.props.get("label") == Some(&Value::text("Send")))
        .expect("the send button");
    assert!(
        window_refuses(&window.host, &shown, &send.key),
        "a second press waits for the first"
    );
    assert_eq!(window.device.started("send").len(), 1, "sent once");
    (sending, operation)
}

/// The service could not say whether the message went: the words stay, and the window says
/// to check.
fn an_uncertain_send_keeps_the_words(window: &Window, sending: &str) {
    window.ends(sending, "failed", json!({"why": "uncertain"}));
    let shown = window.window();
    assert_shows(&shown, "The outcome could not be confirmed.");
    assert_shows(&shown, "Draft: On my way");
}

/// A send that lands empties the draft and reads the conversation again.
fn a_send_lands_once_and_reads_again(window: &Window, operation: &str) {
    let shown = window.window();
    press_confirmed(&window.host, &shown, &person(ADA), "Button", "Send");
    let (sending, asked) = window.device.last("send");
    assert_ne!(
        asked["operation"], operation,
        "a new press is a new operation"
    );
    let reads = window.device.started("history").len();
    window.ends(
        &sending,
        "done",
        json!({"operation": asked["operation"], "status": "submitted", "page": null}),
    );
    assert_eq!(
        window.device.started("history").len(),
        reads.saturating_add(1),
        "read again after a send"
    );
    let shown = window.window();
    assert_hides(&shown, "Draft: On my way");
    assert_hides(&shown, "The outcome could not be confirmed.");
}

/// An address another contact claims too is refused in the window's words, and a new read
/// asks again.
fn conflicting_associations_are_said_and_asked_again(window: &Window) {
    press(&window.window(), "Button", "Refresh messages");
    let (refused, _) = window.device.last("history");
    window.ends(&refused, "failed", json!({"why": "revoked"}));
    let shown = window.window();
    assert_shows(
        &shown,
        "This service address has conflicting contact associations. Review them in Contacts.",
    );
    press(&shown, "Button", "Refresh messages");
    assert_hides(&window.window(), "conflicting contact associations");
}

/// A page that arrives after Ada left the conversation never shows.
fn a_late_page_never_shows(window: &Window) {
    let (late, _) = window.device.last("history");
    window.call("back", vec![window.root()]);
    window.ends(
        &late,
        "done",
        page(&[("m9", "A late line", true, "")], false),
    );
    press(&window.window(), "NavItem", "Bo Diaz");
    let shown = window.window();
    assert!(shown_said(&shown).is_empty(), "{:?}", shown_said(&shown));
    assert_shows(&shown, "Loading messages…");
}

/// Bo's Signal identity changed: the route retires with its history and its draft, and
/// nothing stands in for it until she chooses again.
fn a_changed_identity_retires_the_route(window: &Window) {
    let shown = window.window();
    type_in(&shown, "For the old address");
    assert_shows(&window.window(), "Draft: For the old address");
    window.directory(vec![contact(
        "bo",
        "Bo Diaz",
        vec![endpoint("bo-signal", "signal", BO_SIGNAL, "rev-2")],
    )]);
    let shown = window.window();
    assert_shows(
        &shown,
        "This way of writing to Bo Diaz is no longer confirmed.",
    );
    assert_hides(&shown, "For the old address");
    assert_hides(&shown, "Write a message");
    assert_hides(&shown, "Loading messages…");
    let old = window.device.started("history").len();
    assert!(
        window
            .refused("read_history", vec![window.root()])
            .contains("Choose a service"),
        "nothing is read on the retired route"
    );
    assert_eq!(window.device.started("history").len(), old);
}

/// Whether pressing `key` is refused: its action's requirement does not hold. What leaves the
/// system is confirmed first, so the person's yes is answered too, and the action that runs on it
/// is what refuses.
fn window_refuses(host: &Host, window: &ViewInstance, key: &str) -> bool {
    match window.fire(key, "press") {
        Ok(InvokeOutcome::Asked(_)) => {
            let action = window
                .event_action(key, "press")
                .expect("the control names its action");
            !matches!(
                host.runtime()
                    .invoke_approved_through(&person(ADA), None, action, None),
                Ok(InvokeOutcome::Done(_))
            )
        }
        other => !matches!(other, Ok(InvokeOutcome::Done(_))),
    }
}

/// An address two people claim, or one confirmed under another identity, reaches nobody: the
/// window offers the account without it and asks for a confirmed address, a forged choice is
/// refused, and confirming hands the directory exactly the reviewed address.
#[test]
fn an_address_reaches_its_one_confirmed_owner_and_nothing_is_substituted() {
    let window = Window::new(Device::serving());
    window.directory(vec![
        contact(
            "bo",
            "Bo Diaz",
            vec![endpoint("bo-signal", "signal", BO_SIGNAL, "rev-1")],
        ),
        contact(
            "cy",
            "Cy Park",
            vec![endpoint("cy-signal", "signal", BO_SIGNAL, "rev-1")],
        ),
    ]);
    let listed = window.call("list_routes", vec![window.root(), Value::text("bo")]);
    assert!(
        !format!("{listed:?}").contains(BO_SIGNAL),
        "a claimed-twice address is nobody's route: {listed:?}"
    );
    press(&window.window(), "NavItem", "Bo Diaz");
    assert!(
        window.device.started("history").is_empty(),
        "nothing is read without a route"
    );
    let shown = window.window();
    assert_shows(&shown, "Confirm a service address for this person.");
    assert_hides(&shown, "Write a message");

    // A choice the window never offered: Bo's address on Cy's association.
    let forged = json!({
        "service": "signal", "account": SIGNAL, "recipient": BO_SIGNAL,
        "association": "cy-signal", "scope": SCOPE, "revision": "rev-1",
    })
    .to_string();
    let refused = window.refused("select_route", vec![window.root(), Value::text(&forged)]);
    assert!(refused.contains("unavailable"), "{refused}");

    // Two identities confirmed for one address: neither is guessed.
    window.directory(vec![contact(
        "bo",
        "Bo Diaz",
        vec![
            endpoint("bo-signal", "signal", BO_SIGNAL, "rev-1"),
            endpoint("bo-old", "signal", BO_SIGNAL, "rev-0"),
        ],
    )]);
    let listed = window.call("list_routes", vec![window.root(), Value::text("bo")]);
    assert!(
        !format!("{listed:?}").contains(BO_SIGNAL),
        "two revisions: {listed:?}"
    );

    // Confirmed again: the directory is handed the reviewed address, as it was reviewed.
    window.directory(vec![contact(
        "bo",
        "Bo Diaz",
        vec![endpoint("bo-signal", "signal", BO_SIGNAL, "rev-2")],
    )]);
    let route = choice(&window, "bo", |c| c["recipient"] == BO_SIGNAL);
    window.call(
        "confirm_account",
        vec![window.root(), Value::text("bo"), Value::text(&route)],
    );
    let confirmed = window.asked.last_args("confirm-account");
    for (name, wanted) in [
        ("person", "bo"),
        ("endpoint", "bo-signal"),
        ("service", "signal"),
        ("scope", SCOPE),
        ("recipient", BO_SIGNAL),
        ("revision", "rev-2"),
    ] {
        assert!(
            confirmed
                .iter()
                .any(|(said, value)| said == name && *value == Value::text(wanted)),
            "{name} is {wanted}: {confirmed:?}"
        );
    }
    let half = json!({"service": "signal", "account": SIGNAL, "recipient": BO_SIGNAL}).to_string();
    let refused = window.refused(
        "confirm_account",
        vec![window.root(), Value::text("bo"), Value::text(&half)],
    );
    assert!(refused.contains("verified service identity"), "{refused}");
}

/// Grace is a member: Messages writes to her on Tilo, through Chat's conversation with her, and
/// shows what Chat holds of it.
#[test]
fn a_member_is_written_to_through_chat() {
    let window = Window::new(Device::serving());
    let grace_label = {
        let shown = window.window();
        let mut items = Vec::new();
        nodes_of(shown.tree(), "NavItem", &mut items);
        items
            .iter()
            .find_map(|item| match item.props.get("label") {
                Some(Value::Text(label)) if label.to_lowercase().contains(GRACE) => {
                    Some(label.clone())
                }
                _ => None,
            })
            .expect("Grace's row")
    };
    press(&window.window(), "NavItem", &grace_label);
    press(&window.window(), "Button", "Write a message");
    let shown = window.window();
    type_in(&shown, "Lunch at one?");
    let shown = window.window();
    press_confirmed(&window.host, &shown, &person(ADA), "Button", "Send");
    assert!(
        window.device.started("send").is_empty(),
        "a member is written to through Chat"
    );
    let shown = window.window();
    assert_eq!(
        shown_said(&shown),
        ["Lunch at one?"],
        "Chat's conversation shows here"
    );
    assert!(window.host.root_of(CHAT).is_ok(), "Chat keeps it");
}

/// Linking this device to an account: the private code shows only while it is the one to scan,
/// never in a capture, and finishing names the attempt the code was made for.
#[test]
fn linking_this_device_shows_a_private_code_until_it_is_finished() {
    let window = Window::new(Device::serving());
    press(&window.window(), "NavItem", "Accounts");
    let shown = window.window();
    assert_shows(&shown, "Your accounts");
    assert_shows(&shown, "Account ready");
    // Each state the service reports reads as what the person can do about it.
    for (state, words) in [
        (
            "linking",
            "Linking this account… Confirm the request on your phone.",
        ),
        ("locked", "Unlock this account to use messaging."),
        (
            "offline",
            "This account is offline. Your conversation will resume when it reconnects.",
        ),
        (
            "needs-relink",
            "Link this account again to resume messaging.",
        ),
        (
            "failed",
            "This account's connection stopped. Check its connection and account setup.",
        ),
        ("unknown", "Connection status is unavailable."),
    ] {
        window
            .device
            .states
            .borrow_mut()
            .insert(WHATSAPP.to_owned(), state.to_owned());
        window.call("list_account_states", vec![window.root()]);
        assert_shows(&window.window(), words);
    }
    window.device.states.borrow_mut().remove(WHATSAPP);
    window.call("list_account_states", vec![window.root()]);
    let shown = window.window();
    press(&shown, "ListRow", "Signal");
    press(&window.window(), "Button", "Link this device");
    let (link, asked) = window.device.last("link_begin");
    assert_eq!(asked["service"], "signal", "{asked}");
    assert_eq!(asked["account"], SIGNAL, "{asked}");
    window.ends(
        &link,
        "done",
        json!({"service": "signal", "account": SIGNAL, "attempt": "attempt-7", "generation": 1, "code": "sgnl://linkdevice?uuid=private"}),
    );
    let shown = window.window();
    assert_shows(
        &shown,
        "Scan this private code from Linked devices on your phone, then finish linking.",
    );
    let mut codes = Vec::new();
    nodes_of(shown.tree(), "QrCode", &mut codes);
    assert_eq!(codes.len(), 1, "{}", drawn(shown.tree()));
    assert_eq!(codes[0].props.get("secret"), Some(&Value::Bool(true)));
    assert!(
        shown.capture_protected(),
        "a capture leaves the private code out"
    );
    press(&shown, "Button", "Finish linking");
    let (finish, asked) = window.device.last("link_finish");
    assert_eq!(asked["attempt"], "attempt-7", "{asked}");
    window.ends(
        &finish,
        "done",
        json!({"service": "signal", "account": SIGNAL, "scope": SCOPE}),
    );
    let shown = window.window();
    assert_shows(
        &shown,
        "Device linked. Messaging resumes after its connection and inbox are ready.",
    );
    let mut codes = Vec::new();
    nodes_of(shown.tree(), "QrCode", &mut codes);
    assert!(codes.is_empty(), "the code is gone once it was used");
    press(&shown, "Button", "Close setup");
    assert_shows(&window.window(), "Choose a person");
}

/// A first `WhatsApp` account: its protected storage is reviewed, its code shown and refreshed,
/// and the linked identity confirmed.
#[test]
fn a_first_account_is_reviewed_linked_and_confirmed() {
    let window = Window::new(Device::serving());
    press(&window.window(), "NavItem", "Accounts");
    press(&window.window(), "ListRow", "Link a WhatsApp account");
    let (prepare, asked) = window.device.last("first_prepare");
    assert_eq!(asked["service"], "whatsapp", "{asked}");
    window.ends(
        &prepare,
        "done",
        json!({"service": "whatsapp", "account": "wa-1", "scope": "", "number": ""}),
    );
    let shown = window.window();
    assert_shows(
        &shown,
        "Review access to WhatsApp's private encrypted storage",
    );
    press(&shown, "Button", "Review protected storage access");
    let (review, asked) = window.device.last("first_review");
    assert_eq!(asked["account"], "wa-1", "{asked}");
    assert_eq!(asked["display_name"], "WhatsApp", "{asked}");
    window.ends(&review, "done", json!({"phase": "linking"}));
    let phase = |now: &str| {
        window
            .device
            .phases
            .borrow_mut()
            .insert("wa-1".to_owned(), now.to_owned());
        window.call("list_account_states", vec![window.root()]);
    };

    // Linking: the code is shown, refreshed, and the account finished by the attempt it names.
    phase("linking");
    press(&window.window(), "Button", "Show private linking code");
    let (begin, asked) = window.device.last("first_begin");
    assert_eq!(asked["service"], "whatsapp", "{asked}");
    let code = json!({"service": "whatsapp", "account": "wa-1", "attempt": "a-1", "generation": 1, "code": "2@private,code"});
    window.ends(&begin, "done", code);
    let shown = window.window();
    assert!(
        shown.capture_protected(),
        "a capture leaves the private code out"
    );
    assert_shows(&shown, "Refresh private linking code");
    press(&shown, "Button", "Refresh private linking code");
    let (refreshed, _) = window.device.last("first_begin");
    assert_ne!(refreshed, begin, "a new code is asked for");
    window.ends(&refreshed, "done", json!({"service": "whatsapp", "account": "wa-1", "attempt": "a-2", "generation": 2, "code": "2@fresh,code"}));
    press(&window.window(), "Button", "Finish linking");
    let (finish, asked) = window.device.last("first_finish");
    assert_eq!(
        asked["attempt"], "a-2",
        "the code shown is the one finished: {asked}"
    );
    assert_eq!(asked["account"], "wa-1", "{asked}");
    window.ends(
        &finish,
        "done",
        json!({"service": "whatsapp", "account": "wa-1", "scope": "wa-scope", "number": "+4915550100"}),
    );

    // The identity WhatsApp reports is confirmed, and the account is ready.
    phase("review");
    let shown = window.window();
    assert_shows(&shown, "Verify the selected service identity: +4915550100");
    let mut codes = Vec::new();
    nodes_of(shown.tree(), "QrCode", &mut codes);
    assert!(
        codes.is_empty(),
        "no code once it was used: {}",
        drawn(shown.tree())
    );
    press(&shown, "Button", "Confirm identity");
    let (confirm, _) = window.device.last("first_review");
    window.ends(&confirm, "done", json!({"phase": "ready"}));
    phase("ready");
    assert_shows(
        &window.window(),
        "Device linked. Messaging resumes after its connection and inbox are ready.",
    );
}

/// Signal shows its code at once, where `WhatsApp` first has its protected storage reviewed.
#[test]
fn a_signal_account_shows_its_code_at_once() {
    let window = Window::new(Device::serving());
    press(&window.window(), "NavItem", "Accounts");
    press(&window.window(), "ListRow", "Link a Signal account");
    let (_, asked) = window.device.last("first_begin");
    assert_eq!(asked["service"], "signal", "{asked}");
}

/// An SMS account lists the device's SIMs, or says there are none.
#[test]
fn an_sms_account_lists_the_sims_or_says_there_are_none() {
    let window = Window::new(Device::serving());
    press(&window.window(), "NavItem", "Accounts");
    press(&window.window(), "ListRow", "Choose an SMS SIM");
    let (sims, _) = window.device.last("first_sims");
    window.ends(
        &sims,
        "done",
        json!({"choices": [{"selection": "sim-0", "label": "Telekom"}]}),
    );
    press(&window.window(), "Button", "Use Telekom");
    let (_, asked) = window.device.last("first_select_sim");
    assert_eq!(asked["selection"], "sim-0", "{asked}");
    let window = Window::new(Device::serving());
    press(&window.window(), "NavItem", "Accounts");
    press(&window.window(), "ListRow", "Choose an SMS SIM");
    let (sims, _) = window.device.last("first_sims");
    window.ends(&sims, "done", json!({"choices": []}));
    assert_shows(
        &window.window(),
        "No available SIMs were found on this device.",
    );
}

/// A setup that stopped returns to choosing a service.
#[test]
fn a_setup_that_stopped_returns_to_choosing_a_service() {
    let window = Window::new(Device::serving());
    press(&window.window(), "NavItem", "Accounts");
    press(&window.window(), "ListRow", "Link a Signal account");
    let (begin, _) = window.device.last("first_begin");
    window.ends(&begin, "failed", json!({"why": "unavailable"}));
    let shown = window.window();
    assert_shows(
        &shown,
        "Linking stopped. Return to account selection to start a new attempt.",
    );
    press(&shown, "Button", "Return to account selection");
    assert_shows(&window.window(), "Link a WhatsApp account");
}

/// Press the button labelled `label` on a group's page, as the person does: the change it makes
/// leaves the system, so their yes answers the confirm the system raises.
fn press_group_change(window: &Window, shown: &ViewInstance, label: &str) {
    press_confirmed(&window.host, shown, &person(ADA), "Button", label);
}

/// The private groups of a `WhatsApp` account: listed, opened with their members' roles and what
/// was said, written to, and changed — an administrator removes someone else and adds a
/// reviewed contact; anyone leaves.
#[test]
fn private_groups_are_listed_opened_written_to_and_changed() {
    let device = Device::serving();
    device.grouped.borrow_mut().push(WHATSAPP.to_owned());
    let window = Window::new(device);
    window.directory(vec![contact(
        "bo",
        "Bo Diaz",
        vec![endpoint(
            "bo-wa",
            "whatsapp",
            "4915550100@s.whatsapp.net",
            "rev-1",
        )],
    )]);
    window.call("list_account_states", vec![window.root()]);
    press(&window.window(), "NavItem", "Private groups");
    let (listing, asked) = window.device.last("groups");
    assert_eq!(
        asked["account"], WHATSAPP,
        "only the account that keeps groups: {asked}"
    );
    assert_eq!(window.device.started("groups").len(), 1);
    window.ends(
        &listing,
        "done",
        json!({"service": "whatsapp", "account": WHATSAPP, "groups": [{"key": "g1", "title": "Climbing"}, {"key": "g2", "title": ""}]}),
    );
    let shown = window.window();
    assert_shows(&shown, "WhatsApp groups for Ada's WhatsApp");
    assert_shows(&shown, "Untitled group");
    press(&shown, "ListRow", "Climbing");
    let (membership, _) = window.device.last("group");
    let (history, asked) = window.device.last("group_history");
    assert_eq!(asked["group"], "g1", "{asked}");
    window.ends(
        &membership,
        "done",
        json!({"service": "whatsapp", "account": WHATSAPP, "group": "g1", "title": "Climbing", "announcement": false,
               "self_member": "me@s.whatsapp.net", "review": "review-1",
               "members": [{"address": "me@s.whatsapp.net", "role": "admin"}, {"address": "cy@s.whatsapp.net", "role": "member"}]}),
    );
    window.ends(
        &history,
        "done",
        json!({"group": "g1", "messages": [{"key": "x1", "text": "Saturday at ten?", "incoming": true, "at_ms": 0, "at": "2026-09-21T10:00:00Z", "delivery": "", "author": "cy@s.whatsapp.net"}]}),
    );
    let shown = window.window();
    assert_shows(&shown, "Administrator");
    assert_shows(&shown, "Member");
    assert_eq!(shown_said(&shown), ["Saturday at ten?"]);
    assert_shows(&shown, "Remove cy@s.whatsapp.net");
    assert_hides(&shown, "Remove me@s.whatsapp.net");
    assert_shows(
        &shown,
        "Add Bo Diaz using WhatsApp from Ada's WhatsApp to 4915550100@s.whatsapp.net",
    );

    press_group_change(&window, &shown, "Remove cy@s.whatsapp.net");
    let (_, asked) = window.device.last("group_change");
    assert_eq!(asked["change"], "remove", "{asked}");
    assert_eq!(asked["member"], "cy@s.whatsapp.net", "{asked}");
    assert_eq!(
        asked["review"], "review-1",
        "the change names the membership reviewed: {asked}"
    );

    let shown = window.window();
    press_group_change(
        &window,
        &shown,
        "Add Bo Diaz using WhatsApp from Ada's WhatsApp to 4915550100@s.whatsapp.net",
    );
    let (_, asked) = window.device.last("group_change");
    assert_eq!(asked["change"], "add", "{asked}");
    assert_eq!(
        asked["route"]["recipient"], "4915550100@s.whatsapp.net",
        "{asked}"
    );
    assert_eq!(asked["route"]["person"], "bo", "{asked}");

    press(&window.window(), "Button", "Write a message");
    type_in(&window.window(), "See you there");
    let shown = window.window();
    press_confirmed(&window.host, &shown, &person(ADA), "Button", "Send");
    let (_, asked) = window.device.last("group_send");
    assert_eq!(asked["text"], "See you there", "{asked}");
    assert_eq!(asked["review"], "review-1", "{asked}");

    let shown = window.window();
    press_confirmed(
        &window.host,
        &shown,
        &person(ADA),
        "Button",
        "Leave this group",
    );
    let (_, asked) = window.device.last("group_change");
    assert_eq!(asked["change"], "leave", "{asked}");
}

/// Without the messaging service on this device the setup says why, and offers nothing that
/// would only fail.
#[test]
fn without_the_messaging_service_setup_says_why() {
    let window = Window::new(Device::default());
    press(&window.window(), "NavItem", "Accounts");
    let shown = window.window();
    assert_shows(
        &shown,
        "Messaging isn't running here, so no account can be linked.",
    );
    assert_hides(&shown, "Link a WhatsApp account");
    assert_hides(&shown, "Choose an SMS SIM");
    assert_shows(&shown, "This account's messaging service is unavailable.");
}

/// What changes something at Signal, `WhatsApp` or SMS (a message sent, a group changed, a device
/// linked) is an outside effect, which a caller other than the person is asked about first;
/// keeping a conversation an agent hands Messages is not.
#[test]
fn what_reaches_a_messaging_service_is_asked_before_another_caller_runs_it() {
    let window = Window::new(Device::serving());
    let meta = window.host.package(APP).expect("Messages").meta;
    let class = |name: &str| {
        meta.actions
            .iter()
            .find(|action| action.name == name)
            .unwrap_or_else(|| panic!("no action {name}"))
            .class
    };
    for name in [
        "send",
        "begin_link",
        "finish_link",
        "begin_first_account",
        "select_sim",
        "send_to_group",
        "change_group",
    ] {
        assert_eq!(class(name), tilolang::types::EffectClass::Outside, "{name}");
    }
    for name in ["deliver_message", "send_message", "reply"] {
        assert_eq!(
            class(name),
            tilolang::types::EffectClass::Undoable,
            "{name}"
        );
    }
    for name in ["search_messages", "list_people", "list_routes"] {
        assert_eq!(class(name), tilolang::types::EffectClass::Read, "{name}");
    }
}

/// A German person reads the window, its setup and its refusals in German.
#[test]
fn a_german_window_reads_the_catalogue() {
    let window = Window::reading(Device::default(), "de-DE");
    let shown = window.window();
    assert_shows(&shown, "Person wählen");
    press(&shown, "NavItem", "Konten");
    let shown = window.window();
    assert_shows(&shown, "Messaging läuft hier nicht");
    let refused = window.refused(
        "reply",
        vec![
            window.root(),
            Value::text("+15550100"),
            Value::text("Hallo"),
        ],
    );
    assert!(refused.contains("keine Unterhaltung"), "{refused}");
}

/// What agents and automations hand Messages: deliveries kept once each and in the order they
/// were sent, refusals for what names nobody, queued replies, search and export.
#[test]
fn agents_keep_search_and_export_the_conversations_handed_to_messages() {
    let window = Window::new(Device::serving());
    deliveries_are_kept_once_each_in_the_order_they_were_sent(&window);
    deliveries_that_name_nobody_are_refused(&window);
    replies_reach_the_conversation_they_name(&window);
    a_search_reads_every_word_and_labels_each_hit(&window);
    exports_in_the_formats_a_caller_names(&window);
}

/// What an agent hands Messages lands in the conversation the sender and recipient name, once
/// each, in the order it was sent.
fn deliveries_are_kept_once_each_in_the_order_they_were_sent(window: &Window) {
    let deliver = |from: &str, body: &str, date: &str, to: &str| {
        window.call(
            "deliver_message",
            vec![
                window.root(),
                Value::text(from),
                Value::text(body),
                Value::text(to),
                Value::text(date),
                Value::text(""),
            ],
        )
    };
    let key = deliver(
        "+1 555 0100",
        "Tickets are 18 EUR each.",
        "2026-10-11T09:05:00Z",
        "",
    );
    assert_eq!(
        key,
        Value::text("+15550100"),
        "a number's conversation is its digits"
    );
    deliver(
        "+15550100",
        "Are we still on for Saturday?",
        "2026-10-11T09:00:00Z",
        "+15550199",
    );
    let again = deliver(
        "+15550100",
        "Are we still on for Saturday?",
        "2026-10-11T09:00:00Z",
        "+15550199",
    );
    assert_eq!(
        again, key,
        "the same delivery lands in the same conversation"
    );
    let shown = window.window();
    press(&shown, "NavItem", "+1 555 0100");
    let shown = window.window();
    assert_eq!(
        shown_said(&shown),
        ["Are we still on for Saturday?", "Tickets are 18 EUR each."],
        "kept once each, in the order they were sent"
    );
    assert_shows(&shown, "Kept in this copy of Messages.");
}

/// A delivery that names nobody, says nothing, or names no date is refused in words that say
/// what is missing.
fn deliveries_that_name_nobody_are_refused(window: &Window) {
    for (from, body, date, why) in [
        ("", "words", "", "Name who sent it"),
        ("Bo", " ", "", "A message says something"),
        ("Bo", "words", "yesterday-ish", "is not a date"),
        ("---", "words", "", "names no one"),
    ] {
        let refused = window.refused(
            "deliver_message",
            vec![
                window.root(),
                Value::text(from),
                Value::text(body),
                Value::text(""),
                Value::text(date),
                Value::text(""),
            ],
        );
        assert!(
            refused.contains(why),
            "{from:?} {body:?} {date:?}: {refused}"
        );
    }
}

/// A reply reaches the conversation it names and one naming no conversation is refused; writing
/// to someone new starts a conversation.
fn replies_reach_the_conversation_they_name(window: &Window) {
    window.call(
        "reply",
        vec![
            window.root(),
            Value::text("+15550100"),
            Value::text("Yes! Book two."),
        ],
    );
    let refused = window.refused(
        "reply",
        vec![
            window.root(),
            Value::text("nobody-here"),
            Value::text("Hello?"),
        ],
    );
    assert!(refused.contains("no conversation"), "{refused}");
    let made = window.call(
        "send_message",
        vec![
            window.root(),
            Value::text("Dana Fox"),
            Value::text("Welcome!"),
            Value::text(""),
        ],
    );
    assert_eq!(made, Value::text("dana-fox"));
    let shown = window.window();
    press(&shown, "NavItem", "+1 555 0100");
    let shown = window.window();
    assert_eq!(
        shown_said(&shown),
        [
            "Are we still on for Saturday?",
            "Tickets are 18 EUR each.",
            "Yes! Book two."
        ]
    );
}

/// A search reads every word in any case, and the caller's own answer labels each hit by its
/// words without a read of each.
fn a_search_reads_every_word_and_labels_each_hit(window: &Window) {
    let found = window.call(
        "search_messages",
        vec![window.root(), Value::text("TICKETS eur")],
    );
    assert_eq!(
        window.bodies(&found),
        ["Tickets are 18 EUR each."],
        "every word, in any case"
    );
    // A caller that asked reads each message it found by its words, without a read of each.
    let search = window
        .host
        .package(APP)
        .expect("Messages")
        .meta
        .actions
        .iter()
        .find(|action| action.name == "search_messages")
        .expect("search")
        .id
        .clone();
    let answered = window
        .host
        .action_output(APP, &search, Some(&found))
        .expect("the answer")
        .value
        .expect("a value");
    assert_eq!(
        answered[0]["label"], "Tickets are 18 EUR each.",
        "{answered}"
    );
    assert!(
        window
            .refused("search_messages", vec![window.root(), Value::text("  ")])
            .contains("some words")
    );
}

/// An export writes the format the caller names, and a format it does not have is refused.
fn exports_in_the_formats_a_caller_names(window: &Window) {
    for (format, holds) in [("md", "- **You**"), ("json", "\"queued\":true")] {
        let file = window.call(
            "export_content",
            vec![window.root(), Value::text(format), Value::text("+15550100")],
        );
        let text = format!("{file:?}");
        let name = format!("messages.{format}");
        assert!(text.contains(&name), "{format}: {text}");
        let written = String::from_utf8(window.kept.0.borrow()[&name].clone()).expect("text");
        assert!(written.contains(holds), "{format}: {written}");
        assert!(written.contains("Yes! Book two."), "{format}: {written}");
    }
    let refused = window.refused(
        "export_content",
        vec![window.root(), Value::text("pdf"), Value::text("")],
    );
    assert!(refused.contains("not a format"), "{refused}");
}
