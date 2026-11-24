//! The tilolang app instances this daemon starts itself, with no desktop session: an instance
//! that shows nothing, for a call the device's dispatch delivers to an app with no instance
//! running, an instance whose view the daemon opened for a caller (a page at
//! `/w/<workspace>/<app>`), and the instance of a window the daemon launched itself (on macOS,
//! where the daemon launches apps' windows).
//!
//! Each starts in the app's tilolang process, through the platform's launcher, in the sandbox
//! every tilolang process runs in ([`crate::device::processes`]), exactly as a session's window
//! instance does; only that launcher differs between platforms. What differs is only where its connections come from: the daemon makes them
//! itself, from its own process, as a session makes its windows'. The instance's collaboration
//! connection names the instance it is for, and the daemon's collaboration service attests it
//! from the record made here ([`Instances::take`]), never from the process.
//!
//! The connection is the instance's own: the device runs it for the workspace. Who acts travels
//! with each call and each view. A call the dispatch delivers names its invoker, and the
//! instance's writes for it are that invoker's. A view is opened for the caller the dispatch
//! admitted, and what the instance writes for it outside a call is that caller's
//! ([`tilo_workspace_service::apps::services::AuthenticatedApplicationLaunch::opened_for`]):
//! stamped as theirs, refused where they may not make it, and reading only what they may read.

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tilo_actions::remote::Caller;
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::apps::services::{AuthenticatedApplicationLaunch, WindowlessStart};

use super::OpenedView;
use crate::device::processes::{Instance, Launcher, Processes};

/// What an instance shows.
pub(super) enum Shows {
    /// Nothing: it takes the calls the dispatch delivers.
    Nothing,
    /// A window the daemon launched, for the member the device acts as.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(
            dead_code,
            reason = "the desktop session launches a Linux device's windows"
        )
    )]
    Window,
    /// A view opened for a caller the dispatch admitted.
    View(Caller),
}

/// A connection the daemon made for an instance, until its collaboration service attests it.
struct Made {
    app: String,
    workspace: String,
    instance: u64,
    shows: Shows,
}

/// What finds the platform's launcher, once a process is first asked for.
pub(crate) type FindsLauncher = Box<dyn Fn() -> Result<Launcher, String> + Send + Sync>;

/// A window's instance, started in its app's process: the window takes the window's end of its
/// patch connection on its link, as the window `instance`.
#[cfg(target_os = "macos")]
pub(crate) struct HostedWindow {
    pub instance: u64,
    pub patch: OwnedFd,
}

/// The tilolang instances this daemon starts; see the module docs.
pub(crate) struct Instances {
    /// The daemon's collaboration and identity sockets.
    collab: PathBuf,
    identity: PathBuf,
    /// Finds the platform's launcher.
    launcher: FindsLauncher,
    /// The daemon's launch records: each process started here is written there, so a connection
    /// from it is admitted as the app it runs.
    launched: Arc<super::Launched>,
    /// The tilolang processes, once one was asked for.
    processes: OnceLock<Arc<Processes>>,
    /// The member the device acts as: the one an instance that shows nothing is run for.
    person: OnceLock<String>,
    /// The providers this device registered, whose packages every instance is handed.
    provided: OnceLock<tilo_workspace_service::workspaces::providers::Providers>,
    made: Mutex<Vec<Made>>,
    next: AtomicU64,
}

impl Instances {
    pub(crate) fn new(
        root: &Path,
        launcher: FindsLauncher,
        launched: Arc<super::Launched>,
    ) -> Self {
        Self {
            collab: root.join(crate::COLLAB_SOCKET),
            identity: root.join(crate::IDENTITY_SOCKET),
            launcher,
            launched,
            processes: OnceLock::new(),
            person: OnceLock::new(),
            provided: OnceLock::new(),
            made: Mutex::default(),
            next: AtomicU64::new(1),
        }
    }

    /// The member the device acts as, from now on.
    pub(crate) fn keep_person(&self, person: String) {
        let _ = self.person.set(person);
    }

    /// Hand every instance the processes start what `providers` publish, from now on.
    pub(crate) fn read_provided_from(
        &self,
        providers: tilo_workspace_service::workspaces::providers::Providers,
    ) {
        if let Some(processes) = self.processes.get() {
            processes.read_provided_from(providers.clone());
        }
        let _ = self.provided.set(providers);
    }

    /// The tilolang processes, through the launcher found first.
    pub(crate) fn processes(&self) -> Result<&Arc<Processes>, String> {
        if let Some(processes) = self.processes.get() {
            return Ok(processes);
        }
        let launcher = (self.launcher)()?;
        Ok(self.processes.get_or_init(|| {
            let processes = Processes::new(launcher).attesting(Arc::clone(&self.launched));
            if let Some(providers) = self.provided.get() {
                processes.read_provided_from(providers.clone());
            }
            processes
        }))
    }

    /// The member the device acts as.
    fn person(&self) -> Result<String, String> {
        self.person
            .get()
            .cloned()
            .ok_or_else(|| "this device runs no app's code before it is someone's".to_owned())
    }

    /// Start an instance of `app` in `workspace` that shows nothing, to take the calls the
    /// dispatch delivers to it: it runs once it is handed to its process.
    pub(crate) fn start_with_no_view(
        &self,
        app: &str,
        workspace: &str,
        origin: Option<OwnedFd>,
    ) -> Result<WindowlessStart, String> {
        self.start(
            (app, workspace, origin),
            self.person()?,
            Shows::Nothing,
            None,
            (None, None),
        )?;
        let (started, start) = tokio::sync::oneshot::channel();
        let _ = started.send(Ok(()));
        Ok(start)
    }

    /// Open a view of `app` in `workspace` for `caller`, whom the device's dispatch admitted: an
    /// instance of the app shows it, started on `record` (else what it opens by default) and
    /// revealing `reveal`. Answers the descriptors the window process that lays the view out is
    /// started with, and the device's end of its page link.
    ///
    /// # Errors
    /// The caller may not open views here, the app is no tilolang app installed here, or its
    /// instance could not be started.
    pub(crate) fn open_view(
        &self,
        caller: &Caller,
        (app, workspace, origin): (&str, &str, Option<OwnedFd>),
        record: Option<String>,
        reveal: Option<String>,
    ) -> Result<OpenedView, String> {
        if caller
            .workspace()
            .is_some_and(|admitted| admitted != workspace)
        {
            return Err("the caller was admitted to another workspace".to_owned());
        }
        let acting = caller.acting();
        // Who is at the view, for its own state, its presence and its undo.
        let person = acting
            .person_id()
            .or_else(|| acting.acted_for_person())
            .map_or_else(|| acting.by.to_string(), ToOwned::to_owned);
        let (view, patch) = runtime_portal::process_link::pair()
            .map_err(|error| format!("the view's patch connection: {error}"))?;
        let (link, page) = std::os::unix::net::UnixStream::pair()
            .map_err(|error| format!("the view's page link: {error}"))?;
        self.start(
            (app, workspace, origin),
            person,
            Shows::View(caller.clone()),
            Some(patch),
            (record, reveal),
        )?;
        Ok(OpenedView {
            patch: view,
            page: OwnedFd::from(page),
            link,
        })
    }

    /// Start the instance of a window of `app` in `workspace` that this daemon launches, for the
    /// member the device acts as and reaching its origin's connector over `origin` when its app
    /// has one, in the app's process: the window draws what the instance sends on the patch
    /// connection whose window end this answers, and holds none of the app's data.
    ///
    /// # Errors
    /// The device is no one's yet, the app is no tilolang app installed here, or its instance
    /// could not be started.
    #[cfg(target_os = "macos")]
    pub(crate) fn open_window(
        &self,
        app: &str,
        workspace: &str,
        origin: Option<OwnedFd>,
    ) -> Result<HostedWindow, String> {
        let (window, patch) = runtime_portal::process_link::pair()
            .map_err(|error| format!("the window's patch connection: {error}"))?;
        let instance = self.start(
            (app, workspace, origin),
            self.person()?,
            Shows::Window,
            Some(patch),
            (None, None),
        )?;
        Ok(HostedWindow {
            instance,
            patch: window,
        })
    }

    /// Hand an instance of `app` in `workspace`, with `person` at it and reaching its origin's
    /// connector over `origin` when its app has one, to its process, starting the process when
    /// none runs: shown over `patch` when it has one. The instance it started.
    fn start(
        &self,
        (app, workspace, origin): (&str, &str, Option<OwnedFd>),
        person: String,
        shows: Shows,
        patch: Option<OwnedFd>,
        (document, reveal): (Option<String>, Option<String>),
    ) -> Result<u64, String> {
        if !crate::device::package_runs::runs_in_process(app) {
            return Err(format!("{app} is no tilolang app installed here"));
        }
        let processes = Arc::clone(self.processes()?);
        let instance = self.next.fetch_add(1, Ordering::Relaxed);
        let collaboration = self.connect(app, workspace, instance, shows)?;
        let identity = self.identity(app, workspace)?;
        processes
            .hand_over_now(Instance {
                app: app.to_owned(),
                workspace: workspace.to_owned(),
                instance,
                person,
                patch,
                collaboration,
                identity,
                origin,
                content: None,
                chooser: None,
                clipboard: None,
                tools: None,
                document,
                reveal,
                open: None,
                access: None,
                locale: runtime_portal::process_link::Locale::default(),
                link_home: None,
            })
            .map(|_| instance)
            .map_err(|error| format!("{app}'s instance did not start: {error}"))
    }

    /// The instance's collaboration connection, made from this process and named as
    /// `instance` of `app` in `workspace`, whose record the collaboration service takes as it
    /// attests it.
    pub(super) fn connect(
        &self,
        app: &str,
        workspace: &str,
        instance: u64,
        shows: Shows,
    ) -> Result<OwnedFd, String> {
        let mut stream = tilo_files::native::socket::connect(&self.collab)
            .map_err(|error| format!("{app} could not reach its workspace: {error}"))?;
        self.made.lock_recover().push(Made {
            app: app.to_owned(),
            workspace: workspace.to_owned(),
            instance,
            shows,
        });
        if let Err(error) =
            launcher::services::write_collab_window_handoff(&mut stream, app, workspace, instance)
        {
            let _ = self.take(app, workspace, instance);
            return Err(format!(
                "{app}'s instance was not named to its workspace: {error}"
            ));
        }
        Ok(OwnedFd::from(stream))
    }

    /// The instance's identity-service connection, opened and named for `app` in `workspace` as
    /// the launcher names a launched app's.
    fn identity(&self, app: &str, workspace: &str) -> Result<OwnedFd, String> {
        let mut stream = tilo_files::native::socket::connect(&self.identity)
            .map_err(|error| format!("{app} could not reach the identity service: {error}"))?;
        launcher::services::write_identity_handshake(&mut stream, app, workspace)
            .map_err(|error| format!("{app}'s identity connection: {error}"))?;
        Ok(OwnedFd::from(stream))
    }

    /// The launch the collaboration service attests a connection from this process as: the
    /// instance it names, once, when this daemon made a connection for it.
    pub(crate) fn take(
        &self,
        app: &str,
        workspace: &str,
        instance: u64,
    ) -> Option<AuthenticatedApplicationLaunch> {
        let made = {
            let mut made = self.made.lock_recover();
            let at = made.iter().position(|made| {
                made.app == app && made.workspace == workspace && made.instance == instance
            })?;
            made.remove(at)
        };
        let (windowless, opened_for) = match made.shows {
            Shows::Nothing => (true, None),
            Shows::Window => (false, None),
            Shows::View(caller) => (false, Some(caller)),
        };
        Some(AuthenticatedApplicationLaunch {
            continuation_instance: Some(instance),
            launch_ticket: None,
            windowless,
            opened_for,
        })
    }
}
