//! What opening a resource does (`opened action`): a window runs it as the person who opened the
//! resource, wherever they opened it from, here a search hit; one who may not change the resource
//! opens it all the same, unchanged. A window that opens on its root opens that root too.

use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use tilo_action_policy::{MemberAccess, Role, WorkspacePolicy};
use tilo_render_protocol::patch::local::pair;
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{Actor, Meta};
use tilolang_tilo::OpenEffect;
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::runtime::NameHost;
use tilolang_tilo::runtime::ids::MemberId;

use crate::fixtures::action_id;

const APP: &str = "t.opened";

/// An inbox whose conversations are read once someone opens them.
const SOURCE: &str = r#"app t.opened "Opened"

type Thread {
    subject: Text
    unread: Bool = true
}

type Inbox {
    threads: Collection<Thread>
    visits: Int merge counter = 0
}

/// A conversation arrives.
action receive(i: Inbox, subject: Text) { i.threads.insert(Thread(subject: subject)) }

/// Opening a conversation reads it.
opened action read_on_open(t: Thread) { t.unread = false }

/// Opening the inbox counts the visit.
opened action visit(i: Inbox) { i.visits += 1 }

root Inbox

default view inbox for Inbox

view inbox(i: Inbox) {
    page { text("{i.threads.len()}") }
}

default view thread for Thread

view thread(t: Thread) {
    page { text(t.subject) }
}
"#;

fn ids() -> &'static str {
    static IDS: OnceLock<String> = OnceLock::new();
    IDS.get_or_init(|| {
        tilolang::compile_package_with(
            SOURCE,
            &tilolang::IdsLock::new(),
            &[],
            &mut tilolang::SeededEntropy(11),
        )
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"))
        .ids
        .to_text()
    })
}

struct Opening;

impl Platform for Opening {
    fn load(&self, host: &Host) -> u64 {
        host.load_builtin(SOURCE, ids()).unwrap();
        0
    }

    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-11-22T12:00:00Z".to_owned()
    }

    fn waker(&self) -> Waker {
        Arc::new(|| {})
    }
}

struct Alone;

impl NameHost for Alone {
    fn name(&self, who: &Actor) -> String {
        who.id().to_owned()
    }
}

/// Ada's window of the inbox, over a store of its own.
fn window() -> Instance {
    let (app_end, _window) = pair();
    let mut instance = Instance::new(
        Config {
            app: APP.to_owned(),
            me: "ada".into(),
            mode: Mode::Live,
            device: None,
            store: tilolang_tilo::resources::ResourceStore::in_memory(),
            root: RootView::default(),
            sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
            link_home: None,
        },
        Rc::new(Opening),
        Rc::new(Alone),
        Box::new(app_end),
    );
    instance.sync();
    instance
}

/// A conversation about `subject` arrived in the inbox, as sol, who keeps the workspace, wrote it.
fn receive(window: &mut Instance, subject: &str) -> ResourceRef {
    let host = window.host().expect("the window opened");
    let meta: Meta = host.package(APP).unwrap().meta;
    let inbox = host.root_of(APP).unwrap();
    host.runtime()
        .invoke_action(
            &Actor::Member(MemberId::person("sol")),
            (
                action_id(&meta, "receive").as_str(),
                vec![Value::resource(&inbox), Value::text(subject)],
            ),
        )
        .unwrap();
    window.sync();
    let Value::List(threads) = host.runtime().read(&inbox, "threads").unwrap() else {
        panic!("the inbox lists its conversations");
    };
    threads
        .into_iter()
        .find_map(|thread| match thread {
            Value::Ref(r)
                if host.runtime().read(&r.target, "subject").unwrap() == Value::text(subject) =>
            {
                Some((*r.target).clone())
            }
            _ => None,
        })
        .expect("the conversation arrived")
}

/// Open `thread` as a search hit does: the window is asked to show it.
fn open_from_search(window: &mut Instance, thread: &ResourceRef) {
    window.open_requests().borrow_mut().push(OpenEffect::Open {
        anchor: thread.id_string(),
    });
    window.update();
    window.sync();
}

fn unread(window: &Instance, thread: &ResourceRef) -> Value {
    window
        .host()
        .unwrap()
        .runtime()
        .read(thread, "unread")
        .unwrap()
}

fn shows(window: &Instance, subject: &str) -> bool {
    window
        .detail_tree()
        .is_some_and(|tree| format!("{tree:?}").contains(subject))
}

#[test]
fn opening_from_search_runs_what_opening_does_as_the_opener() {
    let mut window = window();
    let lunch = receive(&mut window, "Lunch on Friday");
    assert_eq!(unread(&window, &lunch), Value::Bool(true));
    open_from_search(&mut window, &lunch);
    assert!(shows(&window, "Lunch on Friday"), "search opened it");
    assert_eq!(
        unread(&window, &lunch),
        Value::Bool(false),
        "opening it from search read it"
    );

    // Ada only views the workspace now: she opens the next conversation as before, and it
    // stays as it was, since she may not change it.
    let rota = receive(&mut window, "Rota");
    let mut policy = WorkspacePolicy::new("workspace", "sol");
    policy.members.insert("ada".to_owned(), Role::Viewer);
    window.set_access(Rc::new(move || {
        Some(MemberAccess::person(policy.clone(), "ada"))
    }));
    open_from_search(&mut window, &rota);
    assert!(shows(&window, "Rota"), "a viewer is never refused the open");
    assert_eq!(
        unread(&window, &rota),
        Value::Bool(true),
        "a viewer's open changes nothing"
    );
}

#[test]
fn a_window_opening_on_its_root_runs_what_opening_the_root_does() {
    let window = window();
    let host = window.host().expect("the window opened");
    let inbox = host.root_of(APP).unwrap();
    assert_eq!(
        host.runtime().read(&inbox, "visits").unwrap(),
        Value::Int(1),
        "the window opening on the inbox opened it, once"
    );
}

/// What code calls is never what opening does: it opens the resource instead.
#[test]
fn code_never_invokes_what_opening_does() {
    let source = SOURCE.replace(
        "action receive(i: Inbox, subject: Text) { i.threads.insert(Thread(subject: subject)) }",
        "action receive(i: Inbox, subject: Text) { i.threads.insert(Thread(subject: subject)) }\n\naction read_all(i: Inbox) { for t in i.threads { read_on_open(t) } }",
    );
    let Err(refused) = tilolang::compile_package(&source, &tilolang::IdsLock::new(), &[]) else {
        panic!("calling it is refused");
    };
    assert!(refused.iter().any(|d| d.code == "E143"), "{refused:#?}");
}
