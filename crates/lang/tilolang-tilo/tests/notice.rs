//! What an action says in passing (`notice`): the window the action ran from is sent the words,
//! their kind, and whether the notice offers the person's undo, as the one message its bar shows.

use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use tilo_render_protocol::patch::local::{End, pair};
use tilo_render_protocol::patch::{self as ui, NoticeKind, ToUi, Transport as _};
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{Actor, InvokeOutcome};
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::runtime::NameHost;
use tilolang_tilo::runtime::ids::MemberId;

const APP: &str = "t.notice";

/// A shelf of things, whose actions say what they did in passing.
const SOURCE: &str = r#"app t.notice "Notice"

type Thing {
    title: Text
}

type Shelf {
    things: Collection<Thing>
}

/// Put a thing on the shelf.
action add(s: Shelf, title: Text) -> Thing {
    s.things.insert(Thing(title: title))
}

/// Say in passing what was done, in a successful, a warning and an error tone.
action checked(t: Thing) {
    notice("Checked {t.title}", kind: .success)
}

/// One that offers the person's undo while it shows.
action cleared(t: Thing) {
    notice("Cleared {t.title}", kind: .warning, undo: true)
}

action unreadable(t: Thing) {
    notice("Could not read {t.title}", kind: .error)
}

root Shelf

default view shelf for Shelf

view shelf(s: Shelf) {
    page { text("Shelf") }
}

default view thing for Thing

view thing(t: Thing) {
    page { text(t.title) }
}
"#;

fn ids() -> &'static str {
    static IDS: OnceLock<String> = OnceLock::new();
    IDS.get_or_init(|| {
        tilolang::compile_package_with(
            SOURCE,
            &tilolang::IdsLock::new(),
            &[],
            &mut tilolang::SeededEntropy(17),
        )
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"))
        .ids
        .to_text()
    })
}

struct Shelf;

impl Platform for Shelf {
    fn load(&self, host: &Host) -> u64 {
        host.load_builtin(SOURCE, ids()).unwrap();
        0
    }

    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-11-24T09:00:00Z".to_owned()
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

/// Ada's window of the shelf, and the window's end of what it sends.
fn window() -> (Instance, End) {
    let (app_end, window_end) = pair();
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
        Rc::new(Shelf),
        Rc::new(Alone),
        Box::new(app_end),
    );
    instance.sync();
    (instance, window_end)
}

/// What the shelf's action `action` answered, run by Ada.
fn call(instance: &Instance, action: &str, args: Vec<Value>) -> Value {
    let host = instance.host().expect("the window opened");
    let id = host
        .package(APP)
        .expect("the shelf package")
        .meta
        .actions
        .iter()
        .find(|held| held.name == action)
        .unwrap_or_else(|| panic!("the action `{action}`"))
        .id
        .clone();
    match host
        .runtime()
        .invoke_action(&Actor::Member(MemberId::person("ada")), (id.as_str(), args))
    {
        Ok(InvokeOutcome::Done(value)) => value,
        other => panic!("`{action}` answered {other:?}"),
    }
}

/// The notices the window was sent since it was last read: each one's words, kind and whether it
/// offers the undo.
fn said(window: &mut End) -> Vec<(String, NoticeKind, bool)> {
    let mut out = Vec::new();
    while let Some(batch) = window.recv().expect("the window reads its batches") {
        for message in ui::decode_batch::<ToUi>(&batch).expect("a batch of the window's") {
            if let ToUi::Notice { words, kind, undo } = message {
                let ui::Words::Plain(text) = words else {
                    panic!("a notice the app said is its words, not a phrase: {words:?}");
                };
                out.push((text, kind, undo));
            }
        }
    }
    out
}

/// One thing on the shelf, made by its own action, and its reference.
fn thing(instance: &Instance) -> ResourceRef {
    let root = instance
        .host()
        .expect("the window opened")
        .root_of(APP)
        .expect("the shelf");
    match call(
        instance,
        "add",
        vec![Value::resource(&root), Value::text("Take the bins out")],
    ) {
        Value::Ref(thing) => (*thing.target).clone(),
        other => panic!("a thing: {other:?}"),
    }
}

#[test]
fn what_an_action_says_in_passing_is_the_windows_notice_with_its_kind_and_undo() {
    let (mut instance, mut window) = window();
    let thing = thing(&instance);
    let _ = said(&mut window);

    call(&instance, "checked", vec![Value::resource(&thing)]);
    instance.flush();
    assert_eq!(
        said(&mut window),
        [(
            "Checked Take the bins out".to_owned(),
            NoticeKind::Success,
            false
        )],
        "the window is sent the words the action wrote, with the tone it gave"
    );

    call(&instance, "cleared", vec![Value::resource(&thing)]);
    instance.flush();
    assert_eq!(
        said(&mut window),
        [(
            "Cleared Take the bins out".to_owned(),
            NoticeKind::Warning,
            true
        )],
        "and the undo it offers beside them"
    );

    call(&instance, "unreadable", vec![Value::resource(&thing)]);
    instance.flush();
    assert_eq!(
        said(&mut window),
        [(
            "Could not read Take the bins out".to_owned(),
            NoticeKind::Error,
            false
        )],
        "a failure is said as one"
    );
}
