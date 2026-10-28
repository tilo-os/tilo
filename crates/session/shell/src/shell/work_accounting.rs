//! Cumulative work at the workspace's construction boundaries, on its UI thread.
//!
//! These count actual factory calls, not cache hits or switch decisions. Keeping
//! them separate makes a rebuilt empty surface distinguishable from remounting
//! applications. They are exposed with the compositor's diagnostic state.
//! Counts saturate rather than wrapping and making cumulative work decrease.

use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub(crate) struct WorkspaceWork {
    pub surface_builds: u64,
    pub tile_resolutions: u64,
    pub app_mounts: u64,
}

thread_local! {
    static WORK: Cell<WorkspaceWork> = const { Cell::new(WorkspaceWork {
        surface_builds: 0,
        tile_resolutions: 0,
        app_mounts: 0,
    }) };
}

pub(crate) fn snapshot() -> WorkspaceWork {
    WORK.get()
}

fn record(increment: impl FnOnce(&mut WorkspaceWork)) {
    WORK.with(|cell| {
        let mut work = cell.get();
        increment(&mut work);
        cell.set(work);
    });
}

pub(crate) fn surface_built() {
    record(|work| work.surface_builds = work.surface_builds.saturating_add(1));
}

pub(crate) fn tile_resolved() {
    record(|work| work.tile_resolutions = work.tile_resolutions.saturating_add(1));
}

pub(crate) fn app_mounted() {
    record(|work| work.app_mounts = work.app_mounts.saturating_add(1));
}
