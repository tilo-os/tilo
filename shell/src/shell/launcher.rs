//! Launching real applications from the desktop.
//!
//! An installed qOS application is a `.qpkg` that `qos-install` has
//! unpacked and recorded in `~/.local/share/qos-apps/registry.toml`.
//! This module is the shell's side of running one: enumerate what is
//! installed, and start a chosen package as a **separate sandboxed
//! process** that connects back to this compositor as a Wayland client.
//!
//! ## What actually runs
//!
//! The shell does not build a sandbox itself. It execs `qos-launcher`
//! (the [`launcher`] crate's binary), which is the canonical
//! implementation of a qOS app launch:
//!
//! - reads the package manifest and derives every bind from it,
//! - compiles the baseline seccomp filter and hands it to `bwrap` on an
//!   fd,
//! - mounts the signed runtime bundle the manifest pins,
//! - re-execs itself inside the namespace as the `__sandbox-exec` shim
//!   so Landlock is applied *after* bwrap finishes its mount setup,
//! - resolves declared service sockets, and finally execs the app.
//!
//! Running it as a child process rather than calling the crate in-process
//! is deliberate: the Landlock shim is `current_exe()`-relative, so the
//! binary bwrap re-execs has to *be* `qos-launcher`. It also keeps a
//! launch failure inside its own process — a broken package cannot take
//! the compositor down with it.
//!
//! ## The display
//!
//! [`set_wayland_socket`] is called by the compositor once it knows which
//! `wayland-N` it bound. Every launch passes that socket to
//! `qos-launcher --wayland-socket`, which binds it into the sandbox at
//! `/run/user/<uid>/wayland-0`. Nothing is inherited from the
//! compositor's own environment: with no compositor socket published
//! there is no display in the sandbox, and the launch is refused up
//! front rather than silently starting an app that can never draw.
//!
//! ## Never on the frame thread
//!
//! [`launch`] resolves the package from an in-memory cache and hands the
//! prepared command to a worker thread, which does the `fork`/`exec` and
//! then blocks for the child's lifetime relaying its output. The
//! compositor's 60fps loop only ever touches a `Mutex<Vec<_>>`. Outcomes
//! come back through [`drain_events`], which the chrome drains once per
//! frame and turns into notifications.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long an enumeration of installed packages is reused before the
/// registry + manifests are re-read. The palette queries this on every
/// keystroke; a couple of seconds is short enough that an app installed
/// in another terminal shows up while you are still typing its name.
const APP_CACHE_TTL: Duration = Duration::from_secs(2);

/// Upper bound on the stderr we keep for a failed launch. Enough to
/// carry `bwrap`'s complaint or the dynamic loader's, small enough that
/// a chatty app can't grow the shell's memory.
const STDERR_TAIL_LINES: usize = 12;

/// One installed application the launcher can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    /// Reverse-DNS package id — the identity `qos-install` records.
    pub package_id: String,
    /// Human-facing name derived from the package id's trailing
    /// component (`dev.qos.text-editor` → `Text Editor`). The manifest
    /// has no display-name field today; when it grows one this is the
    /// single place that changes.
    pub display_name: String,
    pub version: String,
    pub manifest_path: PathBuf,
    pub payload_root: PathBuf,
    /// Whether the package declares a GUI entrypoint. Pure-CLI packages
    /// are installed the same way but have no window to show, so the
    /// applications list hides them.
    pub has_gui: bool,
}

/// Why a launch could not be started.
///
/// These are the failures we can see *before* forking. Everything after
/// the fork (bwrap refusing, a missing loader, the app crashing on
/// start) arrives asynchronously as a [`LaunchEvent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    /// No package with this id in the registry.
    NotInstalled { package_id: String },
    /// The package is installed but declares no GUI entrypoint.
    NotAGuiApp { package_id: String },
    /// `qos-launcher` isn't anywhere we look.
    LauncherBinaryMissing { searched: Vec<PathBuf> },
    /// The compositor hasn't published a Wayland socket, so a GUI app
    /// would have nothing to connect to.
    NoDisplay,
    /// The registry file exists but could not be read or parsed.
    RegistryUnreadable { detail: String },
}

impl LaunchError {
    /// One-line summary for a notification body.
    pub fn user_message(&self) -> String {
        match self {
            LaunchError::NotInstalled { package_id } => {
                format!("{package_id} is not installed")
            }
            LaunchError::NotAGuiApp { package_id } => {
                format!("{package_id} has no graphical entrypoint")
            }
            LaunchError::LauncherBinaryMissing { searched } => {
                let list: Vec<String> =
                    searched.iter().map(|p| p.display().to_string()).collect();
                format!("qos-launcher not found (looked in {})", list.join(", "))
            }
            LaunchError::NoDisplay => {
                "the compositor has not published a Wayland socket yet".to_string()
            }
            LaunchError::RegistryUnreadable { detail } => {
                format!("the app registry could not be read: {detail}")
            }
        }
    }
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

/// Something that happened to a launch after it left the frame thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchEvent {
    pub package_id: String,
    pub display_name: String,
    pub kind: LaunchEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchEventKind {
    /// `qos-launcher` is running with this host pid.
    Started { pid: u32 },
    /// The child could not be spawned at all (e.g. `qos-launcher` was
    /// deleted between resolution and fork).
    SpawnFailed { detail: String },
    /// The launch chain exited non-zero. `stderr_tail` is the last few
    /// lines it printed — usually the actual reason.
    Failed {
        code: Option<i32>,
        stderr_tail: String,
    },
    /// The app ran and exited cleanly. Not an error; reported so
    /// callers can retire per-app UI state.
    Exited,
}

/// A fully-resolved launch, ready to hand to `Command`.
///
/// Split out from [`launch`] so the exact argv and environment a launch
/// would use are assertable without spawning anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Environment *overrides* for the child. The launcher process needs
    /// `XDG_RUNTIME_DIR` to find service sockets; the app's own
    /// `WAYLAND_DISPLAY` is set inside the sandbox by `qos-launcher`, not
    /// inherited from here.
    pub env: Vec<(String, String)>,
}

// ---------------------------------------------------------------------
// Process-wide state.
//
// The compositor is one process with one display and one app registry,
// so this is genuinely process state rather than something worth
// threading through every widget borrow.
// ---------------------------------------------------------------------

static WAYLAND_SOCKET: Mutex<Option<PathBuf>> = Mutex::new(None);
static EVENTS: Mutex<Vec<LaunchEvent>> = Mutex::new(Vec::new());
static APP_CACHE: Mutex<Option<(Vec<InstalledApp>, Instant)>> = Mutex::new(None);
/// `package_id → display name` for everything this session has launched.
/// Consulted when a foreign toplevel is adopted so the tab shows the
/// app's name instead of a raw wayland app-id.
static LAUNCHED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Publish the compositor's Wayland socket. Called once by the
/// compositor after `ListeningSocketSource` binds, with the socket name
/// (`wayland-1`) it printed.
pub fn set_wayland_socket(socket_name: &OsStr) {
    let path = runtime_dir().join(socket_name);
    *WAYLAND_SOCKET.lock().expect("WAYLAND_SOCKET poisoned") = Some(path);
}

/// The socket [`set_wayland_socket`] published, if any.
pub fn wayland_socket() -> Option<PathBuf> {
    WAYLAND_SOCKET
        .lock()
        .expect("WAYLAND_SOCKET poisoned")
        .clone()
}

/// `$XDG_RUNTIME_DIR`, falling back to `/run/user/<uid>`.
fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    // SAFETY: getuid always succeeds.
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/run/user/{uid}"))
}

// ---------------------------------------------------------------------
// Enumeration.
// ---------------------------------------------------------------------

/// Every installed package, GUI or not, freshly read from the registry.
pub fn read_installed_apps() -> Result<Vec<InstalledApp>, LaunchError> {
    let entries = launcher::registry::list_installed().map_err(|e| {
        LaunchError::RegistryUnreadable {
            detail: e.to_string(),
        }
    })?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        // A registry row whose manifest has gone missing (payload
        // deleted behind the registry's back) is skipped rather than
        // failing the whole enumeration — one broken package must not
        // hide every other app from the launcher.
        let Ok(manifest) = launcher::manifest::read_manifest(&entry.manifest_path) else {
            continue;
        };
        let package_id = if manifest.package_id.is_empty() {
            entry.id.clone()
        } else {
            manifest.package_id.clone()
        };
        out.push(InstalledApp {
            display_name: display_name_for_package_id(&package_id),
            package_id,
            version: manifest.version.clone(),
            manifest_path: entry.manifest_path,
            payload_root: entry.payload_root,
            has_gui: manifest.entrypoint.gui_bin().is_some(),
        });
    }
    Ok(out)
}

/// Installed packages, memoised for [`APP_CACHE_TTL`].
///
/// Errors are not cached: a transiently unreadable registry should not
/// wedge the launcher for the rest of the TTL.
pub fn installed_apps() -> Result<Vec<InstalledApp>, LaunchError> {
    {
        let cache = APP_CACHE.lock().expect("APP_CACHE poisoned");
        if let Some((apps, at)) = cache.as_ref() {
            if at.elapsed() < APP_CACHE_TTL {
                return Ok(apps.clone());
            }
        }
    }
    let apps = read_installed_apps()?;
    *APP_CACHE.lock().expect("APP_CACHE poisoned") = Some((apps.clone(), Instant::now()));
    Ok(apps)
}

/// Drop the memoised enumeration. Used after an install/remove and by
/// tests that repoint `QOS_APPS_DATA_DIR`.
pub fn invalidate_app_cache() {
    *APP_CACHE.lock().expect("APP_CACHE poisoned") = None;
}

/// Installed GUI applications, sorted by display name — what the
/// applications search provider offers.
pub fn gui_apps() -> Vec<InstalledApp> {
    let mut apps = installed_apps().unwrap_or_default();
    apps.retain(|a| a.has_gui);
    apps.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    apps
}

/// `dev.qos.text-editor` → `Text Editor`.
///
/// The manifest carries no display name, so the trailing id component is
/// the only signal we have. Separators become spaces and each word is
/// title-cased; an id that is already a display-ish word (`Notes`) comes
/// back unchanged.
pub fn display_name_for_package_id(package_id: &str) -> String {
    let last = package_id
        .rsplit('.')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(package_id);
    let words: Vec<String> = last
        .split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() {
        package_id.to_string()
    } else {
        words.join(" ")
    }
}

// ---------------------------------------------------------------------
// Resolving `qos-launcher`.
// ---------------------------------------------------------------------

/// Where we look for `qos-launcher`, in order.
///
/// `QOS_LAUNCHER_BIN` first so a test (or a developer bisecting a
/// launch) can point the shell at a specific build; then next to the
/// running compositor, which is both how the OS image installs them and
/// how a `cargo build` dev tree lays them out; then the system paths.
fn launcher_bin_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(explicit) = std::env::var_os("QOS_LAUNCHER_BIN") {
        if !explicit.is_empty() {
            out.push(PathBuf::from(explicit));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("qos-launcher"));
        }
    }
    out.push(PathBuf::from("/usr/libexec/qos-launcher"));
    out.push(PathBuf::from("/usr/bin/qos-launcher"));
    out
}

/// First candidate that exists, else the list we searched.
pub fn resolve_launcher_bin() -> Result<PathBuf, LaunchError> {
    let candidates = launcher_bin_candidates();
    for candidate in &candidates {
        if candidate.exists() {
            return Ok(candidate.clone());
        }
    }
    Err(LaunchError::LauncherBinaryMissing {
        searched: candidates,
    })
}

// ---------------------------------------------------------------------
// Building + starting a launch.
// ---------------------------------------------------------------------

/// Render the `qos-launcher` invocation for `app`.
///
/// Pure — no I/O, no spawning — so tests can assert on the exact command
/// the production path builds.
pub fn build_launch_command(
    app: &InstalledApp,
    launcher_bin: &Path,
    wayland_socket: &Path,
) -> LaunchCommand {
    let mut args = vec![
        "run".to_string(),
        "--manifest".to_string(),
        app.manifest_path.display().to_string(),
        "--payload-root".to_string(),
        app.payload_root.display().to_string(),
        "--wayland-socket".to_string(),
        wayland_socket.display().to_string(),
    ];
    // Service sockets (collab, qos-fs, …) are resolved by the launcher
    // against `$XDG_RUNTIME_DIR/qos/services`. Optional ones that are
    // absent degrade to a warning; a package that declares a *required*
    // service the session isn't running fails loudly, which is the
    // behaviour we want.
    if std::env::var_os("QOS_LAUNCH_NO_SERVICES").is_some() {
        args.push("--no-services".to_string());
    }
    LaunchCommand {
        program: launcher_bin.to_path_buf(),
        args,
        env: vec![(
            "XDG_RUNTIME_DIR".to_string(),
            runtime_dir().display().to_string(),
        )],
    }
}

/// Resolve `package_id` and start it.
///
/// Returns as soon as the launch is handed to its worker thread — it
/// never waits for the app to appear. The `Ok` value is the resolved
/// app, so callers can name it in their UI without a second lookup.
pub fn launch(package_id: &str) -> Result<InstalledApp, LaunchError> {
    let app = installed_apps()?
        .into_iter()
        .find(|a| a.package_id == package_id)
        .ok_or_else(|| LaunchError::NotInstalled {
            package_id: package_id.to_string(),
        })?;
    if !app.has_gui {
        return Err(LaunchError::NotAGuiApp {
            package_id: package_id.to_string(),
        });
    }
    let socket = wayland_socket().ok_or(LaunchError::NoDisplay)?;
    let launcher_bin = resolve_launcher_bin()?;
    let command = build_launch_command(&app, &launcher_bin, &socket);
    remember_launch(&app);
    spawn_detached(app.clone(), command);
    Ok(app)
}

/// Run `command` on a worker thread and report the outcome through
/// [`EVENTS`].
///
/// Everything here — the fork, the wait, the output relay — happens off
/// the compositor thread. The thread lives for as long as the app does.
fn spawn_detached(app: InstalledApp, command: LaunchCommand) {
    let builder = std::thread::Builder::new().name("qos-app-launch".to_string());
    // Kept for the "couldn't even start a thread" report below, since
    // the closure takes `app` by move.
    let identity = (app.package_id.clone(), app.display_name.clone());
    let spawn_result = builder.spawn(move || {
        let mut cmd = Command::new(&command.program);
        cmd.args(&command.args);
        for (k, v) in &command.env {
            cmd.env(k, v);
        }
        // The app is not a terminal job: it gets no stdin, and its
        // output is relayed (not inherited) so we can quote it back in a
        // failure notification.
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                push_event(LaunchEvent {
                    package_id: app.package_id.clone(),
                    display_name: app.display_name.clone(),
                    kind: LaunchEventKind::SpawnFailed {
                        detail: e.to_string(),
                    },
                });
                return;
            }
        };
        push_event(LaunchEvent {
            package_id: app.package_id.clone(),
            display_name: app.display_name.clone(),
            kind: LaunchEventKind::Started { pid: child.id() },
        });

        // Relay stdout on its own thread so a chatty app can't fill the
        // pipe and wedge the process while we're reading stderr.
        if let Some(stdout) = child.stdout.take() {
            let tag = app.package_id.clone();
            let _ = std::thread::Builder::new()
                .name("qos-app-stdout".to_string())
                .spawn(move || {
                    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                        eprintln!("[app {tag}] {line}");
                    }
                });
        }

        // stderr is both echoed and kept (last N lines) for the failure
        // notification.
        let mut tail: VecDeque<String> = VecDeque::with_capacity(STDERR_TAIL_LINES);
        if let Some(stderr) = child.stderr.take() {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("[app {}] {line}", app.package_id);
                if tail.len() == STDERR_TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        }

        let status = child.wait();
        let kind = match status {
            Ok(status) if status.success() => LaunchEventKind::Exited,
            Ok(status) => LaunchEventKind::Failed {
                code: status.code(),
                stderr_tail: tail.iter().cloned().collect::<Vec<_>>().join("\n"),
            },
            Err(e) => LaunchEventKind::SpawnFailed {
                detail: e.to_string(),
            },
        };
        push_event(LaunchEvent {
            package_id: app.package_id.clone(),
            display_name: app.display_name.clone(),
            kind,
        });
    });
    if let Err(e) = spawn_result {
        push_event(LaunchEvent {
            package_id: identity.0,
            display_name: identity.1,
            kind: LaunchEventKind::SpawnFailed {
                detail: format!("could not start launch thread: {e}"),
            },
        });
    }
}

fn push_event(event: LaunchEvent) {
    EVENTS.lock().expect("EVENTS poisoned").push(event);
}

/// Take every launch event queued since the last call. Called once per
/// frame by the chrome.
pub fn drain_events() -> Vec<LaunchEvent> {
    let mut events = EVENTS.lock().expect("EVENTS poisoned");
    std::mem::take(&mut *events)
}

// ---------------------------------------------------------------------
// Correlating a launch with the window it opens.
// ---------------------------------------------------------------------

pub(crate) fn remember_launch(app: &InstalledApp) {
    let mut launched = LAUNCHED.lock().expect("LAUNCHED poisoned");
    if launched.iter().any(|(id, _)| id == &app.package_id) {
        return;
    }
    launched.push((app.package_id.clone(), app.display_name.clone()));
}

/// Display name for a Wayland `app_id`, if this session launched the
/// package it belongs to.
///
/// The match is textual and deterministic: either the client's app-id is
/// the package id outright, or it is the package id's trailing component
/// (`dev.qos.hello` ↔ `hello`) — the same reverse-DNS convention the
/// package ids follow. Nothing here guesses by timing.
///
/// An app whose wayland app-id is unrelated to its package id is simply
/// not matched, and its tab keeps the app-id-derived label. Closing that
/// gap needs the toplevel's owning client correlated back to the pid we
/// spawned (`SO_PEERCRED` at `insert_client` time), which is a change to
/// the compositor's client bookkeeping rather than to this module.
pub fn display_name_for_app_id(app_id: &str) -> Option<String> {
    let launched = LAUNCHED.lock().expect("LAUNCHED poisoned");
    let needle = app_id.to_ascii_lowercase();
    for (package_id, display_name) in launched.iter() {
        let pid_lower = package_id.to_ascii_lowercase();
        if pid_lower == needle {
            return Some(display_name.clone());
        }
        if pid_lower.rsplit('.').next() == Some(needle.as_str()) {
            return Some(display_name.clone());
        }
    }
    None
}

/// Test hook: publish a socket path directly (production goes through
/// [`set_wayland_socket`], which resolves it against `$XDG_RUNTIME_DIR`).
#[cfg(test)]
pub(crate) fn set_wayland_socket_path(path: Option<PathBuf>) {
    *WAYLAND_SOCKET.lock().expect("WAYLAND_SOCKET poisoned") = path;
}

/// Rendered `OsString` form of the socket name for logging.
pub fn wayland_socket_display() -> OsString {
    wayland_socket()
        .map(|p| p.into_os_string())
        .unwrap_or_else(|| OsString::from("<none>"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Guards the tests that repoint `QOS_APPS_DATA_DIR` /
    /// `QOS_LAUNCHER_BIN`, which are process-global.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Write a registry + manifest pair into `dir` and point the
    /// launcher's registry reader at it. Returns the app's payload root.
    fn install_fixture(dir: &Path, package_id: &str, entrypoint_type: &str) -> PathBuf {
        let payload = dir.join("payloads").join(package_id);
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        let manifest_path = payload.join("manifest.toml");
        std::fs::write(
            &manifest_path,
            format!(
                "[package]\nid = \"{package_id}\"\nversion = \"1.2.3\"\n\
                 [entrypoint]\nbin = \"bin/app\"\ntype = \"{entrypoint_type}\"\n"
            ),
        )
        .unwrap();
        let registry = dir.join("registry.toml");
        let mut existing = std::fs::read_to_string(&registry).unwrap_or_default();
        existing.push_str(&format!(
            "[[entry]]\nid = '{package_id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
            payload.display().to_string(),
            manifest_path.display().to_string()
        ));
        std::fs::write(&registry, existing).unwrap();
        payload
    }

    #[test]
    fn display_name_title_cases_the_trailing_component() {
        assert_eq!(display_name_for_package_id("dev.qos.hello"), "Hello");
        assert_eq!(
            display_name_for_package_id("dev.qos.text-editor"),
            "Text Editor"
        );
        assert_eq!(display_name_for_package_id("Notes"), "Notes");
        // A degenerate id (trailing separator, empty) neither panics nor
        // produces an empty label — the row would be unclickable prose.
        assert!(!display_name_for_package_id("dev.qos.").is_empty());
    }

    #[test]
    fn enumeration_reads_the_registry_and_flags_gui_apps() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        install_fixture(tmp.path(), "dev.qos.hello", "gui");
        install_fixture(tmp.path(), "dev.qos.notes-cli", "cli");
        // SAFETY: guarded by ENV_LOCK.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        invalidate_app_cache();
        let apps = read_installed_apps().expect("registry readable");
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        invalidate_app_cache();

        assert_eq!(apps.len(), 2, "both packages enumerate: {apps:?}");
        let hello = apps
            .iter()
            .find(|a| a.package_id == "dev.qos.hello")
            .expect("hello present");
        assert!(hello.has_gui, "gui entrypoint must be flagged as GUI");
        assert_eq!(hello.display_name, "Hello");
        assert_eq!(hello.version, "1.2.3");
        let cli = apps
            .iter()
            .find(|a| a.package_id == "dev.qos.notes-cli")
            .expect("cli package present");
        assert!(!cli.has_gui, "a cli-only package is not a GUI app");
    }

    #[test]
    fn launch_command_carries_manifest_payload_and_display() {
        let app = InstalledApp {
            package_id: "dev.qos.hello".into(),
            display_name: "Hello".into(),
            version: "0.1.0".into(),
            manifest_path: PathBuf::from("/apps/hello/manifest.toml"),
            payload_root: PathBuf::from("/apps/hello"),
            has_gui: true,
        };
        let cmd = build_launch_command(
            &app,
            Path::new("/usr/bin/qos-launcher"),
            Path::new("/run/user/1000/wayland-3"),
        );
        assert_eq!(cmd.program, Path::new("/usr/bin/qos-launcher"));
        assert_eq!(cmd.args[0], "run");
        let pos = |flag: &str| {
            cmd.args
                .iter()
                .position(|a| a == flag)
                .unwrap_or_else(|| panic!("{flag} missing from {:?}", cmd.args))
        };
        assert_eq!(cmd.args[pos("--manifest") + 1], "/apps/hello/manifest.toml");
        assert_eq!(cmd.args[pos("--payload-root") + 1], "/apps/hello");
        assert_eq!(
            cmd.args[pos("--wayland-socket") + 1],
            "/run/user/1000/wayland-3",
            "the app must be pointed at THIS compositor's socket"
        );
    }

    #[test]
    fn launch_refuses_an_unknown_package() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: guarded by ENV_LOCK.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        invalidate_app_cache();
        let err = launch("dev.qos.nope").unwrap_err();
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        invalidate_app_cache();
        assert_eq!(
            err,
            LaunchError::NotInstalled {
                package_id: "dev.qos.nope".into()
            }
        );
        assert!(err.user_message().contains("not installed"));
    }

    #[test]
    fn launch_refuses_a_cli_only_package() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        install_fixture(tmp.path(), "dev.qos.notes-cli", "cli");
        // SAFETY: guarded by ENV_LOCK.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        invalidate_app_cache();
        let err = launch("dev.qos.notes-cli").unwrap_err();
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        invalidate_app_cache();
        assert_eq!(
            err,
            LaunchError::NotAGuiApp {
                package_id: "dev.qos.notes-cli".into()
            }
        );
    }

    #[test]
    fn launch_refuses_when_no_compositor_socket_is_published() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        install_fixture(tmp.path(), "dev.qos.hello", "gui");
        // SAFETY: guarded by ENV_LOCK.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        invalidate_app_cache();
        set_wayland_socket_path(None);
        let err = launch("dev.qos.hello").unwrap_err();
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        invalidate_app_cache();
        assert_eq!(err, LaunchError::NoDisplay);
    }

    /// The production `launch` path, end to end, with `qos-launcher`
    /// replaced by a recorder: a real child process is forked, gets the
    /// real argv, and its failure comes back as a `LaunchEvent`.
    #[test]
    fn launch_execs_the_launcher_binary_with_the_compositor_socket() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        install_fixture(tmp.path(), "dev.qos.hello", "gui");
        let record = tmp.path().join("argv.txt");
        let fake = tmp.path().join("qos-launcher");
        {
            let mut f = std::fs::File::create(&fake).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, "printf '%s\\n' \"$@\" > {}", record.display()).unwrap();
            writeln!(f, "echo \"launch refused by fixture\" >&2").unwrap();
            writeln!(f, "exit 3").unwrap();
        }
        let mut perms = std::fs::metadata(&fake).unwrap().permissions();
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o755);
        }
        std::fs::set_permissions(&fake, perms).unwrap();

        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::set_var("QOS_APPS_DATA_DIR", tmp.path());
            std::env::set_var("QOS_LAUNCHER_BIN", &fake);
        }
        invalidate_app_cache();
        set_wayland_socket_path(Some(PathBuf::from("/run/user/1000/wayland-7")));
        let _ = drain_events();

        let app = launch("dev.qos.hello").expect("launch accepted");
        assert_eq!(app.display_name, "Hello");

        // Wait for the worker to report the child's exit.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut failed = None;
        let mut saw_started = false;
        while Instant::now() < deadline && failed.is_none() {
            for event in drain_events() {
                match event.kind {
                    LaunchEventKind::Started { pid } => {
                        assert!(pid > 0, "a real child pid is reported");
                        saw_started = true;
                    }
                    LaunchEventKind::Failed { .. } => failed = Some(event),
                    other => panic!("unexpected launch event: {other:?}"),
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        unsafe {
            std::env::remove_var("QOS_APPS_DATA_DIR");
            std::env::remove_var("QOS_LAUNCHER_BIN");
        }
        invalidate_app_cache();
        set_wayland_socket_path(None);

        assert!(saw_started, "the launch must report a started child");
        let failed = failed.expect("the failing child must surface a Failed event");
        match failed.kind {
            LaunchEventKind::Failed { code, stderr_tail } => {
                assert_eq!(code, Some(3), "the child's exit code is reported");
                assert!(
                    stderr_tail.contains("launch refused by fixture"),
                    "the child's stderr is quoted back: {stderr_tail:?}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }

        let argv = std::fs::read_to_string(&record).expect("the launcher binary really ran");
        let argv: Vec<&str> = argv.lines().collect();
        assert_eq!(argv[0], "run");
        assert!(
            argv.contains(&"--wayland-socket") && argv.contains(&"/run/user/1000/wayland-7"),
            "the spawned launcher is pointed at the compositor's socket: {argv:?}"
        );
        assert!(
            argv.iter()
                .any(|a| a.ends_with("payloads/dev.qos.hello/manifest.toml")),
            "the resolved manifest path is passed through: {argv:?}"
        );
    }

    /// The launch history is process-global (one session, one history),
    /// so this test uses ids no other test registers rather than
    /// clearing shared state out from under a parallel test.
    #[test]
    fn launched_apps_are_matched_back_by_app_id() {
        remember_launch(&InstalledApp {
            package_id: "dev.qos.matchfixture".into(),
            display_name: "Match Fixture".into(),
            version: "0.1.0".into(),
            manifest_path: PathBuf::new(),
            payload_root: PathBuf::new(),
            has_gui: true,
        });
        // Full package id, trailing component, and case-insensitivity.
        assert_eq!(
            display_name_for_app_id("dev.qos.matchfixture").as_deref(),
            Some("Match Fixture")
        );
        assert_eq!(
            display_name_for_app_id("matchfixture").as_deref(),
            Some("Match Fixture")
        );
        assert_eq!(
            display_name_for_app_id("MATCHFIXTURE").as_deref(),
            Some("Match Fixture")
        );
        // An unrelated client keeps its own label.
        assert_eq!(display_name_for_app_id("org.gnome.NeverLaunched"), None);
    }
}
