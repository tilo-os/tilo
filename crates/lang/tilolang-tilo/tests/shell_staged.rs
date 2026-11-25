//! The shell's package as `tilolang check` builds it, its scenarios and the packages they stage
//! included, over a host that runs it: what a test stages one of the shell's scenarios in. The
//! shell a session runs leaves its scenarios out (`tilolang_tilo::shell::compile`), so a test of
//! one builds the package from its directory.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;

use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{Actor, Value};
use tilolang_tilo::host::Host;

pub(crate) const SHELL: &str = "so.tilo.shell";

/// What the shell's views called, in order.
#[derive(Clone, Default)]
pub(crate) struct Called(pub(crate) Rc<RefCell<Vec<Invocation>>>);

impl Invoker for Called {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

pub(crate) fn ada() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// Load the shell's package and the packages it uses into `host`, each asked record in `asked`
/// (`studio/invocations/alarm-2`) publishing its `resolve`, as the session publishes the verbs of
/// the workspace's records; what the shell's views call is told to `called`.
pub(crate) fn load(host: &Host, called: &Called, asked: &[&str]) {
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
    let mut published = tilolang_runtime::published::Published::default();
    for record in asked {
        published.insert(
            *record,
            "resolve",
            tilolang_runtime::published::PublishedAction {
                app: "tilo.invocations".to_owned(),
                // The host's `resolve`, in the order it declares its arguments.
                params: ["resolution", "choice", "chosen", "typed", "files", "edits"]
                    .map(str::to_owned)
                    .to_vec(),
            },
        );
    }
    host.runtime().set_published_actions(published);
}

/// The shell's package and the packages it uses, built once from `apps/tl/shell`, with the
/// platform's and the native apps' descriptions they compile against.
fn built() -> &'static (Vec<tilolang::Compiled>, Vec<tilolang::PackageMeta>) {
    static BUILT: OnceLock<(Vec<tilolang::Compiled>, Vec<tilolang::PackageMeta>)> = OnceLock::new();
    BUILT.get_or_init(|| {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/shell");
        let (built, published) =
            tilolang_tilo::catalogue::compile(&dir, &[], &mut tilolang::SeededEntropy(0))
                .unwrap_or_else(|why| panic!("the shell and its scenarios compile: {why}"));
        (
            built.into_iter().map(|package| package.compiled).collect(),
            published,
        )
    })
}

/// The shell over a host that runs it, the packages its scenarios stage loaded beside it, staged
/// as its scenario `scenario`, with each asked record in `asked` (`studio/invocations/alarm-2`)
/// publishing its `resolve`, as the session publishes the verbs of the workspace's records; what
/// its views call is told to the [`Called`].
pub(crate) fn staged(scenario: &str, asked: &[&str]) -> (Host, Called) {
    let host = Host::new(ada(), || None).unwrap();
    let called = Called::default();
    load(&host, &called, asked);
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
}

/// How many times the window and its instance may answer each other before the frames are taken
/// as they are: the rows a list shows, then the rows those rows show.
#[cfg(target_os = "linux")]
const EXCHANGES: usize = 8;

/// The shell's package and the packages it uses, loaded as [`load`] loads them, beside the asked
/// records whose `resolve` the window's calls answer.
#[cfg(target_os = "linux")]
struct Shell {
    asked: Vec<String>,
}

#[cfg(target_os = "linux")]
impl tilolang_tilo::instance::Platform for Shell {
    fn load(&self, host: &Host) -> u64 {
        let asked: Vec<&str> = self.asked.iter().map(String::as_str).collect();
        load(host, &Called::default(), &asked);
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
    /// `theme`; each asked record in `asked` (`studio/invocations/share-1`) publishes its
    /// `resolve`, as the session publishes the verbs of the workspace's records.
    pub(crate) fn staged(
        scenario: &str,
        asked: &[&str],
        viewport: (u32, u32),
        theme: ui::Theme,
    ) -> Self {
        use tilolang_tilo::host::RootView;
        use tilolang_tilo::instance::{Config, Instance, Mode};
        use tilolang_tilo::names::{Installed, Told};

        let (app_end, window_end) = tilo_render_protocol::patch::local::pair();
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
            Rc::new(Shell {
                asked: asked.iter().copied().map(str::to_owned).collect(),
            }),
            names,
            Box::new(app_end),
        );
        instance.sync();
        instance.settle_pending();
        if let Some(why) = instance.take_open_failure() {
            panic!("the shell's desktop did not open: {why}");
        }
        instance.stage(Some(scenario));
        instance.sync();
        instance.commit_planned();
        if let Some(why) = instance.take_stage_failure() {
            panic!("the scenario \"{scenario}\" failed: {why}");
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
        let mut staged = Self { instance, harness };
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

    /// What holds the keyboard, named as a screen reader announces it.
    pub(crate) fn holds_keyboard(&self) -> Option<String> {
        self.harness
            .focused_access_node()
            .and_then(|node| node.name)
    }
}
