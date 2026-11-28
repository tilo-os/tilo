//! A primary action that makes a resource opens the page of what it made: the app's own view
//! answers showing an item of the type (`on show`), and the page it opens comes forward with
//! its first field taking the keyboard.

use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use tilo_render_protocol::patch::local::{End, pair};
use tilo_render_protocol::patch::{self, Front, Level, ToUi, Transport as _};
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{Actor, InvokeOutcome, Meta};
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::runtime::ids::MemberId;

use crate::fixtures::action_id;

const APP: &str = "t.opens_page";

/// A notebook whose list follows a note it is asked to show, and opens the note's page.
const SOURCE: &str = r#"app t.opens_page "Opens a page"

type Note {
    title: Text
}

type Notebook {
    notes: Collection<Note>
}

root Notebook

/// Makes a note with nothing written in it yet.
primary action new_note(nb: Notebook) -> Note {
    nb.notes.insert(Note(title: ""))
}

default view notebook for Notebook

view notebook(nb: Notebook) {
    state place: Text = "list"
    // Showing a note follows it where it stands and opens its page.
    on show(n: Note): set(place, "note").then(open(n))
    page {
        if place == "note" {
            text("On a note")
        } else {
            text("All notes")
        }
    }
}

default view note for Note

view note(n: Note) {
    page {
        ui.TextInput(bind: n.title, title: true)
    }
}
"#;

fn ids() -> &'static str {
    static IDS: OnceLock<String> = OnceLock::new();
    IDS.get_or_init(|| {
        tilolang::compile_package_with(
            SOURCE,
            &tilolang::IdsLock::new(),
            &[],
            &mut tilolang::SeededEntropy(21),
        )
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"))
        .ids
        .to_text()
    })
}

struct Opens;

impl Platform for Opens {
    fn load(&self, host: &Host) -> u64 {
        host.load_builtin(SOURCE, ids()).unwrap();
        0
    }

    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-11-23T12:00:00Z".to_owned()
    }

    fn waker(&self) -> Waker {
        Arc::new(|| {})
    }
}

struct Alone;

impl tilolang_tilo::runtime::NameHost for Alone {
    fn name(&self, who: &Actor) -> String {
        who.id().to_owned()
    }
}

/// Ada's window of the notebook, over a store of its own, and the far end the window is sent to.
fn window() -> (Instance, End) {
    let (app_end, far) = pair();
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
        Rc::new(Opens),
        Rc::new(Alone),
        Box::new(app_end),
    );
    instance.sync();
    (instance, far)
}

/// What the window was sent since the last call.
fn sent(far: &mut End) -> Vec<ToUi> {
    let mut messages = Vec::new();
    while let Ok(Some(batch)) = far.recv() {
        messages.extend(patch::decode_batch::<ToUi>(&batch).expect("the window is sent batches"));
    }
    messages
}

/// What a page opened with its first text field takes the keyboard into: the first field of the
/// tree, in the order the page stands them.
fn first_field(node: &patch::Node) -> Option<&patch::Node> {
    if node.binding.is_some() && node.kind.ends_with("TextInput") {
        return Some(node);
    }
    node.children.iter().find_map(first_field)
}

#[test]
fn the_window_finds_an_action_by_the_id_it_addresses_it_with() {
    let (instance, _far) = window();
    let host = instance.host().expect("the window opened");
    let meta: Meta = host.package(APP).unwrap().meta;
    // A window's bar and palette address an action by the id they hold: the role it declares is
    // found there too, so what a primary action makes opens.
    let root = host.root_of(APP).unwrap();
    let resource = tilolang_tilo::naming::resource_id(APP, &root.id_string());
    let role = host
        .published_action(&resource, &action_id(&meta, "new_note"))
        .and_then(|action| action.role);
    assert_eq!(role.as_deref(), Some("primary"));
}

#[test]
fn what_a_primary_action_made_opens_its_page_with_its_first_field_focused() {
    let (mut instance, mut far) = window();
    let host = instance.host().expect("the window opened");
    let meta: Meta = host.package(APP).unwrap().meta;
    let notebook = host.root_of(APP).unwrap();
    // The person's step: the bar's New note, which the host runs and shows what it made.
    let made = match host
        .runtime()
        .invoke_action(
            &Actor::Member(MemberId::person("ada")),
            (
                action_id(&meta, "new_note").as_str(),
                vec![Value::resource(&notebook)],
            ),
        )
        .unwrap()
    {
        InvokeOutcome::Done(Value::Ref(made)) => ResourceRef::clone(&made.target),
        other => panic!("new_note makes a note: {other:?}"),
    };
    instance.opens_what_it_made(tilolang_tilo::naming::resource_id(APP, &made.id_string()));
    // What the host does once the press ran: show what it made, then tell the window it ran.
    instance.show_ran();
    instance.sync();

    let messages = sent(&mut far);
    let page = messages
        .iter()
        .find_map(|message| match message {
            ToUi::Open {
                level: Level::Detail,
                target,
                tree,
                front,
                ..
            } => Some((target.clone(), tree.clone(), *front)),
            _ => None,
        })
        .expect("the note's page opened beside the list");
    let (target, tree, front) = page;
    assert_eq!(
        target.id,
        made.id_string(),
        "the page is the note the action made"
    );
    // A page a press opened comes to the front as the person's input does, and the window takes
    // the keyboard into the first text field of it.
    assert_eq!(
        front,
        Some(Front::Input),
        "the press brings its page forward"
    );
    let title = crate::fixtures::field_id(&meta, "Note", "title");
    let first = first_field(&tree).expect("the page shows its title field");
    assert_eq!(
        first
            .binding
            .as_ref()
            .and_then(patch::Binding::field)
            .map(|(_, field)| field),
        Some(title.as_str()),
        "the page's first field is the note's title: {:?}",
        first.binding
    );
    // Showing the note ran the list's own answer for it: it now says it is on a note.
    assert!(
        format!("{:?}", instance.root_tree()).contains("On a note"),
        "the list followed the note it was asked to show"
    );
}
