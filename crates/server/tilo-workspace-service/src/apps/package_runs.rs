//! How a device that holds a workspace runs the workspace's tilolang packages: one tilolang
//! process per workspace and app ([`Processes`]), which serves every page open on the app and
//! every call of its actions that shows nothing. A server and a person's device that holds a
//! workspace start them the same way, through this.
//!
//! Each page, and each run that shows nothing, has its own collaboration connection, made here on
//! the workspace's broker socket and believed by the broker as that page or run only, once
//! ([`PackageRuns::attest_process`], [`PackageRuns::authenticated_launch`]).
//!
//! The device that carries a workspace's migrations also carries what a native app kept there
//! into the package that took its id: [`PackageRuns::carry`] runs the package once for that, showing
//! nothing, and the run ends when its migrations ran.

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tilo_app_launch::tilolang_processes::{Instance, Processes};

use super::services::{AuthenticatedApplicationLaunch, WindowlessStart};

/// How a device starts its packages' processes: through its launcher, or why it cannot.
pub type ProcessStart = Result<Arc<Processes>, String>;

/// The processes a person's device starts through its launcher at `launcher`, each package
/// verified as its installed catalogue holds it.
#[must_use]
pub fn through_launcher(launcher: PathBuf) -> Arc<Processes> {
    Processes::new(launcher)
}

/// Where a device's services for its apps listen, each named by the device that runs them: a run
/// reaches only these, never a default of the machine's.
#[derive(Clone, Debug)]
pub struct Sockets {
    /// The workspace's broker, which every connection is made on.
    pub collab: PathBuf,
    /// The identity service that signs what the apps write.
    pub identity: PathBuf,
    /// The content store the apps keep files and pictures in.
    pub content: PathBuf,
}

/// One connection this device made for an app, waiting for the broker to believe it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Made {
    /// A window: the page that shows the app.
    Window,
    /// A run of the app's code that shows nothing.
    Call,
}

/// The runs of one workspace's packages on this device.
pub struct PackageRuns {
    workspace: String,
    sockets: Sockets,
    /// Who this device is in the workspace: whose the process of a run that shows nothing is.
    /// Each call it runs still runs as the caller who made it.
    device_person: String,
    processes: ProcessStart,
    /// The connections made and not yet believed, by app and tab: each is believed once.
    made: Mutex<BTreeMap<(String, u64), Made>>,
    tabs: AtomicU64,
}

impl PackageRuns {
    /// The runs of `workspace`'s packages, reaching the device's services on `sockets`, started
    /// through `processes`.
    #[must_use]
    pub fn new(
        workspace: String,
        sockets: Sockets,
        device_person: String,
        processes: ProcessStart,
    ) -> Self {
        Self {
            workspace,
            sockets,
            device_person,
            processes,
            made: Mutex::new(BTreeMap::new()),
            tabs: AtomicU64::new(1),
        }
    }

    /// The workspace these runs are of.
    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    fn made(&self) -> std::sync::MutexGuard<'_, BTreeMap<(String, u64), Made>> {
        self.made
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The collaboration connection of the tab `tab` of `app`, which the broker believes as
    /// what `made` says it is, once.
    fn connect(&self, app: &str, tab: u64, made: Made) -> std::io::Result<OwnedFd> {
        let mut stream = std::os::unix::net::UnixStream::connect(&self.sockets.collab)?;
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

    fn processes(&self) -> Result<&Arc<Processes>, String> {
        self.processes.as_ref().map_err(Clone::clone)
    }

    /// What every page and run of `app` is handed beside its collaboration connection: the
    /// device's identity service, serving the call `call` alone when it is made for one, and its
    /// content store, each scoped to the app in this workspace as a launch scopes them.
    fn services(
        &self,
        app: &str,
        call: Option<[u8; 16]>,
    ) -> Result<(OwnedFd, Option<OwnedFd>), String> {
        let identity = launcher::services::resolve_identity_bind_at(
            &self.sockets.identity,
            app,
            &self.workspace,
            call,
        )
        .map_err(|error| format!("the identity service: {error}"))?
        .ok_or("this device's identity service is not running")?;
        let content = launcher::services::resolve_blob_bind_at(
            &self.sockets.content,
            app,
            Some(&self.workspace),
            false,
        )
        .map_err(|error| format!("the content store: {error}"))?
        .map(|bind| bind.host);
        Ok((identity.host, content))
    }

    /// A run of `app` that shows nothing, as this device's own: its connections made, and the
    /// processes that start it.
    fn showing_nothing(&self, app: &str) -> Result<(Arc<Processes>, Instance), String> {
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
        Ok((processes, run))
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

    /// Run `app`'s code for a call that shows nothing: the workspace's process of the app takes
    /// it, started now when none runs. The call runs as its caller, whom the broker checked
    /// before asking. What [`super::services::LauncherServices::windowless`] answers.
    ///
    /// # Errors
    /// `workspace` is not these runs' workspace, or the run's connections cannot be made.
    pub fn run_showing_nothing(
        &self,
        app: &str,
        workspace: &str,
    ) -> Result<Option<WindowlessStart>, String> {
        if workspace != self.workspace {
            return Err(format!(
                "{app} runs here only in workspace {}, not {workspace}",
                self.workspace
            ));
        }
        let (processes, call) = self.showing_nothing(app)?;
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

    /// Carry `documents`, what a native app of the id `app` kept in this workspace as this device
    /// holds them, each by the key it was stored under, into the package now
    /// installed as `app`: they are kept where its process reads what an earlier version of its
    /// app kept, and the package runs once, showing nothing, as the workspace's origin. Its
    /// migrations carry them and mark that they did, so a later carry carries nothing again; the
    /// run ends once they ran. It has the sandbox and grants of any other run of the package.
    ///
    /// # Errors
    /// The documents cannot be kept, no process can start here, or the run's connections cannot
    /// be made.
    pub fn carry(
        &self,
        app: &str,
        documents: Vec<(String, automerge::AutoCommit)>,
    ) -> Result<(), String> {
        if documents.is_empty() {
            return Ok(());
        }
        self.processes()?
            .keep_earlier(&self.workspace, app, documents)
            .map_err(|error| format!("what {app} kept could not be kept for it: {error}"))?;
        let (processes, run) = self.showing_nothing(app)?;
        processes
            .run_carrying_migrations(run)
            .map(drop)
            .map_err(|error| format!("{app} did not run: {error}"))
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
            // connection here yet: such an app keeps to what the workspace holds.
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

    /// Only a connection this process made for one of its pages or runs is one of its apps.
    #[must_use]
    pub fn attest_process(&self, pid: u32, app: &str, workspace: &str) -> bool {
        pid == std::process::id()
            && workspace == self.workspace
            && self.made().keys().any(|(made, _)| made == app)
    }

    /// The launch the connection of the tab `tab` of `app` was made for, taken once: a page, or
    /// a run that shows nothing. `None` for a connection this process did not make.
    #[must_use]
    pub fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
        tab: Option<u64>,
    ) -> Option<AuthenticatedApplicationLaunch> {
        if pid != std::process::id() || workspace != self.workspace {
            return None;
        }
        let tab = tab.filter(|tab| *tab != 0)?;
        let made = self.made().remove(&(app.to_owned(), tab))?;
        Some(AuthenticatedApplicationLaunch {
            continuation_instance: Some(tab),
            launch_ticket: None,
            windowless: made == Made::Call,
        })
    }
}
