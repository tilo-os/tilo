//! The tilolang app instances this daemon starts itself, with no desktop session: an instance
//! that shows nothing, for a call the device's dispatch delivers to an app with no instance
//! running, an instance whose view the daemon opened for a caller (a page at
//! `/w/<workspace>/<app>`), and the instance of a window: the daemon launches the window's
//! process itself on macOS and Windows, and on Linux, where only a session draws windows, it
//! starts the code behind the window the person's layout holds.
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
//! ([`tilo_app_host::services::AuthenticatedApplicationLaunch::opened_for`]):
//! stamped as theirs, refused where they may not make it, and reading only what they may read.

#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use runtime_portal::packet::descriptor_of;
use runtime_portal::process_link::Descriptor;
use tilo_actions::remote::Caller;
use tilo_app_host::services::{AuthenticatedApplicationLaunch, WindowlessStart};
use tilo_lock::MutexRecover as _;

use super::OpenedView;
use crate::device::processes::{Ends, Instance, Launcher, Processes};

/// What an instance shows.
pub(super) enum Shows {
    /// Nothing: it takes the calls the dispatch delivers, opened for `caller` when the dispatch
    /// admitted one that asked for the app's view, whose grants its reads are decided by.
    Nothing(Option<Box<Caller>>),
    /// A window the daemon launched, or one whose instance runs in the daemon's own process (a
    /// phone's), for the member the device acts as.
    Window,
    /// A view opened for a caller the dispatch admitted, with the certificate the link of the
    /// page that opened it verified, when a page did.
    View(Box<(Caller, Option<tilo_user_identity::DeviceCert>)>),
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
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
pub(crate) struct HostedWindow {
    pub instance: u64,
    pub patch: Descriptor,
}

/// The tilolang instances this daemon starts; see the module docs.
pub(crate) struct Instances {
    /// The daemon's collaboration and identity sockets.
    collab: PathBuf,
    identity: PathBuf,
    /// The daemon's document-media socket, where an instance keeps the files it makes.
    #[cfg(unix)]
    blobs: PathBuf,
    /// Finds the platform's launcher.
    launcher: FindsLauncher,
    /// The daemon's launch records: each process started here is written there, so a connection
    /// from it is admitted as the app it runs.
    launched: Arc<super::Launched>,
    /// The tilolang processes, once one was asked for.
    processes: OnceLock<Arc<Processes>>,
    /// The files people choose for hosted apps, kept in this device's records.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    chosen: Arc<tilo_dialogs::chosen::ChosenTable>,
    /// The member the device acts as: the one an instance that shows nothing is run for.
    person: OnceLock<String>,
    /// The person's formats, language and zone, as this device resolved them from their region:
    /// what an instance it starts writes its dates and clocks in, and the language its window
    /// reads. Updated as their region changes, and said again to every process running.
    locale: Mutex<runtime_portal::process_link::Locale>,
    /// The providers this device registered, whose packages every instance is handed.
    provided: OnceLock<tilo_workspace_service::workspaces::providers::Providers>,
    /// What each workspace trusts for packages, which every instance's app is verified against.
    trust: OnceLock<Arc<dyn super::LaunchGrantsSource>>,
    /// What makes each app's own tilolang process its daemon line, once the daemon serves them.
    own_lines: OnceLock<crate::device::processes::OwnLines>,
    /// The web entry of the server this device keeps workspaces for, and the workspaces it
    /// keeps: what its apps mint a resource's link under, `None` where it keeps no server.
    serving: OnceLock<Option<Serving>>,
    made: Mutex<Vec<Made>>,
    next: AtomicU64,
}

/// The page a keeping server serves its workspaces at, and which workspaces it keeps.
struct Serving {
    web_entry: String,
    workspaces: std::collections::BTreeSet<String>,
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

/// The file chooser a hosted instance of `app` in `workspace` reaches on Linux: the desktop
/// session's own chooser where one serves, which asks the person at their window, and otherwise
/// a connection this daemon serves where what the person chose already opens again and nobody
/// is asked for anything new.
#[cfg(target_os = "linux")]
impl Instances {
    fn chooser_for(&self, app: &str, workspace: &str) -> Option<Descriptor> {
        // A session at this computer answers the person's pick: the app reaches its chooser,
        // named for the app and the workspace as the launcher names one.
        match launcher::services::resolve_chooser_bind(app, Some(workspace), false) {
            Ok(Some(bound)) => return Some(bound.host),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(app, %error, "the session's chooser is not reached");
            }
        }
        let serving = tilo_dialogs::chooser_service::Serving {
            person: None,
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
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct ChosenGrants {
    chosen: Arc<tilo_dialogs::chosen::ChosenTable>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
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
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn chosen_key(
    caller: &tilo_dialogs::chooser_service::Caller,
) -> Result<tilo_dialogs::chosen::AppKey, String> {
    tilo_dialogs::chosen::AppKey::new(&caller.workspace, &caller.app)
        .ok_or_else(|| tilo_dialogs::chooser_service::CANNOT_HAND.to_owned())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn kind_of(choice: tilo_dialogs::chooser_service::Choice) -> tilo_dialogs::chosen::Kind {
    use tilo_dialogs::chooser_service::Choice;
    match choice {
        Choice::Read => tilo_dialogs::chosen::Kind::Read,
        Choice::Edit => tilo_dialogs::chosen::Kind::Edit,
        Choice::Folder { write } => tilo_dialogs::chosen::Kind::Folder { write },
        Choice::Save => tilo_dialogs::chosen::Kind::Save,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn granted_of(chosen: tilo_dialogs::chosen::Chosen) -> tilo_dialogs::chooser_service::Granted {
    use tilo_dialogs::chooser_service::{Choice, Granted};
    let choice = match chosen.kind {
        tilo_dialogs::chosen::Kind::Read => Choice::Read,
        tilo_dialogs::chosen::Kind::Edit => Choice::Edit,
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
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct ChosenRecords {
    records: Arc<crate::device::records::DeviceRecords>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl tilo_dialogs::chosen::Store for ChosenRecords {
    fn load(&self) -> Vec<tilo_dialogs::chosen::Kept> {
        chosen_in(&self.records)
    }

    fn keep(&self, kept: Vec<tilo_dialogs::chosen::Kept>) {
        let value =
            serde_json::to_value(kept).unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
        if let Err(error) = self.records.set(tilo_dialogs::chosen::RECORD, value) {
            tracing::warn!(%error, "the files people chose are not written");
        }
    }

    /// The record is followed as it changes, whoever writes it: the files a session's chooser
    /// keeps reach this daemon's instances as they are chosen, not only at the next start.
    fn follow(&self, changed: Arc<dyn Fn(Vec<tilo_dialogs::chosen::Kept>) + Send + Sync>) -> bool {
        let records = Arc::clone(&self.records);
        tilo_rt::app().handle().spawn(async move {
            let mut moved = records.changed();
            while moved.changed().await.is_ok() {
                changed(chosen_in(&records));
            }
        });
        true
    }
}

/// The device's chosen files as the chooser's table keeps them, over the record a session's
/// chooser writes: what an embedder's test grants a pick through.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn chosen_store(
    records: Arc<crate::device::records::DeviceRecords>,
) -> Arc<dyn tilo_dialogs::chosen::Store> {
    Arc::new(ChosenRecords { records })
}

/// The choices `records` keeps, as the table reads them.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn chosen_in(records: &crate::device::records::DeviceRecords) -> Vec<tilo_dialogs::chosen::Kept> {
    match records.get(tilo_dialogs::chosen::RECORD) {
        Some(serde_json::Value::Array(entries)) => entries
            .into_iter()
            .filter_map(|entry| serde_json::from_value(entry).ok())
            .collect(),
        _ => Vec::new(),
    }
}

/// A daemon opens no links in an app of its own.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct NoOpen;

#[cfg(any(target_os = "linux", target_os = "macos"))]
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
            #[cfg(unix)]
            blobs: sockets.blobs.clone(),
            launcher,
            launched: started,
            processes: OnceLock::new(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            chosen: Arc::new(tilo_dialogs::chosen::ChosenTable::new()),
            person: OnceLock::new(),
            locale: Mutex::default(),
            provided: OnceLock::new(),
            trust: OnceLock::new(),
            own_lines: OnceLock::new(),
            serving: OnceLock::new(),
            made: Mutex::default(),
            next: AtomicU64::new(1),
        }
    }

    /// The page this device's keeping server serves its workspace at, and the workspaces it
    /// keeps: from now on the instances of those workspaces mint their resources' links under
    /// it, and a device that keeps no server hands none.
    pub(crate) fn keep_serving(&self, web_entry: String, workspaces: Vec<String>) {
        let _ = self.serving.set(Some(Serving {
            web_entry,
            workspaces: workspaces.into_iter().collect(),
        }));
    }

    /// Where `workspace`'s resources open on the web, as a tilolang instance receives it
    /// (`runtime_portal::process_link::InstanceRequest::link_home`): the entry of the server
    /// that keeps it, and `None` for a workspace this device keeps for no server.
    fn link_home(&self, workspace: &str) -> Option<String> {
        let serving = self.serving.get()?.as_ref()?;
        if !serving.workspaces.contains(workspace) {
            return None;
        }
        let entry = tilo_reference::link::WebEntry::new(&serving.web_entry).ok()?;
        let home = tilo_reference::link::share::LinkHome {
            origin: entry.origin().as_str().to_owned(),
            workspace: workspace.to_owned(),
            host_device: String::new(),
            web_entry: entry.base().to_owned(),
        };
        match home.json() {
            Ok(home) => Some(home),
            Err(error) => {
                // Every field is text, so this cannot fail short of a bug: the workspace's
                // links are refused, and the log says why.
                tracing::warn!(%error, "a workspace's page does not write as JSON");
                None
            }
        }
    }

    /// The member the device acts as, from now on.
    pub(crate) fn keep_person(&self, person: String) {
        let _ = self.person.set(person);
    }

    /// The person's formats, language and zone, as this device resolved them, from now on: what
    /// every instance started here is handed. The processes already running are told separately
    /// ([`Self::locale_changed`]).
    pub(crate) fn keep_locale(&self, locale: runtime_portal::process_link::Locale) {
        *self.locale.lock_recover() = locale;
    }

    /// The person's formats, language or zone changed: every process this daemon runs is told, so
    /// the instances it runs write their dates and clocks again and read their words in the
    /// language, where they stand.
    pub(crate) fn locale_changed(&self, locale: &runtime_portal::process_link::Locale) {
        self.keep_locale(locale.clone());
        if let Some(processes) = self.processes.get() {
            processes.locale_changed(locale);
        }
    }

    /// Keep the files people choose for hosted apps in the device's records from now on, the same
    /// place the desktop session keeps its choices.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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

    /// Verify the app of every instance the processes start against what its workspace trusts,
    /// as `trust` reads it, from now on.
    pub(crate) fn read_trust_from(&self, trust: Arc<dyn super::LaunchGrantsSource>) {
        if let Some(processes) = self.processes.get() {
            processes.read_trust_from(Arc::clone(&trust));
        }
        let _ = self.trust.set(trust);
    }

    /// Hand each app's own tilolang process started from now on its daemon line, made by `lines`.
    #[cfg(unix)]
    pub(crate) fn serve_own_lines(&self, lines: crate::device::processes::OwnLines) {
        let _ = self.own_lines.set(Arc::clone(&lines));
        if let Some(processes) = self.processes.get() {
            processes.serve_own_lines(lines);
        }
    }

    /// Whether this device ever started a tilolang process, for a test that must see that
    /// nothing of an app ran: asking for the processes here never starts one.
    #[cfg(test)]
    pub(crate) fn started_a_process(&self) -> bool {
        self.processes.get().is_some()
    }

    /// The tilolang processes, through the launcher found first.
    pub(crate) fn processes(&self) -> Result<&Arc<Processes>, String> {
        if let Some(processes) = self.processes.get() {
            return Ok(processes);
        }
        (self.launcher)().map(|launcher| {
            self.processes.get_or_init(|| {
                let processes = Processes::new(launcher).attesting(Arc::clone(&self.launched));
                if let Some(providers) = self.provided.get() {
                    processes.read_provided_from(providers.clone());
                }
                if let Some(trust) = self.trust.get() {
                    processes.read_trust_from(Arc::clone(trust));
                }
                if let Some(lines) = self.own_lines.get() {
                    processes.serve_own_lines(Arc::clone(lines));
                }
                processes
            })
        })
    }

    /// The member the device acts as.
    fn person(&self) -> Result<String, String> {
        self.person
            .get()
            .cloned()
            .ok_or_else(|| "this device runs no app's code before it is someone's".to_owned())
    }

    /// Start an instance of `app` in `workspace` that shows nothing, to take the calls the
    /// dispatch delivers to it: the start fails, saying why, when it or its process
    /// ends before a call reached it.
    pub(crate) fn start_with_no_view(
        &self,
        app: &str,
        workspace: &str,
        origin: Option<Descriptor>,
        caller: Option<&Caller>,
    ) -> Result<WindowlessStart, String> {
        let (ends, start) = tokio::sync::oneshot::channel();
        self.start(
            (app, workspace, origin),
            self.person()?,
            Shows::Nothing(caller.map(|caller| Box::new(caller.clone()))),
            None,
            (None, None, Some(ends)),
        )?;
        Ok(start)
    }

    /// How many copies of `app` in `workspace` that show nothing ([`Self::start_with_no_view`])
    /// their process runs now: one that ended is forgotten.
    pub(crate) fn no_view_running(&self, app: &str, workspace: &str) -> usize {
        let Some(processes) = self.processes.get() else {
            return 0;
        };
        let starts = processes.starts.lock_recover();
        starts
            .values()
            .filter(|(of, at, _)| of == app && at == workspace)
            .count()
    }

    /// Start the instance of `app`'s window in `workspace` here, with no process drawing it: the
    /// person's layout holds the window, and the session that shows their layout draws it. What
    /// starts is the app's code, on the app's own connection, exactly as a window's instance is.
    #[cfg(target_os = "linux")]
    pub(crate) fn start_window(
        &self,
        app: &str,
        workspace: &str,
        origin: Option<Descriptor>,
    ) -> Result<(), String> {
        self.start(
            (app, workspace, origin),
            self.person()?,
            Shows::Window,
            None,
            (None, None, None),
        )
        .map(drop)
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
        (caller, certificate): (&Caller, Option<&tilo_user_identity::DeviceCert>),
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
            Shows::View(Box::new((caller.clone(), certificate.cloned()))),
            Some(patch),
            (record, reveal, None),
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
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
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
            (None, None, None),
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
    /// none runs: shown over `patch` when it has one, and telling `ends` why should it end
    /// first. The one it started.
    fn start(
        &self,
        (app, workspace, origin): (&str, &str, Option<Descriptor>),
        person: String,
        shows: Shows,
        patch: Option<Descriptor>,
        (document, reveal, ends): (Option<String>, Option<String>, Option<Ends>),
    ) -> Result<u64, String> {
        if !crate::device::package_runs::runs_in_process(app) {
            return Err(format!("{app} is no tilolang app installed here"));
        }
        let processes = Arc::clone(self.processes()?);
        let instance = self.next.fetch_add(1, Ordering::Relaxed);
        let collaboration = self.connect(app, workspace, instance, shows)?;
        let identity = self.identity(app, workspace)?;
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let chooser = self.chooser_for(app, workspace);
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let chooser = None;
        if let Some(ends) = ends {
            let start = (app.to_owned(), workspace.to_owned(), ends);
            processes.starts.lock_recover().insert(instance, start);
        }
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
                content: self.content(app, workspace),
                chooser,
                clipboard: None,
                tools: None,
                document,
                reveal,
                open: None,
                locale: self.locale.lock_recover().clone(),
                link_home: self.link_home(workspace),
            })
            .map(|_| instance)
            .map_err(|error| {
                processes.starts.lock_recover().remove(&instance);
                format!("{app}'s instance did not start: {error}")
            })
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

    /// The instance's content connection: the device's content store for `app` in `workspace`,
    /// as a window's instance is handed one, so a file it makes is kept in the workspace whoever
    /// an action answered it to reads it from; none where the daemon serves no document media.
    pub(crate) fn content(&self, app: &str, workspace: &str) -> Option<Descriptor> {
        #[cfg(unix)]
        {
            crate::host::connect_blobs(&self.blobs, app, workspace).map(descriptor_of)
        }
        #[cfg(not(unix))]
        {
            let _ = (app, workspace);
            None
        }
    }

    /// [`Self::content`] for a capture's instance, refused where the device serves none.
    #[cfg(target_os = "linux")]
    pub(crate) fn capture_content(&self, app: &str, workspace: &str) -> Result<Descriptor, String> {
        self.content(app, workspace)
            .ok_or_else(|| format!("{app} has no media here"))
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
        let (windowless, opened_for, opened_with) = match made.shows {
            Shows::Nothing(caller) => (true, caller.map(|caller| *caller), None),
            Shows::Window => (false, None, None),
            Shows::View(view) => {
                let (caller, certificate) = *view;
                (false, Some(caller), certificate)
            }
        };
        Some(AuthenticatedApplicationLaunch {
            continuation_instance: Some(instance),
            launch_ticket: None,
            windowless,
            opened_for,
            opened_with,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace this device keeps for a server is handed the page that server serves it at,
    /// and a workspace it keeps for no server, or a device that keeps none, is handed none.
    #[test]
    fn an_instance_is_handed_its_servers_page_for_the_workspaces_it_keeps() {
        let profile = tempfile::tempdir().unwrap();
        let sockets = crate::daemon::Sockets::in_dir(profile.path());
        let instances = Instances::new(
            &sockets,
            Box::new(|| Err("no launcher".into())),
            Arc::default(),
        );
        let served = "5ba25f73-87c5-447f-a5a6-880dc4249a81";
        let other = "4b0d9a52-7b4e-4e0d-9e0e-2b1c2a0c9a11";
        assert!(instances.link_home(served).is_none(), "no server, no page");
        instances.keep_serving("https://entry.example/".into(), vec![served.to_owned()]);
        let home: tilo_reference::link::share::LinkHome = serde_json::from_str(
            &instances
                .link_home(served)
                .expect("the served workspace has a page"),
        )
        .unwrap();
        assert_eq!(home.web_entry, "https://entry.example/");
        assert_eq!(home.origin, "https://entry.example");
        assert_eq!(home.workspace, served);
        assert!(
            instances.link_home(other).is_none(),
            "a workspace the server does not keep has no page"
        );
    }

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
