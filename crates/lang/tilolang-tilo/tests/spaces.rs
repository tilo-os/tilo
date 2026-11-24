//! A view call reads the space layout gives it: a view of a window's page that fits its content to
//! the room it has reads `space`, and follows the size the window says layout gave it, without
//! asking anything. The app tells the window which calls read it, and the window answers each with
//! its own size; nothing polls.

use std::rc::Rc;
use std::sync::Arc;

use tilo_render_protocol::patch::local::{End, pair};
use tilo_render_protocol::patch::{self, Level, ToApp, ToUi, Transport as _};
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};

const APP: &str = "t.spaces";

/// A desk with one panel on it, drawn by a view that says whether the room it has is short or
/// tall: its own space, not the window's.
const SOURCE: &str = r#"app t.spaces "Spaces"

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
        "2026-11-22T12:00:00Z".to_owned()
    }

    fn waker(&self) -> Waker {
        Arc::new(|| {})
    }
}

/// Ada's window of the desk, and the window's end of it.
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
        Rc::new(Loaded(ids())),
        Rc::new(tilolang_tilo::names::Installed),
        Box::new(app_end),
    );
    instance.sync();
    (instance, window_end)
}

/// What the window was sent since the last call.
fn sent(window: &mut End) -> Vec<ToUi> {
    let mut messages = Vec::new();
    while let Ok(Some(batch)) = window.recv() {
        messages.extend(patch::decode_batch::<ToUi>(&batch).expect("the window is sent batches"));
    }
    messages
}

/// What the window's page says now.
fn said(instance: &Instance) -> String {
    format!("{:?}", instance.root_tree().expect("the desk is shown"))
}

fn told(instance: &mut Instance, window: &mut End, key: &str, height: f32) {
    window
        .send(
            patch::encode_batch(&[ToApp::Space {
                level: Level::Root,
                key: key.to_owned(),
                width: 40.0,
                height,
                rem: 16.0,
            }])
            .expect("a batch"),
        )
        .expect("the window tells it");
    instance.update();
}

#[test]
fn a_view_call_follows_the_size_the_window_says_layout_gave_it() {
    let (mut instance, mut window) = window();
    let sent_now = sent(&mut window);
    assert!(
        said(&instance).contains("short"),
        "the view starts at the space it is opened with: {}",
        said(&instance)
    );
    // The app names the view calls that read their space: the panel's view, not the desk.
    let keys = sent_now
        .iter()
        .find_map(|message| match message {
            ToUi::Spaces { level, keys } if *level == Level::Root => Some(keys.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the app says which views read their space: {sent_now:?}"));
    assert_eq!(keys.len(), 1, "{keys:?}");
    assert!(
        sent_now
            .iter()
            .any(|message| matches!(message, ToUi::Open { .. })),
        "the window's page came too"
    );

    // The window says the room layout gave that call: the view runs again and says so.
    told(&mut instance, &mut window, &keys[0], 60.0);
    let after = sent(&mut window);
    assert!(
        said(&instance).contains("tall"),
        "the call read its own space: {}",
        said(&instance)
    );
    assert!(!after.is_empty(), "the window is sent the change");
    assert!(
        !after
            .iter()
            .any(|message| matches!(message, ToUi::Spaces { .. })),
        "the set of views that read their space did not change: {after:?}"
    );

    // The same room again is no change, and a key no view stands at is nothing.
    told(&mut instance, &mut window, &keys[0], 60.0);
    assert!(sent(&mut window).is_empty(), "nothing changed");
    told(&mut instance, &mut window, "no-such-view", 10.0);
    assert!(sent(&mut window).is_empty(), "no view stands there");

    // Smaller again: the view follows the window back down.
    told(&mut instance, &mut window, &keys[0], 20.0);
    assert!(
        said(&instance).contains("short"),
        "the call follows a smaller room: {}",
        said(&instance)
    );
}
