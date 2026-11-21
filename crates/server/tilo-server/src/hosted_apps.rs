//! The apps a server runs for the workspaces it hosts, on the same app broker and with the same
//! sandbox a device runs them with.
//!
//! A hosted workspace's apps are the packages its governance installed: the server keeps each
//! one's `.tapp` file as the workspace's other holders do, installs it into a package store of
//! that workspace's own and verifies the installation exactly as a device's launch verifies it
//! ([`HostedPackages::verified`]). A package whose bytes do not hold together, or that is not
//! the app it was installed as, is refused.
//!
//! An instance runs in a tilolang process started through `tilo-launcher tilolang-process`
//! (bubblewrap, Landlock, seccomp): a server without that launcher runs no app at all
//! ([`tilo_launcher`]), never one unconfined.
//!
//! The only window a server serves is a page open on it. Each instance's collaboration
//! connection is made by this process for a window it serves, and the broker attests the
//! connection as that window only, once ([`HostedApps::connect_window`]); nothing else that
//! reaches the broker's socket is believed to be one of its apps. What the window shows is the
//! page's to say: the connection names a window, not how it is drawn.

use std::collections::BTreeSet;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tilo_broker_core::services::{
    AuthenticatedApplicationLaunch, LaunchPlace, LaunchableApplication, LauncherServices,
    WindowlessStart,
};
use tilo_desktop_model::Workspace;

/// Why a launch with no page to show it in starts nothing.
pub const NO_WINDOW: &str =
    "this server shows an app only in a page open on it, and no page is open for this launch";

/// Why a server starts no windowless instance yet.
pub const NO_WINDOWLESS: &str =
    "this server runs no app without a page open on it yet, so the action has nowhere to run";

/// The packages of the workspaces this server hosts, each installed into a store of its
/// workspace's own under `root` and verified there as a launch verifies it.
pub struct HostedPackages {
    root: PathBuf,
}

impl HostedPackages {
    /// Packages kept under `root`.
    #[must_use]
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Packages kept under the host's data home, beside the app directories its launcher mounts.
    ///
    /// # Errors
    /// The host has no data home.
    pub fn at_host_data_home() -> Result<Self, String> {
        let data_home = launcher::appdir::host_data_home()
            .ok_or("this server has no data home to keep its workspaces' apps in")?;
        Ok(Self::new(
            data_home.join("tilo").join("hosted-packages"),
        ))
    }

    /// The package store of `workspace`.
    fn store(&self, workspace: &str) -> Result<package_store::PackageStore, String> {
        // A workspace id names a directory: only a real id may.
        let id = uuid::Uuid::parse_str(workspace)
            .map_err(|_| format!("{workspace:?} is no workspace id"))?;
        Ok(package_store::PackageStore::at(
            self.root.join(id.hyphenated().to_string()),
        ))
    }

    /// `tapp`, the package `workspace`'s governance installed as `app`, installed for this
    /// server to run and verified as a launch verifies it: its files, its signature, its
    /// publisher not revoked, and a tilolang app.
    ///
    /// # Errors
    /// A package that does not unpack or whose bytes do not match their names, one that is not
    /// `app`, one whose installation does not verify, or one that is no tilolang app.
    pub fn verified(
        &self,
        workspace: &str,
        app: &str,
        tapp: &[u8],
    ) -> Result<launcher::run::tilolang_process::VerifiedInstance, String> {
        let store = self.store(workspace)?;
        let installed = store
            .install(
                tapp,
                |meta| {
                    let id = &meta.manifest.package.id;
                    if id != app {
                        return Err(format!("the package is {id}, not {app}"));
                    }
                    Ok(package_store::Admission {
                        pinned: false,
                        attested_by: String::new(),
                        source: format!("the governance of workspace {workspace}"),
                        held: false,
                    })
                },
                None,
            )
            .map_err(|error| format!("{app} was refused: {error}"))?
            .package;
        launcher::run::tilolang_process::verified_instance(
            &installed,
            &installed.manifest_path,
            &installed.payload_root,
            &installed.package_id_hex,
        )
        .map_err(|error| format!("{app} does not verify: {error}"))
    }
}

/// Where `tilo-launcher` is looked for on a server: beside this executable, then the system
/// paths.
#[must_use]
pub fn launcher_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
    {
        out.push(dir.join("tilo-launcher"));
    }
    out.push(PathBuf::from("/usr/libexec/tilo/tilo-launcher"));
    out.push(PathBuf::from("/usr/libexec/tilo-launcher"));
    out
}

/// The first of `candidates` that is a file: the launcher every app instance on this server is
/// started through.
///
/// # Errors
/// None is installed: then no app runs here, since none may run unconfined.
pub fn tilo_launcher(candidates: &[PathBuf]) -> Result<PathBuf, String> {
    candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .ok_or_else(|| {
            let searched: Vec<String> = candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            format!(
                "no tilo-launcher is installed on this server (looked at {}), so it runs no app: \
                 an app runs only in the launcher's sandbox",
                searched.join(", ")
            )
        })
}

/// One window this server serves an app in: what the collaboration connection made for it is
/// attested as.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Window {
    app: String,
    workspace: String,
    instance: u64,
}

/// The app broker's launcher on a server: the windows it serves, whose connections it made.
pub struct HostedApps {
    /// The broker's socket, which a window's connection is made on.
    collab_socket: PathBuf,
    /// The windows whose connection was made and not yet attested: each is believed once.
    windows: Mutex<BTreeSet<Window>>,
}

impl HostedApps {
    /// The launcher of the broker listening on `collab_socket`.
    #[must_use]
    pub const fn new(collab_socket: PathBuf) -> Self {
        Self {
            collab_socket,
            windows: Mutex::new(BTreeSet::new()),
        }
    }

    fn windows(&self) -> std::sync::MutexGuard<'_, BTreeSet<Window>> {
        self.windows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The collaboration connection of the window `instance` of `app` in `workspace`, which this
    /// server serves: named for that window, and attested by the broker as that window only, once.
    ///
    /// # Errors
    /// The broker's socket cannot be reached, or the connection could not be named.
    pub fn connect_window(
        &self,
        app: &str,
        workspace: &str,
        instance: u64,
    ) -> std::io::Result<OwnedFd> {
        if instance == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a window's instance is never 0",
            ));
        }
        let window = Window {
            app: app.to_owned(),
            workspace: workspace.to_owned(),
            instance,
        };
        let mut stream = std::os::unix::net::UnixStream::connect(&self.collab_socket)?;
        self.windows().insert(window.clone());
        if let Err(error) =
            launcher::services::write_collab_window_handoff(&mut stream, app, workspace, instance)
        {
            self.windows().remove(&window);
            return Err(error);
        }
        Ok(OwnedFd::from(stream))
    }

    /// Take the record of the window this process made a connection for, once.
    fn take_window(&self, pid: u32, app: &str, workspace: &str, instance: Option<u64>) -> bool {
        if pid != std::process::id() {
            return false;
        }
        let Some(instance) = instance.filter(|instance| *instance != 0) else {
            return false;
        };
        self.windows().remove(&Window {
            app: app.to_owned(),
            workspace: workspace.to_owned(),
            instance,
        })
    }
}

impl LauncherServices for HostedApps {
    /// The apps a hosted workspace offers are its governance's, which the workspace publishes
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

    fn windowless(&self, app: &str, _: &str) -> Result<Option<WindowlessStart>, String> {
        Err(format!("{app} did not run: {NO_WINDOWLESS}"))
    }

    /// Only a connection this process made for a window it serves is one of its apps.
    fn attest_process(&self, pid: u32, app: &str, workspace: &str) -> bool {
        pid == std::process::id()
            && self
                .windows()
                .iter()
                .any(|window| window.app == app && window.workspace == workspace)
    }

    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
        window_instance: Option<u64>,
    ) -> Result<Option<AuthenticatedApplicationLaunch>, String> {
        Ok(self
            .take_window(pid, app, workspace, window_instance)
            .then_some(AuthenticatedApplicationLaunch {
                continuation_instance: window_instance,
                launch_ticket: None,
                windowless: false,
            }))
    }
}

#[cfg(test)]
#[path = "hosted_apps_tests.rs"]
mod tests;
