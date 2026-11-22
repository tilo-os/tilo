//! The app broker's launcher for one workspace a server holds.
//!
//! The server runs a package's code in one tilolang process per workspace and app, which serves
//! every page open on the app and every call of its actions that shows nothing
//! ([`tilo_app_launch::tilolang_processes::Processes`]). Each window, and each such call, has its
//! own collaboration connection, made here on the workspace's broker socket and believed by the
//! broker as that window only, once.

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tilo_app_launch::tilolang_processes::{Instance, Processes};
use tilo_broker_core::services::{
    AuthenticatedApplicationLaunch, LaunchPlace, LaunchableApplication, LauncherServices,
    WindowlessStart,
};
use tilo_desktop_model::Workspace;

/// Why a launch with no page to show it in starts nothing.
pub const NO_WINDOW: &str =
    "this server shows an app only in a page open on it, and no page is open for this launch";

/// How this server starts its apps' processes: through its launcher, or why it cannot.
pub type ProcessStart = Result<Arc<Processes>, String>;

/// One connection this process made for an app, waiting for the broker to believe it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Made {
    /// A window: the page that shows the app.
    Window,
    /// A call of the app's actions that shows nothing.
    Call,
}

/// The app broker's launcher for one workspace a server holds.
pub struct ServerLauncher {
    workspace: String,
    /// The workspace's broker socket, which every connection is made on.
    collab_socket: PathBuf,
    /// Who the server's own device is in the workspace: whose the process of a call that shows
    /// nothing is. Each call still runs as the caller who made it.
    device_person: String,
    processes: ProcessStart,
    /// The connections made and not yet believed, by app and tab: each is believed once.
    made: Mutex<BTreeMap<(String, u64), Made>>,
    tabs: AtomicU64,
}

impl ServerLauncher {
    /// The launcher of `workspace`'s broker on `collab_socket`, starting processes through
    /// `processes`.
    #[must_use]
    pub fn new(
        workspace: String,
        collab_socket: PathBuf,
        device_person: String,
        processes: ProcessStart,
    ) -> Self {
        Self {
            workspace,
            collab_socket,
            device_person,
            processes,
            made: Mutex::new(BTreeMap::new()),
            tabs: AtomicU64::new(1),
        }
    }

    fn made(&self) -> std::sync::MutexGuard<'_, BTreeMap<(String, u64), Made>> {
        self.made
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The collaboration connection of the tab `tab` of `app`, which the broker believes as
    /// what `made` says it is, once.
    fn connect(&self, app: &str, tab: u64, made: Made) -> std::io::Result<OwnedFd> {
        let mut stream = std::os::unix::net::UnixStream::connect(&self.collab_socket)?;
        let key = (app.to_owned(), tab);
        self.made().insert(key.clone(), made);
        if let Err(error) =
            launcher::services::write_collab_window_handoff(&mut stream, app, &self.workspace, tab)
        {
            self.made().remove(&key);
            return Err(error);
        }
        Ok(OwnedFd::from(stream))
    }

    /// The process `app`'s code runs in here, started as the launcher starts it.
    fn processes(&self) -> Result<&Arc<Processes>, String> {
        self.processes.as_ref().map_err(Clone::clone)
    }

    /// What every page and call of `app` here is handed beside its collaboration connection: the
    /// server's identity service, serving the call `call` alone when it is made for one, and its
    /// content store, each scoped to the app in this workspace exactly as a device's launcher
    /// scopes them.
    fn services(
        &self,
        app: &str,
        call: Option<[u8; 16]>,
    ) -> Result<(OwnedFd, Option<OwnedFd>), String> {
        let identity = match call {
            Some(call) => {
                launcher::services::resolve_identity_bind_for_call(app, &self.workspace, call)
            }
            None => launcher::services::resolve_identity_bind(app, Some(&self.workspace), false),
        }
        .map_err(|error| format!("the identity service: {error}"))?
        .ok_or("this server's identity service is not running")?;
        let content = launcher::services::resolve_blob_bind(app, Some(&self.workspace), false)
            .map_err(|error| format!("the content store: {error}"))?
            .map(|bind| bind.host);
        Ok((identity.host, content))
    }

    /// Open a page of `app` for `person`, asked for on the other end of `page`: its view runs in
    /// the workspace's process of the app, started when none runs, and lays its window out
    /// there. Every request it makes of the identity service is decided as whoever made the call
    /// `call` (the page's reader). Answers the page's tab.
    ///
    /// # Errors
    /// No process can start here, the package does not verify, or the page's connections cannot
    /// be made.
    pub fn open_page(
        &self,
        app: &str,
        person: &str,
        call: [u8; 16],
        page: OwnedFd,
    ) -> Result<u64, String> {
        let processes = Arc::clone(self.processes()?);
        let (identity, content) = self.services(app, Some(call))?;
        let tab = self.tabs.fetch_add(1, Ordering::Relaxed);
        let collaboration = self
            .connect(app, tab, Made::Window)
            .map_err(|error| format!("{app} could not reach its workspace: {error}"))?;
        processes
            .show_page(self.handed(
                app,
                person,
                tab,
                Some(page),
                collaboration,
                identity,
                content,
            ))
            .map_err(|error| error.to_string())?;
        Ok(tab)
    }

    fn handed(
        &self,
        app: &str,
        person: &str,
        tab: u64,
        patch: Option<OwnedFd>,
        collaboration: OwnedFd,
        identity: OwnedFd,
        content: Option<OwnedFd>,
    ) -> Instance {
        Instance {
            app: app.to_owned(),
            workspace: self.workspace.clone(),
            instance: tab,
            person: person.to_owned(),
            patch,
            collaboration,
            identity,
            // An app that reaches an outside service, and the person's chooser, have no
            // connection on a server yet: such an app keeps to what the workspace holds.
            origin: None,
            content,
            chooser: None,
            opens: tilo_app_launch::Opens::default(),
            // What each person may do is decided per call by the broker, as the caller who made
            // it; the view offers what the workspace's policy says once it is told.
            access: None,
            locale: runtime_portal::process_link::Locale::default(),
        }
    }

    /// Run `app` for this workspace's server, showing nothing, on this thread: the run carries
    /// what a native app of its id kept here, which [`Processes::keep_earlier`] put where it
    /// reads it. It runs as the server's own device, with the sandbox and grants of any other run
    /// of the package.
    ///
    /// # Errors
    /// No process can start here, the package does not verify, or its connections cannot be made.
    pub fn run_for_server(&self, app: &str) -> Result<(), String> {
        let processes = Arc::clone(self.processes()?);
        let (identity, content) = self.services(app, None)?;
        let tab = self.tabs.fetch_add(1, Ordering::Relaxed);
        let collaboration = self
            .connect(app, tab, Made::Call)
            .map_err(|error| format!("{app} could not reach its workspace: {error}"))?;
        let run = self.handed(
            app,
            &self.device_person,
            tab,
            None,
            collaboration,
            identity,
            content,
        );
        processes
            .run_for_workspace_server(run)
            .map(drop)
            .map_err(|error| format!("{app} did not run: {error}"))
    }

    /// Take what this process made for the tab `tab` of `app`, once.
    fn take(&self, pid: u32, app: &str, workspace: &str, tab: Option<u64>) -> Option<Made> {
        if pid != std::process::id() || workspace != self.workspace {
            return None;
        }
        let tab = tab.filter(|tab| *tab != 0)?;
        self.made().remove(&(app.to_owned(), tab))
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

    /// Run `app`'s code for a call that shows nothing: the workspace's process of the app takes
    /// it, started now when none runs. The call runs as its caller, whom the broker checked
    /// before asking.
    fn windowless(&self, app: &str, workspace: &str) -> Result<Option<WindowlessStart>, String> {
        if workspace != self.workspace {
            return Err(format!(
                "{app} runs here only in workspace {}, not {workspace}",
                self.workspace
            ));
        }
        let processes = Arc::clone(self.processes()?);
        let (identity, content) = self.services(app, None)?;
        let tab = self.tabs.fetch_add(1, Ordering::Relaxed);
        let collaboration = self
            .connect(app, tab, Made::Call)
            .map_err(|error| format!("{app} could not reach its workspace: {error}"))?;
        let call = self.handed(
            app,
            &self.device_person,
            tab,
            None,
            collaboration,
            identity,
            content,
        );
        let (started, start) = tokio::sync::oneshot::channel();
        let app = app.to_owned();
        // Verifying the package and starting its process take their time: never on the
        // broker's own thread.
        std::thread::Builder::new()
            .name("tilolang-call".into())
            .spawn(move || {
                let _ = started.send(
                    processes
                        .hand_over_now(call)
                        .map(drop)
                        .map_err(|error| format!("{app} did not run: {error}")),
                );
            })
            .map_err(|error| format!("could not start the call: {error}"))?;
        Ok(Some(start))
    }

    /// Only a connection this process made for one of its windows or calls is one of its apps.
    fn attest_process(&self, pid: u32, app: &str, workspace: &str) -> bool {
        pid == std::process::id()
            && workspace == self.workspace
            && self.made().keys().any(|(made, _)| made == app)
    }

    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
        tab: Option<u64>,
    ) -> Result<Option<AuthenticatedApplicationLaunch>, String> {
        Ok(self
            .take(pid, app, workspace, tab)
            .map(|made| AuthenticatedApplicationLaunch {
                continuation_instance: tab,
                launch_ticket: None,
                windowless: made == Made::Call,
            }))
    }
}
