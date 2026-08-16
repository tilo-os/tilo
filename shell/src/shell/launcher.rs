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
//! ## Permissions
//!
//! The manifest is what the *packager* asked for. What the **user**
//! granted lives on the workspace, as capabilities
//! ([`crate::shell::state::Workspace::capabilities`]). Both have to be
//! true before an app can reach a path, a device or the network, so
//! every launch carries the workspace's capabilities across to
//! `qos-launcher`: they are written to a private JSON file in the
//! runtime dir and named on the command line with `--capabilities`.
//!
//! The launcher turns them into bind mounts and Landlock rules. That is
//! the whole of what "Always allow" does — with no capability there is
//! no mount, and an app cannot open what is not in its namespace. A
//! launch with no capabilities (nothing published, nothing granted,
//! file missing) gets exactly the sandbox the manifest describes;
//! nothing here can widen one.
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

use crate::shell::state::{CapOverride, RoomId, Subject, Workspace};

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
    /// Human-facing name. The manifest's `[package].name` when the
    /// publisher set one, else derived from the package id's trailing
    /// component (`dev.qos.text-editor` → `Text Editor`) — the
    /// derivation cannot know that `api-tester` means `API Tester`,
    /// so a publisher that cares declares it.
    pub display_name: String,
    /// The manifest's `[package].category`, when declared. Matched by
    /// the Applications provider so typing a category word offers the
    /// apps in it.
    pub category: Option<String>,
    pub version: String,
    pub manifest_path: PathBuf,
    pub payload_root: PathBuf,
    /// Whether the package declares a GUI entrypoint. Pure-CLI packages
    /// are installed the same way but have no window to show, so the
    /// applications list hides them.
    pub has_gui: bool,
    /// The manifest's `[providers].ext` — file extensions this package
    /// declares it opens, lowercased and without the leading dot.
    ///
    /// This is the desktop's only answer to "which app owns `.odt`".
    /// The document format registry (`qdoc`) knows which extensions are
    /// editable documents and which codec parses them; it deliberately
    /// knows nothing about installed packages. Joining the two here —
    /// rather than in a table inside the compositor — means installing
    /// an app is what teaches the desktop to open its file types, and
    /// uninstalling it is what un-teaches that.
    pub opens_extensions: Vec<String>,
}

impl InstalledApp {
    /// Whether this package declares it opens `ext` (case-insensitive,
    /// leading dot tolerated).
    pub fn opens_extension(&self, ext: &str) -> bool {
        let want = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        !want.is_empty() && self.opens_extensions.iter().any(|e| *e == want)
    }
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
    /// There is no active workspace to launch into. A workspace is
    /// both the app's sandbox policy and its collaboration room, so
    /// there is nothing coherent to start without one.
    NoWorkspace,
    /// The registry file exists but could not be read or parsed.
    RegistryUnreadable { detail: String },
    /// No `.desktop` entry on this system has that id. Distinct from
    /// [`LaunchError::NotInstalled`] because it is a different
    /// namespace with a different fix: nothing qOS packaged is
    /// missing, the host does not have that application.
    HostAppNotFound { desktop_id: String },
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
                let list: Vec<String> = searched.iter().map(|p| p.display().to_string()).collect();
                format!("qos-launcher not found (looked in {})", list.join(", "))
            }
            LaunchError::NoDisplay => {
                "the compositor has not published a Wayland socket yet".to_string()
            }
            LaunchError::NoWorkspace => "there is no active workspace to open it in".to_string(),
            LaunchError::RegistryUnreadable { detail } => {
                format!("the app registry could not be read: {detail}")
            }
            LaunchError::HostAppNotFound { desktop_id } => {
                format!("no application on this system has the id {desktop_id}")
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

/// Who a running launch is, for the event stream.
///
/// Both kinds of launch report under a `(id, display name)` pair: a
/// package id for a `.qpkg`, a desktop-file id for a host app.
struct LaunchIdentity {
    package_id: String,
    display_name: String,
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
/// `package_id` -> display name for everything this session has
/// launched. Consulted when a foreign toplevel is adopted so the tab
/// shows the app's name instead of a raw wayland app-id.
static LAUNCHED: Mutex<Vec<(String, Vec<String>, String)>> = Mutex::new(Vec::new());
/// The capability list of the workspace apps are being launched into.
///
/// Published by [`set_workspace_capabilities`] (or by
/// [`launch_in_workspace`], which does it for you) the same way
/// [`set_wayland_socket`] publishes the display. Empty is the safe
/// default and the correct one: an app launched before anything is
/// published gets its manifest's sandbox and nothing more.
static WORKSPACE_CAPABILITIES: Mutex<Vec<CapOverride>> = Mutex::new(Vec::new());
/// `package_id` -> where its next window should land, for launches
/// that asked for something other than the default.
///
/// A launch and the toplevel it eventually maps are two events seconds
/// apart, on two processes. This is the note the first leaves for the
/// second; [`placement_for_toplevel`] reads it when the window arrives.
///
/// A note is *claimed* by the first surface that reads it, and then
/// keeps answering for that surface. The compositor adopts every
/// toplevel into two trees - the protocol-side one `XdgShellHandler`
/// mutates and the rendered one the chrome paints from - so a note
/// consumed on the first read would leave the tree the user actually
/// sees falling back to the default, with the two trees disagreeing
/// about where the window went. Claiming rather than consuming is what
/// keeps them in step.
///
/// One note per package, and a *different* surface for the same package
/// retires it: the only honest correlation available is the app-id, and
/// it cannot distinguish two windows from one client.
static PENDING_PLACEMENT: Mutex<Vec<PendingPlacement>> = Mutex::new(Vec::new());

struct PendingPlacement {
    /// The launch this note belongs to — a package id, or a
    /// desktop-file id for a host app. Identities can overlap between
    /// entries, so replacement is keyed on this rather than on them.
    key: String,
    /// Every string the window might announce itself as.
    ///
    /// A qOS package contributes its id and that id's trailing
    /// component. A host app contributes what its `.desktop` entry
    /// says: `StartupWMClass`, the desktop-file id, the Flatpak
    /// application id. Either way the note is matched by *any* of them,
    /// because the toolkit picks which one it reports and the desktop
    /// does not get a say.
    identities: Vec<String>,
    placement: crate::compositor::tab_binding::Placement,
    /// The surface that claimed this note, once one has.
    claimed_by: Option<u64>,
}

impl PendingPlacement {
    fn matches(&self, app_id: &str) -> bool {
        identities_match(&self.identities, app_id)
    }
}

/// Whether `app_id` is one of `identities`, case-insensitively.
fn identities_match(identities: &[String], app_id: &str) -> bool {
    let needle = app_id.trim();
    !needle.is_empty() && identities.iter().any(|i| i.eq_ignore_ascii_case(needle))
}

/// The window identities a qOS package id implies.
///
/// Textual and deterministic: the id itself, and its trailing component
/// (`dev.qos.hello` ↔ `hello`) — the reverse-DNS convention the package
/// ids follow. Nothing here guesses by timing.
fn identities_for_package(package_id: &str) -> Vec<String> {
    let mut out = vec![package_id.to_string()];
    if let Some(last) = package_id.rsplit('.').next() {
        if !last.is_empty() && last != package_id {
            out.push(last.to_string());
        }
    }
    out
}

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

// ---------------------------------------------------------------------
// Workspace capabilities.
//
// One publisher, one reader, one file per launch. The compositor knows
// which workspace an app is being launched into; the launcher knows how
// to turn a capability into a mount. This is the seam between them.
// ---------------------------------------------------------------------

/// Publish the capability list of the workspace apps launch into.
///
/// Call this whenever the active workspace changes or its capabilities
/// do — a grant made in the permissions UI only reaches the next launch
/// if the list is republished. The whole workspace list is published,
/// not a per-app slice: [`launch`] filters it for the app it is starting
/// and the launcher filters it again on the other side.
pub fn set_workspace_capabilities(capabilities: Vec<CapOverride>) {
    *WORKSPACE_CAPABILITIES
        .lock()
        .expect("WORKSPACE_CAPABILITIES poisoned") = capabilities;
}

/// Drop the published capability list. Subsequent launches get manifest
/// permissions only.
pub fn clear_workspace_capabilities() {
    set_workspace_capabilities(Vec::new());
}

/// The capabilities published for `app_id`, as the launcher will see
/// them.
///
/// Filtered by subject: a capability naming a person, a room, or
/// another app never reaches this app's file at all.
///
/// Both effects travel. A `Restrict` is not a grant, but the launcher's
/// decision function needs it to be able to *narrow* one — filtering
/// restrictions out here would silently widen the sandbox.
pub fn capabilities_for_app(app_id: &str) -> Vec<CapOverride> {
    let want = Subject::app(app_id);
    WORKSPACE_CAPABILITIES
        .lock()
        .expect("WORKSPACE_CAPABILITIES poisoned")
        .iter()
        .filter(|c| c.subject == want)
        .cloned()
        .collect()
}

/// Where a launch's capability file lives: `<runtime>/qos/capabilities/`.
///
/// Under `$XDG_RUNTIME_DIR`, which is already a per-user `0700` tmpfs —
/// the file describes what one app may reach and is readable by nobody
/// else. It is deliberately *not* bound into the sandbox: the app never
/// sees its own permission record.
fn capabilities_dir() -> PathBuf {
    runtime_dir().join("qos").join("capabilities")
}

/// Write `capabilities` where `qos-launcher --capabilities` will read
/// them, returning the path.
///
/// `Ok(None)` means "nothing to write": the app was granted nothing, so
/// there is no file and no flag, and the launcher applies the manifest
/// alone. Any previously written file for this app is removed in that
/// case — a revoked grant must not survive as a stale file on disk.
fn write_capabilities_file(
    package_id: &str,
    capabilities: &[CapOverride],
) -> std::io::Result<Option<PathBuf>> {
    // A package id is reverse-DNS (`dev.qos.hello`), but it arrives from
    // a registry file, so refuse anything that could escape the
    // directory rather than trusting that.
    let file_name = format!("{package_id}.json");
    if package_id.is_empty() || package_id.contains('/') || package_id.contains("..") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("refusing to write a capability file for package id {package_id:?}"),
        ));
    }
    let path = capabilities_dir().join(file_name);
    if capabilities.is_empty() {
        // Best-effort: a missing file is the same as an empty one.
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(capabilities_dir())?;
    }
    let json = serde_json::to_vec(capabilities).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("serialising capabilities: {e}"),
        )
    })?;
    // Create at 0600 rather than writing and chmod-ing after: the
    // window between the two is exactly when another process could open
    // it.
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(&json)?;
    file.sync_all()?;
    Ok(Some(path))
}

/// `$XDG_RUNTIME_DIR`, falling back to `/run/user/<uid>`.
pub fn runtime_dir() -> PathBuf {
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
    let entries =
        launcher::registry::list_installed().map_err(|e| LaunchError::RegistryUnreadable {
            detail: e.to_string(),
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
            display_name: manifest
                .name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| display_name_for_package_id(&package_id)),
            category: manifest.category.clone().filter(|c| !c.trim().is_empty()),
            package_id,
            version: manifest.version.clone(),
            manifest_path: entry.manifest_path,
            payload_root: entry.payload_root,
            // Two independent conditions, and both have to hold. The
            // entrypoint says the package can draw; `kind` says it is
            // something a user opens. A package can satisfy the first
            // and not the second — the on-screen keyboard draws a real
            // surface the shell places itself, and a palette row that
            // opened it as a tiled window would be nonsense.
            has_gui: manifest.entrypoint.gui_bin().is_some()
                && manifest.kind == launcher::manifest::KIND_APP,
            opens_extensions: manifest.opens_ext.clone(),
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

/// The installed GUI application that declares it opens `ext`.
///
/// Resolution is by manifest declaration only — there is no built-in
/// table. Two packages claiming the same extension is a real situation
/// (a viewer and an editor both claiming `.pdf`), and the tie is broken
/// by display name so the answer is stable across boots rather than
/// dependent on registry order. A user-visible default-app preference is
/// the right long-term answer to that tie; this is the deterministic
/// stand-in, not a design statement about it.
pub fn app_for_extension(ext: &str) -> Option<InstalledApp> {
    let mut candidates: Vec<InstalledApp> = gui_apps()
        .into_iter()
        .filter(|a| a.opens_extension(ext))
        .collect();
    candidates.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    candidates.into_iter().next()
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
///
/// The environment entry is a **developer-build** seam
/// ([`crate::compositor::dev_env::DEV_ENV_OVERRIDES`]). `qos-launcher` is
/// what builds every app's sandbox, so letting the environment name it means
/// letting the environment decide that apps run unsandboxed. A dev tree does
/// not need it — the compositor's own directory is already the second
/// candidate — so a shipped build simply does not read it.
fn launcher_bin_candidates() -> Vec<PathBuf> {
    launcher_bin_candidates_from(
        crate::compositor::dev_env::DEV_ENV_OVERRIDES,
        std::env::var_os("QOS_LAUNCHER_BIN"),
        std::env::current_exe().ok(),
    )
}

/// [`launcher_bin_candidates`] as a pure function of its inputs, so the
/// shipped ordering is assertable from a debug test run.
fn launcher_bin_candidates_from(
    dev_overrides: bool,
    explicit: Option<std::ffi::OsString>,
    current_exe: Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if dev_overrides {
        if let Some(explicit) = explicit {
            if !explicit.is_empty() {
                out.push(PathBuf::from(explicit));
            }
        }
    }
    if let Some(dir) = current_exe.as_ref().and_then(|exe| exe.parent()) {
        out.push(dir.join("qos-launcher"));
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

/// Render the `qos-launcher` invocation for `app`, launched into
/// `room`.
///
/// Pure — no I/O, no spawning — so tests can assert on the exact command
/// the production path builds.
///
/// `room` is the durable identity of the workspace the app is being
/// opened into, **not** its local [`crate::shell::state::WorkspaceId`].
/// It rides in as `--workspace-id`, which the launcher turns into
/// `QOS_WORKSPACE_ID` inside the sandbox and the toolkit turns into the
/// collaboration cluster every CRDT delta is addressed to. Taking the
/// room rather than the local handle is what lets a peer's app and ours
/// land in the same cluster: the local handle differs on every machine,
/// so an app launched from it could only ever collaborate with apps on
/// this one.
///
/// Without it an app has no room, so two apps the user opened side by
/// side would never see each other's edits — which is why this is a
/// required argument rather than an option: there is no such thing as
/// launching an app into no workspace from the compositor.
///
/// `capabilities_file` is the path [`write_capabilities_file`] produced,
/// or `None` when the app was granted nothing. `None` emits no
/// `--capabilities` flag at all, which is what makes an ungranted app
/// indistinguishable from one launched before capabilities existed: it
/// gets its manifest's sandbox.
///
/// The two are independent on purpose: the workspace id says which room
/// the app collaborates in, the capability file says what it may touch.
/// Naming a room grants nothing.
/// `render_node` is the DRM node this compositor renders on. Passing it
/// down is what makes the compositor's GPU choice authoritative for the
/// app: `qos-launcher` narrows the sandbox to that device and pins the
/// app's EGL/Vulkan loaders to the drivers that match it. A client on a
/// different GPU than its compositor cannot share dma-bufs with it
/// without a cross-device copy, and on many multi-GPU machines cannot
/// share them at all — so this is a correctness argument, not a tuning
/// one. `None` (no node resolved on this machine) launches the app
/// unpinned rather than refusing it: a software-rendered app is still
/// an app.
/// `document`, when set, is the file the app was asked to open. It rides
/// across as `--open <path>` and reaches the app as its `argv[1]`.
///
/// Naming a path here grants nothing. The launcher binds what the
/// manifest declares plus what the capability file allows, and nothing
/// else — so an app handed a path it has no capability for finds it
/// absent from its namespace and fails to open it. Passing the path and
/// granting the path are two separate decisions on purpose, and callers
/// opening a document on the user's behalf have to make both (see
/// [`crate::shell::doc_open`]).
pub fn build_launch_command(
    app: &InstalledApp,
    launcher_bin: &Path,
    wayland_socket: &Path,
    room: RoomId,
    capabilities_file: Option<&Path>,
    render_node: Option<&Path>,
    document: Option<&Path>,
) -> LaunchCommand {
    let mut args = vec![
        "run".to_string(),
        "--manifest".to_string(),
        app.manifest_path.display().to_string(),
        "--payload-root".to_string(),
        app.payload_root.display().to_string(),
        "--wayland-socket".to_string(),
        wayland_socket.display().to_string(),
        "--workspace-id".to_string(),
        room.to_string(),
        // The identity capabilities are addressed to. Always passed, so
        // the launcher checks every capability against this id instead
        // of trusting the file to have been filtered for it.
        "--app-id".to_string(),
        app.package_id.clone(),
    ];
    if let Some(path) = capabilities_file {
        args.push("--capabilities".to_string());
        args.push(path.display().to_string());
    }
    if let Some(node) = render_node {
        args.push("--render-node".to_string());
        args.push(node.display().to_string());
    }
    if let Some(path) = document {
        args.push("--open".to_string());
        args.push(path.display().to_string());
    }
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

/// Resolve `package_id` and start it in `room`, with whatever the
/// published workspace granted it.
///
/// Returns as soon as the launch is handed to its worker thread — it
/// never waits for the app to appear. The `Ok` value is the resolved
/// app, so callers can name it in their UI without a second lookup.
///
/// `room` is the durable identity of the caller's active workspace; it
/// becomes the app's collaboration room (see [`build_launch_command`]).
pub fn launch(package_id: &str, room: RoomId) -> Result<InstalledApp, LaunchError> {
    launch_with_document(package_id, room, None)
}

/// [`launch`], additionally asking the app to open `document`.
///
/// The path is an argument, not a permission: see
/// [`build_launch_command`]. Grant it before calling this, or the app
/// starts and finds nothing at the path it was pointed at.
pub fn launch_with_document(
    package_id: &str,
    room: RoomId,
    document: Option<&Path>,
) -> Result<InstalledApp, LaunchError> {
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

    // Hand the user's decision across to the process that can enforce
    // it. A failure to write the file is *not* a launch failure: the app
    // starts with manifest permissions only, which is the safe
    // direction, and the reason is logged rather than swallowed.
    let capabilities = capabilities_for_app(&app.package_id);
    let capabilities_file = match write_capabilities_file(&app.package_id, &capabilities) {
        Ok(path) => path,
        Err(e) => {
            eprintln!(
                "[launch {}] could not stage {} capabilities ({e}); \
                 starting with manifest permissions only",
                app.package_id,
                capabilities.len()
            );
            None
        }
    };

    let command = build_launch_command(
        &app,
        &launcher_bin,
        &socket,
        room,
        capabilities_file.as_deref(),
        crate::compositor::render_node::resolved(),
        document,
    );
    remember_launch(&app);
    spawn_detached(app.clone(), command);
    Ok(app)
}

/// Publish `workspace`'s capabilities and launch `package_id` into it.
///
/// The one call a caller holding a `&Workspace` needs: it keeps the
/// grant the user made and the sandbox the app runs in from being two
/// separate decisions.
pub fn launch_in_workspace(
    package_id: &str,
    workspace: &Workspace,
) -> Result<InstalledApp, LaunchError> {
    launch_document_in_workspace(package_id, workspace, None)
}

/// [`launch_in_workspace`], additionally asking the app to open
/// `document`.
///
/// The workspace's capability list is published first, exactly as for a
/// plain launch — so a read grant the caller just added to the workspace
/// for this document is what reaches the sandbox, and the path argument
/// alone still reaches nothing.
pub fn launch_document_in_workspace(
    package_id: &str,
    workspace: &Workspace,
    document: Option<&Path>,
) -> Result<InstalledApp, LaunchError> {
    set_workspace_capabilities(workspace.capabilities.clone());
    launch_with_document(package_id, workspace.room, document)
}

/// Run `command` on a worker thread and report the outcome through
/// [`EVENTS`].
///
/// Everything here — the fork, the wait, the output relay — happens off
/// the compositor thread. The thread lives for as long as the app does.
fn spawn_detached(app: InstalledApp, command: LaunchCommand) {
    spawn_launch(app.package_id, app.display_name, command);
}

/// Run `command` on a worker thread under the identity
/// `(package_id, display_name)`, reporting through [`EVENTS`].
///
/// The shared spawn path for both kinds of launch: a `.qpkg` going
/// through `qos-launcher run`, and a host application going through
/// `qos-launcher run-host`. They differ entirely in what the launcher
/// then does — one builds a sandbox, one deliberately does not — and
/// not at all in how the compositor supervises the process, so this is
/// one function rather than two that drift.
pub fn spawn_launch(package_id: String, display_name: String, command: LaunchCommand) {
    let app = LaunchIdentity {
        package_id,
        display_name,
    };
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
    remember_launch_named(
        &app.package_id,
        &identities_for_package(&app.package_id),
        &app.display_name,
    );
}

/// Record that `key` was launched and its window may announce itself as
/// any of `identities`, so a toplevel that arrives later can be given
/// the app's real name.
///
/// The host-app path calls this with the identities its `.desktop`
/// entry declared; see [`set_pending_placement_for_identities`] for why
/// they cannot be derived.
pub fn remember_launch_named(key: &str, identities: &[String], display_name: &str) {
    let mut launched = LAUNCHED.lock().expect("LAUNCHED poisoned");
    if launched.iter().any(|(k, _, _)| k == key) {
        return;
    }
    launched.push((
        key.to_string(),
        identities.to_vec(),
        display_name.to_string(),
    ));
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
    {
        let launched = LAUNCHED.lock().expect("LAUNCHED poisoned");
        if let Some((_, _, display_name)) = launched
            .iter()
            .find(|(_, identities, _)| identities_match(identities, app_id))
        {
            return Some(display_name.clone());
        }
    }
    // Not something this session launched. It may still be a host
    // application the user started another way — the entry is on disk
    // either way, and a tab reading "Spotify" beats one reading
    // "spotify".
    crate::shell::host_apps::app_for_window_id(app_id).map(|app| app.name)
}

/// Whether a client's wayland app-id belongs to `package_id`.
///
/// Textual and deterministic: either the app-id *is* the package id, or
/// it is the package id's trailing component (`dev.qos.hello` ↔
/// `hello`) — the same reverse-DNS convention the package ids follow.
/// Nothing here guesses by timing.
pub(crate) fn app_id_matches_package(app_id: &str, package_id: &str) -> bool {
    let needle = app_id.to_ascii_lowercase();
    let pid = package_id.to_ascii_lowercase();
    pid == needle || pid.rsplit('.').next() == Some(needle.as_str())
}

/// Record where the window of the launch about to start should land.
///
/// Replaces any note for the same package, claimed or not: if two
/// launches of one app are in flight, the newer intent is the live one.
pub fn set_pending_placement(
    package_id: &str,
    placement: crate::compositor::tab_binding::Placement,
) {
    set_pending_placement_for_identities(
        package_id,
        &identities_for_package(package_id),
        placement,
    );
}

/// [`set_pending_placement`] for a launch whose window identities are
/// not derivable from its id.
///
/// A `.qpkg`'s app-id follows the package id by convention, so the qOS
/// path can derive both. A host application's cannot be derived from
/// anything: the `.desktop` entry states it (`StartupWMClass`), or the
/// toolkit picks the desktop-file id, or a Flatpak reports its
/// application id. The entry is the only source, so the caller that
/// read the entry supplies them.
pub fn set_pending_placement_for_identities(
    key: &str,
    identities: &[String],
    placement: crate::compositor::tab_binding::Placement,
) {
    let mut pending = PENDING_PLACEMENT
        .lock()
        .expect("PENDING_PLACEMENT poisoned");
    pending.retain(|p| p.key != key);
    pending.push(PendingPlacement {
        key: key.to_string(),
        identities: identities.to_vec(),
        placement,
        claimed_by: None,
    });
}

/// The placement recorded for whichever package `app_id` belongs to, for
/// the toplevel `surface_id`.
///
/// The first surface to ask claims the note, and every later ask for the
/// same surface gets the same answer - see [`PENDING_PLACEMENT`] for why
/// that matters. A surface arriving after another has claimed the note
/// retires it and gets `None`.
///
/// `None` means "use the default": this client is not one we launched,
/// its launch asked for nothing special, or its note already belongs to
/// an earlier window.
pub fn placement_for_toplevel(
    app_id: &str,
    surface_id: u64,
) -> Option<crate::compositor::tab_binding::Placement> {
    let mut pending = PENDING_PLACEMENT
        .lock()
        .expect("PENDING_PLACEMENT poisoned");
    let idx = pending.iter().position(|p| p.matches(app_id))?;
    match pending[idx].claimed_by {
        None => {
            pending[idx].claimed_by = Some(surface_id);
            Some(pending[idx].placement)
        }
        Some(claimed) if claimed == surface_id => Some(pending[idx].placement),
        // A second window from the same client. The note was for the
        // first one; retire it so a third window can't inherit it either.
        Some(_) => {
            pending.remove(idx);
            None
        }
    }
}

/// Drop every placement note. Used by tests, which share this
/// process-wide state.
pub fn clear_pending_placements() {
    PENDING_PLACEMENT
        .lock()
        .expect("PENDING_PLACEMENT poisoned")
        .clear();
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

    #[test]
    fn dev_build_lets_the_environment_name_the_launcher() {
        let got = launcher_bin_candidates_from(
            true,
            Some(std::ffi::OsString::from("/tmp/my/qos-launcher")),
            Some(PathBuf::from("/home/dev/target/debug/shell")),
        );
        assert_eq!(got[0], PathBuf::from("/tmp/my/qos-launcher"));
        assert_eq!(got[1], PathBuf::from("/home/dev/target/debug/qos-launcher"));
    }

    #[test]
    fn shipped_build_ignores_a_launcher_named_by_the_environment() {
        // The launcher is what builds an app's sandbox; a shipped session
        // must not take that binary's identity from its environment.
        let got = launcher_bin_candidates_from(
            false,
            Some(std::ffi::OsString::from("/tmp/evil/qos-launcher")),
            Some(PathBuf::from("/usr/bin/qos-shell")),
        );
        assert!(
            !got.iter().any(|p| p.starts_with("/tmp")),
            "release build accepted an environment-named launcher: {got:?}"
        );
        assert_eq!(
            got,
            vec![
                PathBuf::from("/usr/bin/qos-launcher"),
                PathBuf::from("/usr/libexec/qos-launcher"),
                PathBuf::from("/usr/bin/qos-launcher"),
            ],
        );
    }

    #[test]
    fn the_install_paths_are_always_searched() {
        let got = launcher_bin_candidates_from(false, None, None);
        assert_eq!(
            got,
            vec![
                PathBuf::from("/usr/libexec/qos-launcher"),
                PathBuf::from("/usr/bin/qos-launcher"),
            ],
        );
    }

    // The tests that repoint `QOS_APPS_DATA_DIR` / `QOS_LAUNCHER_BIN` /
    // the published wayland socket are serialised on `process_env` — the
    // crate's one group for process-global environment. They used to
    // hold a private mutex in this module instead, which serialised them
    // against each other but not against the rest of the crate: any test
    // elsewhere that repoints the same variables (`shell::doc_open`'s
    // opens, for instance) raced them, and the failure landed in
    // whichever of the two got there second.

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
    #[serial_test::serial(process_env)]
    fn enumeration_reads_the_registry_and_flags_gui_apps() {
        let tmp = tempfile::tempdir().unwrap();
        install_fixture(tmp.path(), "dev.qos.hello", "gui");
        install_fixture(tmp.path(), "dev.qos.notes-cli", "cli");
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        invalidate_app_cache();
        let apps = read_installed_apps().expect("registry readable");
        // What the palette actually offers, from the same store.
        let offered = gui_apps();
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

        // Enumerating it is fine; offering it to launch is not. A headless
        // package — `collab-broker` is the shipped one — has no window to
        // show, so a row that starts it would open nothing. This asserts
        // the filter the applications provider actually calls.
        let offered_ids: Vec<&str> = offered.iter().map(|a| a.package_id.as_str()).collect();
        assert_eq!(
            offered_ids,
            vec!["dev.qos.hello"],
            "only the GUI package is launchable from the palette",
        );
    }

    #[test]
    fn launch_command_carries_manifest_payload_and_display() {
        let app = InstalledApp {
            package_id: "dev.qos.hello".into(),
            display_name: "Hello".into(),
            category: None,
            version: "0.1.0".into(),
            manifest_path: PathBuf::from("/apps/hello/manifest.toml"),
            payload_root: PathBuf::from("/apps/hello"),
            has_gui: true,
            opens_extensions: Vec::new(),
        };
        let cmd = build_launch_command(
            &app,
            Path::new("/usr/bin/qos-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            None,
            Some(Path::new("/dev/dri/renderD128")),
            None,
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
        assert_eq!(
            cmd.args[pos("--render-node") + 1],
            "/dev/dri/renderD128",
            "the app must be pointed at THIS compositor's GPU"
        );
    }

    /// A machine where no render node resolved launches the app with no
    /// `--render-node` at all — an empty or invented value would pin the
    /// loaders to nothing, which both loaders read as "no drivers".
    #[test]
    fn launch_command_omits_the_render_node_when_none_resolved() {
        let app = InstalledApp {
            package_id: "dev.qos.hello".into(),
            display_name: "Hello".into(),
            category: None,
            version: "0.1.0".into(),
            manifest_path: PathBuf::from("/apps/hello/manifest.toml"),
            payload_root: PathBuf::from("/apps/hello"),
            has_gui: true,
            opens_extensions: Vec::new(),
        };
        let cmd = build_launch_command(
            &app,
            Path::new("/usr/bin/qos-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            None,
            None,
            None,
        );
        assert!(
            !cmd.args.iter().any(|a| a == "--render-node"),
            "no node resolved must mean no flag: {:?}",
            cmd.args
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn launch_refuses_an_unknown_package() {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        invalidate_app_cache();
        let err = launch("dev.qos.nope", RoomId::new()).unwrap_err();
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
    #[serial_test::serial(process_env)]
    fn launch_refuses_a_cli_only_package() {
        let tmp = tempfile::tempdir().unwrap();
        install_fixture(tmp.path(), "dev.qos.notes-cli", "cli");
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        invalidate_app_cache();
        let err = launch("dev.qos.notes-cli", RoomId::new()).unwrap_err();
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
    #[serial_test::serial(process_env)]
    fn launch_refuses_when_no_compositor_socket_is_published() {
        let tmp = tempfile::tempdir().unwrap();
        install_fixture(tmp.path(), "dev.qos.hello", "gui");
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        invalidate_app_cache();
        set_wayland_socket_path(None);
        let err = launch("dev.qos.hello", RoomId::new()).unwrap_err();
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        invalidate_app_cache();
        assert_eq!(err, LaunchError::NoDisplay);
    }

    /// The production `launch` path, end to end, with `qos-launcher`
    /// replaced by a recorder: a real child process is forked, gets the
    /// real argv, and its failure comes back as a `LaunchEvent`.
    #[test]
    #[serial_test::serial(process_env)]
    fn launch_execs_the_launcher_binary_with_the_compositor_socket() {
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

        // SAFETY: guarded by the `process_env` serial group.
        unsafe {
            std::env::set_var("QOS_APPS_DATA_DIR", tmp.path());
            std::env::set_var("QOS_LAUNCHER_BIN", &fake);
        }
        invalidate_app_cache();
        set_wayland_socket_path(Some(PathBuf::from("/run/user/1000/wayland-7")));
        let _ = drain_events();

        let app = launch("dev.qos.hello", RoomId::new()).expect("launch accepted");
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
        // The compositor's GPU choice reaches the launcher through the
        // real `launch()` path, not just through the pure builder. On a
        // machine with no render node there is nothing to forward, and
        // the flag must then be absent rather than empty.
        match crate::compositor::render_node::resolved() {
            Some(node) => {
                let pos = argv
                    .iter()
                    .position(|a| *a == "--render-node")
                    .unwrap_or_else(|| panic!("--render-node missing from {argv:?}"));
                assert_eq!(
                    argv[pos + 1],
                    node.display().to_string(),
                    "the app must be launched on the compositor's own node"
                );
            }
            None => assert!(
                !argv.contains(&"--render-node"),
                "no node resolved must mean no flag: {argv:?}"
            ),
        }
    }

    /// The launch history is process-global (one session, one history),
    /// so this test uses ids no other test registers rather than
    /// clearing shared state out from under a parallel test.
    #[test]
    fn launched_apps_are_matched_back_by_app_id() {
        remember_launch(&InstalledApp {
            package_id: "dev.qos.matchfixture".into(),
            display_name: "Match Fixture".into(),
            category: None,
            version: "0.1.0".into(),
            manifest_path: PathBuf::new(),
            payload_root: PathBuf::new(),
            has_gui: true,
            opens_extensions: Vec::new(),
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

    // -----------------------------------------------------------------
    // Capabilities crossing to the launcher.
    // -----------------------------------------------------------------

    use crate::shell::state::{Scope, Verb};

    fn test_app(package_id: &str) -> InstalledApp {
        InstalledApp {
            package_id: package_id.to_string(),
            display_name: display_name_for_package_id(package_id),
            category: None,
            version: "0.1.0".into(),
            manifest_path: PathBuf::from("/apps/x/manifest.toml"),
            payload_root: PathBuf::from("/apps/x"),
            has_gui: true,
            opens_extensions: Vec::new(),
        }
    }

    fn fs_grant(app: &str, path: &str, verb: Verb) -> CapOverride {
        CapOverride::grant(Subject::app(app), Scope::FsPath(path.to_string()), verb)
    }

    #[test]
    fn launch_command_names_the_app_id_and_omits_capabilities_when_none_are_granted() {
        let cmd = build_launch_command(
            &test_app("dev.qos.books"),
            Path::new("/usr/bin/qos-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            None,
            None,
            None,
        );
        assert!(
            cmd.args
                .windows(2)
                .any(|w| w[0] == "--app-id" && w[1] == "dev.qos.books"),
            "the launcher must be told which app id capabilities are addressed to: {:?}",
            cmd.args
        );
        assert!(
            !cmd.args.iter().any(|a| a == "--capabilities"),
            "an app granted nothing must not name a capability file: {:?}",
            cmd.args
        );
    }

    #[test]
    fn launch_command_names_the_capability_file_when_one_was_written() {
        let cmd = build_launch_command(
            &test_app("dev.qos.books"),
            Path::new("/usr/bin/qos-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            Some(Path::new(
                "/run/user/1000/qos/capabilities/dev.qos.books.json",
            )),
            None,
            None,
        );
        let pos = cmd
            .args
            .iter()
            .position(|a| a == "--capabilities")
            .expect("--capabilities flag");
        assert_eq!(
            cmd.args[pos + 1],
            "/run/user/1000/qos/capabilities/dev.qos.books.json"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn published_capabilities_are_filtered_to_the_launching_app() {
        set_workspace_capabilities(vec![
            fs_grant("dev.qos.books", "/var/workspace/books", Verb::Read),
            fs_grant("dev.qos.other", "/var/workspace/secrets", Verb::Write),
            // A person's share of a file is a capability in the same
            // list; it is not an app grant and must not travel.
            CapOverride::grant(
                Subject::person("alice"),
                Scope::FsPath("/var/workspace/private".into()),
                Verb::Write,
            ),
        ]);
        let mine = capabilities_for_app("dev.qos.books");
        clear_workspace_capabilities();

        assert_eq!(mine.len(), 1, "only this app's grant travels: {mine:?}");
        assert_eq!(mine[0].scope, Scope::FsPath("/var/workspace/books".into()));
        assert_eq!(mine[0].subject, Subject::app("dev.qos.books"));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn nothing_published_means_nothing_granted() {
        clear_workspace_capabilities();
        assert!(
            capabilities_for_app("dev.qos.books").is_empty(),
            "with no workspace published, an app is granted nothing"
        );
    }

    /// The whole handoff, end to end: publish a workspace capability,
    /// write the file a launch would write, then read it back with
    /// `qos-launcher`'s **own** loader and planner — the same code the
    /// launcher process runs — and check the bind that comes out.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_granted_path_survives_the_handoff_and_becomes_a_read_only_bind() {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmp.path()) };
        set_workspace_capabilities(vec![
            fs_grant("dev.qos.books", "/var/workspace/books", Verb::Read),
            fs_grant("dev.qos.other", "/var/workspace/secrets", Verb::Write),
        ]);

        let caps = capabilities_for_app("dev.qos.books");
        let path = write_capabilities_file("dev.qos.books", &caps)
            .expect("write capability file")
            .expect("a granted app gets a file");

        // Readable only by the user: this file says what one app may
        // reach.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "capability file must not be world-readable");

        let records = launcher::capabilities::load_capability_file(&path);
        let plan = launcher::capabilities::CapabilityPlan::for_app(
            "dev.qos.books",
            &records,
            Some(launcher::capabilities::now_unix()),
        );

        clear_workspace_capabilities();
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };

        assert_eq!(
            plan.ro_paths,
            vec![PathBuf::from("/var/workspace/books")],
            "the granted path arrives at the launcher as a read-only bind: {plan:?}"
        );
        assert!(
            plan.rw_paths.is_empty(),
            "a Read grant must not cross as writable: {plan:?}"
        );
        assert!(
            !plan.allow_network,
            "no network capability was granted: {plan:?}"
        );
    }

    /// Revoking is not a UI state — the file the launcher reads has to
    /// go away too, or the next launch would honour a grant the user
    /// took back.
    #[test]
    #[serial_test::serial(process_env)]
    fn revoking_the_last_capability_removes_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmp.path()) };

        let granted = vec![fs_grant(
            "dev.qos.books",
            "/var/workspace/books",
            Verb::Read,
        )];
        let path = write_capabilities_file("dev.qos.books", &granted)
            .unwrap()
            .expect("file written while granted");
        assert!(path.exists());

        let after_revoke = write_capabilities_file("dev.qos.books", &[]).unwrap();
        let still_there = path.exists();
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };

        assert_eq!(
            after_revoke, None,
            "with nothing granted there is no file to name on the command line"
        );
        assert!(
            !still_there,
            "the stale capability file must be gone, not left for the next launch to read"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_package_id_that_could_escape_the_directory_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmp.path()) };
        let caps = vec![fs_grant("../../etc/x", "/var/workspace/books", Verb::Read)];
        let escaped = write_capabilities_file("../../etc/x", &caps);
        let slashed = write_capabilities_file("a/b", &caps);
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
        assert!(escaped.is_err(), "`..` in a package id must be refused");
        assert!(slashed.is_err(), "`/` in a package id must be refused");
    }
}
