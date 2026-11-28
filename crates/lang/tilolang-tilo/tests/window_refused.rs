//! A window that refuses something its app sent says so and goes on showing the app. What the
//! app sends next is patched against what it sent before, which the window left out, so the
//! window shows the error state and asks the app for its views whole; once they come it shows
//! them, and every update after them.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use tilo_app_window::AppWindow;
use tilo_render_protocol::WireError;
use tilo_render_protocol::patch::local::{End, pair};
use tilo_render_protocol::patch::{self as wire, Level, Node, Patch, ToApp, ToUi, Transport};
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};

const APP: &str = "t.refused";

/// A desk with one panel, whose view says whether the room it has is short or tall: the window
/// saying the room layout gave it is an update the app sends as a patch.
const SOURCE: &str = r#"app t.refused "Refused"

type Panel {
    which: Text
}

type Desk {
    panel: Panel
}

root Desk

default view desk for Desk

view desk(d: Desk) {
    page {
        tall(d.panel)
    }
}

view tall(p: Panel) {
    if space.height < 30.0 {
        text("short")
    } else {
        text("tall")
    }
}
"#;

struct Loaded(String);

impl Platform for Loaded {
    fn load(&self, host: &Host) -> u64 {
        host.load_builtin(SOURCE, &self.0).unwrap();
        0
    }

    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-11-28T12:00:00Z".to_owned()
    }

    fn waker(&self) -> Waker {
        Arc::new(|| {})
    }
}

/// The window's end of its app's connection, with what the app sent read on its way: the test
/// learns the keys of the view calls that read their space, and puts a batch of its own in the
/// app's stream as a broken app would send it.
struct Wire {
    end: End,
    seen: Rc<RefCell<Vec<ToUi>>>,
    slipped: Rc<RefCell<VecDeque<Vec<u8>>>>,
}

impl Transport for Wire {
    fn send(&mut self, batch: Vec<u8>) -> Result<(), WireError> {
        self.end.send(batch)
    }

    fn recv(&mut self) -> Result<Option<Vec<u8>>, WireError> {
        if let Some(batch) = self.slipped.borrow_mut().pop_front() {
            return Ok(Some(batch));
        }
        let batch = self.end.recv()?;
        if let Some(batch) = &batch {
            self.seen
                .borrow_mut()
                .extend(wire::decode_batch::<ToUi>(batch).expect("the app's batch"));
        }
        Ok(batch)
    }
}

fn ids() -> String {
    tilolang::compile_package_with(
        SOURCE,
        &tilolang::IdsLock::new(),
        &[],
        &mut tilolang::SeededEntropy(7),
    )
    .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"))
    .ids
    .to_text()
}

/// What the window shows of its app now.
fn shown(window: &AppWindow) -> String {
    window
        .composed()
        .map(|tree| format!("{tree:?}"))
        .unwrap_or_default()
}

/// The app and its window answer each other until neither has anything more to say.
fn exchange(instance: &mut Instance, window: &mut AppWindow) {
    for _ in 0..4 {
        instance.update();
        window.receive();
        window.flush();
    }
}

#[test]
fn a_window_that_refused_an_update_shows_why_then_takes_its_apps_views_again() {
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
        Rc::new(Loaded(ids())),
        Rc::new(tilolang_tilo::names::Installed),
        Box::new(app_end),
    );
    instance.sync();
    let seen = Rc::default();
    let slipped: Rc<RefCell<VecDeque<Vec<u8>>>> = Rc::default();
    let mut window = AppWindow::new(
        Box::new(Wire {
            end: window_end,
            seen: Rc::clone(&seen),
            slipped: Rc::clone(&slipped),
        }),
        Vec::new(),
    );
    exchange(&mut instance, &mut window);
    assert!(shown(&window).contains("short"), "{}", shown(&window));
    let key = seen
        .borrow()
        .iter()
        .find_map(|message| match message {
            ToUi::Spaces { level, keys } if *level == Level::Root => keys.first().cloned(),
            _ => None,
        })
        .expect("the app names the view call that reads its space");

    // A patch that breaks the protocol, a component the toolkit does not have, as an app with a
    // bug sends it.
    slipped.borrow_mut().push_back(
        wire::encode_batch(&[ToUi::Patch {
            level: Level::Root,
            patches: vec![Patch::Insert {
                parent: Vec::new(),
                index: 0,
                node: Node::empty("ui.NoSuchComponent", "broken"),
            }],
        }])
        .expect("a batch"),
    );
    assert!(window.receive());
    let why = window
        .failed()
        .expect("the window refuses the patch")
        .to_owned();
    assert!(
        why.contains("NoSuchComponent"),
        "it says what it refused: {why}"
    );
    assert!(
        window.composed().is_none(),
        "the error state stands in place of the view, not the page as it was"
    );

    // The window asks for the views whole with its next batch; the app sends them, and the update
    // the person's room makes after them is drawn.
    window.tell(ToApp::Space {
        level: Level::Root,
        key,
        width: 40.0,
        height: 60.0,
        rem: 16.0,
    });
    window.flush();
    exchange(&mut instance, &mut window);
    assert!(
        window.failed().is_none(),
        "the app's views ended the error state: {:?}",
        window.failed()
    );
    assert!(
        shown(&window).contains("tall") && !shown(&window).contains("NoSuchComponent"),
        "the window shows the app's view as it now stands: {}",
        shown(&window)
    );
}
