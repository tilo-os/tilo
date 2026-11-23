//! Tilolang processes: one process per workspace and app, which runs that app's code in that
//! workspace, each view's call in it (`docs/designs/app-language/app-window.md`).
//!
//! The device holds them, as it holds the rest of its work; the desktop session is a client that
//! hands each window's instance over, and a device with no desktop starts them the same way.
//!
//! A tilolang app's window is a launched process of its own, with its own launch identity for its
//! surface and accessibility, started as its window alone (no `--tilolang-process`: the launcher gives
//! it no collaboration connection). The session hands each window's instance to the process of its
//! workspace and app ([`Processes::hand_over`]):
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
//! "tilolang process", `tilo-launcher tilolang-process`), keyed by its workspace, its app and the
//! runtimes it runs on, with the storage of that workspace and app; it ends once the last of its
//! calls ended, which it says on its socket.
//!
//! In a development session (`TILO_LAUNCHER_LIVE_MODULES`), a reinstall of an app a process runs is
//! verified as a launch verifies it and handed to the process on its socket ([`Processes::reinstalled`]):
//! its open windows take the new build over without restarting, as `tilolang dev` expects.
//!
//! Every call opens what it works on from the workspace's committed read, which is the one store:
//! a process keeps no copy of its own on disk.

use std::collections::{HashMap, VecDeque};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use launcher::run::tilolang_process::VerifiedInstance;
use runtime_portal::process_link::{
    AppBuild, FromProcess, InstanceFds, InstanceRequest, OpenedFile, Packet as _, TaskFds,
    TaskRequest, ToProcess,
};

use super::package_runs::{installed_apps, runs_in_process, verified_installed};

/// Hand the process behind `socket` the instance `request` with its connections, keeping them
/// to hand to another process should this one have ended.
fn send_instance(
    socket: &OwnedFd,
    request: &InstanceRequest,
    fds: &InstanceFds,
) -> std::io::Result<()> {
    ToProcess::Instance {
        request: Box::new(request.clone()),
        fds: fds.try_clone()?,
    }
    .send(socket)
}

/// The launcher's flag for a window whose instance a tilolang process runs.
pub const PROCESS_ARG: &str = "--tilolang-process";

/// The app that keeps each person's own views (`apps/tl/views`): views they, their agents or
/// others wrote, which read whatever the workspace has installed.
const PERSON_VIEWS_APP: &str = "so.tilo.lang.views";

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
    /// The session's end of its socket.
    socket: Arc<OwnedFd>,
    /// The windows whose instances it runs, each with its app.
    instances: HashMap<u64, String>,
    /// Which start of the process this is, so the end of an earlier one never retires a later one.
    generation: u64,
    /// The build of each app it runs, the one it was handed last.
    builds: HashMap<String, Build>,
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
    pub patch: Option<OwnedFd>,
    /// The instance's collaboration connection, attested as that window, or as an instance that
    /// shows nothing.
    pub collaboration: OwnedFd,
    /// Its identity-service connection, named for the app and workspace.
    pub identity: OwnedFd,
    /// Its connection to the app's origin connector, when the app has one.
    pub origin: Option<OwnedFd>,
    /// Its connection to the device's content store, scoped to the app in its workspace: the
    /// files a person chooses, the pictures its document holds.
    pub content: Option<OwnedFd>,
    /// Its connection to the person's chooser, when this session serves one.
    pub chooser: Option<OwnedFd>,
    /// Its connection to the person's clipboard, when this session serves one.
    pub clipboard: Option<OwnedFd>,
    /// The record the window was asked to show, by its id.
    pub document: Option<String>,
    /// The item the window opens on, by its anchor.
    pub reveal: Option<String>,
    /// The file the window opens, as content the instance reads from the content store.
    pub open: Option<OpenedFile>,
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

/// How an installed app is verified for a process: the workspace it is installed in, and the app.
type Verifies = dyn Fn(&str, &str) -> Result<VerifiedInstance, String> + Send + Sync;

/// The tilolang processes this device runs, by workspace, app and runtimes.
pub struct Processes {
    /// Where processes keep what their instances keep, and where windows kept it before.
    data_home: PathBuf,
    /// The platform's launcher, which starts each process in its sandbox ([`spawn_host`]).
    launcher: PathBuf,
    verifies: Box<Verifies>,
    running: Mutex<HashMap<HostKey, Host>>,
    generations: AtomicU64,
    /// Whether the app store is watched for reinstalls.
    follows_reinstalls: AtomicBool,
}

impl Processes {
    /// The processes this device starts through `launcher`, keeping what they keep under the
    /// person's data home.
    #[must_use]
    pub fn new(launcher: PathBuf) -> Arc<Self> {
        let data_home =
            tilo_paths::xdg::data_home().unwrap_or_else(|| PathBuf::from("/nonexistent"));
        Self::with(data_home, launcher, Box::new(verified_installed))
    }

    /// Processes over `data_home`, started through `launcher` and running what `verifies` verified.
    #[must_use]
    pub fn with(data_home: PathBuf, launcher: PathBuf, verifies: Box<Verifies>) -> Arc<Self> {
        Arc::new(Self {
            data_home,
            launcher,
            verifies,
            running: Mutex::new(HashMap::new()),
            generations: AtomicU64::new(1),
            follows_reinstalls: AtomicBool::new(false),
        })
    }

    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<HostKey, Host>> {
        self.running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The processes running now, each by its workspace and app, with the calls it runs.
    #[must_use]
    pub fn instances(&self) -> Vec<(String, String, Vec<u64>)> {
        let mut processes: Vec<_> = self
            .running()
            .iter()
            .filter(|(key, _)| key.alone.is_none())
            .map(|(key, process)| {
                let mut instances: Vec<u64> = process.instances.keys().copied().collect();
                instances.sort_unstable();
                (key.workspace.clone(), key.app.clone(), instances)
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

    /// Say on every process running in `workspace` what the person may now do in each app it runs,
    /// `access` of the app, from the workspace's policy as the caller holds it now: a role given
    /// or taken back enables or disables what the windows already open offer. A process started
    /// later is handed it with each instance.
    pub fn policy_changed(
        &self,
        workspace: &str,
        access: impl Fn(&str) -> Option<tilo_action_policy::MemberAccess>,
    ) {
        for (key, process) in self.running().iter() {
            if key.workspace != workspace || key.alone.is_some() {
                continue;
            }
            let app = key.app.clone();
            let access = runtime_portal::process_link::AppAccess {
                access: access(&app),
                app: app.clone(),
            };
            if let Err(error) = ToProcess::Access(access).send(&process.socket) {
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

    /// Hand `instance` to the process of its workspace and app, starting the process when none runs.
    /// Done off the caller's thread: it verifies the package and may start the process.
    pub fn hand_over(self: &Arc<Self>, instance: Instance) {
        let processes = Arc::clone(self);
        let started = std::thread::Builder::new()
            .name("tilolang-handover".into())
            .spawn(move || {
                let (app, window) = (instance.app.clone(), instance.instance);
                if let Err(error) = processes.hand_over_now(instance) {
                    tracing::error!(
                        %error,
                        app,
                        instance = window,
                        "a tilolang window's instance was not handed to its process"
                    );
                }
            });
        if let Err(error) = started {
            tracing::error!(%error, "a tilolang window's instance could not be handed over");
        }
    }

    /// [`Self::hand_over`] on this thread. Whether it started the process. An [`Instance`] with no
    /// patch shows nothing: the host runs the app's code for the call and draws nothing, till the
    /// session ends it.
    ///
    /// # Errors
    /// The app does not verify, its module cannot be sealed, or no process takes the instance.
    pub fn hand_over_now(self: &Arc<Self>, instance: Instance) -> std::io::Result<bool> {
        let refused =
            |what: String| std::io::Error::new(std::io::ErrorKind::PermissionDenied, what);
        let began = std::time::Instant::now();
        let verified = (self.verifies)(&instance.workspace, &instance.app).map_err(refused)?;
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
            link_home: instance.link_home.clone(),
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
        socket: &OwnedFd,
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
        // SAFETY: shutting down the writing half of a socket this device owns.
        let _ = unsafe { libc::shutdown(process.socket.as_raw_fd(), libc::SHUT_WR) };
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
    ) -> std::io::Result<std::os::unix::net::UnixStream> {
        let verified = (self.verifies)(workspace, app).map_err(std::io::Error::other)?;
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

    /// What an earlier version of the app of `key` kept on this device, in the store its windows
    /// and the native app it replaced wrote: the directory of that app's documents in the
    /// workspace, and where it lies beneath that store's root. `None` where it kept nothing.
    fn earlier(&self, key: &HostKey) -> Option<(PathBuf, PathBuf)> {
        if key.alone.is_some() {
            return None;
        }
        let root = launcher::appdir::host_app_dir(
            &self.data_home,
            &launcher::appdir::workspace_slug(Some(&key.workspace)),
            &key.app,
        )
        .join(".local/share");
        let dir = app_state::workspace_index_directory(&root, &key.app, &key.workspace).ok()?;
        let under = dir.strip_prefix(&root).ok()?.to_path_buf();
        dir.is_dir().then_some((dir, under))
    }

    /// Start the process `key`, with its socket at the launch kind's descriptor (a `page` window
    /// with its patch connection and page link besides), and follow it on a thread of its own
    /// until it ends.
    fn start(
        self: &Arc<Self>,
        key: &HostKey,
        page: Option<(OwnedFd, OwnedFd)>,
    ) -> std::io::Result<Host> {
        let storage = self.storage(key)?;
        let earlier = self.earlier(key);
        let (ours, mut child) = spawn_host(
            &self.launcher,
            key,
            &storage,
            earlier
                .as_ref()
                .map(|(dir, under)| (dir.as_path(), under.as_path())),
            page,
        )?;
        let tag = format!("{} {}", key.workspace, key.app);
        let said = child.stderr.take().map(|stderr| {
            std::thread::Builder::new()
                .name("tilolang-process-err".into())
                .spawn(move || last_lines(stderr, &tag))
        });
        let generation = self.generations.fetch_add(1, Ordering::Relaxed);
        let socket = Arc::new(ours);
        tracing::info!(
            workspace = %key.workspace,
            app = %key.app,
            pid = child.id(),
            "tilolang process started"
        );
        let (processes, followed, key) = (Arc::clone(self), Arc::clone(&socket), key.clone());
        std::thread::Builder::new()
            .name("tilolang-process".into())
            .spawn(move || processes.follow(&key, generation, &followed, child, said))?;
        Ok(Host {
            socket,
            instances: HashMap::new(),
            generation,
            builds: HashMap::new(),
        })
    }

    /// Follow the process `key` of `generation` over its socket `followed` until it ends: each
    /// instance that ends is noted, then the process's own end and why, and the process is forgotten.
    fn follow(
        &self,
        key: &HostKey,
        generation: u64,
        followed: &OwnedFd,
        mut child: std::process::Child,
        said: Said,
    ) {
        // Why the process says it exits; none when it ended without saying (killed, crashed).
        let mut reason: Option<String> = None;
        loop {
            match FromProcess::receive(followed) {
                Ok(Some(FromProcess::Ended(instance))) => self.ended(key, generation, instance),
                Ok(Some(FromProcess::Exiting(why))) => reason = Some(why),
                Ok(Some(FromProcess::Serves)) => {}
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
        let status = child.wait();
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
    /// Once its last instance ended it is closed: it ends when it has written what it keeps.
    fn ended(&self, key: &HostKey, generation: u64, instance: u64) {
        let closed = {
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
            if process.instances.is_empty() {
                processes.remove(key)
            } else {
                None
            }
        };
        if let Some(process) = closed {
            // The process reads the end of its socket and ends once it runs nothing.
            // SAFETY: shutting down the writing half of a socket the session owns.
            let _ = unsafe { libc::shutdown(process.socket.as_raw_fd(), libc::SHUT_WR) };
        }
    }
}

/// Start the tilolang host `key`, keeping `storage` and reading what an earlier version of its app
/// kept (the directory, and where the host sees it beneath its mount), on its process link: the
/// one place a host is started, and the only one that names the platform. On Linux the launcher
/// is `tilo-launcher tilolang-process`, which takes the link off its stdin and starts the host in
/// its sandbox; a `page` window's patch connection and page link go to it as descriptors 3 and 4,
/// and it keeps the link while the window runs.
fn spawn_host(
    launcher: &Path,
    key: &HostKey,
    storage: &Path,
    earlier: Option<(&Path, &Path)>,
    page: Option<(OwnedFd, OwnedFd)>,
) -> std::io::Result<(OwnedFd, std::process::Child)> {
    use std::os::unix::process::CommandExt as _;
    let mut command = std::process::Command::new(launcher);
    command
        .arg(launcher::run::tilolang_process::SUBCOMMAND)
        .arg("--runtime")
        .arg(&key.runtime)
        .arg("--app-runtime")
        .arg(&key.app_runtime)
        .arg("--storage")
        .arg(storage);
    if let Some((dir, under)) = earlier {
        command
            .arg("--earlier")
            .arg(dir)
            .arg("--earlier-under")
            .arg(under);
    }
    if let Some((patch, page)) = &page {
        command.arg("--page");
        let handed = [(patch.as_raw_fd(), 3), (page.as_raw_fd(), 4)];
        // SAFETY: only async-signal-safe calls between fork and exec: each descriptor is moved
        // above both slots first, so neither placement overwrites the other, then placed without
        // close-on-exec.
        unsafe {
            command.pre_exec(move || {
                let mut moved = [(0, 0); 2];
                for (slot, (from, to)) in moved.iter_mut().zip(handed) {
                    let fd = libc::fcntl(from, libc::F_DUPFD_CLOEXEC, 16);
                    if fd < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    *slot = (fd, to);
                }
                for (from, to) in moved {
                    if libc::dup2(from, to) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
    // Read here: what it said last is why it ended when it could not say so itself.
    command.stderr(std::process::Stdio::piped());
    let spawned = runtime_portal::process_link::spawn(command);
    drop(page);
    spawned
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

/// Relay each line `stderr` says to the session's log under `tag`, and answer the last ones once
/// it ends.
fn last_lines(stderr: impl std::io::Read, tag: &str) -> VecDeque<String> {
    use std::io::BufRead as _;
    let mut last = VecDeque::with_capacity(SAID_LINES);
    for line in std::io::BufReader::new(stderr).lines() {
        let Ok(line) = line else {
            break;
        };
        tracing::info!(target: tilo_startup_metrics::APP_OUTPUT_TARGET, "[tilolang-process {tag}] {line}");
        if last.len() == SAID_LINES {
            last.pop_front();
        }
        last.push_back(line);
    }
    last
}

/// Why a process ended that did not say so itself: how it ended, as the session saw it, and the last
/// lines of its error output.
fn seen_end(status: &std::io::Result<std::process::ExitStatus>, last: &VecDeque<String>) -> String {
    use std::os::unix::process::ExitStatusExt as _;
    let how = match status {
        Ok(status) => match (status.code(), status.signal()) {
            (Some(code), _) => format!("it exited with status {code} without saying why"),
            (None, Some(signal)) => {
                let name = match signal {
                    libc::SIGTERM => " (SIGTERM)",
                    libc::SIGKILL => " (SIGKILL: killed, as memory pressure kills)",
                    libc::SIGSEGV | libc::SIGBUS | libc::SIGILL => " (it crashed)",
                    libc::SIGABRT => " (SIGABRT: it aborted)",
                    libc::SIGSYS => " (SIGSYS: its sandbox refused a system call)",
                    _ => "",
                };
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

    const NOTES: &str = "so.tilo.notes";

    /// The dirs of `app`'s documents of `workspace` in the store a device's home keeps apps' own
    /// data in, made: where an earlier version of the app wrote what a conversion reads.
    fn earlier_documents(home: &Path, workspace: &str, app: &str) -> PathBuf {
        let root = launcher::appdir::host_app_dir(
            home,
            &launcher::appdir::workspace_slug(Some(workspace)),
            app,
        )
        .join(".local/share");
        let directory = app_state::workspace_index_directory(&root, app, workspace)
            .expect("the app's directory of this workspace");
        std::fs::create_dir_all(directory.join("documents")).expect("the app's directory is made");
        directory
    }

    fn key(workspace: &str, app: &str) -> HostKey {
        HostKey {
            workspace: workspace.to_owned(),
            app: app.to_owned(),
            alone: None,
            runtime: "tilolang@1".to_owned(),
            app_runtime: "tilo@1".to_owned(),
        }
    }

    fn processes(data_home: PathBuf) -> Arc<Processes> {
        Processes::with(
            data_home,
            PathBuf::from("/nonexistent"),
            Box::new(|_, _| Err("no package store here".to_owned())),
        )
    }

    /// A process reads what an earlier version of its app kept of the person's workspace, and
    /// nothing of another workspace's or another app's: the session grants the one directory.
    #[test]
    fn a_process_reads_only_its_apps_documents_of_the_persons_workspace() {
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-0000000000e1";
        const ELSEWHERE: &str = "7a110d00-0000-4000-8000-0000000000e2";
        const ANOTHER_APP: &str = "so.tilo.chat";
        let home = tempfile::tempdir().expect("a device's home");
        let processes = processes(home.path().to_path_buf());
        assert!(
            processes.earlier(&key(WORKSPACE, NOTES)).is_none(),
            "nothing is granted where the app kept nothing"
        );

        let kept = earlier_documents(home.path(), WORKSPACE, NOTES);
        let elsewhere = earlier_documents(home.path(), ELSEWHERE, NOTES);
        let another = earlier_documents(home.path(), WORKSPACE, ANOTHER_APP);
        let (granted, under) = processes
            .earlier(&key(WORKSPACE, NOTES))
            .expect("the app kept something in this workspace");
        assert_eq!(granted, kept, "its app's documents of its own workspace");
        assert_eq!(
            under,
            Path::new(NOTES).join("workspaces").join(WORKSPACE),
            "located beneath the store's own root"
        );
        for other in [&elsewhere, &another] {
            assert!(
                !other.starts_with(&granted) && !granted.starts_with(other),
                "{} is no part of what the process reads",
                other.display()
            );
        }
        assert!(
            processes.earlier(&key(WORKSPACE, "so.tilo.mail")).is_none(),
            "another app's process is granted none of it"
        );
        assert!(
            processes
                .earlier(&key("7a110d00-0000-4000-8000-0000000000e3", NOTES))
                .is_none(),
            "nor is Notes' process of another workspace"
        );
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
            PathBuf::from("/nonexistent"),
            Box::new(move |_, _| {
                Ok(VerifiedInstance {
                    runtime: "tilolang@1".to_owned(),
                    app_runtime: "tilo@1".to_owned(),
                    publisher: [7; 32],
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
        processes.policy_changed(WORKSPACE, access);
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

    /// An agent task's host is started as every tilolang process here is, through the launcher's
    /// tilolang process kind, and granted less than an app's: its own storage, and nothing the
    /// workspace's apps kept on this device. It is handed its task and nothing after it, so it
    /// ends with the task, and the device follows it until it has: nothing else keeps it.
    #[test]
    fn a_tasks_host_starts_through_the_launcher_granted_less_than_an_app() {
        use std::os::unix::fs::PermissionsExt as _;
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-0000000000a1";
        let home = tempfile::tempdir().expect("a device's home");
        // What Notes kept of the workspace, which only Notes' own process is granted.
        let kept = earlier_documents(home.path(), WORKSPACE, NOTES);
        // A launcher that says what it was asked to start, and holds the link it was handed
        // until the device lets go of it.
        let asked = home.path().join("asked");
        let handed = home.path().join("handed");
        let launcher = home.path().join("tilo-launcher");
        std::fs::write(
            &launcher,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexec cat > '{}'\n",
                asked.display(),
                handed.display()
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
        assert!(
            !asked.contains(&kept.display().to_string()),
            "nothing Notes kept is granted to a task's host"
        );
        assert!(
            std::fs::metadata(&handed).expect("the link was read").len() > 0,
            "the task was handed over the link"
        );
    }

    /// A view shown on a web page gets a window of its own, started through the launcher's
    /// tilolang process on the runtimes the app's package names, granted none of what the app
    /// kept, and holding the view's patch connection and the page's link. The device answers its
    /// end of that link, the window answers on it, and closing it ends the window.
    #[test]
    fn a_page_window_starts_through_the_launcher_and_ends_with_its_link() {
        use std::io::{Read as _, Write as _};
        use std::os::unix::fs::PermissionsExt as _;
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-0000000000a2";
        let home = tempfile::tempdir().expect("a device's home");
        let kept = earlier_documents(home.path(), WORKSPACE, NOTES);
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
        let processes = Processes::with(
            home.path().to_path_buf(),
            launcher,
            Box::new(|_, app| {
                Ok(VerifiedInstance {
                    runtime: "tilolang/0.1.0+tilolang".to_owned(),
                    app_runtime: format!("tilo-runtime/2026.10.1+{app}"),
                    publisher: [0; 32],
                    module: runtime_portal::process_link::Module::default(),
                })
            }),
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
            "the launcher's tilolang process on the app's runtimes, granted nothing the app kept"
        );
        assert!(!asked.contains(&kept.display().to_string()));
    }
}
