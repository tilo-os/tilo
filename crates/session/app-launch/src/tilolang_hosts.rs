//! Tilolang hosts: one process per workspace and author that runs that author's tilolang apps in
//! that workspace, each window's instance in it (`docs/designs/app-language/ui-host.md`).
//!
//! A tilolang app's window is a launched process of its own, with its own launch identity for its
//! surface and accessibility, started as its window alone (`--tilolang-hosted`: the launcher gives
//! it no collaboration connection). The session hands each window's instance to the host of its
//! workspace and author ([`Hosts::hand_over`]):
//!
//! - the host's end of the window's patch connection, whose other end the window took on its
//!   window door (`runtime_portal::windows::PATCH_VERB`);
//! - the instance's collaboration connection, attested as that window of the app's launch, exactly
//!   as a later window's own connection is: the broker attributes it from the session's record,
//!   never from the host;
//! - its identity-service connection, opened and named for the app and workspace;
//! - the connection to the app's origin connector, when its types hold an outside service's
//!   resources;
//! - its module, read from the installation verified as a launch verifies it, in a sealed memfd.
//!
//! The person's region formats ride with the instance, and are said again on every host's door
//! when they change them in Settings ([`Hosts::locale_changed`]): their windows' dates, clocks
//! and weeks follow where they stand, without waiting for a restart.
//!
//! The author is the publisher whose key signed the package; a person's own build, signed with
//! their own key, runs in a host of that person's. A host is started through the launch kind
//! "tilolang host" (`tilo-launcher tilolang-host`), keyed by its workspace, its author and the
//! runtimes it runs on, with the storage of that workspace and author; it ends once the last of its
//! instances ended, which it says on its door.
//!
//! A window that ran its instance in its own process kept the app's record in the app's own
//! directory. The first time a host keeps that app's record in that workspace, it takes that copy
//! over ([`adopt_window_copy`]), so nothing a person kept before is lost.

use std::collections::{HashMap, HashSet, VecDeque};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use launcher::run::tilolang_host::VerifiedInstance;
use runtime_portal::tilolang_host::{FromHost, InstanceFds, InstanceRequest, receive_from_host};

/// The launcher's flag for a window whose instance a tilolang host runs.
pub const HOSTED_ARG: &str = "--tilolang-hosted";

/// The app that keeps each person's own views (`apps/tl/views`): views they, their agents or
/// others wrote, which read whatever the workspace has installed.
const PERSON_VIEWS_APP: &str = "so.tilo.lang.views";

/// How many of its last error lines a host that ended without saying why is reported by.
const SAID_LINES: usize = 8;

/// Whether the installed app `app` is a tilolang app: its manifest's entrypoint runs a tilolang
/// runtime. Read only to decide how its windows are launched; what a host runs is read from the
/// installation verified as a launch verifies it.
#[must_use]
pub fn runs_in_host(app: &str) -> bool {
    app_catalogue::apps::read_one_from_env(app)
        .ok()
        .flatten()
        .and_then(|installed| launcher::manifest::read_manifest(&installed.manifest_path).ok())
        .and_then(|manifest| manifest.entrypoint.runtime)
        .is_some_and(|runtime| launcher::run::tilolang_host::is_tilolang(&runtime))
}

/// An installed tilolang app as an agent task's programs reach it: its package, verified as a
/// launch verifies it, and the id its records are kept under when it was renamed from an earlier
/// one.
pub struct TaskPackage {
    pub app: String,
    pub module: runtime_portal::tilolang_host::Module,
    pub records_under: Option<String>,
}

/// Every installed tilolang app a task's programs name, each verified as a launch verifies it.
/// The task's host runs in the session, so no host of the app's runtimes is started. A program names an app by what its package declares (its actions,
/// its roots), which only the package itself holds: the surface it publishes names its actions
/// by their stored ids.
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
        .filter(|app| runs_in_host(&app.package_id))
        .filter_map(|app| match verified_package(&app.package_id) {
            Ok(verified) => Some(TaskPackage {
                app: app.package_id.clone(),
                module: verified.module,
                records_under: Some(earlier.records_under(&app.package_id).to_owned())
                    .filter(|under| *under != app.package_id),
            }),
            Err(error) => {
                tracing::warn!(package_id = %app.package_id, %error, "a task leaves out an app it could not verify");
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
        .filter(|app| runs_in_host(&app.package_id))
        .map(|app| {
            (
                app.package_id.clone(),
                earlier.records_under(&app.package_id).to_owned(),
            )
        })
        .collect()
}

/// The one origin (`imap`, `youtube`) the installed app `app` reaches an outside service through,
/// when it declares one: its instance reaches that origin's connector.
///
/// # Errors
/// The manifest does not read, or it declares more than one origin.
pub fn origin_of(app: &str) -> Result<Option<String>, String> {
    let installed = app_catalogue::apps::read_one_from_env(app)
        .ok()
        .flatten()
        .ok_or_else(|| format!("{app} is not installed here"))?;
    let manifest = launcher::manifest::read_manifest(&installed.manifest_path)
        .map_err(|error| format!("reading {app}'s manifest: {error}"))?;
    match manifest.entrypoint.origins.as_slice() {
        [] => Ok(None),
        [origin] => Ok(Some(origin.clone())),
        more => Err(format!(
            "{app} declares {} origins; an instance reaches one connector",
            more.len()
        )),
    }
}

/// Whose apps a host runs.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Author {
    /// The publisher whose key signed the packages.
    Publisher([u8; 32]),
    /// A person, for their own builds.
    Person(String),
}

impl Author {
    /// The author of a package signed by `publisher`: the person themselves when it is their own
    /// key, else the publisher.
    fn of(publisher: [u8; 32], person: &str) -> Self {
        if hex::encode(publisher) == person {
            Self::Person(person.to_owned())
        } else {
            Self::Publisher(publisher)
        }
    }

    /// The author's key in the hosts' names and storage: one path component.
    fn key(&self) -> String {
        match self {
            Self::Publisher(key) => format!("publisher-{}", hex::encode(key)),
            Self::Person(person) => format!("person-{}", hex::encode(person)),
        }
    }
}

/// What identifies one host.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct HostKey {
    workspace: String,
    author: String,
    /// The tilolang runtime and the Tilo runtime it runs on: an instance runs only in a host of
    /// the runtimes its package names.
    runtime: String,
    app_runtime: String,
}

/// A running host.
struct Host {
    /// The session's end of its door.
    door: Arc<OwnedFd>,
    /// The windows whose instances it runs.
    instances: HashSet<u64>,
    /// Which start of the host this is, so the end of an earlier one never retires a later one.
    generation: u64,
}

/// One window's instance, as the session hands it over: the app and workspace it runs in, the
/// window it draws, who is at it, and the connections the session made for it.
pub struct Instance {
    pub app: String,
    pub workspace: String,
    /// The window's instance: the tab it takes.
    pub instance: u64,
    /// The participant at the window, whose steps its undo takes back.
    pub person: String,
    /// The host's end of the window's patch connection.
    pub patch: OwnedFd,
    /// The instance's collaboration connection, attested as that window.
    pub collaboration: OwnedFd,
    /// Its identity-service connection, named for the app and workspace.
    pub identity: OwnedFd,
    /// Its connection to the app's origin connector, when the app has one.
    pub origin: Option<OwnedFd>,
    /// What the window was asked to show.
    pub opens: crate::Opens,
}

/// How a host is started: the command for one on `runtime` and `app_runtime`, keeping `storage`.
type Starts = dyn Fn(&str, &str, &Path) -> Command + Send + Sync;
/// How an installed app is verified for a host.
type Verifies = dyn Fn(&str) -> Result<VerifiedInstance, String> + Send + Sync;

/// The tilolang hosts this session runs, by workspace, author and runtimes.
pub struct Hosts {
    /// Where hosts keep what their instances keep, and where windows kept it before.
    data_home: PathBuf,
    starts: Box<Starts>,
    verifies: Box<Verifies>,
    running: Mutex<HashMap<HostKey, Host>>,
    generations: AtomicU64,
}

impl Hosts {
    /// The hosts this session starts through `launcher`, keeping what they keep under the person's
    /// data home.
    #[must_use]
    pub fn new(launcher: PathBuf) -> Arc<Self> {
        let data_home =
            tilo_paths::xdg::data_home().unwrap_or_else(|| PathBuf::from("/nonexistent"));
        Self::with(
            data_home,
            Box::new(move |runtime, app_runtime, storage| {
                let mut command = Command::new(&launcher);
                command
                    .arg(launcher::run::tilolang_host::SUBCOMMAND)
                    .arg("--runtime")
                    .arg(runtime)
                    .arg("--app-runtime")
                    .arg(app_runtime)
                    .arg("--storage")
                    .arg(storage);
                command
            }),
            Box::new(verified_installed),
        )
    }

    /// Hosts over `data_home`, started by `starts` and running what `verifies` verified.
    #[must_use]
    pub fn with(data_home: PathBuf, starts: Box<Starts>, verifies: Box<Verifies>) -> Arc<Self> {
        Arc::new(Self {
            data_home,
            starts,
            verifies,
            running: Mutex::new(HashMap::new()),
            generations: AtomicU64::new(1),
        })
    }

    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<HostKey, Host>> {
        self.running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The hosts running now, each by its workspace and author, with the windows whose instances
    /// it runs.
    #[must_use]
    pub fn instances(&self) -> Vec<(String, String, Vec<u64>)> {
        let mut hosts: Vec<_> = self
            .running()
            .iter()
            .map(|(key, host)| {
                let mut instances: Vec<u64> = host.instances.iter().copied().collect();
                instances.sort_unstable();
                (key.workspace.clone(), key.author.clone(), instances)
            })
            .collect();
        hosts.sort();
        hosts
    }

    /// Say on every host running now that the person's formats or language changed: the
    /// instances write the times and days their windows show again in the new formats (their
    /// week's first day, their date and clock styles), and their windows read their words in the
    /// language, where they stand. A host started later is handed them with its first instance.
    pub fn locale_changed(&self) {
        let locale = runtime_portal::tilolang_host::Locale {
            formats: ui::format::region::current(),
            language: ui::i18n::preferred_language().unwrap_or_default(),
        };
        for (key, host) in self.running().iter() {
            if let Err(error) = runtime_portal::tilolang_host::send_locale(&host.door, &locale) {
                // Its follow thread reads the end of the door and forgets it: the next instance
                // starts another.
                tracing::warn!(
                    %error,
                    workspace = %key.workspace,
                    author = %key.author,
                    "a tilolang host was not told the person's formats or language changed"
                );
            }
        }
    }

    /// Hand `instance` to the host of its workspace and author, starting the host when none runs.
    /// Done off the caller's thread: it verifies the package and may start the host.
    pub fn hand_over(self: &Arc<Self>, instance: Instance) {
        let hosts = Arc::clone(self);
        let started = std::thread::Builder::new()
            .name("tilolang-handover".into())
            .spawn(move || {
                let (app, window) = (instance.app.clone(), instance.instance);
                if let Err(error) = hosts.hand_over_now(instance) {
                    tracing::error!(
                        %error,
                        app,
                        instance = window,
                        "a tilolang window's instance was not handed to its host"
                    );
                }
            });
        if let Err(error) = started {
            tracing::error!(%error, "a tilolang window's instance could not be handed over");
        }
    }

    /// [`Self::hand_over`] on this thread. Whether it started the host.
    ///
    /// # Errors
    /// The app does not verify, its module cannot be sealed, or no host takes the instance.
    pub fn hand_over_now(self: &Arc<Self>, instance: Instance) -> std::io::Result<bool> {
        let refused =
            |what: String| std::io::Error::new(std::io::ErrorKind::PermissionDenied, what);
        let began = std::time::Instant::now();
        let verified = (self.verifies)(&instance.app).map_err(refused)?;
        let key = HostKey {
            workspace: instance.workspace.clone(),
            author: Author::of(verified.publisher, &instance.person).key(),
            runtime: verified.runtime,
            app_runtime: verified.app_runtime,
        };
        let mut module = verified.module;
        if instance.app == PERSON_VIEWS_APP {
            self.with_every_installed_app(&mut module, &key);
        }
        // A package the app uses that is installed as an app is another app: its instance reads
        // that app's records from where it keeps them.
        let uses_packages = !module.imports.is_empty();
        let module = module.encode();
        let module_len = module.len() as u64;
        let module = tilo_render_protocol::shm::sealed_blob("tilolang-module", &module)?;
        let records_under = Some(
            tilo_action_policy::earlier::held()
                .records_under(&instance.app)
                .to_owned(),
        )
        .filter(|id| *id != instance.app);
        let storage = self.storage(&key)?;
        adopt_window_copy(
            &self.data_home,
            &storage,
            &instance.app,
            records_under.as_deref().unwrap_or(&instance.app),
            &instance.workspace,
        );
        let request = InstanceRequest {
            app: instance.app.clone(),
            room: instance.workspace.clone(),
            instance: instance.instance,
            person: instance.person,
            module_len,
            records_under,
            applies_change: false,
            formats: ui::format::region::current(),
            // The person's language, as this session resolved it: their windows read every word in
            // it, so the host that draws them must not read its own process's language instead.
            language: ui::i18n::preferred_language().unwrap_or_default(),
            record: instance.opens.record,
            reveal: instance.opens.reveal,
            installed: if uses_packages {
                installed_apps()
            } else {
                std::collections::BTreeMap::new()
            },
        };
        let fds = InstanceFds {
            broker: instance.collaboration,
            identity: instance.identity,
            module,
            window: instance.patch,
            origin: instance.origin,
        };
        let started = self.send(&key, &request, &fds)?;
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

    /// Give `module`, the person's views, every installed tilolang app its host can run (`key`'s
    /// runtimes), each verified as a launch verifies it: a written view reads any type the
    /// workspace has, so the host that draws them holds every app's package.
    fn with_every_installed_app(
        &self,
        module: &mut runtime_portal::tilolang_host::Module,
        key: &HostKey,
    ) {
        let apps = match app_catalogue::apps::read_from_env() {
            Ok(apps) => apps,
            Err(error) => {
                tracing::error!(%error, "the person's views get no other app: the installed apps are not known");
                return;
            }
        };
        for app in apps
            .iter()
            .filter(|app| app.package_id != PERSON_VIEWS_APP && runs_in_host(&app.package_id))
        {
            let Ok(verified) = (self.verifies)(&app.package_id) else {
                tracing::warn!(package_id = %app.package_id, "the person's views leave out an app that could not be verified");
                continue;
            };
            if verified.runtime != key.runtime || verified.app_runtime != key.app_runtime {
                tracing::warn!(package_id = %app.package_id, "the person's views leave out an app on other runtimes");
                continue;
            }
            if !module.imports.contains(&verified.module.wasm) {
                module.imports.push(verified.module.wasm);
            }
        }
    }

    /// Send the instance to the host `key`, starting it first when none runs. Whether it started
    /// the host.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "a host is chosen, started and handed the instance in one hold, so it never ends in between"
    )]
    fn send(
        self: &Arc<Self>,
        key: &HostKey,
        request: &InstanceRequest,
        fds: &InstanceFds,
    ) -> std::io::Result<bool> {
        let mut hosts = self.running();
        if let Some(host) = hosts.get_mut(key) {
            match runtime_portal::tilolang_host::send_instance(&host.door, request, fds) {
                Ok(()) => {
                    host.instances.insert(request.instance);
                    return Ok(false);
                }
                // The host is ending: start another.
                Err(error) => {
                    tracing::info!(%error, "a tilolang host's door is closed");
                    hosts.remove(key);
                }
            }
        }
        let mut host = self.start(key)?;
        runtime_portal::tilolang_host::send_instance(&host.door, request, fds)?;
        host.instances.insert(request.instance);
        hosts.insert(key.clone(), host);
        Ok(true)
    }

    /// Where the host `key` keeps what its instances keep.
    fn storage(&self, key: &HostKey) -> std::io::Result<PathBuf> {
        let dir = self
            .data_home
            .join("tilo")
            .join("tilolang-hosts")
            .join(&key.workspace)
            .join(&key.author);
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Start the host `key`, with its door at the launch kind's descriptor, and follow it on a
    /// thread of its own until it ends.
    fn start(self: &Arc<Self>, key: &HostKey) -> std::io::Result<Host> {
        use std::os::unix::process::CommandExt as _;
        let storage = self.storage(key)?;
        let (ours, theirs) = runtime_portal::tilolang_host::pair()?;
        let mut command = (self.starts)(&key.runtime, &key.app_runtime, &storage);
        command
            .stdin(std::process::Stdio::null())
            // Read here: what it said last is why it ended when it could not say so itself.
            .stderr(std::process::Stdio::piped());
        let door = theirs.as_raw_fd();
        let slot = launcher::run::tilolang_host::DOOR_CHILD_FD;
        // SAFETY: only async-signal-safe calls between fork and exec: the door is placed at its
        // slot without close-on-exec.
        unsafe {
            command.pre_exec(move || {
                if door == slot {
                    if libc::fcntl(slot, libc::F_SETFD, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                } else if libc::dup2(door, slot) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        drop(command);
        drop(theirs);
        let tag = format!("{} {}", key.workspace, key.author);
        let said = child.stderr.take().map(|stderr| {
            std::thread::Builder::new()
                .name("tilolang-host-err".into())
                .spawn(move || last_lines(stderr, &tag))
        });
        let generation = self.generations.fetch_add(1, Ordering::Relaxed);
        let door = Arc::new(ours);
        tracing::info!(
            workspace = %key.workspace,
            author = %key.author,
            pid = child.id(),
            "tilolang host started"
        );
        let (hosts, followed, key) = (Arc::clone(self), Arc::clone(&door), key.clone());
        std::thread::Builder::new()
            .name("tilolang-host".into())
            .spawn(move || hosts.follow(&key, generation, &followed, child, said))?;
        Ok(Host {
            door,
            instances: HashSet::new(),
            generation,
        })
    }

    /// Follow the host `key` of `generation` over its door `followed` until it ends: each
    /// instance that ends is noted, then the host's own end and why, and the host is forgotten.
    fn follow(
        &self,
        key: &HostKey,
        generation: u64,
        followed: &OwnedFd,
        mut child: std::process::Child,
        said: Said,
    ) {
        // Why the host says it exits; none when it ended without saying (killed, crashed).
        let mut reason: Option<String> = None;
        loop {
            match receive_from_host(followed) {
                Ok(Some(FromHost::Ended(instance))) => self.ended(key, generation, instance),
                Ok(Some(FromHost::Exiting(why))) => reason = Some(why),
                Ok(None) => break,
                // The packet was taken: the door carries the next one.
                Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                    tracing::error!(%error, workspace = %key.workspace, author = %key.author, "a tilolang host said something unreadable");
                }
                // The door failed: it carries nothing more; the host is waited for.
                Err(error) => {
                    tracing::error!(%error, workspace = %key.workspace, author = %key.author, "a tilolang host's door failed");
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
            tracing::info!(workspace = %key.workspace, author = %key.author, ?status, reason, "tilolang host ended");
        } else {
            tracing::error!(workspace = %key.workspace, author = %key.author, ?status, reason, "tilolang host ended");
        }
        let mut hosts = self.running();
        if hosts
            .get(key)
            .is_some_and(|host| host.generation == generation)
        {
            hosts.remove(key);
        }
    }

    /// The host `key` (its start `generation`) says the instance of the window `instance` ended.
    /// Once its last instance ended it is closed: it ends when it has written what it keeps.
    fn ended(&self, key: &HostKey, generation: u64, instance: u64) {
        let closed = {
            let mut hosts = self.running();
            let Some(host) = hosts
                .get_mut(key)
                .filter(|host| host.generation == generation)
            else {
                return;
            };
            host.instances.remove(&instance);
            if host.instances.is_empty() {
                hosts.remove(key)
            } else {
                None
            }
        };
        if let Some(host) = closed {
            // The host reads the end of its door and ends once it runs nothing.
            // SAFETY: shutting down the writing half of a socket the session owns.
            let _ = unsafe { libc::shutdown(host.door.as_raw_fd(), libc::SHUT_WR) };
        }
    }
}

/// `app`, installed here, verified as a launch verifies it.
fn verified_package(app: &str) -> Result<VerifiedInstance, String> {
    let installed = package_store::find_installed(app)
        .map_err(|error| format!("the package store cannot be read: {error}"))?
        .ok_or_else(|| format!("{app} is not installed"))?;
    launcher::run::tilolang_host::verified_instance(
        &installed,
        &installed.manifest_path,
        &installed.payload_root,
        &installed.package_id_hex,
    )
    .map_err(|error| error.to_string())
}

/// `app`, installed here, verified as a launch verifies it, on runtimes installed here.
fn verified_installed(app: &str) -> Result<VerifiedInstance, String> {
    let verified = verified_package(app)?;
    if !verified.runs_here() {
        return Err(format!(
            "{app} names runtimes not installed here ({}, {})",
            verified.runtime, verified.app_runtime
        ));
    }
    Ok(verified)
}

/// Take over what a window of `app` that ran its instance in its own process kept of its record
/// in `workspace`, the first time the host whose storage is `storage` keeps it: the app's own
/// directory held it (`<data home>/tilo-rooms/<workspace>/<app>`, its documents under its data
/// home, keyed by `records`, the id its records are kept under), and the host's documents hold
/// it from now on, the same files in the same layout. Once the host holds a copy, nothing moves.
pub fn adopt_window_copy(
    data_home: &Path,
    storage: &Path,
    app: &str,
    records: &str,
    workspace: &str,
) {
    let window_root = launcher::appdir::host_app_dir(
        data_home,
        &launcher::appdir::workspace_slug(Some(workspace)),
        app,
    )
    .join(".local/share");
    let host_root = storage.join(app).join("documents");
    let (Ok(from), Ok(to)) = (
        app_state::workspace_index_directory(&window_root, records, workspace),
        app_state::workspace_index_directory(&host_root, records, workspace),
    ) else {
        return;
    };
    if !from.exists() || to.exists() {
        return;
    }
    let moved = to
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::rename(&from, &to));
    match moved {
        Ok(()) => tracing::info!(
            app,
            workspace,
            "a host took over what the app's windows kept"
        ),
        Err(error) => tracing::error!(
            %error,
            app,
            workspace,
            from = %from.display(),
            "what the app's windows kept could not be taken over by its host"
        ),
    }
}

/// A host's error output, read on a thread of its own: its last lines once it has ended.
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
        tracing::info!(target: tilo_startup_metrics::APP_OUTPUT_TARGET, "[tilolang-host {tag}] {line}");
        if last.len() == SAID_LINES {
            last.pop_front();
        }
        last.push_back(line);
    }
    last
}

/// Why a host ended that did not say so itself: how it ended, as the session saw it, and the last
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

    /// Every host running says on its door what the person's formats now are, so the instances it
    /// runs write their weeks and times again; a host that is not running hears nothing.
    #[test]
    fn a_formats_change_is_said_on_the_door_of_every_host_running() {
        let (ours, theirs) = runtime_portal::tilolang_host::pair().unwrap();
        let (idle_ours, idle_theirs) = runtime_portal::tilolang_host::pair().unwrap();
        let hosts = Hosts::with(
            std::env::temp_dir(),
            Box::new(|_, _, _| Command::new("true")),
            Box::new(|_| Err("no package store here".to_owned())),
        );
        let key = |workspace: &str| HostKey {
            workspace: workspace.to_owned(),
            author: "person-ada".to_owned(),
            runtime: "tilolang@1".to_owned(),
            app_runtime: "tilo@1".to_owned(),
        };
        let host = |door: OwnedFd| Host {
            door: Arc::new(door),
            instances: HashSet::from([7]),
            generation: 1,
        };
        hosts
            .running()
            .insert(key("7a110d00-0000-4000-8000-0000000000f1"), host(ours));

        let before = ui::format::region::current();
        let sunday = ui::format::region::RegionFormats {
            first_day: ui::format::region::FirstDayOfWeek::Sunday,
            ..before
        };
        ui::format::region::install(sunday);
        hosts.locale_changed();
        let handed = runtime_portal::tilolang_host::receive(&theirs).unwrap();
        ui::format::region::install(before);
        let Some(runtime_portal::tilolang_host::Handed::Locale(locale)) = handed else {
            panic!("a runtime host's door carries the formats as formats: {handed:?}");
        };
        assert_eq!(
            locale.formats, sunday,
            "the week the person starts on reaches the host its instances run in"
        );
        // The session keeps no other host's door: nothing was said on it, so its end is all the
        // host would hear.
        drop(idle_ours);
        assert!(
            runtime_portal::tilolang_host::receive(&idle_theirs)
                .unwrap()
                .is_none(),
            "a host that is not running is not told"
        );
    }

    /// A host that ended without saying why is reported by how it ended and what it last wrote.
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

    #[test]
    fn a_hosts_author_is_its_publisher_or_the_person_whose_own_build_it_is() {
        let key = [7_u8; 32];
        assert_eq!(Author::of(key, "ada"), Author::Publisher(key));
        assert_eq!(
            Author::of(key, &hex::encode(key)),
            Author::Person(hex::encode(key))
        );
        let names = [
            Author::Publisher(key).key(),
            Author::Person("ada".into()).key(),
        ];
        assert!(names.iter().all(|name| !name.contains(['/', '\0', '.'])));
        assert_ne!(names[0], names[1], "kinds never collide");
    }

    /// What a window kept of its app's record in its own process, before the app's windows ran
    /// their instances in a host, is what the host opens: the record and the member records
    /// beside it, nothing lost. Once the host holds a copy, a later adoption leaves it alone.
    #[test]
    fn a_host_opens_what_the_apps_windows_kept_before() {
        use automerge::ReadDoc as _;
        use automerge::transaction::Transactable as _;
        use std::rc::Rc;
        const APP: &str = "so.tilo.lang.notes";
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-0000000000a1";
        let home = tempfile::tempdir().unwrap();
        let data_home = home.path();
        let storage = data_home
            .join("tilo/tilolang-hosts")
            .join(WORKSPACE)
            .join("publisher-ab");
        std::fs::create_dir_all(&storage).unwrap();
        // The window's own process: its app directory, its documents under its data home.
        let window_root =
            launcher::appdir::host_app_dir(data_home, WORKSPACE, APP).join(".local/share");
        let write = |root: &Path, member: Option<&str>, note: &str| {
            let documents: Rc<dyn durable_store::DurableStore> =
                Rc::new(app_state::FsStore::new(root.to_path_buf()));
            let mut store = match member {
                None => collaboration::record::save::workspace_document(&documents, APP, WORKSPACE),
                Some(record) => {
                    collaboration::record::save::record_document(&documents, APP, WORKSPACE, record)
                }
            }
            .unwrap();
            let mut doc = store.load_or_new();
            doc.put(automerge::ROOT, "note", note).unwrap();
            store.save(&mut doc).unwrap();
            documents.flush(None).unwrap();
        };
        let read = |root: &Path, member: Option<&str>| -> Option<String> {
            let documents: Rc<dyn durable_store::DurableStore> =
                Rc::new(app_state::FsStore::provisioned(root.to_path_buf()));
            let mut store = match member {
                None => collaboration::record::save::workspace_document(&documents, APP, WORKSPACE),
                Some(record) => {
                    collaboration::record::save::record_document(&documents, APP, WORKSPACE, record)
                }
            }
            .unwrap();
            let doc = store.load_or_new();
            doc.get(automerge::ROOT, "note")
                .unwrap()
                .and_then(|(value, _)| value.into_string().ok())
        };
        write(&window_root, None, "kept by the window");
        write(&window_root, Some("member-ada"), "ada's own");

        adopt_window_copy(data_home, &storage, APP, APP, WORKSPACE);
        let host_root = storage.join(APP).join("documents");
        assert_eq!(
            read(&host_root, None).as_deref(),
            Some("kept by the window")
        );
        assert_eq!(
            read(&host_root, Some("member-ada")).as_deref(),
            Some("ada's own")
        );

        // The window's directory holds a copy again (a window started before its app ran in a
        // host): the host's is what counts, and stays.
        write(&window_root, None, "a stale copy");
        adopt_window_copy(data_home, &storage, APP, APP, WORKSPACE);
        assert_eq!(
            read(&host_root, None).as_deref(),
            Some("kept by the window")
        );
    }
}
