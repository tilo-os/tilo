//! Launching applications from the desktop.
//!
//! The shell never builds a sandbox itself — it execs `tilo-launcher`, which
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

#[cfg(any(test, target_os = "linux"))]
use std::collections::VecDeque;
#[cfg(any(test, target_os = "linux"))]
use std::process::Command;
#[cfg(any(test, target_os = "linux"))]
use std::process::Stdio;

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
#[cfg(any(test, target_os = "linux"))]
use std::io::{self, Read, Write};
#[cfg(any(test, target_os = "linux"))]
use std::os::fd::AsRawFd;
#[cfg(any(test, target_os = "linux"))]
use std::os::unix::net::UnixStream;
#[cfg(any(test, target_os = "linux"))]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
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
#[cfg(any(test, target_os = "linux"))]
const STDERR_TAIL_LINES: usize = 12;
/// Maximum bytes retained for one stdout/stderr line from an untrusted app.
/// Reading with `BufRead::lines` grows until a newline and lets a package
/// exhaust compositor memory with one unterminated write.
#[cfg(any(test, target_os = "linux"))]
const APP_LOG_LINE_BYTES: usize = 8 * 1024;
const SERVICE_LAUNCH_TTL: Duration = Duration::from_secs(30);
/// The signed headless package the desktop is allowed to manage for Task
/// Center work. Keeping this fixed prevents a task title or agent id from
/// becoming an executable/package selector.
pub const WORKSPACE_AGENT_PACKAGE_ID: &str = "dev.tilo.agent";

#[cfg(any(test, target_os = "linux"))]
fn relay_capped_lines<R: Read>(mut reader: R, mut on_line: impl FnMut(String)) {
    let mut chunk = [0_u8; 4096];
    let mut line = Vec::with_capacity(APP_LOG_LINE_BYTES.min(chunk.len()));
    let mut truncated = false;
    let emit = |line: &mut Vec<u8>, truncated: &mut bool, on_line: &mut dyn FnMut(String)| {
        let mut text = String::from_utf8_lossy(line).into_owned();
        if *truncated {
            text.push_str("… [truncated]");
        }
        on_line(text);
        line.clear();
        *truncated = false;
    };
    while let Ok(read) = reader.read(&mut chunk) {
        if read == 0 {
            break;
        }
        let Some(bytes) = chunk.get(..read) else {
            break;
        };
        for &byte in bytes {
            if byte == b'\n' {
                emit(&mut line, &mut truncated, &mut on_line);
            } else if line.len() < APP_LOG_LINE_BYTES {
                line.push(byte);
            } else {
                truncated = true;
            }
        }
    }
    if !line.is_empty() || truncated {
        emit(&mut line, &mut truncated, &mut on_line);
    }
}

/// A launcher process is the stable host-side ancestor of its sandbox,
/// including the per-app xdg-dbus-proxy. D-Bus sender names are resolved to
/// PIDs by the bus daemon and then walked back to this table. We retain the
/// kernel start-time as well as the numeric PID so PID reuse cannot attribute a
/// later, unrelated process to an exited app.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LaunchedProcess {
    package_id: String,
    /// Durable room identity supplied by the compositor for this launch.
    /// Kept host-side; it is not trusted when read back from a sandbox.
    workspace_id: Option<String>,
    start_time: u64,
    /// True only for a qpkg launch carrying compositor-minted service
    /// identity. Host/Flatpak launches are attributed to a room for placement
    /// and portals but their Wayland app ids remain compatibility-owned.
    confined: bool,
}

static LAUNCHED_PROCESSES: std::sync::LazyLock<Mutex<HashMap<u32, LaunchedProcess>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceLaunchIdentity {
    pub app_id: String,
    pub workspace_id: String,
    pub allowed_compat_entry_ids: Vec<String>,
    pub compat_entry_generation: u64,
}

#[derive(Clone, Debug)]
struct TrustedServiceLaunch {
    start_time: u64,
    identity: ServiceLaunchIdentity,
    registered_at: Instant,
}

static TRUSTED_SERVICE_LAUNCHES: std::sync::LazyLock<Mutex<HashMap<u32, TrustedServiceLaunch>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static WINDOWS_COMPAT_ENTRY_IDS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static WINDOWS_COMPAT_ENTRY_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[cfg(target_os = "linux")]
fn proc_stat(pid: u32) -> Option<(u32, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm is parenthesised and may contain spaces/parentheses; the final ')'
    // is the only safe place from which to index the remaining fields.
    let rest = stat.rsplit_once(") ")?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // `rest[0]` is field 3 (state), so ppid=field 4 -> 1 and starttime=22 -> 19.
    Some((fields.get(1)?.parse().ok()?, fields.get(19)?.parse().ok()?))
}

// Linux process ancestry is the authority for sandbox launch attribution.
// Other hosts must not infer equivalent authority from numeric PIDs.
#[cfg(not(target_os = "linux"))]
fn proc_stat(_pid: u32) -> Option<(u32, u64)> {
    None
}

#[cfg(any(test, target_os = "linux"))]
fn register_launched_process(
    pid: u32,
    package_id: &str,
    workspace_id: Option<&str>,
    confined: bool,
) {
    let Some((_, start_time)) = proc_stat(pid) else {
        return;
    };
    if let Ok(mut map) = LAUNCHED_PROCESSES.lock() {
        map.insert(
            pid,
            LaunchedProcess {
                package_id: package_id.to_string(),
                workspace_id: workspace_id.map(str::to_string),
                start_time,
                confined,
            },
        );
    }
}

#[cfg(any(test, target_os = "linux"))]
const LAUNCH_REGISTRATION_FD_ENV: &str = "TILO_INTERNAL_LAUNCH_REGISTRATION_FD";

/// Hand the launcher a private inherited descriptor that its process entry
/// waits on before doing any launch work. `Command::spawn` can therefore
/// return after exec, the compositor registers the real pid, and only then
/// releases the launcher without a fork/exec race.
#[cfg(any(test, target_os = "linux"))]
fn install_launch_registration_gate(command: &mut Command) -> io::Result<UnixStream> {
    let (release, wait) = UnixStream::pair()?;
    let wait_fd = wait.as_raw_fd();
    command.env(LAUNCH_REGISTRATION_FD_ENV, wait_fd.to_string());
    // SAFETY: the closure runs after fork and before exec and calls only the
    // async-signal-safe `fcntl(2)`. The closure retains the child copy through
    // exec; the launcher closes it immediately after reading release.
    unsafe {
        command.pre_exec(move || {
            let inherited_fd = wait.as_raw_fd();
            let flags = libc::fcntl(inherited_fd, libc::F_GETFD);
            if flags == -1
                || libc::fcntl(inherited_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(release)
}

/// Durable room ids for live, launcher-verified instances of `package_id`.
/// Stale PID records are discarded before returning. Portal attribution uses
/// this instead of trusting the backend caller's `app_id` alone.
pub(crate) fn verified_workspaces_for_app(package_id: &str) -> Vec<String> {
    let mut result = Vec::new();
    let Ok(mut map) = LAUNCHED_PROCESSES.lock() else {
        return result;
    };
    map.retain(|pid, launch| {
        let live = proc_stat(*pid).is_some_and(|(_, start)| start == launch.start_time);
        if live
            && launch.package_id == package_id
            && let Some(room) = launch.workspace_id.as_ref()
        {
            result.push(room.clone());
        }
        live
    });
    result.sort();
    result.dedup();
    result
}

/// Package ids with a launcher-verified live process. Background portal state
/// is derived from this host-owned registry rather than an application's own
/// claims. Stale PID/start-time pairs are removed as part of the snapshot.
#[cfg(any(test, target_os = "linux"))]
pub(crate) fn verified_running_packages() -> Vec<String> {
    let Ok(mut map) = LAUNCHED_PROCESSES.lock() else {
        return Vec::new();
    };
    let mut packages = Vec::new();
    map.retain(|pid, launch| {
        let live = proc_stat(*pid).is_some_and(|(_, start)| start == launch.start_time);
        if live {
            packages.push(launch.package_id.clone());
        }
        live
    });
    packages.sort();
    packages.dedup();
    packages
}

#[cfg(any(test, target_os = "linux"))]
fn register_trusted_service_launch(pid: u32, identity: ServiceLaunchIdentity) {
    let Some((_, start_time)) = proc_stat(pid) else {
        return;
    };
    let mut launches = lock(&TRUSTED_SERVICE_LAUNCHES);
    launches.retain(|_, launch| launch.registered_at.elapsed() <= SERVICE_LAUNCH_TTL);
    launches.insert(
        pid,
        TrustedServiceLaunch {
            start_time,
            identity,
            registered_at: Instant::now(),
        },
    );
}

/// Consume the compositor's one-shot authority record for a launcher child.
/// Kernel process facts corroborate the record but never create authority.
pub fn consume_trusted_service_launch_identity(
    pid: u32,
    start_time: u64,
    app_id: &str,
    workspace_id: &str,
) -> Option<ServiceLaunchIdentity> {
    let mut launches = lock(&TRUSTED_SERVICE_LAUNCHES);
    launches.retain(|_, launch| launch.registered_at.elapsed() <= SERVICE_LAUNCH_TTL);
    let launch = launches.get(&pid)?;
    if launch.start_time != start_time
        || launch.identity.app_id != app_id
        || launch.identity.workspace_id != workspace_id
    {
        return None;
    }
    launches.remove(&pid).map(|launch| launch.identity)
}

/// Compatibility service view of [`consume_trusted_service_launch_identity`].
/// The compatibility server only needs its fixed entry snapshot; the
/// collaboration broker consumes the full record to authenticate a launch.
pub fn consume_trusted_service_launch(
    pid: u32,
    start_time: u64,
    app_id: &str,
    workspace_id: &str,
) -> Option<Vec<String>> {
    consume_trusted_service_launch_identity(pid, start_time, app_id, workspace_id)
        .map(|identity| identity.allowed_compat_entry_ids)
}

/// Atomically consume a service launch record using the kernel's current PID
/// start-time. A direct `tilo-launcher` invocation has no compositor record,
/// and a reused PID cannot inherit a dead launch's authority.
pub fn consume_trusted_service_launch_for_pid(
    pid: u32,
    app_id: &str,
    workspace_id: &str,
) -> Option<ServiceLaunchIdentity> {
    let (_, start_time) = proc_stat(pid)?;
    consume_trusted_service_launch_identity(pid, start_time, app_id, workspace_id)
}

/// Verify (without spending) the shell-owned process record for a service
/// descriptor. Collaboration and compatibility may both be declared by one
/// package, so consuming collaboration's descriptor must not invalidate the
/// compatibility service's independent one-shot handoff.
pub fn attest_trusted_service_launch_for_pid(
    pid: u32,
    app_id: &str,
    workspace_id: &str,
) -> Option<ServiceLaunchIdentity> {
    let (_, start_time) = proc_stat(pid)?;
    let mut launches = lock(&TRUSTED_SERVICE_LAUNCHES);
    launches.retain(|_, launch| launch.registered_at.elapsed() <= SERVICE_LAUNCH_TTL);
    let identity = launches.get(&pid).and_then(|launch| {
        (launch.start_time == start_time
            && launch.identity.app_id == app_id
            && launch.identity.workspace_id == workspace_id)
            .then(|| launch.identity.clone())
    });
    // Keep the registry lock out of the return expression. This function is
    // called while service descriptors are being inspected, and no caller
    // needs the registry held while it handles the resulting identity.
    drop(launches);
    identity
}

/// Publish the server-owned compatibility entries that a newly launched
/// Windows manager may address. Existing launches deliberately retain their
/// one-shot snapshot.
#[must_use]
pub fn set_windows_compat_entry_ids(mut entry_ids: Vec<String>) -> u64 {
    entry_ids.sort();
    entry_ids.dedup();
    let mut current = lock(&WINDOWS_COMPAT_ENTRY_IDS);
    if *current != entry_ids {
        *current = entry_ids;
        drop(current);
        return WINDOWS_COMPAT_ENTRY_GENERATION
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            .saturating_add(1);
    }
    WINDOWS_COMPAT_ENTRY_GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

#[must_use]
/// Current server-owned entry registry generation. A manager records this at
/// launch; an update becomes visible only through a newly authenticated launch.
pub fn windows_compat_entry_generation() -> u64 {
    WINDOWS_COMPAT_ENTRY_GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

#[cfg(any(test, target_os = "linux"))]
fn unregister_launched_process(pid: u32) {
    if let Ok(mut map) = LAUNCHED_PROCESSES.lock() {
        map.remove(&pid);
    }
    lock(&TRUSTED_SERVICE_LAUNCHES).remove(&pid);
}

/// Host-owned identity of a process descended from a launcher supervised by
/// the compositor. Neither field is read from the sandbox environment or from
/// a Wayland protocol string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerifiedLaunchIdentity {
    pub package_id: String,
    pub workspace_id: Option<String>,
    pub confined: bool,
}

/// Resolve a descendant to the exact live launch record that owns it.
/// Kernel parent links and process start-times prevent app-id spoofing and PID
/// reuse; the bounded walk fails closed for daemonised or unknown clients.
pub(crate) fn verified_launch_for_descendant(mut pid: u32) -> Option<VerifiedLaunchIdentity> {
    const MAX_ANCESTORS: usize = 32;
    for _ in 0..MAX_ANCESTORS {
        let (parent_pid, start_time) = proc_stat(pid)?;
        if let Ok(map) = LAUNCHED_PROCESSES.lock()
            && let Some(known) = map.get(&pid)
            && known.start_time == start_time
        {
            return Some(VerifiedLaunchIdentity {
                package_id: known.package_id.clone(),
                workspace_id: known.workspace_id.clone(),
                confined: known.confined,
            });
        }
        if parent_pid == 0 || parent_pid == pid {
            return None;
        }
        pid = parent_pid;
    }
    None
}

/// Resolve a host process (normally xdg-dbus-proxy) to the Tilo package
/// supervising one of its ancestors. The walk is bounded and refuses stale
/// registry entries. Unknown/direct session-bus callers stay unattributed.
pub(crate) fn verified_package_for_descendant(pid: u32) -> Option<String> {
    verified_launch_for_descendant(pid).map(|identity| identity.package_id)
}

const FILES_PACKAGE_ID: &str = "dev.tilo.file-explorer";
const TRUSTED_IDENTITIES_FILE: &str = "trusted-identities.json";
static PROJECTION_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static TRUSTED_IDENTITY_PROJECTIONS: LazyLock<
    Mutex<std::collections::HashMap<RoomId, tilo_user_identity::TrustedIdentityProjectionV2>>,
> = LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// One installed application the launcher can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    /// Reverse-DNS package id — the identity `tilo-install` records.
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
    /// The selected host has no authority to execute this package.
    ProviderUnavailable { provider: &'static str },
    /// No package with this id in the registry.
    NotInstalled { package_id: String },
    /// The package is installed but declares no GUI entrypoint.
    NotAGuiApp { package_id: String },
    /// The fixed workspace-agent package is not a headless service package.
    NotAManagedService { package_id: String },
    /// The requested agent is not currently admitted to this workspace.
    AgentNotAdmitted { agent_id: String },
    /// Delegated agents may only be started by a workspace administrator.
    AgentLaunchNotAuthorized,
    /// The shell could not even create the worker that starts a managed
    /// agent; callers must show Retry rather than claiming it is starting.
    AgentLaunchWorkerUnavailable { detail: String },
    /// `tilo-launcher` isn't anywhere we look.
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
    /// [`LaunchError::NotInstalled`]: nothing Tilo packaged is missing).
    HostAppNotFound { desktop_id: String },
}

impl LaunchError {
    /// One-line summary for a notification body.
    pub fn user_message(&self) -> String {
        match self {
            LaunchError::ProviderUnavailable { provider } => {
                format!("required {provider} provider is unavailable on this host")
            }
            LaunchError::NotInstalled { package_id } => {
                format!("this desktop has no app with the id {package_id}")
            }
            LaunchError::NotAGuiApp { package_id } => {
                format!("{package_id} has no graphical entrypoint")
            }
            LaunchError::NotAManagedService { package_id } => {
                format!("{package_id} is not an installed managed service")
            }
            LaunchError::AgentNotAdmitted { agent_id } => {
                format!("{agent_id} is not an enabled agent in this workspace")
            }
            LaunchError::AgentLaunchNotAuthorized => {
                "only a workspace administrator can start an agent".to_string()
            }
            LaunchError::AgentLaunchWorkerUnavailable { detail } => {
                format!("could not start the agent launcher: {detail}")
            }
            LaunchError::LauncherBinaryMissing { searched } => {
                let list: Vec<String> = searched.iter().map(|p| p.display().to_string()).collect();
                format!("tilo-launcher not found (looked in {})", list.join(", "))
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

    /// Concise recovery copy for fixed-height desktop notifications. Detailed
    /// ids, paths, and backend errors belong in the session log and in the
    /// fallible caller's error, not in top-right chrome.
    pub(crate) fn notification_message(&self) -> &'static str {
        match self {
            Self::ProviderUnavailable { .. } => {
                "The required app execution service is unavailable. Try again when it is available."
            }
            Self::NotInstalled { .. } | Self::HostAppNotFound { .. } => {
                "This app is no longer installed. Refresh the app list and try again."
            }
            Self::NotAGuiApp { .. } => "This app does not provide a window.",
            Self::NotAManagedService { .. } => "This service cannot be opened as an app.",
            Self::AgentNotAdmitted { .. } | Self::AgentLaunchNotAuthorized => {
                "You do not have permission to start this workspace service."
            }
            Self::AgentLaunchWorkerUnavailable { .. } | Self::LauncherBinaryMissing { .. } => {
                "The app launcher is unavailable. Try again."
            }
            Self::NoDisplay => "The desktop is still starting. Try again in a moment.",
            Self::NoWorkspace => "Choose a workspace, then try again.",
            Self::ShellOwnedIntent => "Open this destination from the desktop controls.",
            Self::IdentityUnavailable { .. } => {
                "The workspace identity could not be loaded. Try again."
            }
            Self::RegistryUnreadable { .. } => "The app list could not be read. Try again.",
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
    /// The delegated agent this event belongs to, when the desktop started a
    /// managed Task Center service. Ordinary app launches leave this unset.
    pub managed_agent_id: Option<String>,
    /// Workspace paired with [`Self::managed_agent_id`], keeping a launch
    /// event from one room from changing another room's Task Center state.
    pub managed_workspace_id: Option<String>,
    pub kind: LaunchEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchEventKind {
    /// `tilo-launcher` is running with this host pid.
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

/// Normal-user copy for an asynchronous launch failure. The launcher keeps
/// the complete process output in the event; this deliberately does not.
/// Fixed-height desktop banners must remain a summary, not become a log
/// viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchFailureNotice {
    pub title: String,
    pub body: &'static str,
}

/// Long enough to read and act on, but not a persistent alarm for a
/// recoverable failure.
pub(crate) const LAUNCH_FAILURE_NOTICE_LIFETIME: std::time::Duration =
    std::time::Duration::from_secs(8);

impl LaunchEvent {
    /// Build the short, actionable banner for a failed launch. Full process
    /// diagnostics are emitted by [`Self::log_failure`] instead of being
    /// copied into notification chrome.
    pub(crate) fn failure_notice(&self) -> Option<LaunchFailureNotice> {
        let body = match &self.kind {
            LaunchEventKind::SpawnFailed { .. } => {
                "The app launcher is unavailable. Open Applications and try again."
            }
            LaunchEventKind::Failed { .. } => {
                "The app stopped before opening. Open Applications and try again."
            }
            LaunchEventKind::Started { .. } | LaunchEventKind::Exited => return None,
        };
        Some(LaunchFailureNotice {
            title: launch_failure_title(&self.display_name),
            body,
        })
    }

    /// Preserve the complete failure in the compositor's session log. Field
    /// values use tracing's structured formatting, so embedded newlines or
    /// terminal escapes cannot alter the surrounding log record.
    pub(crate) fn log_failure(&self) {
        match &self.kind {
            LaunchEventKind::SpawnFailed { detail } => {
                tracing::error!(
                    target: "tilo::app_launch",
                    package_id = %self.package_id,
                    display_name = %self.display_name,
                    detail = ?detail,
                    "application launcher could not be started"
                );
            }
            LaunchEventKind::Failed { code, stderr_tail } => {
                tracing::error!(
                    target: "tilo::app_launch",
                    package_id = %self.package_id,
                    display_name = %self.display_name,
                    exit_code = ?code,
                    stderr_tail = ?stderr_tail,
                    "application stopped before presenting a window"
                );
            }
            LaunchEventKind::Started { .. } | LaunchEventKind::Exited => {}
        }
    }
}

/// Keep untrusted desktop-entry names on one notification line. Whitespace is
/// normalised and the scalar-value cap avoids slicing UTF-8; the full name is
/// still present in the structured failure log.
fn compact_notice_name(name: &str) -> String {
    const MAX_CHARS: usize = 32;
    let normalised = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalised = if normalised.is_empty() {
        "application"
    } else {
        &normalised
    };
    let mut chars = normalised.chars();
    let compact: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{compact}…")
    } else {
        compact
    }
}

pub(crate) fn launch_failure_title(display_name: &str) -> String {
    format!("Couldn't open {}", compact_notice_name(display_name))
}

/// Who a running launch is, for the event stream — a package id for a
/// `.qpkg`, a desktop-file id for a host app.
#[cfg(any(test, target_os = "linux"))]
struct LaunchIdentity {
    package_id: String,
    display_name: String,
    managed_agent_id: Option<String>,
    managed_workspace_id: Option<String>,
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
    /// inside the sandbox by `tilo-launcher`, not inherited from here.
    pub env: Vec<(String, String)>,
    /// Compositor-owned service authority registered only after this exact
    /// launcher process exists. Never serialised into the child environment.
    pub service_identity: Option<ServiceLaunchIdentity>,
    /// Workspace used only to attribute standard desktop-portal requests from
    /// an unconfined host/Flatpak process.  This is deliberately separate
    /// from `service_identity`: it grants no Tilo service authority and never
    /// enters the child environment.
    pub portal_workspace_id: Option<String>,
    /// Correlates launch events to one delegated agent without putting that
    /// identifier in ambient environment state.
    pub managed_agent_id: Option<String>,
}

static WAYLAND_SOCKET: Mutex<Option<PathBuf>> = Mutex::new(None);
static EVENTS: Mutex<Vec<LaunchEvent>> = Mutex::new(Vec::new());
static APP_CACHE: Mutex<Option<(Vec<InstalledApp>, Instant)>> = Mutex::new(None);
static APP_CACHE_REVISION: AtomicU64 = AtomicU64::new(0);
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
/// Resume bytes waiting for the user-directed launch of one dormant tab.
type ContinuationResumePayload = Option<(u16, Vec<u8>)>;
type StagedContinuationResume = (ContinuationResumePayload, u64);

static PENDING_CONTINUATION_RESUME: LazyLock<
    Mutex<HashMap<(RoomId, u64), StagedContinuationResume>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

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
    /// Package id, or desktop-file id for a host app; identifies one launch
    /// within the FIFO queue for that application.
    key: String,
    /// Every string the window might announce itself as (package id +
    /// trailing component, or a host app's `.desktop` identities) —
    /// matched by any of them.
    identities: Vec<String>,
    placement: crate::shell::tab_binding::Placement,
    /// Generated when the process is actually launched, then carried to the
    /// app in `TILO_CONTINUITY_INSTANCE` until its first toplevel claims this
    /// note. It is not generated by the pure command builder.
    continuity_instance: Option<u64>,
    /// The surface that claimed this note, once one has.
    claimed_by: Option<u64>,
    /// A successful launcher parent may exit after daemonising before its
    /// actual GUI process maps a window. Keep the destination until this
    /// deadline; a truly windowless launch is retired once it passes.
    retire_after: Option<Instant>,
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

/// The window identities a Tilo package id implies: the id itself, and
/// its trailing component (`dev.tilo.hello` ↔ `hello`).
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

/// Where a launch's capability file lives: `<runtime>/tilo/capabilities/`,
/// under the per-user `0700` runtime tmpfs. Deliberately *not* bound into
/// the sandbox: the app never sees its own permission record.
fn capabilities_dir() -> PathBuf {
    runtime_dir().join("tilo").join("capabilities")
}

/// Host-only staging directory for Files' identity projection. It is outside
/// the app's writable directory and only the final file is bind-mounted into
/// the sandbox.
fn trusted_identities_dir() -> PathBuf {
    runtime_dir().join("tilo").join("identity-projections")
}

/// Atomically stage a bounded governance projection for one room.
///
/// The session owns both directory and file. A temporary 0600 inode is fully
/// written and synced before rename, so Files sees either the previous whole
/// snapshot or the next whole snapshot, never a prefix. This function accepts
/// only the typed governance format; Contacts and presence payloads cannot be
/// accidentally serialized through it.
///
/// # Errors
///
/// Returns an I/O error when validation, serialization, bounded staging, or
/// the atomic durable replacement fails.
pub fn write_trusted_identities_projection(
    room: RoomId,
    projection: &tilo_user_identity::TrustedIdentityProjectionV2,
) -> std::io::Result<PathBuf> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

    if let Err(error) = projection.validate() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid trusted identity projection: {error}"),
        ));
    }
    let bytes = serde_json::to_vec(projection).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("serialising trusted identities: {error}"),
        )
    })?;
    if bytes.len() as u64 > tilo_user_identity::MAX_TRUSTED_IDENTITY_PROJECTION_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "trusted identity projection exceeds its byte bound",
        ));
    }

    let directory = trusted_identities_dir().join(room.to_string());
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    // Tighten a pre-existing directory as well; create_dir_all's mode applies
    // only to newly-created components.
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;

    let final_path = directory.join(TRUSTED_IDENTITIES_FILE);
    let sequence = PROJECTION_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp_path = directory.join(format!(
        ".{TRUSTED_IDENTITIES_FILE}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp_path, &final_path)?;
        std::fs::set_permissions(&final_path, std::fs::Permissions::from_mode(0o600))?;
        std::fs::File::open(&directory)?.sync_all()?;
        Ok(final_path.clone())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

/// Publish a complete accepted room-governance projection for later Files
/// launches. Callers derive this from `WorkspaceTree`'s accepted room document,
/// never Contacts or presence.
pub fn set_trusted_identity_projection(
    room: RoomId,
    projection: &tilo_user_identity::TrustedIdentityProjectionV2,
) {
    if projection.validate().is_err() {
        return;
    }
    if let Ok(mut projections) = TRUSTED_IDENTITY_PROJECTIONS.lock() {
        projections.insert(room, projection.clone());
    }
    // Existing Files sandboxes bind the containing directory, so this atomic
    // replacement becomes visible to them as well as to later launches.
    if let Err(error) = write_trusted_identities_projection(room, projection) {
        tracing::warn!(%room, %error, "could not publish live governed identity projection");
    }
}

fn stage_current_trusted_identities(room: RoomId) -> std::io::Result<PathBuf> {
    let projection = TRUSTED_IDENTITY_PROJECTIONS
        .lock()
        .ok()
        .and_then(|projections| projections.get(&room).cloned())
        .unwrap_or_default();
    write_trusted_identities_projection(room, &projection)
}

fn attach_trusted_identities_projection(
    app: &InstalledApp,
    room: RoomId,
    command: &mut LaunchCommand,
) -> std::io::Result<()> {
    if app.package_id != FILES_PACKAGE_ID {
        return Ok(());
    }
    let path = stage_current_trusted_identities(room)?;
    command.args.push("--trusted-identities".to_string());
    command.args.push(path.display().to_string());
    Ok(())
}

/// Write `capabilities` where `tilo-launcher --capabilities` will read
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
    #[cfg(target_os = "macos")]
    {
        // Darwin supplies a per-user temporary directory through TMPDIR.
        std::env::temp_dir()
    }
    #[cfg(not(target_os = "macos"))]
    {
        // SAFETY: getuid always succeeds.
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/run/user/{uid}"))
    }
}

/// Every installed package, GUI or not, freshly read from the registry.
///
/// # Errors
///
/// Returns [`LaunchError::RegistryUnreadable`] if the app registry file
/// cannot be read or parsed.
pub fn read_installed_apps() -> Result<Vec<InstalledApp>, LaunchError> {
    tracing::info!(
        target: "tilo::app_registry",
        system_registry = ?launcher::registry::system_registry_path(),
        system_payload = ?std::env::var_os("TILO_APPS_SYSTEM_PAYLOAD_DIR"),
        user_registry = ?launcher::registry::default_registry_path().ok(),
        "reading installed application registry"
    );
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

/// Result of resolving an installed package's signed continuity contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstalledContinuityRequirements {
    /// No installed package owns this id (development/legacy callers use the
    /// framework presentation baseline).
    NotInstalled,
    /// Signed contract parsed from the installed manifest. An old manifest
    /// produces the presentation-only default.
    Required(launcher::manifest::ContinuityContract),
    /// Registry or manifest authority was present but unreadable/inconsistent.
    /// Callers must reject rather than silently weaken the signed contract.
    Invalid,
}

/// Read the signed continuity contract for an installed package without using
/// the UI-oriented app cache. This keeps every checkpoint tied to current
/// install authority and distinguishes legacy absence from corruption.
pub fn installed_continuity_contract(package_id: &str) -> InstalledContinuityRequirements {
    let Ok(entries) = launcher::registry::list_installed() else {
        return InstalledContinuityRequirements::Invalid;
    };
    let Some(entry) = entries.into_iter().find(|entry| entry.id == package_id) else {
        return InstalledContinuityRequirements::NotInstalled;
    };
    let Ok(manifest) = launcher::manifest::read_manifest(&entry.manifest_path) else {
        return InstalledContinuityRequirements::Invalid;
    };
    if !manifest.package_id.is_empty() && manifest.package_id != package_id {
        return InstalledContinuityRequirements::Invalid;
    }
    InstalledContinuityRequirements::Required(manifest.continuity)
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
    APP_CACHE_REVISION.fetch_add(1, Ordering::Relaxed);
    Ok(apps)
}

/// Drop the memoised enumeration. Used after an install/remove and by
/// tests that repoint `TILO_APPS_DATA_DIR`.
pub fn invalidate_app_cache() {
    *lock(&APP_CACHE) = None;
    APP_CACHE_REVISION.fetch_add(1, Ordering::Relaxed);
}

/// Immutable installed-app snapshot revision for retained search projections.
pub fn gui_apps_snapshot() -> (u64, Vec<InstalledApp>) {
    // Pair a source generation with the exact projection read under it. If
    // cache invalidation races this small read, return the observed changed
    // generation; retained index workers will reject it instead of treating
    // old records as a current snapshot.
    let before = APP_CACHE_REVISION.load(Ordering::Acquire);
    let apps = gui_apps();
    let after = APP_CACHE_REVISION.load(Ordering::Acquire);
    (if before == after { before } else { 0 }, apps)
}

/// Current installed-app source generation.  Search projections use this
/// cheap read between rebuilds; callers that need the records themselves use
/// [`gui_apps_snapshot`].
#[must_use]
pub fn gui_apps_generation() -> u64 {
    APP_CACHE_REVISION.load(Ordering::Relaxed)
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

/// `dev.tilo.text-editor` → `Text Editor`, derived from the trailing id
/// component since the manifest carries no display name. Lives in the
/// `launcher` crate too, so a package's name can't differ between
/// "offered" and "installed".
pub fn display_name_for_package_id(package_id: &str) -> String {
    launcher::registry::display_name_for_package_id(package_id)
}

/// Order: `TILO_LAUNCHER_BIN` (a **developer-build** seam only — see
/// [`crate::compositor::dev_env::DEV_ENV_OVERRIDES`]), next to the running
/// compositor, then system paths. A shipped build must not let the
/// environment name the binary that builds every app's sandbox.
fn launcher_bin_candidates() -> Vec<PathBuf> {
    let current_exe = std::env::current_exe().ok();
    launcher_bin_candidates_from(
        cfg!(debug_assertions),
        std::env::var_os("TILO_LAUNCHER_BIN"),
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
        out.push(dir.join("tilo-launcher"));
    }
    out.push(PathBuf::from("/usr/libexec/tilo-launcher"));
    out.push(PathBuf::from("/usr/bin/tilo-launcher"));
    out
}

/// First candidate that exists, else the list we searched.
///
/// # Errors
///
/// Returns [`LaunchError::LauncherBinaryMissing`] if `tilo-launcher` is not
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
/// [`crate::shell::state::WorkspaceId`]): it becomes `TILO_WORKSPACE_ID`, the
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
        Some(wayland_socket),
        room,
        &LaunchExtras {
            capabilities_file,
            identity_path,
            render_node,
            document,
            uri: None,
            continuity_tab_id: None,
        },
    )
}

/// Build a launch command for the durable shell tab that owns this app
/// instance. The tab id is passed through the launcher environment and becomes
/// the record component of the app's authenticated collaboration route, so a
/// later continuation capture can attach metadata to this exact tab.
#[allow(
    clippy::too_many_arguments,
    reason = "launch authority and durable tab identity are explicit boundary inputs"
)]
pub fn build_launch_command_for_tab(
    app: &InstalledApp,
    launcher_bin: &Path,
    wayland_socket: &Path,
    room: RoomId,
    capabilities_file: Option<&Path>,
    identity_path: Option<&Path>,
    render_node: Option<&Path>,
    document: Option<&Path>,
    tab_id: u64,
) -> LaunchCommand {
    build_launch_command_for(
        app,
        launcher_bin,
        Some(wayland_socket),
        room,
        &LaunchExtras {
            capabilities_file,
            identity_path,
            render_node,
            document,
            uri: None,
            continuity_tab_id: Some(tab_id),
        },
    )
}

/// Build the sandboxed, display-less launch for one admitted workspace
/// agent. The executable and package id come only from the fixed signed
/// package; `agent_id` is forwarded as one opaque argv value and is never
/// interpreted as a command line.
pub fn build_workspace_agent_command(
    app: &InstalledApp,
    launcher_bin: &Path,
    room: RoomId,
    identity_path: Option<&Path>,
    agent_id: &str,
) -> LaunchCommand {
    let mut command = build_launch_command_for(
        app,
        launcher_bin,
        None,
        room,
        &LaunchExtras {
            capabilities_file: None,
            identity_path,
            render_node: None,
            document: None,
            uri: None,
            continuity_tab_id: None,
        },
    );
    command.args.push("--service".into());
    command.args.push("--".into());
    command.args.extend([
        "--workspace".into(),
        room.to_string(),
        "--app".into(),
        WORKSPACE_AGENT_PACKAGE_ID.into(),
        "--name".into(),
        agent_id.to_string(),
        "--task-runs".into(),
    ]);
    command.managed_agent_id = Some(agent_id.to_string());
    command
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
    /// Durable shell tab id, when this launch belongs to a tiled app instance.
    continuity_tab_id: Option<u64>,
}

fn build_launch_command_for(
    app: &InstalledApp,
    launcher_bin: &Path,
    wayland_socket: Option<&Path>,
    room: RoomId,
    extras: &LaunchExtras<'_>,
) -> LaunchCommand {
    let LaunchExtras {
        capabilities_file,
        identity_path,
        render_node,
        document,
        uri,
        continuity_tab_id,
    } = *extras;
    let mut args = vec![
        "run".to_string(),
        "--manifest".to_string(),
        app.manifest_path.display().to_string(),
        "--payload-root".to_string(),
        app.payload_root.display().to_string(),
        "--workspace-id".to_string(),
        room.to_string(),
        // Always passed, so the launcher checks every capability against
        // this id rather than trusting the file to already be filtered.
        "--app-id".to_string(),
        app.package_id.clone(),
    ];
    if let Some(wayland_socket) = wayland_socket {
        args.push("--wayland-socket".to_string());
        args.push(wayland_socket.display().to_string());
    }
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
    // `$XDG_RUNTIME_DIR/tilo/services`; a required-but-absent one fails loudly.
    if std::env::var_os("TILO_LAUNCH_NO_SERVICES").is_some() {
        args.push("--no-services".to_string());
    }
    let mut env: Vec<(String, String)> = std::iter::once((
        "XDG_RUNTIME_DIR".to_string(),
        runtime_dir().display().to_string(),
    ))
    .chain(
        continuity_tab_id
            .map(|tab_id| ("TILO_CONTINUITY_INSTANCE".to_string(), tab_id.to_string())),
    )
    .collect();
    // The developer session wrapper validates these paths and exports them as
    // one matched registry/payload/runtime overlay. Production builds do not
    // read or forward the variables at all: a normal user's environment must
    // never be able to replace package authority for a shipped compositor.
    if crate::compositor::dev_env::DEV_ENV_OVERRIDES {
        for name in [
            "TILO_APPS_SYSTEM_DIR",
            "TILO_APPS_SYSTEM_PAYLOAD_DIR",
            "TILO_RUNTIME_BUNDLE_DIR",
            "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE",
        ] {
            if let Some(value) = std::env::var_os(name).filter(|value| !value.is_empty()) {
                env.push((name.to_string(), value.to_string_lossy().into_owned()));
            }
        }
    }
    LaunchCommand {
        program: launcher_bin.to_path_buf(),
        args,
        env,
        service_identity: Some(ServiceLaunchIdentity {
            app_id: app.package_id.clone(),
            workspace_id: room.to_string(),
            allowed_compat_entry_ids: if app.package_id == "dev.tilo.windows" {
                lock(&WINDOWS_COMPAT_ENTRY_IDS).clone()
            } else {
                Vec::new()
            },
            compat_entry_generation: windows_compat_entry_generation(),
        }),
        portal_workspace_id: None,
        managed_agent_id: None,
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

/// Start one admitted agent as a desktop-managed headless service in its
/// workspace. This is the native Task Center path: the signed package is
/// resolved again immediately before launch, then receives the same
/// launcher-authenticated collaboration descriptor and selected identity as
/// any other workspace app. Per-agent action authority remains broker-side.
///
/// # Errors
///
/// Returns an error when the caller is not a workspace administrator, the
/// agent is not admitted, the fixed agent service is missing/malformed, or
/// the normal identity/launcher setup fails.
pub fn launch_workspace_agent(
    agent_id: &str,
    workspace: &Workspace,
) -> Result<InstalledApp, LaunchError> {
    let agent_id = agent_id.trim();
    if workspace
        .role_of_agent(agent_id)
        .is_none_or(|role| role == crate::shell::state::CapRole::Denied)
    {
        return Err(LaunchError::AgentNotAdmitted {
            agent_id: agent_id.to_string(),
        });
    }
    if !workspace.may_administer(&crate::shell::state::local_user_id()) {
        return Err(LaunchError::AgentLaunchNotAuthorized);
    }
    let app = installed_apps()?
        .into_iter()
        .find(|app| app.package_id == WORKSPACE_AGENT_PACKAGE_ID)
        .ok_or_else(|| LaunchError::NotInstalled {
            package_id: WORKSPACE_AGENT_PACKAGE_ID.into(),
        })?;
    let manifest = launcher::manifest::read_manifest(&app.manifest_path).map_err(|error| {
        LaunchError::RegistryUnreadable {
            detail: format!("reading the workspace-agent manifest: {error}"),
        }
    })?;
    let requires_collaboration = manifest
        .sandbox
        .services
        .get(launcher::services::COLLAB_SERVICE)
        .is_some_and(|requirement| {
            launcher::manifest::ServiceReq::parse(requirement)
                .is_ok_and(|requirement| requirement.is_required())
        });
    if manifest.kind != launcher::manifest::KIND_SERVICE
        || manifest.entrypoint.cli_bin().is_none()
        || !requires_collaboration
    {
        return Err(LaunchError::NotAManagedService {
            package_id: app.package_id,
        });
    }
    execute_workspace_agent(app, agent_id, workspace)
}

#[cfg(not(target_os = "linux"))]
fn execute_workspace_agent(
    _app: InstalledApp,
    _agent_id: &str,
    _workspace: &Workspace,
) -> Result<InstalledApp, LaunchError> {
    Err(LaunchError::ProviderUnavailable {
        provider: "workspace process service",
    })
}

#[cfg(target_os = "linux")]
fn execute_workspace_agent(
    app: InstalledApp,
    agent_id: &str,
    workspace: &Workspace,
) -> Result<InstalledApp, LaunchError> {
    let launcher_bin = selected_execution_program()?;
    let identity_path = crate::identity_registry::selected_identity_path(
        &workspace.room.to_string(),
    )
    .map_err(|error| LaunchError::IdentityUnavailable {
        detail: error.to_string(),
    })?;
    let command = build_workspace_agent_command(
        &app,
        &launcher_bin,
        workspace.room,
        identity_path.as_deref(),
        agent_id,
    );
    if let Err(detail) = spawn_detached(app.clone(), command) {
        return Err(LaunchError::AgentLaunchWorkerUnavailable { detail });
    }
    Ok(app)
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
/// [`resolve_launcher_bin`] if `tilo-launcher` cannot be found.
pub fn launch_with_document(
    package_id: &str,
    room: RoomId,
    document: Option<&Path>,
) -> Result<InstalledApp, LaunchError> {
    launch_with_context(package_id, room, document, None, None, None)
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
    launch_with_context(package_id, room, None, Some(uri), None, None)
}

/// Launch a media application at an exact playhead after an authenticated
/// destination device accepted a private handoff action.
///
/// # Errors
///
/// Returns an error under the same conditions as [`launch_with_document`].
pub fn launch_media_handoff(
    package_id: &str,
    room: RoomId,
    locator: &crate::shell::device_fabric::MediaSourceLocator,
    title: &str,
    request_id: [u8; 16],
    position_ms: u64,
    resume_playing: bool,
) -> Result<InstalledApp, LaunchError> {
    let state = MediaHandoffLaunch {
        title: title.to_string(),
        request_id,
        position_ms,
        resume_playing,
    };
    match locator {
        crate::shell::device_fabric::MediaSourceLocator::Workspace { path } => {
            let document = media::Source::workspace_root().join(path);
            launch_with_context(package_id, room, Some(&document), None, None, Some(state))
        }
        crate::shell::device_fabric::MediaSourceLocator::RemoteStream { url } => {
            launch_with_context(package_id, room, None, Some(url), None, Some(state))
        }
        crate::shell::device_fabric::MediaSourceLocator::AppResource { uri } => {
            launch_with_context(package_id, room, None, Some(uri), None, Some(state))
        }
        crate::shell::device_fabric::MediaSourceLocator::PersonalTransfer { .. } => {
            // Transfer identities are deliberately not paths. The fabric
            // runtime must resolve and hash-verify one before launching.
            Err(LaunchError::NoWorkspace)
        }
    }
}

/// Launch media from a shell-owned, content-verified personal-transfer cache.
/// The cache path never appears in the wire locator and is passed through the
/// same document-capability path as a user-opened local file.
///
/// # Errors
///
/// Returns an error under the same conditions as [`launch_media_handoff`].
pub fn launch_verified_media_handoff(
    package_id: &str,
    room: RoomId,
    path: &Path,
    title: &str,
    request_id: [u8; 16],
    position_ms: u64,
    resume_playing: bool,
) -> Result<InstalledApp, LaunchError> {
    let state = MediaHandoffLaunch {
        title: title.to_string(),
        request_id,
        position_ms,
        resume_playing,
    };
    launch_with_context(package_id, room, Some(path), None, None, Some(state))
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
    launch_with_context(package_id, room, None, None, Some(intent), None)
}

#[derive(Clone)]
struct MediaHandoffLaunch {
    title: String,
    request_id: [u8; 16],
    position_ms: u64,
    resume_playing: bool,
}

fn attach_media_handoff(command: &mut LaunchCommand, handoff: MediaHandoffLaunch) {
    command
        .env
        .push(("TILO_MEDIA_HANDOFF_TITLE".to_string(), handoff.title));
    command.env.push((
        "TILO_MEDIA_HANDOFF_REQUEST_ID".to_string(),
        hex::encode(handoff.request_id),
    ));
    command.env.push((
        "TILO_MEDIA_HANDOFF_POSITION_MS".to_string(),
        handoff.position_ms.to_string(),
    ));
    command.env.push((
        "TILO_MEDIA_HANDOFF_RESUME_PLAYING".to_string(),
        u8::from(handoff.resume_playing).to_string(),
    ));
}

fn launch_with_context(
    package_id: &str,
    room: RoomId,
    document: Option<&Path>,
    uri: Option<&str>,
    tool_intent: Option<&ToolIntent>,
    media_handoff: Option<MediaHandoffLaunch>,
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
    execute_installed(app, room, document, uri, tool_intent, media_handoff, None)
}

#[cfg(test)]
thread_local! {
    static TEST_EXECUTION_PROGRAM: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_test_execution_program(program: Option<PathBuf>) {
    TEST_EXECUTION_PROGRAM.with(|slot| *slot.borrow_mut() = program);
}

fn selected_execution_program() -> Result<PathBuf, LaunchError> {
    #[cfg(test)]
    if let Some(program) = TEST_EXECUTION_PROGRAM.with(|slot| slot.borrow().clone()) {
        return Ok(program);
    }
    #[cfg(target_os = "linux")]
    {
        resolve_launcher_bin()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(LaunchError::ProviderUnavailable {
            provider: "Linux package execution",
        })
    }
}

fn execute_installed(
    app: InstalledApp,
    room: RoomId,
    document: Option<&Path>,
    uri: Option<&str>,
    tool_intent: Option<&ToolIntent>,
    media_handoff: Option<MediaHandoffLaunch>,
    capability_snapshot: Option<Vec<CapOverride>>,
) -> Result<InstalledApp, LaunchError> {
    tracing::info!(target: "tilo::launch_probe", package_id = %app.package_id, "launch preflight begin");
    let socket = wayland_socket().ok_or(LaunchError::NoDisplay)?;
    let launcher_bin = selected_execution_program()?;
    tracing::info!(target: "tilo::launch_probe", package_id = %app.package_id, "launcher and display resolved");

    // A failure to write the capability file is not a launch failure —
    // the app starts with manifest-only permissions, the safe direction.
    let capabilities = capability_snapshot.unwrap_or_else(|| capabilities_for_app(&app.package_id));
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
    tracing::info!(target: "tilo::launch_probe", package_id = %app.package_id, "identity selection resolved");

    let mut command = build_launch_command_for(
        &app,
        &launcher_bin,
        Some(&socket),
        room,
        &LaunchExtras {
            capabilities_file: capabilities_file.as_deref(),
            identity_path: identity_path.as_deref(),
            render_node: crate::compositor::render_node::resolved(),
            document,
            uri,
            continuity_tab_id: None,
        },
    );
    // This id is a durable shell-id allocation, not a random process token.
    // The pending-placement claim binds it to whichever tab the launch
    // actually adopts; sibling windows cannot overwrite each other's report.
    let staged_resume = take_staged_resume_for_package(&app.package_id, room);
    // A restored tab must reconnect to the source record.  A fresh launch
    // still receives a newly allocated record identity until it adopts a tab.
    let continuity_instance = staged_resume
        .as_ref()
        .map_or_else(crate::shell::state::next_id, |(_, record_instance)| {
            *record_instance
        });
    command.env.push((
        "TILO_CONTINUITY_INSTANCE".to_string(),
        continuity_instance.to_string(),
    ));
    command.env.push((
        "TILO_STARTUP_METRICS_PATH".to_string(),
        format!("/run/tilo/app/startup-{continuity_instance}.json"),
    ));
    attach_continuity_instance(&app.package_id, continuity_instance);
    tracing::info!(target: "tilo::launch_probe", package_id = %app.package_id, "continuity staged");
    if let Some((Some((schema, resume)), _)) = staged_resume {
        command.env.push((
            "TILO_CONTINUITY_RESUME_SCHEMA".to_string(),
            schema.to_string(),
        ));
        command.env.push((
            "TILO_CONTINUITY_RESUME_HEX".to_string(),
            hex::encode(resume),
        ));
    }
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
    if let Some(handoff) = media_handoff {
        attach_media_handoff(&mut command, handoff);
    }
    if let Err(error) = attach_trusted_identities_projection(&app, room, &mut command) {
        eprintln!(
            "[launch {}] could not stage the governance identity projection ({error}); \
             Files will use an empty directory",
            app.package_id
        );
    }
    tracing::info!(target: "tilo::launch_probe", package_id = %app.package_id, "governance projection staged");
    remember_launch(&app);
    let _ = spawn_detached(app.clone(), command);
    tracing::info!(target: "tilo::launch_probe", package_id = %app.package_id, "launch worker queued");
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

/// Queue a GUI launch without doing registry, identity, or filesystem work on
/// the compositor thread.  The workspace inputs are cloned before the worker
/// starts, so a later workspace switch cannot redirect this launch or widen
/// its capabilities.
///
/// The ordinary synchronous helpers remain available to non-interactive
/// callers that need their immediate error.  Desktop activation must use this
/// path: registry verification and identity lookup can involve cold storage or
/// D-Bus and must never stall input or presentation.
pub fn queue_launch_in_workspace(
    app: InstalledApp,
    workspace: &Workspace,
) -> Result<(), LaunchError> {
    let package_id = app.package_id.clone();
    let display_name = app.display_name.clone();
    let room = workspace.room;
    let capabilities = workspace.capabilities.clone();
    std::thread::Builder::new()
        .name("tilo-launch-preflight".to_string())
        .spawn(move || {
            let app_capabilities = capabilities
                .into_iter()
                .filter(|capability| capability.subject == Subject::app(&package_id))
                .collect();
            let outcome =
                execute_installed(app, room, None, None, None, None, Some(app_capabilities));
            if let Err(error) = outcome {
                push_event(LaunchEvent {
                    package_id,
                    display_name,
                    managed_agent_id: None,
                    managed_workspace_id: None,
                    kind: LaunchEventKind::SpawnFailed {
                        detail: error.user_message(),
                    },
                });
            }
        })
        .map_err(|error| LaunchError::AgentLaunchWorkerUnavailable {
            detail: error.to_string(),
        })?;
    Ok(())
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
fn spawn_detached(app: InstalledApp, command: LaunchCommand) -> Result<(), String> {
    spawn_launch(app.package_id, app.display_name, command)
}

/// Run `command` on a worker thread under `(package_id, display_name)`,
/// reporting through [`EVENTS`]. Shared by both launch kinds — a `.qpkg`
/// via `tilo-launcher run` and a host app via `run-host` — since they
/// differ only in what the launcher does, not in how it's supervised.
#[allow(
    clippy::too_many_lines,
    reason = "This is the complete launch-thread lifecycle, whose ordered cleanup and reporting share the moved launch identity."
)]
/// # Errors
///
/// Returns the operating system's thread-spawn error when the launcher
/// cannot create the worker responsible for supervising `command`.
#[cfg(any(test, target_os = "linux"))]
pub fn spawn_launch(
    package_id: String,
    display_name: String,
    command: LaunchCommand,
) -> Result<(), String> {
    #[cfg(all(test, not(target_os = "linux")))]
    if !TEST_EXECUTION_PROGRAM.with(|slot| slot.borrow().as_ref() == Some(&command.program)) {
        return Err("Linux package execution provider is unavailable".into());
    }
    let app = LaunchIdentity {
        package_id,
        display_name,
        managed_agent_id: command.managed_agent_id.clone(),
        managed_workspace_id: command
            .service_identity
            .as_ref()
            .map(|identity| identity.workspace_id.clone()),
    };
    let builder = std::thread::Builder::new().name("tilo-app-launch".to_string());
    // Kept for the spawn-failure report below since the closure moves `app`.
    let identity = (
        app.package_id.clone(),
        app.display_name.clone(),
        app.managed_agent_id.clone(),
        app.managed_workspace_id.clone(),
    );
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

        let mut launch_gate = match install_launch_registration_gate(&mut cmd) {
            Ok(gate) => gate,
            Err(error) => {
                push_event(LaunchEvent {
                    package_id: app.package_id.clone(),
                    display_name: app.display_name.clone(),
                    managed_agent_id: app.managed_agent_id.clone(),
                    managed_workspace_id: app.managed_workspace_id.clone(),
                    kind: LaunchEventKind::SpawnFailed {
                        detail: error.to_string(),
                    },
                });
                return;
            }
        };

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                push_event(LaunchEvent {
                    package_id: app.package_id.clone(),
                    display_name: app.display_name.clone(),
                    managed_agent_id: app.managed_agent_id.clone(),
                    managed_workspace_id: app.managed_workspace_id.clone(),
                    kind: LaunchEventKind::SpawnFailed {
                        detail: e.to_string(),
                    },
                });
                return;
            }
        };
        if let Some(identity) = command.service_identity.clone() {
            register_trusted_service_launch(child.id(), identity);
        }
        let workspace_id = command
            .service_identity
            .as_ref()
            .map(|identity| identity.workspace_id.as_str())
            .or(command.portal_workspace_id.as_deref());
        register_launched_process(
            child.id(),
            &app.package_id,
            workspace_id,
            command.service_identity.is_some(),
        );
        // The child cannot exec the launcher (and therefore cannot reach the
        // public Wayland socket) until both registries above are complete.
        let _ = launch_gate.write_all(&[1]);
        push_event(LaunchEvent {
            package_id: app.package_id.clone(),
            display_name: app.display_name.clone(),
            managed_agent_id: app.managed_agent_id.clone(),
            managed_workspace_id: app.managed_workspace_id.clone(),
            kind: LaunchEventKind::Started { pid: child.id() },
        });

        // Drain both pipes concurrently. A GUI normally keeps both streams
        // open for its entire lifetime, so reading either one to EOF before
        // `wait` would turn the launch supervisor into a second lifetime
        // wait. Worse, a child filling the other pipe would deadlock both
        // processes. The joins below happen only after the child exits and
        // closes its inherited descriptors.
        if let Some(stdout) = child.stdout.take() {
            let tag = app.package_id.clone();
            let _ = std::thread::Builder::new()
                .name("tilo-app-stdout".to_string())
                .spawn(move || {
                    relay_capped_lines(stdout, |line| {
                        eprintln!("[app {tag} stdout] {line}");
                    });
                });
        }

        // stderr is both echoed and kept (last N lines) for the failure
        // notification.
        let stderr_tag = app.package_id.clone();
        let stderr_drain = child.stderr.take().and_then(|stderr| {
            std::thread::Builder::new()
                .name("tilo-app-stderr".to_string())
                .spawn(move || {
                    let mut tail = VecDeque::with_capacity(STDERR_TAIL_LINES);
                    relay_capped_lines(stderr, |line| {
                        eprintln!("[app {stderr_tag} stderr] {line}");
                        if tail.len() == STDERR_TAIL_LINES {
                            tail.pop_front();
                        }
                        tail.push_back(line);
                    });
                    tail
                })
                .ok()
        });

        let launcher_pid = child.id();
        let status = child.wait();
        let tail = stderr_drain
            .and_then(|drain| drain.join().ok())
            .unwrap_or_default();
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
            managed_agent_id: app.managed_agent_id.clone(),
            managed_workspace_id: app.managed_workspace_id.clone(),
            kind,
        });
    });
    match spawn_result {
        Ok(_) => Ok(()),
        Err(e) => {
            let detail = format!("could not start launch thread: {e}");
            push_event(LaunchEvent {
                package_id: identity.0,
                display_name: identity.1,
                managed_agent_id: identity.2,
                managed_workspace_id: identity.3,
                kind: LaunchEventKind::SpawnFailed {
                    detail: detail.clone(),
                },
            });
            Err(detail)
        }
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
/// component (`dev.tilo.hello` ↔ `hello`). An unrelated app-id keeps its own
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
/// Multiple launches of the same package remain FIFO: each arriving surface
/// consumes the destination chosen by its own user action.
pub fn set_pending_placement(package_id: &str, placement: crate::shell::tab_binding::Placement) {
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
    placement: crate::shell::tab_binding::Placement,
) {
    let mut pending = lock(&PENDING_PLACEMENT);
    pending.push(PendingPlacement {
        key: key.to_string(),
        identities: identities.to_vec(),
        placement,
        continuity_instance: None,
        claimed_by: None,
        retire_after: None,
    });
}

/// The first surface to ask claims the note and gets the same answer on
/// every later ask; a different surface retires it and gets `None`. `None`
/// also covers: not launched by us, or nothing special asked.
pub fn placement_for_toplevel(
    app_id: &str,
    surface_id: u64,
) -> Option<crate::shell::tab_binding::Placement> {
    placement_and_instance_for_toplevel(app_id, surface_id).map(|(placement, _)| placement)
}

/// The claimed placement plus the launch instance that must be bound to the
/// durable tab adopted for this toplevel.
pub fn placement_and_instance_for_toplevel(
    app_id: &str,
    surface_id: u64,
) -> Option<(crate::shell::tab_binding::Placement, Option<u64>)> {
    let mut pending = lock(&PENDING_PLACEMENT);
    loop {
        let idx = pending.iter().position(|p| p.matches(app_id))?;
        if pending
            .get(idx)
            .and_then(|note| note.retire_after)
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            pending.remove(idx);
            continue;
        }
        let claimed_by = pending.get(idx)?.claimed_by;
        let placement = pending.get(idx)?.placement;
        let instance = pending.get(idx)?.continuity_instance;
        match claimed_by {
            None => {
                if let Some(entry) = pending.get_mut(idx) {
                    entry.claimed_by = Some(surface_id);
                    // Both compositor trees ask for the same launch note,
                    // so keep a claimed answer briefly rather than consume
                    // it on the first lookup. Bound that duplication window
                    // as well: an app that never causes a second lookup must
                    // not leave a permanent note capable of steering a much
                    // later surface with the same identity.
                    entry.retire_after =
                        Some(pending_placement_deadline(PENDING_PLACEMENT_CLAIM_GRACE));
                }
                return Some((placement, instance));
            }
            Some(claimed) if claimed == surface_id => return Some((placement, instance)),
            // This note belongs to an earlier window. Retire it and let this
            // surface claim the next queued launch for the same app, if any.
            Some(_) => {
                pending.remove(idx);
            }
        }
    }
}

/// Attach the launch instance to the existing pending placement note. This is
/// called after the real launcher command is assembled, never by pure command
/// construction, so tests and previews cannot invent a resumable instance.
fn attach_continuity_instance(package_id: &str, instance: u64) {
    let mut pending = lock(&PENDING_PLACEMENT);
    if let Some(note) = pending
        .iter_mut()
        .rev()
        .find(|note| note.key == package_id && note.continuity_instance.is_none())
    {
        note.continuity_instance = Some(instance);
    } else {
        pending.push(PendingPlacement {
            key: package_id.to_string(),
            identities: identities_for_package(package_id),
            placement: crate::shell::tab_binding::Placement::NewGroup,
            continuity_instance: Some(instance),
            claimed_by: None,
            retire_after: None,
        });
    }
}

/// Carry a previously validated continuation projection into the next launch
/// that binds `tab` in `room`. The payload is not acted on by the shell.
pub fn stage_continuation_resume(
    room: RoomId,
    tab: u64,
    schema: u16,
    bytes: Vec<u8>,
    record_instance: u64,
) {
    lock(&PENDING_CONTINUATION_RESUME)
        .insert((room, tab), (Some((schema, bytes)), record_instance));
}

/// Carry only the source CRDT record identity when an app has no transient
/// resume projection.  Its document still synchronizes through the normal
/// CRDT route once the process starts.
pub fn stage_continuation_record(room: RoomId, tab: u64, record_instance: u64) {
    lock(&PENDING_CONTINUATION_RESUME).insert((room, tab), (None, record_instance));
}

fn take_staged_resume(
    placement: crate::shell::tab_binding::Placement,
    room: RoomId,
) -> Option<StagedContinuationResume> {
    let crate::shell::tab_binding::Placement::IntoTab(tab) = placement else {
        return None;
    };
    lock(&PENDING_CONTINUATION_RESUME).remove(&(room, tab))
}

pub(crate) fn take_staged_resume_for_package(
    package_id: &str,
    room: RoomId,
) -> Option<StagedContinuationResume> {
    let placement = lock(&PENDING_PLACEMENT)
        .iter()
        .rev()
        .find(|note| note.key == package_id)
        .map(|note| note.placement)?;
    take_staged_resume(placement, room)
}

/// Drop the newest unclaimed placement note for `package_id`, if any —
/// undoing the most recently staged launch that failed before a surface was
/// created without destroying older concurrent launches' destinations.
pub fn clear_pending_placement(package_id: &str) {
    let mut pending = lock(&PENDING_PLACEMENT);
    if let Some(index) = pending
        .iter()
        .rposition(|p| p.key == package_id && p.claimed_by.is_none())
    {
        pending.remove(index);
    }
}

/// Forget the one claimed placement belonging to a launch that reported a
/// terminal failure after its toplevel was already adopted.
///
/// A claim normally lives briefly so the rendered and protocol trees can
/// agree on a just-mapped client.  A failed launcher is different: retaining
/// that claim leaves the unpainted foreign tab with no future producer.  We
/// only take it when it is the sole in-flight launch for the key; launch
/// events have no per-process token, so guessing during concurrent launches
/// could close the other launch's real window.
pub(crate) fn take_sole_claimed_surface_for_failed_launch(package_id: &str) -> Option<u64> {
    let mut pending = lock(&PENDING_PLACEMENT);
    let matching: Vec<usize> = pending
        .iter()
        .enumerate()
        .filter(|(_, note)| note.key == package_id)
        .map(|(index, _)| index)
        .collect();
    if matching.len() != 1 {
        return None;
    }
    let index = matching[0];
    let surface_id = pending.get(index)?.claimed_by?;
    pending.remove(index);
    Some(surface_id)
}

/// Allow a cleanly-exited launcher parent a bounded handoff window for a
/// daemonised GUI child to announce its toplevel. Failure paths still call
/// [`clear_pending_placement`] immediately.
pub fn retire_pending_placement_after_grace(package_id: &str) {
    retire_pending_placement_at(
        package_id,
        pending_placement_deadline(PENDING_PLACEMENT_EXIT_GRACE),
    );
}

/// Produce a bounded placement deadline without panicking if an unusual
/// platform clock makes the requested duration unrepresentable. Expiring the
/// transient claim immediately is safer than retaining it indefinitely.
fn pending_placement_deadline(grace: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(grace).unwrap_or(now)
}

fn retire_pending_placement_at(package_id: &str, deadline: Instant) {
    let mut pending = lock(&PENDING_PLACEMENT);
    if let Some(note) = pending
        .iter_mut()
        .rev()
        .find(|note| note.key == package_id && note.claimed_by.is_none())
    {
        note.retire_after = Some(deadline);
    }
}

/// Retire windowless clean launches without waiting for another application
/// with the same identity to map.
pub fn expire_pending_placements() {
    expire_pending_placements_at(Instant::now());
}

fn expire_pending_placements_at(now: Instant) {
    lock(&PENDING_PLACEMENT).retain(|note| note.retire_after.is_none_or(|deadline| now < deadline));
}

/// Long enough for a daemonising desktop entry to hand off to its GUI child,
/// while keeping a genuinely windowless launch from steering a later app.
const PENDING_PLACEMENT_EXIT_GRACE: Duration = Duration::from_secs(10);

/// Enough for the rendered and protocol trees to observe the same newly
/// mapped surface, but finite so a claimed note cannot leak forever.
const PENDING_PLACEMENT_CLAIM_GRACE: Duration = Duration::from_secs(10);

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

    fn launch_event(kind: LaunchEventKind) -> LaunchEvent {
        LaunchEvent {
            package_id: "org.example.Broken".into(),
            display_name: "Broken App".into(),
            managed_agent_id: None,
            managed_workspace_id: None,
            kind,
        }
    }

    #[test]
    fn failed_launch_banner_never_contains_raw_stderr() {
        let raw = "error: could not connect\nWAYLAND_DISPLAY=wayland-1\nbacktrace: 0xdeadbeef";
        let event = launch_event(LaunchEventKind::Failed {
            code: Some(1),
            stderr_tail: raw.into(),
        });
        let notice = event.failure_notice().expect("failure has a notice");
        assert_eq!(notice.title, "Couldn't open Broken App");
        assert_eq!(
            notice.body,
            "The app stopped before opening. Open Applications and try again."
        );
        assert!(!notice.body.contains(raw));
        assert!(!notice.body.contains('\n'));
        assert_eq!(LAUNCH_FAILURE_NOTICE_LIFETIME, Duration::from_secs(8));
    }

    #[test]
    fn spawn_failure_and_non_failure_events_have_sensible_banner_copy() {
        let event = launch_event(LaunchEventKind::SpawnFailed {
            detail: "No such file or directory (os error 2)".into(),
        });
        let notice = event.failure_notice().expect("failure has a notice");
        assert_eq!(notice.title, "Couldn't open Broken App");
        assert_eq!(
            notice.body,
            "The app launcher is unavailable. Open Applications and try again."
        );
        assert!(
            launch_event(LaunchEventKind::Started { pid: 42 })
                .failure_notice()
                .is_none()
        );
        assert!(
            launch_event(LaunchEventKind::Exited)
                .failure_notice()
                .is_none()
        );
    }

    #[test]
    fn launch_failure_title_normalises_and_bounds_untrusted_app_names() {
        let mut event = launch_event(LaunchEventKind::Failed {
            code: None,
            stderr_tail: String::new(),
        });
        event.display_name = "  An app\nwith a deliberately extremely long display name  ".into();
        let notice = event.failure_notice().unwrap();
        assert_eq!(
            notice.title,
            "Couldn't open An app with a deliberately extre…"
        );
        assert!(!notice.title.contains('\n'));
        assert_eq!(compact_notice_name(" \t "), "application");
    }

    #[test]
    fn preflight_notification_copy_excludes_paths_and_backend_detail() {
        let missing = LaunchError::LauncherBinaryMissing {
            searched: vec![
                PathBuf::from("/a/private/developer/build/tilo-launcher"),
                PathBuf::from("/another/very/long/path/tilo-launcher"),
            ],
        };
        let registry = LaunchError::RegistryUnreadable {
            detail: "permission denied while reading /var/lib/tilo/apps.json".into(),
        };
        assert_eq!(
            missing.notification_message(),
            "The app launcher is unavailable. Try again."
        );
        assert_eq!(
            registry.notification_message(),
            "The app list could not be read. Try again."
        );
        assert!(!missing.notification_message().contains('/'));
        assert!(!registry.notification_message().contains('/'));
        assert!(missing.user_message().contains("/a/private"));
        assert!(registry.user_message().contains("/var/lib"));
    }

    #[test]
    fn restored_launch_keeps_the_source_crdt_record_identity() {
        let room = RoomId::new();
        clear_pending_placements();
        lock(&PENDING_CONTINUATION_RESUME).clear();
        set_pending_placement(
            "dev.tilo.browser",
            crate::shell::tab_binding::Placement::IntoTab(77),
        );
        stage_continuation_record(room, 77, 0xdead_beef);
        assert_eq!(
            take_staged_resume_for_package("dev.tilo.browser", room),
            Some((None, 0xdead_beef))
        );
        clear_pending_placements();
    }

    #[test]
    #[serial_test::serial(pending_placement)]
    fn same_app_launch_placements_are_claimed_fifo() {
        clear_pending_placements();
        set_pending_placement(
            "dev.tilo.same-app",
            crate::shell::tab_binding::Placement::IntoTab(11),
        );
        set_pending_placement(
            "dev.tilo.same-app",
            crate::shell::tab_binding::Placement::IntoTab(22),
        );

        assert_eq!(
            placement_for_toplevel("same-app", 101),
            Some(crate::shell::tab_binding::Placement::IntoTab(11))
        );
        assert_eq!(
            placement_for_toplevel("same-app", 202),
            Some(crate::shell::tab_binding::Placement::IntoTab(22))
        );
        clear_pending_placements();
    }

    #[test]
    #[serial_test::serial(pending_placement)]
    fn failed_newest_same_app_launch_keeps_older_destination() {
        clear_pending_placements();
        set_pending_placement(
            "dev.tilo.same-app",
            crate::shell::tab_binding::Placement::IntoTab(11),
        );
        set_pending_placement(
            "dev.tilo.same-app",
            crate::shell::tab_binding::Placement::IntoTab(22),
        );
        clear_pending_placement("dev.tilo.same-app");

        assert_eq!(
            placement_for_toplevel("same-app", 101),
            Some(crate::shell::tab_binding::Placement::IntoTab(11)),
            "clearing the failed second launch must not erase the first launch's placement"
        );
        assert_eq!(placement_for_toplevel("same-app", 202), None);
        clear_pending_placements();
    }

    #[test]
    #[serial_test::serial(pending_placement)]
    fn clean_parent_exit_keeps_delayed_window_placement_until_deadline() {
        clear_pending_placements();
        let package = "dev.tilo.daemonised";
        let wanted = crate::shell::tab_binding::Placement::IntoTab(44);
        set_pending_placement(package, wanted);

        retire_pending_placement_at(package, Instant::now() + Duration::from_secs(1));
        assert_eq!(
            placement_for_toplevel("daemonised", 701),
            Some(wanted),
            "a GUI child mapping after its launcher parent exits keeps the user's destination"
        );
        clear_pending_placements();
    }

    #[test]
    #[serial_test::serial(pending_placement)]
    fn windowless_clean_launch_expires_without_sleeping() {
        clear_pending_placements();
        let package = "dev.tilo.windowless";
        set_pending_placement(package, crate::shell::tab_binding::Placement::IntoTab(55));

        let now = Instant::now();
        retire_pending_placement_at(package, now);
        expire_pending_placements_at(now);
        assert_eq!(placement_for_toplevel("windowless", 702), None);
        clear_pending_placements();
    }

    #[test]
    #[serial_test::serial(pending_placement)]
    fn claimed_placement_expires_without_a_second_surface() {
        clear_pending_placements();
        let package = "dev.tilo.claimed-once";
        let wanted = crate::shell::tab_binding::Placement::IntoTab(56);
        set_pending_placement(package, wanted);

        assert_eq!(placement_for_toplevel("claimed-once", 703), Some(wanted));
        let deadline = lock(&PENDING_PLACEMENT)[0]
            .retire_after
            .expect("claim receives a bounded retirement deadline");
        expire_pending_placements_at(deadline);
        assert_eq!(placement_for_toplevel("claimed-once", 703), None);
        clear_pending_placements();
    }

    #[test]
    fn untrusted_app_log_lines_are_memory_bounded_and_stream_label_safe() {
        let input = vec![b'x'; APP_LOG_LINE_BYTES.saturating_mul(3)];
        let mut lines = Vec::new();
        relay_capped_lines(std::io::Cursor::new(input), |line| lines.push(line));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].ends_with("… [truncated]"));
        assert!(
            lines[0].len() <= APP_LOG_LINE_BYTES.saturating_add(20),
            "capped line unexpectedly retained {} bytes",
            lines[0].len()
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial(launch_events)]
    fn live_child_with_open_stderr_does_not_block_exit_supervision() {
        use std::os::unix::fs::PermissionsExt;

        const PACKAGE_ID: &str = "dev.tilo.live-gui";
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("live-gui");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf 'starting without closing stderr\\n' >&2\nsleep 0.15\nexit 0\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();

        let _ = drain_events();
        spawn_launch(
            PACKAGE_ID.into(),
            "Live GUI".into(),
            LaunchCommand {
                program: script,
                args: Vec::new(),
                env: Vec::new(),
                service_identity: None,
                portal_workspace_id: None,
                managed_agent_id: None,
            },
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_started = false;
        let mut saw_exit = false;
        while Instant::now() < deadline && !saw_exit {
            for event in drain_events() {
                if event.package_id != PACKAGE_ID {
                    continue;
                }
                match event.kind {
                    LaunchEventKind::Started { .. } => saw_started = true,
                    LaunchEventKind::Exited => saw_exit = true,
                    other => panic!("unexpected launch event: {other:?}"),
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(saw_started, "the child was reported started");
        assert!(
            saw_exit,
            "the supervisor must wait for a live GUI child without synchronously blocking on its open stderr pipe"
        );
    }
    use std::io::Write;

    #[test]
    fn dev_build_lets_the_environment_name_the_launcher() {
        let got = launcher_bin_candidates_from(
            true,
            Some(std::ffi::OsString::from("/tmp/my/tilo-launcher")),
            Some(Path::new("/home/dev/target/debug/shell")),
        );
        assert_eq!(got[0], PathBuf::from("/tmp/my/tilo-launcher"));
        assert_eq!(
            got[1],
            PathBuf::from("/home/dev/target/debug/tilo-launcher")
        );
    }

    #[test]
    fn shipped_build_ignores_a_launcher_named_by_the_environment() {
        // A shipped session must not take the sandbox-building binary's
        // identity from its environment.
        let got = launcher_bin_candidates_from(
            false,
            Some(std::ffi::OsString::from("/tmp/evil/tilo-launcher")),
            Some(Path::new("/usr/bin/tilo-shell")),
        );
        assert!(
            !got.iter().any(|p| p.starts_with("/tmp")),
            "release build accepted an environment-named launcher: {got:?}"
        );
        assert_eq!(
            got,
            vec![
                PathBuf::from("/usr/bin/tilo-launcher"),
                PathBuf::from("/usr/libexec/tilo-launcher"),
                PathBuf::from("/usr/bin/tilo-launcher"),
            ],
        );
    }

    #[test]
    fn the_install_paths_are_always_searched() {
        let got = launcher_bin_candidates_from(false, None, None);
        assert_eq!(
            got,
            vec![
                PathBuf::from("/usr/libexec/tilo-launcher"),
                PathBuf::from("/usr/bin/tilo-launcher"),
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
        assert_eq!(display_name_for_package_id("dev.tilo.hello"), "Hello");
        assert_eq!(
            display_name_for_package_id("dev.tilo.text-editor"),
            "Text Editor"
        );
        assert_eq!(display_name_for_package_id("Notes"), "Notes");
        // A degenerate id must not produce an empty (unclickable) label.
        assert!(!display_name_for_package_id("dev.tilo.").is_empty());
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn enumeration_reads_the_registry_and_flags_gui_apps() {
        let tmp = tempfile::tempdir().unwrap();
        install_fixture(tmp.path(), "dev.tilo.hello", "gui");
        install_fixture(tmp.path(), "dev.tilo.notes-cli", "cli");
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("TILO_APPS_DATA_DIR", tmp.path()) };
        invalidate_app_cache();
        let apps = read_installed_apps().expect("registry readable");
        let offered = gui_apps(); // what the palette actually offers
        unsafe { std::env::remove_var("TILO_APPS_DATA_DIR") };
        invalidate_app_cache();

        assert_eq!(apps.len(), 2, "both packages enumerate: {apps:?}");
        let hello = apps
            .iter()
            .find(|a| a.package_id == "dev.tilo.hello")
            .expect("hello present");
        assert!(hello.has_gui, "gui entrypoint must be flagged as GUI");
        assert_eq!(hello.display_name, "Hello");
        assert_eq!(hello.version, "1.2.3");
        let cli = apps
            .iter()
            .find(|a| a.package_id == "dev.tilo.notes-cli")
            .expect("cli package present");
        assert!(!cli.has_gui, "a cli-only package is not a GUI app");

        // Enumerating it is fine; offering it to launch is not — a
        // headless package has no window to show.
        let offered_ids: Vec<&str> = offered.iter().map(|a| a.package_id.as_str()).collect();
        assert_eq!(
            offered_ids,
            vec!["dev.tilo.hello"],
            "only the GUI package is launchable from the palette",
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn installed_continuity_lookup_reads_signed_manifest_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let payload = install_fixture(tmp.path(), "dev.tilo.browser", "gui");
        let manifest_path = payload.join("manifest.toml");
        let mut manifest = std::fs::OpenOptions::new()
            .append(true)
            .open(&manifest_path)
            .unwrap();
        writeln!(
            manifest,
            "\n[continuity]\nrequired = [\"presentation\", \"semantic\"]\nsemantic-schema = \"web-location-v1\""
        )
        .unwrap();

        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("TILO_APPS_DATA_DIR", tmp.path()) };
        let required = installed_continuity_contract("dev.tilo.browser");
        let legacy = installed_continuity_contract("dev.tilo.not-installed");
        unsafe { std::env::remove_var("TILO_APPS_DATA_DIR") };

        assert_eq!(
            required,
            InstalledContinuityRequirements::Required(launcher::manifest::ContinuityContract {
                required: vec![
                    launcher::manifest::ContinuityFacet::Presentation,
                    launcher::manifest::ContinuityFacet::Semantic,
                ],
                semantic_schema: Some(launcher::manifest::ContinuitySemanticSchema::WebLocationV1,),
            })
        );
        assert_eq!(legacy, InstalledContinuityRequirements::NotInstalled);
    }

    #[test]
    fn launch_command_carries_manifest_payload_and_display() {
        let app = InstalledApp {
            package_id: "dev.tilo.hello".into(),
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
            Path::new("/usr/bin/tilo-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            None,
            None,
            Some(Path::new("/dev/dri/renderD128")),
            None,
        );
        assert_eq!(cmd.program, Path::new("/usr/bin/tilo-launcher"));
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

    #[test]
    #[serial_test::serial(process_env)]
    fn debug_launch_forwards_only_the_matched_developer_app_overlay() {
        let names = [
            "TILO_APPS_SYSTEM_DIR",
            "TILO_APPS_SYSTEM_PAYLOAD_DIR",
            "TILO_RUNTIME_BUNDLE_DIR",
            "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE",
        ];
        let previous: Vec<_> = names.iter().map(|name| std::env::var_os(name)).collect();
        // SAFETY: guarded by the process_env serial group.
        unsafe {
            for (index, name) in names.iter().enumerate() {
                std::env::set_var(name, format!("/developer/overlay/{index}"));
            }
        }
        let command = build_launch_command(
            &test_app("dev.tilo.settings"),
            Path::new("/usr/bin/tilo-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            None,
            None,
            None,
            None,
        );
        if crate::compositor::dev_env::DEV_ENV_OVERRIDES {
            for (index, name) in names.iter().enumerate() {
                assert!(
                    command
                        .env
                        .contains(&((*name).to_string(), format!("/developer/overlay/{index}")))
                );
            }
        } else {
            assert!(
                command
                    .env
                    .iter()
                    .all(|(name, _)| !names.contains(&name.as_str()))
            );
        }
        unsafe {
            for (name, value) in names.iter().zip(previous) {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn workspace_agent_command_is_headless_fixed_package_service_argv() {
        let app = InstalledApp {
            package_id: WORKSPACE_AGENT_PACKAGE_ID.into(),
            display_name: "Agent".into(),
            category: None,
            version: "0.1.0".into(),
            manifest_path: PathBuf::from("/apps/agent/manifest.toml"),
            payload_root: PathBuf::from("/apps/agent"),
            has_gui: false,
            opens_extensions: Vec::new(),
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
        };
        let room = RoomId::new();
        let command = build_workspace_agent_command(
            &app,
            Path::new("/usr/bin/tilo-launcher"),
            room,
            Some(Path::new("/run/user/1000/tilo/identity.json")),
            "studio-agent",
        );

        assert!(!command.args.iter().any(|arg| arg == "--wayland-socket"));
        assert!(!command.args.iter().any(|arg| arg == "--capabilities"));
        assert_eq!(command.managed_agent_id.as_deref(), Some("studio-agent"));
        let separator = command
            .args
            .iter()
            .position(|arg| arg == "--")
            .expect("service argv has an explicit separator");
        assert_eq!(
            command
                .args
                .get(separator.wrapping_sub(1))
                .map(String::as_str),
            Some("--service")
        );
        assert_eq!(
            command.args.get(separator.saturating_add(1)..),
            Some(
                [
                    "--workspace".to_string(),
                    room.to_string(),
                    "--app".to_string(),
                    WORKSPACE_AGENT_PACKAGE_ID.to_string(),
                    "--name".to_string(),
                    "studio-agent".to_string(),
                    "--task-runs".to_string(),
                ]
                .as_slice()
            )
        );
        let room_string = room.to_string();
        assert_eq!(
            command
                .service_identity
                .as_ref()
                .map(|identity| (identity.app_id.as_str(), identity.workspace_id.as_str())),
            Some((WORKSPACE_AGENT_PACKAGE_ID, room_string.as_str()))
        );
    }

    /// No render node resolved means no `--render-node` flag at all — an
    /// empty or invented value would read to both loaders as "no drivers".
    #[test]
    fn launch_command_omits_the_render_node_when_none_resolved() {
        let app = InstalledApp {
            package_id: "dev.tilo.hello".into(),
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
            Path::new("/usr/bin/tilo-launcher"),
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
        unsafe { std::env::set_var("TILO_APPS_DATA_DIR", tmp.path()) };
        invalidate_app_cache();
        let err = launch("dev.tilo.nope", RoomId::new()).unwrap_err();
        unsafe { std::env::remove_var("TILO_APPS_DATA_DIR") };
        invalidate_app_cache();
        assert_eq!(
            err,
            LaunchError::NotInstalled {
                package_id: "dev.tilo.nope".into()
            }
        );
        let message = err.user_message();
        assert!(
            message.contains("dev.tilo.nope"),
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
        install_fixture(tmp.path(), "dev.tilo.notes-cli", "cli");
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("TILO_APPS_DATA_DIR", tmp.path()) };
        invalidate_app_cache();
        let err = launch("dev.tilo.notes-cli", RoomId::new()).unwrap_err();
        unsafe { std::env::remove_var("TILO_APPS_DATA_DIR") };
        invalidate_app_cache();
        assert_eq!(
            err,
            LaunchError::NotAGuiApp {
                package_id: "dev.tilo.notes-cli".into()
            }
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn launch_refuses_when_no_compositor_socket_is_published() {
        let tmp = tempfile::tempdir().unwrap();
        install_fixture(tmp.path(), "dev.tilo.hello", "gui");
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("TILO_APPS_DATA_DIR", tmp.path()) };
        invalidate_app_cache();
        set_wayland_socket_path(None);
        let err = launch("dev.tilo.hello", RoomId::new()).unwrap_err();
        unsafe { std::env::remove_var("TILO_APPS_DATA_DIR") };
        invalidate_app_cache();
        assert_eq!(err, LaunchError::NoDisplay);
    }

    /// The production `launch` path end to end, with `tilo-launcher`
    /// replaced by a recorder that fails and is quoted back as a
    /// `LaunchEvent`.
    #[test]
    #[serial_test::serial(process_env)]
    #[cfg(target_os = "linux")]
    fn launch_execs_the_launcher_binary_with_the_compositor_socket() {
        let tmp = tempfile::tempdir().unwrap();
        install_fixture(tmp.path(), "dev.tilo.hello", "gui");
        let record = tmp.path().join("argv.txt");
        let fake = tmp.path().join("tilo-launcher");
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
            std::env::set_var("TILO_APPS_DATA_DIR", tmp.path());
            std::env::set_var("TILO_LAUNCHER_BIN", &fake);
        }
        invalidate_app_cache();
        set_wayland_socket_path(Some(PathBuf::from("/run/user/1000/wayland-7")));
        let _ = drain_events();

        let app = launch("dev.tilo.hello", RoomId::new()).expect("launch accepted");
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
            std::env::remove_var("TILO_APPS_DATA_DIR");
            std::env::remove_var("TILO_LAUNCHER_BIN");
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
                .any(|a| a.ends_with("payloads/dev.tilo.hello/manifest.toml")),
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
            package_id: "dev.tilo.matchfixture".into(),
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
            display_name_for_app_id("dev.tilo.matchfixture").as_deref(),
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
            &test_app("dev.tilo.books"),
            Path::new("/usr/bin/tilo-launcher"),
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
                .any(|w| w[0] == "--app-id" && w[1] == "dev.tilo.books"),
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
            &test_app("dev.tilo.books"),
            Path::new("/usr/bin/tilo-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            Some(Path::new(
                "/run/user/1000/tilo/capabilities/dev.tilo.books.json",
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
            "/run/user/1000/tilo/capabilities/dev.tilo.books.json"
        );
    }

    #[test]
    fn launch_command_names_the_workspace_selected_identity() {
        let cmd = build_launch_command(
            &test_app("dev.tilo.books"),
            Path::new("/usr/bin/tilo-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            RoomId::new(),
            None,
            Some(Path::new("/home/alice/.config/tilo/identity/alice.json")),
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
            "/home/alice/.config/tilo/identity/alice.json"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn published_capabilities_are_filtered_to_the_launching_app() {
        set_workspace_capabilities(vec![
            fs_grant("dev.tilo.books", "/var/workspace/books", Verb::Read),
            fs_grant("dev.tilo.other", "/var/workspace/secrets", Verb::Write),
            // A person's share is a capability in the same list but not
            // an app grant, and must not travel.
            CapOverride::grant(
                Subject::person("alice"),
                Scope::FsPath("/var/workspace/private".into()),
                Verb::Write,
            ),
        ]);
        let mine = capabilities_for_app("dev.tilo.books");
        clear_workspace_capabilities();

        assert_eq!(mine.len(), 1, "only this app's grant travels: {mine:?}");
        assert_eq!(mine[0].scope, Scope::FsPath("/var/workspace/books".into()));
        assert_eq!(mine[0].subject, Subject::app("dev.tilo.books"));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn nothing_published_means_nothing_granted() {
        clear_workspace_capabilities();
        assert!(
            capabilities_for_app("dev.tilo.books").is_empty(),
            "with no workspace published, an app is granted nothing"
        );
    }

    /// The whole handoff end to end: publish a capability, write the
    /// file a launch would write, then read it back with `tilo-launcher`'s
    /// **own** loader and planner and check the bind that comes out.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_granted_path_survives_the_handoff_and_becomes_a_read_only_bind() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmp.path()) };
        set_workspace_capabilities(vec![
            fs_grant("dev.tilo.books", "/var/workspace/books", Verb::Read),
            fs_grant("dev.tilo.other", "/var/workspace/secrets", Verb::Write),
        ]);

        let caps = capabilities_for_app("dev.tilo.books");
        let path = write_capabilities_file("dev.tilo.books", &caps)
            .expect("write capability file")
            .expect("a granted app gets a file");

        // readable only by the user: this file says what one app may reach
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "capability file must not be world-readable");

        let records = launcher::capabilities::load_capability_file(&path);
        let plan = launcher::capabilities::CapabilityPlan::for_app(
            "dev.tilo.books",
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
            "dev.tilo.books",
            "/var/workspace/books",
            Verb::Read,
        )];
        let path = write_capabilities_file("dev.tilo.books", &granted)
            .unwrap()
            .expect("file written while granted");
        assert!(path.exists());

        let after_revoke = write_capabilities_file("dev.tilo.books", &[]).unwrap();
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
    #[cfg(target_os = "linux")]
    fn dbus_proxy_descendant_resolves_only_while_launcher_identity_is_live() {
        let parent = std::process::id();
        register_launched_process(parent, "dev.tilo.mail", Some("room-mail"), true);
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 2"])
            .spawn()
            .expect("spawn descendant");
        assert_eq!(
            verified_package_for_descendant(child.id()).as_deref(),
            Some("dev.tilo.mail")
        );
        assert_eq!(
            verified_launch_for_descendant(child.id()),
            Some(VerifiedLaunchIdentity {
                package_id: "dev.tilo.mail".into(),
                workspace_id: Some("room-mail".into()),
                confined: true,
            })
        );
        unregister_launched_process(parent);
        assert_eq!(verified_package_for_descendant(child.id()), None);
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    #[serial_test::serial(process_env)]
    #[cfg(target_os = "linux")]
    fn an_unconfined_launch_is_portal_attributed_without_service_authority() {
        let _ = drain_events();
        let command = LaunchCommand {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), "sleep 1".into()],
            env: Vec::new(),
            service_identity: None,
            portal_workspace_id: Some("host-room".into()),
            managed_agent_id: None,
        };
        spawn_launch("org.example.Flatpak".into(), "Example".into(), command)
            .expect("launch worker");

        let deadline = Instant::now() + Duration::from_secs(2);
        let launcher_pid = loop {
            let started = drain_events().into_iter().find_map(|event| {
                (event.package_id == "org.example.Flatpak")
                    .then_some(event.kind)
                    .and_then(|kind| match kind {
                        LaunchEventKind::Started { pid } => Some(pid),
                        _ => None,
                    })
            });
            if let Some(pid) = started {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "launch never became attributable"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            verified_workspaces_for_app("org.example.Flatpak"),
            ["host-room".to_string()]
        );
        assert!(
            consume_trusted_service_launch_for_pid(
                launcher_pid,
                "org.example.Flatpak",
                "host-room"
            )
            .is_none(),
            "portal attribution must not mint service authority"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn stale_start_time_is_never_accepted_as_pid_identity() {
        let pid = std::process::id();
        LAUNCHED_PROCESSES.lock().unwrap().insert(
            pid,
            LaunchedProcess {
                package_id: "dev.tilo.spoof".into(),
                workspace_id: Some("room-spoof".into()),
                start_time: u64::MAX,
                confined: true,
            },
        );
        assert_eq!(verified_package_for_descendant(pid), None);
        unregister_launched_process(pid);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn compatibility_service_authority_is_exact_and_consumed_once() {
        let pid = std::process::id();
        lock(&TRUSTED_SERVICE_LAUNCHES).clear();
        register_trusted_service_launch(
            pid,
            ServiceLaunchIdentity {
                app_id: "dev.tilo.windows".into(),
                workspace_id: "room-1".into(),
                allowed_compat_entry_ids: vec!["windows-reader".into()],
                compat_entry_generation: 7,
            },
        );
        let (_, start_time) = proc_stat(pid).expect("current process stat");
        assert_eq!(
            consume_trusted_service_launch(pid, start_time, "dev.tilo.windows", "wrong-room"),
            None,
            "a mismatch must deny without transferring the record"
        );
        assert_eq!(
            consume_trusted_service_launch(pid, start_time, "dev.tilo.windows", "room-1"),
            Some(vec!["windows-reader".into()])
        );
        assert_eq!(
            consume_trusted_service_launch(pid, start_time, "dev.tilo.windows", "room-1"),
            None,
            "the authority record is single-use"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn direct_launcher_pid_has_no_service_authority_to_redeem() {
        let pid = std::process::id();
        lock(&TRUSTED_SERVICE_LAUNCHES).clear();
        assert_eq!(
            attest_trusted_service_launch_for_pid(pid, "dev.tilo.agent", "room-1"),
            None,
            "a direct tilo-launcher invocation must not mint the shell's one-shot authority"
        );
    }

    #[test]
    fn managed_entry_registry_generation_changes_only_with_its_snapshot() {
        let first = set_windows_compat_entry_ids(vec!["windows-one".into()]);
        let unchanged = set_windows_compat_entry_ids(vec!["windows-one".into()]);
        let second = set_windows_compat_entry_ids(vec!["windows-two".into()]);
        assert_eq!(unchanged, first);
        assert!(second > first);
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn governance_projection_is_atomic_bounded_and_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_RUNTIME_DIR");
        // SAFETY: guarded by the process_env serial group.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmp.path()) };

        let room = RoomId::new();
        let path = write_trusted_identities_projection(
            room,
            &tilo_user_identity::TrustedIdentityProjectionV2::empty(),
        )
        .expect("stage projection");
        let bytes = std::fs::read(&path).unwrap();
        let decoded: tilo_user_identity::TrustedIdentityProjectionV2 =
            serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            decoded,
            tilo_user_identity::TrustedIdentityProjectionV2::empty()
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(
            std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")),
            "an atomic publish leaves no staging inode"
        );

        // Replacing a complete snapshot keeps the stable path and remains a
        // complete decodable document.
        let again = write_trusted_identities_projection(
            room,
            &tilo_user_identity::TrustedIdentityProjectionV2::empty(),
        )
        .unwrap();
        assert_eq!(again, path);
        serde_json::from_slice::<tilo_user_identity::TrustedIdentityProjectionV2>(
            &std::fs::read(&again).unwrap(),
        )
        .unwrap();

        // A network/governance update publishes immediately; it is not merely
        // cached for the next application launch.
        std::fs::remove_file(&again).unwrap();
        set_trusted_identity_projection(
            room,
            &tilo_user_identity::TrustedIdentityProjectionV2::empty(),
        );
        assert!(again.exists());

        // SAFETY: restore the process-global environment under the same lock.
        unsafe {
            match previous {
                Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn files_launch_receives_only_the_staged_projection_path() {
        let tmp = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_RUNTIME_DIR");
        // SAFETY: guarded by the process_env serial group.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmp.path()) };
        let room = RoomId::new();
        let files = test_app(FILES_PACKAGE_ID);
        let mut command = build_launch_command(
            &files,
            Path::new("/usr/bin/tilo-launcher"),
            Path::new("/run/user/1000/wayland-3"),
            room,
            None,
            None,
            None,
            None,
        );
        attach_trusted_identities_projection(&files, room, &mut command).unwrap();
        let position = command
            .args
            .iter()
            .position(|arg| arg == "--trusted-identities")
            .unwrap();
        let projection = PathBuf::from(&command.args[position + 1]);
        assert_eq!(projection.file_name().unwrap(), TRUSTED_IDENTITIES_FILE);
        assert!(projection.starts_with(tmp.path()));

        let other = test_app("dev.tilo.contacts");
        let mut other_command = command.clone();
        other_command.args.truncate(position);
        attach_trusted_identities_projection(&other, room, &mut other_command).unwrap();
        assert!(
            !other_command
                .args
                .iter()
                .any(|arg| arg == "--trusted-identities"),
            "the member directory is scoped to Files"
        );

        unsafe {
            match previous {
                Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }
}

/// Package processes require the Linux execution authority.
///
/// # Errors
/// Always fails on hosts without that authority, before spawning any worker.
#[cfg(all(not(test), not(target_os = "linux")))]
pub fn spawn_launch(
    _package_id: String,
    _display_name: String,
    _command: LaunchCommand,
) -> Result<(), String> {
    Err(LaunchError::ProviderUnavailable {
        provider: "Linux package execution",
    }
    .to_string())
}
