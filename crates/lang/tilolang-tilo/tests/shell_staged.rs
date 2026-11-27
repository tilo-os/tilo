//! The shell's package as `tilolang check` builds it, its scenarios and the packages they stage
//! included, over a host that runs it: what a test stages one of the shell's scenarios in. The
//! shell a session runs leaves its scenarios out (`tilolang_tilo::shell::compile`), so a test of
//! one builds the package from its directory.

use std::rc::Rc;
use std::sync::OnceLock;

use tilolang_runtime::Actor;
use tilolang_tilo::host::Host;

pub(crate) const SHELL: &str = "so.tilo.shell";

/// What the shell's views called, in order.
pub(crate) use crate::fixtures::invoker::Sent as Called;

pub(crate) fn ada() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// Load the shell's package and the packages it uses into `host`; what the shell's views call is
/// told to `called`. An asked record's `resolve` is its own, as the host declares it: nothing
/// publishes it, as nothing does in a session, and a view's answer reaches `called` as the
/// record's call.
pub(crate) fn load(host: &Host, called: &Called) {
    let (built, published) = built();
    for meta in published {
        host.runtime()
            .load_foreign(meta.clone())
            .unwrap_or_else(|why| panic!("{}: {why}", meta.app));
    }
    for compiled in built {
        host.load_compiled(compiled)
            .unwrap_or_else(|why| panic!("{}: {why}", compiled.meta.app));
    }
    host.runtime().set_foreign_invoker(called.clone());
    stage_device_settings(host);
}

/// The session's device settings a staged shell reads, as the device holds them before it draws:
/// the widget layer (`settings.widgets`) and the control hints (`settings.control_hints`). A
/// test's host publishes no session, so both read nothing and each view waits on a reading that
/// never comes, drawing the toolkit's page-shaped loading skeleton over the desktop while it
/// shows. The widgets read as a new install leaves them (nothing plays, nobody is here) and the
/// hints as the person turned them off, so nothing of theirs stands on the shell and a capture
/// shows the surface it is of. A scenario that stages either replaces its reading.
fn stage_device_settings(host: &Host) {
    crate::fixtures::set_reading(
        host,
        "devices/{device}/settings/widgets",
        &serde_json::json!({}),
    );
    crate::fixtures::set_reading(
        host,
        "settings/control-hints",
        &serde_json::json!({"learned": {"enabled": false, "known": [], "shown": {}}}),
    );
}

/// The shell's package and the packages it uses, built once from `apps/tl/shell`, with the
/// platform's and the native apps' descriptions they compile against.
fn built() -> &'static (Vec<tilolang::Compiled>, Vec<tilolang::PackageMeta>) {
    static BUILT: OnceLock<(Vec<tilolang::Compiled>, Vec<tilolang::PackageMeta>)> = OnceLock::new();
    BUILT.get_or_init(|| {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/shell");
        let (built, published) =
            tilolang_tilo::platform::compile(&dir, &[], &mut tilolang::SeededEntropy(0))
                .unwrap_or_else(|why| panic!("the shell and its scenarios compile: {why}"));
        (
            built.into_iter().map(|package| package.compiled).collect(),
            published,
        )
    })
}

/// The shell over a host that runs it, the packages its scenarios stage loaded beside it, staged
/// as its scenario `scenario`; what its views call is told to the [`Called`].
pub(crate) fn staged(scenario: &str) -> (Host, Called) {
    let host = Host::new(ada(), || None).unwrap();
    let called = Called::default();
    load(&host, &called);
    host.root_of(SHELL).unwrap();
    let action = host
        .package(SHELL)
        .unwrap()
        .meta
        .scenarios
        .iter()
        .find(|staged| staged.name == scenario)
        .unwrap_or_else(|| panic!("the shell stages {scenario:?}"))
        .action
        .clone();
    host.runtime()
        .invoke_action(&ada(), (action.as_str(), vec![]))
        .unwrap();
    (host, called)
}

/// The shell's desktop in a window, driven without a display: the instance that runs the shell's
/// views and the window that shows the tree it sends.
#[cfg(target_os = "linux")]
pub(crate) struct StagedWindow {
    pub(crate) instance: tilolang_tilo::instance::Instance,
    pub(crate) harness: ui::headless::Driver,
    /// What the shell's views called, in order: every action a press ran, so a test reads what a
    /// press asked of a resource.
    pub(crate) called: Called,
}

/// How many times the window and its instance may answer each other before the frames are taken
/// as they are: the rows a list shows, then the rows those rows show.
#[cfg(target_os = "linux")]
const EXCHANGES: usize = 8;

/// The shell's package and the packages it uses, loaded as [`load`] loads them.
#[cfg(target_os = "linux")]
struct Shell {
    called: Called,
}

#[cfg(target_os = "linux")]
impl tilolang_tilo::instance::Platform for Shell {
    fn load(&self, host: &Host) -> u64 {
        load(host, &self.called);
        0
    }

    fn phrase(&self, id: &str, _: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-11-23T09:00:00Z".to_owned()
    }

    fn waker(&self) -> tilolang_tilo::background::Waker {
        std::sync::Arc::new(|| {})
    }
}

#[cfg(target_os = "linux")]
impl StagedWindow {
    /// The shell staged as its scenario `scenario`, its desktop in a window of `viewport` in
    /// `theme`.
    pub(crate) fn staged(scenario: &str, viewport: (u32, u32), theme: tilo_ui_base::Theme) -> Self {
        use tilolang_tilo::host::RootView;
        use tilolang_tilo::instance::{Config, Instance, Mode};
        use tilolang_tilo::names::Installed;

        let (app_end, window_end) = tilo_render_protocol::patch::local::pair();
        let names = Rc::new(Installed);
        let called = Called::default();
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
            Rc::new(Shell {
                called: called.clone(),
            }),
            names,
            Box::new(app_end),
        );
        instance.sync();
        instance.settle_pending();
        if let Some(why) = instance.take_open_failure() {
            panic!("the shell's desktop did not open: {why}");
        }
        if let Err(why) = instance.stage(scenario) {
            panic!("{why}");
        }
        instance.update();
        let window = tilo_app_window::AppWindow::new(Box::new(window_end), Vec::new());
        let mut harness = ui::headless::Driver::new_with_theme(
            ui::State::new(),
            Box::new(window),
            viewport,
            theme,
        );
        // The window takes the keyboard as it opens, as a session's window does: what the app's
        // entry is then holds it.
        harness.send(&ui::winit::event::WindowEvent::Focused(true));
        let mut staged = Self {
            instance,
            harness,
            called,
        };
        staged.exchange();
        staged
    }

    /// Let the window and its instance answer each other until neither has more to say, laying the
    /// window out after each answer, and run out the motion that lands even when reduced.
    pub(crate) fn exchange(&mut self) {
        use tilo_app_window::AppWindow;

        for _ in 0..EXCHANGES {
            self.instance.settle_pending();
            let (moved, received, owing) = {
                let Some(window) = self
                    .harness
                    .root_widget_mut()
                    .as_any_mut()
                    .and_then(|widget| widget.downcast_mut::<AppWindow>())
                else {
                    break;
                };
                window.flush();
                let received = window.receive();
                let moved = self.instance.update();
                (moved, received, window.owes() || self.instance.pending())
            };
            self.harness.relayout();
            if !moved && !received && !owing {
                break;
            }
        }
        assert!(
            self.harness
                .settle_for_capture(ui::time::Duration::from_secs(30)),
            "the window comes to rest"
        );
    }

    /// Let the window and its instance answer each other for one frame read at `at`, laying the
    /// window out after each answer: no motion is run out, so a test steps a fade a frame at a
    /// time.
    pub(crate) fn frame_at(&mut self, at: ui::time::Instant) {
        use tilo_app_window::AppWindow;

        ui::time::testing::with_monotonic_time(at, || {
            self.instance.settle_pending();
            if let Some(window) = self
                .harness
                .root_widget_mut()
                .as_any_mut()
                .and_then(|widget| widget.downcast_mut::<AppWindow>())
            {
                window.flush();
                window.receive();
                self.instance.update();
            }
            self.harness.relayout();
        });
    }

    /// Every accessible node of the window, as a screen reader reads it.
    pub(crate) fn accessible(&self) -> Vec<tilo_ui_base::a11y::AccessNode> {
        let snapshot = self.harness.inspection();
        snapshot
            .root
            .nodes()
            .into_iter()
            .filter_map(|node| node.semantics.as_ref())
            .flat_map(tilo_ui_base::a11y::AccessNode::nodes)
            .cloned()
            .collect()
    }

    /// Press the control `name` where it stands, as a person does.
    pub(crate) fn press(&mut self, name: &str) {
        // A row is named for what it shows: its title, then what stands beside it ("Sony
        // WH-1000XM5, Connected"). The press names the title.
        let bare = |shown: &str| shown == name || shown.starts_with(&format!("{name}, "));
        let at = self
            .accessible()
            .into_iter()
            .find(|node| node.name.as_deref().is_some_and(bare) && node.bounds.is_some())
            .map_or_else(
                || {
                    let names: Vec<String> = self
                        .accessible()
                        .into_iter()
                        .filter_map(|node| node.name)
                        .collect();
                    panic!("the window offers `{name}`: {names:?}")
                },
                |node| node.bounds.expect("the control's bounds"),
            );
        self.harness
            .click_at(f64::midpoint(at.x0, at.x1), f64::midpoint(at.y0, at.y1));
        self.exchange();
    }

    /// What holds the keyboard, named as a screen reader announces it.
    pub(crate) fn holds_keyboard(&self) -> Option<String> {
        self.harness
            .focused_access_node()
            .and_then(|node| node.name)
    }
}
