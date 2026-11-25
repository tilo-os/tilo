//! A capture: the shell's confirm for the scenario "a press that needs a yes", standing at the
//! point of the Notes window the Share was pressed at, in both themes. The scenario is staged in
//! the shell's own package by the instance that sends its desktop to the window, the same staging
//! the shell's other tests read the scenario from.

#![cfg(target_os = "linux")]

use std::rc::Rc;

use tilo_app_window::AppWindow;
use tilo_render_protocol::patch::local::pair;
use tilolang_runtime::{UiNode, Value};
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::names::{Installed, Told};
use ui::headless::Driver;

use crate::shell_staged::{Called, SHELL};

/// The asked record the scenario leaves waiting, whose own `resolve` answers the confirm.
const ASKED: &[&str] = &["studio/invocations/share-1"];

/// The scenario staged: Notes' Share, pressed at (640, 212) in its window, waits for Ada.
const SCENARIO: &str = "a press that needs a yes";

/// How many times the window and its instance may answer each other before the frame is taken as
/// it is: the rows a list shows, then the rows those rows show.
const EXCHANGES: usize = 8;

/// The shell's package and the packages it uses, loaded as the shell's own staging loads them.
struct Shell;

impl Platform for Shell {
    fn load(&self, host: &Host) -> u64 {
        crate::shell_staged::load(host, &Called::default(), ASKED);
        0
    }

    fn phrase(&self, id: &str, _: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-11-23T09:00:00Z".to_owned()
    }

    fn waker(&self) -> Waker {
        std::sync::Arc::new(|| {})
    }
}

/// The directory a capture is written to: beside the test's built binary, as the other shots
/// are, since a source tree read from a test may be read-only.
fn captures() -> std::path::PathBuf {
    std::env::current_exe()
        .expect("the test's binary")
        .parent()
        .expect("its directory")
        .parent()
        .expect("the profile's directory")
        .parent()
        .expect("the build's directory")
        .join("asking-at-press-shot")
}

/// `value`, out of the option it may be given in.
fn given(value: Option<&Value>) -> Option<&Value> {
    match value? {
        Value::Opt(inner) => inner.as_deref(),
        value => Some(value),
    }
}

/// Every text the nodes under `tree` say, as their props carry it.
fn texts(tree: &UiNode) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = vec![tree];
    while let Some(node) = at.pop() {
        out.extend(node.props.values().filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        }));
        at.extend(node.children.iter());
    }
    out
}

/// The shell's desktop as its scenario "a press that needs a yes" stages it, in both themes, saved
/// beside `out`.
#[test]
fn a_press_that_needs_a_yes_in_both_themes() {
    let (app_end, window_end) = pair();
    let names = Rc::new(Told::new("ada".into(), Rc::new(Installed)));
    let mut instance = Instance::new(
        Config {
            app: SHELL.to_owned(),
            me: "ada".into(),
            mode: Mode::Live,
            device: None,
            store: tilolang_tilo::resources::ResourceStore::in_memory(),
            root: RootView {
                root: Some("Layout".to_owned()),
                view: None,
            },
            sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
            link_home: None,
        },
        Rc::new(Shell),
        names,
        Box::new(app_end),
    );
    instance.sync();
    instance.settle_pending();
    if let Some(why) = instance.take_open_failure() {
        panic!("the shell's desktop did not open: {why}");
    }
    instance.stage(Some(SCENARIO));
    instance.sync();
    instance.commit_planned();
    if let Some(why) = instance.take_stage_failure() {
        panic!("the scenario \"{SCENARIO}\" failed: {why}");
    }
    instance.update();
    // The confirm is what the capture is of: it stands at the press, showing whom the call would
    // give the note to, in the tree the window is sent.
    let tree = instance.root_tree().expect("the window's tree");
    let [confirm] = tree.of_kind("ui.Popover")[..] else {
        panic!("one confirm stands at the press: {tree:#?}");
    };
    assert_eq!(
        given(confirm.props.get("at")),
        Some(&Value::List(vec![
            Value::Float(640.0),
            Value::Float(212.0)
        ])),
        "at the point of the window the press was in"
    );
    let shown = texts(confirm);
    for fact in ["Mia Clayton", "Can edit"] {
        assert!(
            shown.iter().any(|text| text.contains(fact)),
            "the confirm shows what the call would do ({fact}): {shown:?}"
        );
    }

    let mut dark = ui::Theme::dark();
    dark.reduce_motion = true;
    let window = AppWindow::new(Box::new(window_end), Vec::new());
    let mut harness =
        Driver::new_with_theme(ui::State::new(), Box::new(window), (1280, 800), dark);
    harness.relayout();
    exchange(&mut instance, &mut harness);
    // The captures stand beside the build, as the other shots do: a source tree read from a
    // test may be read-only.
    let out = captures();
    std::fs::create_dir_all(&out).expect("the captures' directory");
    harness.render_png(&out.join("asking-at-press-dark.png"));

    let mut light = ui::Theme::light();
    light.reduce_motion = true;
    harness
        .relayout_themed((1280, 800), light)
        .expect("the light frame");
    exchange(&mut instance, &mut harness);
    harness.render_png(&out.join("asking-at-press-light.png"));
    for theme in ["dark", "light"] {
        let path = out.join(format!("asking-at-press-{theme}.png"));
        assert!(
            std::fs::metadata(&path).is_ok_and(|held| held.len() > 0),
            "the {theme} capture is written: {}",
            path.display()
        );
    }
}

/// Let the window and its instance answer each other until neither has more to say, laying the
/// window out after each answer, and run out the motion that lands even when reduced.
fn exchange(instance: &mut Instance, harness: &mut Driver) {
    for _ in 0..EXCHANGES {
        instance.settle_pending();
        let (moved, received, owing) = {
            let Some(window) = harness
                .root_widget_mut()
                .as_any_mut()
                .and_then(|widget| widget.downcast_mut::<AppWindow>())
            else {
                break;
            };
            window.flush();
            let received = window.receive();
            let moved = instance.update();
            (moved, received, window.owes() || instance.pending())
        };
        harness.relayout();
        if !moved && !received && !owing {
            break;
        }
    }
    assert!(
        harness.settle_for_capture(ui::time::Duration::from_secs(30)),
        "the window comes to rest"
    );
}
