//! A live window's join of its workspace, made off the window's thread.
//!
//! The launch starts the join on a thread of its own and shows the window's
//! loading state while it runs; the join's end tells the window, which gives
//! its place to the app. A later window of the process joins before it is
//! built ([`crate::window_door`]).

use super::{LaunchArgs, LaunchError, LaunchSpec};
use crate::App;

/// What [`join_in_window`] ended with.
pub(super) enum WindowJoin {
    /// The join is over, or there was none to make (`None`): the launch
    /// builds the app.
    Joined(Option<Result<crate::collaboration::NativeLaunch, LaunchError>>),
    /// The person closed the window while it joined.
    Closed,
}

/// Start the workspace's join of a live window that joins one, off this
/// thread. While it runs the window shows its loading state ([`joining_window`],
/// dressed by `dress`), and the join's end tells it: the window then gives
/// its place to the app. No clock is involved: the join ends when the broker
/// answers or fails, or its worker stops, and the window waits for that
/// without blocking its thread.
pub(super) fn join_in_window(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    dress: impl FnOnce(App) -> App,
) -> WindowJoin {
    if !joins_its_workspace(spec, args) {
        return WindowJoin::Joined(None);
    }
    if let Err(error) = crate::collaboration::native_initial::ensure_unoccupied() {
        return WindowJoin::Joined(Some(Err(LaunchError::Operation(error))));
    }
    let launch = crate::collaboration::CollaborationManager::begin_native_launch(
        &spec.definition.record_kind,
    );
    if let Ok(crate::collaboration::NativeLaunch::Joining(join)) = &launch
        && !join.is_done()
    {
        let window = dress(joining_window(spec));
        {
            let _entry = window.enter_wake_scope();
            let wake = crate::wake::WakeHandle::new();
            join.when_done(move || {
                WINDOW_GIVES_PLACE.store(true, std::sync::atomic::Ordering::Release);
                wake.wake();
            });
        }
        window.run();
        if !WINDOW_GIVES_PLACE.swap(false, std::sync::atomic::Ordering::AcqRel) {
            return WindowJoin::Closed;
        }
    }
    WindowJoin::Joined(Some(launch))
}

/// The window a launch shows while it joins its workspace: the shape of the
/// page that is on its way.
fn joining_window(_spec: &LaunchSpec) -> App {
    App::without_workspace(|| {
        Box::new(crate::widget::Skeleton::new(
            crate::widget::SkeletonShape::Page,
        ))
    })
}

/// Set when what a window held the launch's place for has arrived (the
/// workspace's join [`join_in_window`] showed its loading state for): the
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
/// workspace: its join runs off the window's thread
/// ([`crate::collaboration::CollaborationManager::begin_native_launch`]).
pub(crate) fn joins_its_workspace(spec: &LaunchSpec, args: &LaunchArgs) -> bool {
    live_native(spec, args) && (spec.runtime_attachment)(args)
}

/// Join a window's own `connection` (`crate::window_door`) to `spec`'s
/// workspace, off the calling thread.
pub(crate) fn join_window_connection(
    spec: &LaunchSpec,
    connection: runtime_portal::windows::Descriptor,
) -> Result<crate::collaboration::NativeJoin, LaunchError> {
    crate::collaboration::CollaborationManager::join_window_connection(
        connection,
        &spec.definition.record_kind,
    )
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
