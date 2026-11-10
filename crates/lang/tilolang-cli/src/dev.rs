//! `tilolang dev`: edit a package and see the running app change, with its state.
//!
//! The tool watches the package's sources (and those of the packages it uses). On every save it
//! compiles in-process; a compile error is printed and the app keeps running the last good
//! build. A save is anything that changes what a build reads: a `.tl` file, a translation in the
//! package's `locales/`, an entry added or removed anywhere below the package's directory. The
//! packages are watched as trees, and each directory's own directory with them, so a package or
//! a build's output directory made, replaced or moved in is a change too.
//!
//! Without `--session` the package runs as an ordinary window on this desktop, started here: the
//! tool publishes each build into a directory of numbered generations
//! (`tilo_app_module`, `TILO_APP_MODULE_UPDATES`) and runs `tilolang-app` on this generation, as
//! a person's own app, with no session. The app's record is its local store, kept across saves,
//! process restarts and later runs. The running app takes a new generation on the same store and
//! windows; when it cannot read the state the old one wrote ([`tilolang_tilo::upgrade`]), the
//! tool says why in one line and the app starts over on the new build with its data, as its next
//! start would. The runtime binary is `--runtime BIN`, or a build of `apps/tilolang` the tool
//! makes with cargo and remakes on every change of its sources, restarting the window on it. A
//! binary built by hand is taken when the file it was written to is whole again, by whichever
//! of the events a build makes: the tool watches the binary's directory, and the one above it,
//! and starts the window again on the new program once it has stopped growing.
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

/// The lib target of the runtime package: what a rebuild of the runtime's sources is linked
/// into.
const RUNTIME_LIBRARY_TARGET: &str = "tilolang_app";

/// The crate of the shared toolkit a thin build links against.
const RUNTIME_DYLIB: &str = "runtime_dylib";

/// The build of the tilolang runtime binary alone, thin, against the runtime library a nested
/// session runs, for this machine's architecture: a binary of a few megabytes, as the session's
/// own runtime bundle carries.
const RUNTIME_SCRIPT: &str = "dist/dev/tilolang-runtime-build.sh";

/// What a change of the runtime's sources cannot swap into a running window: the startup code
/// the process ran once, and the manifest of what the binary links.
const RUNTIME_STARTUP: [&str; 2] = ["apps/tilolang/src/main.rs", "apps/tilolang/Cargo.toml"];

/// The features a locally built runtime is built with: `thin` links the toolkit and `libstd`
/// from one shared library, which the process that owns the window maps and keeps while the
/// app's own code is linked again beside it.
#[cfg(unix)]
const RUNTIME_FEATURES: &str = "thin";

/// The profile of a locally built runtime: the desktop's, without LTO and with incremental
/// builds, and `panic = "unwind"` as a shared library needs.
const LOCAL_PROFILE: &str = "fast";

/// Both the host and the reload library link the toolkit dynamically.
#[cfg(unix)]
const PREFER_DYNAMIC: [&str; 2] = ["-C", "prefer-dynamic"];

/// The file the tool writes the path of the library the running window takes, and the one the
/// window writes beside a staged library once its first frame is presented.
const RELOAD_PATH_FILE: &str = "reload.path";
const RELOAD_TIMINGS_SUFFIX: &str = ".timings.json";

/// Where the thin build lands in the checkout's target directory: the binary and the runtime
/// library it links.
const RUNTIME_BINARY: &str = "target/tilolang-runtime/tilolang-app";
const RUNTIME_LIBRARY: &str = "target/tilolang-runtime/libruntime_dylib.so";

/// How long the app gets to flush its checkpoint when the tool restarts or stops it.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// How long the loop gathers the events of one change before it acts on them: an editor's save,
/// and a build that writes a file in steps, arrive as several events, and one build is one
/// rebuild or one restart.
const SETTLE: Duration = Duration::from_millis(60);

/// The longest a change is gathered: a build writing into a watched tree must not hold the loop.
const SETTLE_LIMIT: Duration = Duration::from_millis(500);

/// How often a file a build replaced is measured while it is being written.
const COMPLETE_POLL: Duration = Duration::from_millis(20);

/// How long a file a build replaced is given to be whole. A build elsewhere copies a large
/// runtime binary here over several seconds.
const COMPLETE_WAIT: Duration = Duration::from_secs(60);

/// How long a program a build took away is waited for: a build that replaces one takes it away
/// for a moment at most.
const MISSING_WAIT: Duration = Duration::from_secs(2);

/// The sources of the tool itself: the checkout's answers, which can be newer than the tool.
const TOOL_SOURCES: [&str; 4] = [
    "crates/lang/tilolang/src",
    "crates/lang/tilolang-runtime/src",
    "crates/platform/tilolang-tilo/src",
    "apps/tilolang/src",
];

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
    /// Whether the window can take a rebuild of the runtime in place: the process stays and maps
    /// the new code of the app. It cannot for a binary built by hand, nor where the window's
    /// platform has no in-place reload.
    reloads_in_place: bool,
    /// The shared toolkit and `libstd` the thin build's programs link: where the running
    /// process finds them. Empty for a build that links them in.
    loaders: Vec<PathBuf>,
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
        // A run of the package before this one handed a library to the process it had; this
        // process is not that one.
        let _ = std::fs::remove_file(session.join(RELOAD_PATH_FILE));
        Ok(Self {
            updates,
            session,
            package: String::new(),
            child: None,
        })
    }

    /// Publish `app` as the first generation and run it on `runtime`.
    fn start(
        &mut self,
        app: &package::Published,
        runtime: &Path,
        loaders: &[PathBuf],
    ) -> Result<(), String> {
        self.package.clone_from(&app.app);
        let generation = tilo_app_module::publish_with(&self.updates, |generation| {
            app.bundle
                .write(generation)
                .map_err(|error| std::io::Error::other(error.to_string()))
        })
        .map_err(|error| format!("{}: {error}", self.updates.display()))?;
        self.spawn(generation, runtime, loaders)
    }

    /// Run the app on generation `generation` of `runtime`. `loaders` are the directories the
    /// program finds the shared toolkit and `libstd` in, when it links them dynamically.
    fn spawn(
        &mut self,
        generation: u64,
        runtime: &Path,
        loaders: &[PathBuf],
    ) -> Result<(), String> {
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
        attach_loader_path(&mut command, runtime, loaders)?;
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
    fn restart(&mut self, runtime: &Path, loaders: &[PathBuf]) -> Result<(), String> {
        let generation = self.published()?;
        self.stop();
        self.spawn(generation, runtime, loaders)
    }

    /// Hand the running window `library`, a new build of the runtime, and ask it to take it:
    /// the process stays, so the window, its size and its place do. Answers where it staged
    /// the library, and when it did.
    #[cfg(unix)]
    fn reload(&self, library: &Path) -> Result<(PathBuf, std::time::SystemTime), String> {
        let staged = stage_reload_library(&self.session, library)?;
        let ready = std::time::SystemTime::now();
        let child = self
            .child
            .as_ref()
            .ok_or_else(|| "the app is not running".to_owned())?;
        let pid = i32::try_from(child.id())
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .ok_or_else(|| "the app's process has no id".to_owned())?;
        rustix::process::kill_process(pid, rustix::process::Signal::USR1)
            .map_err(|error| format!("asking the window to take the new runtime: {error}"))?;
        Ok((staged, ready))
    }

    /// The build of the package the app runs now.
    fn published(&self) -> Result<u64, String> {
        tilo_app_module::current(&self.updates)
            .map_err(|error| format!("{}: {error}", self.updates.display()))?
            .ok_or_else(|| "no build is published".to_owned())
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

/// Point `command` at where a dynamically linked program finds the toolkit it shares with the
/// process that maps it, and `libstd`: the program's own directory, the dependencies beside it,
/// the checkout's compiler libraries, and what the developer's own shell already named.
fn attach_loader_path(
    command: &mut Command,
    program: &Path,
    loaders: &[PathBuf],
) -> Result<(), String> {
    if loaders.is_empty() {
        return Ok(());
    }
    let mut dirs: Vec<PathBuf> = program
        .parent()
        .map(Path::to_path_buf)
        .into_iter()
        .collect();
    if let Some(parent) = program.parent() {
        dirs.push(parent.join("deps"));
    }
    dirs.extend(loaders.iter().cloned());
    let mut value = std::env::join_paths(&dirs)
        .map_err(|error| format!("the search path of the runtime's shared libraries: {error}"))?;
    // What the shell named is read as one value, not as another path of ours to join: it may
    // name several directories already.
    if let Some(existing) =
        std::env::var_os(loader_path_variable()).filter(|value| !value.is_empty())
    {
        value.push(path_separator());
        value.push(existing);
    }
    command.env(loader_path_variable(), value);
    Ok(())
}

/// The separator between the directories of a search path on this platform.
const fn path_separator() -> &'static str {
    if cfg!(windows) { ";" } else { ":" }
}

/// The variable the platform's loader reads its search path from.
const fn loader_path_variable() -> &'static str {
    if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else if cfg!(windows) {
        "PATH"
    } else {
        "LD_LIBRARY_PATH"
    }
}

/// Copy `library` to a new file in the dev session `session` and point `reload.path` at it: a
/// new path is what makes the running process read this build rather than one it mapped
/// before. The convention is the one `tilo-app dev` hands a running app its library with.
fn stage_reload_library(session: &Path, library: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(session).map_err(|error| format!("{}: {error}", session.display()))?;
    let counter = session.join("reload.seq");
    let seq = std::fs::read_to_string(&counter)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(0)
        .saturating_add(1);
    std::fs::write(&counter, seq.to_string())
        .map_err(|error| format!("{}: {error}", counter.display()))?;
    let destination = session.join(format!("reload-{seq}.so"));
    std::fs::copy(library, &destination).map_err(|error| {
        format!(
            "copying {} to {}: {error}",
            library.display(),
            destination.display()
        )
    })?;
    let pointer = session.join(RELOAD_PATH_FILE);
    std::fs::write(&pointer, format!("{}\n", destination.display()))
        .map_err(|error| format!("{}: {error}", pointer.display()))?;
    remove_superseded_libraries(session, seq);
    Ok(destination)
}

/// Drop every staged library older than the one before `current`: a process that mapped one
/// keeps it after the file goes, and only ever reads the one the pointer names.
fn remove_superseded_libraries(session: &Path, current: u64) {
    let Ok(entries) = std::fs::read_dir(session) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let superseded = name
            .to_str()
            .and_then(|name| {
                let name = name.strip_prefix("reload-")?;
                name.strip_suffix(RELOAD_TIMINGS_SUFFIX)
                    .unwrap_or(name)
                    .strip_suffix(".so")
            })
            .and_then(|seq| seq.parse::<u64>().ok())
            .is_some_and(|seq| seq.saturating_add(1) < current);
        if superseded {
            let _ = std::fs::remove_file(entry.path());
        }
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
    /// The library the running window was handed, and when the build was ready: the window
    /// says how long it took to draw it.
    reload_pending: Option<(PathBuf, std::time::SystemTime)>,
    /// What wakes the loop while it waits on the app the tool runs.
    wake: Option<Arc<WakeFd>>,
    /// The checkout the tool was built from: whose sources being newer than the tool mean the
    /// tool cannot know all the types they declare.
    repo: PathBuf,
    /// The package directories a build reads: every change of what is below one is a build.
    sources: BTreeSet<PathBuf>,
    /// The program of a runtime built by hand that the window was last started on.
    runtime_taken: Option<Program>,
}

impl Dev {
    fn new(
        build: BuildOptions,
        identity: SigningIdentity,
        delivery: Delivery,
        repo: &Path,
    ) -> Self {
        Self {
            build,
            identity,
            delivery,
            opened: String::new(),
            delivered: HashMap::new(),
            runtime: None,
            reload_pending: None,
            wake: None,
            repo: repo.to_path_buf(),
            sources: BTreeSet::new(),
            runtime_taken: None,
        }
    }

    /// The loop over a running session, which installs each build.
    pub(crate) fn in_session(
        build: BuildOptions,
        identity: SigningIdentity,
        peers: Vec<Peer>,
        bin: PathBuf,
        repo: &Path,
    ) -> Self {
        Self::new(build, identity, Delivery::Session { peers, bin }, repo)
    }

    /// The loop over the app the tool runs itself on this desktop.
    fn on_desktop(
        build: BuildOptions,
        identity: SigningIdentity,
        root: &Path,
        repo: &Path,
    ) -> Result<Self, String> {
        Ok(Self::new(
            build,
            identity,
            Delivery::Local(Local::new(root)?),
            repo,
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

    /// The app the tool runs itself, when it does.
    fn local(&self) -> Option<&Local> {
        match &self.delivery {
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

    /// Build the bundles the desktop app reads from its generations, as starting it will have
    /// them published; the apps.
    pub(crate) fn build_for_start_local(&mut self) -> Result<Vec<package::Published>, String> {
        let apps = package::published(&self.build, &self.identity)?;
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
        if self.is_local() {
            self.publish_local(saved)?;
            return Ok(Vec::new());
        }
        let apps = package::build(&self.build, &self.identity)?;
        let built = saved.elapsed();
        for app in &apps {
            let previous = self.delivered.get(&app.app);
            if previous.is_some_and(|(digest, _)| *digest == app.digest) {
                continue;
            }
            let plan = previous.map(|(_, meta)| compare(meta, &app.meta));
            let Delivery::Session { peers, bin } = &mut self.delivery else {
                return Ok(Vec::new());
            };
            for peer in peers.iter() {
                peer.install(bin, &app.tapp)?;
            }
            report(&app.title, plan, built, saved.elapsed(), "installed");
            self.delivered
                .insert(app.app.clone(), (app.digest.clone(), app.meta.clone()));
        }
        if let Some(last) = apps.last() {
            self.opened.clone_from(&last.app);
        }
        Ok(apps)
    }

    /// Build, and publish each changed build of the app the window shows into the generations it
    /// follows. Nothing is staged, packed or signed: the app reads the bundle itself.
    fn publish_local(&mut self, saved: Instant) -> Result<(), String> {
        let opened = self.opened.clone();
        let apps = package::published(&self.build, &self.identity)?;
        let built = saved.elapsed();
        for app in &apps {
            let previous = self.delivered.get(&app.app);
            if previous.is_some_and(|(digest, _)| *digest == app.digest) {
                continue;
            }
            // Only the package the window shows runs here; its bundle carries the packages it
            // uses.
            if app.app != opened {
                continue;
            }
            let plan = previous.map(|(_, meta)| compare(meta, &app.meta));
            let local = self
                .local_mut()
                .ok_or_else(|| "the app is not run here".to_owned())?;
            tilo_app_module::publish_with(&local.updates, |generation| {
                app.bundle
                    .write(generation)
                    .map_err(|error| std::io::Error::other(error.to_string()))
            })
            .map_err(|error| format!("{}: {error}", local.updates.display()))?;
            report(&app.title, plan, built, saved.elapsed(), "published");
            self.delivered
                .insert(app.app.clone(), (app.digest.clone(), app.meta.clone()));
        }
        if let Some(last) = apps.last() {
            self.opened.clone_from(&last.app);
        }
        Ok(())
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
            if let Some(hint) = self.build_failure_hint(&error) {
                eprintln!("==> {hint}");
            }
            eprintln!("==> the app keeps running the last good build");
        }
    }

    /// What a build's errors most likely mean beyond the errors themselves: a type the
    /// checkout's sources declare, which this build of the tool is too old to know, is answered
    /// by rebuilding the tool, not by editing the package.
    fn build_failure_hint(&self, error: &str) -> Option<&'static str> {
        (error.contains("E013") && self.older_than_sources())
            .then_some("the tilolang CLI is older than the sources: rebuild it and start dev again")
    }

    /// Whether the checkout's sources of the tool are newer than the tool itself.
    fn older_than_sources(&self) -> bool {
        let Ok(built) = std::env::current_exe()
            .and_then(std::fs::metadata)
            .and_then(|meta| meta.modified())
        else {
            return false;
        };
        TOOL_SOURCES
            .iter()
            .filter_map(|dir| newest_source(&self.repo.join(dir)))
            .any(|newest| newest > built)
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
    fn runtime_changed(&mut self, restart: bool) {
        let started = Instant::now();
        let result = if self.is_local() {
            if restart {
                self.restart_here(started)
            } else {
                self.changed_here(started)
            }
        } else {
            self.install_runtime()
                .and_then(|()| self.build_and_deliver(started))
                .and_then(|apps| self.restart(&apps))
                .map(|()| {
                    eprintln!(
                        "==> the app runs on the new runtime, {} ms after it changed",
                        started.elapsed().as_millis()
                    );
                })
        };
        if let Err(error) = result {
            eprintln!("{error}");
        } else {
            self.take_note_of_runtime();
        }
    }

    /// Take the new code of the runtime to the desktop's app. The window's process keeps the
    /// window: it maps the new build of the app's own code and draws that, so the window is the
    /// same one, at the same place, showing the same document. A runtime built by hand cannot be
    /// taken that way, and starts the process again on it.
    fn changed_here(&mut self, started: Instant) -> Result<(), String> {
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(());
        };
        let reloads_in_place = runtime.reloads_in_place;
        let built_here = runtime.built_in.clone().filter(|_| reloads_in_place);
        let Some(repo) = built_here else {
            return self.restart_here(started);
        };
        let library = build_local_library(&repo)?;
        let built = started.elapsed().as_millis();
        let local = self
            .local()
            .ok_or_else(|| "the app is not run here".to_owned())?;
        let (staged, ready) = local.reload(&library)?;
        eprintln!("==> the runtime's new code is built in {built} ms; the window takes it");
        self.reload_pending = Some((staged, ready));
        Ok(())
    }

    /// Start the app's window again on the runtime as it is now: the process ran its startup
    /// code already, or the runtime is a binary the tool did not build and cannot swap into it.
    /// The app flushes its state on the way out, and its next window opens on it.
    fn restart_here(&mut self, started: Instant) -> Result<(), String> {
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(());
        };
        let binary = runtime
            .binary
            .clone()
            .ok_or_else(|| "no runtime binary to run the app on".to_owned())?;
        let loaders = runtime.loaders.clone();
        let local = self
            .local_mut()
            .ok_or_else(|| "the app is not run here".to_owned())?;
        local.restart(&binary, &loaders)?;
        eprintln!(
            "==> the app runs on the new runtime, {} ms after it changed",
            started.elapsed().as_millis()
        );
        Ok(())
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

    /// Watch what a build reads, the runtime, and the session, so that every change of them
    /// wakes the loop. A watch whose directory is gone, or was replaced ([`ends_watch`]), is let
    /// go before this and taken again once the directory is there: watching the directory above
    /// each one is what reports that it is back.
    fn watch(
        &mut self,
        wakes: &mpsc::Sender<Wake>,
        watching: &mut BTreeMap<PathBuf, ChangeWatch>,
    ) -> Result<(), String> {
        self.sources = self.watched_dirs()?;
        // The package's own directory, and the one above it, where the package itself may be
        // made, replaced or moved in.
        let mut dirs: Vec<(PathBuf, bool)> = Vec::new();
        for dir in &self.sources {
            dirs.push((dir.clone(), true));
            dirs.extend(dir.parent().map(|above| (above.to_path_buf(), false)));
        }
        if let Some(runtime) = self.runtime.as_ref() {
            if let Some(repo) = runtime.built_in() {
                dirs.extend(RUNTIME_SOURCES.iter().map(|dir| (repo.join(dir), true)));
            } else if let Some(dir) = runtime.binary().and_then(Path::parent) {
                // The binary's directory, and the one above it, where a build may replace the
                // directory itself.
                dirs.push((dir.to_path_buf(), false));
                dirs.extend(dir.parent().map(|above| (above.to_path_buf(), false)));
            }
        }
        // The running window writes what a rebuild it was handed cost it beside the library:
        // the loop reads that to say when the window drew it.
        if let Some(session) = self.local().map(|local| local.session.clone()) {
            dirs.push((session, false));
        }
        // A watch of a directory that went away ends: it is taken again below when the
        // directory is back, and until then what is below it is not watched.
        watching.retain(|dir, _| dir.is_dir());
        for (dir, tree) in dirs {
            if !dir.is_dir() {
                continue;
            }
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

    /// Build the runtime in its checkout. A session's is thin, and a thin binary runs only on
    /// the runtime library of its build graph, so the library it was built with must be the one
    /// the session runs. The desktop's runtime is thin too, so a change of its sources is
    /// linked as the library the window's own process maps (see [`Self::changed_here`]).
    fn build_runtime(&mut self) -> Result<(), String> {
        let Some(repo) = self.runtime.as_ref().and_then(|r| r.built_in.clone()) else {
            return Ok(());
        };
        let started = Instant::now();
        let mut reloads_in_place = false;
        let mut loaders = Vec::new();
        let binary = if self.is_local() {
            let built = build_local_runtime(&repo)?;
            bounded_command::make_executable(&built.binary)
                .map_err(|error| format!("{}: {error}", built.binary.display()))?;
            reloads_in_place = built.reloads_in_place;
            loaders = built.loaders;
            built.binary
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
            runtime.reloads_in_place = reloads_in_place;
            runtime.loaders = loaders;
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
            let events = match wakes.recv() {
                Ok(Wake::File(events)) => events,
                Ok(Wake::Ended(status)) => return ended(status),
                Err(_) => return Err("the file watcher stopped".to_owned()),
            };
            let events = match Self::gather(wakes, events)? {
                Next::Changed(events) => events,
                Next::Ended(status) => return ended(status),
            };
            watching.retain(|dir, _| !ends_watch(dir, &events));
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
            let events = match Self::gather(wakes, events)? {
                Next::Changed(events) => events,
                Next::Ended(status) => return ended(status),
            };
            // The deliverer may stop and start the app, so it is its owner while work happens.
            if let Some(local) = self.local_mut() {
                local.child = Some(child);
            }
            watching.retain(|dir, _| !ends_watch(dir, &events));
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

    /// Take the events of one change: these, and every event that arrives before the change
    /// has been quiet for [`SETTLE`], or for [`SETTLE_LIMIT`] at most.
    fn gather(wakes: &mpsc::Receiver<Wake>, mut events: Vec<WatchEvent>) -> Result<Next, String> {
        let started = Instant::now();
        loop {
            match wakes.recv_timeout(SETTLE) {
                Ok(Wake::File(more)) => {
                    events.extend(more);
                    if started.elapsed() >= SETTLE_LIMIT {
                        return Ok(Next::Changed(events));
                    }
                }
                Ok(Wake::Ended(status)) => return Ok(Next::Ended(status)),
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(Next::Changed(events)),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("the file watcher stopped".to_owned());
                }
            }
        }
    }

    /// What a batch of file events asks for: a build, a rebuild of the runtime, or both.
    fn handle(&mut self, events: &[WatchEvent]) {
        let mut source = false;
        let mut runtime = false;
        let mut runtime_source = false;
        let mut startup = false;
        for event in events {
            source |= self.is_source_edit(event);
            runtime |= self.is_runtime_change(event);
            runtime_source |= self.is_runtime_source_edit(event);
            startup |= self.is_runtime_startup_edit(event);
            self.report_reload(event);
        }
        if !source && !runtime && !runtime_source {
            return;
        }
        if runtime_source && (startup || !self.reloads_in_place()) {
            match self.build_runtime() {
                Ok(()) => self.runtime_changed(true),
                Err(error) => {
                    eprintln!("{error}");
                    eprintln!("==> the app keeps running on the last good runtime");
                }
            }
        } else if runtime_source || runtime {
            match self.new_runtime_program() {
                Ok(true) => self.runtime_changed(false),
                Ok(false) => {}
                Err(error) => {
                    eprintln!("{error}");
                    eprintln!("==> the app keeps running on the last good runtime");
                }
            }
        }
        if source {
            self.rebuild();
        }
    }

    /// Whether a rebuild of the runtime's sources reaches the running window in place: the
    /// library alone is linked again, and the process that owns the window keeps it.
    fn reloads_in_place(&self) -> bool {
        self.runtime
            .as_ref()
            .is_some_and(|runtime| runtime.reloads_in_place && runtime.built_in.is_some())
    }

    /// Whether `event` changed what a build reads: anything in a package's directory, wherever
    /// it is, or changes in one that may have been missed.
    fn is_source_edit(&self, event: &WatchEvent) -> bool {
        if event.kind == ChangeKind::Rescan {
            // A rescan of a package's directory, or of the directory above it, may have hidden
            // a change of the package.
            return self
                .sources
                .iter()
                .any(|dir| event.path == *dir || dir.starts_with(&event.path));
        }
        self.sources.iter().any(|dir| event.path.starts_with(dir)) && !is_ids_lock(&event.path)
    }

    /// Wait until the binary a build by hand replaced is a whole program, and answer whether it
    /// is one the window is not running yet: the events of a change already taken restart
    /// nothing.
    fn new_runtime_program(&self) -> Result<bool, String> {
        let Some(binary) = self
            .runtime
            .as_ref()
            .filter(|runtime| runtime.built_in.is_none())
            .and_then(DevRuntime::binary)
        else {
            // A runtime the tool builds itself is what its own build made.
            return Ok(true);
        };
        Ok(whole_program(binary, MISSING_WAIT, COMPLETE_WAIT)?
            && program(binary) != self.runtime_taken)
    }

    /// Note the program the window runs now, so that the rest of its change's events ask for
    /// nothing.
    fn take_note_of_runtime(&mut self) {
        self.runtime_taken = self
            .runtime
            .as_ref()
            .filter(|runtime| runtime.built_in.is_none())
            .and_then(DevRuntime::binary)
            .and_then(program);
    }

    /// An edit of what the runtime process runs before any window exists: a running window
    /// cannot take it, so the process starts again on it. Changes that may have been missed
    /// anywhere in the runtime's sources start it again too: they may have been such an edit.
    fn is_runtime_startup_edit(&self, event: &WatchEvent) -> bool {
        let Some(repo) = self.runtime.as_ref().and_then(DevRuntime::built_in) else {
            return false;
        };
        RUNTIME_STARTUP
            .iter()
            .any(|path| event.path == repo.join(path))
            || (event.kind == ChangeKind::Rescan && self.in_runtime_sources(&event.path))
    }

    /// Say how long the window took to draw the rebuild of the runtime it was handed, once it
    /// has: the process never went away, so that frame is a frame of the same window.
    fn report_reload(&mut self, event: &WatchEvent) {
        let Some((staged, ready)) = self.reload_pending.as_ref() else {
            return;
        };
        let mut timings = staged.clone().into_os_string();
        timings.push(RELOAD_TIMINGS_SUFFIX);
        if event.path.as_os_str() != timings {
            return;
        }
        let Ok(text) = std::fs::read_to_string(&event.path) else {
            return;
        };
        let Ok(recorded) = serde_json::from_str::<serde_json::Value>(&text) else {
            return;
        };
        let millis = |key: &str| {
            recorded
                .get(key)
                .and_then(serde_json::Value::as_f64)
                .unwrap_or_default()
        };
        let ready = ready
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs_f64() * 1000.0)
            .unwrap_or_default();
        eprintln!(
            "==> the same window drew the new runtime {} ms after it was ready (its own code \
             loaded in {} ms, the view handed over in {} ms, laid out in {} ms)",
            (millis("presented_unix_ms") - ready).max(0.0).round(),
            millis("load_ms").round(),
            millis("handoff_ms").round(),
            millis("install_ms").round(),
        );
        self.reload_pending = None;
    }

    /// A new binary of a runtime built by hand: the program itself, the directory that holds
    /// it, or the one above, which a build may replace whole.
    fn is_runtime_change(&self, event: &WatchEvent) -> bool {
        let Some(binary) = self
            .runtime
            .as_ref()
            .filter(|runtime| runtime.built_in.is_none())
            .and_then(DevRuntime::binary)
        else {
            return false;
        };
        binary.starts_with(&event.path)
    }

    /// An edit of the sources of a runtime the tool builds.
    fn is_runtime_source_edit(&self, event: &WatchEvent) -> bool {
        let path = &event.path;
        if !self.in_runtime_sources(path) {
            return false;
        }
        // Changes below the runtime's sources may have been missed.
        if event.kind == ChangeKind::Rescan {
            return true;
        }
        path.extension().is_some_and(|extension| extension == "rs")
            || path.file_name().is_some_and(|name| name == "Cargo.toml")
    }

    /// Whether `path` is inside the sources of a runtime the tool builds.
    fn in_runtime_sources(&self, path: &Path) -> bool {
        let Some(repo) = self.runtime.as_ref().and_then(DevRuntime::built_in) else {
            return false;
        };
        RUNTIME_SOURCES
            .iter()
            .any(|dir| path.starts_with(repo.join(dir)))
    }
}

/// Say what the running app does with the build of `title`, which took `built` and its delivery
/// `delivered`, `how` being what the delivery did with it.
fn report(title: &str, plan: Option<Upgrade>, built: Duration, delivered: Duration, how: &str) {
    match plan {
        None => eprintln!(
            "==> {title}: built in {} ms, {how} in {} ms",
            built.as_millis(),
            delivered.as_millis()
        ),
        Some(Upgrade::InPlace) => eprintln!(
            "==> {title}: built in {} ms, {how} in {} ms; the running app takes it with its state",
            built.as_millis(),
            delivered.as_millis()
        ),
        Some(Upgrade::Restart(reason)) => eprintln!(
            "==> {title}: {reason}; {how} in {} ms, the running app starts over on it with its data",
            delivered.as_millis()
        ),
    }
}

/// What a gathered change asks of the loop.
enum Next {
    /// Act on these changes.
    Changed(Vec<WatchEvent>),
    /// The session the loop follows ended.
    Ended(std::io::Result<std::process::ExitStatus>),
}

/// Whether the events report the watch of `dir` itself gone, which ends that watch: kept, it
/// would never report `dir` again.
fn ends_watch(dir: &Path, events: &[WatchEvent]) -> bool {
    events
        .iter()
        .any(|event| event.kind == ChangeKind::Removed && event.path == dir)
}

/// Whether `path` is a package's ids lock, which a build writes itself: its change is the
/// loop's own and not a save.
fn is_ids_lock(path: &Path) -> bool {
    path.file_name() == Some(std::ffi::OsStr::new(tilolang::source::IDS_FILE))
}

/// Wait up to `missing` for a program at `path` to appear, and up to `writing` for it to stop
/// growing: a build writes a file in steps, and a program that is still being written cannot be
/// started. `false` when the build has made no program at all, which the change that writes one
/// reports again; why when what it wrote is never whole.
fn whole_program(path: &Path, missing: Duration, writing: Duration) -> Result<bool, String> {
    let started = Instant::now();
    let mut previous: Option<Program> = None;
    loop {
        let now = program(path).filter(|program| program.size > 0 && is_executable(path));
        if now.is_some() && now == previous {
            return Ok(true);
        }
        let waited = started.elapsed();
        match program(path) {
            None if waited >= missing => return Ok(false),
            Some(_) if waited >= writing => {
                return Err(format!(
                    "{} is not a whole program after {} s of writing: the build did not finish",
                    path.display(),
                    writing.as_secs()
                ));
            }
            _ => {}
        }
        previous = now;
        std::thread::sleep(COMPLETE_POLL);
    }
}

/// What tells one build of a program from another: the file the build made, its length, and
/// when it wrote it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Program {
    /// The file itself, where the platform numbers files: a build that replaces the program
    /// renames a new one over it.
    inode: u64,
    size: u64,
    written: std::time::SystemTime,
}

/// The program at `path` as it is now.
fn program(path: &Path) -> Option<Program> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let inode = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let inode = 0;
    Some(Program {
        inode,
        size: meta.len(),
        written: meta.modified().ok()?,
    })
}

/// Whether `path` is a file the platform will run.
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// When the newest file below `dir` was written.
fn newest_source(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(entry.path()),
                Ok(_) => {
                    if let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) {
                        newest = newest.max(Some(modified));
                    }
                }
                Err(_) => {}
            }
        }
    }
    newest
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

/// A build of the runtime a desktop's window runs on: the program, whether a rebuild of the
/// runtime's sources can be handed to the window in place, and the directories the program finds
/// the shared toolkit and `libstd` in.
struct LocalBuild {
    binary: PathBuf,
    reloads_in_place: bool,
    loaders: Vec<PathBuf>,
}

/// Build the tilolang runtime in `repo` for the window on this desktop.
fn build_local_runtime(repo: &Path) -> Result<LocalBuild, String> {
    #[cfg(unix)]
    {
        build_thin_runtime(repo)
    }
    #[cfg(not(unix))]
    {
        build_local_runtime_binary(repo).map(|binary| LocalBuild {
            binary,
            reloads_in_place: false,
            loaders: Vec::new(),
        })
    }
}

/// Build the runtime thin: the toolkit and `libstd` from one shared library the window's process
/// maps, and the app's own code as a library beside it, which a rebuild of the runtime's sources
/// replaces in that same process.
#[cfg(unix)]
fn build_thin_runtime(repo: &Path) -> Result<LocalBuild, String> {
    let host = thin_build(repo, &["--bin", RUNTIME_PACKAGE], "the runtime")?;
    // The app's own code is built once here too: a runtime whose code cannot be linked as the
    // window's library says so before the window opens, not on the first edit.
    build_local_library(repo)?;
    let toolkit = host.toolkit.ok_or_else(|| {
        format!(
            "the runtime build links no {RUNTIME_DYLIB}: the window cannot take a rebuild in place"
        )
    })?;
    let mut loaders: Vec<PathBuf> = toolkit
        .parent()
        .map(Path::to_path_buf)
        .into_iter()
        .collect();
    loaders.extend(rustc_target_libdir(repo));
    Ok(LocalBuild {
        binary: host.artifact,
        reloads_in_place: true,
        loaders,
    })
}

/// The app's own code of the runtime, linked as the library the window's process maps to take a
/// rebuild: the toolkit it shares with the process is not part of it.
#[cfg(unix)]
fn build_local_library(repo: &Path) -> Result<PathBuf, String> {
    Ok(thin_build(
        repo,
        &["--lib", "--crate-type", "cdylib"],
        "the runtime's new code",
    )?
    .artifact)
}

/// What one build of a target of the runtime made.
struct ThinArtifacts {
    /// The program, or the library, the build made.
    artifact: PathBuf,
    /// The runtime library the build links the toolkit from.
    toolkit: Option<PathBuf>,
}

/// Build `target` of the runtime package in `repo` thin, and say what came out. The build goes
/// through the workspace's `fast` profile: no LTO, incremental, the profile the desktop's apps
/// are built with.
#[cfg(unix)]
fn thin_build(repo: &Path, target: &[&str], what: &str) -> Result<ThinArtifacts, String> {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .current_dir(repo)
        .args([
            "rustc",
            "--locked",
            "--profile",
            LOCAL_PROFILE,
            "-p",
            RUNTIME_PACKAGE,
        ])
        .args(target)
        .args([
            "--features",
            RUNTIME_FEATURES,
            "--message-format=json-render-diagnostics",
            "--",
        ])
        .args(PREFER_DYNAMIC)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let output = command
        .output()
        .map_err(|error| format!("building {what}: {error}"))?;
    if !output.status.success() {
        return Err(format!("building {what}: {}", output.status));
    }
    let messages: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|message: &serde_json::Value| {
            message.get("reason").and_then(serde_json::Value::as_str) == Some("compiler-artifact")
        })
        .collect();
    let toolkit = messages
        .iter()
        .find(|message| target_name(message) == Some(RUNTIME_DYLIB))
        .and_then(|message| filenames(message).into_iter().next());
    let artifact = messages
        .iter()
        .filter_map(|message| {
            message
                .get("executable")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
        })
        .next_back()
        .or_else(|| {
            messages
                .iter()
                .find(|message| target_name(message) == Some(RUNTIME_LIBRARY_TARGET))
                .and_then(|message| {
                    filenames(message).into_iter().find(|path| {
                        path.extension()
                            .is_some_and(|extension| extension == dylib_extension())
                    })
                })
        });
    let artifact =
        artifact.ok_or_else(|| format!("the build of {what} made nothing to run or to map"))?;
    Ok(ThinArtifacts { artifact, toolkit })
}

/// The name of the target an artifact message is about.
fn target_name(message: &serde_json::Value) -> Option<&str> {
    message
        .pointer("/target/name")
        .and_then(serde_json::Value::as_str)
}

/// The files an artifact message names.
fn filenames(message: &serde_json::Value) -> Vec<PathBuf> {
    message
        .get("filenames")
        .and_then(serde_json::Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

/// The extension of a dynamically linked library on this platform.
fn dylib_extension() -> &'static str {
    if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    }
}

/// The libraries of the compiler the checkout's toolchain selects: where a dynamically linked
/// program finds `libstd`.
#[cfg(unix)]
fn rustc_target_libdir(repo: &Path) -> Option<PathBuf> {
    let output = Command::new("rustc")
        .args(["--print", "target-libdir"])
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8(output.stdout).ok()?.trim());
    path.is_dir().then_some(path)
}

/// Build the tilolang runtime binary in `repo` as an ordinary program, and say where it is: on a
/// platform whose windows cannot take a rebuild in place, the window is started over on it.
#[cfg(not(unix))]
fn build_local_runtime_binary(repo: &Path) -> Result<PathBuf, String> {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .current_dir(repo)
        .args([
            "build",
            "--locked",
            "--profile",
            LOCAL_PROFILE,
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
                && target_name(message) == Some(RUNTIME_PACKAGE)
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
        reloads_in_place: false,
        loaders: Vec::new(),
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
    let mut dev = Dev::in_session(build, identity, vec![peer], bin, repo);
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
    let mut dev = Dev::on_desktop(build, identity, &root, repo)?;
    dev.runtime = Some(DevRuntime {
        binary: runtime,
        built_in,
        reloads_in_place: false,
        loaders: Vec::new(),
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
    let (binary, loaders) = match dev.runtime.as_ref() {
        Some(runtime) => (
            runtime
                .binary()
                .map(Path::to_path_buf)
                .ok_or_else(|| "no runtime binary to run the app on".to_owned())?,
            runtime.loaders.clone(),
        ),
        None => return Err("no runtime binary to run the app on".to_owned()),
    };
    let apps = dev.build_for_start_local()?;
    let app = apps
        .iter()
        .find(|app| app.app == dev.opened)
        .ok_or_else(|| format!("{} was not built", dev.opened))?;
    let local = dev
        .local_mut()
        .ok_or_else(|| "the app is not run here".to_owned())?;
    local.start(app, &binary, &loaders)?;
    dev.take_note_of_runtime();
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

    /// How long a test gives a program to appear and to be written.
    const PATIENCE: Duration = Duration::from_secs(1);

    /// Write a program that says `note`, and that the platform may run.
    #[cfg(unix)]
    fn write_program(path: &Path, note: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(path, format!("#!/bin/sh\n{note}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A program a build is still writing is taken once it has stopped growing, and one it has
    /// not written yet answers that there is none.
    #[cfg(unix)]
    #[test]
    fn a_program_is_taken_once_it_is_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tilolang-app");
        assert_eq!(
            whole_program(&path, Duration::from_millis(50), PATIENCE),
            Ok(false)
        );
        write_program(&path, "half");
        let written = path.clone();
        let writing = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            write_program(&written, "half and the other half");
        });
        assert_eq!(whole_program(&path, PATIENCE, PATIENCE), Ok(true));
        writing.join().unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "#!/bin/sh\nhalf and the other half\n",
            "the answer waited for the whole program"
        );
    }

    /// The program of a build is the file the build made: one renamed over the old is another
    /// program, even when it is as long and as old.
    #[cfg(unix)]
    #[test]
    fn a_program_renamed_over_the_old_one_is_another_program() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tilolang-app");
        write_program(&path, "one");
        let first = program(&path).unwrap();
        write_program(&dir.path().join(".staged"), "one");
        std::fs::rename(dir.path().join(".staged"), &path).unwrap();
        let second = program(&path).unwrap();
        assert_eq!((first.size, first.written), (second.size, second.written));
        assert_ne!(first, second, "a renamed program is a new file");
    }

    /// A build's ids lock is written by the tool itself: a save it is not.
    #[test]
    fn a_builds_ids_lock_is_no_save() {
        assert!(is_ids_lock(
            &Path::new("/app/notes").join(tilolang::source::IDS_FILE)
        ));
        assert!(!is_ids_lock(Path::new("/app/notes/app.tl")));
        assert!(!is_ids_lock(Path::new("/app/notes/locales/de-DE.ftl")));
    }

    /// A watch whose directory the events report gone ends, even where the directory is there
    /// again: the watch of it was of the directory that went away.
    #[test]
    fn a_watch_ends_with_the_directory_it_reports_gone() {
        let dir = PathBuf::from("/repo/target/debug");
        let gone = WatchEvent {
            path: dir.clone(),
            kind: ChangeKind::Removed,
        };
        let inside = WatchEvent {
            path: dir.join("tilolang-app"),
            kind: ChangeKind::Removed,
        };
        let created = WatchEvent {
            path: dir.clone(),
            kind: ChangeKind::Created,
        };
        assert!(ends_watch(&dir, &[gone]));
        assert!(!ends_watch(&dir, &[inside]));
        assert!(!ends_watch(&dir, &[created]));
    }
}
