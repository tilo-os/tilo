//! `tilolang dev`: edit a package and see the running app change, with its state.
//!
//! The tool watches the package's sources (and those of the packages it uses). On every save it
//! compiles in-process; a compile error is printed and the app keeps running the last good
//! build.
//!
//! Without `--session` the package runs as an ordinary window on this desktop, started here: the
//! tool publishes each build into a directory of numbered generations
//! (`tilo_app_module`, `TILO_APP_MODULE_UPDATES`) and runs `tilolang-app` on this generation, as
//! a person's own app, with no session. The app's record is its local store, kept across saves,
//! process restarts and later runs. The running app takes a new generation on the same store and
//! windows; when it cannot read the state the old one wrote ([`tilolang_tilo::upgrade`]), the
//! tool says why in one line and the app starts over on the new build with its data, as its next
//! start would. The runtime binary is `--runtime BIN`, or a build of `apps/tilolang` the tool
//! makes with cargo and remakes on every change of its sources, restarting the window on it.
//!
//! With `--session` the package is developed inside a running nested session: a good build is
//! signed into a `.tapp` and installed with `tilo-install`, exactly as any install: verified,
//! recorded, its old version kept while something runs it. The session's launcher follows
//! reinstalls of a module payload's package (`crates/packaging/launcher/src/live_module.rs`),
//! verifies the new installation as a launch would, and hands its payload to the running
//! `tilolang-app`, which takes the new module over in place or starts over, as on the desktop.
//! With `--runtime BIN` the apps run on a tilolang runtime binary of the developer's own build
//! instead of the session's: the tool bakes it into a signed runtime bundle, installs that as a
//! runtime package (`tilolang/<version>+dev`) with `tilo-install`, builds the apps naming it, and
//! restarts their windows on it. Each later change of the binary re-bakes and reinstalls the
//! bundle and restarts the apps that use it; the session keeps running.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use bounded_command::{WakeFd, Wakeup, wait_child_or};
use tilo_app_module::UPDATES_ENV;
use tilo_app_package::pack::SigningIdentity;
use tilo_files::ChangeKind;
use tilo_files::native::{ChangeWatch, WatchEvent, Watcher};
use tilolang::PackageMeta;
use tilolang_tilo::upgrade::{Upgrade, compare};

use crate::package::{self, BuildOptions, BuiltApp};
use crate::session::{self, Peer};

/// The sources of the tilolang runtime binary: what a rebuild of it is watched on.
const RUNTIME_SOURCES: [&str; 4] = [
    "apps/tilolang",
    "crates/platform/tilolang-tilo",
    "crates/lang/tilolang-runtime",
    "crates/lang/tilolang",
];

/// The crate and binary of the tilolang runtime a local run builds and runs.
const RUNTIME_PACKAGE: &str = "tilolang-app";

/// The build of the tilolang runtime binary alone, thin, against the runtime library a nested
/// session runs, for this machine's architecture: a binary of a few megabytes, as the session's
/// own runtime bundle carries.
const RUNTIME_SCRIPT: &str = "dist/dev/tilolang-runtime-build.sh";

/// Where the thin build lands in the checkout's target directory: the binary and the runtime
/// library it links.
const RUNTIME_BINARY: &str = "target/tilolang-runtime/tilolang-app";
const RUNTIME_LIBRARY: &str = "target/tilolang-runtime/libruntime_dylib.so";

/// How long the app gets to flush its checkpoint when the tool restarts or stops it.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// The variable the launcher names the package's signed id with.
const PACKAGE_ID_ENV: &str = "TILO_PACKAGE_ID";

/// The variables a session names to an app it starts, which would make the desktop's app a
/// participant of a session it is not part of.
const SESSION_ENV_PREFIXES: [&str; 2] = ["TILO_SVC_", "TILO_RUNTIME_"];
const SESSION_ENV: [&str; 10] = [
    "TILO_WORKSPACE_ID",
    "TILO_PARTICIPATION",
    "TILO_SURFACE",
    "TILO_SURFACE_HASH",
    "TILO_APP_MODULE",
    "TILO_APP_MODULE_UPDATES",
    "TILO_PACKAGE_ID",
    "TILO_DEV_SESSION",
    "TILO_LAUNCHER_LIVE_MODULES",
    "TILO_TEST_CONTROL",
];

/// The ELF machine of this machine's architecture, as an ELF header names it (`e_machine`).
const fn elf_machine() -> Option<u16> {
    if cfg!(target_arch = "x86_64") {
        Some(62)
    } else if cfg!(target_arch = "aarch64") {
        Some(183)
    } else {
        None
    }
}

/// Fail if `path` is an ELF file of another architecture than this machine's: a builder of
/// another architecture builds programs this machine cannot run.
fn check_architecture(path: &Path) -> Result<(), String> {
    use std::io::Read as _;
    let Some(expected) = elf_machine() else {
        return Ok(());
    };
    let mut header = [0_u8; 20];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let [.., m0, m1] = header;
    let machine = header
        .starts_with(b"\x7fELF")
        .then(|| u16::from_le_bytes([m0, m1]));
    match machine {
        Some(machine) if machine == expected => Ok(()),
        _ => Err(format!(
            "{} is not built for {}: build on a builder of this architecture",
            path.display(),
            std::env::consts::ARCH
        )),
    }
}

/// What `tilolang dev` was asked for.
#[derive(Clone, Debug)]
pub struct DevOptions {
    pub dir: PathBuf,
    pub search: Vec<PathBuf>,
    /// A running nested session to develop in: its ready file, runtime directory or control
    /// socket. Without one the package runs as an ordinary window on this desktop.
    pub session: Option<PathBuf>,
    /// A tilolang runtime binary (a build of `apps/tilolang`) to run the apps on.
    pub runtime: Option<PathBuf>,
    /// Build the runtime from the checkout and rebuild it on every change of its sources. A
    /// local run does this when it is not given a runtime binary.
    pub build_runtime: bool,
    /// The key the runtime bundle is signed with (default: the session's runtime publisher).
    pub runtime_identity: Option<PathBuf>,
    /// The Tilo checkout whose build has the session's tools.
    pub repo: Option<PathBuf>,
}

/// What wakes the loop: a file event, or the end of the session the tool started.
pub(crate) enum Wake {
    File(Vec<WatchEvent>),
    Ended(std::io::Result<std::process::ExitStatus>),
}

pub(crate) fn ended(status: std::io::Result<std::process::ExitStatus>) -> Result<(), String> {
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("the nested session ended ({status})")),
        Err(error) => Err(error.to_string()),
    }
}

/// The version of the runtime bundle `--runtime` makes: the tool's tilolang runtime version
/// with `+dev` for its build.
fn dev_runtime_version() -> String {
    format!("{}+dev", env!("CARGO_PKG_VERSION"))
}

/// The package id of the development runtime: its own, so installing it never touches the
/// tilolang runtime package a system has.
const DEV_RUNTIME_PACKAGE: &str = "so.tilo.runtime.tilolang.dev";

/// A tilolang runtime binary the apps run on.
struct DevRuntime {
    /// The binary; `None` until the tool built it.
    binary: Option<PathBuf>,
    /// The checkout the tool builds `binary` in, on every change of its sources; without one
    /// the tool follows a binary built by hand.
    built_in: Option<PathBuf>,
    /// The key a session's runtime bundle is signed with: one its launcher trusts.
    identity_file: Option<PathBuf>,
    /// The version a session's runtime bundle offers.
    version: String,
}

impl DevRuntime {
    fn binary(&self) -> Option<&Path> {
        self.binary.as_deref()
    }

    fn built_in(&self) -> Option<&Path> {
        self.built_in.as_deref()
    }
}

/// Where a build goes: into each session of a running one, or into the app the tool runs.
enum Delivery {
    Session { peers: Vec<Peer>, bin: PathBuf },
    Local(Local),
}

/// The app the tool runs itself on this desktop.
struct Local {
    /// The generations the app reads its later builds from.
    updates: PathBuf,
    /// The app's own session directory: its checkpoint, and the signal it flushes it on.
    session: PathBuf,
    /// The package the window shows, as its id.
    package: String,
    /// The process showing it.
    child: Option<Child>,
}

impl Local {
    /// The local run of the package in `root`: a fresh set of generations, and a session
    /// directory of its own.
    fn new(root: &Path) -> Result<Self, String> {
        let updates = root.join("updates");
        let _ = std::fs::remove_dir_all(&updates);
        tilo_app_module::make_dir(&updates)
            .map_err(|error| format!("{}: {error}", updates.display()))?;
        let session = root.join("session");
        std::fs::create_dir_all(&session)
            .map_err(|error| format!("{}: {error}", session.display()))?;
        Ok(Self {
            updates,
            session,
            package: String::new(),
            child: None,
        })
    }

    /// Publish `app` as the first generation and run it on `runtime`.
    fn start(&mut self, app: &BuiltApp, runtime: &Path) -> Result<(), String> {
        self.package.clone_from(&app.app);
        let generation = tilo_app_module::publish(&self.updates, &app.payload)
            .map_err(|error| format!("{}: {error}", self.updates.display()))?;
        self.spawn(generation, runtime)
    }

    /// Run the app on generation `generation` of `runtime`.
    fn spawn(&mut self, generation: u64, runtime: &Path) -> Result<(), String> {
        let mut command = Command::new(runtime);
        // The developer's own desktop, not a session's: nothing the tool inherited from one
        // reaches the app.
        let inherited: Vec<std::ffi::OsString> = std::env::vars_os()
            .map(|(name, _)| name)
            .filter(|name| {
                let name = name.to_string_lossy();
                SESSION_ENV_PREFIXES
                    .iter()
                    .any(|prefix| name.starts_with(*prefix))
                    || SESSION_ENV.iter().any(|known| *known == &*name)
            })
            .collect();
        for name in inherited {
            command.env_remove(name);
        }
        command
            .env(
                tilolang_tilo::bundle::MODULE_ENV,
                self.updates.join(generation.to_string()),
            )
            .env(PACKAGE_ID_ENV, &self.package)
            .env(UPDATES_ENV, &self.updates)
            .env("TILO_DEV_SESSION", &self.session)
            .env("TILO_RUNTIME_STANDALONE", "1")
            .stdin(Stdio::null());
        bounded_command::without_console_window(&mut command);
        // The window ends with the loop: a signal to the tool must not leave it running, on the
        // store the next run opens.
        crate::run::end_with_this_process(&mut command);
        let child = command
            .spawn()
            .map_err(|error| format!("running {}: {error}", runtime.display()))?;
        eprintln!(
            "==> {} runs on {} (pid {})",
            self.package,
            runtime.display(),
            child.id()
        );
        self.child = Some(child);
        Ok(())
    }

    /// Restart the running app on `runtime`, on the generation it runs now: the resources and
    /// view state it flushes are back when it opens again.
    fn restart(&mut self, runtime: &Path) -> Result<(), String> {
        let generation = tilo_app_module::current(&self.updates)
            .map_err(|error| format!("{}: {error}", self.updates.display()))?
            .ok_or_else(|| "no build is published".to_owned())?;
        self.stop();
        self.spawn(generation, runtime)
    }

    /// Ask the running app to stop and wait until it has: it flushes its checkpoint on the way
    /// out.
    fn stop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(child.id())
            && let Some(pid) = rustix::process::Pid::from_raw(pid)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        }
        let _ = bounded_command::wait_or_kill(&mut child, STOP_GRACE);
    }
}

/// The development loop over one session, or over the desktop.
pub(crate) struct Dev {
    pub(crate) build: BuildOptions,
    pub(crate) identity: SigningIdentity,
    delivery: Delivery,
    /// The app the tool opens: the package, built last after what it uses.
    pub(crate) opened: String,
    /// What each app was built into last: its digest and metadata.
    delivered: HashMap<String, (String, PackageMeta)>,
    runtime: Option<DevRuntime>,
    /// What wakes the loop while it waits on the app the tool runs.
    wake: Option<Arc<WakeFd>>,
}

impl Dev {
    fn new(build: BuildOptions, identity: SigningIdentity, delivery: Delivery) -> Self {
        Self {
            build,
            identity,
            delivery,
            opened: String::new(),
            delivered: HashMap::new(),
            runtime: None,
            wake: None,
        }
    }

    /// The loop over a running session, which installs each build.
    pub(crate) fn in_session(
        build: BuildOptions,
        identity: SigningIdentity,
        peers: Vec<Peer>,
        bin: PathBuf,
    ) -> Self {
        Self::new(build, identity, Delivery::Session { peers, bin })
    }

    /// The loop over the app the tool runs itself on this desktop.
    fn on_desktop(
        build: BuildOptions,
        identity: SigningIdentity,
        root: &Path,
    ) -> Result<Self, String> {
        Ok(Self::new(
            build,
            identity,
            Delivery::Local(Local::new(root)?),
        ))
    }

    fn is_local(&self) -> bool {
        matches!(self.delivery, Delivery::Local(_))
    }

    fn local_mut(&mut self) -> Option<&mut Local> {
        match &mut self.delivery {
            Delivery::Local(local) => Some(local),
            Delivery::Session { .. } => None,
        }
    }

    /// The sessions the tool develops in.
    pub(crate) fn peers(&self) -> &[Peer] {
        match &self.delivery {
            Delivery::Session { peers, .. } => peers,
            Delivery::Local(_) => &[],
        }
    }

    /// The session's tools: `tilo-install`, `tilo-control`, `tilo-runtime-bundle`.
    pub(crate) fn bin(&self) -> Option<&Path> {
        match &self.delivery {
            Delivery::Session { bin, .. } => Some(bin),
            Delivery::Local(_) => None,
        }
    }

    /// Build, as a session that installs the apps as it starts will have them installed; the
    /// apps.
    pub(crate) fn build_for_start(&mut self) -> Result<Vec<BuiltApp>, String> {
        let apps = package::build(&self.build, &self.identity)?;
        for app in &apps {
            self.delivered
                .insert(app.app.clone(), (app.digest.clone(), app.meta.clone()));
        }
        if let Some(last) = apps.last() {
            self.opened.clone_from(&last.app);
        }
        Ok(apps)
    }

    /// Build, and take what changed to what runs it; the apps built, or the compile errors.
    fn build_and_deliver(&mut self, saved: Instant) -> Result<Vec<BuiltApp>, String> {
        let apps = package::build(&self.build, &self.identity)?;
        let built = saved.elapsed();
        let opened = self.opened.clone();
        for app in &apps {
            let previous = self.delivered.get(&app.app);
            if previous.is_some_and(|(digest, _)| *digest == app.digest) {
                continue;
            }
            let plan = previous.map(|(_, meta)| compare(meta, &app.meta));
            match &mut self.delivery {
                Delivery::Session { peers, bin } => {
                    for peer in peers.iter() {
                        peer.install(bin, &app.tapp)?;
                    }
                    report(app, plan, built, saved.elapsed(), "installed");
                }
                Delivery::Local(local) => {
                    // Only the package the window shows runs here; its payload carries the
                    // packages it uses.
                    if app.app != opened {
                        continue;
                    }
                    tilo_app_module::publish(&local.updates, &app.payload)
                        .map_err(|error| format!("{}: {error}", local.updates.display()))?;
                    report(app, plan, built, saved.elapsed(), "published");
                }
            }
            self.delivered
                .insert(app.app.clone(), (app.digest.clone(), app.meta.clone()));
        }
        if let Some(last) = apps.last() {
            self.opened.clone_from(&last.app);
        }
        Ok(apps)
    }

    /// Open the app where it has no window yet. A session indexes an app it just installed
    /// before it can open it; the desktop's app is the process the tool started.
    pub(crate) fn open(&self) -> Result<(), String> {
        let Delivery::Session { peers, bin } = &self.delivery else {
            return Ok(());
        };
        for peer in peers {
            if !peer.is_open(bin, &self.opened) {
                peer.wait_offered(bin, &self.opened)?;
                peer.open(bin, &self.opened)?;
            }
        }
        Ok(())
    }

    /// One save: build, publish or install, and say what the running app does with it.
    fn rebuild(&mut self) {
        let saved = Instant::now();
        if let Err(error) = self.build_and_deliver(saved) {
            eprintln!("{error}");
            eprintln!("==> the app keeps running the last good build");
        }
    }

    /// Bake the runtime binary into a signed bundle and install it as the development runtime
    /// package; the session's apps name it from now on.
    fn install_runtime(&mut self) -> Result<(), String> {
        let Some(runtime) = &self.runtime else {
            return Ok(());
        };
        let (Some(binary), Some(identity_file)) =
            (runtime.binary.as_ref(), runtime.identity_file.as_ref())
        else {
            return Err("the runtime has no binary to install".to_owned());
        };
        let version = runtime.version.clone();
        let started = Instant::now();
        let work = self.build.out.join("runtime");
        let bundle = work.join("bundle");
        let bin = self
            .bin()
            .ok_or_else(|| "installing the runtime needs a session".to_owned())?;
        if bundle.exists() {
            std::fs::remove_dir_all(&bundle)
                .map_err(|error| format!("{}: {error}", bundle.display()))?;
        }
        session::status(
            Command::new(bin.join("tilo-runtime-bundle"))
                .args(["build", "--binaries-only", "--version", &version, "--out"])
                .arg(&bundle)
                .arg("--include-bin")
                .arg(binary)
                // The tilolang host the session's bundle carries beside it.
                .args(
                    Some(bin.join("tilolang-host"))
                        .filter(|host| host.is_file())
                        .map(|host| [std::ffi::OsString::from("--include-bin"), host.into()])
                        .into_iter()
                        .flatten(),
                )
                .arg("--identity")
                .arg(identity_file)
                .stdout(std::process::Stdio::null()),
            "baking the runtime bundle",
        )?;
        let baked = started.elapsed();
        let tapp = runtime_package(&bundle, &version, &self.identity, &work)?;
        for peer in self.peers() {
            peer.install(bin, &tapp)?;
        }
        eprintln!(
            "==> runtime tilolang/{version}: baked in {} ms, installed in {} ms",
            baked.as_millis(),
            started.elapsed().as_millis()
        );
        self.build.tilolang_runtime = format!("tilolang/{version}");
        Ok(())
    }

    /// The runtime binary changed (or the tool just started on it): take it to the app.
    fn runtime_changed(&mut self) {
        let started = Instant::now();
        let result = if self.is_local() {
            let runtime = self
                .runtime
                .as_ref()
                .and_then(DevRuntime::binary)
                .map(Path::to_path_buf);
            match (runtime, self.local_mut()) {
                (Some(runtime), Some(local)) => local.restart(&runtime),
                _ => Ok(()),
            }
        } else {
            self.install_runtime()
                .and_then(|()| self.build_and_deliver(started))
                .and_then(|apps| self.restart(&apps))
        };
        match result {
            Ok(()) => eprintln!(
                "==> the app runs on the new runtime, {} ms after it changed",
                started.elapsed().as_millis()
            ),
            Err(error) => eprintln!("{error}"),
        }
    }

    /// Start every app of `apps` that has a window again, on what is installed now.
    fn restart(&self, apps: &[BuiltApp]) -> Result<(), String> {
        let Delivery::Session { peers, bin } = &self.delivery else {
            return Ok(());
        };
        for peer in peers {
            for app in apps {
                if peer.is_open(bin, &app.app) {
                    peer.restart(bin, &app.app)?;
                }
            }
        }
        Ok(())
    }

    /// The package directories a build reads: the ones to watch.
    fn watched_dirs(&self) -> Result<BTreeSet<PathBuf>, String> {
        Ok(crate::package_dirs(&self.build.dir, &self.build.search)?
            .into_iter()
            .collect())
    }

    fn watch(
        &self,
        wakes: &mpsc::Sender<Wake>,
        watching: &mut BTreeMap<PathBuf, ChangeWatch>,
    ) -> Result<(), String> {
        let mut dirs: Vec<(PathBuf, bool)> = self
            .watched_dirs()?
            .into_iter()
            .map(|dir| (dir, false))
            .collect();
        if let Some(runtime) = self.runtime.as_ref() {
            if let Some(repo) = runtime.built_in() {
                dirs.extend(RUNTIME_SOURCES.iter().map(|dir| (repo.join(dir), true)));
            } else if let Some(dir) = runtime.binary().and_then(Path::parent) {
                dirs.push((dir.to_path_buf(), false));
            }
        }
        for (dir, tree) in dirs {
            if let std::collections::btree_map::Entry::Vacant(slot) = watching.entry(dir) {
                let watcher = if tree {
                    Watcher::tree(slot.key())
                } else {
                    Watcher::entries(slot.key())
                };
                let watch = watch_into(watcher, wakes.clone(), self.wake.clone())
                    .map_err(|error| format!("{}: {error}", slot.key().display()))?;
                slot.insert(watch);
            }
        }
        Ok(())
    }

    /// Build the runtime binary in its checkout. A session's is thin, and a thin binary runs
    /// only on the runtime library of its build graph, so the library it was built with must be
    /// the one the session runs; the desktop's app is the ordinary build of the same sources.
    fn build_runtime(&mut self) -> Result<(), String> {
        let Some(repo) = self.runtime.as_ref().and_then(|r| r.built_in.clone()) else {
            return Ok(());
        };
        let started = Instant::now();
        let binary = if self.is_local() {
            let binary = build_local_runtime(&repo)?;
            bounded_command::make_executable(&binary)
                .map_err(|error| format!("{}: {error}", binary.display()))?;
            binary
        } else {
            let mut command = Command::new(repo.join(RUNTIME_SCRIPT));
            command.current_dir(&repo);
            session::status(&mut command, "building the runtime")?;
            let binary = repo.join(RUNTIME_BINARY);
            check_architecture(&binary)?;
            let built = repo.join(RUNTIME_LIBRARY);
            let running = self
                .bin()
                .ok_or_else(|| "building the runtime needs a session".to_owned())?
                .with_file_name("thin-app")
                .join("libruntime_dylib.so");
            let same = |a: &Path, b: &Path| -> Result<bool, String> {
                let read = |path: &Path| {
                    std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))
                };
                Ok(read(a)? == read(b)?)
            };
            if !same(&built, &running)? {
                return Err(format!(
                    "the runtime was built against another runtime library than the session runs \
                     ({}): start the session on a build of this checkout (dist/dev/nested-session.sh)",
                    running.display()
                ));
            }
            bounded_command::make_executable(&binary)
                .map_err(|error| format!("{}: {error}", binary.display()))?;
            binary
        };
        if let Some(runtime) = &mut self.runtime {
            runtime.binary = Some(binary);
        }
        eprintln!(
            "==> the runtime built in {:.1} s",
            started.elapsed().as_secs_f64()
        );
        Ok(())
    }

    /// Rebuild on every edit until the app (or the session) ends.
    pub(crate) fn follow(
        &mut self,
        wakes: &mpsc::Receiver<Wake>,
        sender: &mpsc::Sender<Wake>,
    ) -> Result<(), String> {
        if self.is_local() {
            self.follow_local(wakes, sender)
        } else {
            self.follow_session(wakes, sender)
        }
    }

    fn follow_session(
        &mut self,
        wakes: &mpsc::Receiver<Wake>,
        sender: &mpsc::Sender<Wake>,
    ) -> Result<(), String> {
        let mut watching = BTreeMap::new();
        self.watch(sender, &mut watching)?;
        loop {
            let mut events = match wakes.recv() {
                Ok(Wake::File(events)) => events,
                Ok(Wake::Ended(status)) => return ended(status),
                Err(_) => return Err("the file watcher stopped".to_owned()),
            };
            // An editor's save, or a link of the runtime, is several events: take every one
            // already queued with these.
            while let Ok(wake) = wakes.try_recv() {
                match wake {
                    Wake::Ended(status) => return ended(status),
                    Wake::File(more) => events.extend(more),
                }
            }
            self.handle(&events);
            if let Err(error) = self.watch(sender, &mut watching) {
                eprintln!("{error}");
            }
        }
    }

    /// Watch the sources while waiting on the app's process: each save is published for it to
    /// take, and the window closing ends the loop.
    fn follow_local(
        &mut self,
        wakes: &mpsc::Receiver<Wake>,
        sender: &mpsc::Sender<Wake>,
    ) -> Result<(), String> {
        let wake = Arc::new(WakeFd::new().map_err(|error| error.to_string())?);
        self.wake = Some(wake.clone());
        let mut watching = BTreeMap::new();
        self.watch(sender, &mut watching)?;
        let mut child = self
            .local_mut()
            .and_then(|local| local.child.take())
            .ok_or_else(|| "the app is not running".to_owned())?;
        loop {
            let wakeup = wait_child_or(&mut child, Some(wake.fd()), None)
                .map_err(|error| format!("waiting on the app: {error}"))?;
            match wakeup {
                Wakeup::Exited(status) => {
                    return if status.success() {
                        Ok(())
                    } else {
                        Err(format!("the app ended ({status})"))
                    };
                }
                Wakeup::Woken => {}
                // The loop waits without a deadline: it wakes for an event or the app's end.
                Wakeup::Deadline => continue,
            }
            wake.drain();
            let mut events = Vec::new();
            while let Ok(Wake::File(more)) = wakes.try_recv() {
                events.extend(more);
            }
            if events.is_empty() {
                continue;
            }
            // The deliverer may stop and start the app, so it is its owner while work happens.
            if let Some(local) = self.local_mut() {
                local.child = Some(child);
            }
            self.handle(&events);
            if let Err(error) = self.watch(sender, &mut watching) {
                eprintln!("{error}");
            }
            child = self
                .local_mut()
                .and_then(|local| local.child.take())
                .ok_or_else(|| "the app stopped".to_owned())?;
        }
    }

    /// What a batch of file events asks for: a build, a rebuild of the runtime, or both.
    fn handle(&mut self, events: &[WatchEvent]) {
        let mut source = false;
        let mut runtime = false;
        let mut runtime_source = false;
        for event in events {
            source |= is_source_edit(event);
            runtime |= self.is_runtime_change(event);
            runtime_source |= self.is_runtime_source_edit(event);
        }
        if !source && !runtime && !runtime_source {
            return;
        }
        if runtime_source {
            match self.build_runtime() {
                Ok(()) => self.runtime_changed(),
                Err(error) => {
                    eprintln!("{error}");
                    eprintln!("==> the app keeps running on the last good runtime");
                }
            }
        } else if runtime {
            self.runtime_changed();
        } else {
            self.rebuild();
        }
    }

    /// A new binary of a runtime built by hand.
    fn is_runtime_change(&self, event: &WatchEvent) -> bool {
        let Some(binary) = self
            .runtime
            .as_ref()
            .filter(|runtime| runtime.built_in.is_none())
            .and_then(DevRuntime::binary)
        else {
            return false;
        };
        matches!(event.kind, ChangeKind::Created | ChangeKind::Changed) && event.path == binary
    }

    /// An edit of the sources of a runtime the tool builds.
    fn is_runtime_source_edit(&self, event: &WatchEvent) -> bool {
        let Some(repo) = self.runtime.as_ref().and_then(DevRuntime::built_in) else {
            return false;
        };
        let path = &event.path;
        event.kind != ChangeKind::Rescan
            && RUNTIME_SOURCES
                .iter()
                .any(|dir| path.starts_with(repo.join(dir)))
            && (path.extension().is_some_and(|extension| extension == "rs")
                || path.file_name().is_some_and(|name| name == "Cargo.toml"))
    }
}

/// Say what the running app does with `app`, whose build took `built` and its delivery
/// `delivered`, `how` being what the delivery did with it.
fn report(app: &BuiltApp, plan: Option<Upgrade>, built: Duration, delivered: Duration, how: &str) {
    match plan {
        None => eprintln!(
            "==> {}: built in {} ms, {how} in {} ms",
            app.title,
            built.as_millis(),
            delivered.as_millis()
        ),
        Some(Upgrade::InPlace) => eprintln!(
            "==> {}: built in {} ms, {how} in {} ms; the running app takes it with its state",
            app.title,
            built.as_millis(),
            delivered.as_millis()
        ),
        Some(Upgrade::Restart(reason)) => eprintln!(
            "==> {}: {reason}; {how} in {} ms, the running app starts over on it with its data",
            app.title,
            delivered.as_millis()
        ),
    }
}

/// Whether `event` changed a package's source, or changes may have been missed.
pub(crate) fn is_source_edit(event: &WatchEvent) -> bool {
    event.kind == ChangeKind::Rescan
        || event
            .path
            .extension()
            .is_some_and(|extension| extension == "tl")
}

/// Start `watcher`, its changes sent to `wakes`; with a `ring`, the wait on the app is woken
/// too.
pub(crate) fn watch_into(
    watcher: Watcher,
    wakes: mpsc::Sender<Wake>,
    ring: Option<Arc<WakeFd>>,
) -> std::io::Result<ChangeWatch> {
    watcher.start(move |events| {
        let _ = wakes.send(Wake::File(events.to_vec()));
        if let Some(ring) = &ring {
            ring.ring();
        }
    })
}

/// The signed runtime package that offers the baked bundle in `bundle` as `tilolang/<version>`,
/// written into `work`.
fn runtime_package(
    bundle: &Path,
    version: &str,
    identity: &SigningIdentity,
    work: &Path,
) -> Result<PathBuf, String> {
    let isa = match std::env::consts::ARCH {
        "x86_64" => "x86_64 = \"v3\"",
        "aarch64" => "aarch64 = \"aarch64\"",
        other => return Err(format!("no runtime package for the architecture {other}")),
    };
    let publisher = hex::encode(identity.public_key_bytes());
    let text = format!(
        r#"# Generated by `tilolang dev --runtime`: a tilolang runtime of the developer's own build.

[package]
id = "{DEV_RUNTIME_PACKAGE}"
name = "tilolang development runtime"
category = "System"
version = "{package_version}"
kind = "runtime"
provides = "tilolang/{version}"

[package.isa]
{isa}

[publisher]
identity = "ed25519:{publisher}"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
type = "cli"
bin = "{bin}"
"#,
        package_version = env!("CARGO_PKG_VERSION"),
        bin = package::RUNTIME_BIN,
    );
    let manifest = tilo_app_package::manifest::Manifest::from_toml_str(&text)
        .map_err(|error| format!("the runtime package's manifest: {error}"))?;
    let packed = tilo_app_package::pack::pack(bundle, &manifest, identity)
        .map_err(|error| format!("packing the runtime: {error}"))?;
    let tapp = work.join("tilolang-dev-runtime.tapp");
    package::write_tapp(&tapp, &packed)?;
    Ok(tapp)
}

/// Build the tilolang runtime binary in `repo` as an ordinary program, and say where it is.
fn build_local_runtime(repo: &Path) -> Result<PathBuf, String> {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .current_dir(repo)
        .args([
            "build",
            "--locked",
            "-p",
            RUNTIME_PACKAGE,
            "--bin",
            RUNTIME_PACKAGE,
            "--message-format=json-render-diagnostics",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let output = command
        .output()
        .map_err(|error| format!("building the runtime: {error}"))?;
    if !output.status.success() {
        return Err(format!("building the runtime: {}", output.status));
    }
    let binary = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| {
            message.get("reason").and_then(serde_json::Value::as_str) == Some("compiler-artifact")
                && message
                    .pointer("/target/name")
                    .and_then(serde_json::Value::as_str)
                    == Some(RUNTIME_PACKAGE)
        })
        .filter_map(|message| {
            message
                .get("executable")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
        })
        .next_back();
    binary.ok_or_else(|| "the runtime build made no tilolang-app binary".to_owned())
}

/// Develop the package in the running session `options.session`, or on this desktop when there
/// is none, until interrupted.
///
/// # Errors
/// The session is not running, the package does not compile at the start, or a tool of the
/// session fails.
pub fn dev(options: &DevOptions) -> Result<(), String> {
    let repo = session::repo_root(options.repo.as_deref(), &options.dir)?;
    let identity = crate::identity_at(&crate::state_dir().join("publisher.json"))?;
    let (sender, wakes) = mpsc::channel();
    match &options.session {
        Some(session) => dev_in_session(options, &repo, session, identity, &sender, &wakes),
        None => dev_on_desktop(options, &repo, identity, &sender, &wakes),
    }
}

fn dev_in_session(
    options: &DevOptions,
    repo: &Path,
    session: &Path,
    identity: SigningIdentity,
    sender: &mpsc::Sender<Wake>,
    wakes: &mpsc::Receiver<Wake>,
) -> Result<(), String> {
    let bin = session::tools(repo)?;
    let peer = Peer::attach(session, repo)?;
    let (tilo_runtime, tilolang_runtime) = peer.runtimes_baked()?;
    let built_in = options.build_runtime.then(|| repo.to_path_buf());
    let binary = options
        .runtime
        .clone()
        .or_else(|| built_in.as_ref().map(|repo| repo.join(RUNTIME_BINARY)));
    let runtime = binary.map(|binary| DevRuntime {
        binary: Some(std::path::absolute(&binary).unwrap_or(binary)),
        built_in,
        identity_file: Some(
            options
                .runtime_identity
                .clone()
                .unwrap_or_else(|| peer.runtime_publisher()),
        ),
        version: dev_runtime_version(),
    });
    let build = BuildOptions {
        dir: options.dir.clone(),
        search: options.search.clone(),
        out: dev_build_dir(&peer.state, &options.dir)?,
        tilo_runtime,
        tilolang_runtime,
        version: "0.1.0".to_owned(),
    };
    let mut dev = Dev::in_session(build, identity, vec![peer], bin);
    dev.runtime = runtime;
    if dev.runtime.is_some() {
        dev.build_runtime()?;
        dev.install_runtime()?;
    }
    let apps = dev.build_and_deliver(Instant::now())?;
    if dev.runtime.is_some() {
        dev.restart(&apps)?;
    }
    dev.open()?;
    eprintln!(
        "==> {} is open; a saved .tl file reaches it{}",
        dev.opened,
        if dev.runtime.is_some() {
            ", and so does a new build of the runtime"
        } else {
            ""
        }
    );
    dev.follow(wakes, sender)
}

/// Run the package as an ordinary window on this desktop, and publish every saved build into
/// the directory it follows, until its window closes.
fn dev_on_desktop(
    options: &DevOptions,
    repo: &Path,
    identity: SigningIdentity,
    sender: &mpsc::Sender<Wake>,
    wakes: &mpsc::Receiver<Wake>,
) -> Result<(), String> {
    let root = dev_build_dir(&crate::state_dir(), &options.dir)?;
    let built_in = (options.build_runtime || options.runtime.is_none()).then(|| repo.to_path_buf());
    let runtime = options
        .runtime
        .clone()
        .map(|binary| std::path::absolute(&binary).unwrap_or(binary));
    let build = BuildOptions {
        dir: options.dir.clone(),
        search: options.search.clone(),
        out: root.clone(),
        tilo_runtime: package::DEFAULT_TILO_RUNTIME.to_owned(),
        tilolang_runtime: package::DEFAULT_TILOLANG_RUNTIME.to_owned(),
        version: "0.1.0".to_owned(),
    };
    let mut dev = Dev::on_desktop(build, identity, &root)?;
    dev.runtime = Some(DevRuntime {
        binary: runtime,
        built_in,
        identity_file: None,
        version: dev_runtime_version(),
    });
    if dev
        .runtime
        .as_ref()
        .and_then(DevRuntime::built_in)
        .is_some()
    {
        dev.build_runtime()?;
    }
    let binary = dev
        .runtime
        .as_ref()
        .and_then(DevRuntime::binary)
        .map(Path::to_path_buf)
        .ok_or_else(|| "no runtime binary to run the app on".to_owned())?;
    let apps = dev.build_for_start()?;
    let app = apps
        .iter()
        .find(|app| app.app == dev.opened)
        .ok_or_else(|| format!("{} was not built", dev.opened))?;
    let local = dev
        .local_mut()
        .ok_or_else(|| "the app is not run here".to_owned())?;
    local.start(app, &binary)?;
    eprintln!(
        "==> {} is open; a saved .tl file reaches it, and so does a new build of the runtime",
        dev.opened
    );
    dev.follow(wakes, sender)
}

/// Where `tilolang dev` of the package in `dir` builds into the state whose root is `state`: a
/// directory of its own per package, so several developed at once (each with the packages it
/// uses, which they may share) never write over each other's builds.
fn dev_build_dir(state: &Path, dir: &Path) -> Result<PathBuf, String> {
    use sha2::Digest as _;
    let package =
        std::fs::canonicalize(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let digest = sha2::Sha256::digest(package.as_os_str().as_encoded_bytes());
    let short = hex::encode(digest.get(..6).unwrap_or_default());
    let name = package
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(state.join("dev-build").join(format!("{name}-{short}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two packages developed at once build apart; the same package always builds where it did
    /// before.
    #[test]
    fn each_developed_package_builds_in_its_own_directory() {
        let checkout = tempfile::tempdir().unwrap();
        let (notes, tasks) = (checkout.path().join("notes"), checkout.path().join("tasks"));
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::create_dir_all(&tasks).unwrap();
        let state = Path::new("/session/state");
        let one = dev_build_dir(state, &notes).unwrap();
        assert_ne!(one, dev_build_dir(state, &tasks).unwrap());
        assert_eq!(one, dev_build_dir(state, &notes.join(".")).unwrap());
        assert!(
            one.starts_with(state.join("dev-build")),
            "{}",
            one.display()
        );
    }

    #[test]
    fn a_build_of_another_architecture_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let this = std::env::current_exe().unwrap();
        check_architecture(&this).unwrap();
        let mut other = std::fs::read(&this).unwrap();
        let foreign: u16 = if cfg!(target_arch = "aarch64") {
            62
        } else {
            183
        };
        other[18..20].copy_from_slice(&foreign.to_le_bytes());
        let path = dir.path().join("tilolang-app");
        std::fs::write(&path, &other).unwrap();
        let error = check_architecture(&path).unwrap_err();
        assert!(error.contains("is not built for"), "{error}");
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n# not an ELF file at all").unwrap();
        assert!(check_architecture(&path).is_err());
    }
}
