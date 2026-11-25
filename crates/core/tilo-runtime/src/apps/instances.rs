//! The tilolang app instances this daemon starts itself, with no desktop session: an instance
//! that shows nothing, for a call the device's dispatch delivers to an app with no instance
//! running, an instance whose view the daemon opened for a caller (a page at
//! `/w/<workspace>/<app>`), and the instance of a window the daemon launched itself (on macOS
//! and Windows, where the daemon launches apps' windows).
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

#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use runtime_portal::packet::descriptor_of;
use runtime_portal::process_link::Descriptor;
use tilo_actions::remote::Caller;
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::apps::services::{AuthenticatedApplicationLaunch, WindowlessStart};

use super::OpenedView;
use crate::device::processes::{Instance, Launcher, Processes};

/// What an instance shows.
pub(super) enum Shows {
    /// Nothing: it takes the calls the dispatch delivers.
    Nothing,
    /// A window the daemon launched, or one whose instance runs in the daemon's own process (a
    /// phone's), for the member the device acts as.
    Window,
    /// A view opened for a caller the dispatch admitted.
    View(Box<Caller>),
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
#[cfg(any(target_os = "macos", windows))]
pub(crate) struct HostedWindow {
    pub instance: u64,
    pub patch: Descriptor,
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
    /// The files people choose for hosted apps, kept in this device's records.
    #[cfg(target_os = "macos")]
    chosen: Arc<tilo_dialogs::chosen::ChosenTable>,
    /// The member the device acts as: the one an instance that shows nothing is run for.
    person: OnceLock<String>,
    /// The providers this device registered, whose packages every instance is handed.
    provided: OnceLock<tilo_workspace_service::workspaces::providers::Providers>,
    /// What makes each app's own tilolang process its daemon line, once the daemon serves them.
    own_lines: OnceLock<crate::device::processes::OwnLines>,
    made: Mutex<Vec<Made>>,
    next: AtomicU64,
}

/// The file chooser a hosted instance of `app` in `workspace` reaches: a connection this daemon
/// serves, where the person answers in the panel outside the app's sandbox and the app is handed
/// exactly the chosen file. `None` on a platform where the desktop session serves it instead.
#[cfg(target_os = "macos")]
impl Instances {
    fn chooser_for(&self, app: &str, workspace: &str) -> Option<Descriptor> {
        let serving = tilo_dialogs::chooser_service::Serving {
            person: Some(Arc::new(MacPerson)),
            grants: Arc::new(ChosenGrants {
                chosen: Arc::clone(&self.chosen),
            }),
            opens: Arc::new(NoOpen),
        };
        tilo_dialogs::chooser_service::pair(serving, app, workspace)
            .inspect_err(|error| tracing::warn!(app, %error, "a hosted app gets no file chooser"))
            .ok()
            .map(descriptor_of)
    }
}

/// The person answers in this runtime's own file panel, shown outside the app's sandbox
/// ([`tilo_dialogs::confined::show_in_helper`]).
#[cfg(target_os = "macos")]
struct MacPerson;

#[cfg(target_os = "macos")]
impl tilo_dialogs::chooser_service::Person for MacPerson {
    fn choose(
        &self,
        _caller: &tilo_dialogs::chooser_service::Caller,
        _handle: String,
        question: &tilo_dialogs::chooser_service::Question,
    ) -> Result<Vec<PathBuf>, tilo_dialogs::chooser_service::Asked> {
        use tilo_dialogs::chooser_service::Asked;
        let exe = std::env::current_exe().map_err(|_| Asked::Failed)?;
        let mut helper = std::process::Command::new(exe);
        helper.arg(crate::cli::CHOOSER_HELPER_ARG);
        // The helper answers nothing when the person closed the panel without choosing.
        match tilo_dialogs::confined::show_in_helper(helper, &pick_request(question)) {
            Ok(Some(paths)) => Ok(paths),
            Ok(None) => Err(Asked::Cancelled),
            Err(_) => Err(Asked::Failed),
        }
    }
}

/// The question a chooser asks, as this runtime's panel asks it.
#[cfg(target_os = "macos")]
fn pick_request(
    question: &tilo_dialogs::chooser_service::Question,
) -> tilo_dialogs::portal::PickRequest {
    use tilo_dialogs::portal::{FileFilter as PanelFilter, PickRequest};
    let title = question.title.clone();
    let accept = question.accept_label.clone().unwrap_or_default();
    let mut request = if question.save {
        PickRequest::save(
            &title,
            &accept,
            question.current_name.as_deref().unwrap_or(""),
        )
    } else if question.directory {
        if question.writable_directory {
            PickRequest::writable_directory(&title, &accept)
        } else {
            PickRequest::directory(&title, &accept)
        }
    } else if question.multiple {
        PickRequest::open_multiple(&title, &accept)
    } else {
        PickRequest::open(&title, &accept)
    };
    for filter in &question.filters {
        let mut panel = PanelFilter::new(filter.name());
        for glob in filter.globs() {
            panel = panel.glob(glob);
        }
        for mimetype in filter.mimetypes() {
            panel = panel.mimetype(mimetype);
        }
        request = request.filter(panel);
    }
    request
}

/// The device's chosen files, as the chooser reaches it: a choice is a grant, and only a grant
/// this app was given opens again.
#[cfg(target_os = "macos")]
struct ChosenGrants {
    chosen: Arc<tilo_dialogs::chosen::ChosenTable>,
}

#[cfg(target_os = "macos")]
impl tilo_dialogs::chooser_service::Grants for ChosenGrants {
    fn grant(
        &self,
        caller: &tilo_dialogs::chooser_service::Caller,
        chosen: &Path,
        choice: tilo_dialogs::chooser_service::Choice,
    ) -> Result<tilo_dialogs::chooser_service::Granted, String> {
        let key = chosen_key(caller)?;
        self.chosen
            .grant(&key, chosen, kind_of(choice))
            .map(granted_of)
            .map_err(|error| {
                tracing::warn!(
                    %error,
                    path = %chosen.display(),
                    "a chosen item is not kept for the app"
                );
                tilo_dialogs::chooser_service::DOES_NOT_FIT.to_owned()
            })
    }

    fn document(
        &self,
        caller: &tilo_dialogs::chooser_service::Caller,
        reference: &Path,
    ) -> Option<tilo_dialogs::chooser_service::Granted> {
        let key = chosen_key(caller).ok()?;
        self.chosen.looked_up(&key, reference).map(granted_of)
    }
}

/// The app's key, as its launcher names it on the connection.
#[cfg(target_os = "macos")]
fn chosen_key(
    caller: &tilo_dialogs::chooser_service::Caller,
) -> Result<tilo_dialogs::chosen::AppKey, String> {
    tilo_dialogs::chosen::AppKey::new(&caller.workspace, &caller.app)
        .ok_or_else(|| tilo_dialogs::chooser_service::CANNOT_HAND.to_owned())
}

#[cfg(target_os = "macos")]
fn kind_of(choice: tilo_dialogs::chooser_service::Choice) -> tilo_dialogs::chosen::Kind {
    use tilo_dialogs::chooser_service::Choice;
    match choice {
        Choice::Read => tilo_dialogs::chosen::Kind::Read,
        Choice::Folder { write } => tilo_dialogs::chosen::Kind::Folder { write },
        Choice::Save => tilo_dialogs::chosen::Kind::Save,
    }
}

#[cfg(target_os = "macos")]
fn granted_of(chosen: tilo_dialogs::chosen::Chosen) -> tilo_dialogs::chooser_service::Granted {
    use tilo_dialogs::chooser_service::{Choice, Granted};
    let choice = match chosen.kind {
        tilo_dialogs::chosen::Kind::Read => Choice::Read,
        tilo_dialogs::chosen::Kind::Folder { write } => Choice::Folder { write },
        tilo_dialogs::chosen::Kind::Save => Choice::Save,
    };
    Granted {
        reference: chosen.reference,
        name: chosen.name,
        host: chosen.host,
        choice,
        save_sibling: chosen.save_sibling,
    }
}

/// The device's own records, as the chosen files are kept: the same path the desktop session
/// keeps them by, so a hosted instance and a window see one table of choices.
#[cfg(target_os = "macos")]
struct ChosenRecords {
    records: Arc<crate::device::records::DeviceRecords>,
}

#[cfg(target_os = "macos")]
impl tilo_dialogs::chosen::Store for ChosenRecords {
    fn load(&self) -> Vec<tilo_dialogs::chosen::Kept> {
        match self.records.get(tilo_dialogs::chosen::RECORD) {
            Some(serde_json::Value::Array(entries)) => entries
                .into_iter()
                .filter_map(|entry| serde_json::from_value(entry).ok())
                .collect(),
            _ => Vec::new(),
        }
    }

    fn keep(&self, kept: Vec<tilo_dialogs::chosen::Kept>) {
        let value =
            serde_json::to_value(kept).unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
        if let Err(error) = self.records.set(tilo_dialogs::chosen::RECORD, value) {
            tracing::warn!(%error, "the files people chose are not written");
        }
    }
}

/// A daemon opens no links in an app.
#[cfg(target_os = "macos")]
struct NoOpen;

#[cfg(target_os = "macos")]
impl tilo_dialogs::chooser_service::Opens for NoOpen {
    fn open(&self, _workspace: &str, _uri: &str) -> Result<Option<()>, String> {
        Ok(None)
    }
}

impl Instances {
    pub(crate) fn new(
        sockets: &crate::daemon::Sockets,
        launcher: FindsLauncher,
        started: Arc<super::Launched>,
    ) -> Self {
        Self {
            collab: sockets.collab.clone(),
            identity: sockets.identity.clone(),
            launcher,
            launched: started,
            processes: OnceLock::new(),
            #[cfg(target_os = "macos")]
            chosen: Arc::new(tilo_dialogs::chosen::ChosenTable::new()),
            person: OnceLock::new(),
            provided: OnceLock::new(),
            own_lines: OnceLock::new(),
            made: Mutex::default(),
            next: AtomicU64::new(1),
        }
    }

    /// The member the device acts as, from now on.
    pub(crate) fn keep_person(&self, person: String) {
        let _ = self.person.set(person);
    }

    /// Keep the files people choose for hosted apps in the device's records from now on, the same
    /// place the desktop session keeps its choices.
    #[cfg(target_os = "macos")]
    pub(crate) fn keep_chosen_records(&self, records: Arc<crate::device::records::DeviceRecords>) {
        self.chosen.bind(Arc::new(ChosenRecords { records }));
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

    /// Hand each app's own tilolang process started from now on its daemon line, made by `lines`.
    pub(crate) fn serve_own_lines(&self, lines: crate::device::processes::OwnLines) {
        let _ = self.own_lines.set(Arc::clone(&lines));
        if let Some(processes) = self.processes.get() {
            processes.serve_own_lines(lines);
        }
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
            if let Some(lines) = self.own_lines.get() {
                processes.serve_own_lines(Arc::clone(lines));
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
        origin: Option<Descriptor>,
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
        (app, workspace, origin): (&str, &str, Option<Descriptor>),
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
        let (link, page) =
            tilo_socket::pair().map_err(|error| format!("the view's page link: {error}"))?;
        self.start(
            (app, workspace, origin),
            person,
            Shows::View(Box::new(caller.clone())),
            Some(patch),
            (record, reveal),
        )?;
        Ok(OpenedView {
            patch: view,
            page: descriptor_of(page),
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
    #[cfg(any(target_os = "macos", windows))]
    pub(crate) fn open_window(
        &self,
        app: &str,
        workspace: &str,
        origin: Option<Descriptor>,
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

    /// The connections of a window of `app` in `workspace` whose instance runs in this process, as
    /// a phone's do: the daemon is the app's own process there, so the window's instance takes
    /// them itself instead of a process the daemon starts. Answers the member the device acts as,
    /// who is at the window, its collaboration connection, which the collaboration service
    /// attests as that window ([`Self::take`]) exactly as it attests a window the daemon
    /// starts on macOS or Windows, the instance it names, and its identity-service connection.
    ///
    /// # Errors
    /// The device is no one's yet, or a service of the daemon could not be reached.
    pub(crate) fn window_here(
        &self,
        app: &str,
        workspace: &str,
    ) -> Result<(String, u64, Descriptor, Descriptor), String> {
        let person = self.person()?;
        let instance = self.next.fetch_add(1, Ordering::Relaxed);
        let collaboration = self.connect(app, workspace, instance, Shows::Window)?;
        let identity = match self.identity(app, workspace) {
            Ok(identity) => identity,
            Err(error) => {
                let _ = self.take(app, workspace, instance);
                return Err(error);
            }
        };
        Ok((person, instance, collaboration, identity))
    }

    /// Hand an instance of `app` in `workspace`, with `person` at it and reaching its origin's
    /// connector over `origin` when its app has one, to its process, starting the process when
    /// none runs: shown over `patch` when it has one. The instance it started.
    fn start(
        &self,
        (app, workspace, origin): (&str, &str, Option<Descriptor>),
        person: String,
        shows: Shows,
        patch: Option<Descriptor>,
        (document, reveal): (Option<String>, Option<String>),
    ) -> Result<u64, String> {
        if !crate::device::package_runs::runs_in_process(app) {
            return Err(format!("{app} is no tilolang app installed here"));
        }
        let processes = Arc::clone(self.processes()?);
        let instance = self.next.fetch_add(1, Ordering::Relaxed);
        let collaboration = self.connect(app, workspace, instance, shows)?;
        let identity = self.identity(app, workspace)?;
        #[cfg(target_os = "macos")]
        let chooser = self.chooser_for(app, workspace);
        #[cfg(not(target_os = "macos"))]
        let chooser = None;
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
                chooser,
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
    ) -> Result<Descriptor, String> {
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
        Ok(descriptor_of(stream))
    }

    /// The instance's identity-service connection, opened and named for `app` in `workspace` as
    /// the launcher names a launched app's.
    fn identity(&self, app: &str, workspace: &str) -> Result<Descriptor, String> {
        let mut stream = tilo_files::native::socket::connect(&self.identity)
            .map_err(|error| format!("{app} could not reach the identity service: {error}"))?;
        launcher::services::write_identity_handshake(&mut stream, app, workspace)
            .map_err(|error| format!("{app}'s identity connection: {error}"))?;
        Ok(descriptor_of(stream))
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
            Shows::View(caller) => (false, Some(*caller)),
        };
        Some(AuthenticatedApplicationLaunch {
            continuation_instance: Some(instance),
            launch_ticket: None,
            windowless,
            opened_for,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An instance connects at the collaboration socket the daemon it belongs to listens on, not
    /// at one the profile's path would name: a profile named apart keeps its sockets outside
    /// itself, so a path worked out beside the profile reaches nothing and no view opens.
    #[cfg(unix)]
    #[test]
    fn an_instance_reaches_the_collaboration_socket_its_daemon_listens_on() {
        let daemons_sockets = tempfile::tempdir().unwrap();
        let sockets = crate::daemon::Sockets::in_dir(daemons_sockets.path());
        let _listening = std::os::unix::net::UnixListener::bind(&sockets.collab).unwrap();
        let instances = Instances::new(
            &sockets,
            Box::new(|| Err("no launcher".into())),
            Arc::default(),
        );
        let reached = instances.connect(
            "so.tilo.notes",
            "5ba25f73-87c5-447f-a5a6-880dc4249a81",
            1,
            Shows::Window,
        );
        assert!(
            reached.is_ok(),
            "an instance reaches its daemon's collaboration socket: {reached:?}"
        );
    }

    /// A hosted instance is handed a chooser that names it, served by this daemon: the app's
    /// file chooses reach the person instead of nothing.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_hosted_instance_is_handed_a_chooser_named_for_it() {
        let profile = tempfile::tempdir().unwrap();
        let sockets = crate::daemon::Sockets::in_dir(profile.path());
        let instances = Instances::new(
            &sockets,
            Box::new(|| Err("no launcher".into())),
            Arc::default(),
        );
        let fd = instances
            .chooser_for("so.tilo.notes", "5ba25f73-87c5-447f-a5a6-880dc4249a81")
            .expect("a hosted app gets a chooser");
        let stream = std::os::unix::net::UnixStream::from(fd);
        let link = tilo_dialogs::shell::ShellLink::connect(stream).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        link.choose_app("Paste in apps", move |outcome| {
            let _ = tx.send(outcome);
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap(),
            Err(tilo_dialogs::chooser_service::NO_APP_CHOOSER.to_owned()),
            "the daemon's chooser answers the app it was named for"
        );
    }
}
