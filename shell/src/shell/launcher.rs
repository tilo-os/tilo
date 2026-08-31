//! Launching real applications from the desktop.
//!
//! The shell never builds a sandbox itself — it execs `qos-launcher`, which
//! reads the manifest, compiles the seccomp filter, mounts the signed
//! runtime bundle, and re-execs as `__sandbox-exec` so Landlock applies
//! after bwrap's mount setup. Out-of-process because the Landlock shim is
//! `current_exe()`-relative and a broken package must not take the
//! compositor down with it.
//!
//! Permissions are the manifest intersected with the workspace's granted
//! capabilities. [`launch`] hands the command to a worker thread that
//! forks/execs and blocks for the child's lifetime; outcomes come back
//! through [`drain_events`], drained once per frame.

use std::collections::{HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::shell::state::{CapOverride, RoomId, Subject, Workspace};
use os_actions::{TOOL_CONTEXT_ENV, ToolIntent};

/// How long an installed-package enumeration is reused before re-reading
/// the registry. Short enough that an app installed elsewhere shows up
/// while the palette query is still being typed.
const APP_CACHE_TTL: Duration = Duration::from_secs(2);

/// Stderr tail kept for a failed launch — enough to carry `bwrap`'s or
/// the dynamic loader's complaint, small enough a chatty app can't grow
/// the shell's memory.
const STDERR_TAIL_LINES: usize = 12;

/// A launcher process is the stable host-side ancestor of its sandbox,
/// including the per-app xdg-dbus-proxy. D-Bus sender names are resolved to
/// PIDs by the bus daemon and then walked back to this table. We retain the
/// kernel start-time as well as the numeric PID so PID reuse cannot attribute a
/// later, unrelated process to an exited app.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LaunchedProcess {
    package_id: String,
    start_time: u64,
}

static LAUNCHED_PROCESSES: std::sync::LazyLock<Mutex<HashMap<u32, LaunchedProcess>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn proc_stat(pid: u32) -> Option<(u32, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm is parenthesised and may contain spaces/parentheses; the final ')'
    // is the only safe place from which to index the remaining fields.
    let rest = stat.rsplit_once(") ")?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // `rest[0]` is field 3 (state), so ppid=field 4 -> 1 and starttime=22 -> 19.
    Some((fields.get(1)?.parse().ok()?, fields.get(19)?.parse().ok()?))
}

fn register_launched_process(pid: u32, package_id: &str) {
    let Some((_, start_time)) = proc_stat(pid) else {
        return;
    };
    if let Ok(mut map) = LAUNCHED_PROCESSES.lock() {
        map.insert(
            pid,
            LaunchedProcess {
                package_id: package_id.to_string(),
                start_time,
            },
        );
    }
}

fn unregister_launched_process(pid: u32) {
    if let Ok(mut map) = LAUNCHED_PROCESSES.lock() {
        map.remove(&pid);
    }
}

/// Resolve a host process (normally xdg-dbus-proxy) to the qOS package
/// supervising one of its ancestors. The walk is bounded and refuses stale
/// registry entries. Unknown/direct session-bus callers stay unattributed.
pub(crate) fn verified_package_for_descendant(mut pid: u32) -> Option<String> {
    const MAX_ANCESTORS: usize = 32;
    for _ in 0..MAX_ANCESTORS {
        let (parent_pid, start_time) = proc_stat(pid)?;
        if let Ok(map) = LAUNCHED_PROCESSES.lock()
            && let Some(known) = map.get(&pid)
            && known.start_time == start_time
        {
            return Some(known.package_id.clone());
        }
        if parent_pid == 0 || parent_pid == pid {
            return None;
        }
        pid = parent_pid;
    }
    None
}

/// One installed application the launcher can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    /// Reverse-DNS package id — the identity `qos-install` records.
    pub package_id: String,
    /// Human-facing name — the manifest's `[package].name`, or derived
    /// from the package id's trailing component when unset.
    pub display_name: String,
    /// The manifest's `[package].category`, when declared; matched by
    /// the Applications search provider.
    pub category: Option<String>,
    pub version: String,
    pub manifest_path: PathBuf,
    pub payload_root: PathBuf,
    /// Whether the package declares a GUI entrypoint; CLI-only packages
    /// have no window, so the applications list hides them.
    pub has_gui: bool,
    /// The manifest's `[providers].ext` — file extensions this package
    /// opens (lowercased, no leading dot). The desktop's only answer to
    /// "which app owns `.odt`": installing an app is what teaches the
    /// desktop its file types.
    pub opens_extensions: Vec<String>,
    /// Concrete normalized MIME claims from `[providers].mime`.
    pub opens_mime_types: Vec<String>,
    /// Normalized URI schemes from `[providers].schemes`.
    pub opens_uri_schemes: Vec<String>,
}

impl InstalledApp {
    /// Whether this package declares it opens `ext` (case-insensitive,
    /// leading dot tolerated).
    pub fn opens_extension(&self, ext: &str) -> bool {
        let want = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        !want.is_empty() && self.opens_extensions.contains(&want)
    }

    pub fn opens_mime(&self, mime: &str) -> bool {
        desktop_policy::ResourceKind::mime(mime).is_ok_and(|resource| {
            resource
                .mime_name()
                .is_some_and(|mime| self.opens_mime_types.iter().any(|claim| claim == mime))
        })
    }

    pub fn opens_scheme(&self, scheme: &str) -> bool {
        desktop_policy::ResourceKind::scheme(scheme).is_ok_and(|resource| {
            resource
                .scheme_name()
                .is_some_and(|scheme| self.opens_uri_schemes.iter().any(|claim| claim == scheme))
        })
    }
}

/// Why a launch could not be started, before any fork. Failures after
/// the fork arrive asynchronously as a [`LaunchEvent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    /// No package with this id in the registry.
    NotInstalled { package_id: String },
    /// The package is installed but declares no GUI entrypoint.
    NotAGuiApp { package_id: String },
    /// `qos-launcher` isn't anywhere we look.
    LauncherBinaryMissing { searched: Vec<PathBuf> },
    /// The compositor hasn't published a Wayland socket to connect to.
    NoDisplay,
    /// No active workspace — both the app's sandbox policy and room.
    NoWorkspace,
    /// A typed intent addresses shell chrome, not an application package.
    ShellOwnedIntent,
    /// The workspace's selected identity could not be resolved safely.
    IdentityUnavailable { detail: String },
    /// The registry file exists but could not be read or parsed.
    RegistryUnreadable { detail: String },
    /// No `.desktop` entry with this id (distinct from
    /// [`LaunchError::NotInstalled`]: nothing qOS packaged is missing).
    HostAppNotFound { desktop_id: String },
}

impl LaunchError {
    /// One-line summary for a notification body.
    pub fn user_message(&self) -> String {
        match self {
            LaunchError::NotInstalled { package_id } => {
                format!("this desktop has no app with the id {package_id}")
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
            LaunchError::ShellOwnedIntent => {
                "this destination belongs to shell chrome, not an application".to_string()
            }
            LaunchError::IdentityUnavailable { detail } => {
                format!("the workspace identity could not be loaded: {detail}")
            }
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
    /// The child could not be spawned at all.
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

/// Who a running launch is, for the event stream — a package id for a
/// `.qpkg`, a desktop-file id for a host app.
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
    /// Environment *overrides* for the child; `WAYLAND_DISPLAY` is set
    /// inside the sandbox by `qos-launcher`, not inherited from here.
    pub env: Vec<(String, String)>,
}

static WAYLAND_SOCKET: Mutex<Option<PathBuf>> = Mutex::new(None);
static EVENTS: Mutex<Vec<LaunchEvent>> = Mutex::new(Vec::new());
static APP_CACHE: Mutex<Option<(Vec<InstalledApp>, Instant)>> = Mutex::new(None);
/// `package_id` -> display name for everything this session has
/// launched, so an adopted foreign toplevel can show the app's name.
static LAUNCHED: Mutex<Vec<(String, Vec<String>, String)>> = Mutex::new(Vec::new());
/// Capabilities of the workspace apps are being launched into, published
/// by [`set_workspace_capabilities`] or [`launch_in_workspace`]. Empty is
/// the safe default: an app launched before publish gets manifest-only.
static WORKSPACE_CAPABILITIES: Mutex<Vec<CapOverride>> = Mutex::new(Vec::new());
/// `package_id` -> where its next window should land; claimed (not
/// consumed) by [`placement_for_toplevel`] since the compositor tracks each
/// toplevel in two trees and consuming here would desync them.
static PENDING_PLACEMENT: Mutex<Vec<PendingPlacement>> = Mutex::new(Vec::new());

/// Locks `mutex`, recovering the guard instead of panicking a second time
/// if some other thread poisoned it. None of this module's locked state
/// (`Vec`/`Option`/plain structs) has an invariant a partial mutation
/// could break, so continuing with whatever the poisoned thread left
/// behind is safer than aborting the whole session over it.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct PendingPlacement {
    /// Package id, or desktop-file id for a host app; keys replacement.
    key: String,
    /// Every string the window might announce itself as (package id +
    /// trailing component, or a host app's `.desktop` identities) —
    /// matched by any of them.
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

/// The window identities a qOS package id implies: the id itself, and
/// its trailing component (`dev.qos.hello` ↔ `hello`).
fn identities_for_package(package_id: &str) -> Vec<String> {
    let mut out = vec![package_id.to_string()];
    if let Some(last) = package_id.rsplit('.').next()
        && !last.is_empty()
        && last != package_id
    {
        out.push(last.to_string());
    }
    out
}

/// Publish the compositor's Wayland socket. Called once by the
/// compositor after `ListeningSocketSource` binds, with the socket name
/// (`wayland-1`) it printed.
pub fn set_wayland_socket(socket_name: &OsStr) {
    let path = runtime_dir().join(socket_name);
    *lock(&WAYLAND_SOCKET) = Some(path);
}

/// The socket [`set_wayland_socket`] published, if any.
pub fn wayland_socket() -> Option<PathBuf> {
    lock(&WAYLAND_SOCKET).clone()
}

/// Publish the capability list of the workspace apps launch into. Call
/// whenever the active workspace or its capabilities change — a grant
/// only reaches the next launch if republished. The whole list is
/// published; [`launch`] and the launcher each filter their own slice.
pub fn set_workspace_capabilities(capabilities: Vec<CapOverride>) {
    *lock(&WORKSPACE_CAPABILITIES) = capabilities;
}

/// Drop the published capability list. Subsequent launches get manifest
/// permissions only.
pub fn clear_workspace_capabilities() {
    set_workspace_capabilities(Vec::new());
}

/// The capabilities published for `app_id`, filtered by subject — a
/// capability naming a person, room, or other app never reaches this
/// app's file. Restrictions travel too: filtering them out here would
/// silently widen the sandbox, since the launcher's decision function
/// needs to be able to *narrow* a grant.
pub fn capabilities_for_app(app_id: &str) -> Vec<CapOverride> {
    let want = Subject::app(app_id);
    lock(&WORKSPACE_CAPABILITIES)
        .iter()
        .filter(|c| c.subject == want)
        .cloned()
        .collect()
}

/// Where a launch's capability file lives: `<runtime>/qos/capabilities/`,
/// under the per-user `0700` runtime tmpfs. Deliberately *not* bound into
/// the sandbox: the app never sees its own permission record.
fn capabilities_dir() -> PathBuf {
    runtime_dir().join("qos").join("capabilities")
}

/// Write `capabilities` where `qos-launcher --capabilities` will read
/// them, returning the path. `Ok(None)` means nothing was granted — no
/// file, no flag, the launcher applies the manifest alone — and any
/// stale file for this app is removed so a revoked grant can't survive
/// on disk.
fn write_capabilities_file(
    package_id: &str,
    capabilities: &[CapOverride],
) -> std::io::Result<Option<PathBuf>> {
    // Create at 0600 rather than writing and chmod-ing after: the window
    // between the two is exactly when another process could open it.
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;

    // Reverse-DNS in principle, but this arrives from a registry file —
    // refuse anything that could escape the directory.
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
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    // SAFETY: getuid always succeeds.
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/run/user/{uid}"))
}

/// Every installed package, GUI or not, freshly read from the registry.
///
/// # Errors
///
/// Returns [`LaunchError::RegistryUnreadable`] if the app registry file
/// cannot be read or parsed.
pub fn read_installed_apps() -> Result<Vec<InstalledApp>, LaunchError> {
    let entries =
        launcher::registry::list_installed().map_err(|e| LaunchError::RegistryUnreadable {
            detail: e.to_string(),
        })?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        // A missing manifest (payload deleted behind the registry's
        // back) is skipped, not a failure of the whole enumeration.
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
            // Both conditions must hold: the entrypoint says the package
            // can draw, `kind` says it's something a user opens — the
            // on-screen keyboard satisfies the first but not the second.
            has_gui: manifest.entrypoint.gui_bin().is_some()
                && manifest.kind == launcher::manifest::KIND_APP,
            opens_extensions: manifest.opens_ext.clone(),
            opens_mime_types: manifest.opens_mime.clone(),
            opens_uri_schemes: manifest.opens_schemes.clone(),
        });
    }
    Ok(out)
}

/// Installed packages, memoised for [`APP_CACHE_TTL`]. Errors are not
/// cached, so a transiently unreadable registry doesn't wedge the
/// launcher for the rest of the TTL.
///
/// # Errors
///
/// Returns an error if the cache is stale and re-reading the registry
/// fails — see [`read_installed_apps`].
pub fn installed_apps() -> Result<Vec<InstalledApp>, LaunchError> {
    {
        let cache = lock(&APP_CACHE);
        if let Some((apps, at)) = cache.as_ref()
            && at.elapsed() < APP_CACHE_TTL
        {
            return Ok(apps.clone());
        }
    }
    let apps = read_installed_apps()?;
    *lock(&APP_CACHE) = Some((apps.clone(), Instant::now()));
    Ok(apps)
}

/// Drop the memoised enumeration. Used after an install/remove and by
/// tests that repoint `QOS_APPS_DATA_DIR`.
pub fn invalidate_app_cache() {
    *lock(&APP_CACHE) = None;
}

/// Installed GUI applications, sorted by display name — what the
/// applications search provider offers.
pub fn gui_apps() -> Vec<InstalledApp> {
    let mut apps = installed_apps().unwrap_or_default();
    apps.retain(|a| a.has_gui);
    apps.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    apps
}

/// Two packages can claim the same extension; the shared policy resolver uses
/// personal/workspace defaults and a deterministic display-name fallback.
pub fn app_for_extension(ext: &str) -> Option<InstalledApp> {
    app_for_extension_with_policy(ext, "", None)
}

/// Resolve an extension through the shared personal/workspace policy engine.
/// The returned id is looked up in the same installed-app snapshot used to
/// build candidates, so policy can never launch an uninstalled or non-claiming
/// package.
pub fn app_for_extension_with_policy(
    ext: &str,
    user_id: &str,
    workspace: Option<&desktop_policy::WorkspaceDefaults>,
) -> Option<InstalledApp> {
    let resource = desktop_policy::ResourceKind::extension(ext).ok()?;
    app_for_resource_with_policy(&resource, user_id, workspace)
}

pub fn app_for_resource_with_policy(
    resource: &desktop_policy::ResourceKind,
    user_id: &str,
    workspace: Option<&desktop_policy::WorkspaceDefaults>,
) -> Option<InstalledApp> {
    let apps = gui_apps();
    let candidates = default_app_candidates_from(&apps);
    let personal = desktop_policy::PersonalDefaults::load();
    let selected = desktop_policy::DefaultAppResolver::new(&candidates)
        .resolve(resource, user_id, &personal, workspace)?;
    apps.into_iter()
        .find(|app| app.package_id == selected.package_id)
}

/// Re-read the installed registry and manifest immediately before launch.
/// Picker state and the two-second application cache are never authority for
/// executing a resource handler: an uninstalled app or a claim removed by an
/// upgrade fails closed here.
pub fn revalidate_resource_claim(
    package_id: &str,
    resource: &desktop_policy::ResourceKind,
) -> bool {
    let Ok(apps) = read_installed_apps() else {
        return false;
    };
    let candidates = default_app_candidates_from(&apps);
    desktop_policy::DefaultAppResolver::new(&candidates)
        .validate_app(resource, package_id)
        .is_ok()
}

pub fn default_app_candidates() -> Vec<desktop_policy::AppCandidate> {
    default_app_candidates_from(&gui_apps())
}

fn default_app_candidates_from(apps: &[InstalledApp]) -> Vec<desktop_policy::AppCandidate> {
    apps.iter()
        .map(|app| desktop_policy::AppCandidate {
            package_id: app.package_id.clone(),
            display_name: app.display_name.clone(),
            resources: app
                .opens_extensions
                .iter()
                .filter_map(|ext| desktop_policy::ResourceKind::extension(ext).ok())
                .chain(
                    app.opens_mime_types
                        .iter()
                        .filter_map(|mime| desktop_policy::ResourceKind::mime(mime).ok()),
                )
                .chain(
                    app.opens_uri_schemes
                        .iter()
                        .filter_map(|scheme| desktop_policy::ResourceKind::scheme(scheme).ok()),
                )
                .collect(),
        })
        .collect()
}

/// `dev.qos.text-editor` → `Text Editor`, derived from the trailing id
/// component since the manifest carries no display name. Lives in the
/// `launcher` crate too, so a package's name can't differ between
/// "offered" and "installed".
pub fn display_name_for_package_id(package_id: &str) -> String {
    launcher::registry::display_name_for_package_id(package_id)
}

/// Order: `QOS_LAUNCHER_BIN` (a **developer-build** seam only — see
/// [`crate::compositor::dev_env::DEV_ENV_OVERRIDES`]), next to the running
/// compositor, then system paths. A shipped build must not let the
/// environment name the binary that builds every app's sandbox.
fn launcher_bin_candidates() -> Vec<PathBuf> {
    let current_exe = std::env::current_exe().ok();
    launcher_bin_candidates_from(
        crate::compositor::dev_env::DEV_ENV_OVERRIDES,
        std::env::var_os("QOS_LAUNCHER_BIN"),
        current_exe.as_deref(),
    )
}

/// [`launcher_bin_candidates`] as a pure function of its inputs, so the
/// shipped ordering is assertable from a debug test run.
fn launcher_bin_candidates_from(
    dev_overrides: bool,
    explicit: Option<std::ffi::OsString>,
    current_exe: Option<&Path>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if dev_overrides
        && let Some(explicit) = explicit
        && !explicit.is_empty()
    {
        out.push(PathBuf::from(explicit));
    }
    if let Some(dir) = current_exe.and_then(Path::parent) {
        out.push(dir.join("qos-launcher"));
    }
    out.push(PathBuf::from("/usr/libexec/qos-launcher"));
    out.push(PathBuf::from("/usr/bin/qos-launcher"));
    out
}

/// First candidate that exists, else the list we searched.
///
/// # Errors
///
/// Returns [`LaunchError::LauncherBinaryMissing`] if `qos-launcher` is not
/// found at any candidate path.
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

/// Pure — no I/O, no spawning — so tests can assert on the exact command.
/// `room` is the workspace's durable identity (not the local
/// [`crate::shell::state::WorkspaceId`]): it becomes `QOS_WORKSPACE_ID`, the
/// collaboration cluster every CRDT delta addresses. `capabilities_file` is
/// `None` when nothing was granted (no `--capabilities` flag).
/// `render_node` pins the app's EGL/Vulkan loaders to this compositor's GPU
/// (dma-bufs generally can't cross GPUs). `document` rides across as
/// `--open <path>` but grants nothing on its own — see
/// [`crate::shell::doc_open`].
#[allow(
    clippy::too_many_arguments,
    reason = "the flat public convenience signature every non-uri caller (production and \
              test) already depends on; `build_launch_command_for` is the one that groups \
              extras into `LaunchExtras`, which this just forwards into with `uri: None`"
)]
pub fn build_launch_command(
    app: &InstalledApp,
    launcher_bin: &Path,
    wayland_socket: &Path,
    room: RoomId,
    capabilities_file: Option<&Path>,
    identity_path: Option<&Path>,
    render_node: Option<&Path>,
    document: Option<&Path>,
) -> LaunchCommand {
    build_launch_command_for(
        app,
        launcher_bin,
        wayland_socket,
        room,
        &LaunchExtras {
            capabilities_file,
            identity_path,
            render_node,
            document,
            uri: None,
        },
    )
}

/// The launch-command inputs beyond core app identity, grouped so
/// [`build_launch_command_for`] stays under clippy's argument-count
/// budget. `capabilities_file` is `None` when nothing was granted (no
/// `--capabilities` flag); `render_node` pins the app's EGL/Vulkan
/// loaders to this compositor's GPU; `document` rides across as
/// `--open <path>`; `uri` is the one opaque URI argument for
/// [`launch_with_uri`]. `Clone`/`Copy` since every field is a reference —
/// [`build_launch_command_for`] takes it by `&` and copies it back apart.
#[derive(Clone, Copy)]
struct LaunchExtras<'a> {
    capabilities_file: Option<&'a Path>,
    identity_path: Option<&'a Path>,
    render_node: Option<&'a Path>,
    document: Option<&'a Path>,
    uri: Option<&'a str>,
}

fn build_launch_command_for(
    app: &InstalledApp,
    launcher_bin: &Path,
    wayland_socket: &Path,
    room: RoomId,
    extras: &LaunchExtras<'_>,
) -> LaunchCommand {
    let LaunchExtras {
        capabilities_file,
        identity_path,
        render_node,
        document,
        uri,
    } = *extras;
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
        // Always passed, so the launcher checks every capability against
        // this id rather than trusting the file to already be filtered.
        "--app-id".to_string(),
        app.package_id.clone(),
    ];
    if let Some(path) = capabilities_file {
        args.push("--capabilities".to_string());
        args.push(path.display().to_string());
    }
    if let Some(path) = identity_path {
        args.push("--identity".to_string());
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
    if let Some(uri) = uri {
        args.push("--open-uri".to_string());
        args.push(uri.to_string());
    }
    // Service sockets are resolved by the launcher against
    // `$XDG_RUNTIME_DIR/qos/services`; a required-but-absent one fails loudly.
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
/// published workspace granted it. Returns as soon as the launch is
/// handed to its worker thread — it never waits for the app to appear.
///
/// # Errors
///
/// Returns an error if `package_id` is not installed or is not a GUI app,
/// if no Wayland socket has been published, or if the launcher binary
/// cannot be found — see [`launch_with_document`].
pub fn launch(package_id: &str, room: RoomId) -> Result<InstalledApp, LaunchError> {
    launch_with_document(package_id, room, None)
}

/// [`launch`], additionally asking the app to open `document`. Grant the
/// path before calling this (see [`build_launch_command`]), or the app
/// starts and finds nothing where it was pointed.
///
/// # Errors
///
/// Returns [`LaunchError::NotInstalled`] if `package_id` names no
/// installed package, [`LaunchError::NotAGuiApp`] if it has no GUI
/// entrypoint, [`LaunchError::NoDisplay`] if the compositor has not
/// published a Wayland socket yet, or an error from
/// [`resolve_launcher_bin`] if `qos-launcher` cannot be found.
pub fn launch_with_document(
    package_id: &str,
    room: RoomId,
    document: Option<&Path>,
) -> Result<InstalledApp, LaunchError> {
    launch_with_context(package_id, room, document, None, None)
}

/// Launch an installed GUI application with one opaque URI argument. The
/// resource dispatcher must validate the scheme claim immediately before
/// calling this; this layer only guarantees the value is not interpreted as a
/// program or split into additional arguments.
///
/// # Errors
///
/// Returns an error under the same conditions as [`launch_with_document`].
pub fn launch_with_uri(
    package_id: &str,
    room: RoomId,
    uri: &str,
) -> Result<InstalledApp, LaunchError> {
    launch_with_context(package_id, room, None, Some(uri), None)
}

/// Launch the only package owned by a typed tool intent and pass that exact
/// context through the trusted launcher. The request format cannot smuggle an
/// executable, package id, or extra arguments into this path.
///
/// # Errors
///
/// Returns an error under the same conditions as [`launch_with_document`],
/// plus [`LaunchError::RegistryUnreadable`] if `intent` fails its own
/// validation or cannot be serialised into the launched process's
/// environment.
pub fn launch_tool_intent(intent: &ToolIntent, room: RoomId) -> Result<InstalledApp, LaunchError> {
    let package_id = intent.package_id().ok_or(LaunchError::ShellOwnedIntent)?;
    launch_with_context(package_id, room, None, None, Some(intent))
}

fn launch_with_context(
    package_id: &str,
    room: RoomId,
    document: Option<&Path>,
    uri: Option<&str>,
    tool_intent: Option<&ToolIntent>,
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

    // A failure to write the capability file is not a launch failure —
    // the app starts with manifest-only permissions, the safe direction.
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
    let identity_path = crate::identity_registry::selected_identity_path(&room.to_string())
        .map_err(|error| LaunchError::IdentityUnavailable {
            detail: error.to_string(),
        })?;

    let mut command = build_launch_command_for(
        &app,
        &launcher_bin,
        &socket,
        room,
        &LaunchExtras {
            capabilities_file: capabilities_file.as_deref(),
            identity_path: identity_path.as_deref(),
            render_node: crate::compositor::render_node::resolved(),
            document,
            uri,
        },
    );
    if let Some(intent) = tool_intent {
        // Validation happens at the service boundary too; repeat it here so
        // this public launcher helper remains safe when called elsewhere.
        intent
            .validate()
            .map_err(|detail| LaunchError::RegistryUnreadable {
                detail: format!("invalid tool context: {detail}"),
            })?;
        let encoded =
            serde_json::to_string(intent).map_err(|e| LaunchError::RegistryUnreadable {
                detail: format!("serialising tool context: {e}"),
            })?;
        command.env.push((TOOL_CONTEXT_ENV.to_string(), encoded));
    }
    remember_launch(&app);
    spawn_detached(app.clone(), command);
    Ok(app)
}

/// Publish `workspace`'s capabilities and launch `package_id` into it —
/// keeps the grant and the sandbox from being two separate decisions.
///
/// # Errors
///
/// Returns an error under the same conditions as [`launch_with_document`].
pub fn launch_in_workspace(
    package_id: &str,
    workspace: &Workspace,
) -> Result<InstalledApp, LaunchError> {
    launch_document_in_workspace(package_id, workspace, None)
}

/// [`launch_in_workspace`], additionally asking the app to open
/// `document`. The workspace's capabilities are published first, so a
/// read grant just added for this document actually reaches the sandbox.
///
/// # Errors
///
/// Returns an error under the same conditions as [`launch_with_document`].
pub fn launch_document_in_workspace(
    package_id: &str,
    workspace: &Workspace,
    document: Option<&Path>,
) -> Result<InstalledApp, LaunchError> {
    set_workspace_capabilities(workspace.capabilities.clone());
    launch_with_document(package_id, workspace.room, document)
}

/// [`launch_uri_in_workspace`] taking the room and capabilities directly
/// rather than a borrowed [`Workspace`], so a caller reading them out of a
/// lock guard can drop it before the launch's file I/O and process spawn
/// rather than holding it through them.
///
/// # Errors
///
/// Returns an error under the same conditions as [`launch_with_document`]
/// (via [`launch_with_uri`]).
pub fn launch_uri_in_room(
    package_id: &str,
    room: RoomId,
    capabilities: Vec<CapOverride>,
    uri: &str,
) -> Result<InstalledApp, LaunchError> {
    set_workspace_capabilities(capabilities);
    launch_with_uri(package_id, room, uri)
}

/// [`launch_uri_in_room`], reading `room` and `capabilities` off `workspace`
/// instead of taking them separately.
///
/// # Errors
///
/// Returns an error under the same conditions as [`launch_with_document`]
/// (via [`launch_uri_in_room`] and [`launch_with_uri`]).
pub fn launch_uri_in_workspace(
    package_id: &str,
    workspace: &Workspace,
    uri: &str,
) -> Result<InstalledApp, LaunchError> {
    launch_uri_in_room(
        package_id,
        workspace.room,
        workspace.capabilities.clone(),
        uri,
    )
}

/// Run `command` on a worker thread and report the outcome through
/// [`EVENTS`]; the thread lives as long as the app does.
fn spawn_detached(app: InstalledApp, command: LaunchCommand) {
    spawn_launch(app.package_id, app.display_name, command);
}

/// Run `command` on a worker thread under `(package_id, display_name)`,
/// reporting through [`EVENTS`]. Shared by both launch kinds — a `.qpkg`
/// via `qos-launcher run` and a host app via `run-host` — since they
/// differ only in what the launcher does, not in how it's supervised.
pub fn spawn_launch(package_id: String, display_name: String, command: LaunchCommand) {
    let app = LaunchIdentity {
        package_id,
        display_name,
    };
    let builder = std::thread::Builder::new().name("qos-app-launch".to_string());
    // Kept for the spawn-failure report below since the closure moves `app`.
    let identity = (app.package_id.clone(), app.display_name.clone());
    let spawn_result = builder.spawn(move || {
        let mut cmd = Command::new(&command.program);
        cmd.args(&command.args);
        for (k, v) in &command.env {
            cmd.env(k, v);
        }
        // Not a terminal job: no stdin, and output is relayed (not
        // inherited) so it can be quoted back in a failure notification.
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
        register_launched_process(child.id(), &app.package_id);
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

        let launcher_pid = child.id();
        let status = child.wait();
        unregister_launched_process(launcher_pid);
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

pub(crate) fn push_event(event: LaunchEvent) {
    lock(&EVENTS).push(event);
}

/// Take every launch event queued since the last call. Called once per
/// frame by the chrome.
pub fn drain_events() -> Vec<LaunchEvent> {
    let mut events = lock(&EVENTS);
    std::mem::take(&mut *events)
}

pub(crate) fn remember_launch(app: &InstalledApp) {
    remember_launch_named(
        &app.package_id,
        &identities_for_package(&app.package_id),
        &app.display_name,
    );
}

/// Records that `key`'s window may announce itself as any of `identities`,
/// so a later toplevel can be given the app's real name.
pub fn remember_launch_named(key: &str, identities: &[String], display_name: &str) {
    let mut launched = lock(&LAUNCHED);
    if launched.iter().any(|(k, _, _)| k == key) {
        return;
    }
    launched.push((
        key.to_string(),
        identities.to_vec(),
        display_name.to_string(),
    ));
}

/// Forget every launch this session has recorded — for tests asserting
/// the fallback resolution rather than a name a neighbouring test left,
/// since the table is process-wide and append-only.
#[cfg(test)]
pub(crate) fn clear_launched_for_test() {
    LAUNCHED.lock().expect("LAUNCHED poisoned").clear();
}

/// Matched textually: either the app-id is the package id, or its trailing
/// component (`dev.qos.hello` ↔ `hello`). An unrelated app-id keeps its own
/// label — closing that gap needs the client correlated back to the
/// spawned pid (`SO_PEERCRED`), not this module.
pub fn display_name_for_app_id(app_id: &str) -> Option<String> {
    {
        let launched = lock(&LAUNCHED);
        if let Some((_, _, display_name)) = launched
            .iter()
            .find(|(_, identities, _)| identities_match(identities, app_id))
        {
            return Some(display_name.clone());
        }
    }
    // Not launched by this session, but the machine still has it: a
    // package's manifest name is the app's name regardless of who
    // started the window.
    if let Ok(apps) = installed_apps()
        && let Some(app) = apps
            .iter()
            .find(|a| app_id_matches_package(app_id, &a.package_id))
    {
        return Some(app.display_name.clone());
    }
    // Nor a package launched this session — maybe a host app started
    // another way; its `.desktop` entry gives a nicer label than the raw id.
    crate::shell::host_apps::app_for_window_id(app_id).map(|app| app.name)
}

/// Whether a client's wayland app-id belongs to `package_id`: either the
/// app-id *is* the package id, or its trailing component.
pub(crate) fn app_id_matches_package(app_id: &str, package_id: &str) -> bool {
    let needle = app_id.to_ascii_lowercase();
    let pid = package_id.to_ascii_lowercase();
    pid == needle || pid.rsplit('.').next() == Some(needle.as_str())
}

/// Record where the window of the launch about to start should land.
/// Replaces any note for the same package — the newer intent wins.
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

/// [`set_pending_placement`] for a launch whose window identities can't
/// be derived from its id — a host app's `.desktop` entry
/// (`StartupWMClass`, desktop-file id, Flatpak app id) is the only
/// source, so the caller that read it supplies them.
pub fn set_pending_placement_for_identities(
    key: &str,
    identities: &[String],
    placement: crate::compositor::tab_binding::Placement,
) {
    let mut pending = lock(&PENDING_PLACEMENT);
    pending.retain(|p| p.key != key);
    pending.push(PendingPlacement {
        key: key.to_string(),
        identities: identities.to_vec(),
        placement,
        claimed_by: None,
    });
}

/// The first surface to ask claims the note and gets the same answer on
/// every later ask; a different surface retires it and gets `None`. `None`
/// also covers: not launched by us, or nothing special asked.
pub fn placement_for_toplevel(
    app_id: &str,
    surface_id: u64,
) -> Option<crate::compositor::tab_binding::Placement> {
    let mut pending = lock(&PENDING_PLACEMENT);
    let idx = pending.iter().position(|p| p.matches(app_id))?;
    let claimed_by = pending.get(idx)?.claimed_by;
    let placement = pending.get(idx)?.placement;
    match claimed_by {
        None => {
            if let Some(entry) = pending.get_mut(idx) {
                entry.claimed_by = Some(surface_id);
            }
            Some(placement)
        }
        Some(claimed) if claimed == surface_id => Some(placement),
        // A second window from the same client — the note was for the
        // first one; retire it so a third can't inherit it either.
        Some(_) => {
            pending.remove(idx);
            None
        }
    }
}

/// Drop the placement note for `package_id`, if any — undoes a
/// [`set_pending_placement`] whose launch then failed to start.
pub fn clear_pending_placement(package_id: &str) {
    let mut pending = lock(&PENDING_PLACEMENT);
    pending.retain(|p| p.key != package_id);
}

/// Drop every placement note. Used by tests, which share this
/// process-wide state.
pub fn clear_pending_placements() {
    lock(&PENDING_PLACEMENT).clear();
}

/// Test hook: publish a socket path directly (production goes through
/// [`set_wayland_socket`], which resolves it against `$XDG_RUNTIME_DIR`).
#[cfg(test)]
pub(crate) fn set_wayland_socket_path(path: Option<PathBuf>) {
    *WAYLAND_SOCKET.lock().expect("WAYLAND_SOCKET poisoned") = path;
}

/// Rendered `OsString` form of the socket name for logging.
pub fn wayland_socket_display() -> OsString {
    wayland_socket().map_or_else(
        || OsString::from("<none>"),
        std::path::PathBuf::into_os_string,
    )
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
            Some(Path::new("/home/dev/target/debug/shell")),
        );
        assert_eq!(got[0], PathBuf::from("/tmp/my/qos-launcher"));
        assert_eq!(got[1], PathBuf::from("/home/dev/target/debug/qos-launcher"));
    }

    #[test]
    fn shipped_build_ignores_a_launcher_named_by_the_environment() {
        // A shipped session must not take the sandbox-building binary's
        // identity from its environment.
        let got = launcher_bin_candidates_from(
            false,
            Some(std::ffi::OsString::from("/tmp/evil/qos-launcher")),
            Some(Path::new("/usr/bin/qos-shell")),
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

    // Tests that repoint process-global env vars are serialised on
    // `process_env`, shared with other tests touching the same variables.

    /// Write a registry + manifest pair into `dir` and point the
    /// launcher's registry reader at it. Returns the app's payload root.
    fn install_fixture(dir: &Path, package_id: &str, entrypoint_type: &str) -> PathBuf {
        use std::fmt::Write as _;
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
        write!(
            existing,
            "[[entry]]\nid = '{package_id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
            payload.display().to_string(),
            manifest_path.display().to_string()
        )
        .unwrap();
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
        // A degenerate id must not produce an empty (unclickable) label.
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
        let offered = gui_apps(); // what the palette actually offers
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

        // Enumerating it is fine; offering it to launch is not — a
        // headless package has no window to show.
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
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
        };
        let cmd = build_launch_command(
            &app,
            Path::new("/usr/bin/qos-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            None,
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

    /// No render node resolved means no `--render-node` flag at all — an
    /// empty or invented value would read to both loaders as "no drivers".
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
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
        };
        let cmd = build_launch_command(
            &app,
            Path::new("/usr/bin/qos-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            None,
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
        let message = err.user_message();
        assert!(
            message.contains("dev.qos.nope"),
            "the message must name the id it could not find: {message}"
        );
        // "installed" is not a word this desktop says to a user.
        assert!(
            !message.contains("install"),
            "user-facing text must not talk about installing: {message}"
        );
    }

    #[test]
    fn shell_owned_intent_never_falls_through_to_the_app_registry() {
        let intent = ToolIntent::WorkspaceSettings {
            route: os_actions::WorkspaceSettingsRoute::Members,
        };
        assert_eq!(
            launch_tool_intent(&intent, RoomId::new()).unwrap_err(),
            LaunchError::ShellOwnedIntent
        );
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

    /// The production `launch` path end to end, with `qos-launcher`
    /// replaced by a recorder that fails and is quoted back as a
    /// `LaunchEvent`.
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

        // wait for the worker to report the child's exit
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
        // The GPU choice must reach the launcher through the real
        // `launch()` path here, not just the pure builder.
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

    /// The launch history is process-global, so this test uses ids no
    /// other test registers rather than clearing shared state.
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
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
        });
        // full package id, trailing component, case-insensitivity
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
        // an unrelated client keeps its own label
        assert_eq!(display_name_for_app_id("org.gnome.NeverLaunched"), None);
    }

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
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
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
    fn launch_command_names_the_workspace_selected_identity() {
        let cmd = build_launch_command(
            &test_app("dev.qos.books"),
            Path::new("/usr/bin/qos-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            None,
            Some(Path::new("/home/alice/.config/qos/identity/alice.json")),
            None,
            None,
        );
        let position = cmd
            .args
            .iter()
            .position(|arg| arg == "--identity")
            .expect("--identity flag");
        assert_eq!(
            cmd.args[position + 1],
            "/home/alice/.config/qos/identity/alice.json"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn published_capabilities_are_filtered_to_the_launching_app() {
        set_workspace_capabilities(vec![
            fs_grant("dev.qos.books", "/var/workspace/books", Verb::Read),
            fs_grant("dev.qos.other", "/var/workspace/secrets", Verb::Write),
            // A person's share is a capability in the same list but not
            // an app grant, and must not travel.
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

    /// The whole handoff end to end: publish a capability, write the
    /// file a launch would write, then read it back with `qos-launcher`'s
    /// **own** loader and planner and check the bind that comes out.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_granted_path_survives_the_handoff_and_becomes_a_read_only_bind() {
        use std::os::unix::fs::PermissionsExt;
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

        // readable only by the user: this file says what one app may reach
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

    #[test]
    fn dbus_proxy_descendant_resolves_only_while_launcher_identity_is_live() {
        let parent = std::process::id();
        register_launched_process(parent, "dev.qos.mail");
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 2"])
            .spawn()
            .expect("spawn descendant");
        assert_eq!(
            verified_package_for_descendant(child.id()).as_deref(),
            Some("dev.qos.mail")
        );
        unregister_launched_process(parent);
        assert_eq!(verified_package_for_descendant(child.id()), None);
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn stale_start_time_is_never_accepted_as_pid_identity() {
        let pid = std::process::id();
        LAUNCHED_PROCESSES.lock().unwrap().insert(
            pid,
            LaunchedProcess {
                package_id: "dev.qos.spoof".into(),
                start_time: u64::MAX,
            },
        );
        assert_eq!(verified_package_for_descendant(pid), None);
        unregister_launched_process(pid);
    }
}
