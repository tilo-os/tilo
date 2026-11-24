//! Tilolang processes: one process per workspace and app, which runs that app's code in that
//! workspace, each view's call in it (`docs/designs/app-language/app-window.md`).
//!
//! The device's daemon runs them, as it runs the rest of its work: one [`Processes`] per daemon,
//! started with it whether or not anyone is signed in at a desktop. A desktop session is a client
//! that hands each window's instance over on the shell link the daemon started it with
//! (`runtime_portal::device_processes`, [`Processes::for_the_shell`]), and the daemon starts the
//! process of a call that shows nothing the same way ([`Processes::hand_over_now`]).
//!
//! A tilolang app's window is a launched process of its own, with its own launch identity for its
//! surface and accessibility, started as its window alone (no `--tilolang-process`: the launcher gives
//! it no collaboration connection). The session hands each window's instance to the process of its
//! workspace and app, and the daemon takes it from the person's shell alone, for a workspace the
//! device has for them:
//!
//! - the process's end of the window's patch connection, whose other end the window took on its
//!   link (`runtime_portal::process_link::ToProcess::Patch`);
//! - the instance's collaboration connection, attested as that window of the app's launch, exactly
//!   as a later window's own connection is: the broker attributes it from the session's record,
//!   never from the process;
//! - its identity-service connection, opened and named for the app and workspace;
//! - the connection to the app's origin connector, when its types hold an outside service's
//!   resources;
//! - its module, read from the installation verified as a launch verifies it, in a sealed memfd.
//!
//! The person's region formats ride with the instance, and are said again on every process's socket
//! when they change them in Settings ([`Processes::locale_changed`]): their windows' dates, clocks
//! and weeks follow where they stand, without waiting for a restart. So does what the person may
//! do in the app, said again on the sockets of the workspace's processes when its policy changes
//! ([`Processes::policy_changed`]): a viewer's window offers no change they cannot make.
//!
//! A process is started through the platform's launcher ([`spawn_host`]; on Linux the launch kind
//! "tilolang process", `tilo-launcher tilolang-process`; on macOS the tilolang runtime's binary in a
//! Seatbelt sandbox), keyed by its workspace, its app and the runtimes it runs on, with the storage
//! of that workspace and app. It stays once the last of its calls ended, which it says on its
//! socket, and ends when nothing called it for [`IDLE`]: a person who closes an app and opens it
//! again finds it running.
//!
//! In a development session (`TILO_LAUNCHER_LIVE_MODULES`), a reinstall of an app a process runs is
//! verified as a launch verifies it and handed to the process on its socket ([`Processes::reinstalled`]):
//! its open windows take the new build over without restarting, as `tilolang dev` expects.
//!
//! Every call opens what it works on from the workspace's committed read, which is the one store:
//! a process keeps no copy of its own on disk.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use runtime_portal::device_processes::{
    FromDevice, HandedFds, HandedInstance, MediaChange, Running, ToDevice,
};
use runtime_portal::process_link::{
    AppAccess, AppBuild, Descriptor, FromProcess, InstanceFds, InstanceRequest, MediaCommand,
    MediaView, OpenRequest, Packet as _, TaskFds, TaskRequest, ToProcess,
};
use tilo_lock::MutexRecover as _;

use super::package_runs::{Verified, installed_apps, runs_in_process, verified_installed};

/// Hand the process behind `socket` the instance `request` with its connections, keeping them
/// to hand to another process should this one have ended.
fn send_instance(
    socket: &Descriptor,
    request: &InstanceRequest,
    fds: &InstanceFds,
) -> std::io::Result<()> {
    ToProcess::Instance {
        request: Box::new(request.clone()),
        fds: fds.try_clone()?,
    }
    .send(socket)
}

/// The app that keeps each person's own views (`apps/tl/views`): views they, their agents or
/// others wrote, which read whatever the workspace has installed.
const PERSON_VIEWS_APP: &str = "so.tilo.lang.views";

/// How long a process runs on with no call before it ends: long enough that an app closed and
/// opened again finds its process running, short enough that the apps of an afternoon don't hold a
/// small device's memory. It is the one period every app process the daemon runs keeps
/// ([`crate::IDLE`]).
pub use crate::IDLE;

/// How many of its last error lines a process that ended without saying why is reported by.
const SAID_LINES: usize = 8;

/// Where the hosts of a workspace's agent tasks keep what they compile, beside its apps': no app
/// id starts with a dot.
const TASK_STORAGE: &str = ".tasks";

/// What the windows a workspace's views show on web pages are granted to write: nothing an app
/// or a task keeps.
const PAGE_STORAGE: &str = ".pages";

/// A process that runs no app's instances: an agent task's host, or the window of a view shown on
/// a web page.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Alone {
    Task(String),
    /// The page window's start, numbered apart from every other.
    Page(u64),
}

/// What identifies one process: one workspace and one app, on the runtimes its package names; or
/// one process of its own ([`Alone`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct HostKey {
    workspace: String,
    app: String,
    /// What the process runs instead of instances, for one that takes none.
    alone: Option<Alone>,
    /// The tilolang runtime and the Tilo runtime it runs on: an instance runs only in a process of
    /// the runtimes its package names.
    runtime: String,
    app_runtime: String,
}

/// A running process.
struct Host {
    /// The daemon's end of its socket.
    socket: Arc<Descriptor>,
    /// The windows whose instances it runs, each with its app.
    instances: HashMap<u64, String>,
    /// Which start of the process this is, so the end of an earlier one never retires a later one.
    generation: u64,
    /// The build of each app it runs, the one it was handed last.
    builds: HashMap<String, Build>,
    /// Since when it runs no call: it ends [`IDLE`] after.
    idle_since: Option<Instant>,
}

/// The build of an app a process runs: its module's hash.
struct Build {
    module: u64,
}

/// One instance, as the desktop hands it over: the app and workspace it runs in, the window it
/// draws (or none, for an action that shows nothing), who is at it, and the connections the
/// desktop made for it.
pub struct Instance {
    pub app: String,
    pub workspace: String,
    /// The window's instance: the tab it takes.
    pub instance: u64,
    /// The member at the window, whose steps its undo takes back.
    pub person: String,
    /// The process's end of the window's patch connection: `None` for an instance that shows
    /// nothing.
    pub patch: Option<Descriptor>,
    /// The instance's collaboration connection, attested as that window, or as an instance that
    /// shows nothing.
    pub collaboration: Descriptor,
    /// Its identity-service connection, named for the app and workspace.
    pub identity: Descriptor,
    /// Its connection to the app's origin connector, when the app has one.
    pub origin: Option<Descriptor>,
    /// Its connection to the device's content store, scoped to the app in its workspace: the
    /// files a person chooses, the pictures its document holds.
    pub content: Option<Descriptor>,
    /// Its connection to the person's chooser, when this session serves one.
    pub chooser: Option<Descriptor>,
    /// Its connection to the person's clipboard, when this session serves one.
    pub clipboard: Option<Descriptor>,
    /// Its connection to this session's tools service, whose far end answers for this app: what
    /// `File.open` asks which app opens a file and starts it.
    pub tools: Option<Descriptor>,
    /// The record the window was asked to show, by its id.
    pub document: Option<String>,
    /// The item the window opens on, by its anchor.
    pub reveal: Option<String>,
    /// What the window opens: a file, as content the instance reads from the content store, or a
    /// link it hands its package's `open-link`.
    pub open: Option<OpenRequest>,
    /// What the person may do in the app, projected from the workspace's policy.
    pub access: Option<tilo_action_policy::MemberAccess>,
    /// The person's formats and language, as the session resolved them: what the instance writes
    /// its dates and clocks in, and the language its window reads every word in.
    pub locale: runtime_portal::process_link::Locale,
    /// Where the workspace's resources open on the web, as JSON, when the session knows it: what
    /// the instance's host mints a resource's `share-link` under
    /// ([`runtime_portal::process_link::InstanceRequest::link_home`]).
    pub link_home: Option<String>,
}

impl Instance {
    /// The instance a session handed over, with the connections it opened for it.
    fn handed(instance: HandedInstance, fds: HandedFds, origin: Option<Descriptor>) -> Self {
        Self {
            app: instance.app,
            workspace: instance.workspace,
            instance: instance.instance,
            person: instance.person,
            patch: fds.patch,
            collaboration: fds.collaboration,
            identity: fds.identity,
            origin,
            content: fds.content,
            chooser: fds.chooser,
            clipboard: fds.clipboard,
            tools: fds.tools,
            document: instance.document,
            reveal: instance.reveal,
            open: instance.open,
            access: instance.access,
            locale: instance.locale,
            link_home: instance.link_home,
        }
    }
}

/// Whether the device has the person's workspace of this id: the only workspaces the shell's
/// hand-overs run in.
pub(crate) type Has = dyn Fn(&str) -> bool + Send + Sync;

/// How an installed app is verified for a process: the workspace it is installed in, and the app.
type Verifies = dyn Fn(&str, &str) -> Result<Verified, String> + Send + Sync;

/// What starts a host here ([`spawn_host`]): `tilo-launcher`, which builds its sandbox, on Linux.
#[cfg(target_os = "linux")]
pub type Launcher = PathBuf;

/// What starts a host here ([`spawn_host`]): the installed packages, whose tilolang runtime the
/// host runs from in its sandbox (Seatbelt on macOS, an `AppContainer` on Windows).
#[cfg(any(target_os = "macos", windows))]
pub type Launcher = package_store::Catalog;

/// Nothing, on Android: the daemon starts no host there ([`spawn_host`]), since an app runs in
/// the Tilo app's own process.
#[cfg(target_os = "android")]
pub type Launcher = ();

/// Who plays what: what the system's media controls are told, and which process and instance
/// plays each Playback, so a control the person presses reaches it.
#[derive(Default)]
struct Playing {
    told: Option<Arc<dyn Fn(MediaChange) + Send + Sync>>,
    /// The process and instance playing each Playback, by its id.
    where_played: HashMap<String, (HostKey, u64)>,
}

/// How the shell's hand-overs reach the device's origin connectors, which the daemon runs
/// ([`crate::apps::Apps`]).
pub(crate) trait Origins: Send + Sync {
    /// The connection an instance of `app` in `workspace` reaches its origin's connector over,
    /// when the app has one.
    ///
    /// # Errors
    /// Its connector cannot be started.
    fn connection(&self, app: &str, workspace: &str) -> Result<Option<Descriptor>, String>;

    /// Attach `connection` to `app`'s connector of `origin` in `workspace`.
    ///
    /// # Errors
    /// The app reaches no such origin, or its connector cannot be started.
    fn attach(
        &self,
        app: &str,
        origin: &str,
        workspace: &str,
        connection: &Descriptor,
    ) -> Result<(), String>;
}

/// What records that an app was started from the cache: its id.
type Opened = dyn Fn(&str) + Send + Sync;

/// Where a workspace's content as this device's person reads it comes from: its owners' limits
/// on what members post there and the content a limit refused, as the Workspace package's root
/// carries them (`tilolang_tilo::content::WorkspaceContent` JSON). The daemon hands it to each
/// window's process, which serves it as `workspace.content`.
pub(crate) trait ContentOf: Send + Sync {
    /// The reading of `workspace` for this device's person, now.
    fn content(&self, workspace: &str) -> Option<String>;
}

/// The tilolang processes this device runs, by workspace, app and runtimes.
pub struct Processes {
    /// Where processes keep what their instances keep, and where windows kept it before.
    data_home: PathBuf,
    /// The platform's launcher, which starts each process in its sandbox ([`spawn_host`]).
    launcher: Launcher,
    verifies: Box<Verifies>,
    /// Records each app started, through the device's package cache ([`super::packages::opened`]).
    opened: Box<Opened>,
    running: Mutex<HashMap<HostKey, Host>>,
    generations: AtomicU64,
    /// Whether the app store is watched for reinstalls.
    follows_reinstalls: AtomicBool,
    /// What each app here plays, as the system's media controls hear it.
    playing: Mutex<Playing>,
    /// The providers this device registered, whose packages each instance is handed
    /// ([`InstanceRequest::provided`]).
    provided: OnceLock<tilo_workspace_service::workspaces::providers::Providers>,
    /// Where each workspace's content is read, once the daemon keeps its workspaces here
    /// ([`Self::read_content_from`]): what each window's process serves as `workspace.content`.
    content_of: OnceLock<Arc<dyn ContentOf>>,
    /// The content last handed to each workspace's processes, so a reading that did not move is
    /// said no second time.
    contents: Mutex<HashMap<String, String>>,
    /// The daemon's launch records, once it keeps them: each process this device starts is
    /// written there, so a connection from it is admitted as the app it runs.
    attests: OnceLock<Arc<crate::apps::Launched>>,
}

impl Processes {
    /// The processes this device starts through `launcher`, keeping what they keep under the
    /// person's data directory: the XDG data home on Linux, Application Support on a Mac.
    #[must_use]
    pub fn new(launcher: Launcher) -> Arc<Self> {
        let data_home = tilo_paths::data_dir().unwrap_or_else(|| PathBuf::from("/nonexistent"));
        Self::with(data_home, launcher, Box::new(verified_installed))
    }

    /// Processes over `data_home`, started through `launcher` and running what `verifies` verified.
    #[must_use]
    pub fn with(data_home: PathBuf, launcher: Launcher, verifies: Box<Verifies>) -> Arc<Self> {
        Self::recording(
            data_home,
            launcher,
            verifies,
            Box::new(super::packages::opened),
        )
    }

    /// [`Self::with`], recording each app it starts with `opened`.
    fn recording(
        data_home: PathBuf,
        launcher: Launcher,
        verifies: Box<Verifies>,
        opened: Box<Opened>,
    ) -> Arc<Self> {
        Arc::new(Self {
            data_home,
            launcher,
            verifies,
            opened,
            running: Mutex::new(HashMap::new()),
            generations: AtomicU64::new(1),
            follows_reinstalls: AtomicBool::new(false),
            playing: Mutex::new(Playing::default()),
            provided: OnceLock::new(),
            content_of: OnceLock::new(),
            contents: Mutex::new(HashMap::new()),
            attests: OnceLock::new(),
        })
    }

    /// Record each process this device starts in `attests` from now on: the daemon's launch
    /// records, from which a connection is admitted as the app the process runs.
    pub(crate) fn attesting(self: &Arc<Self>, attests: Arc<crate::apps::Launched>) -> Arc<Self> {
        let _ = self.attests.set(attests);
        Arc::clone(self)
    }

    /// Tell `told` what each app here plays, as its Playback moves, and stop telling the one
    /// before. The system's media controls show every Playback and press the one a key reaches.
    pub fn on_media(&self, told: Arc<dyn Fn(MediaChange) + Send + Sync>) {
        self.playing.lock_recover().told = Some(told);
    }

    /// Run `command` as the Playback `id`'s own action, on the instance that plays it. Whether a
    /// process here plays it.
    pub fn media_command(&self, id: &str, command: MediaCommand) -> bool {
        let found = self
            .playing
            .lock_recover()
            .where_played
            .get(id)
            .map(|(key, instance)| (key.clone(), *instance));
        let Some((key, instance)) = found else {
            return false;
        };
        let socket = self
            .running()
            .get(&key)
            .map(|process| Arc::clone(&process.socket));
        let Some(socket) = socket else {
            return false;
        };
        if let Err(error) = (ToProcess::MediaCommand { instance, command }).send(&socket) {
            tracing::warn!(%error, workspace = %key.workspace, app = %key.app, "a media control did not reach its app");
            return false;
        }
        true
    }

    /// Show `anchor` in the window `instance` of `app` in `workspace`, in the process that runs
    /// it: whether one does.
    pub fn reveal(&self, workspace: &str, app: &str, instance: u64, anchor: String) -> bool {
        let socket = self
            .running()
            .iter()
            .find(|(key, process)| {
                key.workspace == workspace
                    && key.app == app
                    && process.instances.contains_key(&instance)
            })
            .map(|(_, process)| Arc::clone(&process.socket));
        let Some(socket) = socket else {
            return false;
        };
        if let Err(error) = (ToProcess::Reveal { instance, anchor }).send(&socket) {
            tracing::warn!(%error, workspace, app, "a window was not told what to show");
            return false;
        }
        true
    }

    /// What the instance `instance` of `key` plays now, told to whoever serves the media
    /// controls: its Playback's view, or the id of the one it stopped playing.
    fn media(&self, key: &HostKey, instance: u64, view: Option<MediaView>) {
        let change = {
            let mut playing = self.playing.lock_recover();
            match &view {
                Some(view) => {
                    playing
                        .where_played
                        .insert(view.id.clone(), (key.clone(), instance));
                    MediaChange::Playing(view.clone())
                }
                None => {
                    let stopped = playing
                        .where_played
                        .iter()
                        .find(|(_, (held, held_instance))| {
                            held.workspace == key.workspace
                                && held.app == key.app
                                && *held_instance == instance
                        })
                        .map(|(id, _)| id.clone());
                    let Some(stopped) = stopped else {
                        return;
                    };
                    playing.where_played.remove(&stopped);
                    MediaChange::Stopped(stopped)
                }
            }
        };
        let told = self.playing.lock_recover().told.clone();
        if let Some(told) = told {
            told(change);
        }
    }

    /// Hand each instance started from now on what `providers` publish.
    pub fn read_provided_from(
        &self,
        providers: tilo_workspace_service::workspaces::providers::Providers,
    ) {
        let _ = self.provided.set(providers);
    }

    /// Read each workspace's content from `content` from now on: what each window's process
    /// serves as `workspace.content` when it starts, and what they are told again whenever
    /// [`Self::content_changed`] says the workspace's content moved.
    pub(crate) fn read_content_from(&self, content: Arc<dyn ContentOf>) {
        let _ = self.content_of.set(content);
    }

    /// The workspace's content as this device's person reads it now, as an instance is handed it:
    /// empty where no service reads one here.
    fn content(&self, workspace: &str) -> String {
        let content = self
            .content_of
            .get()
            .and_then(|content| content.content(workspace))
            .unwrap_or_default();
        self.contents
            .lock_recover()
            .insert(workspace.to_owned(), content.clone());
        content
    }

    /// The content of `workspace` moved — its owners set another limit, or a host refused content
    /// or kept it once a limit was raised: say it on every process running a window of it, so the
    /// views that read `workspace.content` show the refusal or its going. A reading that did not
    /// move is said no second time.
    pub(crate) fn content_changed(&self, workspace: &str) {
        let Some(content) = self.content_of.get() else {
            return;
        };
        let reading = content.content(workspace).unwrap_or_default();
        if self
            .contents
            .lock_recover()
            .get(workspace)
            .is_some_and(|said| said == &reading)
        {
            return;
        }
        self.contents
            .lock_recover()
            .insert(workspace.to_owned(), reading.clone());
        for (key, process) in self.running().iter() {
            if key.workspace != workspace || key.alone.is_some() {
                continue;
            }
            if let Err(error) = ToProcess::Content(runtime_portal::process_link::WorkspaceContent {
                workspace: workspace.to_owned(),
                content: reading.clone(),
            })
            .send(&process.socket)
            {
                tracing::warn!(
                    %error,
                    workspace,
                    app = %key.app,
                    "a tilolang process was not told the workspace's content changed"
                );
            }
        }
    }

    /// Read every workspace's content again: what a daemon told of writes it missed does, so no
    /// process keeps a reading that moved without it hearing.
    pub(crate) fn content_refresh_all(&self) {
        let workspaces: Vec<String> = {
            let running = self.running();
            let mut workspaces: Vec<String> =
                running.keys().map(|key| key.workspace.clone()).collect();
            workspaces.sort();
            workspaces.dedup();
            workspaces
        };
        for workspace in workspaces {
            self.content_changed(&workspace);
        }
    }

    /// What the registered providers publish, as an instance is handed it: a JSON list, or
    /// nothing where none publishes a package.
    fn provided(&self) -> std::io::Result<String> {
        let packages = self
            .provided
            .get()
            .map(tilo_workspace_service::workspaces::providers::Providers::packages)
            .unwrap_or_default();
        if packages.is_empty() {
            return Ok(String::new());
        }
        serde_json::to_string(&packages).map_err(std::io::Error::other)
    }

    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<HostKey, Host>> {
        self.running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The processes running now, each by its workspace and app, with the calls it runs.
    #[must_use]
    pub fn instances(&self) -> Vec<Running> {
        let mut processes: Vec<_> = self
            .running()
            .iter()
            .filter(|(key, _)| key.alone.is_none())
            .map(|(key, process)| {
                let mut instances: Vec<u64> = process.instances.keys().copied().collect();
                instances.sort_unstable();
                Running {
                    workspace: key.workspace.clone(),
                    app: key.app.clone(),
                    instances,
                }
            })
            .collect();
        processes.sort();
        processes
    }

    /// Say on every process running now that the person's formats or language changed: the
    /// instances write the times and days their windows show again in the new formats (their
    /// week's first day, their date and clock styles), and their windows read their words in the
    /// language, where they stand. A process started later is handed them with its first instance.
    pub fn locale_changed(&self, locale: &runtime_portal::process_link::Locale) {
        // A task's host takes nothing after its task.
        for (key, process) in self.running().iter().filter(|(key, _)| key.alone.is_none()) {
            if let Err(error) = ToProcess::Locale(locale.clone()).send(&process.socket) {
                // Its follow thread reads the end of the socket and forgets it: the next instance
                // starts another.
                tracing::warn!(
                    %error,
                    workspace = %key.workspace,
                    app = %key.app,
                    "a tilolang process was not told the person's formats or language changed"
                );
            }
        }
    }

    /// Say on every process running an app of `apps` in `workspace` what the person may now do in
    /// it, from the workspace's policy as the caller holds it now: a role given or taken back
    /// enables or disables what the windows already open offer. A process started later is handed
    /// it with each instance.
    pub fn policy_changed(&self, workspace: &str, apps: &[AppAccess]) {
        for (key, process) in self.running().iter() {
            if key.workspace != workspace || key.alone.is_some() {
                continue;
            }
            let Some(access) = apps.iter().find(|access| access.app == key.app) else {
                continue;
            };
            let app = key.app.clone();
            if let Err(error) = ToProcess::Access(access.clone()).send(&process.socket) {
                // Its follow thread reads the end of the socket and forgets it: the next
                // instance starts another.
                tracing::warn!(
                    %error,
                    workspace = %key.workspace,
                    app,
                    "a tilolang process was not told what the person may now do"
                );
            }
        }
    }

    /// What the daemon's dispatch hands each connection the person's shell opened to these
    /// processes: a packet socket, admitted by a one-use secret the shell asked for on its own
    /// link (`runtime_collab::wire::shell::ShellCall::Processes`), whose one ask is answered on a
    /// thread of its own, for a workspace `has` says the device has for the person. What reaches
    /// an app's origin connector, for a handed instance or a service of the shell, is `origins`.
    pub(crate) fn for_the_shell(
        self: &Arc<Self>,
        has: Box<Has>,
        origins: Arc<dyn Origins>,
    ) -> tilo_workspace_service::apps::shell::ShellProcesses {
        let (processes, has) = (Arc::clone(self), Arc::<Has>::from(has));
        Arc::new(move |connection: Descriptor| {
            let (processes, has, origins) = (
                Arc::clone(&processes),
                Arc::clone(&has),
                Arc::clone(&origins),
            );
            let answering = std::thread::Builder::new()
                .name("tilolang-handover".into())
                .spawn(move || processes.answer(&connection, &*has, &*origins));
            if let Err(error) = answering {
                tracing::error!(%error, "a hand-over to the processes was not answered");
            }
        })
    }

    /// Answer the one ask on `connection`, which the shell opened.
    fn answer(self: &Arc<Self>, connection: &Descriptor, has: &Has, origins: &dyn Origins) {
        let asked = match ToDevice::receive(connection) {
            Ok(Some(asked)) => asked,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "a hand-over to the processes does not read");
                return;
            }
        };
        let answer = match asked {
            ToDevice::Instance { instance, fds } => {
                FromDevice::Handed(self.take_handed(*instance, fds, has, origins))
            }
            ToDevice::Locale(locale) => {
                self.locale_changed(&locale);
                FromDevice::Told
            }
            ToDevice::Access { workspace, apps } => {
                if has(&workspace) {
                    self.policy_changed(&workspace, &apps);
                }
                FromDevice::Told
            }
            ToDevice::Running => FromDevice::Running(self.instances()),
            ToDevice::Task { request, fds } => FromDevice::Started(if has(&request.workspace) {
                super::package_runs::task_runtimes().and_then(|runs_on| {
                    self.start_task(*request, fds, runs_on)
                        .map_err(|error| error.to_string())
                })
            } else {
                Err(format!(
                    "this device has no workspace {} of yours",
                    request.workspace
                ))
            }),
            ToDevice::Origin {
                app,
                origin,
                workspace,
                connection: theirs,
            } => FromDevice::Attached(if has(&workspace) {
                origins.attach(&app, &origin, &workspace, &theirs)
            } else {
                Err(format!("this device has no workspace {workspace} of yours"))
            }),
            ToDevice::Media { id, command } => {
                FromDevice::Pressed(self.media_command(&id, command))
            }
            ToDevice::Reveal {
                workspace,
                app,
                instance,
                anchor,
            } => FromDevice::Pressed(
                has(&workspace) && self.reveal(&workspace, &app, instance, anchor),
            ),
            ToDevice::FollowMedia => {
                if let Err(error) = FromDevice::Following.send(connection) {
                    tracing::warn!(%error, "the desktop's media controls follow nothing");
                    return;
                }
                // The connection stays open while it is told: the next to follow replaces it.
                let following = match connection.try_clone() {
                    Ok(following) => following,
                    Err(error) => {
                        tracing::warn!(%error, "the desktop's media controls follow nothing");
                        return;
                    }
                };
                self.on_media(Arc::new(move |change| {
                    if let Err(error) = FromDevice::Media(change).send(&following) {
                        tracing::debug!(%error, "the desktop's media controls stopped following");
                    }
                }));
                return;
            }
        };
        if let Err(error) = answer.send(connection) {
            tracing::warn!(%error, "a hand-over to the processes was not answered");
        }
    }

    /// Run the instance the shell handed over, in a workspace the device has for the person:
    /// whether its process started for it, or why it runs nowhere.
    fn take_handed(
        self: &Arc<Self>,
        instance: HandedInstance,
        fds: HandedFds,
        has: &Has,
        origins: &dyn Origins,
    ) -> Result<bool, String> {
        if !has(&instance.workspace) {
            return Err(format!(
                "this device has no workspace {} of yours",
                instance.workspace
            ));
        }
        let (app, window) = (instance.app.clone(), instance.instance);
        // The instance reaches its app's origin connector, which the daemon runs, as the daemon's
        // own instances do.
        let origin = origins.connection(&app, &instance.workspace)?;
        self.hand_over_now(Instance::handed(instance, fds, origin))
            .map_err(|error| {
                tracing::error!(
                    %error,
                    app,
                    instance = window,
                    "a tilolang instance was not handed to its process"
                );
                error.to_string()
            })
    }

    /// Hand `instance` to the process of its workspace and app, starting the process when none
    /// runs: whether it started the process. It verifies the package and may start the process, so
    /// it runs off any thread that must not wait. An [`Instance`] with no patch shows nothing: the
    /// host runs the app's code for the call and draws nothing, till its caller ends it.
    ///
    /// # Errors
    /// The app does not verify, its module cannot be sealed, or no process takes the instance.
    pub fn hand_over_now(self: &Arc<Self>, instance: Instance) -> std::io::Result<bool> {
        let refused =
            |what: String| std::io::Error::new(std::io::ErrorKind::PermissionDenied, what);
        let began = std::time::Instant::now();
        let verified = (self.verifies)(&instance.workspace, &instance.app).map_err(refused)?;
        (self.opened)(&instance.app);
        let key = HostKey {
            workspace: instance.workspace.clone(),
            app: instance.app.clone(),
            alone: None,
            runtime: verified.runtime,
            app_runtime: verified.app_runtime,
        };
        let module = self.module_for(&instance.app, verified.module, &key);
        // A package the app uses that is installed as an app is another app: its instance reads
        // that app's records from where it keeps them.
        let uses_packages = !module.imports.is_empty();
        let module = module.encode();
        let build = Build {
            module: hash(&module),
        };
        let module_len = module.len() as u64;
        let module = tilo_render_protocol::shm::sealed_blob("tilolang-module", &module)?;
        let request = InstanceRequest {
            app: instance.app.clone(),
            workspace: instance.workspace.clone(),
            instance: instance.instance,
            person: instance.person,
            module_len,
            // An instance with no patch connection draws nothing: it runs the app's actions for
            // whoever invokes them.
            shows: if instance.patch.is_some() {
                runtime_portal::process_link::Shows::View
            } else {
                runtime_portal::process_link::Shows::Nothing
            },
            formats: instance.locale.formats,
            // The person's language, as this session resolved it: their windows read every word in
            // it, so the process that draws them must not read its own process's language instead.
            language: instance.locale.language,
            document: instance.document,
            reveal: instance.reveal,
            open: instance.open,
            installed: if uses_packages {
                installed_apps()
            } else {
                std::collections::BTreeMap::new()
            },
            access: instance.access,
            content: self.content(&instance.workspace),
            link_home: instance.link_home.clone(),
            provided: self.provided()?,
        };
        let fds = InstanceFds {
            broker: instance.collaboration,
            identity: instance.identity,
            module,
            window: instance.patch,
            origin: instance.origin,
            content: instance.content,
            chooser: instance.chooser,
            clipboard: instance.clipboard,
            tools: instance.tools,
        };
        let started = self.send(&key, &request, &fds, build)?;
        tracing::info!(
            target: tilo_startup_metrics::LAUNCH_LATENCY_TARGET,
            package_id = %instance.app,
            instance = instance.instance,
            handed_us = began.elapsed().as_micros(),
            host_started = started,
            "tilolang instance handed over"
        );
        Ok(started)
    }

    /// The module the process `key` runs `app` from, its verified `module`: the person's views get
    /// every installed tilolang app its process can run (`key`'s runtimes), each verified as a launch
    /// verifies it, since a written view reads any type the workspace has.
    fn module_for(
        &self,
        app: &str,
        mut module: runtime_portal::process_link::Module,
        key: &HostKey,
    ) -> runtime_portal::process_link::Module {
        if app != PERSON_VIEWS_APP {
            return module;
        }
        let apps = match app_catalogue::apps::read_from_env() {
            Ok(apps) => apps,
            Err(error) => {
                tracing::error!(%error, "the person's views get no other app: the installed apps are not known");
                return module;
            }
        };
        for app in apps
            .iter()
            .filter(|app| app.app_id != PERSON_VIEWS_APP && runs_in_process(&app.app_id))
        {
            let Ok(verified) = (self.verifies)(&key.workspace, &app.app_id) else {
                tracing::warn!(package_id = %app.app_id, "the person's views leave out an app that could not be verified");
                continue;
            };
            if verified.runtime != key.runtime || verified.app_runtime != key.app_runtime {
                tracing::warn!(package_id = %app.app_id, "the person's views leave out an app on other runtimes");
                continue;
            }
            if !module.imports.contains(&verified.module.wasm) {
                module.imports.push(verified.module.wasm);
            }
        }
        module
    }

    /// A package was installed, in a development session: every app a running process runs whose
    /// installed build changed is verified as a launch verifies it and handed to that process on its
    /// socket, once, so its open windows take the new build over. A build that does not verify, or
    /// that only a process of other runtimes could run is left for the app's next
    /// start.
    pub fn reinstalled(&self) {
        let mut running = self.running();
        for (key, process) in running.iter_mut() {
            for (app, build) in &mut process.builds {
                match self.send_later_build(key, &process.socket, app, build) {
                    Ok(true) => {
                        tracing::info!(app, workspace = %key.workspace, "a later build was handed to its process");
                    }
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(app, workspace = %key.workspace, %error, "a later build was not handed to its process");
                    }
                }
            }
        }
    }

    /// Hand the process `key` behind `socket` the installed build of `app` when it is not `build`, the
    /// one it runs, which it is from then on. Whether it was handed.
    fn send_later_build(
        &self,
        key: &HostKey,
        socket: &Descriptor,
        app: &str,
        build: &mut Build,
    ) -> std::io::Result<bool> {
        let refused =
            |what: String| std::io::Error::new(std::io::ErrorKind::PermissionDenied, what);
        let verified = (self.verifies)(&key.workspace, app).map_err(refused)?;
        let runs_in = HostKey {
            workspace: key.workspace.clone(),
            app: app.to_owned(),
            alone: key.alone.clone(),
            runtime: verified.runtime,
            app_runtime: verified.app_runtime,
        };
        if runs_in != *key {
            return Err(refused(
                "it names other runtimes than the process runs: only a new start runs it"
                    .to_owned(),
            ));
        }
        let module = self.module_for(app, verified.module, key).encode();
        let hashed = hash(&module);
        if hashed == build.module {
            return Ok(false);
        }
        let later = AppBuild {
            app: app.to_owned(),
            module_len: module.len() as u64,
        };
        let module = tilo_render_protocol::shm::sealed_blob("tilolang-module", &module)?;
        ToProcess::Build {
            build: later,
            module,
        }
        .send(socket)?;
        build.module = hashed;
        Ok(true)
    }

    /// Watch the person's app store for reinstalls, once, in a development session: each one is
    /// [`Self::reinstalled`].
    fn follow_reinstalls(self: &Arc<Self>) {
        let develops = package_store::dev_env::dev_override(launcher::dev_env::LIVE_MODULES_ENV)
            .is_some_and(|value| value == "1");
        if !develops || self.follows_reinstalls.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(store) = package_store::layout::default_user_root() else {
            tracing::warn!("reinstalls are not followed: there is no app store");
            return;
        };
        let processes = Arc::downgrade(self);
        let started = std::thread::Builder::new()
            .name("tilolang-reinstalls".into())
            .spawn(move || {
                let (replaced, replacements) = std::sync::mpsc::channel();
                let watch = tilo_files::native::watch_changes(
                    &store,
                    |path| {
                        path.file_name()
                            .is_some_and(|name| name == package_store::layout::REGISTRY_FILE)
                    },
                    move || {
                        let _ = replaced.send(());
                    },
                );
                let _watch = match watch {
                    Ok(watch) => watch,
                    Err(error) => {
                        tracing::warn!(%error, store = %store.display(), "reinstalls are not followed");
                        return;
                    }
                };
                while replacements.recv().is_ok() {
                    // A burst of replacements is one look at the store.
                    while replacements.try_recv().is_ok() {}
                    let Some(processes) = processes.upgrade() else {
                        return;
                    };
                    processes.reinstalled();
                }
            });
        if let Err(error) = started {
            tracing::warn!(%error, "reinstalls are not followed");
        }
    }

    /// Send the instance to the process `key`, starting it first when none runs. Whether it started
    /// the process.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "a process is chosen, started and handed the instance in one hold, so it never ends in between"
    )]
    fn send(
        self: &Arc<Self>,
        key: &HostKey,
        request: &InstanceRequest,
        fds: &InstanceFds,
        build: Build,
    ) -> std::io::Result<bool> {
        let mut processes = self.running();
        if let Some(process) = processes.get_mut(key) {
            match send_instance(&process.socket, request, fds) {
                Ok(()) => {
                    process.idle_since = None;
                    process
                        .instances
                        .insert(request.instance, request.app.clone());
                    // Its other windows run the build handed last; a later one reaches them all.
                    process.builds.entry(request.app.clone()).or_insert(build);
                    return Ok(false);
                }
                // The process is ending: start another.
                Err(error) => {
                    tracing::info!(%error, "a tilolang process's socket is closed");
                    processes.remove(key);
                }
            }
        }
        let mut process = self.start(key, None)?;
        send_instance(&process.socket, request, fds)?;
        process
            .instances
            .insert(request.instance, request.app.clone());
        process.builds.insert(request.app.clone(), build);
        processes.insert(key.clone(), process);
        self.follow_reinstalls();
        Ok(true)
    }

    /// Start a host of its own for the agent task `request`, handed `fds`: a tilolang process
    /// started like every other ([`spawn_host`]), on the tilolang and Tilo runtimes `runs_on`
    /// (those the apps installed here run on, [`super::package_runs::task_runtimes`]), in the same
    /// sandbox as an app's process and granted less: its own storage, and nothing any
    /// app kept. It takes no other task and no instance, and it is followed like every process
    /// here until it ends: once the API closes the task's program connection, or with this
    /// process, whose end of its socket is its lifeline.
    ///
    /// # Errors
    /// The process did not start, or did not take the task.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the process is started and kept in one hold, so its follow never misses it"
    )]
    pub fn start_task(
        self: &Arc<Self>,
        request: TaskRequest,
        fds: TaskFds,
        (runtime, app_runtime): (String, String),
    ) -> std::io::Result<()> {
        let key = HostKey {
            workspace: request.workspace.clone(),
            app: String::new(),
            alone: Some(Alone::Task(request.task.clone())),
            runtime,
            app_runtime,
        };
        let mut running = self.running();
        let process = self.start(&key, None)?;
        let (workspace, task) = (request.workspace.clone(), request.task.clone());
        ToProcess::Task { request, fds }.send(&process.socket)?;
        // It takes nothing more: it ends once its task did.
        let _ = ToProcess::Finish.send(&process.socket);
        running.insert(key, process);
        tracing::info!(%workspace, %task, "a task's host started");
        Ok(())
    }

    /// Show the view `view` of `app` in `workspace` on a web page: its window, started in the
    /// sandbox every tilolang process here runs in ([`spawn_host`]), on the runtimes the app's
    /// package names and granted none of what the app keeps, holding the view's patch connection
    /// and its end of the page's link. The answer is the device's end of that link: the window
    /// ends once it is closed, and is followed like every process here until it has.
    ///
    /// # Errors
    /// The app does not verify, or its window did not start.
    pub(crate) fn show_page(
        self: &Arc<Self>,
        app: &str,
        workspace: &str,
        view: crate::apps::OpenedView,
    ) -> std::io::Result<tilo_socket::Stream> {
        let verified = (self.verifies)(workspace, app).map_err(std::io::Error::other)?;
        (self.opened)(app);
        let key = HostKey {
            workspace: workspace.to_owned(),
            app: app.to_owned(),
            alone: Some(Alone::Page(
                self.generations.fetch_add(1, Ordering::Relaxed),
            )),
            runtime: verified.runtime,
            app_runtime: verified.app_runtime,
        };
        let mut running = self.running();
        let process = self.start(&key, Some((view.patch, view.page)))?;
        running.insert(key, process);
        drop(running);
        tracing::info!(%workspace, %app, "a view's page window started");
        Ok(view.link)
    }

    /// Where the process `key` keeps what its instances keep.
    fn storage(&self, key: &HostKey) -> std::io::Result<PathBuf> {
        let dir = self
            .data_home
            .join("tilo")
            .join("tilolang-processes")
            .join(&key.workspace)
            .join(match &key.alone {
                Some(Alone::Task(_)) => TASK_STORAGE,
                Some(Alone::Page(_)) => PAGE_STORAGE,
                None => &key.app,
            });
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Start the process `key`, with its socket at the launch kind's descriptor (a `page` window
    /// with its patch connection and page link besides), and follow it on a thread of its own
    /// until it ends.
    fn start(
        self: &Arc<Self>,
        key: &HostKey,
        page: Option<(Descriptor, Descriptor)>,
    ) -> std::io::Result<Host> {
        let storage = self.storage(key)?;
        let (ours, started) = spawn_host(&self.launcher, key, &storage, page)?;
        let tag = format!("{} {}", key.workspace, key.app);
        let said = started.stderr.map(|stderr| {
            std::thread::Builder::new()
                .name("tilolang-process-err".into())
                .spawn(move || last_lines(stderr, &tag))
        });
        let generation = self.generations.fetch_add(1, Ordering::Relaxed);
        let socket = Arc::new(ours);
        // A connection from this process is the app it runs, not the account that started it.
        if let Some(attests) = self.attests.get() {
            let _ = attests.record_started(started.pid, &key.app, &key.workspace);
        }
        tracing::info!(
            workspace = %key.workspace,
            app = %key.app,
            pid = started.pid,
            "tilolang process started"
        );
        let (processes, followed, key) = (Arc::clone(self), Arc::clone(&socket), key.clone());
        let pid = started.pid;
        std::thread::Builder::new()
            .name("tilolang-process".into())
            .spawn(move || {
                processes.follow(&key, generation, pid, &followed, started.wait, said)
            })?;
        Ok(Host {
            socket,
            instances: HashMap::new(),
            generation,
            builds: HashMap::new(),
            idle_since: None,
        })
    }

    /// Follow the process `key` of `generation` over its socket `followed` until it ends: each
    /// instance that ends is noted, then the process's own end and why, and the process is forgotten.
    fn follow(
        self: &Arc<Self>,
        key: &HostKey,
        generation: u64,
        pid: u32,
        followed: &Descriptor,
        wait: Wait,
        said: Said,
    ) {
        // Why the process says it exits; none when it ended without saying (killed, crashed).
        let mut reason: Option<String> = None;
        loop {
            match FromProcess::receive(followed) {
                Ok(Some(FromProcess::Ended(instance))) => self.ended(key, generation, instance),
                Ok(Some(FromProcess::Exiting(why))) => reason = Some(why),
                Ok(Some(FromProcess::Serves)) => {}
                Ok(Some(FromProcess::Media { instance, view })) => {
                    self.media(key, instance, view);
                }
                Ok(None) => break,
                // The packet was taken: the socket carries the next one.
                Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                    tracing::error!(%error, workspace = %key.workspace, app = %key.app, "a tilolang process said something unreadable");
                }
                // The socket failed: it carries nothing more; the process is waited for.
                Err(error) => {
                    tracing::error!(%error, workspace = %key.workspace, app = %key.app, "a tilolang process's socket failed");
                    break;
                }
            }
        }
        let status = wait();
        // The process ended: nothing that connects from its pid is its app any more.
        if let Some(attests) = self.attests.get() {
            attests.forget(pid);
        }
        let clean = status.as_ref().is_ok_and(std::process::ExitStatus::success);
        // Its error output ends with it: what it said last is all read by now.
        let last = said
            .and_then(Result::ok)
            .and_then(|reading| reading.join().ok())
            .unwrap_or_default();
        let reason = reason.unwrap_or_else(|| seen_end(&status, &last));
        if clean {
            tracing::info!(workspace = %key.workspace, app = %key.app, ?status, reason, "tilolang process ended");
        } else {
            tracing::error!(workspace = %key.workspace, app = %key.app, ?status, reason, "tilolang process ended");
        }
        let mut processes = self.running();
        if processes
            .get(key)
            .is_some_and(|process| process.generation == generation)
        {
            processes.remove(key);
        }
    }

    /// The process `key` (its start `generation`) says the instance of the window `instance` ended.
    /// Once its last instance ended it is idle, and ends [`IDLE`] later unless called again.
    fn ended(self: &Arc<Self>, key: &HostKey, generation: u64, instance: u64) {
        let idle = {
            let mut processes = self.running();
            let Some(process) = processes
                .get_mut(key)
                .filter(|process| process.generation == generation)
            else {
                return;
            };
            if let Some(app) = process.instances.remove(&instance)
                && !process.instances.values().any(|running| *running == app)
            {
                process.builds.remove(&app);
            }
            process.instances.is_empty().then(|| {
                process.idle_since = Some(Instant::now());
            })
        };
        if idle.is_some() {
            let processes = Arc::clone(self);
            let spawned = std::thread::Builder::new()
                .name("tilolang-process-idle".into())
                .spawn(move || {
                    std::thread::sleep(IDLE);
                    processes.close_idle(Instant::now());
                });
            if let Err(error) = spawned {
                // With no thread to end it later, it ends now: a process is never left for good.
                tracing::warn!(%error, "no thread to end an idle tilolang process later");
                self.close_idle(Instant::now() + IDLE);
            }
        }
    }

    /// Close every process that ran no call for [`IDLE`] by `now`: each ends once it has written
    /// what it keeps.
    fn close_idle(&self, now: Instant) {
        let closed: Vec<Host> = {
            let mut processes = self.running();
            let idle: Vec<HostKey> = processes
                .iter()
                .filter(|(_, process)| {
                    process.instances.is_empty()
                        && process
                            .idle_since
                            .is_some_and(|since| now.saturating_duration_since(since) >= IDLE)
                })
                .map(|(key, _)| key.clone())
                .collect();
            idle.iter()
                .filter_map(|key| processes.remove(key))
                .collect()
        };
        for process in closed {
            // The process lets its link go and ends once it runs nothing.
            let _ = ToProcess::Finish.send(&process.socket);
        }
    }
}

/// What waits for a started host to end, and answers how it ended.
type Wait = Box<dyn FnOnce() -> std::io::Result<std::process::ExitStatus> + Send>;

/// A host as it started: its process, its error output, and what waits for its end.
struct Started {
    pid: u32,
    /// Read here: what it said last is why it ended when it could not say so itself.
    stderr: Option<Box<dyn std::io::Read + Send>>,
    wait: Wait,
}

/// Start the tilolang host `key`, keeping `storage`, on its process link: the one place a host is started, and the only one that names the platform. On Linux the launcher
/// is `tilo-launcher tilolang-process`, which takes the link off its stdin and starts the host in
/// its sandbox; a `page` window's patch connection and page link go to it as descriptors 3 and 4,
/// and it keeps the link while the window runs.
#[cfg(target_os = "linux")]
fn spawn_host(
    launcher: &Launcher,
    key: &HostKey,
    storage: &Path,
    page: Option<(Descriptor, Descriptor)>,
) -> std::io::Result<(runtime_portal::process_link::Descriptor, Started)> {
    use std::os::fd::AsRawFd as _;
    let mut command = std::process::Command::new(launcher);
    command
        .arg(launcher::run::tilolang_process::SUBCOMMAND)
        .arg("--runtime")
        .arg(&key.runtime)
        .arg("--app-runtime")
        .arg(&key.app_runtime)
        .arg("--storage")
        .arg(storage);
    if let Some((patch, page)) = &page {
        command.arg("--page");
        launcher::inherit::hand_down(
            &mut command,
            vec![(patch.as_raw_fd(), 3), (page.as_raw_fd(), 4)],
            false,
        );
    }
    command.stderr(std::process::Stdio::piped());
    let spawned = runtime_portal::process_link::spawn(command);
    drop(page);
    let (link, mut child) = spawned?;
    let stderr = child
        .stderr
        .take()
        .map(|stderr| Box::new(stderr) as Box<dyn std::io::Read + Send>);
    Ok((
        link,
        Started {
            pid: child.id(),
            stderr,
            wait: Box::new(move || child.wait()),
        },
    ))
}

/// [`spawn_host`] on macOS: the tilolang runtime's own binary, from the installed runtime package
/// the host runs on (verified as a launch verifies it), in a deny-default Seatbelt sandbox that
/// reads the runtime, keeps its storage and reaches nothing else: no network, no service socket,
/// no other app's storage. Its link and a `page` window's connections are inherited at their own
/// numbers, named in its environment.
#[cfg(target_os = "android")]
fn spawn_host(
    (): &Launcher,
    _key: &HostKey,
    _storage: &Path,
    _page: Option<(Descriptor, Descriptor)>,
) -> std::io::Result<(runtime_portal::process_link::Descriptor, Started)> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Android starts no process for an app: its apps run in the Tilo app's own process",
    ))
}

#[cfg(target_os = "macos")]
fn spawn_host(
    launcher: &Launcher,
    key: &HostKey,
    storage: &Path,
    page: Option<(Descriptor, Descriptor)>,
) -> std::io::Result<(runtime_portal::process_link::Descriptor, Started)> {
    use runtime_portal::process_link::{FD_ENV, PAGE_ENV, STORAGE_ENV};
    use std::ffi::OsString;
    use std::os::fd::AsRawFd as _;
    let runtime = crate::apps::installed_runtime(launcher, &key.runtime)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotFound, error))?;
    let (ours, theirs) = runtime_portal::process_link::pair()?;
    let (said, says) = std::io::pipe()?;
    let mut inherit_fds = vec![theirs.as_raw_fd()];
    let mut env = vec![(OsString::from(STORAGE_ENV), storage.as_os_str().to_owned())];
    match &page {
        Some((patch, page)) => {
            inherit_fds.extend([patch.as_raw_fd(), page.as_raw_fd()]);
            env.push((
                OsString::from(PAGE_ENV),
                OsString::from(format!("{},{}", patch.as_raw_fd(), page.as_raw_fd())),
            ));
        }
        None => env.push((
            OsString::from(FD_ENV),
            OsString::from(theirs.as_raw_fd().to_string()),
        )),
    }
    let request = launcher_macos::LaunchRequest {
        package_id: key.app.clone(),
        executable: runtime.join(launcher::manifest::TILOLANG_BIN),
        args: Vec::new(),
        payload_dir: runtime.clone(),
        runtime_dir: None,
        container_dir: storage.to_path_buf(),
        capabilities: launcher::capabilities::CapabilityPlan::default(),
        egress: launcher::egress_proxy::EgressDecision::Blocked,
        inherit_fds,
        stderr: Some(says.as_raw_fd()),
        env,
        trust: launcher_macos::TrustRequirement::DeveloperId,
    };
    let launched = launcher_macos::launch(&request).map_err(std::io::Error::other);
    // The host holds its own ends now.
    drop((theirs, says, page));
    let mut launched = launched?;
    let pid = u32::try_from(launched.pid()).map_err(|_| std::io::ErrorKind::InvalidData)?;
    Ok((
        ours,
        Started {
            pid,
            stderr: Some(Box::new(said)),
            wait: Box::new(move || launched.wait()),
        },
    ))
}

/// [`spawn_host`] on Windows: the tilolang runtime's own binary, from the installed runtime
/// package the host runs on (verified as on macOS), in an `AppContainer` of the app's own that
/// reads the runtime, keeps its storage and reaches nothing else: no network, no pipe of the
/// daemon's, no other app's storage. It inherits its link alone and takes everything else on it,
/// and its job ends it, and whatever it started, with the daemon. A visitor's page window is
/// started with two connections besides its link, which a confined Windows start does not hand
/// over yet, so a page view is refused on Windows.
#[cfg(windows)]
fn spawn_host(
    launcher: &Launcher,
    key: &HostKey,
    storage: &Path,
    page: Option<(Descriptor, Descriptor)>,
) -> std::io::Result<(runtime_portal::process_link::Descriptor, Started)> {
    if page.is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "a page view's window is not started on a Windows device",
        ));
    }
    let runtime = crate::apps::installed_runtime(launcher, &key.runtime)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotFound, error))?;
    let mut command = std::process::Command::new(
        runtime
            .join(launcher::manifest::TILOLANG_BIN)
            .with_extension(std::env::consts::EXE_EXTENSION),
    );
    command.env(runtime_portal::process_link::STORAGE_ENV, storage);
    let (link, mut launched) =
        super::capabilities::launch_confined(launcher_windows::LaunchRequest {
            package_id: key.app.clone(),
            command,
            read: vec![runtime],
            read_write: vec![storage.to_path_buf()],
        })?;
    let stderr = launched
        .take_stderr()
        .map(|stderr| Box::new(stderr) as Box<dyn std::io::Read + Send>);
    Ok((
        link,
        Started {
            pid: launched.pid(),
            stderr,
            wait: Box::new(move || launched.wait()),
        },
    ))
}

/// The hash of an encoded module: whether a process runs that build already.
fn hash(module: &[u8]) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::hash::DefaultHasher::new();
    module.hash(&mut hasher);
    hasher.finish()
}

/// A process's error output, read on a thread of its own: its last lines once it has ended.
type Said = Option<std::io::Result<std::thread::JoinHandle<VecDeque<String>>>>;

/// Relay each line `stderr` says to the daemon's log under `tag`, and answer the last ones once
/// it ends.
fn last_lines(mut stderr: impl std::io::Read, tag: &str) -> VecDeque<String> {
    let mut last = VecDeque::with_capacity(SAID_LINES);
    let mut chunk = [0_u8; 4096];
    let mut lines = wire_frame::BoundedLines::new(wire_frame::LINE_BYTES);
    let mut keep = |bytes: &[u8], truncated: bool| {
        let mut line = String::from_utf8_lossy(bytes).into_owned();
        if truncated {
            line.push_str("… [truncated]");
        }
        tracing::info!(
            target: tilo_startup_metrics::APP_OUTPUT_TARGET,
            "[tilolang-process {tag}] {line}"
        );
        if last.len() == SAID_LINES {
            last.pop_front();
        }
        last.push_back(line);
    };
    loop {
        let read = match stderr.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let bytes = chunk.get(..read).unwrap_or_default();
        lines.push(bytes, &mut keep);
    }
    lines.finish(keep);
    last
}

/// Why a process ended that did not say so itself: how it ended, as the daemon saw it, and the last
/// lines of its error output.
fn seen_end(status: &std::io::Result<std::process::ExitStatus>, last: &VecDeque<String>) -> String {
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt as _;
    #[cfg(unix)]
    let signal = |status: &std::process::ExitStatus| status.signal();
    // A Windows process always ends with a code: one the system's end of it set says so itself.
    #[cfg(windows)]
    let signal = |_: &std::process::ExitStatus| None::<i32>;
    let how = match status {
        Ok(status) => match (status.code(), signal(status)) {
            (Some(code), _) => format!("it exited with status {code} without saying why"),
            (None, Some(signal)) => {
                #[cfg(unix)]
                let name = match signal {
                    libc::SIGTERM => " (SIGTERM)",
                    libc::SIGKILL => " (SIGKILL: killed, as memory pressure kills)",
                    libc::SIGSEGV | libc::SIGBUS | libc::SIGILL => " (it crashed)",
                    libc::SIGABRT => " (SIGABRT: it aborted)",
                    libc::SIGSYS => " (SIGSYS: its sandbox refused a system call)",
                    _ => "",
                };
                #[cfg(windows)]
                let name = "";
                format!("it was ended by signal {signal}{name} before it could say why")
            }
            (None, None) => format!("it ended ({status}) without saying why"),
        },
        Err(error) => format!("it ended, and waiting for it failed: {error}"),
    };
    if last.is_empty() {
        return format!("{how}; it wrote no error output");
    }
    let said: Vec<&str> = last.iter().map(String::as_str).collect();
    format!("{how}; it last wrote: {}", said.join(" | "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::OwnedFd;

    const NOTES: &str = "so.tilo.notes";

    fn key(workspace: &str, app: &str) -> HostKey {
        HostKey {
            workspace: workspace.to_owned(),
            app: app.to_owned(),
            alone: None,
            runtime: "tilolang@1".to_owned(),
            app_runtime: "tilo@1".to_owned(),
        }
    }

    /// A launcher that starts nothing.
    fn no_launcher() -> Launcher {
        #[cfg(target_os = "linux")]
        return PathBuf::from("/nonexistent");
        #[cfg(target_os = "macos")]
        return package_store::Catalog::at(PathBuf::from("/nonexistent"), None);
    }

    fn processes(data_home: PathBuf) -> Arc<Processes> {
        Processes::with(
            data_home,
            no_launcher(),
            Box::new(|_, _| Err("no package store here".to_owned())),
        )
    }

    /// The processes a launcher starts: a script that starts this test binary, which says what it
    /// was handed. A Mac starts the runtime package's own binary from its catalog instead.
    #[cfg(target_os = "linux")]
    mod launched {
        use super::*;

        /// The variable that makes this test binary the process the test launcher starts, and the
        /// pipe it says what it was handed on.
        const LAUNCHED_ENV: &str = "TILO_TEST_TILOLANG_PROCESS_SAYS";
        const LAUNCHED_TEST: &str =
            "device::processes::tests::launched::the_process_the_tests_launcher_starts";
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-0000000000a1";
        const ELSEWHERE: &str = "7a110d00-0000-4000-8000-0000000000a2";

        /// What the process the test launcher started was handed: the launcher's arguments, the
        /// instance, and which of its connections came with it.
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Handed {
            args: String,
            request: InstanceRequest,
            window: bool,
            content: bool,
        }

        /// Run as the process the test launcher starts: read the first message on the link the
        /// daemon handed it, and say what it held on the pipe its test reads.
        #[test]
        fn the_process_the_tests_launcher_starts() {
            use std::os::fd::AsFd as _;
            let Some(says) = std::env::var_os(LAUNCHED_ENV) else {
                return;
            };
            let link = std::io::stdin().as_fd().try_clone_to_owned().unwrap();
            let Some(ToProcess::Instance { request, fds }) = ToProcess::receive(&link).unwrap()
            else {
                panic!("the process is handed an instance first");
            };
            let handed = Handed {
                args: std::env::var("TILO_TEST_LAUNCHER_ARGS").unwrap(),
                request: *request,
                window: fds.window.is_some(),
                content: fds.content.is_some(),
            };
            std::fs::write(says, serde_json::to_vec(&handed).unwrap()).unwrap();
        }

        /// A device whose launcher starts this test binary as each process, which says on a pipe what
        /// it was handed, and that runs `NOTES` alone: any other app does not verify.
        struct Device {
            dir: tempfile::TempDir,
            processes: Arc<Processes>,
            _launcher: tilo_test_env::StandIn,
        }

        impl Device {
            fn new() -> Self {
                let dir = tempfile::tempdir().unwrap();
                let says = dir.path().join("says");
                let pipe = std::ffi::CString::new(says.as_os_str().as_encoded_bytes()).unwrap();
                // SAFETY: a NUL-terminated path this test owns.
                assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
                let launcher = tilo_test_env::stand_in(
                    dir.path().join("tilo-launcher"),
                    &format!(
                        "export TILO_TEST_LAUNCHER_ARGS=\"$*\" {LAUNCHED_ENV}='{says}'\nexec '{exe}' --exact {LAUNCHED_TEST} --nocapture --quiet\n",
                        says = says.display(),
                        exe = std::env::current_exe().unwrap().display(),
                    ),
                )
                .unwrap();
                let processes = Processes::with(
                    dir.path().join("data"),
                    launcher.path().to_owned(),
                    Box::new(|_, app| {
                        if app != NOTES {
                            return Err(format!("{app} is not installed here"));
                        }
                        Ok(crate::device::package_runs::Verified {
                            runtime: "tilolang@1".to_owned(),
                            app_runtime: "tilo@1".to_owned(),
                            module: runtime_portal::process_link::Module::default(),
                        })
                    }),
                );
                Self {
                    dir,
                    processes,
                    _launcher: launcher,
                }
            }

            /// What the process its launcher started says it was handed, once it says it.
            fn handed(&self) -> Handed {
                let says = self.dir.path().join("says");
                let (told, telling) = std::sync::mpsc::channel();
                std::thread::spawn(move || told.send(std::fs::read(says)));
                let said = telling
                    .recv_timeout(std::time::Duration::from_secs(120))
                    .expect("the process says what it was handed")
                    .unwrap();
                serde_json::from_slice(&said).unwrap()
            }

            /// The processes as the person's shell reaches them, in [`WORKSPACE`] alone: each ask
            /// on a packet socket handed to them as the daemon's dispatch hands the one a shell's
            /// admitted connection carried.
            fn shell(&self) -> runtime_portal::device_processes::Device {
                let handing = self.processes.for_the_shell(
                    Box::new(|workspace| workspace == WORKSPACE),
                    Arc::new(NoOrigins),
                );
                runtime_portal::device_processes::Device::over(Arc::new(
                    move || -> std::io::Result<OwnedFd> {
                        let (ours, theirs) = runtime_portal::packet::pair()?;
                        handing(theirs);
                        Ok(ours)
                    },
                ))
            }
        }

        fn connection() -> (OwnedFd, OwnedFd) {
            runtime_portal::process_link::pair().unwrap()
        }

        /// The apps these tests hand over reach no outside service.
        struct NoOrigins;

        impl Origins for NoOrigins {
            fn connection(&self, _: &str, _: &str) -> Result<Option<OwnedFd>, String> {
                Ok(None)
            }

            fn attach(&self, app: &str, origin: &str, _: &str, _: &OwnedFd) -> Result<(), String> {
                Err(format!("{app} reaches no outside service called {origin}"))
            }
        }

        fn handed(app: &str, workspace: &str) -> (HandedInstance, HandedFds, Vec<OwnedFd>) {
            let (patch, patch_peer) = connection();
            let (collaboration, collaboration_peer) = connection();
            let (identity, identity_peer) = connection();
            let (content, content_peer) = connection();
            let instance = HandedInstance {
                app: app.to_owned(),
                workspace: workspace.to_owned(),
                instance: 7,
                person: "ada".to_owned(),
                document: None,
                reveal: None,
                open: None,
                access: None,
                locale: runtime_portal::process_link::Locale::default(),
                link_home: None,
            };
            let fds = HandedFds {
                patch: Some(patch),
                collaboration,
                identity,
                content: Some(content),
                chooser: None,
                clipboard: None,
                tools: None,
            };
            (
                instance,
                fds,
                vec![patch_peer, collaboration_peer, identity_peer, content_peer],
            )
        }

        /// A window's instance the shell hands over on its link runs in the process the daemon starts
        /// for its workspace and app, with the window's connections, and the daemon
        /// lists that process as running it.
        #[test]
        fn a_window_instance_handed_over_the_shell_link_runs_in_the_daemons_process() {
            let device = Device::new();
            let daemon = device.shell();
            let (instance, fds, _peers) = handed(NOTES, WORKSPACE);
            assert!(
                daemon.hand_over(instance, fds).unwrap(),
                "the daemon starts the process of the workspace and app"
            );
            assert_eq!(
                daemon.running().unwrap(),
                vec![Running {
                    workspace: WORKSPACE.to_owned(),
                    app: NOTES.to_owned(),
                    instances: vec![7],
                }],
                "the daemon's process runs the window's instance"
            );
            let handed = device.handed();
            assert!(
                handed.args.starts_with("tilolang-process ")
                    && handed.args.contains(&format!("{WORKSPACE}/{NOTES}")),
                "the launcher starts a tilolang process with the storage of the workspace and app: {}",
                handed.args
            );
            assert_eq!(
                (
                    handed.request.app.as_str(),
                    handed.request.workspace.as_str(),
                    handed.request.instance,
                    handed.request.person.as_str(),
                    handed.request.shows,
                ),
                (
                    NOTES,
                    WORKSPACE,
                    7,
                    "ada",
                    runtime_portal::process_link::Shows::View
                ),
                "the process runs the window's instance, for the person at it"
            );
            assert!(
                handed.window && handed.content,
                "the window's own connections reach the process"
            );
        }

        /// The desktop's media controls follow what the daemon's processes play over the shell's
        /// connection, and a control for a Playback nothing here plays reaches no process.
        #[test]
        fn the_desktops_media_controls_follow_the_daemons_processes() {
            use runtime_portal::device_processes::MediaChange;
            let device = Device::new();
            let daemon = device.shell();
            let following = daemon.follow_media().unwrap();
            let key = HostKey {
                workspace: WORKSPACE.to_owned(),
                app: NOTES.to_owned(),
                alone: None,
                runtime: "tilolang@1".to_owned(),
                app_runtime: "tilo@1".to_owned(),
            };
            let view = MediaView {
                id: "playback-1".to_owned(),
                identity: "Video".to_owned(),
                title: "Undersea cables".to_owned(),
                artist: String::new(),
                art_url: String::new(),
                playing: true,
                position_us: 0,
                length_us: 60_000_000,
                volume: 1.0,
                rate: 1.0,
                can_next: false,
                can_prev: false,
            };
            device.processes.media(&key, 7, Some(view.clone()));
            assert_eq!(
                FromDevice::receive(&following).unwrap(),
                Some(FromDevice::Media(MediaChange::Playing(view))),
                "what an app plays reaches the desktop's controls"
            );
            device.processes.media(&key, 7, None);
            assert_eq!(
                FromDevice::receive(&following).unwrap(),
                Some(FromDevice::Media(MediaChange::Stopped(
                    "playback-1".to_owned()
                ))),
                "a Playback that stopped leaves them"
            );
            assert!(
                !daemon
                    .media_command("playback-1", MediaCommand::Toggle)
                    .unwrap(),
                "a control for a Playback nothing plays reaches no process"
            );
        }

        /// A daemon no desktop session asks starts a process the same way: a call that shows nothing
        /// runs in the process of its workspace and app.
        #[test]
        fn a_process_starts_on_a_daemon_with_no_session() {
            let device = Device::new();
            let (collaboration, _collaboration) = connection();
            let (identity, _identity) = connection();
            let started = device
                .processes
                .hand_over_now(Instance {
                    app: NOTES.to_owned(),
                    workspace: WORKSPACE.to_owned(),
                    instance: 9,
                    person: "ada".to_owned(),
                    patch: None,
                    collaboration,
                    identity,
                    origin: None,
                    content: None,
                    chooser: None,
                    clipboard: None,
                    tools: None,
                    document: None,
                    reveal: None,
                    open: None,
                    access: None,
                    locale: runtime_portal::process_link::Locale::default(),
                    link_home: None,
                })
                .unwrap();
            assert!(started, "the daemon starts the process itself");
            let handed = device.handed();
            assert_eq!(
                (
                    handed.request.workspace.as_str(),
                    handed.request.app.as_str(),
                    handed.request.instance,
                    handed.request.shows,
                ),
                (
                    WORKSPACE,
                    NOTES,
                    9,
                    runtime_portal::process_link::Shows::Nothing
                ),
                "the call runs in its workspace's process and shows nothing"
            );
            assert!(!handed.window, "a call that shows nothing has no window");
        }

        /// A hand-over for a workspace the device has none of the person's, or for an app that does
        /// not verify there, is refused, naming why, and starts nothing.
        #[test]
        fn a_hand_over_the_caller_may_not_make_is_refused() {
            let device = Device::new();
            let daemon = device.shell();
            for (app, workspace, why) in [
                (NOTES, ELSEWHERE, "has no workspace"),
                ("so.tilo.unknown", WORKSPACE, "is not installed here"),
            ] {
                let (instance, fds, _peers) = handed(app, workspace);
                let refused = daemon.hand_over(instance, fds).unwrap_err();
                assert_eq!(
                    refused.kind(),
                    std::io::ErrorKind::PermissionDenied,
                    "{refused}"
                );
                assert!(refused.to_string().contains(why), "{refused}");
            }
            assert!(daemon.running().unwrap().is_empty(), "nothing started");
        }
    }

    /// What changed reaches every process it is about, once, and no other: the person's formats
    /// and language, a reinstall that verified, and the workspace's policy — each said on the
    /// socket of the process running where it changed.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one flow through what a session tells its running processes, which is what it proves"
    )]
    fn what_changed_reaches_the_processes_it_is_about_once_and_no_others() {
        use runtime_portal::process_link::Module;
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-0000000000f1";
        const ELSEWHERE: &str = "7a110d00-0000-4000-8000-0000000000f3";
        const REINSTALLED: &str = "7a110d00-0000-4000-8000-0000000000f4";
        let module = |wasm: &[u8]| Module {
            wasm: wasm.to_vec(),
            ..Module::default()
        };
        // The build the store holds now, when it verifies.
        let installed = Arc::new(Mutex::new(Some(module(b"old"))));
        let store = Arc::clone(&installed);
        let processes = Processes::with(
            std::env::temp_dir(),
            no_launcher(),
            Box::new(move |_, _| {
                Ok(Verified {
                    runtime: "tilolang@1".to_owned(),
                    app_runtime: "tilo@1".to_owned(),
                    module: store.lock().unwrap().clone().ok_or("it does not verify")?,
                })
            }),
        );
        let (ours, theirs) = runtime_portal::process_link::pair().unwrap();
        let (other_ours, other_theirs) = runtime_portal::process_link::pair().unwrap();
        let (build_ours, build_theirs) = runtime_portal::process_link::pair().unwrap();
        let (idle_ours, idle_theirs) = runtime_portal::process_link::pair().unwrap();
        let old = Build {
            module: hash(&module(b"old").encode()),
        };
        for (workspace, socket, builds) in [
            (WORKSPACE, ours, HashMap::new()),
            (ELSEWHERE, other_ours, HashMap::new()),
            (
                REINSTALLED,
                build_ours,
                HashMap::from([(NOTES.to_owned(), old)]),
            ),
        ] {
            let process = Host {
                socket: Arc::new(socket),
                instances: HashMap::from([(7, NOTES.to_owned()), (8, NOTES.to_owned())]),
                generation: 1,
                builds,
                idle_since: None,
            };
            processes.running().insert(key(workspace, NOTES), process);
        }

        // Every process running says what the person's formats now are, so the instances it runs
        // write their weeks and times again; a process that is not running hears nothing.
        let before = ui::format::region::current();
        let sunday = ui::format::region::RegionFormats {
            first_day: ui::format::region::FirstDayOfWeek::Sunday,
            ..before
        };
        ui::format::region::install(sunday);
        let locale = runtime_portal::process_link::Locale {
            formats: ui::format::region::current(),
            language: ui::i18n::preferred_language().unwrap_or_default(),
        };
        processes.locale_changed(&locale);
        let handed = ToProcess::receive(&theirs).unwrap();
        ui::format::region::install(before);
        let Some(ToProcess::Locale(locale)) = handed else {
            panic!("a runtime process's socket carries the formats as formats: {handed:?}");
        };
        assert_eq!(
            locale.formats, sunday,
            "the week the person starts on reaches the process its instances run in"
        );
        drop(idle_ours);
        assert!(
            ToProcess::receive(&idle_theirs).unwrap().is_none(),
            "a process that is not running is not told"
        );
        for socket in [&other_theirs, &build_theirs] {
            assert!(
                matches!(ToProcess::receive(socket), Ok(Some(ToProcess::Locale(_)))),
                "every process running is told the formats, wherever it works"
            );
        }

        // In a development session, a reinstall of an app a process runs reaches that process's
        // socket once, as the module that verified; a reinstall that does not verify reaches it
        // not at all.
        processes.reinstalled();
        *installed.lock().unwrap() = Some(module(b"new"));
        processes.reinstalled();
        processes.reinstalled();
        *installed.lock().unwrap() = None;
        processes.reinstalled();
        let handed = ToProcess::receive(&build_theirs).unwrap();
        let Some(ToProcess::Build {
            build: later,
            module: fd,
        }) = handed
        else {
            panic!("the process is handed the later build: {handed:?}");
        };
        assert_eq!(later.app, NOTES);
        let len = usize::try_from(later.module_len).unwrap();
        let mapping = tilo_render_protocol::shm::Mapping::map_sealed(&fd, true, len, len).unwrap();
        let handed = Module::decode(mapping.get(0, len).unwrap()).unwrap();
        assert_eq!(handed, module(b"new"), "the module that verified");

        // When the workspace's policy changes, every process running in it is told what the
        // person may now do in each app it runs, so the windows already open follow.
        let mut policy = tilo_action_policy::WorkspacePolicy::new(WORKSPACE, "bob");
        policy
            .members
            .insert("ada".to_owned(), tilo_action_policy::Role::Viewer);
        let policy = Arc::new(policy);
        let access = |app: &str| {
            (app == NOTES)
                .then(|| tilo_action_policy::MemberAccess::person(Arc::clone(&policy), "ada"))
        };
        processes.policy_changed(
            WORKSPACE,
            &[AppAccess {
                app: NOTES.to_owned(),
                access: access(NOTES),
            }],
        );
        let handed = ToProcess::receive(&theirs).unwrap();
        let Some(ToProcess::Access(said)) = handed else {
            panic!("the workspace's process is told the person's access: {handed:?}");
        };
        assert_eq!(said.app, NOTES);
        // What a launched Notes reads of the person's authority, as it reads back off the socket.
        let expected = access(NOTES).unwrap();
        let expected = serde_json::from_slice(&serde_json::to_vec(&expected).unwrap()).unwrap();
        assert_eq!(said.access, Some(expected));

        // Each was said once, however many of its windows are open, and a process of another
        // workspace or of no workspace heard none of it.
        drop(processes);
        for (socket, what) in [
            (&theirs, "the workspace's own process"),
            (&other_theirs, "a process of another workspace"),
            (&build_theirs, "a process where nothing changed"),
        ] {
            assert!(
                ToProcess::receive(socket).unwrap().is_none(),
                "{what} is told once, or not at all"
            );
        }
    }

    /// A process whose last window closed runs on, so the app opened again finds it, and ends only
    /// once nothing called it for [`IDLE`]; a process still running a call is never ended as idle.
    #[test]
    fn a_process_outlives_its_last_window_until_it_is_idle() {
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-0000000000a1";
        const BUSY: &str = "7a110d00-0000-4000-8000-0000000000a2";
        let processes = processes(std::env::temp_dir());
        let (ours, theirs) = runtime_portal::process_link::pair().unwrap();
        let (busy_ours, busy_theirs) = runtime_portal::process_link::pair().unwrap();
        for (workspace, socket) in [(WORKSPACE, ours), (BUSY, busy_ours)] {
            processes.running().insert(
                key(workspace, NOTES),
                Host {
                    socket: Arc::new(socket),
                    instances: HashMap::from([(7, NOTES.to_owned())]),
                    generation: 1,
                    builds: HashMap::new(),
                    idle_since: None,
                },
            );
        }
        let running = |workspace: &str| {
            processes
                .instances()
                .into_iter()
                .any(|running| running.workspace == workspace && running.app == NOTES)
        };

        processes.ended(&key(WORKSPACE, NOTES), 1, 7);
        processes.close_idle(Instant::now());
        assert!(
            running(WORKSPACE),
            "its last window closed, and it still runs"
        );

        processes.close_idle(Instant::now() + IDLE);
        assert!(!running(WORKSPACE), "idle for long enough, it is closed");
        assert!(
            matches!(
                ToProcess::receive(&theirs).unwrap(),
                Some(ToProcess::Finish)
            ),
            "the process is told nothing more comes, and ends"
        );
        assert!(running(BUSY), "a process running a window is never idle");
        drop(busy_theirs);
    }

    /// A process that ended without saying why is reported by how it ended and what it last wrote.
    #[test]
    fn a_silent_hosts_end_is_told_as_the_session_saw_it() {
        use std::os::unix::process::ExitStatusExt as _;
        let killed = Ok(std::process::ExitStatus::from_raw(libc::SIGKILL));
        let said = VecDeque::from(vec![
            "loading the module".to_owned(),
            "out of memory".to_owned(),
        ]);
        let told = seen_end(&killed, &said);
        assert!(told.contains("signal 9 (SIGKILL"), "{told}");
        assert!(
            told.ends_with("loading the module | out of memory"),
            "{told}"
        );
        let exited = Ok(std::process::ExitStatus::from_raw(3 << 8));
        assert_eq!(
            seen_end(&exited, &VecDeque::new()),
            "it exited with status 3 without saying why; it wrote no error output"
        );
    }

    /// Where [`a_task_host_reads_until_finished`] says what its link carried.
    #[cfg(target_os = "linux")]
    const TASK_HANDED_ENV: &str = "TILO_TEST_TASK_HANDED";

    /// Run as the task's host the test launcher starts: read the link until the device says
    /// nothing more comes, write what came, and end. Outside that, nothing to do.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_task_host_reads_until_finished() {
        use std::os::fd::AsFd as _;
        let Some(handed) = std::env::var_os(TASK_HANDED_ENV) else {
            return;
        };
        let link = std::io::stdin().as_fd().try_clone_to_owned().unwrap();
        let mut came = Vec::new();
        loop {
            match ToProcess::receive(&link).unwrap() {
                Some(ToProcess::Finish) | None => break,
                Some(ToProcess::Task { request, .. }) => came.push(request.task),
                Some(other) => panic!("a task's host is handed its task alone: {other:?}"),
            }
        }
        std::fs::write(handed, came.join("\n")).unwrap();
    }

    /// An agent task's host is started as every tilolang process here is, through the launcher's
    /// tilolang process kind, and granted less than an app's: its own storage, and nothing the
    /// workspace's apps kept on this device. It is handed its task and nothing after it, so it
    /// ends with the task, and the device follows it until it has: nothing else keeps it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_tasks_host_starts_through_the_launcher_granted_less_than_an_app() {
        use std::os::unix::fs::PermissionsExt as _;
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-0000000000a1";
        let home = tempfile::tempdir().expect("a device's home");
        // A launcher that says what it was asked to start, then runs as the host: it reads its
        // link until the device says nothing more comes ([`a_task_host_reads_until_finished`]).
        let asked = home.path().join("asked");
        let handed = home.path().join("handed");
        let launcher = home.path().join("tilo-launcher");
        std::fs::write(
            &launcher,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n\
                 {}='{}' exec '{}' device::processes::tests::a_task_host_reads_until_finished \
                 --exact --test-threads 1\n",
                asked.display(),
                TASK_HANDED_ENV,
                handed.display(),
                std::env::current_exe().unwrap().display(),
            ),
        )
        .expect("the launcher is written");
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))
            .expect("the launcher runs");
        let processes = Processes::with(
            home.path().to_path_buf(),
            launcher,
            Box::new(|_, _| Err("no app is started here".to_owned())),
        );

        let (_api, program) = std::os::unix::net::UnixStream::pair().expect("a program link");
        let apps = tilo_render_protocol::shm::sealed_blob("tilolang-apps", b"[]")
            .expect("the task's apps");
        processes
            .start_task(
                TaskRequest {
                    task: "run-1".to_owned(),
                    workspace: WORKSPACE.to_owned(),
                    person: Some("ada".to_owned()),
                    apps_len: 2,
                    documents_under: std::collections::BTreeMap::new(),
                    changed_at_document: std::collections::BTreeSet::new(),
                },
                TaskFds {
                    program: program.into(),
                    apps,
                },
                (
                    "tilolang/0.1.0+tilolang".to_owned(),
                    "tilo-runtime/2026.10.1".to_owned(),
                ),
            )
            .expect("the task's host starts");
        assert!(
            processes.instances().is_empty(),
            "a task's host runs no app's instance"
        );

        // It ends once it read its task: the device said nothing more. Its follow forgets it.
        let ends_by = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !processes.running().is_empty() {
            assert!(
                std::time::Instant::now() < ends_by,
                "the task's host outlived its task"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let asked = std::fs::read_to_string(&asked).expect("the launcher was asked");
        let storage = home
            .path()
            .join("tilo/tilolang-processes")
            .join(WORKSPACE)
            .join(TASK_STORAGE);
        assert_eq!(
            asked.lines().collect::<Vec<_>>(),
            [
                launcher::run::tilolang_process::SUBCOMMAND,
                "--runtime",
                "tilolang/0.1.0+tilolang",
                "--app-runtime",
                "tilo-runtime/2026.10.1",
                "--storage",
                &storage.display().to_string(),
            ],
            "the launcher's tilolang process, granted its own storage and nothing an app kept"
        );
        assert_eq!(
            std::fs::read_to_string(&handed).expect("the link was read"),
            "run-1",
            "the task was handed over the link, and nothing after it"
        );
    }

    /// A view shown on a web page gets a window of its own, started through the launcher's
    /// tilolang process on the runtimes the app's package names, granted none of what the app
    /// kept, and holding the view's patch connection and the page's link. The device answers its
    /// end of that link, the window answers on it, and closing it ends the window.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_page_window_starts_through_the_launcher_and_ends_with_its_link() {
        use std::io::{Read as _, Write as _};
        use std::os::unix::fs::PermissionsExt as _;
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-0000000000a2";
        let home = tempfile::tempdir().expect("a device's home");
        // A launcher that says what it was asked to start, then stands in for the window: it
        // answers on the view's patch connection and on the page's link, and reads the link
        // until the device lets go of it.
        let asked = home.path().join("asked");
        let launcher = home.path().join("tilo-launcher");
        std::fs::write(
            &launcher,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n[ -S /proc/self/fd/3 ] && [ -S /proc/self/fd/4 ] || exit 3\nprintf patch >&3\nprintf drawn >&4\ncat <&4 >/dev/null\n",
                asked.display()
            ),
        )
        .expect("the launcher is written");
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))
            .expect("the launcher runs");
        let opened = Arc::new(Mutex::new(Vec::new()));
        let processes = Processes::recording(
            home.path().to_path_buf(),
            launcher,
            Box::new(|_, app| {
                Ok(Verified {
                    runtime: "tilolang/0.1.0+tilolang".to_owned(),
                    app_runtime: format!("tilo-runtime/2026.10.1+{app}"),
                    module: runtime_portal::process_link::Module::default(),
                })
            }),
            {
                let opened = Arc::clone(&opened);
                Box::new(move |app: &str| opened.lock().unwrap().push(app.to_owned()))
            },
        );

        let (patch, mut instance) =
            std::os::unix::net::UnixStream::pair().expect("a patch connection");
        let (link, page) = std::os::unix::net::UnixStream::pair().expect("a page link");
        let mut link = processes
            .show_page(
                NOTES,
                WORKSPACE,
                crate::apps::OpenedView {
                    patch: patch.into(),
                    page: page.into(),
                    link,
                },
            )
            .expect("the page's window starts");
        let mut answered = [0; 5];
        link.read_exact(&mut answered)
            .expect("the window answers its page");
        assert_eq!(&answered, b"drawn");
        assert_eq!(
            *opened.lock().unwrap(),
            [NOTES],
            "starting the app marks it used in the cache, once"
        );
        instance
            .read_exact(&mut answered)
            .expect("the window holds the view's patch connection");
        assert_eq!(&answered, b"patch");
        assert!(
            processes.instances().is_empty(),
            "a page window runs no instance of its own"
        );
        assert_eq!(
            processes.running().len(),
            1,
            "it is followed as every process here"
        );

        // The device lets go of the page: the window ends, and its follow forgets it.
        link.write_all(b"bye").expect("the link is open");
        drop(link);
        let ends_by = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !processes.running().is_empty() {
            assert!(
                std::time::Instant::now() < ends_by,
                "the page's window outlived its link"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let asked = std::fs::read_to_string(&asked).expect("the launcher was asked");
        let storage = home
            .path()
            .join("tilo/tilolang-processes")
            .join(WORKSPACE)
            .join(PAGE_STORAGE);
        assert_eq!(
            asked.lines().collect::<Vec<_>>(),
            [
                launcher::run::tilolang_process::SUBCOMMAND,
                "--runtime",
                "tilolang/0.1.0+tilolang",
                "--app-runtime",
                "tilo-runtime/2026.10.1+so.tilo.notes",
                "--storage",
                &storage.display().to_string(),
                "--page",
            ],
            "the launcher's tilolang process on the app's runtimes, granted nothing more"
        );
    }

    #[test]
    fn last_lines_cuts_an_overlong_line_and_keeps_the_line_after_it() {
        /// Streams `remaining` copies of `b'a'`, then `tail`, in 4 KiB chunks,
        /// with an `Interrupted` error before the first chunk.
        struct HugeThenShort {
            remaining: usize,
            tail: &'static [u8],
            interrupted: bool,
        }

        impl std::io::Read for HugeThenShort {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                if self.remaining > 0 {
                    let count = buf.len().min(4096).min(self.remaining);
                    if let Some(slot) = buf.get_mut(..count) {
                        slot.fill(b'a');
                    }
                    self.remaining = self.remaining.saturating_sub(count);
                    return Ok(count);
                }
                if self.tail.is_empty() {
                    return Ok(0);
                }
                let count = buf.len().min(self.tail.len());
                if let (Some(dst), Some(src)) = (buf.get_mut(..count), self.tail.get(..count)) {
                    dst.copy_from_slice(src);
                }
                self.tail = self.tail.get(count..).unwrap_or_default();
                Ok(count)
            }
        }

        const BOUND: usize = wire_frame::LINE_BYTES;
        const MARK: &str = "… [truncated]";
        let reader = HugeThenShort {
            remaining: 1024 * 1024,
            tail: b"\nshort\n",
            interrupted: false,
        };
        let kept = last_lines(reader, "ws app");
        assert_eq!(kept.len(), 2, "kept both the cut line and the short line");
        for line in &kept {
            assert!(
                line.len() <= BOUND.saturating_add(MARK.len()),
                "kept line length {} exceeds bound + mark",
                line.len()
            );
        }
        assert_eq!(
            kept.front().map(String::as_str),
            Some(format!("{}{MARK}", "a".repeat(BOUND)).as_str())
        );
        assert_eq!(kept.back().map(String::as_str), Some("short"));
    }

    #[test]
    fn last_lines_keeps_reading_past_non_utf8_bytes() {
        let reader = std::io::Cursor::new(b"bad \xff\xfe bytes\nafter\n".to_vec());
        let kept = last_lines(reader, "ws app");
        assert_eq!(kept.len(), 2, "non-UTF-8 must not stop reading: {kept:?}");
        assert_eq!(
            kept.front().map(String::as_str),
            Some("bad \u{FFFD}\u{FFFD} bytes")
        );
        assert_eq!(kept.back().map(String::as_str), Some("after"));
    }

    /// On a Mac, a host runs the tilolang runtime's binary from the installed runtime package in
    /// its sandbox: it keeps what it keeps in its own storage, and reads nothing another app's host
    /// keeps beside it. The runtime here is a probe that tries both and says which it could.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_macs_host_keeps_its_own_storage_and_reads_no_other_apps() {
        use std::process::Command;
        use tilo_test_env::SpawnUnderEnvLock as _;
        const PROBE: &str = r#"
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int main(void) {
    const char *own = getenv("TILO_TILOLANG_PROCESS_STORAGE");
    if (!own) return 3;
    char path[4096];
    snprintf(path, sizeof path, "%s/kept", own);
    int kept = open(path, O_CREAT | O_WRONLY, 0600);
    if (kept < 0 || write(kept, "kept", 4) != 4) return 4;
    close(kept);
    snprintf(path, sizeof path, "%s/../so.tilo.mail/secret", own);
    if (open(path, O_RDONLY) >= 0) return 5;
    return 0;
}
"#;
        let temp = tempfile::tempdir().unwrap();
        // Seatbelt sees resolved paths (`/var` is `/private/var`).
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let source = root.join("probe.c");
        std::fs::write(&source, PROBE).unwrap();
        let payload = root.join("runtime");
        let binary = payload.join(launcher::manifest::TILOLANG_BIN);
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        for command in [
            Command::new("cc")
                .arg("-O0")
                .arg("-o")
                .arg(&binary)
                .arg(&source)
                .status_locked(),
            Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", "-"])
                .arg(&binary)
                .status_locked(),
        ] {
            assert!(command.unwrap().success());
        }
        let identity = tilolang_cli::identity_at(&root.join("publisher.json")).unwrap();
        let manifest = format!(
            r#"[package]
id = "so.tilo.runtime.tilolang"
name = "tilolang runtime"
category = "System"
version = "0.1.0"
kind = "runtime"
provides = "tilolang/1"
platforms = ["macos"]
platform-reason = "Its binaries are Mac binaries."

[package.isa]
aarch64 = "aarch64"

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
type = "cli"
bin = "{}"
"#,
            hex::encode(identity.public_key_bytes()),
            launcher::manifest::TILOLANG_BIN,
        );
        let manifest = tilo_app_package::manifest::Manifest::from_toml_str(&manifest).unwrap();
        let packed = tilo_app_package::pack::pack(&payload, &manifest, &identity).unwrap();
        let tapp = root.join("runtime.tapp");
        tilo_app_package::tapp::write_file(
            &tapp,
            &packed.meta,
            Some(&packed as &dyn tilo_app_package::BlobSource),
            tilo_app_package::tapp::WriteOptions::replace(),
        )
        .unwrap();
        let store = root.join("store");
        package_store::PackageStore::at(&store)
            .install(
                std::io::BufReader::new(std::fs::File::open(&tapp).unwrap()),
                |_| {
                    Ok(package_store::Admission {
                        pinned: false,
                        attested_by: String::new(),
                        source: "a test".to_owned(),
                        held: false,
                    })
                },
                None,
            )
            .unwrap();

        // Mail's host kept something; Notes' host starts beside it.
        let processes = root.join("tilo/tilolang-processes/workspace");
        let mail = processes.join("so.tilo.mail");
        std::fs::create_dir_all(&mail).unwrap();
        std::fs::write(mail.join("secret"), "mail's").unwrap();
        let storage = processes.join(NOTES);
        std::fs::create_dir_all(&storage).unwrap();
        let mut key = key("workspace", NOTES);
        key.runtime = "tilolang/1".to_owned();
        let (_link, started) = spawn_host(
            &package_store::Catalog::at(&store, None),
            &key,
            &storage,
            None,
        )
        .expect("the host starts from the installed runtime");
        let mut said = String::new();
        if let Some(mut stderr) = started.stderr {
            let _ = std::io::Read::read_to_string(&mut stderr, &mut said);
        }
        let status = (started.wait)().unwrap();
        assert_eq!(
            status.code(),
            Some(0),
            "the host keeps its own storage (4 if not) and reads no other app's (5 if it does): \
             {said}"
        );
        assert_eq!(
            std::fs::read_to_string(storage.join("kept")).unwrap(),
            "kept"
        );
    }
}
