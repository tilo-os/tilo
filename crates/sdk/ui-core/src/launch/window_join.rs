//! A live window opens at once on what this device holds, and joins its
//! workspace in the background.
//!
//! The launch starts the join without waiting for it
//! ([`crate::collaboration::CollaborationManager::join_native`]) and builds
//! the app on its local replica: the broker's answer arrives later as an
//! event the running app takes, and the route's committed history merges in
//! as at any other time. Only a window whose record is not on this device
//! yet (its first open here) waits: it shows the page's loading state until
//! the join is over, and is built then from the room's committed history.
//! A later window of the process waits the same way, off the window's
//! thread ([`crate::window_door`]).

use super::{DefinitionDispatch, LaunchArgs, LaunchError, LaunchSpec};
use crate::App;

/// A launch whose record is not on this device yet: the workspace's join,
/// waited for off the window's thread, and the wake scope the app built
/// after it takes, which the join's manager wakes.
pub(crate) struct Arriving {
    join: crate::collaboration::NativeJoin,
    scope: crate::wake::WakeScope,
}

impl std::fmt::Debug for Arriving {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Arriving")
            .field("join", &self.join)
            .finish_non_exhaustive()
    }
}

impl Arriving {
    /// Wait for `manager`'s join (in `scope`) off this thread.
    pub(super) fn start(
        manager: crate::collaboration::CollaborationManager,
        scope: crate::wake::WakeScope,
    ) -> Result<Self, LaunchError> {
        let join = crate::collaboration::NativeJoin::start(manager)?;
        Ok(Self { join, scope })
    }

    /// Call `told` once the join is over (see
    /// [`crate::collaboration::NativeJoin::when_done`]).
    pub(crate) fn when_done(&self, told: impl FnOnce() + Send + 'static) {
        self.join.when_done(told);
    }
}

/// What a live launch's dispatch ended with.
enum LocalFirst {
    /// Built, on the local replica or without a workspace, or failed.
    Built(Result<DefinitionDispatch, LaunchError>),
    /// The record is not on this device yet; build once the join is over
    /// ([`dispatch_arrived`]).
    Arriving(Arriving),
}

/// Dispatch a launch: one that joins its workspace builds at once on the
/// local replica while the join runs, or, where the record is not on this
/// device, hands back the join to wait for.
fn dispatch_local_first(spec: &LaunchSpec, args: &LaunchArgs) -> LocalFirst {
    if !joins_its_workspace(spec, args) {
        return LocalFirst::Built(super::dispatch_definition(spec, args));
    }
    let scope = crate::wake::WakeScope::new();
    match dispatch_definition_in(spec, args, Some(scope), || {
        crate::collaboration::native_initial::ensure_unoccupied()
            .map_err(LaunchError::Operation)?;
        crate::collaboration::CollaborationManager::join_native(
            &spec.definition.record_kind,
            native_connection(),
        )
        .map_err(LaunchError::from)
    }) {
        Ok(dispatched) => LocalFirst::Built(Ok(dispatched)),
        Err(Unbuilt::Failed(error)) => LocalFirst::Built(Err(error)),
        Err(Unbuilt::NotHere { manager, scope }) => match Arriving::start(manager, scope) {
            Ok(arriving) => LocalFirst::Arriving(arriving),
            Err(error) => LocalFirst::Built(Err(error)),
        },
    }
}

/// Build the app `arriving` waited for, now that its join is over: from the
/// room's committed history, in the scope its manager wakes.
fn dispatch_arrived(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    arriving: Arriving,
) -> Result<DefinitionDispatch, LaunchError> {
    let Arriving { join, scope } = arriving;
    dispatch_definition_in(spec, args, Some(scope), move || {
        join.finish().map_err(LaunchError::from)
    })
    .map_err(Unbuilt::into_error)
}

/// Dispatch a live launch that waits in its window when its record arrives
/// with its join (see [`dispatch_local_first`], [`wait_in_window`]); `None`
/// when the person closed the window meanwhile.
pub(super) fn dispatch_live(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    dress: impl FnOnce(App) -> App,
) -> Option<Result<DefinitionDispatch, LaunchError>> {
    match dispatch_local_first(spec, args) {
        LocalFirst::Built(dispatched) => Some(dispatched),
        LocalFirst::Arriving(arriving) => {
            let arrived = wait_in_window(spec, arriving, dress)?;
            Some(dispatch_arrived(spec, args, arrived))
        }
    }
}

/// Show the window's loading state ([`arriving_window`], dressed by `dress`)
/// until `arriving`'s join is over, then hand it back: the window gives its
/// place to the app. `None` when the person closed the window meanwhile. No
/// clock is involved: the join ends when the broker answers or fails, or its
/// worker stops, and the window waits for that without blocking its thread.
pub(super) fn wait_in_window(
    spec: &LaunchSpec,
    arriving: Arriving,
    dress: impl FnOnce(App) -> App,
) -> Option<Arriving> {
    if arriving.join.is_done() {
        return Some(arriving);
    }
    let window = dress(arriving_window(spec));
    {
        let _entry = window.enter_wake_scope();
        let wake = crate::wake::WakeHandle::new();
        arriving.when_done(move || {
            WINDOW_GIVES_PLACE.store(true, std::sync::atomic::Ordering::Release);
            wake.wake();
        });
    }
    window.run();
    WINDOW_GIVES_PLACE
        .swap(false, std::sync::atomic::Ordering::AcqRel)
        .then_some(arriving)
}

/// The window a launch shows while its record arrives with the workspace's
/// join: the shape of the page that is on its way.
fn arriving_window(_spec: &LaunchSpec) -> App {
    App::without_workspace(|| {
        Box::new(crate::widget::Skeleton::new(
            crate::widget::SkeletonShape::Page,
        ))
    })
}

/// Set when what a window held the launch's place for has arrived (the
/// workspace's join [`wait_in_window`] showed its loading state for): the
/// window leaves its loop, keeping the window, so the launch opens the app
/// in it.
static WINDOW_GIVES_PLACE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether the window in front gives its place to the app (see
/// [`WINDOW_GIVES_PLACE`]).
pub(crate) fn window_gives_place() -> bool {
    WINDOW_GIVES_PLACE.load(std::sync::atomic::Ordering::Acquire)
}

/// Whether a launch with `args` opens a live window that joins its
/// workspace.
pub(crate) fn joins_its_workspace(spec: &LaunchSpec, args: &LaunchArgs) -> bool {
    live_native(spec, args) && (spec.runtime_attachment)(args)
}

/// Whether `args` open a live window of the native component `spec`.
pub(super) fn live_native(spec: &LaunchSpec, args: &LaunchArgs) -> bool {
    spec.component_native_services
        && args.headless.is_none()
        && args.shots.is_none()
        && args.responsive_matrix.is_none()
        && !args.native_audit
        && !args.first_frame
}

/// The live app `args` launch, built as the process's first window was
/// (`crate::window_door`); `None` when they launch no live window.
pub(crate) fn dispatch_window(
    spec: &LaunchSpec,
    args: &LaunchArgs,
) -> Result<WindowBuild, LaunchError> {
    let mut args = args.clone();
    args.positionals.clear();
    match dispatch_local_first(spec, &args) {
        LocalFirst::Built(dispatched) => {
            dispatched.map(|dispatched| WindowBuild::Built(dispatched.into_app()))
        }
        LocalFirst::Arriving(arriving) => Ok(WindowBuild::Arriving(arriving)),
    }
}

/// [`dispatch_window`] once the join of a window whose record was not on
/// this device is over.
pub(crate) fn dispatch_window_arrived(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    arriving: Arriving,
) -> Result<Option<App>, LaunchError> {
    let mut args = args.clone();
    args.positionals.clear();
    dispatch_arrived(spec, &args, arriving).map(DefinitionDispatch::into_app)
}

/// What [`dispatch_window`] built.
pub(crate) enum WindowBuild {
    /// The live app, if the launch opens one.
    Built(Option<App>),
    /// The window's record arrives with its join; build it then
    /// ([`dispatch_window_arrived`]).
    Arriving(Arriving),
}

impl DefinitionDispatch {
    fn into_app(self) -> Option<App> {
        match self {
            Self::Native(app) => Some(*app),
            _ => None,
        }
    }
}

/// Why a dispatch built no app.
pub(super) enum Unbuilt {
    Failed(LaunchError),
    /// The record is not on this device yet and the manager, still joining
    /// its workspace, brings it ([`LaunchError::RecordNotHere`]): the
    /// manager and the scope it wakes, to build in once the join is over.
    NotHere {
        manager: crate::collaboration::CollaborationManager,
        scope: crate::wake::WakeScope,
    },
}

impl From<LaunchError> for Unbuilt {
    fn from(error: LaunchError) -> Self {
        Self::Failed(error)
    }
}

impl Unbuilt {
    pub(crate) fn into_error(self) -> LaunchError {
        match self {
            Self::Failed(error) => error,
            Self::NotHere { .. } => LaunchError::RecordNotHere,
        }
    }
}

/// [`dispatch_definition`] of a live window in `scope`, the wake scope the
/// app takes, on the manager `manager` makes in it.
pub(super) fn dispatch_definition_in(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    scope: Option<crate::wake::WakeScope>,
    manager: impl FnOnce() -> Result<crate::collaboration::CollaborationManager, LaunchError>,
) -> Result<DefinitionDispatch, Unbuilt> {
    super::dispatch_definition_with(spec, args, construct_native, scope, manager)
}

/// The connection a native launch's manager joins on: a window the shell
/// opened in this running process has one of its own
/// ([`crate::window_door`]); the one the launcher handed the process is its
/// first window's.
pub(super) fn native_connection() -> crate::collaboration::NativeConnection {
    if crate::window_door::is_building() {
        crate::collaboration::NativeConnection::Window(crate::window_door::take_collaboration())
    } else {
        crate::collaboration::NativeConnection::Launch
    }
}

/// Build the app of a prepared native component.
pub(super) fn construct_native(
    component: crate::application::AppComponent,
    inputs: crate::application::ComponentRunnerInputs,
    native_services: bool,
) -> Result<App, LaunchError> {
    tilo_startup_metrics::span("app-construct", || {
        if native_services {
            App::from_component_with_native_services(component, inputs)
        } else {
            App::from_component(component, inputs)
        }
    })
}
