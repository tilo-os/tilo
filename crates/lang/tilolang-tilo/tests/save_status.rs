//! The save status every window's bar shares: a change under way, one the store took, and one
//! the workspace refused, which stands with "Try again" and runs the act that wrote it once more.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use collab_types::store::wire::{Command, Reply};
use tilo_render_protocol::patch::local::{End, pair};
use tilo_render_protocol::patch::{Level, SaveStatus, ToApp, ToUi};
use tilolang_runtime::Actor;
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::resources::ResourceStore;
use tilolang_tilo::runtime::NameHost;

use crate::window_reads::{tell, told};

const APP: &str = "t.save";

/// Why the workspace says it keeps nothing, in its own words.
const REFUSED: &str = "The workspace refused the change";

/// A note with one button that writes it: the smallest act a person makes that is a write.
const SOURCE: &str = r#"app t.save "Save"

type Note {
    body: Text
}

root note: Note

default view note_page for Note

view note_page(n: Note) {
    page {
        ui.Button("Write") {
            on press: write_note(n)
        }
    }
}

/// Write the note.
action write_note(n: Note) {
    n.body = "written"
}
"#;

fn ids() -> &'static str {
    static IDS: OnceLock<String> = OnceLock::new();
    IDS.get_or_init(|| {
        tilolang::compile_package_with(
            SOURCE,
            &tilolang::IdsLock::new(),
            &[],
            &mut tilolang::SeededEntropy(41),
        )
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"))
        .ids
        .to_text()
    })
}

struct Note;

impl Platform for Note {
    fn load(&self, host: &Host) -> u64 {
        host.load_builtin(SOURCE, ids()).unwrap();
        0
    }

    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-11-28T09:00:00Z".to_owned()
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

/// Whether the workspace takes writes now, and how many it was offered: one per act that wrote,
/// and one more for each retry the person asked for.
#[derive(Default)]
struct Writes {
    refuse: Cell<bool>,
    offered: Cell<u32>,
}

/// The workspace's store as a person meets it: it takes writes until it does not, and then says
/// why in its own words, as its actions answer a refusal.
struct Refusing {
    store: tilo_resource_store::LocalPort,
    writes: Rc<Writes>,
}

impl tilo_resource_store::Port for Refusing {
    fn command(&self, command: Command) -> Result<Reply, String> {
        if command.writes() {
            self.writes
                .offered
                .set(self.writes.offered.get().saturating_add(1));
            if self.writes.refuse.get() {
                return Err(REFUSED.to_owned());
            }
        }
        tilo_resource_store::Port::command(&self.store, command)
    }
}

/// Ada's window of the note, its end of the connection, and the workspace it writes into.
fn window() -> (Instance, End, Rc<Writes>) {
    let (app_end, far) = pair();
    let writes = Rc::new(Writes::default());
    let store = ResourceStore::new(Rc::new(Refusing {
        store: tilo_resource_store::LocalPort::in_memory(),
        writes: Rc::clone(&writes),
    }));
    let mut instance = Instance::new(
        Config {
            app: APP.to_owned(),
            me: "ada".into(),
            mode: Mode::Live,
            device: None,
            store,
            root: RootView::default(),
            sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
            link_home: None,
        },
        Rc::new(Note),
        Rc::new(Alone),
        Box::new(app_end),
    );
    instance.sync();
    (instance, far, writes)
}

/// The key of the note's one button, from the page the window was sent.
fn button(far: &mut End) -> String {
    for message in told(far) {
        let ToUi::Open {
            level: Level::Root,
            tree,
            ..
        } = message
        else {
            continue;
        };
        let buttons = tree.of_kind("ui.Button");
        let [button] = buttons.as_slice() else {
            panic!("one button writes the note: {tree:#?}");
        };
        return button.key.clone();
    }
    panic!("the window opens on the note's page");
}

/// What the window heard of the save, in order.
fn saves(messages: &[ToUi]) -> Vec<SaveStatus> {
    messages
        .iter()
        .filter_map(|message| match message {
            ToUi::Save(status) => Some(status.clone()),
            _ => None,
        })
        .collect()
}

/// The press of the note's button.
fn press(key: &str) -> ToApp {
    ToApp::Fire {
        level: Level::Root,
        key: key.to_owned(),
        event: "press".to_owned(),
        value: None,
        gesture: None,
    }
}

#[test]
fn a_change_is_saving_while_it_is_written_and_saved_once_the_store_took_it() {
    let (mut instance, mut far, _writes) = window();
    let key = button(&mut far);
    tell(&mut instance, &mut far, press(&key));
    assert_eq!(
        saves(&told(&mut far)),
        [SaveStatus::Saving, SaveStatus::Saved],
        "the window hears the save begin, and that it landed"
    );
    // A frame that changed nothing says nothing more: the same status again is no news.
    instance.update();
    assert_eq!(saves(&told(&mut far)), []);
}

#[test]
fn a_refused_change_says_not_saved_with_try_again_which_runs_it_once_more() {
    let (mut instance, mut far, writes) = window();
    let key = button(&mut far);
    writes.refuse.set(true);
    let before = writes.offered.get();
    tell(&mut instance, &mut far, press(&key));
    assert_eq!(
        saves(&told(&mut far)),
        [SaveStatus::Saving, SaveStatus::Failed(REFUSED.to_owned())],
        "the window hears it begin and that the workspace refused it"
    );
    assert_eq!(
        writes.offered.get() - before,
        1,
        "the press wrote once, and the refusal kept that write"
    );

    // Try again while the workspace still refuses: the act runs once more, and the refusal
    // stands as it did.
    let before = writes.offered.get();
    tell(&mut instance, &mut far, ToApp::SaveAgain);
    assert_eq!(
        saves(&told(&mut far)),
        [SaveStatus::Saving, SaveStatus::Failed(REFUSED.to_owned())],
        "a second refusal is said again"
    );
    assert_eq!(
        writes.offered.get() - before,
        1,
        "one retry per press, never a loop"
    );

    // The workspace takes it: the retry lands and the failure goes.
    writes.refuse.set(false);
    tell(&mut instance, &mut far, ToApp::SaveAgain);
    assert_eq!(
        saves(&told(&mut far)),
        [SaveStatus::Saving, SaveStatus::Saved],
        "the retry lands, and the save status says so"
    );
    // Nothing is left to retry: a later press is the person's own act, and saves as any other.
    tell(&mut instance, &mut far, press(&key));
    assert_eq!(
        saves(&told(&mut far)),
        [SaveStatus::Saving, SaveStatus::Saved]
    );
}
