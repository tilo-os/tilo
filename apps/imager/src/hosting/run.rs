//! The installer's entry: the helpers its binary also is, its shots, and its live window.

use ui::launch::LaunchArgs;
use tilo_ui_base::widget::Widget;

use super::Store;

/// The size a shot is taken at unless `--viewport` names another.
pub(crate) const SHOT_VIEWPORT: (u32, u32) = (1180, 820);

/// The switches a live launch takes.
const FLAGS: &[&str] = &[
    #[cfg(feature = "installation")]
    "--setup",
    #[cfg(feature = "installation")]
    "--demo",
    #[cfg(feature = "destination")]
    "--destination",
];

/// Which window a run opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    /// The image writer.
    Wizard,
    /// The guided setup, from the computer it runs on.
    #[cfg(feature = "installation")]
    Setup,
    /// The installer on the live medium.
    #[cfg(feature = "destination")]
    Destination,
}

impl Flow {
    /// The flow a shot's scenario belongs to.
    pub(crate) fn of_scenario(scenario: Option<&str>) -> Self {
        match scenario {
            #[cfg(feature = "installation")]
            Some(name) if name.starts_with("installer-") => Self::Setup,
            #[cfg(feature = "destination")]
            Some(name) if name.starts_with("destination-") => Self::Destination,
            _ => Self::Wizard,
        }
    }

    /// The flow a live launch with `args` opens.
    pub(crate) fn launched(args: &LaunchArgs) -> Self {
        #[cfg(feature = "destination")]
        if crate::setup::destination::launched() || args.has("--destination") {
            return Self::Destination;
        }
        #[cfg(feature = "installation")]
        if args.has("--setup") || args.has("--demo") {
            return Self::Setup;
        }
        let _ = args;
        Self::Wizard
    }
}

/// How a window is opened: live on this computer, or as a scenario's shot that touches
/// nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Opened<'a> {
    /// A window someone uses: on this computer, on the file it showed last when `restore` names
    /// the reference the session opened it on.
    Live {
        demo: bool,
        restore: Option<&'a std::path::Path>,
    },
    Shot(Option<&'a str>),
}

impl Opened<'_> {
    pub(crate) fn is_live(self) -> bool {
        matches!(self, Self::Live { .. })
    }
}

/// Every scenario a shot can show.
pub(crate) fn scenarios() -> Vec<&'static str> {
    let scenarios = crate::app::SCENARIOS.iter();
    #[cfg(feature = "installation")]
    let scenarios = scenarios.chain(crate::setup::journey::SCENARIOS);
    #[cfg(feature = "destination")]
    let scenarios = scenarios.chain(crate::setup::destination::SCENARIOS);
    scenarios.copied().collect()
}

/// The window `flow` opens on `store`, woken through `wake`.
pub(crate) fn window(
    flow: Flow,
    store: &Store,
    wake: &ui::WakeHandle,
    opened: Opened<'_>,
) -> Box<dyn Widget> {
    match flow {
        Flow::Wizard => crate::app::window(store, wake, opened),
        #[cfg(feature = "installation")]
        Flow::Setup => crate::setup::journey::window(store, wake, opened),
        #[cfg(feature = "destination")]
        Flow::Destination => crate::setup::destination::window(wake, opened),
    }
}

/// The resource a scenario's shot shows, seeded as the scenario says.
#[expect(
    clippy::panic,
    reason = "a scenario is a fixture of this binary: one its own resource refuses is a bug"
)]
pub(crate) fn scenario_store(scenario: Option<&str>) -> Store {
    let store = Store::in_memory();
    for invoke in crate::resource::scenario(scenario) {
        if let Err(error) = store.apply(invoke) {
            panic!("the scenario {scenario:?} seeds what its resource refuses: {error}");
        }
    }
    store
}

#[expect(
    clippy::exit,
    reason = "the installer's binary ends with the exit code of what it ran"
)]
fn exit(code: i32) -> ! {
    std::process::exit(code)
}

/// Run the installer's binary: one of its helpers, a shot, or the live window.
pub(crate) fn run() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = crate::helpers::run(&args) {
        exit(code);
    }
    let launch = match LaunchArgs::from_env_with(FLAGS, &[]) {
        Ok(launch) => launch,
        Err(error) => {
            eprintln!("Error: {error}");
            exit(2);
        }
    };
    if let Some(out) = &launch.shots {
        if let Err(error) = shot(&launch, out) {
            eprintln!("Error: {error}");
            exit(1);
        }
        return;
    }
    // A window brought back opens on the file it showed, named by the reference the session
    // kept for it, as the launch that first opened it did.
    let restore = launch
        .positionals()
        .first()
        .map(std::path::Path::new)
        .filter(|path| path.is_absolute());
    live(
        Flow::launched(&launch),
        Opened::Live {
            demo: launch.has("--demo"),
            restore,
        },
    );
}

fn shot(launch: &LaunchArgs, out: &std::path::Path) -> Result<(), String> {
    let scenario = launch.scenario.as_deref();
    if let Some(name) = scenario
        && !scenarios().contains(&name)
    {
        return Err(format!("unknown --scenario {name}"));
    }
    tilo_ui_base::clock::freeze_for_capture();
    let store = scenario_store(scenario);
    let root = window(
        Flow::of_scenario(scenario),
        &store,
        &ui::WakeHandle::new(),
        Opened::Shot(scenario),
    );
    let mut frame = ui::headless::Driver::new_with_theme(
        ui::State::new(),
        root,
        launch.viewport.unwrap_or(SHOT_VIEWPORT),
        ui::launch::shot_theme(launch),
    );
    frame.settle_for_capture(ui::launch::CAPTURE_SETTLE_BOUND);
    frame.render_png(out);
    println!("imager: wrote shot to {}", out.display());
    Ok(())
}

/// Open `flow`'s window on this computer and run it until it closes.
pub(crate) fn live(flow: Flow, opened: Opened<'_>) {
    let wake_scope = ui::WakeScope::new();
    let _entered = wake_scope.enter();
    let wake = wake_scope.handle();
    let store = Store::open();
    let root = window(flow, &store, &wake, opened);
    #[cfg(unix)]
    let root: Box<dyn Widget> = match flow {
        // The live medium's installer answers its console itself.
        #[cfg(feature = "destination")]
        Flow::Destination => root,
        _ => Box::new(super::console::Console::serve(
            root,
            store,
            crate::resource::APP_ID,
            wake.clone(),
        )),
    };
    #[cfg(not(unix))]
    drop(store);
    let app = ui::App::from_scoped_parts(wake_scope, ui::State::new(), root)
        .without_native_document_chrome()
        .with_window_title(crate::resource::CATALOG.t("installer-app-title"))
        .with_window_app_id(crate::resource::APP_ID);
    app.run_with_setup(|_| {});
}
