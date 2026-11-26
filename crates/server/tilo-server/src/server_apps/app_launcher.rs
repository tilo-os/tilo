//! The app broker's launcher for one workspace a server holds: the workspace's package runs
//! ([`PackageRuns`]), which a server starts as any device that holds a workspace does. A server
//! opens no window, so a launch that would show one is refused.

use std::os::fd::OwnedFd;

use tilo_broker_core::services::{
    AuthenticatedApplicationLaunch, LaunchPlace, LaunchableApplication, LauncherServices,
    WindowlessStart,
};
use tilo_desktop_model::Workspace;
use tilo_workspace_service::apps::package_runs::PackageRuns;
pub use tilo_workspace_service::apps::package_runs::{ProcessStart, Sockets};

/// Why a launch with no page to show it in starts nothing.
pub const NO_WINDOW: &str =
    "this server shows an app only in a page open on it, and no page is open for this launch";

/// The app broker's launcher for one workspace a server holds.
pub struct ServerLauncher {
    runs: PackageRuns,
    /// The packages this server keeps: what tells the ids an app was published under before.
    packages: std::sync::Arc<super::WorkspacePackages>,
}

impl ServerLauncher {
    /// The launcher of `workspace`'s broker, its apps reaching the server's services on
    /// `sockets`, starting processes through `processes` as the server's device, `device_person`,
    /// each the package `packages` keeps.
    #[must_use]
    pub fn new(
        workspace: String,
        sockets: Sockets,
        device_person: String,
        processes: ProcessStart,
        packages: std::sync::Arc<super::WorkspacePackages>,
    ) -> Self {
        Self {
            runs: PackageRuns::new(workspace, sockets, device_person, processes),
            packages,
        }
    }

    /// The workspace's package runs.
    #[must_use]
    pub const fn runs(&self) -> &PackageRuns {
        &self.runs
    }

    /// Open a page of `app` for `person` ([`PackageRuns::open_page`]).
    ///
    /// # Errors
    /// As [`PackageRuns::open_page`].
    pub fn open_page(
        &self,
        app: &str,
        person: &str,
        call: [u8; 16],
        page: OwnedFd,
        access: tilo_action_policy::MemberAccess,
    ) -> Result<u64, String> {
        self.runs.open_page(app, person, call, page, access)
    }
}

impl LauncherServices for ServerLauncher {
    /// The apps a workspace offers are the packages it installed, which the workspace publishes
    /// itself; this launcher lists none of its own.
    fn applications(&self) -> Vec<LaunchableApplication> {
        Vec::new()
    }

    fn launch(
        &self,
        app: &str,
        _: &Workspace,
        _: Option<&str>,
        _: &LaunchPlace,
    ) -> Result<(), String> {
        Err(format!("{app} did not open: {NO_WINDOW}"))
    }

    fn windowless(&self, app: &str, workspace: &str) -> Result<Option<WindowlessStart>, String> {
        self.runs.run_showing_nothing(app, workspace)
    }

    /// The ids the workspace's apps were published under before, as the packages this server
    /// keeps for it say: an app renamed by its publisher keeps its earlier id's records.
    fn earlier_ids(&self) -> tilo_action_policy::EarlierIds {
        self.packages.earlier_ids(self.runs.workspace())
    }

    fn attest_process(&self, pid: u32, app: &str, workspace: &str) -> bool {
        self.runs.attest_process(pid, app, workspace)
    }

    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
        tab: Option<u64>,
    ) -> Result<Option<AuthenticatedApplicationLaunch>, String> {
        Ok(self.runs.authenticated_launch(pid, app, workspace, tab))
    }
}
