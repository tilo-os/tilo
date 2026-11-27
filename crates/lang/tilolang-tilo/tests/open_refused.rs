//! A window whose app the workspace will not let keep its records opens on why, where its page
//! would stand: the platform's words that the app did not open, and under them the workspace's
//! own words of why it refused. Never an empty window, and never a notice that fades and leaves
//! one.

use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use collab_types::store::wire::{Command, Reply};
use tilo_render_protocol::patch::local::{End, pair};
use tilo_render_protocol::patch::{self as ui, Level, ToUi, Transport as _};
use tilolang_runtime::Actor;
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::resources::ResourceStore;
use tilolang_tilo::runtime::NameHost;

const APP: &str = "t.refused";

/// A shelf whose first start fills it, as the gallery's does: the write the app makes by itself
/// before its window shows anything.
const SOURCE: &str = r#"app t.refused "Shelf"

type Shelf {
    title: Text
}

root Shelf

initial action fill(s: Shelf) {
    s.title = "Kitchen"
}

default view shelf for Shelf

view shelf(s: Shelf) {
    page { text(s.title) }
}
"#;

/// What the workspace says of a write by an app no member installed in it.
const NOT_INSTALLED: &str = "Shelf isn't installed in this workspace, so it can't keep records \
     here. Install Shelf in the workspace to use it.";

fn ids() -> &'static str {
    static IDS: OnceLock<String> = OnceLock::new();
    IDS.get_or_init(|| {
        tilolang::compile_package_with(
            SOURCE,
            &tilolang::IdsLock::new(),
            &[],
            &mut tilolang::SeededEntropy(23),
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
        match id {
            "tilolang-open-failed" => "This app could not be opened.".to_owned(),
            other => other.to_owned(),
        }
    }

    fn now(&self) -> String {
        "2026-11-27T09:00:00Z".to_owned()
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

/// The workspace's store as it stands to an app it does not run: it reads, and refuses every
/// write, in the words the workspace writes for the person, as its actions answer a refusal.
struct NotInstalled {
    store: tilo_resource_store::LocalPort,
}

impl tilo_resource_store::Port for NotInstalled {
    fn command(&self, command: Command) -> Result<Reply, String> {
        if command.writes() {
            return Err(
                tilo_actions::remote::ActionCallError::refused(NOT_INSTALLED.to_owned())
                    .to_string(),
            );
        }
        tilo_resource_store::Port::command(&self.store, command)
    }

    fn declare(&self, schema: tilo_resource_store::Schema) {
        tilo_resource_store::Port::declare(&self.store, schema);
    }

    fn set_record_reader(&self, reader: Arc<dyn tilo_resource_store::RecordReader>) {
        tilo_resource_store::Port::set_record_reader(&self.store, reader);
    }
}

/// The root pages the window was sent: each `Open` of its root level's tree.
fn root_pages(window: &mut End) -> Vec<ui::Node> {
    let mut out = Vec::new();
    while let Some(batch) = window.recv().expect("the window reads its batches") {
        for message in ui::decode_batch::<ToUi>(&batch).expect("a batch of the window's") {
            if let ToUi::Open {
                level: Level::Root,
                tree,
                ..
            } = message
            {
                out.push(tree);
            }
        }
    }
    out
}

fn text_prop(node: &ui::Node, name: &str) -> Option<String> {
    match node.props.get(name) {
        Some(ui::Value::Text(text)) => Some(text.clone()),
        _ => None,
    }
}

#[test]
fn a_window_the_workspace_refuses_opens_on_why_in_its_words() {
    let (app_end, mut window) = pair();
    let mut instance = Instance::new(
        Config {
            app: APP.to_owned(),
            me: "ada".into(),
            mode: Mode::Live,
            device: None,
            store: ResourceStore::new(Rc::new(NotInstalled {
                store: tilo_resource_store::LocalPort::in_memory(),
            })),
            root: RootView::default(),
            sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
            link_home: None,
        },
        Rc::new(Shelf),
        Rc::new(Alone),
        Box::new(app_end),
    );
    instance.sync();
    instance.flush();

    assert!(
        instance.open_failed().is_some(),
        "the refused first start leaves the app's root unopened"
    );
    let pages = root_pages(&mut window);
    let page = pages
        .last()
        .unwrap_or_else(|| panic!("the window opens on a page, not on nothing"));
    let states = page.of_kind("ui.EmptyState");
    let [state] = states.as_slice() else {
        panic!("one state says why: {page:#?}");
    };
    assert_eq!(
        text_prop(state, "title").as_deref(),
        Some("This app could not be opened.")
    );
    assert_eq!(
        text_prop(state, "message").as_deref(),
        Some(NOT_INSTALLED),
        "the workspace's own words, without the plumbing they came through"
    );
}
