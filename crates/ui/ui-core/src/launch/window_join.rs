//! A live window opens at once on what this device holds, and joins its workspace in the background.
//! The launch starts the join without waiting for it
//! ([`tilo_ui_base::collaboration::CollaborationManager::join_native`]) and builds the app on its local
//! replica: the broker's answer arrives later as an event the running app takes, and the route's
//! committed history merges in as at any other time. Only a window whose record is not on this device
//! yet (its first open here) waits: it shows the page's loading state until the join is over, and is
//! built then from the workspace's committed history.

use super::{DefinitionDispatch, LaunchArgs, LaunchError, LaunchSpec};
use crate::App;

/// A launch whose record is not on this device yet: the workspace's join, waited for off the window's
/// thread, and the wake scope the app built after it takes, which the join's manager wakes.
pub(crate) struct Arriving {
    pub(crate) join: tilo_ui_base::collaboration::NativeJoin,
    scope: crate::wake::WakeScope,
}

impl std::fmt::Debug for Arriving {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Arriving").finish_non_exhaustive()
    }
}

enum LocalFirst {
    Built(Result<DefinitionDispatch, LaunchError>),
    Arriving(Arriving),
}

/// The join of a live window that joins its workspace, started: `None` for any other launch.
///
/// The join's worker wakes the app at each step it takes, and what arrives over it later (an
/// invocation, the action surface it publishes) wakes the app the same way: so the join starts in the
/// wake scope the app is built in, made here, before the app is.
pub(super) fn start_join(spec: &LaunchSpec, args: &LaunchArgs) -> Option<Joining> {
    (live_native(spec, args) && spec.attaches_runtime(args)).then(|| {
        join_in_scope(|| {
            tilo_ui_base::collaboration::native_initial::ensure_unoccupied()
                .map_err(LaunchError::Operation)?;
            tilo_ui_base::collaboration::CollaborationManager::join_native(
                &spec.definition.resource_type,
                native_connection()?,
            )
            .map_err(LaunchError::from)
        })
    })
}

/// The live window `args` launch, built as the launch builds its first window, without running
/// its loop: joined to its workspace on the connection the launch has ([`native_connection`]), and
/// when its record is not on this device yet, built once that join is over. What hosts the window
/// other than the launch's own loop drives, a headless one included.
///
/// # Errors
/// The window's join or its build failed, or `args` launch no live window.
pub(super) fn build_window(spec: &LaunchSpec, args: &LaunchArgs) -> Result<App, LaunchError> {
    let dispatched = match dispatch_local_first(spec, args, start_join(spec, args)) {
        LocalFirst::Built(dispatched) => dispatched?,
        LocalFirst::Arriving(arriving) => dispatch_arrived(spec, args, arriving)?,
    };
    dispatched
        .into_app()
        .ok_or_else(|| LaunchError::Operation("this launch opens no window".to_owned()))
}

/// What a process that is its own launcher handed its first window
/// ([`super::hand_connection`]): the connection its join takes, once, or why the process could not
/// make one.
static HANDED: std::sync::Mutex<
    Option<Result<tilo_ui_base::collaboration::HandedConnection, String>>,
> = std::sync::Mutex::new(None);

/// Whether this process handed its first window a connection, or tried to: it is its own launcher,
/// and its windows join the workspace as a launched process's do.
static WAS_HANDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(super) fn hand(connection: Result<tilo_ui_base::collaboration::HandedConnection, String>) {
    *HANDED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(connection);
    WAS_HANDED.store(true, std::sync::atomic::Ordering::Release);
}

pub(super) fn was_handed() -> bool {
    WAS_HANDED.load(std::sync::atomic::Ordering::Acquire)
}

/// Start the join `join` makes in a wake scope of its own, the one the app is built in.
fn join_in_scope(
    join: impl FnOnce() -> Result<tilo_ui_base::collaboration::CollaborationManager, LaunchError>,
) -> Joining {
    let scope = crate::wake::WakeScope::new();
    let manager = {
        let _entered = scope.enter();
        join()
    };
    Joining { scope, manager }
}

/// A started join and the wake scope it runs in.
pub(super) struct Joining {
    scope: crate::wake::WakeScope,
    manager: Result<tilo_ui_base::collaboration::CollaborationManager, LaunchError>,
}

impl std::fmt::Debug for Joining {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Joining").finish_non_exhaustive()
    }
}

/// Dispatch a launch: one that joins its workspace builds at once on the local replica while the join
/// runs, or, where the record is not on this device, hands back the join to wait for.
fn dispatch_local_first(spec: &LaunchSpec, args: &LaunchArgs, join: Option<Joining>) -> LocalFirst {
    let Some(Joining { scope, manager }) = join else {
        return LocalFirst::Built(super::dispatch_definition(spec, args));
    };
    match dispatch_definition_in(spec, args, Some(scope), || manager) {
        Ok(dispatched) => LocalFirst::Built(Ok(dispatched)),
        Err(Unbuilt::Failed(error)) => LocalFirst::Built(Err(error)),
        // The join is waited for off this thread.
        Err(Unbuilt::NotHere { manager, scope }) => {
            match tilo_ui_base::collaboration::NativeJoin::start(manager) {
                Ok(join) => LocalFirst::Arriving(Arriving { join, scope }),
                Err(error) => LocalFirst::Built(Err(error.into())),
            }
        }
    }
}

/// Build the app `arriving` waited for, now that its join is over: from the workspace's committed
/// history, in the scope its manager wakes.
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

/// Dispatch a live launch that waits in its window when its record arrives with its join (see
/// [`dispatch_local_first`], [`wait_in_window`]); `None` when the person closed the window meanwhile.
pub(super) fn dispatch_live(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    joining: Option<Joining>,
    dress: impl FnOnce(App) -> App,
) -> Option<Result<DefinitionDispatch, LaunchError>> {
    match dispatch_local_first(spec, args, joining) {
        LocalFirst::Built(dispatched) => Some(dispatched),
        LocalFirst::Arriving(arriving) => {
            let arrived = wait_in_window(arriving, dress)?;
            Some(dispatch_arrived(spec, args, arrived))
        }
    }
}

/// Show the window's loading state ([`arriving_window`], dressed by `dress`) until `arriving`'s join is
/// over, then hand it back: the window gives its place to the app.
pub(super) fn wait_in_window(
    arriving: Arriving,
    dress: impl FnOnce(App) -> App,
) -> Option<Arriving> {
    if arriving.join.is_done() {
        return Some(arriving);
    }
    let mut window = dress(arriving_window());
    // What the person types while the record arrives is the app's, which takes it once it stands.
    window.stand_in_for_arriving_app();
    {
        let _entry = window.enter_wake_scope();
        let wake = crate::wake::WakeHandle::new();
        arriving.join.when_done(move || {
            WINDOW_GIVES_PLACE.store(true, std::sync::atomic::Ordering::Release);
            wake.wake();
        });
    }
    window.run();
    WINDOW_GIVES_PLACE
        .swap(false, std::sync::atomic::Ordering::AcqRel)
        .then_some(arriving)
}

/// The window a launch shows while its record arrives with the workspace's join: the shape of the page
/// that is on its way.
fn arriving_window() -> App {
    App::without_workspace(|| {
        Box::new(crate::widget::Skeleton::new(
            tilo_ui_base::widget::SkeletonShape::Page,
        ))
    })
}

/// Set when what a window held the launch's place for has arrived (the workspace's join
/// [`wait_in_window`] showed its loading state for): the window leaves its loop, keeping the window, so
/// the launch opens the app in it.
static WINDOW_GIVES_PLACE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether the window in front gives its place to the app (see [`WINDOW_GIVES_PLACE`]).
pub(crate) fn window_gives_place() -> bool {
    WINDOW_GIVES_PLACE.load(std::sync::atomic::Ordering::Acquire)
}

/// Whether `args` open a live window of the native component `spec`.
pub(super) fn live_native(spec: &LaunchSpec, args: &LaunchArgs) -> bool {
    spec.component_native_services && super::opens_native_window(args)
}

/// The live app `args` launch, built as the process's first window was (`crate::window_link`); `None`
/// when they launch no live window.
pub(crate) fn dispatch_window(
    spec: &LaunchSpec,
    args: &LaunchArgs,
) -> Result<WindowBuild, LaunchError> {
    let args = window_args(args);
    match dispatch_local_first(spec, &args, start_join(spec, &args)) {
        LocalFirst::Built(dispatched) => {
            dispatched.map(|dispatched| WindowBuild::Built(dispatched.into_app()))
        }
        LocalFirst::Arriving(arriving) => Ok(WindowBuild::Arriving(arriving)),
    }
}

/// [`dispatch_window`] once the join of a window whose record was not on this device is over.
pub(crate) fn dispatch_window_arrived(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    arriving: Arriving,
) -> Result<Option<App>, LaunchError> {
    let args = window_args(args);
    dispatch_arrived(spec, &args, arriving).map(DefinitionDispatch::into_app)
}

/// The process's launch, opening what the shell's request names in place of what its first window was
/// launched on.
fn window_args(args: &LaunchArgs) -> LaunchArgs {
    let mut args = args.clone();
    args.positionals = crate::window_link::requested_open().into_iter().collect();
    args
}

/// What [`dispatch_window`] built.
pub(crate) enum WindowBuild {
    /// The live app, if the launch opens one.
    Built(Option<App>),
    /// The window's record arrives with its join; build it then ([`dispatch_window_arrived`]).
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
    /// The record is not on this device yet and the manager, still joining its workspace, brings it
    /// ([`LaunchError::ResourceNotHere`]): the manager and the scope it wakes, to build in once the join
    /// is over.
    NotHere {
        manager: tilo_ui_base::collaboration::CollaborationManager,
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
            Self::NotHere { .. } => LaunchError::ResourceNotHere,
        }
    }
}

/// [`dispatch_definition`] of a live window in `scope`, the wake scope the app takes, on the manager
/// `manager` makes in it.
pub(super) fn dispatch_definition_in(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    scope: Option<crate::wake::WakeScope>,
    manager: impl FnOnce() -> Result<tilo_ui_base::collaboration::CollaborationManager, LaunchError>,
) -> Result<DefinitionDispatch, Unbuilt> {
    super::dispatch_definition_with(spec, args, construct_native, scope, manager)
}

/// The connection a native launch's manager joins on: a window the shell opened in this running process
/// has one of its own ([`crate::window_link`]); the one the launcher handed the process is its first
/// window's.
///
/// A process that is its own launcher hands its first window the connection it made for it
/// ([`super::hand_connection`]); a window of such a process with none (its making failed, or a
/// later window took it) does not open, rather than open on records of this device alone.
///
/// # Errors
/// The process is its own launcher and has no connection for this window.
pub(super) fn native_connection()
-> Result<tilo_ui_base::collaboration::NativeConnection, LaunchError> {
    if crate::window_link::is_building() {
        return Ok(tilo_ui_base::collaboration::NativeConnection::Window(
            crate::window_link::take_collaboration(),
        ));
    }
    if !was_handed() {
        return Ok(tilo_ui_base::collaboration::NativeConnection::Launch);
    }
    let handed = HANDED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    match handed {
        Some(Ok(connection)) => Ok(tilo_ui_base::collaboration::NativeConnection::Handed(
            connection,
        )),
        Some(Err(why)) => Err(LaunchError::Operation(why)),
        None => Err(LaunchError::Operation(
            "this window has no connection to its workspace: its process handed one to its first \
             window only"
                .to_owned(),
        )),
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

#[cfg(all(test, unix, feature = "broker-transport"))]
mod tests {
    use super::*;

    /// The join a launch starts before its window wakes the scope the app is then built in, at each
    /// step it takes: a join whose wakes reached no scope completed, and nothing that arrived over it
    /// (an invocation, the action surface to publish) ever woke the running app to answer.
    #[test]
    fn the_early_join_wakes_the_scope_the_app_is_built_in() {
        use crate::application_test_support as probe;
        let workspace: uuid::Uuid = "3f2b1c00-0000-4000-8000-00000000000e".parse().unwrap();
        let mut broker = None;
        let joining = join_in_scope(|| {
            let (manager, probe) =
                tilo_ui_base::collaboration::CollaborationManager::native_joining_fixture_for_test(
                    tilo_ui_base::collaboration::RouteSpec::app(probe::APP, workspace),
                    "probe",
                    automerge::AutoCommit::new().save(),
                    runtime_collab::WorkspaceStatus::Ready,
                );
            broker = Some(probe);
            Ok(manager)
        });
        let broker = broker.unwrap();
        let manager = joining.manager.as_ref().unwrap();
        assert!(manager.is_joining());
        let _ = joining.scope.take_unbound();

        broker.answer();
        let deadline = tilo_test_env::hang_deadline();
        while !joining.scope.is_pending() {
            assert!(
                crate::time::now() < deadline,
                "the join's answer never woke the app's scope"
            );
            std::thread::yield_now();
        }
    }
}
