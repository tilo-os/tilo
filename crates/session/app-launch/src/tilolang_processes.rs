//! Tilolang processes: one process per workspace and app, which runs that app's code in that
//! workspace, each view's call in it (`docs/designs/app-language/app-window.md`).
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
//! A process is started through the launch kind "tilolang process" (`tilo-launcher
//! tilolang-process`), keyed by its workspace, its app and the runtimes it runs on, with the
//! storage of that workspace and app; it ends once the last of its calls ended, which it says on
//! its socket.
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
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use launcher::run::tilolang_process::VerifiedInstance;
use runtime_portal::process_link::{
    AppBuild, FromProcess, InstanceFds, InstanceRequest, OpenedFile, Packet as _, ToProcess,
};

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

/// Whether the installed app `app` is a tilolang app: its manifest's entrypoint runs a tilolang
/// runtime. Read only to decide how its windows are launched; what a process runs is read from the
/// installation verified as a launch verifies it.
#[must_use]
pub fn runs_in_process(app: &str) -> bool {
    app_catalogue::apps::read_one_from_env(app)
        .ok()
        .flatten()
        .and_then(|installed| launcher::manifest::read_manifest(&installed.manifest_path).ok())
        .and_then(|manifest| manifest.entrypoint.runtime)
        .is_some_and(|runtime| launcher::run::tilolang_process::is_tilolang(&runtime))
}

/// An installed tilolang app as an agent task's programs reach it: its package, verified as a
/// launch verifies it, and the id its records are kept under when it was renamed from an earlier
/// one.
pub struct TaskPackage {
    pub app: String,
    pub module: runtime_portal::process_link::Module,
    pub documents_under: Option<String>,
}

/// Every installed tilolang app a task's programs name, each verified as a launch verifies it.
/// The task's process runs in the session, so no process of the app's runtimes is started. A program names an app by what its package declares (its actions,
/// its roots), which only the package itself holds: a program is compiled against its
/// declarations, not against the surface it publishes.
#[must_use]
pub fn task_packages() -> Vec<TaskPackage> {
    let apps = match app_catalogue::apps::read_from_env() {
        Ok(apps) => apps,
        Err(error) => {
            tracing::warn!(%error, "a task names no tilolang app: the installed apps are not known");
            return Vec::new();
        }
    };
    let earlier = tilo_action_policy::earlier::held();
    apps.iter()
        .filter(|app| runs_in_process(&app.app_id))
        .filter_map(|app| match verified_package(&app.app_id) {
            Ok(verified) => Some(TaskPackage {
                app: app.app_id.to_string(),
                module: verified.module,
                documents_under: Some(earlier.documents_under(&app.app_id).to_owned())
                    .filter(|under| under.as_str() != app.app_id.as_str()),
            }),
            Err(error) => {
                tracing::warn!(package_id = %app.app_id, %error, "a task leaves out an app it could not verify");
                None
            }
        })
        .collect()
}

/// The tilolang apps installed here, by id, each with the id its records are kept under (its
/// earliest, when it was renamed): what an instance tells another app it uses by.
fn installed_apps() -> std::collections::BTreeMap<String, String> {
    let apps = match app_catalogue::apps::read_from_env() {
        Ok(apps) => apps,
        Err(error) => {
            tracing::warn!(%error, "an instance reads no other app: the installed apps are not known");
            return std::collections::BTreeMap::new();
        }
    };
    let earlier = tilo_action_policy::earlier::held();
    apps.iter()
        .filter(|app| runs_in_process(&app.app_id))
        .map(|app| {
            (
                app.app_id.to_string(),
                earlier.documents_under(&app.app_id).to_owned(),
            )
        })
        .collect()
}

/// The one origin (`imap`, `youtube`) the installed app `app` reaches an outside service through,
/// when it declares one, and whether its signed manifest grants that origin's connector the
/// workspace's account sign-ins ([`tilo_app_package::manifest::ACCOUNT_SIGN_INS`]).
///
/// # Errors
/// The manifest does not read, or it declares more than one origin.
pub fn origin_of(app: &str) -> Result<Option<(String, bool)>, String> {
    let installed = app_catalogue::apps::read_one_from_env(app)
        .ok()
        .flatten()
        .ok_or_else(|| format!("{app} is not installed here"))?;
    let manifest = launcher::manifest::read_manifest(&installed.manifest_path)
        .map_err(|error| format!("reading {app}'s manifest: {error}"))?;
    match manifest.entrypoint.origins.as_slice() {
        [] => Ok(None),
        [origin] => Ok(Some((
            origin.clone(),
            manifest.declares_capability(tilo_app_package::manifest::ACCOUNT_SIGN_INS),
        ))),
        more => Err(format!(
            "{app} declares {} origins; an instance reaches one connector",
            more.len()
        )),
    }
}

/// What identifies one process: one workspace and one app, on the runtimes its package names.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct HostKey {
    workspace: String,
    app: String,
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

/// One instance, as the session hands it over: the app and workspace it runs in, the window it
/// draws (or none, for an action that shows nothing), who is at it, and the connections the
/// session made for it.
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
    /// What the window was asked to show.
    pub opens: crate::Opens,
    /// What the person may do in the app ([`crate::authority::access`]).
    pub access: Option<tilo_action_policy::MemberAccess>,
    /// The person's formats and language, as the session resolved them: what the instance writes
    /// its dates and clocks in, and the language its window reads every word in.
    pub locale: runtime_portal::process_link::Locale,
}

/// How a process is started: the command for one on `runtime` and `app_runtime`, keeping `storage`
/// and reading what an earlier version of its app kept (the directory, and where the process sees
/// it beneath its mount).
pub type Starts = dyn Fn(&str, &str, &Path, Option<(&Path, &Path)>) -> Command + Send + Sync;
/// How an installed app is verified for a process: the workspace it is installed in, and the app.
pub type Verifies = dyn Fn(&str, &str) -> Result<VerifiedInstance, String> + Send + Sync;

/// The tilolang processes this session runs, by workspace, app and runtimes.
pub struct Processes {
    /// Where processes keep what their instances keep, and where windows kept it before.
    data_home: PathBuf,
    starts: Box<Starts>,
    verifies: Box<Verifies>,
    running: Mutex<HashMap<HostKey, Host>>,
    generations: AtomicU64,
    /// Whether the app store is watched for reinstalls.
    follows_reinstalls: AtomicBool,
}

impl Processes {
    /// The processes this session starts through `launcher`, keeping what they keep under the person's
    /// data home.
    #[must_use]
    pub fn new(launcher: PathBuf) -> Arc<Self> {
        let data_home =
            tilo_paths::xdg::data_home().unwrap_or_else(|| PathBuf::from("/nonexistent"));
        Self::with(
            data_home,
            Self::started_through(launcher),
            Box::new(verified_installed),
        )
    }

    /// How a process is started through `launcher`: as `tilolang-process`, which confines it, on
    /// the runtimes it runs and with the storage it keeps.
    #[must_use]
    pub fn started_through(launcher: PathBuf) -> Box<Starts> {
        Box::new(move |runtime, app_runtime, storage, earlier| {
            let mut command = Command::new(&launcher);
            command
                .arg(launcher::run::tilolang_process::SUBCOMMAND)
                .arg("--runtime")
                .arg(runtime)
                .arg("--app-runtime")
                .arg(app_runtime)
                .arg("--storage")
                .arg(storage);
            if let Some((dir, under)) = earlier {
                command
                    .arg("--earlier")
                    .arg(dir)
                    .arg("--earlier-under")
                    .arg(under);
            }
            command
        })
    }

    /// Processes over `data_home`, started by `starts` and running what `verifies` verified.
    #[must_use]
    pub fn with(data_home: PathBuf, starts: Box<Starts>, verifies: Box<Verifies>) -> Arc<Self> {
        Arc::new(Self {
            data_home,
            starts,
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
        for (key, process) in self.running().iter() {
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
    /// from the workspace's policy as the session holds it now: a role given or taken back
    /// enables or disables what the windows already open offer. A process started later is handed
    /// it with each instance.
    pub fn policy_changed(&self, workspace: &crate::workspace::Workspace) {
        for (key, process) in self.running().iter() {
            if key.workspace != workspace.id {
                continue;
            }
            let app = key.app.clone();
            let access = runtime_portal::process_link::AppAccess {
                app: app.clone(),
                access: crate::authority::access(workspace, &app),
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
        // A run with no patch connection draws nothing: it runs the app's actions for
        // whoever invokes them.
        let shows = if instance.patch.is_some() {
            runtime_portal::process_link::Shows::View
        } else {
            runtime_portal::process_link::Shows::Nothing
        };
        self.hand_over_showing(instance, shows, false)
    }

    /// Run `instance`, which shows nothing and carries the package's migrations: it carries over,
    /// once, what a native app of the package's id kept in the workspace ([`Self::keep_earlier`]). It is started as any other run of the package is, with the same
    /// sandbox and grants.
    ///
    /// # Errors
    /// As [`Self::hand_over_now`].
    pub fn run_carrying_migrations(self: &Arc<Self>, instance: Instance) -> std::io::Result<bool> {
        self.hand_over_showing(instance, runtime_portal::process_link::Shows::Nothing, true)
    }

    /// Keep `documents`, what a native app of the id `app` kept in `workspace` as the host that
    /// runs this holds it, each by the key it was stored under, where a process of
    /// `app` in `workspace` reads what an earlier version of its app kept ([`Self::earlier`]):
    /// that app's documents of that workspace, and nothing else.
    ///
    /// # Errors
    /// A key that names no document of the app, or the store does not keep one.
    pub fn keep_earlier(
        &self,
        workspace: &str,
        app: &str,
        documents: Vec<(String, automerge::AutoCommit)>,
    ) -> std::io::Result<()> {
        let root = self.earlier_root(workspace, app);
        let store: std::rc::Rc<dyn durable_store::DurableStore> =
            std::rc::Rc::new(app_state::FsStore::provisioned(&root));
        for (key, mut document) in documents {
            app_state::DocStore::exclusive(
                std::rc::Rc::clone(&store),
                durable_store::StorageKey::document(app, workspace, &key),
            )
            .and_then(|mut kept| kept.save(&mut document))
            .map_err(|error| std::io::Error::other(format!("{app}'s {key}: {error}")))?;
        }
        store.flush(None).map_err(std::io::Error::other)
    }

    /// Show a viewer's page of an app from its tilolang process: the process lays its view out
    /// itself and writes it as a page for the one who looks at it, asked for on `patch`, which
    /// is the page's connection here (`tilo_server_protocol::page_window`).
    ///
    /// # Errors
    /// As [`Self::hand_over_now`].
    pub fn show_page(self: &Arc<Self>, instance: Instance) -> std::io::Result<bool> {
        self.hand_over_showing(instance, runtime_portal::process_link::Shows::Page, false)
    }

    fn hand_over_showing(
        self: &Arc<Self>,
        instance: Instance,
        shows: runtime_portal::process_link::Shows,
        carries_migrations: bool,
    ) -> std::io::Result<bool> {
        let refused =
            |what: String| std::io::Error::new(std::io::ErrorKind::PermissionDenied, what);
        let began = std::time::Instant::now();
        let verified = (self.verifies)(&instance.workspace, &instance.app).map_err(refused)?;
        let key = HostKey {
            workspace: instance.workspace.clone(),
            app: instance.app.clone(),
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
            shows,
            formats: instance.locale.formats,
            // The person's language, as this session resolved it: their windows read every word in
            // it, so the process that draws them must not read its own process's language instead.
            language: instance.locale.language,
            document: instance.opens.record,
            reveal: instance.opens.reveal,
            open: instance.opens.content.map(|content| OpenedFile {
                hash: content.hash,
                name: content.name,
            }),
            installed: if uses_packages {
                installed_apps()
            } else {
                std::collections::BTreeMap::new()
            },
            access: instance.access,
            carries_migrations,
        };
        let fds = InstanceFds {
            broker: instance.collaboration,
            identity: instance.identity,
            module,
            window: instance.patch,
            origin: instance.origin,
            content: instance.content,
            chooser: instance.chooser,
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
        let mut process = self.start(key)?;
        send_instance(&process.socket, request, fds)?;
        process
            .instances
            .insert(request.instance, request.app.clone());
        process.builds.insert(request.app.clone(), build);
        processes.insert(key.clone(), process);
        self.follow_reinstalls();
        Ok(true)
    }

    /// Where the process `key` keeps what its instances keep.
    fn storage(&self, key: &HostKey) -> std::io::Result<PathBuf> {
        let dir = self
            .data_home
            .join("tilo")
            .join("tilolang-processes")
            .join(&key.workspace)
            .join(&key.app);
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// What an earlier version of the app of `key` kept on this device, in the store its windows
    /// and the native app it replaced wrote: the directory of that app's documents in the
    /// workspace, and where it lies beneath that store's root. `None` where it kept nothing.
    fn earlier(&self, key: &HostKey) -> Option<(PathBuf, PathBuf)> {
        let root = self.earlier_root(&key.workspace, &key.app);
        let dir = app_state::workspace_index_directory(&root, &key.app, &key.workspace).ok()?;
        let under = dir.strip_prefix(&root).ok()?.to_path_buf();
        dir.is_dir().then_some((dir, under))
    }

    /// The root of the store the native app `app` wrote in `workspace` on this device.
    fn earlier_root(&self, workspace: &str, app: &str) -> PathBuf {
        launcher::appdir::host_app_dir(
            &self.data_home,
            &launcher::appdir::workspace_slug(Some(workspace)),
            app,
        )
        .join(".local/share")
    }

    /// Start the process `key`, with its socket as its stdin, and follow it on a thread of its own
    /// until it ends.
    fn start(self: &Arc<Self>, key: &HostKey) -> std::io::Result<Host> {
        let storage = self.storage(key)?;
        let (ours, theirs) = runtime_portal::process_link::pair()?;
        let earlier = self.earlier(key);
        let mut command = (self.starts)(
            &key.runtime,
            &key.app_runtime,
            &storage,
            earlier
                .as_ref()
                .map(|(dir, under)| (dir.as_path(), under.as_path())),
        );
        command
            // The socket goes as the launcher's stdin, which it moves to the launch kind's
            // descriptor itself: a descriptor placed anywhere else takes a `pre_exec` step, and
            // that makes `Command::spawn` `fork()` the compositor (`runtime_portal::launch_hold`).
            .stdin(std::process::Stdio::from(theirs))
            // Read here: what it said last is why it ended when it could not say so itself.
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn()?;
        drop(command);
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

/// The hash of an encoded module: whether a process runs that build already.
fn hash(module: &[u8]) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::hash::DefaultHasher::new();
    module.hash(&mut hasher);
    hasher.finish()
}

/// `app`, installed here, verified as a launch verifies it.
fn verified_package(app: &str) -> Result<VerifiedInstance, String> {
    let installed = package_store::find_installed(app)
        .map_err(|error| format!("the package store cannot be read: {error}"))?
        .ok_or_else(|| format!("{app} is not installed"))?;
    launcher::run::tilolang_process::verified_instance(
        &installed,
        &installed.manifest_path,
        &installed.payload_root,
        &installed.package_id_hex,
    )
    .map_err(|error| error.to_string())
}

/// `app`, installed here, verified as a launch verifies it, on runtimes installed here.
fn verified_installed(_workspace: &str, app: &str) -> Result<VerifiedInstance, String> {
    let verified = verified_package(app)?;
    if !verified.runs_here() {
        return Err(format!(
            "{app} names runtimes not installed here ({}, {})",
            verified.runtime, verified.app_runtime
        ));
    }
    Ok(verified)
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
            runtime: "tilolang@1".to_owned(),
            app_runtime: "tilo@1".to_owned(),
        }
    }

    fn processes(data_home: PathBuf) -> Arc<Processes> {
        Processes::with(
            data_home,
            Box::new(|_, _, _, _| Command::new("true")),
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
        // A workspace the app kept nothing in grants nothing: the process reads no empty store.
        let absent = HostKey {
            workspace: "7a110d00-0000-4000-8000-0000000000e3".to_owned(),
            ..key(WORKSPACE, NOTES)
        };
        assert!(processes.earlier(&absent).is_none());
    }

    /// What the server keeps of a native app for one workspace is what a process of the app in
    /// that workspace reads, and nothing of another workspace's: each is kept where only its own
    /// workspace's process is granted it.
    #[test]
    fn what_the_server_keeps_for_a_workspace_is_read_there_alone() {
        use durable_store::DurableStore as _;
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-0000000000e1";
        const ELSEWHERE: &str = "7a110d00-0000-4000-8000-0000000000e2";
        let home = tempfile::tempdir().expect("the server's data home");
        let processes = Processes::with(
            home.path().to_path_buf(),
            Box::new(|_, _, _, _| Command::new("true")),
            Box::new(|_, _| Err("no package store here".to_owned())),
        );
        let notebook = |title: &str| {
            use automerge::transaction::Transactable as _;
            let mut doc = automerge::AutoCommit::new();
            doc.put(automerge::ROOT, "title", title).unwrap();
            doc
        };
        processes
            .keep_earlier(
                WORKSPACE,
                NOTES,
                vec![("ours".to_owned(), notebook("Ours"))],
            )
            .unwrap();
        processes
            .keep_earlier(
                ELSEWHERE,
                NOTES,
                vec![("theirs".to_owned(), notebook("Theirs"))],
            )
            .unwrap();
        let key = HostKey {
            workspace: WORKSPACE.to_owned(),
            app: NOTES.to_owned(),
            runtime: "tilolang@1".to_owned(),
            app_runtime: "tilo@1".to_owned(),
        };
        let (granted, under) = processes
            .earlier(&key)
            .expect("the workspace's documents are kept");
        // The process reads the store from the root its grant sits under, as its mount shows it.
        let root = granted
            .ancestors()
            .nth(under.components().count())
            .expect("the store's root")
            .to_path_buf();
        let store = app_state::FsStore::provisioned(&root);
        assert_eq!(
            store.documents(NOTES, WORKSPACE).unwrap(),
            ["ours"],
            "the workspace's own documents are there"
        );
        let elsewhere = HostKey {
            workspace: ELSEWHERE.to_owned(),
            ..key
        };
        let (theirs, _) = processes
            .earlier(&elsewhere)
            .expect("the other workspace's are kept");
        assert!(
            !theirs.starts_with(&granted) && !granted.starts_with(&theirs),
            "another workspace's documents are no part of what the process reads"
        );
    }

    /// Processes running in each workspace of `running`, behind its socket, each with two windows of
    /// Notes.
    fn running(running: Vec<(&str, OwnedFd)>) -> Arc<Processes> {
        let processes = Processes::with(
            std::env::temp_dir(),
            Box::new(|_, _, _, _| Command::new("true")),
            Box::new(|_, _| Err("no package store here".to_owned())),
        );
        for (workspace, socket) in running {
            let key = HostKey {
                workspace: workspace.to_owned(),
                app: NOTES.to_owned(),
                runtime: "tilolang@1".to_owned(),
                app_runtime: "tilo@1".to_owned(),
            };
            let process = Host {
                socket: Arc::new(socket),
                instances: HashMap::from([(7, NOTES.to_owned()), (8, NOTES.to_owned())]),
                generation: 1,
                builds: HashMap::new(),
            };
            processes.running().insert(key, process);
        }
        processes
    }

    /// Every process running says on its socket what the person's formats now are, so the instances it
    /// runs write their weeks and times again; a process that is not running hears nothing.
    #[test]
    fn a_formats_change_is_said_on_the_socket_of_every_host_running() {
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-0000000000f1";
        let (ours, theirs) = runtime_portal::process_link::pair().unwrap();
        let (idle_ours, _idle_theirs) = runtime_portal::process_link::pair().unwrap();
        let processes = running(vec![("7a110d00-0000-4000-8000-0000000000f1", ours)]);

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
        // The session keeps no other process's socket: nothing was said on it, so its end is all the
        // process would hear.
        drop(idle_ours);
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
            Box::new(|_, _, _, _| Command::new("true")),
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
        let workspace = crate::workspace::Workspace {
            policy: Some(policy),
            person: "ada".to_owned(),
            ..crate::workspace::Workspace::new(WORKSPACE, std::env::temp_dir())
        };
        processes.policy_changed(&workspace);
        let handed = ToProcess::receive(&theirs).unwrap();
        let Some(ToProcess::Access(said)) = handed else {
            panic!("the workspace's process is told the person's access: {handed:?}");
        };
        assert_eq!(said.app, NOTES);
        // What a launched Notes reads of the person's authority, as it reads back off the socket.
        let expected = crate::authority::access(&workspace, NOTES).unwrap();
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
}
