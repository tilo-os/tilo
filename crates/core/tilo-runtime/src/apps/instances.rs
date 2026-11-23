//! The tilolang app instances this daemon starts itself, with no desktop session: an instance
//! that shows nothing, for a call the device's dispatch delivers to an app with no instance
//! running, and an instance whose view the daemon opened for a caller (a page at
//! `/w/<workspace>/<app>`).
//!
//! Each starts in the app's tilolang process, through the platform's launcher, in the sandbox
//! every tilolang process runs in ([`crate::device::processes`]), exactly as a session's window
//! instance does. What differs is only where its connections come from: the daemon makes them
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
use crate::device::processes::{Instance, Processes};

/// A connection the daemon made for an instance, until its collaboration service attests it.
struct Made {
    app: String,
    workspace: String,
    instance: u64,
    /// The caller the instance's view was opened for; `None` for an instance that shows nothing.
    view: Option<Caller>,
}

/// The tilolang instances this daemon starts; see the module docs.
pub(crate) struct Instances {
    /// The daemon's collaboration and identity sockets.
    collab: PathBuf,
    identity: PathBuf,
    /// Where `tilo-launcher` is looked for, in order.
    launcher_candidates: Vec<PathBuf>,
    /// The tilolang processes, once one was asked for.
    processes: OnceLock<Arc<Processes>>,
    /// The member the device acts as: the one an instance that shows nothing is run for.
    person: OnceLock<String>,
    made: Mutex<Vec<Made>>,
    next: AtomicU64,
}

impl Instances {
    pub(crate) fn new(root: &Path, launcher_candidates: Vec<PathBuf>) -> Self {
        Self {
            collab: root.join(crate::COLLAB_SOCKET),
            identity: root.join(crate::IDENTITY_SOCKET),
            launcher_candidates,
            processes: OnceLock::new(),
            person: OnceLock::new(),
            made: Mutex::default(),
            next: AtomicU64::new(1),
        }
    }

    /// The member the device acts as, from now on.
    pub(crate) fn keep_person(&self, person: String) {
        let _ = self.person.set(person);
    }

    /// The tilolang processes, through the launcher found first.
    pub(crate) fn processes(&self) -> Result<&Arc<Processes>, String> {
        if let Some(processes) = self.processes.get() {
            return Ok(processes);
        }
        let launcher = super::linux::resolve_launcher(&self.launcher_candidates)?;
        Ok(self.processes.get_or_init(|| Processes::new(launcher)))
    }

    /// Start an instance of `app` in `workspace` that shows nothing, to take the calls the
    /// dispatch delivers to it: it runs once it is handed to its process.
    pub(crate) fn start_with_no_view(
        &self,
        app: &str,
        workspace: &str,
    ) -> Result<WindowlessStart, String> {
        let person = self
            .person
            .get()
            .cloned()
            .ok_or("this device runs no app's code before it is someone's")?;
        self.start(app, workspace, person, None, None, (None, None))?;
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
        app: &str,
        workspace: &str,
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
            app,
            workspace,
            person,
            Some(caller.clone()),
            Some(patch),
            (record, reveal),
        )?;
        Ok(OpenedView {
            patch: view,
            page: OwnedFd::from(page),
            link,
        })
    }

    /// Hand an instance of `app` in `workspace`, with `person` at it, to its process, starting
    /// the process when none runs: shown over `patch` when it has one, opened for `view`.
    fn start(
        &self,
        app: &str,
        workspace: &str,
        person: String,
        view: Option<Caller>,
        patch: Option<OwnedFd>,
        (document, reveal): (Option<String>, Option<String>),
    ) -> Result<(), String> {
        if !crate::device::package_runs::runs_in_process(app) {
            return Err(format!("{app} is no tilolang app installed here"));
        }
        let processes = Arc::clone(self.processes()?);
        let instance = self.next.fetch_add(1, Ordering::Relaxed);
        let collaboration = self.connect(app, workspace, instance, view)?;
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
                origin: None,
                content: None,
                chooser: None,
                clipboard: None,
                document,
                reveal,
                open: None,
                access: None,
                locale: runtime_portal::process_link::Locale::default(),
                link_home: None,
            })
            .map(drop)
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
        view: Option<Caller>,
    ) -> Result<OwnedFd, String> {
        let mut stream = tilo_files::native::socket::connect(&self.collab)
            .map_err(|error| format!("{app} could not reach its workspace: {error}"))?;
        self.made.lock_recover().push(Made {
            app: app.to_owned(),
            workspace: workspace.to_owned(),
            instance,
            view,
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
        Some(AuthenticatedApplicationLaunch {
            continuation_instance: Some(instance),
            launch_ticket: None,
            windowless: made.view.is_none(),
            opened_for: made.view,
        })
    }
}
