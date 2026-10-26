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

pub(crate) mod diagnostics;
mod embed_launch;
pub(crate) mod launch_history;
pub(crate) use embed_launch::queue_embed_launch;
mod recorded_use;
#[cfg(target_os = "linux")]
pub(crate) mod retained_home;
#[cfg(any(test, target_os = "linux"))]
pub mod room_hold;
#[cfg(test)]
mod room_hold_tests;
#[cfg(target_os = "linux")]
pub(crate) mod window_doors;

use crate::shell::action_authority;
use crate::shell::state::WorkspaceAuthority as _;
#[cfg(any(test, target_os = "linux"))]
mod agent_provider;
#[cfg(target_os = "linux")]
use agent_provider::workspace_agent_provider;
use std::collections::VecDeque;
#[cfg(any(test, target_os = "linux"))]
use std::process::Command;
#[cfg(any(test, target_os = "linux"))]
use std::process::Stdio;

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
#[cfg(any(test, target_os = "linux"))]
use std::io::{self, Read};
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::shell::host_runtime::{proc_stat, write_runtime_private};
use crate::shell::state::{RoomId, Workspace};
use collab_types::caps::LaunchGrants;
use notify::Watcher as _;
use os_actions::{TOOL_CONTEXT_ENV, ToolIntent};

/// Fallback only: the registry is watched with inotify, so a new install
/// shows up on the next `installed_apps()` without waiting. This bound
/// catches a missed watch (NFS, the watch limit, a swapped `TILO_APPS_DATA_DIR`).
const APP_CACHE_TTL: Duration = Duration::from_secs(30);

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
pub const WORKSPACE_AGENT_PACKAGE_ID: &str = "so.tilo.agent";
/// Signed headless services started beside a workspace agent, because the
/// agent acts only by invoking published actions and these publish actions it
/// needs (looking things up on the web) that no window offers. A service is
/// never launched on demand, so it has to be running before a task needs it.
/// Fixed for the same reason as [`WORKSPACE_AGENT_PACKAGE_ID`]; one that is
/// not installed is skipped and the agent works without its actions.
pub const AGENT_COMPANION_PACKAGE_IDS: &[&str] = &["so.tilo.web-search"];

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
    /// True only for a tapp launch carrying compositor-minted service
    /// identity. Host/Flatpak launches are attributed to a room for placement
    /// and portals but their Wayland app ids remain compatibility-owned.
    confined: bool,
    /// The last native toplevel is closing, but the supervisor has not
    /// observed process exit and completed writer retirement yet.
    retiring: bool,
}

static LAUNCHED_PROCESSES: std::sync::LazyLock<Mutex<HashMap<u32, LaunchedProcess>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static LAUNCH_RETIREMENT_CHANGED: std::sync::Condvar = std::sync::Condvar::new();
/// Packages whose launch is held behind a retiring writer, so a test can
/// see the worker waiting instead of guessing how long it takes to get there.
#[cfg(test)]
static RETIREMENT_WAITS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Bumped whenever a launched process is registered, exits or is
/// unregistered, so observers of the running set (the background portal)
/// wait for a change instead of re-reading `/proc` on a timer.
#[cfg(any(test, target_os = "linux"))]
static RUNNING_CHANGES: std::sync::LazyLock<tokio::sync::watch::Sender<u64>> =
    std::sync::LazyLock::new(|| tokio::sync::watch::Sender::new(0));

#[cfg(any(test, target_os = "linux"))]
fn note_running_changed() {
    RUNNING_CHANGES.send_modify(|generation| *generation = generation.wrapping_add(1));
}

/// Wakes on every change to the launched-process registry; see
/// [`verified_running_packages`] for the set itself.
#[cfg(target_os = "linux")]
pub(crate) fn subscribe_running_changes() -> tokio::sync::watch::Receiver<u64> {
    RUNNING_CHANGES.subscribe()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceLaunchIdentity {
    pub app_id: String,
    pub workspace_id: String,
    pub allowed_compat_entry_ids: Vec<String>,
    pub compat_entry_generation: u64,
    /// Original signed declaration snapshot, not the package currently installed.
    pub application_creation: Option<tilo_action_broker::services::ApplicationCreationEvidence>,
}

#[derive(Clone, Debug)]
struct TrustedServiceLaunch {
    start_time: u64,
    identity: ServiceLaunchIdentity,
    registered_at: Instant,
    continuation_instance: Option<u64>,
    /// The broker's ticket for the launch that started this process: the
    /// startup token the compositor gave it, never read back from the app.
    launch_ticket: Option<u64>,
}

static TRUSTED_SERVICE_LAUNCHES: std::sync::LazyLock<Mutex<HashMap<u32, TrustedServiceLaunch>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static WINDOWS_COMPAT_ENTRY_IDS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static WINDOWS_COMPAT_ENTRY_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

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
    // Its scope starts in the foreground tier; the focus policy keeps it
    // there only until the window appears and settles.
    #[cfg(target_os = "linux")]
    ::launcher::priority::notify_launch(pid);
    if let Ok(mut map) = LAUNCHED_PROCESSES.lock() {
        map.insert(
            pid,
            LaunchedProcess {
                package_id: package_id.to_string(),
                workspace_id: workspace_id.map(str::to_string),
                start_time,
                confined,
                retiring: false,
            },
        );
    }
    note_running_changed();
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
#[cfg(test)]
fn register_trusted_service_launch(pid: u32, identity: ServiceLaunchIdentity) {
    register_trusted_window_launch(pid, identity, None, None);
}

fn register_trusted_window_launch(
    pid: u32,
    identity: ServiceLaunchIdentity,
    continuation_instance: Option<u64>,
    launch_ticket: Option<u64>,
) {
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
            continuation_instance,
            launch_ticket,
        },
    );
}

/// Launch records of the apps the session shows inside its own process: the
/// app and room each is about to connect for, until its collaboration
/// connection is attested or the record expires.
struct InProcessLaunch {
    identity: ServiceLaunchIdentity,
    registered_at: Instant,
    continuation_instance: Option<u64>,
}
static IN_PROCESS_LAUNCHES: Mutex<Vec<InProcessLaunch>> = Mutex::new(Vec::new());

/// Record that the session itself is about to connect `app_id` in room
/// `workspace_id`, as it records a process it launched.
pub fn register_in_process_launch(app_id: &str, workspace_id: &str) {
    register_in_process_window(app_id, workspace_id, None);
}

pub(crate) fn register_in_process_window(
    app_id: &str,
    workspace_id: &str,
    continuation_instance: Option<u64>,
) {
    let mut launches = lock(&IN_PROCESS_LAUNCHES);
    launches.retain(|launch| launch.registered_at.elapsed() <= SERVICE_LAUNCH_TTL);
    launches.push(InProcessLaunch {
        identity: ServiceLaunchIdentity {
            app_id: app_id.to_owned(),
            workspace_id: workspace_id.to_owned(),
            allowed_compat_entry_ids: Vec::new(),
            compat_entry_generation: 0,
            application_creation: None,
        },
        registered_at: Instant::now(),
        continuation_instance,
    });
}

/// Spend the session's launch record for a connection from its own process
/// naming `app_id` in room `workspace_id`.
pub fn consume_in_process_launch(pid: u32, app_id: &str, workspace_id: &str) -> bool {
    take_in_process_window_launch(pid, app_id, workspace_id).is_some()
}

pub(crate) fn authenticated_window_launch(
    pid: u32,
    app: &str,
    room: &str,
) -> Option<tilo_action_broker::services::AuthenticatedApplicationLaunch> {
    let (_, start_time) = proc_stat(pid)?;
    {
        let mut launches = lock(&TRUSTED_SERVICE_LAUNCHES);
        launches.retain(|_, launch| launch.registered_at.elapsed() <= SERVICE_LAUNCH_TTL);
        if let Some(launch) = launches.get(&pid)
            && launch.start_time == start_time
            && launch.identity.app_id == app
            && launch.identity.workspace_id == room
        {
            return Some(
                tilo_action_broker::services::AuthenticatedApplicationLaunch {
                    creation: launch.identity.application_creation.clone(),
                    continuation_instance: launch.continuation_instance,
                    launch_ticket: launch.launch_ticket,
                },
            );
        }
    }
    take_in_process_window_launch(pid, app, room)
}

fn take_in_process_window_launch(
    pid: u32,
    app: &str,
    room: &str,
) -> Option<tilo_action_broker::services::AuthenticatedApplicationLaunch> {
    if pid != std::process::id() {
        return None;
    }
    let mut launches = lock(&IN_PROCESS_LAUNCHES);
    launches.retain(|launch| launch.registered_at.elapsed() <= SERVICE_LAUNCH_TTL);
    let index = launches
        .iter()
        .position(|launch| launch.identity.app_id == app && launch.identity.workspace_id == room)?;
    let launch = launches.remove(index);
    drop(launches);
    Some(
        tilo_action_broker::services::AuthenticatedApplicationLaunch {
            creation: launch.identity.application_creation,
            continuation_instance: launch.continuation_instance,
            launch_ticket: None,
        },
    )
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
    #[cfg(target_os = "linux")]
    ::launcher::priority::notify_launch_exited(pid);
    if let Ok(mut map) = LAUNCHED_PROCESSES.lock() {
        map.remove(&pid);
    }
    lock(&TRUSTED_SERVICE_LAUNCHES).remove(&pid);
    LAUNCH_RETIREMENT_CHANGED.notify_all();
    note_running_changed();
}

/// Whether the launcher process `pid` is retiring: its app's last window
/// is closing, and the app is saving before it ends.
#[cfg(target_os = "linux")]
fn launch_is_retiring(pid: u32) -> bool {
    LAUNCHED_PROCESSES
        .lock()
        .is_ok_and(|processes| processes.get(&pid).is_some_and(|process| process.retiring))
}

/// A native app's last toplevel is closing. Attribute retirement through
/// the kernel peer's exact live launcher ancestor, never its claimed app id.
pub(crate) fn note_native_writer_retiring(mut pid: u32) {
    for _ in 0..32 {
        let Some((parent, start_time)) = proc_stat(pid) else {
            return;
        };
        if let Ok(mut processes) = LAUNCHED_PROCESSES.lock()
            && let Some(process) = processes.get_mut(&pid)
            && process.start_time == start_time
        {
            if process.confined {
                process.retiring = true;
                eprintln!(
                    "app-writer-retiring: package={} room={:?} launcher_pid={pid}",
                    process.package_id, process.workspace_id
                );
            }
            return;
        }
        if parent == 0 || parent == pid {
            return;
        }
        pid = parent;
    }
}

/// How long the launch worker sleeps between looks at a retiring writer's
/// launcher, in case its exit came without a wake-up.
const RETIREMENT_RECHECK: std::time::Duration = std::time::Duration::from_millis(250);

/// Wait on the launch worker, before spawning a replacement. Surface
/// disappearance is not process exit: a native app may still checkpoint its
/// canonical record. Never remove its lock or race a second writer into it.
///
/// The wait ends when the retiring launcher has exited, which the kernel
/// proves: its pid no longer names a process with the start time it was
/// registered with. The launcher outlives its sandbox, so the writer inside
/// has exited too. Unregistering the launch is not that proof and can come
/// much later or never: the launch worker does it only once every holder of
/// the launcher's stderr has closed it, and a descendant that outlives the
/// launcher (or a launch worker that never got there) would otherwise hold
/// every later open of the app for `timeout` and then refuse it, for the
/// rest of the session.
fn wait_for_retiring_app(
    package_id: &str,
    command: &LaunchCommand,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let Some(identity) = &command.service_identity else {
        return Ok(());
    };
    let started = std::time::Instant::now();
    let mut processes = LAUNCHED_PROCESSES
        .lock()
        .map_err(|_| crate::shell::i18n::t("shell-launch-registry-unavailable"))?;
    let retiring = |processes: &mut HashMap<u32, LaunchedProcess>| {
        processes.retain(|pid, process| {
            !process.retiring
                || proc_stat(*pid).is_some_and(|(_, start)| start == process.start_time)
        });
        processes.values().any(|process| {
            process.confined
                && process.retiring
                && process.package_id == package_id
                && process.workspace_id.as_deref() == Some(identity.workspace_id.as_str())
        })
    };
    if retiring(&mut processes) {
        eprintln!(
            "app-writer-retirement-wait: package={package_id} room={}",
            identity.workspace_id
        );
        #[cfg(test)]
        lock(&RETIREMENT_WAITS).push(package_id.to_owned());
    }
    while retiring(&mut processes) {
        let remaining = timeout
            .checked_sub(started.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                eprintln!(
                    "app-writer-retirement-timeout: package={package_id} room={} after={:?}",
                    identity.workspace_id, timeout
                );
                crate::shell::i18n::t("shell-launch-writer-still-closing")
            })?;
        let (next, _) = LAUNCH_RETIREMENT_CHANGED
            .wait_timeout(processes, remaining.min(RETIREMENT_RECHECK))
            .map_err(|_| crate::shell::i18n::t("shell-launch-registry-unavailable"))?;
        processes = next;
    }
    drop(processes);
    Ok(())
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

/// Kernel-bound identity of one supervised launcher process. The start time
/// makes a recycled numeric PID a different custody domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VerifiedLaunchCustody {
    pub supervisor_pid: u32,
    pub supervisor_start_time: u64,
}

/// Resolve a descendant to the exact live launch record that owns it.
/// Kernel parent links and process start-times prevent app-id spoofing and PID
/// reuse; the bounded walk fails closed for daemonised or unknown clients.
pub(crate) fn verified_launch_for_descendant(pid: u32) -> Option<VerifiedLaunchIdentity> {
    verified_launch_custody_for_descendant(pid).map(|(identity, _)| identity)
}

/// Resolve both the durable launch identity and the exact live supervising
/// process which authenticated this descendant.
pub(crate) fn verified_launch_custody_for_descendant(
    mut pid: u32,
) -> Option<(VerifiedLaunchIdentity, VerifiedLaunchCustody)> {
    const MAX_ANCESTORS: usize = 32;
    for _ in 0..MAX_ANCESTORS {
        let (parent_pid, start_time) = proc_stat(pid)?;
        if let Ok(map) = LAUNCHED_PROCESSES.lock()
            && let Some(known) = map.get(&pid)
            && known.start_time == start_time
        {
            return Some((
                VerifiedLaunchIdentity {
                    package_id: known.package_id.clone(),
                    workspace_id: known.workspace_id.clone(),
                    confined: known.confined,
                },
                VerifiedLaunchCustody {
                    supervisor_pid: pid,
                    supervisor_start_time: start_time,
                },
            ));
        }
        if parent_pid == 0 || parent_pid == pid {
            return None;
        }
        pid = parent_pid;
    }
    None
}

pub(crate) fn launch_custody_is_current(
    custody: VerifiedLaunchCustody,
    package_id: &str,
    workspace_id: &str,
) -> bool {
    let Some((_, start_time)) = proc_stat(custody.supervisor_pid) else {
        return false;
    };
    if start_time != custody.supervisor_start_time {
        return false;
    }
    LAUNCHED_PROCESSES.lock().is_ok_and(|launches| {
        launches.get(&custody.supervisor_pid).is_some_and(|launch| {
            launch.start_time == custody.supervisor_start_time
                && launch.confined
                && launch.package_id == package_id
                && launch.workspace_id.as_deref() == Some(workspace_id)
        })
    })
}

/// Resolve a host process (normally xdg-dbus-proxy) to the Tilo package
/// supervising one of its ancestors. The walk is bounded and refuses stale
/// registry entries. Unknown/direct session-bus callers stay unattributed.
pub(crate) fn verified_package_for_descendant(pid: u32) -> Option<String> {
    verified_launch_for_descendant(pid).map(|identity| identity.package_id)
}

/// End every unconfined host launch of `package_id`.
///
/// `xdg_toplevel.close` unmaps Wine File's window; `wineserver` and
/// `tilo-launcher run-host` stay. The launcher child `setsid`s the app, so
/// a lone SIGTERM of the supervisor leaves the prefix running. Kill the
/// app's cgroup (or stop its systemd scope, for a launcher that had to ask
/// for one), then signal the process tree — including processes that share
/// a session the app created.
#[cfg(any(test, target_os = "linux"))]
pub(crate) fn terminate_unconfined_launches(package_id: &str) {
    for pid in live_unconfined_launch_pids(package_id) {
        request_stop_host_scope(package_id, pid);
        terminate_process_tree(pid);
    }
}

#[cfg(any(test, target_os = "linux"))]
fn live_unconfined_launch_pids(package_id: &str) -> Vec<u32> {
    let Ok(mut map) = LAUNCHED_PROCESSES.lock() else {
        return Vec::new();
    };
    map.retain(|pid, launch| proc_stat(*pid).is_some_and(|(_, start)| start == launch.start_time));
    map.iter()
        .filter(|(_, launch)| {
            !launch.confined && launch.package_id.eq_ignore_ascii_case(package_id)
        })
        .map(|(pid, _)| *pid)
        .collect()
}

/// Desktop ids of unconfined launches that still have a live supervisor.
#[cfg(any(test, target_os = "linux"))]
pub(crate) fn live_unconfined_package_ids() -> Vec<String> {
    let Ok(mut map) = LAUNCHED_PROCESSES.lock() else {
        return Vec::new();
    };
    map.retain(|pid, launch| proc_stat(*pid).is_some_and(|(_, start)| start == launch.start_time));
    let mut ids: Vec<String> = map
        .iter()
        .filter(|(_, launch)| !launch.confined)
        .map(|(_, launch)| launch.package_id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Whether `package_id` is a live unconfined launch, not merely a window class.
#[cfg(any(test, target_os = "linux"))]
pub(crate) fn unconfined_launch_is_live(package_id: &str) -> bool {
    !live_unconfined_launch_pids(package_id).is_empty()
}

#[cfg(all(target_os = "linux", not(test)))]
fn request_stop_host_scope(desktop_id: &str, pid: u32) {
    // Born in the session's app tree: its cgroup ends it at once, processes
    // that left its tree included, with no round trip.
    if let Some(cgroup) = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .ok()
        .and_then(|cgroup| launcher::app_cgroup::app_cgroup_of(&cgroup))
    {
        if let Err(error) = launcher::app_cgroup::kill(&launcher::app_cgroup::fs_path(&cgroup)) {
            eprintln!("app-stop: package={desktop_id} cgroup={cgroup}: {error}");
        }
        return;
    }
    let Some(unit) = launcher::scope::unit_name(desktop_id, pid) else {
        return;
    };
    let _ = Command::new("/usr/bin/busctl")
        .args([
            "--user",
            "--timeout=2",
            "call",
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
            "StopUnit",
            "ss",
            &unit,
            "replace",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
fn request_stop_host_scope(_desktop_id: &str, _pid: u32) {}

/// Packages whose launches were ended to start again on a newer build, and
/// until when their ends count as that.
static RESTARTING_ON_NEW_BUILD: std::sync::LazyLock<Mutex<HashMap<String, Instant>>> =
    std::sync::LazyLock::new(Mutex::default);

/// How long after a restart was asked an end of the app's launch counts as
/// it.
const RESTART_ON_NEW_BUILD_WITHIN: Duration = Duration::from_secs(30);

/// End every launch of `package_id` so its windows start again, in place,
/// on the build now installed: the windows that go with the launches are
/// kept, as for an app ended for memory, and put back where they were once
/// each launch has ended ([`restarting_on_new_build`]). Whether any launch
/// was running.
pub(crate) fn restart_on_new_build(package_id: &str) -> bool {
    let pids: Vec<u32> = {
        let Ok(mut map) = LAUNCHED_PROCESSES.lock() else {
            return false;
        };
        map.retain(|pid, launch| {
            proc_stat(*pid).is_some_and(|(_, start)| start == launch.start_time)
        });
        map.iter()
            .filter(|(_, launch)| launch.package_id.eq_ignore_ascii_case(package_id))
            .map(|(pid, _)| *pid)
            .collect()
    };
    if pids.is_empty() {
        return false;
    }
    lock(&RESTARTING_ON_NEW_BUILD).insert(
        package_id.to_owned(),
        Instant::now() + RESTART_ON_NEW_BUILD_WITHIN,
    );
    #[cfg(any(test, target_os = "linux"))]
    for pid in pids {
        terminate_process_tree(pid);
    }
    true
}

/// Whether `package_id`'s launch ended because [`restart_on_new_build`]
/// ended it: its lost windows are to be put back, not reported.
pub(crate) fn restarting_on_new_build(package_id: &str) -> bool {
    let mut restarting = lock(&RESTARTING_ON_NEW_BUILD);
    restarting.retain(|_, until| *until > Instant::now());
    restarting.contains_key(package_id)
}

/// Mark `package_id` as restarting on a new build, as
/// [`restart_on_new_build`] does, without a process to end.
#[cfg(test)]
pub(crate) fn mark_restarting_for_test(package_id: &str) {
    lock(&RESTARTING_ON_NEW_BUILD).insert(
        package_id.to_owned(),
        Instant::now() + RESTART_ON_NEW_BUILD_WITHIN,
    );
}

#[cfg(any(test, target_os = "linux"))]
fn terminate_process_tree(root: u32) {
    let pids = descendant_and_session_pids(root);
    for pid in &pids {
        signal_pid(*pid, libc::SIGTERM);
    }
    for pid in pids {
        if proc_stat(pid).is_some() {
            signal_pid(pid, libc::SIGKILL);
        }
    }
}

#[cfg(any(test, target_os = "linux"))]
fn signal_pid(pid: u32, signal: i32) {
    let Ok(raw) = i32::try_from(pid) else {
        return;
    };
    // SAFETY: signal a compositor-supervised launch pid, or a process in
    // that launch's session. Never the compositor: the launcher child
    // called `setsid` and lives in its own session.
    let _ = unsafe { libc::kill(raw, signal) };
}

#[derive(Clone, Copy)]
#[cfg(any(test, target_os = "linux"))]
struct ProcRow {
    pid: u32,
    parent: u32,
    session: u32,
}

#[cfg(any(test, target_os = "linux"))]
fn scan_proc_rows() -> Vec<ProcRow> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_string_lossy().parse().ok()?;
            let (parent, session, _) = proc_ancestry(pid)?;
            Some(ProcRow {
                pid,
                parent,
                session,
            })
        })
        .collect()
}

#[cfg(any(test, target_os = "linux"))]
fn proc_ancestry(pid: u32) -> Option<(u32, u32, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(") ")?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    Some((
        fields.get(1)?.parse().ok()?,
        fields.get(3)?.parse().ok()?,
        fields.get(19)?.parse().ok()?,
    ))
}

/// The launch pid, every descendant, and every process that joined a
/// session a descendant created with `setsid` (Wine's prefix).
#[cfg(any(test, target_os = "linux"))]
fn descendant_and_session_pids(root: u32) -> Vec<u32> {
    let rows = scan_proc_rows();
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    for row in &rows {
        kids.entry(row.parent).or_default().push(row.pid);
    }
    let mut tree = Vec::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if tree.contains(&pid) {
            continue;
        }
        tree.push(pid);
        if let Some(children) = kids.get(&pid) {
            stack.extend(children.iter().copied());
        }
    }
    let sessions: Vec<u32> = rows
        .iter()
        .filter(|row| tree.contains(&row.pid) && row.session == row.pid)
        .map(|row| row.session)
        .collect();
    let mut pids = tree;
    for row in rows {
        if sessions.contains(&row.session) && !pids.contains(&row.pid) {
            pids.push(row.pid);
        }
    }
    pids
}

const FILES_PACKAGE_ID: &str = "so.tilo.file-explorer";
const TRUSTED_IDENTITIES_FILE: &str = "trusted-identities.json";
static TRUSTED_IDENTITY_PROJECTIONS: LazyLock<
    Mutex<std::collections::HashMap<RoomId, tilo_user_identity::TrustedIdentityProjectionV2>>,
> = LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

pub use tilo_installed_apps::InstalledApp;

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
    /// The host entry exists but cannot be run as written (for example an
    /// unusable `Exec=` line).
    HostAppRefused { desktop_id: String, detail: String },
    /// The app's build needs interface versions newer than this system
    /// serves; the system's update has been started
    /// (`crate::shell::app_versions`).
    SystemBehind { package_id: String, unmet: String },
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
            LaunchError::HostAppRefused { desktop_id, detail } => {
                format!("{desktop_id} cannot be started: {detail}")
            }
            LaunchError::SystemBehind { package_id, unmet } => {
                format!("{package_id} needs a newer system: {unmet}")
            }
        }
    }

    /// Concise recovery copy for fixed-height desktop notifications. Detailed
    /// ids, paths, and backend errors belong in the session log and in the
    /// fallible caller's error, not in top-right chrome.
    pub(crate) fn notification_message(&self) -> String {
        use crate::shell::i18n::t;
        match self {
            Self::ProviderUnavailable { .. } => t("launch-error-provider-unavailable"),
            Self::NotInstalled { .. } | Self::HostAppNotFound { .. } => {
                t("launch-error-not-installed")
            }
            Self::NotAGuiApp { .. } => t("launch-error-not-gui-app"),
            Self::NotAManagedService { .. } => t("launch-error-not-managed-service"),
            Self::AgentNotAdmitted { .. } | Self::AgentLaunchNotAuthorized => {
                t("launch-error-not-authorized")
            }
            Self::AgentLaunchWorkerUnavailable { .. } | Self::LauncherBinaryMissing { .. } => {
                t("launch-error-launcher-unavailable")
            }
            Self::NoDisplay => t("launch-error-no-display"),
            Self::NoWorkspace => t("launch-error-no-workspace"),
            Self::ShellOwnedIntent => t("launch-error-shell-owned-intent"),
            Self::IdentityUnavailable { .. } => t("launch-error-identity-unavailable"),
            Self::RegistryUnreadable { .. } => t("launch-error-registry-unreadable"),
            Self::HostAppRefused { .. } => t("launch-error-host-app-refused"),
            Self::SystemBehind { .. } => t("launch-failed-system-behind"),
        }
    }
}

/// Why a launch failed before the app's process existed, as a closed set the
/// notification can name. `detail` on the event stays in the log; this is
/// what the person reads, so a refusal is never reported as "the launcher is
/// unavailable" unless the launcher really is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchFailure {
    /// `tilo-launcher` is missing or its process could not be created.
    LauncherUnavailable,
    ProviderUnavailable,
    NotInstalled,
    NotAGuiApp,
    NotAManagedService,
    NotAuthorized,
    NoDisplay,
    NoWorkspace,
    ShellOwnedIntent,
    IdentityUnavailable,
    RegistryUnreadable,
    HostAppRefused,
    /// The room's file permissions changed between choosing to open the app
    /// and starting it, so the grants it was given no longer hold.
    AuthorityChanged,
    /// The room's file permissions were not published in time.
    AuthorityTimedOut,
    /// The sandbox's file permissions could not be supervised.
    SandboxSupervision,
    /// The previous copy of the app is still shutting down.
    PreviousInstanceBusy,
    /// The app was behind this system's interfaces, and its update could not
    /// be fetched.
    UpdateFailed,
    /// The app is behind this system's interfaces and its developer has not
    /// published a build that fits yet.
    WaitingForDeveloper,
    /// The app needs a newer system, whose update has been started.
    SystemBehind,
    /// The room the app was to open in will not be ready for it; the
    /// event's detail says why, in the person's words.
    RoomRefused,
    /// Held launches released by a failed prerequisite and other causes with
    /// no more specific copy.
    Other,
}

impl LaunchFailure {
    /// Classify the error that stopped `register_sandbox` or the sandbox
    /// supervision around it.
    #[cfg(any(test, target_os = "linux"))]
    pub(crate) fn of_authority_error(error: &std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::PermissionDenied => Self::AuthorityChanged,
            std::io::ErrorKind::TimedOut => Self::AuthorityTimedOut,
            _ => Self::SandboxSupervision,
        }
    }

    pub(crate) const fn message_key(self) -> &'static str {
        match self {
            Self::LauncherUnavailable | Self::Other => "launch-failed-launcher-unavailable",
            Self::ProviderUnavailable => "launch-error-provider-unavailable",
            Self::NotInstalled => "launch-error-not-installed",
            Self::NotAGuiApp => "launch-error-not-gui-app",
            Self::NotAManagedService => "launch-error-not-managed-service",
            Self::NotAuthorized => "launch-error-not-authorized",
            Self::NoDisplay => "launch-error-no-display",
            Self::NoWorkspace => "launch-error-no-workspace",
            Self::ShellOwnedIntent => "launch-error-shell-owned-intent",
            Self::IdentityUnavailable => "launch-error-identity-unavailable",
            Self::RegistryUnreadable => "launch-error-registry-unreadable",
            Self::HostAppRefused => "launch-error-host-app-refused",
            Self::AuthorityChanged => "launch-failed-authority-changed",
            Self::AuthorityTimedOut => "launch-failed-authority-timeout",
            Self::SandboxSupervision => "launch-error-sandbox-supervision",
            Self::PreviousInstanceBusy => "launch-failed-previous-instance",
            Self::UpdateFailed => "launch-failed-update",
            Self::WaitingForDeveloper => "launch-failed-waiting-for-developer",
            Self::SystemBehind => "launch-failed-system-behind",
            Self::RoomRefused => "workspace-needs-join-again",
        }
    }
}

impl LaunchError {
    /// The typed reason this error is reported under.
    pub(crate) const fn failure(&self) -> LaunchFailure {
        match self {
            Self::ProviderUnavailable { .. } => LaunchFailure::ProviderUnavailable,
            Self::NotInstalled { .. } | Self::HostAppNotFound { .. } => LaunchFailure::NotInstalled,
            Self::NotAGuiApp { .. } => LaunchFailure::NotAGuiApp,
            Self::NotAManagedService { .. } => LaunchFailure::NotAManagedService,
            Self::AgentNotAdmitted { .. } | Self::AgentLaunchNotAuthorized => {
                LaunchFailure::NotAuthorized
            }
            Self::AgentLaunchWorkerUnavailable { .. } | Self::LauncherBinaryMissing { .. } => {
                LaunchFailure::LauncherUnavailable
            }
            Self::NoDisplay => LaunchFailure::NoDisplay,
            Self::NoWorkspace => LaunchFailure::NoWorkspace,
            Self::ShellOwnedIntent => LaunchFailure::ShellOwnedIntent,
            Self::IdentityUnavailable { .. } => LaunchFailure::IdentityUnavailable,
            Self::RegistryUnreadable { .. } => LaunchFailure::RegistryUnreadable,
            Self::HostAppRefused { .. } => LaunchFailure::HostAppRefused,
            Self::SystemBehind { .. } => LaunchFailure::SystemBehind,
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
    /// managed agent service. Ordinary app launches leave this unset.
    pub managed_agent_id: Option<String>,
    /// Workspace paired with [`Self::managed_agent_id`], keeping a launch
    /// event from one room from changing another room's agent state.
    pub managed_workspace_id: Option<String>,
    pub kind: LaunchEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchEventKind {
    /// `tilo-launcher` is running with this host pid.
    Started { pid: u32 },
    /// The child could not be spawned at all.
    SpawnFailed {
        /// Why, as the person is told. `detail` is the log's account of it.
        reason: LaunchFailure,
        detail: String,
    },
    /// The launch chain exited non-zero. `stderr_tail` is the last few
    /// lines it printed — usually the actual reason.
    Failed {
        code: Option<i32>,
        stderr_tail: String,
    },
    /// The app ran and exited cleanly. Not an error; reported so
    /// callers can retire per-app UI state.
    Exited,
    /// The app's window went away without anyone closing it, whatever its
    /// process reported. `code` is its exit status when it had one.
    WindowLost {
        code: Option<i32>,
        stderr_tail: String,
    },
    /// The system ended the app, or one of its independent jobs, because
    /// memory ran out (`launcher::memory_pressure`).
    OutOfMemory(MemoryEnd),
}

/// How memory ended an app or one of its jobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEnd {
    pub cause: ::launcher::memory_pressure::Cause,
    /// The job that ended (a language server, a terminal session), by the
    /// name of its program, when only it did: the app keeps running. `None`
    /// when the whole app was closed.
    pub job: Option<String>,
}

impl MemoryEnd {
    /// Whether the whole app ended, not just one of its jobs.
    #[must_use]
    pub const fn closed_the_app(&self) -> bool {
        self.job.is_none()
    }
}

/// What memory did to a launch that ended with `status`: the spawner's
/// `cause` stands unless the app ended on its own, cleanly.
#[must_use]
pub fn memory_end_of(
    status: &std::io::Result<std::process::ExitStatus>,
    cause: Option<::launcher::memory_pressure::Cause>,
) -> Option<MemoryEnd> {
    let clean = status.as_ref().is_ok_and(std::process::ExitStatus::success);
    cause
        .filter(|_| !clean)
        .map(|cause| MemoryEnd { cause, job: None })
}

/// Normal-user copy for an asynchronous launch failure. The launcher keeps
/// the complete process output in the event; this deliberately does not.
/// Fixed-height desktop banners must remain a summary, not become a log
/// viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchFailureNotice {
    pub title: String,
    pub body: String,
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
        if let LaunchEventKind::OutOfMemory(end) = &self.kind {
            return Some(memory_notice(&self.display_name, end));
        }
        let body = match &self.kind {
            LaunchEventKind::SpawnFailed {
                reason: LaunchFailure::RoomRefused,
                detail,
            } => detail.clone(),
            LaunchEventKind::SpawnFailed { reason, .. } => {
                crate::shell::i18n::t(reason.message_key())
            }
            LaunchEventKind::Failed { code, stderr_tail } => {
                crate::shell::i18n::t(stopped_reason_key(*code, stderr_tail))
            }
            LaunchEventKind::WindowLost { .. } => {
                crate::shell::i18n::t("launch-failed-window-lost")
            }
            LaunchEventKind::Started { .. }
            | LaunchEventKind::Exited
            | LaunchEventKind::OutOfMemory(_) => return None,
        };
        let window_lost = match &self.kind {
            LaunchEventKind::WindowLost { .. } => true,
            LaunchEventKind::Failed { stderr_tail, .. } => {
                stderr_tail.contains(ui::app::WINDOW_LOST)
            }
            _ => false,
        };
        let title = if window_lost {
            crate::shell::i18n::t_args(
                "launch-window-lost-title",
                &[("app", compact_notice_name(&self.display_name).into())],
            )
        } else {
            launch_failure_title(&self.display_name)
        };
        Some(LaunchFailureNotice { title, body })
    }

    /// Preserve the complete failure in the compositor's session log. Field
    /// values use tracing's structured formatting, so embedded newlines or
    /// terminal escapes cannot alter the surrounding log record.
    pub(crate) fn log_failure(&self) {
        match &self.kind {
            LaunchEventKind::SpawnFailed { detail, .. } => {
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
            LaunchEventKind::WindowLost { code, stderr_tail } => {
                tracing::error!(
                    target: "tilo::app_launch",
                    package_id = %self.package_id,
                    display_name = %self.display_name,
                    exit_code = ?code,
                    stderr_tail = ?stderr_tail,
                    "application window went away without being closed"
                );
            }
            LaunchEventKind::OutOfMemory(end) => {
                tracing::error!(
                    target: "tilo::app_launch",
                    package_id = %self.package_id,
                    display_name = %self.display_name,
                    job = ?end.job,
                    cause = end.cause.wire(),
                    "ended because memory ran out"
                );
            }
            LaunchEventKind::Started { .. } | LaunchEventKind::Exited => {}
        }
    }

    /// This event, read knowing whether the launch's window went away
    /// without being closed: then neither a clean exit nor an error after
    /// the window was shown is the app ending as asked.
    #[must_use]
    pub(crate) fn with_window_lost(self, lost: bool) -> Self {
        if !lost {
            return self;
        }
        let kind = match self.kind {
            LaunchEventKind::Exited => LaunchEventKind::WindowLost {
                code: Some(0),
                stderr_tail: String::new(),
            },
            LaunchEventKind::Failed { code, stderr_tail } => {
                LaunchEventKind::WindowLost { code, stderr_tail }
            }
            other => other,
        };
        Self { kind, ..self }
    }
}

/// Why a launch stopped, as one of a few fixed sentences. The process's own
/// words never reach the banner, since they carry paths and environment;
/// they are only recognised here and logged in full by
/// [`LaunchEvent::log_failure`].
fn stopped_reason_key(code: Option<i32>, stderr_tail: &str) -> &'static str {
    let loader_failed = stderr_tail.contains("error while loading shared libraries")
        || stderr_tail.contains("cannot open shared object file");
    // The launcher's own refusal (`RunError::RuntimeLibraryMissing`), or the
    // loader failing on a library only a Tilo runtime bundle ships: the app
    // was built against another runtime than the one installed.
    if stderr_tail.contains("built for a different Tilo runtime")
        || (loader_failed
            && (stderr_tail.contains("libstd-") || stderr_tail.contains("libruntime_dylib")))
    {
        "launch-failed-runtime-mismatch"
    } else if loader_failed {
        "launch-failed-missing-library"
    } else if stderr_tail.lines().any(|line| line.starts_with("bwrap: ")) {
        "launch-failed-sandbox"
    } else if stderr_tail.contains(ui::collaboration::ROUTE_JOIN_FAILED) {
        "launch-failed-collaboration"
    } else if stderr_tail.contains(ui::app::WINDOW_LOST) {
        "launch-failed-window-lost"
    } else if code.is_none() {
        "launch-failed-crashed"
    } else {
        "launch-failed-stopped"
    }
}

/// Keep untrusted desktop-entry names on one notification line. Whitespace is
/// normalised and the scalar-value cap avoids slicing UTF-8; the full name is
/// still present in the structured failure log.
fn compact_notice_name(name: &str) -> String {
    const MAX_CHARS: usize = 32;
    let normalised = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let fallback;
    let normalised = if normalised.is_empty() {
        fallback = crate::shell::i18n::t("launch-unnamed-app");
        &fallback
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
    crate::shell::i18n::t_args(
        "launch-failure-title",
        &[("app", compact_notice_name(display_name).into())],
    )
}

/// What the person is told when memory ended an app or one of its jobs:
/// which, and why.
pub(crate) fn memory_notice(display_name: &str, end: &MemoryEnd) -> LaunchFailureNotice {
    use ::launcher::memory_pressure::Cause;
    let app = compact_notice_name(display_name);
    match &end.job {
        None => LaunchFailureNotice {
            title: crate::shell::i18n::t_args("memory-app-closed-title", &[("app", app.into())]),
            body: crate::shell::i18n::t(match end.cause {
                Cause::Kernel => "memory-app-closed-body",
                Cause::Runaway => "memory-app-runaway-body",
            }),
        },
        Some(job) => LaunchFailureNotice {
            title: crate::shell::i18n::t_args(
                "memory-job-stopped-title",
                &[
                    ("process", compact_notice_name(job).into()),
                    ("app", app.clone().into()),
                ],
            ),
            body: crate::shell::i18n::t_args(
                match end.cause {
                    Cause::Kernel => "memory-job-stopped-body",
                    Cause::Runaway => "memory-job-runaway-body",
                },
                &[("app", app.into())],
            ),
        },
    }
}

/// Who a running launch is, for the event stream — a package id for a
/// `.tapp`, a desktop-file id for a host app.
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
    /// The workspace whose X11 display a host app gets (`--x11-display`,
    /// `--x11-authority`), looked up on the launch's own thread: a display
    /// is bound on first use, which touches the disk, and a launch can be
    /// started from the compositor thread.
    pub x11_workspace: Option<String>,
}

static WAYLAND_SOCKET: Mutex<Option<PathBuf>> = Mutex::new(None);
/// The launch bookkeeping of one desktop session: the [`LaunchEvent`]s its
/// chrome drains through [`drain_events`], the windows its launches are
/// expected to open, and the names of the apps it has launched.
///
/// A launch belongs to the session of the thread that asked for it, and the
/// workers it starts carry that session with them ([`spawn_reporting`]), so
/// an event or a window's destination always reaches the session that
/// started the launch. A shell process runs one session, and every thread
/// shares it. The unit tests run many sessions side by side in one process,
/// one per test thread, so there each thread starts a session of its own: a
/// test that ticks the chrome or claims a placement sees its own launches,
/// never another test's.
#[derive(Clone, Default)]
pub(crate) struct LaunchSession(std::sync::Arc<SessionLaunches>);

pub(crate) struct SessionLaunches {
    events: Mutex<Vec<LaunchEvent>>,
    pub(crate) diagnostics: Mutex<Option<diagnostics::Options>>,
    pub(crate) launch_history: Mutex<launch_history::History>,
    pub(super) startup: super::launch_readiness::StartupObservations,
    /// `package_id` -> where its next window should land; claimed (not
    /// consumed) by [`placement_for_toplevel`] since the compositor tracks
    /// each toplevel in two trees and consuming here would desync them.
    pending: Mutex<Vec<PendingPlacement>>,
    /// `package_id` -> display name for everything this session has
    /// launched, so an adopted foreign toplevel can show the app's name.
    launched: Mutex<Vec<(String, Vec<String>, String)>>,
    /// The launch key each mapped surface was attributed to.
    surfaces: Mutex<HashMap<u64, String>>,
    /// Launch keys whose app has shown a window since it last started.
    presented: Mutex<HashSet<String>>,
    /// Launch keys whose window went away without anyone closing it: the
    /// compositor never asked, and the person never reached the app to ask
    /// it. However the process then exits, the window was lost.
    vanished: Mutex<HashSet<String>>,
    /// The tabs of windows that went away without being asked to close, by
    /// launch key, as a reopen restores them: kept until the launch's end
    /// says whether memory ended it (then its notice reopens them) or not.
    lost_tabs:
        Mutex<HashMap<String, Vec<(std::time::Instant, crate::shell::closed_tabs::ClosedTab)>>>,
    /// Launch keys whose app has been handed an action invocation since it
    /// started: an agent, another device or the person drove it, so a
    /// window it then closes was closed on request.
    acted_on: Mutex<HashSet<String>>,
    /// User launches still waiting for a window. This outlives the short
    /// placement claim: Wine's first window can announce `wineboot.exe` long
    /// after that claim expires, and it still belongs to the launch the
    /// person asked for. Session autostart never enters this set.
    awaiting_window: Mutex<HashSet<String>>,
    /// User launches that already own a window whose toolkit id is not the
    /// desktop id (`wineboot.exe`, `control.exe`).
    unresolved_owners: Mutex<HashSet<String>>,
    /// XDG autostart entries. Their exit is not a failed open of the app the
    /// person just launched.
    session_helpers: Mutex<HashSet<String>>,
    /// Autostart entries with `X-GNOME-AutoRestart=true`: how to start each
    /// again, and when it last started.
    restartable_helpers: Mutex<HashMap<String, RestartableHelper>>,
    /// Each package still on its way, so a repeated activation does not
    /// start it twice; the frame thread settles it when its window appears.
    pub(crate) opening_launches: Mutex<Vec<String>>,
    /// The documents open in this session, and the windows showing them.
    pub(crate) open_documents: Mutex<Vec<crate::shell::doc_open::OpenDocument>>,
    /// Launches held until the session's services are up.
    #[cfg(any(test, target_os = "linux"))]
    gate: Mutex<Gate>,
    /// Launches held until their room is ready for apps.
    #[cfg(any(test, target_os = "linux"))]
    rooms: room_hold::State,
}

impl Default for SessionLaunches {
    fn default() -> Self {
        Self {
            events: Mutex::default(),
            diagnostics: Mutex::default(),
            launch_history: Mutex::default(),
            startup: super::launch_readiness::StartupObservations::default(),
            pending: Mutex::default(),
            launched: Mutex::default(),
            surfaces: Mutex::default(),
            presented: Mutex::default(),
            vanished: Mutex::default(),
            lost_tabs: Mutex::default(),
            acted_on: Mutex::default(),
            awaiting_window: Mutex::default(),
            unresolved_owners: Mutex::default(),
            session_helpers: Mutex::default(),
            restartable_helpers: Mutex::default(),
            opening_launches: Mutex::default(),
            open_documents: Mutex::default(),
            #[cfg(any(test, target_os = "linux"))]
            gate: Mutex::new(Gate::open_with_capacity(
                crate::shell::launch_gate::MAX_HELD_LAUNCHES,
            )),
            #[cfg(any(test, target_os = "linux"))]
            rooms: room_hold::State::default(),
        }
    }
}

impl std::fmt::Debug for LaunchSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LaunchSession")
    }
}

#[cfg(not(test))]
static PROCESS_SESSION: LazyLock<LaunchSession> = LazyLock::new(LaunchSession::default);

thread_local! {
    static THREAD_SESSION: std::cell::RefCell<Option<LaunchSession>> =
        const { std::cell::RefCell::new(None) };
}

impl LaunchSession {
    pub(crate) fn presentation_observer_enabled(&self) -> bool {
        lock(&self.diagnostics).map_or_else(
            || std::env::var_os("TILO_PROFILE_PRESENTATION").is_some(),
            |options| options.0[0],
        )
    }

    pub(crate) fn diagnostic_options(&self) -> diagnostics::Options {
        lock(&self.diagnostics).unwrap_or_else(diagnostics::Options::inherited)
    }

    pub(crate) fn configure_diagnostics(&self, options: diagnostics::Options) {
        *lock(&self.diagnostics) = Some(options);
    }

    /// Run `f` on the session launches started on this thread belong to.
    pub(crate) fn with<R>(f: impl FnOnce(&Self) -> R) -> R {
        f(&Self::current())
    }

    /// The session launches started on this thread belong to.
    pub(crate) fn current() -> Self {
        THREAD_SESSION.with(|slot| {
            slot.borrow_mut()
                .get_or_insert_with(Self::for_new_thread)
                .clone()
        })
    }

    #[cfg(not(test))]
    fn for_new_thread() -> Self {
        PROCESS_SESSION.clone()
    }

    #[cfg(test)]
    fn for_new_thread() -> Self {
        Self::default()
    }

    /// Make this the session launches started on this thread belong to.
    fn adopt(self) {
        THREAD_SESSION.with(|slot| *slot.borrow_mut() = Some(self));
    }

    pub(crate) fn push(&self, event: LaunchEvent) {
        if matches!(
            event.kind,
            LaunchEventKind::SpawnFailed { .. } | LaunchEventKind::Failed { .. }
        ) {
            super::doc_open::forget_unpresented_documents(self, &event.package_id);
        }
        lock(&self.0.events).push(event);
    }

    fn take(&self) -> Vec<LaunchEvent> {
        std::mem::take(&mut *lock(&self.0.events))
    }
}

impl std::ops::Deref for LaunchSession {
    type Target = SessionLaunches;

    fn deref(&self) -> &SessionLaunches {
        &self.0
    }
}

/// Spawn a launch worker in the calling thread's [`LaunchSession`], as is
/// anything it launches in turn.
///
/// # Errors
///
/// As [`std::thread::Builder::spawn`].
pub(crate) fn spawn_reporting<T: Send + 'static>(
    builder: std::thread::Builder,
    work: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<T>> {
    let events = LaunchSession::current();
    builder.spawn(move || {
        events.adopt();
        work()
    })
}

/// Last enumeration and when it was memoised; `None` marks it stale, so it
/// is only the baseline the next read compares against.
static APP_CACHE: Mutex<Option<(Vec<InstalledApp>, Option<Instant>)>> = Mutex::new(None);
static APP_CACHE_REVISION: AtomicU64 = AtomicU64::new(0);
/// Bumped when a watch (or an explicit invalidate) makes the memoised
/// snapshot untrustworthy. Distinct from [`APP_CACHE_REVISION`]: a
/// recursive inotify walk must not look like a new generation of apps.
static APP_CACHE_EPOCH: AtomicU64 = AtomicU64::new(0);
static APP_CACHE_WATCH: Mutex<Option<Vec<PathBuf>>> = Mutex::new(None);
/// Capabilities of the workspace apps are being launched into, published
/// by [`set_workspace_capabilities`] or [`launch_in_workspace`]. Empty is
/// the safe default: an app launched before publish gets manifest-only.
static WORKSPACE_CAPABILITIES: Mutex<LaunchGrants> = Mutex::new(LaunchGrants {
    authors: Vec::new(),
    sandbox: Vec::new(),
});
/// Resume bytes waiting for the user-directed launch of one dormant tab.
pub type ContinuationResumePayload = Option<(u16, Vec<u8>)>;
pub type StagedContinuationResume = (ContinuationResumePayload, u64);

static PENDING_CONTINUATION_RESUME: LazyLock<
    Mutex<HashMap<(RoomId, u64), StagedContinuationResume>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Locks `mutex`, recovering the guard instead of panicking a second time
/// if some other thread poisoned it. None of this module's locked state
/// (`Vec`/`Option`/plain structs) has an invariant a partial mutation
/// could break, so continuing with whatever the poisoned thread left
/// behind is safer than aborting the whole session over it.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock_recover()
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
    /// A started launcher must produce a toplevel within a bounded interval.
    /// This is independent of `retire_after`: the latter handles a process
    /// that already exited after daemonising, while this deadline prevents a
    /// still-running but windowless process from pinning "Opening…" forever.
    present_before: Instant,
    /// Opaque id in the small startup-metrics crate. Placement remains the
    /// authoritative FIFO; metrics never guesses by app id during failures.
    launch_metric_id: u64,
    metric_spawned: bool,
    /// One supervisor owns the diagnostic join even before its child spawns.
    diagnostic_reserved: bool,
    /// The live view this launch runs ([`crate::shell::live_embeds`]): its
    /// window is drawn inside another window, never placed as one.
    embed: Option<u64>,
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

/// The window identities a Tilo package id implies: the id itself plus its
/// trailing component (`so.tilo.hello` ↔ `hello`).
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

/// Publish what launches into the workspace hand their apps
/// ([`Workspace::launch_grants`]). Call whenever the active workspace or its
/// grants change — a grant only reaches the next launch if republished. The
/// whole set is published; [`launch`] hands each app its own slice.
pub fn set_workspace_capabilities(grants: LaunchGrants) {
    *lock(&WORKSPACE_CAPABILITIES) = grants;
}

/// Drop the published capability list. Subsequent launches get manifest
/// permissions only.
pub fn clear_workspace_capabilities() {
    set_workspace_capabilities(LaunchGrants::default());
}

/// What the published grants hand `app_id` ([`LaunchGrants::for_app`]): a
/// capability naming a person, room, or other app never reaches this app's
/// file.
pub fn capabilities_for_app(app_id: &str) -> LaunchGrants {
    lock(&WORKSPACE_CAPABILITIES).for_app(app_id)
}

/// Asks the person, on a launch worker, for permissions the sandbox applies
/// at launch (`crate::compositor::launch_permissions`), and returns what to
/// add to that launch's capabilities.
pub(crate) type LaunchPermissionAsk = dyn Fn(&InstalledApp, RoomId, &LaunchGrants) -> Vec<collab_types::caps::CapOverride>
    + Send
    + Sync;

/// Registered once by the compositor. Absent (tests, headless tools) nobody
/// is asked and nothing beyond the room's grants is added.
static LAUNCH_PERMISSIONS: std::sync::OnceLock<Box<LaunchPermissionAsk>> =
    std::sync::OnceLock::new();

/// Install the session's launch-time permission prompt.
pub(crate) fn set_launch_permissions(ask: Box<LaunchPermissionAsk>) {
    let _ = LAUNCH_PERMISSIONS.set(ask);
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
/// written before rename, so Files sees either the previous whole snapshot or
/// the next whole snapshot, never a prefix. It is not synced: the runtime
/// directory does not outlive the session (see `write_runtime_private`). This function accepts
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

    // Owner-only room directory (tightened when it already exists) and a
    // 0600 file that is never visible at its final name with wider access.
    let final_path = trusted_identities_dir()
        .join(room.to_string())
        .join(TRUSTED_IDENTITIES_FILE);
    write_runtime_private(&final_path, &bytes)?;
    Ok(final_path)
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

type RetainedLocations = HashMap<RoomId, Option<(String, PathBuf)>>;
static RETAINED_FILES: std::sync::LazyLock<Mutex<RetainedLocations>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn retained_files_path(room: RoomId) -> PathBuf {
    runtime_dir()
        .join("tilo/retained-files")
        .join(room.to_string())
        .join("locations.json")
}

pub(crate) fn set_retained_files(room: RoomId, location: Option<(&str, &Path)>) -> bool {
    let location = location.map(|(operation, path)| (operation.to_owned(), path.to_path_buf()));
    let changed = {
        let mut locations = lock(&RETAINED_FILES);
        if locations.get(&room) == Some(&location) {
            false
        } else {
            locations.insert(room, location.clone());
            true
        }
    };
    if changed && let Err(error) = write_retained_files(room, location.as_ref()) {
        tracing::warn!(%room, %error, "Could not publish retained Files location");
    }
    changed
}

pub(crate) fn clear_retained_files() {
    let rooms: Vec<_> = lock(&RETAINED_FILES).keys().copied().collect();
    for room in rooms {
        set_retained_files(room, None);
    }
}

fn write_retained_files(
    room: RoomId,
    location: Option<&(String, PathBuf)>,
) -> std::io::Result<PathBuf> {
    let locations: Vec<_> = location
        .into_iter()
        .map(|(id, path)| serde_json::json!({"id":id,"path":path}))
        .collect();
    let bytes = serde_json::to_vec(&serde_json::json!({"schema":1,"locations":locations}))?;
    if bytes.len() > 16 * 1024 {
        return Err(std::io::Error::other(
            "Retained Files projection exceeds bounds",
        ));
    }
    let path = retained_files_path(room);
    tilo_files::native::atomic::write_private(&path, &bytes)?;
    Ok(path)
}

// Session authority comes only from the first-use helper's verified result.
// Kept outside the onboarding view so finishing setup does not revoke delivery.
static BROWSER_CONTINUITY: LazyLock<Mutex<Option<PathBuf>>> = LazyLock::new(|| Mutex::new(None));

pub(crate) fn set_browser_continuity(projection: Option<PathBuf>) {
    *lock(&BROWSER_CONTINUITY) = projection;
}

fn attach_browser_continuity(package_id: &str, command: &mut LaunchCommand) {
    if package_id != "so.tilo.browser" {
        return;
    }
    let projection = lock(&BROWSER_CONTINUITY).clone();
    if let Some(path) = projection
        && crate::onboarding_browser::validate_published_projection(&path)
    {
        command.args.push("--browser-continuity".into());
        command.args.push(path.display().to_string());
    }
}

fn attach_retained_files(
    app: &InstalledApp,
    room: RoomId,
    command: &mut LaunchCommand,
) -> std::io::Result<()> {
    if app.package_id != FILES_PACKAGE_ID {
        return Ok(());
    }
    let location = lock(&RETAINED_FILES).get(&room).cloned().flatten();
    let path = write_retained_files(room, location.as_ref())?;
    command.args.push("--retained-files".into());
    command.args.push(path.display().to_string());
    Ok(())
}

/// Write `grants` where `tilo-launcher --capabilities` will read them. Each launch owns a private immutable snapshot until its supervisor
/// drops the file. `Ok(None)` means no file or flag: manifest permissions alone.
fn write_capabilities_file(
    package_id: &str,
    grants: &LaunchGrants,
) -> std::io::Result<Option<tempfile::NamedTempFile>> {
    // Create at 0600 rather than writing and chmod-ing after: the window
    // between the two is exactly when another process could open it.
    use std::io::Write as _;

    // Reverse-DNS in principle, but this arrives from a registry file —
    // refuse anything that could escape the directory.
    if package_id.is_empty() || package_id.contains('/') || package_id.contains("..") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("refusing to write a capability file for package id {package_id:?}"),
        ));
    }
    if grants.is_empty() {
        return Ok(None);
    }
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(capabilities_dir())?;
    }
    let json = serde_json::to_vec(grants).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("serialising capabilities: {e}"),
        )
    })?;
    let mut file = tempfile::Builder::new()
        .prefix("launch-")
        .suffix(".json")
        .tempfile_in(capabilities_dir())?;
    // Not synced: the launcher reads it through the page cache, and the
    // runtime tmpfs does not outlive the session. This runs on the
    // compositor's loop for synchronous launches, where a sync under I/O
    // pressure froze the desktop.
    file.write_all(&json)?;
    Ok(Some(file))
}

/// `$XDG_RUNTIME_DIR`, falling back to `/run/user/<uid>`.
pub fn runtime_dir() -> PathBuf {
    tilo_paths::xdg::runtime_dir_or_default()
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
        system_store = ?package_store::layout::system_data_dir(),
        system_payload = ?package_store::layout::system_payload_dir(),
        user_store = ?package_store::layout::default_user_root(),
        "reading installed application registry"
    );
    tilo_installed_apps::read_from_env().map_err(|e| LaunchError::RegistryUnreadable {
        detail: e.to_string(),
    })
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
    let Ok(entries) = package_store::list_installed() else {
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

/// Installed packages, memoised for `APP_CACHE_TTL`. Errors are not
/// cached, so a transiently unreadable registry doesn't wedge the
/// launcher for the rest of the TTL.
///
/// # Errors
///
/// Returns an error if the cache is stale and re-reading the registry
/// fails — see [`read_installed_apps`].
pub fn installed_apps() -> Result<Vec<InstalledApp>, LaunchError> {
    installed_apps_snapshot().map(|(_, apps)| apps)
}

/// Resolve one canonical installed package without parsing every app's
/// manifest on the compositor loop. Keep the registry watch armed so a real
/// update invalidates the input-method service before its TTL fallback.
pub(crate) fn installed_app(
    catalog: &package_store::Catalog,
    app_id: &str,
) -> Result<Option<InstalledApp>, LaunchError> {
    let app = tilo_installed_apps::read_one(catalog, app_id).map_err(|e| {
        LaunchError::RegistryUnreadable {
            detail: e.to_string(),
        }
    });
    ensure_app_cache_watch();
    app
}

/// Pair records with their cache revision while holding the cache lock.
fn installed_apps_snapshot() -> Result<(u64, Vec<InstalledApp>), LaunchError> {
    {
        let cache = lock(&APP_CACHE);
        if let Some((apps, Some(at))) = cache.as_ref()
            && at.elapsed() < APP_CACHE_TTL
        {
            return Ok((APP_CACHE_REVISION.load(Ordering::Acquire), apps.clone()));
        }
    }
    let epoch = APP_CACHE_EPOCH.load(Ordering::Acquire);
    let apps = read_installed_apps()?;
    let generation = record_app_read(epoch, &apps);
    // Watch after the first memoize so opening the registry to fill the
    // cache cannot look like an install and wipe the snapshot we just wrote.
    ensure_app_cache_watch();
    Ok((generation, apps))
}

/// [`installed_apps`] for the compositor loop: the last enumeration, fresh
/// or stale, and never a registry read, which scans the registry and parses
/// every manifest. One past its TTL or invalidated by the registry watch is
/// refreshed on a worker and served until the refresh lands; before the
/// first enumeration there is none.
pub(crate) fn installed_apps_without_waiting() -> Vec<InstalledApp> {
    let (apps, fresh) = {
        let cache = lock(&APP_CACHE);
        cache.as_ref().map_or_else(
            || (Vec::new(), false),
            |(apps, at)| {
                (
                    apps.clone(),
                    at.is_some_and(|at| at.elapsed() < APP_CACHE_TTL),
                )
            },
        )
    };
    if !fresh {
        refresh_installed_apps_in_background();
    }
    apps
}

/// Re-read the registry on a worker, one refresh at a time.
fn refresh_installed_apps_in_background() {
    static REFRESHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if REFRESHING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("tilo-apps-refresh".into())
        .spawn(|| {
            if let Err(error) = installed_apps_snapshot() {
                tracing::debug!(%error, "installed apps refresh failed; the last list stays");
            }
            REFRESHING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        REFRESHING.store(false, Ordering::Release);
    }
}

/// Store a registry read that started at `read_epoch` as the new baseline.
fn record_app_read(read_epoch: u64, apps: &[InstalledApp]) -> u64 {
    let mut cache = lock(&APP_CACHE);
    // An invalidation (install, removal, watch event, repointed registry)
    // that raced this read means the read may predate it: keep it only as
    // a stale baseline so the next call re-reads instead of hiding the
    // change for a whole TTL.
    let fresh = (APP_CACHE_EPOCH.load(Ordering::Acquire) == read_epoch).then(Instant::now);
    let changed = cache.as_ref().is_none_or(|(cached, _)| cached != apps);
    // The TTL controls disk freshness, not search identity. Advancing this
    // generation for identical records invalidates both native and host app
    // indexes, making a just-discovered action disappear before invocation.
    if changed {
        APP_CACHE_REVISION.fetch_add(1, Ordering::Relaxed);
    }
    *cache = Some((apps.to_vec(), fresh));
    let generation = if fresh.is_some() {
        APP_CACHE_REVISION.load(Ordering::Acquire)
    } else {
        0
    };
    drop(cache);
    generation
}

/// Drop the memoised enumeration. Used after an install/remove and by
/// tests that repoint `TILO_APPS_DATA_DIR`.
pub fn invalidate_app_cache() {
    // Bump the epoch under the lock so a read finishing right after cannot
    // memoise itself as fresh.
    let mut cache = lock(&APP_CACHE);
    APP_CACHE_EPOCH.fetch_add(1, Ordering::Release);
    APP_CACHE_REVISION.fetch_add(1, Ordering::Relaxed);
    *cache = None;
}

/// Cheap installed-store invalidation token for session-owned services.
/// A changed token means callers must reread the canonical cached catalogue;
/// it is not an authorization or a package identity.
pub(crate) fn installed_apps_epoch() -> u64 {
    APP_CACHE_EPOCH.load(Ordering::Acquire)
}

fn expire_watched_app_cache() {
    let mut cache = lock(&APP_CACHE);
    APP_CACHE_EPOCH.fetch_add(1, Ordering::Release);
    if let Some((_, at)) = cache.as_mut() {
        *at = None;
    }
}

fn app_cache_roots() -> Vec<PathBuf> {
    [
        package_store::layout::default_user_root(),
        package_store::layout::system_data_dir(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn ensure_app_cache_watch() {
    let roots = app_cache_roots();
    if roots.is_empty() {
        return;
    }
    let mut watched = lock(&APP_CACHE_WATCH);
    if watched.as_ref() == Some(&roots) {
        return;
    }
    let watch_roots = roots.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    if std::thread::Builder::new()
        .name("tilo-apps-watch".into())
        .spawn(move || watch_app_cache_roots(&watch_roots, &ready_tx))
        .is_ok()
    {
        *watched = Some(roots);
        drop(watched);
        // A write that lands before the recursive watch is armed never
        // arrives; wait so the first fill is already subscribed.
        let _ = ready_rx.recv_timeout(Duration::from_secs(2));
    }
}

fn app_cache_event_matters(event: &notify::Event) -> bool {
    use notify::event::{EventKind, ModifyKind};
    match event.kind {
        EventKind::Modify(ModifyKind::Metadata(_))
        | EventKind::Access(_)
        | EventKind::Any
        | EventKind::Other => false,
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(_) => true,
    }
}

fn watch_app_cache_roots(roots: &[PathBuf], ready: &std::sync::mpsc::Sender<()>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok_and(|event| app_cache_event_matters(&event)) {
            let _ = tx.send(());
        }
    });
    let mut watcher = match watcher {
        Ok(watcher) => watcher,
        Err(error) => {
            tracing::warn!(%error, "app registry cannot be watched; falling back to TTL");
            let _ = ready.send(());
            return;
        }
    };
    for root in roots {
        if let Err(error) = watcher.watch(root, notify::RecursiveMode::Recursive) {
            tracing::info!(%error, path = %root.display(), "app registry path is not watched");
        }
    }
    // Keep `watcher` alive for the thread lifetime. Drop the walk that
    // `watch` itself performs so it does not look like a registry change.
    let _watcher = watcher;
    while rx.try_recv().is_ok() {}
    let _ = ready.send(());
    while rx.recv().is_ok() {
        while rx.try_recv().is_ok() {}
        expire_watched_app_cache();
    }
}

/// Immutable installed-app snapshot revision for retained search projections.
pub fn gui_apps_snapshot() -> (u64, Vec<InstalledApp>) {
    // Cache population advances the revision but does not invalidate the
    // records it just read. Only an invalidation racing the read stamps it
    // stale; the generation is paired under the cache lock.
    let before = gui_apps_generation();
    let (generation, mut apps) = installed_apps_snapshot().unwrap_or_else(|_| {
        let after = gui_apps_generation();
        (if before == after { before } else { 0 }, Vec::new())
    });
    apps.retain(|app| app.has_gui);
    apps.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    (generation, apps)
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

/// Who opens a resource: a Tilo package, or a host desktop entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceHandler {
    Package(InstalledApp),
    Host(crate::shell::host_apps::HostApp),
}

/// Resolve a resource over Tilo packages *and* host desktop entries, honouring
/// the host's `mimeapps.list` chain among host entries. Personal and
/// workspace choices may name either kind; without one a Tilo package wins.
pub fn handler_for_resource_with_policy(
    resource: &desktop_policy::ResourceKind,
    user_id: &str,
    workspace: Option<&desktop_policy::WorkspaceDefaults>,
) -> Option<(ResourceHandler, desktop_policy::defaults::SelectionSource)> {
    let apps = gui_apps();
    let hosts = crate::shell::host_apps::host_apps();
    let chain = desktop_policy::mimeapps::session_chain();
    let mut candidates = default_app_candidates_from(&apps);
    candidates.extend(host_app_candidates(&hosts, &chain));
    let personal = desktop_policy::PersonalDefaults::load();
    let selected = desktop_policy::DefaultAppResolver::new(&candidates)
        .with_system_defaults(&chain)
        .resolve(resource, user_id, &personal, workspace);
    // Keep host tools in step with what this resolution just saw; the write
    // is skipped when nothing changed and never delays the open.
    let published = resolved_host_defaults(&candidates, &chain, &personal);
    let _ = std::thread::Builder::new()
        .name("tilo-mimeapps-publish".to_string())
        .spawn(move || publish_host_defaults(&published));
    let selected = selected?;
    let source = selected.source;
    if let Some(desktop_id) = desktop_policy::host_desktop_id(&selected.package_id) {
        return hosts
            .into_iter()
            .find(|host| host.desktop_id == desktop_id)
            .map(|host| (ResourceHandler::Host(host), source));
    }
    apps.into_iter()
        .find(|app| app.package_id == selected.package_id)
        .map(|app| (ResourceHandler::Package(app), source))
}

/// Host desktop entries as policy candidates: their `MimeType=` claims plus
/// associations the `mimeapps.list` chain adds, minus those it removes.
pub fn host_app_candidates(
    hosts: &[crate::shell::host_apps::HostApp],
    chain: &desktop_policy::MimeAppsChain,
) -> Vec<desktop_policy::AppCandidate> {
    hosts
        .iter()
        .filter_map(|host| {
            let package_id = desktop_policy::host_app_id(&host.desktop_id);
            desktop_policy::defaults::validate_package_id(&package_id).ok()?;
            let declared: std::collections::BTreeSet<String> = host
                .mime_types
                .iter()
                .map(|mime| mime.trim().to_ascii_lowercase())
                .collect();
            let resources = declared
                .iter()
                .cloned()
                .chain(chain.added_types(&package_id))
                .filter(|mime| chain.handles(&package_id, mime, declared.contains(mime)))
                .filter_map(|mime| desktop_policy::ResourceKind::from_mimeapps_type(&mime))
                .collect();
            Some(desktop_policy::AppCandidate {
                package_id,
                display_name: host.name.clone(),
                resources,
                secondary: false,
            })
        })
        .collect()
}

/// Publish the host defaults this desktop resolves to
/// `$XDG_CONFIG_HOME/tilo-mimeapps.list`, so `xdg-open` and toolkits started
/// by host apps open files with the same application Tilo would. Called once
/// the session is up and again whenever a resolution runs.
pub fn publish_host_mimeapps() {
    let apps = gui_apps();
    let hosts = crate::shell::host_apps::host_apps();
    let chain = desktop_policy::mimeapps::session_chain();
    let mut candidates = default_app_candidates_from(&apps);
    candidates.extend(host_app_candidates(&hosts, &chain));
    publish_host_defaults(&resolved_host_defaults(
        &candidates,
        &chain,
        &desktop_policy::PersonalDefaults::load(),
    ));
}

fn publish_host_defaults(list: &desktop_policy::MimeAppsList) {
    let Some(path) = desktop_policy::mimeapps::published_list_path() else {
        return;
    };
    if let Err(error) = desktop_policy::mimeapps::publish(&path, list) {
        tracing::warn!(%error, path = %path.display(), "could not publish host default applications");
    }
}

/// Every MIME type (and scheme) a host entry claims, mapped to the host entry
/// the resolver picks for it from personal defaults. Types a Tilo package wins
/// are left out: there is no desktop file a host tool could start for them.
/// Workspace policy is not published — host tools have no workspace.
pub fn resolved_host_defaults(
    candidates: &[desktop_policy::AppCandidate],
    chain: &desktop_policy::MimeAppsChain,
    personal: &desktop_policy::PersonalDefaults,
) -> desktop_policy::MimeAppsList {
    let resolver = desktop_policy::DefaultAppResolver::new(candidates).with_system_defaults(chain);
    let claimed: std::collections::BTreeSet<&desktop_policy::ResourceKind> = candidates
        .iter()
        .filter(|candidate| candidate.is_host())
        .flat_map(|candidate| candidate.resources.iter())
        .collect();
    let mut list = desktop_policy::MimeAppsList::default();
    for resource in claimed {
        let (Some(key), Some(selected)) = (
            resource.mimeapps_type(),
            resolver.resolve(resource, "", personal, None),
        ) else {
            continue;
        };
        if desktop_policy::host_desktop_id(&selected.package_id).is_some() {
            list.defaults.insert(key, vec![selected.package_id]);
        }
    }
    list
}

pub fn app_for_resource_with_policy(
    resource: &desktop_policy::ResourceKind,
    user_id: &str,
    workspace: Option<&desktop_policy::WorkspaceDefaults>,
) -> Option<InstalledApp> {
    package_for_resource_with_policy(resource, user_id, workspace).map(|(app, _)| app)
}

/// [`app_for_resource_with_policy`], also saying why that package was chosen,
/// so a caller can tell a person's choice from a fallback.
pub fn package_for_resource_with_policy(
    resource: &desktop_policy::ResourceKind,
    user_id: &str,
    workspace: Option<&desktop_policy::WorkspaceDefaults>,
) -> Option<(InstalledApp, desktop_policy::defaults::SelectionSource)> {
    let apps = gui_apps();
    let candidates = default_app_candidates_from(&apps);
    let personal = desktop_policy::PersonalDefaults::load();
    let selected = desktop_policy::DefaultAppResolver::new(&candidates)
        .resolve(resource, user_id, &personal, workspace)?;
    apps.into_iter()
        .find(|app| app.package_id == selected.package_id)
        .map(|app| (app, selected.source))
}

/// Tilo packages and host desktop entries as policy candidates, host claims
/// adjusted by the session's `mimeapps.list` chain: everything "Open With"
/// may offer and a personal default may name.
pub fn resource_candidates() -> Vec<desktop_policy::AppCandidate> {
    let hosts = crate::shell::host_apps::host_apps();
    let chain = desktop_policy::mimeapps::session_chain();
    let mut candidates = default_app_candidates_from(&gui_apps());
    candidates.extend(host_app_candidates(&hosts, &chain));
    candidates
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

/// Whether the installed package `package_id` declares `capability` in its
/// signed manifest, re-read now: an upgrade that drops it stops what it
/// allowed at once, and a package that is gone declares nothing.
pub fn package_declares_capability(package_id: &str, capability: &str) -> bool {
    read_installed_apps().is_ok_and(|apps| {
        apps.iter()
            .any(|app| app.package_id == package_id && app.declares_capability(capability))
    })
}

/// This machine's applications, as the shared workspace settings name
/// them: running under this session, installed as packages, or host apps.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShellApps;

impl tilo_desktop::workspace_settings::AppDirectory for ShellApps {
    fn app_display_name(&self, app_id: &str) -> Option<String> {
        Some(display_name_for_app_id(app_id).unwrap_or_else(|| display_name_for_package_id(app_id)))
    }

    fn default_app_candidates(&self) -> Vec<desktop_policy::AppCandidate> {
        default_app_candidates()
    }
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
            secondary: app.opens_secondary,
        })
        .collect()
}

/// `so.tilo.text-editor` → `Text Editor`, derived from the trailing id
/// component since the manifest carries no display name. Lives in the
/// `launcher` crate too, so a package's name can't differ between
/// "offered" and "installed".
pub fn display_name_for_package_id(package_id: &str) -> String {
    package_store::display_name_for_package_id(package_id)
}

/// Order: `TILO_LAUNCHER_BIN` (a **developer-build** seam only — see
/// [`crate::compositor::dev_env::DEV_ENV_OVERRIDES`]), next to the running
/// compositor, then system paths. A shipped build must not let the
/// environment name the binary that builds every app's sandbox.
fn launcher_bin_candidates() -> Vec<PathBuf> {
    let current_exe = std::env::current_exe().ok();
    launcher_bin_candidates_from(
        crate::compositor::dev_env::DEV_ENV_OVERRIDES,
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
    out.push(PathBuf::from("/usr/libexec/tilo/tilo-launcher"));
    out.push(PathBuf::from("/usr/libexec/tilo-launcher"));
    out
}

/// Whether `path` is a link to a program of another name: on Tilo OS
/// `/usr/bin/tilo-launcher` is the old name, a link to `tilo`, which would
/// only run the real launcher in `/usr/libexec/tilo` with one more exec.
fn names_another_program(path: &Path) -> bool {
    path.is_symlink()
        && std::fs::canonicalize(path)
            .ok()
            .is_some_and(|target| target.file_name() != path.file_name())
}

/// First candidate that exists (and is the launcher, not a link to another
/// program), else the list we searched.
///
/// # Errors
///
/// Returns [`LaunchError::LauncherBinaryMissing`] if `tilo-launcher` is not
/// found at any candidate path.
pub fn resolve_launcher_bin() -> Result<PathBuf, LaunchError> {
    let candidates = launcher_bin_candidates();
    for candidate in &candidates {
        if candidate.exists() && !names_another_program(candidate) {
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

/// Build the sandboxed, display-less launch of one agent companion service
/// ([`AGENT_COMPANION_PACKAGE_IDS`]) in `room`. Like the agent it gets the
/// workspace's identity and no capability file; unlike the agent it is an
/// ordinary app connection, not a managed agent, so it publishes under its
/// own package id.
pub fn build_agent_companion_command(
    app: &InstalledApp,
    launcher_bin: &Path,
    room: RoomId,
    identity_path: Option<&Path>,
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
        },
    );
    command.args.push("--service".into());
    command.args.push("--".into());
    command
        .args
        .extend(["--workspace".into(), room.to_string()]);
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
    } = *extras;
    // One read of the package store per launch: the evidence and the surface
    // both need the installed copy, and reading the registry is most of
    // what preparing a launch costs.
    let installed = installed_copy(app);
    let application_creation = installed
        .as_ref()
        .and_then(|installed| original_application_creation(app, installed));
    if application_creation.is_none() {
        note_missing_creation_evidence(app);
    }
    if let Some(installed) = &installed {
        register_signed_surface(app, installed);
    }
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
    if let Some(evidence) = &application_creation {
        args.extend([
            "--expected-package-hash".into(),
            evidence.package_hash.clone(),
        ]);
    }
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
    let mut env = vec![(
        "XDG_RUNTIME_DIR".to_string(),
        runtime_dir().display().to_string(),
    )];
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
            allowed_compat_entry_ids: if app.package_id == "so.tilo.windows" {
                lock(&WINDOWS_COMPAT_ENTRY_IDS).clone()
            } else {
                Vec::new()
            },
            compat_entry_generation: windows_compat_entry_generation(),
            application_creation,
        }),
        portal_workspace_id: None,
        managed_agent_id: None,
        x11_workspace: None,
    }
}

/// Run the package `app` with the surface its signed package ships, so the
/// records of the person's it declares there are kept in their room as a
/// built-in app's are. A package whose surface no longer matches its
/// signature declares none.
fn register_signed_surface(app: &InstalledApp, installed: &package_store::InstalledPackage) {
    if let Some(surface) = package_store::integrity::signed_surface(installed) {
        tilo_action_broker::register_package_surface(&app.package_id, &surface);
    }
}

/// The package store's installed copy of `app`, when it is exactly the copy
/// being launched (the same manifest and payload). A launch of anything
/// else carries no evidence and declares no surface.
fn installed_copy(app: &InstalledApp) -> Option<package_store::InstalledPackage> {
    let installed = package_store::find_installed(&app.package_id).ok()??;
    (installed.manifest_path == app.manifest_path && installed.payload_root == app.payload_root)
        .then_some(installed)
}

/// What [`original_application_creation`] verified, by the exact inputs it
/// verified: the package (its signed root's hash), where it is installed, and
/// the BLAKE3 of the installed manifest. The signed root names the manifest's
/// hash and the package hash names the signed root, so an entry can never come
/// to mean anything else; a reinstall or an edited manifest is a new key.
type CreationKey = (String, String, PathBuf, PathBuf, [u8; 32]);
/// Only what verified is kept: a failed read is tried again next launch.
static CREATION_EVIDENCE: LazyLock<
    Mutex<HashMap<CreationKey, tilo_action_broker::services::ApplicationCreationEvidence>>,
> = LazyLock::new(Mutex::default);
/// Entries kept before the cache starts over: one per installed version.
const CREATION_EVIDENCE_MAX: usize = 256;

/// The evidence a launch carries that `app` is the signed original: read the
/// installed manifest and name the installed package on every launch, and
/// verify the signed record and parse the manifest only the first time these
/// exact bytes are launched from this installation.
fn original_application_creation(
    app: &InstalledApp,
    installed: &package_store::InstalledPackage,
) -> Option<tilo_action_broker::services::ApplicationCreationEvidence> {
    let manifest = std::fs::read(&app.manifest_path).ok()?;
    let key = (
        app.package_id.clone(),
        installed.package_id_hex.clone(),
        installed.manifest_path.clone(),
        installed.payload_root.clone(),
        *blake3::hash(&manifest).as_bytes(),
    );
    cached_creation_evidence(key, || {
        verify_original_application_creation(app, installed, &manifest)
    })
}

/// Why `app`'s launch carries no creation evidence: the first link of
/// [`original_application_creation`] that does not hold. Diagnostic only;
/// it decides nothing.
fn creation_evidence_gap(app: &InstalledApp) -> String {
    let installed = match package_store::find_installed(&app.package_id) {
        Ok(Some(installed)) => installed,
        Ok(None) => return "the package store holds no installed copy".into(),
        Err(error) => return format!("the package store cannot be read: {error}"),
    };
    if installed.manifest_path != app.manifest_path || installed.payload_root != app.payload_root {
        return format!(
            "the launched copy ({}, {}) is not the installed one ({}, {})",
            app.manifest_path.display(),
            app.payload_root.display(),
            installed.manifest_path.display(),
            installed.payload_root.display()
        );
    }
    let Ok(manifest) = std::fs::read(&app.manifest_path) else {
        return "the installed manifest cannot be read".into();
    };
    let dir = package_store::integrity::record_dir(&installed);
    let record =
        match package_store::integrity::read_record(&dir, &installed.id, &installed.package_id_hex)
        {
            Ok(record) => record,
            Err(error) => return format!("the signed record in {} fails: {error}", dir.display()),
        };
    if manifest != record.manifest_bytes {
        return "the installed manifest differs from the signed one".into();
    }
    let declared = std::str::from_utf8(&record.manifest_bytes)
        .ok()
        .and_then(|text| toml::from_str::<toml::Value>(text).ok())
        .and_then(|manifest| {
            manifest
                .get("resources")?
                .get("kinds")?
                .as_array()
                .map(Vec::len)
        });
    match declared {
        None => "the signed manifest declares no resource kinds".into(),
        Some(kinds) => format!("the signed manifest's {kinds} resource kinds do not verify"),
    }
}

/// Log once per package and session why its launches carry no creation
/// evidence: such an app cannot create its record in a committed workspace.
fn note_missing_creation_evidence(app: &InstalledApp) {
    static NOTED: LazyLock<Mutex<std::collections::HashSet<String>>> =
        LazyLock::new(Mutex::default);
    if !lock(&NOTED).insert(app.package_id.clone()) {
        return;
    }
    tracing::error!(
        target: "tilo::app_launch",
        package_id = %app.package_id,
        reason = %creation_evidence_gap(app),
        "launch carries no original-package creation evidence"
    );
}

/// `verify`'s evidence for `key`, verified once while it holds.
fn cached_creation_evidence(
    key: CreationKey,
    verify: impl FnOnce() -> Option<tilo_action_broker::services::ApplicationCreationEvidence>,
) -> Option<tilo_action_broker::services::ApplicationCreationEvidence> {
    if let Some(evidence) = lock(&CREATION_EVIDENCE).get(&key) {
        return Some(evidence.clone());
    }
    let evidence = verify()?;
    let mut cache = lock(&CREATION_EVIDENCE);
    if cache.len() >= CREATION_EVIDENCE_MAX {
        cache.clear();
    }
    cache.insert(key, evidence.clone());
    drop(cache);
    Some(evidence)
}

fn verify_original_application_creation(
    app: &InstalledApp,
    installed: &package_store::InstalledPackage,
    manifest: &[u8],
) -> Option<tilo_action_broker::services::ApplicationCreationEvidence> {
    let record = package_store::integrity::read_record(
        &package_store::integrity::record_dir(installed),
        &installed.id,
        &installed.package_id_hex,
    )
    .ok()?;
    if manifest != record.manifest_bytes {
        return None;
    }
    let original = std::str::from_utf8(&record.manifest_bytes).ok()?;
    let parsed: launcher::manifest::ManifestView = toml::from_str(original).ok()?;
    if parsed.package_id != app.package_id {
        return None;
    }
    let manifest: toml::Value = toml::from_str(original).ok()?;
    let kinds = manifest.get("resources")?.get("kinds")?.as_array()?;
    let evidence = tilo_action_broker::services::ApplicationCreationEvidence {
        package_hash: installed.package_id_hex.clone(),
        declared_kinds: kinds
            .iter()
            .map(|kind| kind.as_str().map(str::to_owned))
            .collect::<Option<_>>()?,
    };
    evidence.valid().then_some(evidence)
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
/// workspace. This is the native agent path: the signed package is
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
    if !workspace
        .agent_members()
        .iter()
        .any(|(agent, _)| agent == agent_id)
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
    if !is_headless_collab_service(&app)? {
        return Err(LaunchError::NotAManagedService {
            package_id: app.package_id,
        });
    }
    execute_workspace_agent(app, agent_id, workspace)
}

/// Whether `app`'s signed manifest describes a headless service that needs
/// the collaboration broker: `kind = "service"`, a `cli` entrypoint and a
/// required `collab` service — the only shape started without a window.
fn is_headless_collab_service(app: &InstalledApp) -> Result<bool, LaunchError> {
    let manifest = launcher::manifest::read_manifest(&app.manifest_path).map_err(|error| {
        LaunchError::RegistryUnreadable {
            detail: format!("reading the {} manifest: {error}", app.package_id),
        }
    })?;
    let requires_collaboration = manifest
        .sandbox
        .services
        .get(launcher::services::COLLAB_SERVICE)
        .is_some_and(launcher::manifest::ServiceReq::is_required);
    Ok(manifest.kind == launcher::manifest::KIND_SERVICE
        && manifest.entrypoint.cli_bin().is_some()
        && requires_collaboration)
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
        &workspace.room().to_string(),
    )
    .map_err(|error| LaunchError::IdentityUnavailable {
        detail: error.to_string(),
    })?;
    // Companions first, and named to the agent, so it plans its first run
    // only once their actions are published instead of without them.
    let companions =
        start_agent_companions(&launcher_bin, workspace.room(), identity_path.as_deref());
    let mut command = build_workspace_agent_command(
        &app,
        &launcher_bin,
        workspace.room(),
        identity_path.as_deref(),
        agent_id,
    );
    for companion in companions {
        command
            .args
            .extend(["--companion".into(), companion.into()]);
    }
    if let Some(provider) = workspace_agent_provider(agent_id) {
        command.args.extend(["--provider".into(), provider]);
    }
    if let Err(detail) = spawn_detached(app.clone(), command, None) {
        return Err(LaunchError::AgentLaunchWorkerUnavailable { detail });
    }
    Ok(app)
}

/// Start the [`AGENT_COMPANION_PACKAGE_IDS`] not already running in `room`,
/// and answer the companions running there now: started, or already up.
/// A companion that is missing, malformed or fails to start costs the agent
/// its actions, not its launch, so failures are logged and skipped.
#[cfg(target_os = "linux")]
fn start_agent_companions(
    launcher_bin: &Path,
    room: RoomId,
    identity_path: Option<&Path>,
) -> Vec<&'static str> {
    let apps = match installed_apps() {
        Ok(apps) => apps,
        Err(error) => {
            tracing::warn!(%error, "cannot list installed apps for the agent's companions");
            return Vec::new();
        }
    };
    let room_id = room.to_string();
    let mut running = Vec::new();
    for package_id in AGENT_COMPANION_PACKAGE_IDS {
        let Some(app) = apps.iter().find(|app| app.package_id == *package_id) else {
            continue;
        };
        if verified_workspaces_for_app(package_id).contains(&room_id) {
            running.push(*package_id);
            continue;
        }
        match is_headless_collab_service(app) {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(
                    package_id,
                    "agent companion is not a headless broker service"
                );
                continue;
            }
            Err(error) => {
                tracing::warn!(package_id, %error, "cannot read the agent companion's manifest");
                continue;
            }
        }
        let command = build_agent_companion_command(app, launcher_bin, room, identity_path);
        match spawn_detached(app.clone(), command, None) {
            Ok(()) => running.push(*package_id),
            Err(detail) => {
                tracing::warn!(package_id, %detail, "could not start the agent companion");
            }
        }
    }
    running
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

/// The Tilo terminal, which hosts `Terminal=true` desktop entries.
pub const TERMINAL_PACKAGE_ID: &str = "so.tilo.terminal";

/// Launch context the terminal reads to run one program instead of a login
/// shell. Carried in the launcher's bounded JSON-object context channel.
pub fn terminal_command_context(argv: &[String]) -> String {
    serde_json::json!({ "terminal_command": argv }).to_string()
}

/// Run a console program's argv in the Tilo terminal, in `room`. The program
/// runs inside the terminal's confined guest, never directly on the host.
///
/// # Errors
///
/// Returns an error when `argv` is empty or too large for the launch
/// context, or under the same conditions as [`launch_with_document`].
pub fn launch_terminal_command(argv: &[String], room: RoomId) -> Result<InstalledApp, LaunchError> {
    let context = terminal_command_context(argv);
    if argv.is_empty() || context.len() > 4 * 1024 {
        return Err(LaunchError::HostAppRefused {
            desktop_id: argv.first().cloned().unwrap_or_default(),
            detail: "the command does not fit a terminal launch".to_string(),
        });
    }
    let app = installed_apps()?
        .into_iter()
        .find(|a| a.package_id == TERMINAL_PACKAGE_ID)
        .ok_or_else(|| LaunchError::NotInstalled {
            package_id: TERMINAL_PACKAGE_ID.to_string(),
        })?;
    execute_installed_with_context(app, room, context)
}

/// [`launch_terminal_command`] for the compositor thread: the argv is
/// validated here, and everything that touches the registry or the disk runs
/// on a preflight worker. A refusal after that point arrives as a launch
/// event.
///
/// # Errors
///
/// Returns an error when `argv` is empty or too large for the launch context,
/// or when the preflight worker cannot be started.
pub(crate) fn queue_terminal_command(argv: &[String], room: RoomId) -> Result<(), LaunchError> {
    let context = terminal_command_context(argv);
    if argv.is_empty() || context.len() > 4 * 1024 {
        return Err(LaunchError::HostAppRefused {
            desktop_id: argv.first().cloned().unwrap_or_default(),
            detail: "the command does not fit a terminal launch".to_string(),
        });
    }
    spawn_reporting(
        std::thread::Builder::new().name("tilo-launch-preflight".to_string()),
        super::launch_readiness::inherit(move || {
            let outcome = installed_apps().and_then(|apps| {
                let app = apps
                    .into_iter()
                    .find(|a| a.package_id == TERMINAL_PACKAGE_ID)
                    .ok_or_else(|| LaunchError::NotInstalled {
                        package_id: TERMINAL_PACKAGE_ID.to_string(),
                    })?;
                execute_installed_with_context(app, room, context)
            });
            if let Err(error) = outcome {
                super::launch_readiness::fail_current(error.user_message());
                push_event(LaunchEvent {
                    package_id: TERMINAL_PACKAGE_ID.to_string(),
                    display_name: display_name_for_package_id(TERMINAL_PACKAGE_ID),
                    managed_agent_id: None,
                    managed_workspace_id: None,
                    kind: LaunchEventKind::SpawnFailed {
                        detail: error.user_message(),
                        reason: error.failure(),
                    },
                });
            }
        }),
    )
    .map_err(|error| LaunchError::AgentLaunchWorkerUnavailable {
        detail: error.to_string(),
    })?;
    Ok(())
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
    let apps = installed_apps()?;
    let app = apps
        .iter()
        .find(|a| a.package_id == package_id)
        .cloned()
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

fn execute_installed_with_context(
    app: InstalledApp,
    room: RoomId,
    context: String,
) -> Result<InstalledApp, LaunchError> {
    execute_installed_inner(
        app,
        room,
        None,
        None,
        LaunchContext::Raw(context),
        None,
        None,
        None,
    )
}

/// The launcher's typed context channel: a validated tool intent, or a
/// shell-built context for one specific package.
enum LaunchContext<'a> {
    #[cfg(target_os = "linux")]
    SelectedOpen(&'a Arc<retained_home::selected_target::SelectedContinuation>),
    None,
    Tool(&'a ToolIntent),
    Raw(String),
}

/// A launch to run again once its app is updated: everything
/// [`execute_installed_inner`] was asked, owned.
struct Replay {
    room: RoomId,
    document: Option<PathBuf>,
    uri: Option<String>,
    context: ReplayContext,
    media_handoff: Option<MediaHandoffLaunch>,
    capability_snapshot: Option<LaunchGrants>,
    native_owner: Option<super::native_workspace_owner::NativeWorkspaceOwner>,
}

enum ReplayContext {
    #[cfg(target_os = "linux")]
    SelectedOpen(Arc<retained_home::selected_target::SelectedContinuation>),
    None,
    Tool(ToolIntent),
    Raw(String),
}

impl ReplayContext {
    fn of(context: &LaunchContext<'_>) -> Self {
        match context {
            #[cfg(target_os = "linux")]
            LaunchContext::SelectedOpen(selected) => Self::SelectedOpen(Arc::clone(selected)),
            LaunchContext::None => Self::None,
            LaunchContext::Tool(intent) => Self::Tool((*intent).clone()),
            LaunchContext::Raw(raw) => Self::Raw(raw.clone()),
        }
    }
}

impl Replay {
    fn start(self, app: InstalledApp) -> Result<InstalledApp, LaunchError> {
        let context = match &self.context {
            #[cfg(target_os = "linux")]
            ReplayContext::SelectedOpen(selected) => LaunchContext::SelectedOpen(selected),
            ReplayContext::None => LaunchContext::None,
            ReplayContext::Tool(intent) => LaunchContext::Tool(intent),
            ReplayContext::Raw(raw) => LaunchContext::Raw(raw.clone()),
        };
        execute_installed_inner(
            app,
            self.room,
            self.document.as_deref(),
            self.uri.as_deref(),
            context,
            self.media_handoff,
            self.capability_snapshot,
            self.native_owner,
        )
    }
}

/// Update `app`, which is behind this system, then start `replay` with the
/// build that fits. When the store has none yet, the app waits for its
/// developer and does not start; when the update cannot be fetched, the
/// place says so and offers to try again.
///
/// `recorded` is the open's use being written to the store: the update waits
/// for it, because the size sweep after the update evicts the app opened
/// least recently, and the update keeps the app's last use.
fn update_then_start(
    app: InstalledApp,
    replay: Replay,
    recorded: Option<std::thread::JoinHandle<()>>,
) {
    let package_id = app.package_id.clone();
    let first = move || {
        if let Some(recorded) = recorded {
            let _ = recorded.join();
        }
    };
    crate::shell::app_versions::update_after(&package_id, first, move |outcome| {
        let fail = |reason: LaunchFailure, detail: String| {
            push_event(LaunchEvent {
                package_id: app.package_id.clone(),
                display_name: app.display_name.clone(),
                managed_agent_id: None,
                managed_workspace_id: None,
                kind: LaunchEventKind::SpawnFailed { reason, detail },
            });
        };
        let fresh = match installed_apps() {
            Ok(apps) => apps
                .into_iter()
                .find(|installed| installed.package_id == app.package_id),
            Err(error) => {
                fail(error.failure(), error.user_message());
                return;
            }
        };
        let Some(fresh) = fresh else {
            fail(
                LaunchFailure::NotInstalled,
                format!("{} is no longer installed", app.package_id),
            );
            return;
        };
        match (
            crate::shell::app_versions::fit(&fresh.manifest_path),
            outcome,
        ) {
            (crate::shell::app_versions::Fit::Fits, _) => {
                if let Err(error) = replay.start(fresh) {
                    fail(error.failure(), error.user_message());
                }
            }
            (
                crate::shell::app_versions::Fit::AppBehind(unmet),
                crate::shell::app_versions::UpdateOutcome::Updated,
            ) => fail(
                LaunchFailure::WaitingForDeveloper,
                format!(
                    "no build of {} fits this system yet: {}",
                    app.package_id,
                    registry_types::interfaces::describe_unmet(&unmet)
                ),
            ),
            (
                crate::shell::app_versions::Fit::AppBehind(_),
                crate::shell::app_versions::UpdateOutcome::Failed(detail),
            ) => fail(LaunchFailure::UpdateFailed, detail),
            (crate::shell::app_versions::Fit::SystemBehind(unmet), _) => {
                crate::shell::app_versions::update_system_now();
                fail(
                    LaunchFailure::SystemBehind,
                    registry_types::interfaces::describe_unmet(&unmet),
                );
            }
        }
    });
}

fn execute_installed(
    app: InstalledApp,
    room: RoomId,
    document: Option<&Path>,
    uri: Option<&str>,
    tool_intent: Option<&ToolIntent>,
    media_handoff: Option<MediaHandoffLaunch>,
    capability_snapshot: Option<LaunchGrants>,
) -> Result<InstalledApp, LaunchError> {
    execute_installed_inner(
        app,
        room,
        document,
        uri,
        tool_intent.map_or(LaunchContext::None, LaunchContext::Tool),
        media_handoff,
        capability_snapshot,
        None,
    )
}

pub(crate) fn stage_launch_capabilities(
    app_id: &str,
    grants: &mut LaunchGrants,
) -> Option<tempfile::NamedTempFile> {
    // Exclude authority already expired before staging from both the child's
    // input and its supervisor's snapshot. Otherwise reopening after expiry
    // would be refused rather than launch with the remaining permissions.
    // Expiry after this point still refuses a pending launch or interrupts it.
    let now = Some(tilo_clock::unix_now_ms() / 1000);
    grants.sandbox.retain(|cap| cap.is_live_at(now));
    match write_capabilities_file(app_id, grants) {
        Ok(path) => path,
        Err(error) => {
            eprintln!(
                "[launch {app_id}] could not stage {} capabilities ({error}); starting with manifest permissions only",
                grants.sandbox.len()
            );
            None
        }
    }
}

fn attach_sandbox_authority(
    app: &InstalledApp,
    room: RoomId,
    command: &mut LaunchCommand,
    grants: LaunchGrants,
    file: Option<tempfile::NamedTempFile>,
) -> Result<Option<action_authority::Lease>, LaunchError> {
    let requires_supervision = file.is_some();
    let applied = if requires_supervision {
        grants.sandbox
    } else {
        Vec::new()
    };
    match action_authority::attach_optional(&app.package_id, room, &mut command.args) {
        Some(lease) => Ok(Some(lease.with_sandbox_capabilities(applied, file))),
        None if requires_supervision => Err(LaunchError::RegistryUnreadable {
            detail: crate::shell::i18n::t("launch-error-sandbox-supervision"),
        }),
        None => Ok(None),
    }
}

fn execute_installed_inner(
    app: InstalledApp,
    room: RoomId,
    document: Option<&Path>,
    uri: Option<&str>,
    context: LaunchContext<'_>,
    media_handoff: Option<MediaHandoffLaunch>,
    capability_snapshot: Option<LaunchGrants>,
    native_owner: Option<super::native_workspace_owner::NativeWorkspaceOwner>,
) -> Result<InstalledApp, LaunchError> {
    // Opening it is a use, whichever window it opens in, and whether or not
    // it must update first.
    let recorded = recorded_use::record(&app);
    // A build speaks exactly the interface versions this system serves, or
    // it does not start: one that is behind is updated first and starts
    // after, in the place held for it now.
    match crate::shell::app_versions::fit(&app.manifest_path) {
        crate::shell::app_versions::Fit::Fits => {}
        crate::shell::app_versions::Fit::AppBehind(unmet) => {
            tracing::info!(
                package_id = %app.package_id,
                unmet = %registry_types::interfaces::describe_unmet(&unmet),
                "updating an app that is behind this system before it starts"
            );
            update_then_start(
                app.clone(),
                Replay {
                    room,
                    document: document.map(Path::to_path_buf),
                    uri: uri.map(str::to_owned),
                    context: ReplayContext::of(&context),
                    media_handoff,
                    capability_snapshot,
                    native_owner,
                },
                recorded,
            );
            return Ok(app);
        }
        crate::shell::app_versions::Fit::SystemBehind(unmet) => {
            crate::shell::app_versions::update_system_now();
            return Err(LaunchError::SystemBehind {
                package_id: app.package_id,
                unmet: registry_types::interfaces::describe_unmet(&unmet),
            });
        }
    }
    launch_fitting(
        app,
        room,
        (document, uri),
        context,
        media_handoff,
        capability_snapshot,
        native_owner,
    )
}

/// [`execute_installed_inner`] for an app whose build speaks this system's
/// interface versions.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the selected open is moved into its dispatch at its last use"
)]
fn launch_fitting(
    app: InstalledApp,
    room: RoomId,
    (document, uri): (Option<&Path>, Option<&str>),
    context: LaunchContext<'_>,
    media_handoff: Option<MediaHandoffLaunch>,
    capability_snapshot: Option<LaunchGrants>,
    native_owner: Option<super::native_workspace_owner::NativeWorkspaceOwner>,
) -> Result<InstalledApp, LaunchError> {
    // Nothing named to open: the app's own window.
    #[cfg(target_os = "linux")]
    let plain = document.is_none() && uri.is_none() && matches!(&context, LaunchContext::None);
    #[cfg(target_os = "linux")]
    let retained = if plain {
        retained_home::FilesHomeLaunch::select(&app, room, native_owner.as_ref()).map_err(
            |error| LaunchError::IdentityUnavailable {
                detail: error.to_string(),
            },
        )?
    } else {
        None
    };
    #[cfg(not(target_os = "linux"))]
    drop(native_owner);
    #[cfg(target_os = "linux")]
    let selected_open = match &context {
        LaunchContext::SelectedOpen(selected) => Some((*selected).clone()),
        _ => None,
    };
    // Another window of an app already running in the room opens in its
    // process; the first one starts the process, reserving its door.
    #[cfg(target_os = "linux")]
    let Ok(door) = window_doors::route_or_reserve(
        &app,
        room,
        plain && media_handoff.is_none() && retained.is_none(),
        &|| {
            window_alone(
                &app,
                room,
                capability_snapshot.as_ref(),
                native_owner.as_ref(),
            )
        },
    ) else {
        return Ok(app);
    };
    tracing::info!(target: "tilo::launch_probe", package_id = %app.package_id, "launch preflight begin");
    tilo_startup_metrics::launch_phase(&app.package_id, "preflight");
    let socket = wayland_socket().ok_or(LaunchError::NoDisplay)?;
    let launcher_bin = selected_execution_program()?;
    tracing::info!(target: "tilo::launch_probe", package_id = %app.package_id, "launcher and display resolved");

    let (capabilities, capabilities_file) = launch_capabilities(&app, room, capability_snapshot);
    let mut command = launch_command(
        &app,
        (launcher_bin.as_path(), socket.as_path()),
        room,
        capabilities_file.as_ref(),
        (document, uri),
    )?;
    stage_continuity(&app, room, &mut command);
    apply_launch_context(context, (document, uri), &mut command)?;
    if let Some(handoff) = media_handoff {
        attach_media_handoff(&mut command, handoff);
    }
    stage_trusted_identities_projection(&app, room, &mut command);
    #[cfg(target_os = "linux")]
    let retained_projection = if let Some(retained) = &retained {
        Some(retained.attach_projection(&mut command).map_err(|error| {
            LaunchError::RegistryUnreadable {
                detail: error.to_string(),
            }
        })?)
    } else {
        None
    };
    #[cfg(target_os = "linux")]
    let ordinary_projection = retained.is_none() && selected_open.is_none();
    #[cfg(not(target_os = "linux"))]
    let ordinary_projection = true;
    if ordinary_projection {
        attach_retained_files(&app, room, &mut command).map_err(|error| {
            LaunchError::RegistryUnreadable {
                detail: format!("Could not stage retained Files projection: {error}"),
            }
        })?;
    }
    let authority_lease =
        attach_sandbox_authority(&app, room, &mut command, capabilities, capabilities_file)?;
    tracing::info!(target: "tilo::launch_probe", package_id = %app.package_id, "governance projection staged");
    tilo_startup_metrics::launch_phase(&app.package_id, "authority");
    remember_launch(&app);
    #[cfg(target_os = "linux")]
    let launched = if let Some(selected) = selected_open {
        selected
            .dispatch(app.clone(), command, authority_lease, native_owner)
            .map_err(|error| error.to_string())
    } else {
        match retained {
            Some(retained) => retained
                .dispatch(app.clone(), command, authority_lease, retained_projection)
                .map_err(|error| error.to_string()),
            None => spawn_detached(app.clone(), command, authority_lease),
        }
    };
    #[cfg(not(target_os = "linux"))]
    let launched = spawn_detached(app.clone(), command, authority_lease);
    launched.map_err(|detail| LaunchError::AgentLaunchWorkerUnavailable { detail })?;
    #[cfg(target_os = "linux")]
    window_doors::launched(door);
    tracing::info!(target: "tilo::launch_probe", package_id = %app.package_id, "launch worker queued");
    Ok(app)
}

/// Start a window of `app` in `room` as its launch would have, in a process of
/// its own: for an app that does not open the windows the shell asks for in
/// its running process (`window_doors`), built before the window door.
#[cfg(target_os = "linux")]
fn window_alone(
    app: &InstalledApp,
    room: RoomId,
    capability_snapshot: Option<&LaunchGrants>,
    native_owner: Option<&super::native_workspace_owner::NativeWorkspaceOwner>,
) -> window_doors::StartAlone {
    let (app, capability_snapshot, native_owner) = (
        app.clone(),
        capability_snapshot.cloned(),
        native_owner.cloned(),
    );
    Box::new(move || {
        let package_id = app.package_id.clone();
        if let Err(error) = launch_fitting(
            app,
            room,
            (None, None),
            LaunchContext::None,
            None,
            capability_snapshot,
            native_owner,
        ) {
            tracing::warn!(%error, %package_id, "a window of an app built before the window door did not start");
        }
    })
}

/// Stage the governance identity projection for `app` in `room`; without
/// it Files shows an empty directory, and the launch goes on.
fn stage_trusted_identities_projection(
    app: &InstalledApp,
    room: RoomId,
    command: &mut LaunchCommand,
) {
    if let Err(error) = attach_trusted_identities_projection(app, room, command) {
        eprintln!(
            "[launch {}] could not stage the governance identity projection ({error}); \
             Files will use an empty directory",
            app.package_id
        );
    }
}

/// The command that starts `app` in `room` through `launcher_bin` on the
/// display `socket`, with the identity the room selects.
fn launch_command(
    app: &InstalledApp,
    (launcher_bin, socket): (&Path, &Path),
    room: RoomId,
    capabilities_file: Option<&tempfile::NamedTempFile>,
    (document, uri): (Option<&Path>, Option<&str>),
) -> Result<LaunchCommand, LaunchError> {
    let identity_path = crate::identity_registry::selected_identity_path(&room.to_string())
        .map_err(|error| LaunchError::IdentityUnavailable {
            detail: error.to_string(),
        })?;
    tracing::info!(target: "tilo::launch_probe", package_id = %app.package_id, "identity selection resolved");
    tilo_startup_metrics::launch_phase(&app.package_id, "identity");

    let command = build_launch_command_for(
        app,
        launcher_bin,
        Some(socket),
        room,
        &LaunchExtras {
            capabilities_file: capabilities_file.map(tempfile::NamedTempFile::path),
            identity_path: identity_path.as_deref(),
            render_node: crate::compositor::render_node::resolved(),
            document,
            uri,
        },
    );
    tilo_startup_metrics::launch_phase(&app.package_id, "command");
    Ok(command)
}

/// The capabilities `app` launches with in `room`, from `snapshot` when the
/// launch carries one, plus what the person was just asked for, and the
/// file they are staged in for the launcher.
fn launch_capabilities(
    app: &InstalledApp,
    room: RoomId,
    snapshot: Option<LaunchGrants>,
) -> (LaunchGrants, Option<tempfile::NamedTempFile>) {
    // A failure to write the capability file is not a launch failure —
    // the app starts with manifest-only permissions, the safe direction.
    let mut capabilities = snapshot.map_or_else(
        || capabilities_for_app(&app.package_id),
        |grants| grants.for_app(&app.package_id),
    );
    if let Some(ask) = LAUNCH_PERMISSIONS.get() {
        let added = ask(app, room, &capabilities);
        capabilities.sandbox.extend(added);
    }
    let capabilities_file = stage_launch_capabilities(&app.package_id, &mut capabilities);
    (capabilities, capabilities_file)
}

/// Give the launch its continuity instance: the record a restored tab
/// reconnects to, or a fresh one, and the state it resumes from.
fn stage_continuity(app: &InstalledApp, room: RoomId, command: &mut LaunchCommand) {
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
        format!("/run/tilo/app/.local/state/startup-{continuity_instance}.json"),
    ));
    // An unattended session's apps save where it was told to.
    if let Some(directory) = std::env::var_os(ui::portal::SAVE_DIRECTORY_ENV) {
        command.env.push((
            ui::portal::SAVE_DIRECTORY_ENV.to_string(),
            directory.to_string_lossy().into_owned(),
        ));
    }
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
}

/// The instance of a window of `app` in `room` that opens in its running
/// process (`window_doors`): a restored tab's own, else a fresh one, noted
/// for the tab its window claims, as [`stage_continuity`] notes a launch's.
#[cfg(target_os = "linux")]
fn window_instance(app: &InstalledApp, room: RoomId) -> u64 {
    let instance = take_staged_resume_for_package(&app.package_id, room)
        .map_or_else(crate::shell::state::next_id, |(_, instance)| instance);
    attach_continuity_instance(&app.package_id, instance);
    instance
}

/// Pass the launch's context to the app: the selected thing to open, a tool
/// intent or its raw encoding.
fn apply_launch_context(
    context: LaunchContext<'_>,
    (document, uri): (Option<&Path>, Option<&str>),
    command: &mut LaunchCommand,
) -> Result<(), LaunchError> {
    match context {
        #[cfg(target_os = "linux")]
        LaunchContext::SelectedOpen(selected) => {
            if document.is_some() || uri.is_some() {
                return Err(LaunchError::RegistryUnreadable {
                    detail: "selected open cannot borrow a host path or URI".into(),
                });
            }
            command
                .args
                .extend(["--open".into(), selected.destination().into()]);
        }
        LaunchContext::None => {}
        LaunchContext::Tool(intent) => {
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
        LaunchContext::Raw(encoded) => command.env.push((TOOL_CONTEXT_ENV.to_string(), encoded)),
    }
    Ok(())
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
///
/// # Errors
///
/// Returns [`LaunchError::AgentLaunchWorkerUnavailable`] when the preflight
/// worker thread cannot be started. Later failures arrive as launch events.
pub fn queue_launch_in_workspace(
    app: InstalledApp,
    workspace: &Workspace,
) -> Result<(), LaunchError> {
    queue_launch_target_in_workspace(app, workspace, None)
}

/// [`queue_launch_in_workspace`], asking the app to open `uri` when given —
/// a link an app opened in a new tab. The app's manifest must claim the
/// URI's scheme; the launcher refuses one it does not.
///
/// # Errors
///
/// As [`queue_launch_in_workspace`].
pub fn queue_launch_target_in_workspace(
    app: InstalledApp,
    workspace: &Workspace,
    uri: Option<String>,
) -> Result<(), LaunchError> {
    let grants = workspace.launch_grants(tilo_clock::unix_now());
    queue_prepared_launch(
        app,
        workspace.room(),
        move || Ok(grants),
        QueuedOpen::uri(uri),
        None,
    )
}

/// A retained host notification resolves its verified package on the same
/// preflight worker as other launches. It carries its original room snapshot,
/// and never adds capabilities or app authorship while opening its URI.
pub(crate) fn queue_package_target_in_workspace(
    package_id: String,
    workspace: &Workspace,
    uri: String,
) -> Result<(), LaunchError> {
    queue_package_open(package_id, None, workspace, None, Some(uri))
}

/// Open `document` or `uri` (or just the app) in the package `package_id`
/// into `workspace`, resolving the package on the preflight worker. For
/// callers on the compositor thread that hold an app id and not an
/// [`InstalledApp`]; a refusal arrives as a launch event, named
/// `display_name` when the caller knows what the person asked to open (a
/// package that is no longer installed has no registry name to fall back
/// on), else the package's own display name.
///
/// # Errors
///
/// Returns [`LaunchError::AgentLaunchWorkerUnavailable`] when the preflight
/// worker cannot be started.
pub(crate) fn queue_package_open(
    package_id: String,
    display_name: Option<String>,
    workspace: &Workspace,
    document: Option<PathBuf>,
    uri: Option<String>,
) -> Result<(), LaunchError> {
    let workspace = workspace.clone();
    spawn_reporting(
        std::thread::Builder::new().name("tilo-launch-preflight".to_string()),
        super::launch_readiness::inherit(move || {
            let outcome = installed_apps().and_then(|apps| {
                let app = apps
                    .into_iter()
                    .find(|app| app.package_id == package_id)
                    .ok_or_else(|| LaunchError::NotInstalled {
                        package_id: package_id.clone(),
                    })?;
                if !app.has_gui {
                    return Err(LaunchError::NotAGuiApp {
                        package_id: package_id.clone(),
                    });
                }
                let grants = workspace.launch_grants(tilo_clock::unix_now());
                execute_installed(
                    app,
                    workspace.room(),
                    document.as_deref(),
                    uri.as_deref(),
                    None,
                    None,
                    Some(grants),
                )
            });
            if let Err(error) = outcome {
                super::launch_readiness::fail_current(error.user_message());
                push_event(LaunchEvent {
                    display_name: display_name
                        .unwrap_or_else(|| display_name_for_package_id(&package_id)),
                    package_id,
                    managed_agent_id: None,
                    managed_workspace_id: None,
                    kind: LaunchEventKind::SpawnFailed {
                        reason: error.failure(),
                        detail: error.user_message(),
                    },
                });
            }
        }),
    )
    .map_err(|error| LaunchError::AgentLaunchWorkerUnavailable {
        detail: error.to_string(),
    })?;
    Ok(())
}

/// The session's existing authenticated broker, attached by its owning host.
#[derive(Clone)]
pub(crate) struct NativeAppOpening(pub tilo_action_broker::collab_broker::WeakAppActionBroker);

impl std::fmt::Debug for NativeAppOpening {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeAppOpening")
    }
}

/// Open only after the normal broker durably delegates app authorship for the
/// retained local person. Missing or stale custody never grants in the cache.
///
/// # Errors
/// Refuses unavailable owner/preflight workers; asynchronous authority and
/// publication failures arrive through the ordinary launch event channel.
pub(crate) fn queue_native_authoring_launch(
    app: InstalledApp,
    tree: super::state::WorkspaceTreeHandle,
    room: RoomId,
    opening: Option<NativeAppOpening>,
) -> Result<(), LaunchError> {
    let native_owner = tree.read().installed_native_owner();
    let package = app.package_id.clone();
    let expected = tree
        .read()
        .installed_native_owner()
        .ok_or_else(|| LaunchError::IdentityUnavailable {
            detail: crate::shell::i18n::t("launch-error-identity-unavailable"),
        })?
        .with_current(room, |view| Ok(view.device().clone()))
        .map_err(|detail| LaunchError::IdentityUnavailable { detail })?;
    queue_prepared_launch(
        app,
        room,
        move || {
            let prepare = || -> Result<LaunchGrants, String> {
                let (_, certificate) =
                    tilo_identity_host::broker::trusted_current_device(&room.to_string())
                        .map_err(|error| error.to_string())?;
                if certificate != expected {
                    return Err("native app opening identity changed before admission".into());
                }
                let owner = tree
                    .read()
                    .installed_native_owner()
                    .ok_or("native app owner disappeared")?;
                let access =
                    admit_native_app(&owner, room, &certificate, &package, opening.as_ref())?;
                if access != NativeAppOpenAccess::Delegate {
                    return Ok(native_launch_capabilities(
                        &native_launch_workspace(&tree, room)?,
                        &package,
                        access,
                    ));
                }
                native_launch_workspace(&tree, room)
                    .map(|workspace| workspace.launch_grants(tilo_clock::unix_now()))
            };
            prepare().map_err(|detail| LaunchError::IdentityUnavailable { detail })
        },
        QueuedOpen::default(),
        native_owner,
    )
}

/// Open `package` in `room` as `certificate`'s person: when the room does not
/// let the app author there yet and that person may grant it, ask through
/// the session's broker that it may, and wait until that is durable. What
/// every native opening of an app goes through, launched or restored into a
/// tab, before the app writes in the room. Answers the access the room gave
/// the app before, [`NativeAppOpenAccess::Delegate`] once it was granted
/// here. Blocks: call it off the frame loop.
pub(crate) fn admit_native_app(
    owner: &super::native_workspace_owner::NativeWorkspaceOwner,
    room: RoomId,
    certificate: &tilo_user_identity::DeviceCert,
    package: &str,
    opening: Option<&NativeAppOpening>,
) -> Result<NativeAppOpenAccess, String> {
    let access = native_app_open_access(owner, room, certificate, package)?;
    if access != NativeAppOpenAccess::Delegate {
        return Ok(access);
    }
    let broker = opening.ok_or("native app authorship broker unavailable")?;
    let broker = broker.0.upgrade()?;
    let job = broker.change_workspace_membership(
        &room.to_string(),
        &tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::GrantAppAuthorship {
            app: package.to_owned(),
        },
        certificate,
    )?;
    drop(broker);
    let completed = job
        .completion
        .recv_timeout(Duration::from_secs(30))
        .map_err(|error| format!("native app authorship did not durably complete: {error}"))?;
    if !completed.code.is_empty() {
        return Err(format!(
            "native app authorship refused: {}",
            completed.message
        ));
    }
    Ok(NativeAppOpenAccess::Delegate)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeAppOpenAccess {
    Delegate,
    Author,
    Viewer,
}

pub(crate) fn queue_observed_native_launch(
    app: InstalledApp,
    tree: super::state::WorkspaceTreeHandle,
    room: RoomId,
    original: tilo_action_broker::services::ObservedApplicationLaunch,
) -> Result<(), LaunchError> {
    let native_owner = tree.read().installed_native_owner();
    let package = app.package_id.clone();
    queue_prepared_launch(
        app,
        room,
        move || {
            prepare_observed_native_launch(&tree, room, &package, &original)
                .map_err(|detail| LaunchError::IdentityUnavailable { detail })
        },
        QueuedOpen::default(),
        native_owner,
    )
}

#[cfg(target_os = "linux")]
enum PreparedNavigation {
    SelectedOpen(Arc<retained_home::selected_target::SelectedContinuation>),
}

#[cfg(target_os = "linux")]
pub(crate) fn queue_selected_open(
    app: InstalledApp,
    tree: super::state::WorkspaceTreeHandle,
    selected: Arc<retained_home::selected_target::SelectedContinuation>,
) -> Result<(), LaunchError> {
    let room = selected.room();
    let owner = tree.read().installed_native_owner();
    let package = app.package_id.clone();
    let prepare_selected = selected.clone();
    queue_prepared_launch(
        app,
        room,
        move || {
            prepare_selected
                .prepare_workspace(&tree, &package)
                .map_err(|detail| LaunchError::IdentityUnavailable { detail })
        },
        QueuedOpen {
            navigation: Some(PreparedNavigation::SelectedOpen(selected)),
            ..QueuedOpen::default()
        },
        owner,
    )
}

fn prepare_observed_native_launch(
    tree: &super::state::WorkspaceTreeHandle,
    room: RoomId,
    package: &str,
    original: &tilo_action_broker::services::ObservedApplicationLaunch,
) -> Result<LaunchGrants, String> {
    let certificate = original.certificate()?;
    let owner = tree
        .read()
        .installed_native_owner()
        .ok_or("native app owner disappeared")?;
    let access = native_app_open_access(&owner, room, &certificate, package)?;
    if access == NativeAppOpenAccess::Delegate {
        let job = original.delegate_authorship()?;
        let completed = job
            .completion
            .recv_timeout(Duration::from_secs(30))
            .map_err(|error| format!("native app authorship did not durably complete: {error}"))?;
        if !completed.code.is_empty() {
            return Err(format!(
                "native app authorship refused: {}",
                completed.message
            ));
        }
        if original.certificate()? != certificate {
            return Err("original launch certificate changed during admission".into());
        }
        if native_app_open_access(&owner, room, &certificate, package)?
            != NativeAppOpenAccess::Author
        {
            return Err("native app authorship was not admitted by the canonical owner".into());
        }
    }
    // Recheck even viewer/author opens; the earlier result was not authority.
    original.certificate()?;
    Ok(native_launch_capabilities(
        &native_launch_workspace(tree, room)?,
        package,
        access,
    ))
}

/// What a native open hands `package`: the room's launch grants, never
/// naming a viewer as an author.
fn native_launch_capabilities(
    workspace: &Workspace,
    package: &str,
    access: NativeAppOpenAccess,
) -> LaunchGrants {
    let mut grants = workspace.launch_grants(tilo_clock::unix_now());
    if access == NativeAppOpenAccess::Viewer {
        grants.authors.retain(|author| author != package);
    }
    grants
}

#[cfg(test)]
#[path = "launcher_native_open_tests.rs"]
mod native_open_tests;
#[cfg(test)]
#[path = "launcher_package_surface_tests.rs"]
mod package_surface_tests;

pub(crate) fn native_app_open_access(
    owner: &super::native_workspace_owner::NativeWorkspaceOwner,
    room: RoomId,
    certificate: &tilo_user_identity::DeviceCert,
    package: &str,
) -> Result<NativeAppOpenAccess, String> {
    owner.with_current(room, |view| {
        if !view.peer().certificates().contains(certificate) {
            return Err("native app opening certificate is no longer admitted".into());
        }
        let now = std::time::SystemTime::now();
        let member = view
            .peer()
            .permissions(certificate.device_public_key, now)?;
        let administrator = view
            .peer()
            .workspace()
            .check_workspace_administration(certificate.device_public_key, now)
            .is_ok();
        let person_writes = member.allows(
            &member.policy.node().app(package),
            tilo_action_policy::ActionKind::Undoable,
            now,
        );
        let application_writes = member
            .policy
            .app_authors(package, &tilo_action_policy::Context::at_time(now));
        // A member who just joined holds the room's governance before any of
        // its app data: an app the governor lets write in the room opens
        // before this device has its document.
        //
        // The authorship decides, not whether this device happens to hold a
        // copy of the app's document. A member whose device already has one
        // opens the app exactly as a member whose device is still catching up
        // does, so every launch path gives the same answer for the same
        // member, room and app.
        if !administrator && !application_writes {
            if view.peer().document(package, "").is_none() {
                return Err(
                    "creating an application requires governor-authorized authorship".into(),
                );
            }
            view.peer()
                .check_document_read(certificate.device_public_key, package, "", now)?;
        }
        if person_writes && application_writes {
            return Ok(NativeAppOpenAccess::Author);
        }
        Ok(if administrator {
            NativeAppOpenAccess::Delegate
        } else {
            NativeAppOpenAccess::Viewer
        })
    })
}

fn native_launch_workspace(
    tree: &super::state::WorkspaceTreeHandle,
    room: RoomId,
) -> Result<Workspace, String> {
    tree.read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .cloned()
        .ok_or_else(|| "native app launch room disappeared".into())
}

/// What a queued launch asks its app to open, and what to undo if the
/// launch is refused before the process starts.
#[derive(Default)]
struct QueuedOpen {
    #[cfg(target_os = "linux")]
    navigation: Option<PreparedNavigation>,
    document: Option<PathBuf>,
    uri: Option<String>,
    /// Runs on the preflight worker when the launch is refused, so state the
    /// caller recorded optimistically (an "opening" note) does not outlive it.
    refused: Option<Box<dyn FnOnce() + Send>>,
}

impl QueuedOpen {
    fn uri(uri: Option<String>) -> Self {
        Self {
            uri,
            ..Self::default()
        }
    }
}

fn queue_prepared_launch(
    app: InstalledApp,
    room: RoomId,
    prepare: impl FnOnce() -> Result<LaunchGrants, LaunchError> + Send + 'static,
    open: QueuedOpen,
    native_owner: Option<super::native_workspace_owner::NativeWorkspaceOwner>,
) -> Result<(), LaunchError> {
    let package_id = app.package_id.clone();
    let display_name = app.display_name.clone();
    spawn_reporting(
        std::thread::Builder::new().name("tilo-launch-preflight".to_string()),
        super::launch_readiness::inherit(move || {
            let QueuedOpen {
                #[cfg(target_os = "linux")]
                navigation,
                document,
                uri,
                refused,
            } = open;
            let outcome = prepare().and_then(|grants| {
                #[cfg(target_os = "linux")]
                let context = match &navigation {
                    Some(PreparedNavigation::SelectedOpen(selected)) => {
                        LaunchContext::SelectedOpen(selected)
                    }
                    None => LaunchContext::None,
                };
                #[cfg(not(target_os = "linux"))]
                let context = LaunchContext::None;
                execute_installed_inner(
                    app,
                    room,
                    document.as_deref(),
                    uri.as_deref(),
                    context,
                    None,
                    Some(grants),
                    native_owner,
                )
            });
            if let Err(error) = outcome {
                #[cfg(target_os = "linux")]
                if let Some(PreparedNavigation::SelectedOpen(selected)) = &navigation {
                    selected.report_error(error.user_message());
                }
                if let Some(refused) = refused {
                    refused();
                }
                super::launch_readiness::fail_current(error.user_message());
                push_event(LaunchEvent {
                    package_id,
                    display_name,
                    managed_agent_id: None,
                    managed_workspace_id: None,
                    kind: LaunchEventKind::SpawnFailed {
                        reason: error.failure(),
                        detail: error.user_message(),
                    },
                });
            }
        }),
    )
    .map_err(|error| LaunchError::AgentLaunchWorkerUnavailable {
        detail: error.to_string(),
    })?;
    Ok(())
}

/// [`launch_document_in_workspace`] for the compositor thread: the launch's
/// file work (the synced capability file, the identity registry, the
/// authority binding) runs on a preflight worker, so a click that opens a
/// document never waits on the disk. The workspace's capabilities are still
/// published first, so a read grant added for this document reaches the
/// sandbox. A refusal arrives as a launch event, after `refused` has run on
/// the worker.
///
/// # Errors
///
/// Returns [`LaunchError::AgentLaunchWorkerUnavailable`] when the preflight
/// worker cannot be started; later failures arrive as launch events.
pub(crate) fn queue_document_in_workspace(
    app: InstalledApp,
    workspace: &Workspace,
    document: PathBuf,
    refused: impl FnOnce() + Send + 'static,
) -> Result<(), LaunchError> {
    let grants = workspace.launch_grants(tilo_clock::unix_now());
    set_workspace_capabilities(grants.clone());
    queue_prepared_launch(
        app,
        workspace.room(),
        move || Ok(grants),
        QueuedOpen {
            document: Some(document),
            uri: None,
            refused: Some(Box::new(refused)),
            ..QueuedOpen::default()
        },
        None,
    )
}

/// [`launch_uri_in_room`] for the compositor thread; see
/// [`queue_document_in_workspace`].
///
/// # Errors
///
/// As [`queue_document_in_workspace`].
pub(crate) fn queue_uri_in_room(
    app: InstalledApp,
    room: RoomId,
    grants: LaunchGrants,
    uri: String,
) -> Result<(), LaunchError> {
    set_workspace_capabilities(grants.clone());
    queue_prepared_launch(
        app,
        room,
        move || Ok(grants),
        QueuedOpen::uri(Some(uri)),
        None,
    )
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
    set_workspace_capabilities(workspace.launch_grants(tilo_clock::unix_now()));
    launch_with_document(package_id, workspace.room(), document)
}

/// Execute an already resolved installed package under the workspace's
/// ordinary capability, identity and continuity authority.
///
/// # Errors
/// Refuses packages without a GUI or the normal launcher preflight errors.
pub(crate) fn launch_resolved_in_workspace(
    app: InstalledApp,
    workspace: &Workspace,
) -> Result<(), LaunchError> {
    if !app.has_gui {
        return Err(LaunchError::NotAGuiApp {
            package_id: app.package_id,
        });
    }
    execute_installed(
        app,
        workspace.room(),
        None,
        None,
        None,
        None,
        Some(workspace.launch_grants(tilo_clock::unix_now())),
    )
    .map(|_| ())
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
    grants: LaunchGrants,
    uri: &str,
) -> Result<InstalledApp, LaunchError> {
    set_workspace_capabilities(grants);
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
        workspace.room(),
        workspace.launch_grants(tilo_clock::unix_now()),
        uri,
    )
}

#[cfg(any(test, target_os = "linux"))]
enum SupervisedLaunchGate {
    Local(launcher::registration_gate::RegistrationGate),
    #[cfg(target_os = "linux")]
    Retained(UnixStream),
}

/// Same launch supervision, with root retaining actual Child/reap custody for
/// the private outer mount namespace. This never constructs Child from a PID.
#[cfg(any(test, target_os = "linux"))]
enum SupervisedLaunchChild {
    Local(std::process::Child),
    /// Started by the session's app spawner, born in its own cgroup and
    /// still this process's child.
    #[cfg(target_os = "linux")]
    Delegated(launcher::spawner::DelegatedChild),
    #[cfg(target_os = "linux")]
    Retained(launcher::ordinary_child::PreparedOrdinaryChild),
}
#[cfg(any(test, target_os = "linux"))]
impl SupervisedLaunchChild {
    #[cfg(target_os = "linux")]
    fn retained(&self) -> io::Result<&launcher::ordinary_child::PreparedOrdinaryChild> {
        match self {
            Self::Retained(child) => Ok(child),
            Self::Local(_) | Self::Delegated(_) => Err(io::ErrorKind::PermissionDenied.into()),
        }
    }
    fn id(&self) -> u32 {
        match self {
            Self::Local(child) => child.id(),
            #[cfg(target_os = "linux")]
            Self::Delegated(child) => child.id(),
            #[cfg(target_os = "linux")]
            Self::Retained(child) => child.id(),
        }
    }
    /// The app cgroup the session's spawner made for it, when it did.
    #[cfg(target_os = "linux")]
    fn app_cgroup(&self) -> Option<&std::path::Path> {
        match self {
            Self::Delegated(child) => Some(child.cgroup()),
            Self::Local(_) | Self::Retained(_) => None,
        }
    }
    fn kill(&mut self) -> io::Result<()> {
        match self {
            Self::Local(child) => child.kill(),
            #[cfg(target_os = "linux")]
            Self::Delegated(child) => child.kill(),
            #[cfg(target_os = "linux")]
            Self::Retained(child) => child.kill(),
        }
    }
    fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        match self {
            Self::Local(child) => child.wait(),
            #[cfg(target_os = "linux")]
            Self::Delegated(child) => child.wait(),
            #[cfg(target_os = "linux")]
            Self::Retained(child) => child.wait(),
        }
    }
    fn stdout(&mut self) -> Option<std::fs::File> {
        match self {
            Self::Local(child) => child
                .stdout
                .take()
                .map(|pipe| std::fs::File::from(std::os::fd::OwnedFd::from(pipe))),
            #[cfg(target_os = "linux")]
            Self::Delegated(child) => child.take_stdout(),
            #[cfg(target_os = "linux")]
            Self::Retained(child) => child.take_stdout(),
        }
    }
    fn stderr(&mut self) -> Option<std::fs::File> {
        match self {
            Self::Local(child) => child
                .stderr
                .take()
                .map(|pipe| std::fs::File::from(std::os::fd::OwnedFd::from(pipe))),
            #[cfg(target_os = "linux")]
            Self::Delegated(child) => child.take_stderr(),
            #[cfg(target_os = "linux")]
            Self::Retained(child) => child.take_stderr(),
        }
    }
}

/// Run `command` on a worker thread and report the outcome through
/// [`LaunchSession`]; the thread lives as long as the app does.
fn spawn_detached(
    app: InstalledApp,
    command: LaunchCommand,
    authority: Option<action_authority::Lease>,
) -> Result<(), String> {
    #[cfg(any(test, target_os = "linux"))]
    {
        spawn_launch_with_authority(app.package_id, app.display_name, command, authority)
    }
    #[cfg(not(any(test, target_os = "linux")))]
    {
        drop(authority);
        spawn_launch(app.package_id, app.display_name, command)
    }
}

/// Put a [`LaunchCommand`]'s environment on the `tilo-launcher` process.
///
/// The launcher otherwise inherits the compositor's whole environment. A
/// release launcher already ignores its developer-only overrides
/// ([`launcher::dev_env::DEV_OVERRIDE_VARS`]); a release compositor also
/// strips them, so a launcher built with different settings still never
/// sees them.
pub fn apply_launch_env(command: &mut std::process::Command, env: &[(String, String)]) {
    apply_launch_env_with(command, env, crate::compositor::dev_env::DEV_ENV_OVERRIDES);
}

/// [`apply_launch_env`] as a function of the gate, so the release behaviour
/// is assertable from a debug test run.
fn apply_launch_env_with(
    command: &mut std::process::Command,
    env: &[(String, String)],
    dev_overrides: bool,
) {
    if !dev_overrides {
        for name in launcher::dev_env::DEV_OVERRIDE_VARS {
            command.env_remove(name);
        }
    }
    for (key, value) in env {
        command.env(key, value);
    }
}

/// A launch waiting for the session's services to come up.
#[cfg(any(test, target_os = "linux"))]
#[derive(Debug)]
struct HeldLaunch {
    authority: Option<action_authority::Lease>,
    #[cfg(target_os = "linux")]
    retained: Option<retained_home::Registration>,
    package_id: String,
    display_name: String,
    command: LaunchCommand,
}

/// Identifies a held launch for deduplication: the same app asked to open
/// the same thing. The environment is left out: it carries per-attempt
/// values (continuity instance, activation token) that differ every click.
#[cfg(any(test, target_os = "linux"))]
type HeldLaunchKey = (String, PathBuf, Vec<String>);

#[cfg(any(test, target_os = "linux"))]
type Gate = crate::shell::launch_gate::LaunchGate<HeldLaunchKey, HeldLaunch>;

/// Hold every app launch until [`release_held_launches`] or
/// [`fail_held_launches`]. The compositor calls this when it starts the
/// session network in the background, so that no app starts before the
/// collaboration broker it resolves at startup exists.
#[cfg(any(test, target_os = "linux"))]
pub fn hold_launches() {
    lock(&LaunchSession::current().gate).hold();
}

/// Run the held launches in the order they were requested, and let later
/// launches run at once.
#[cfg(any(test, target_os = "linux"))]
pub fn release_held_launches() {
    let session = LaunchSession::current();
    release_from(&session.gate, |held| {
        // A failure to start is reported through the session by `start_launch`.
        let _ = room_hold::admit(&session, held, start_held);
    });
}

/// Start `held` now in `session`.
#[cfg(any(test, target_os = "linux"))]
fn start_held(held: HeldLaunch, session: &LaunchSession) -> Result<(), String> {
    start_launch(
        held.package_id,
        held.display_name,
        held.command,
        session,
        held.authority,
        #[cfg(target_os = "linux")]
        held.retained,
    )
}

/// Report every held launch as failed with `reason`, and let later launches
/// run at once.
#[cfg(any(test, target_os = "linux"))]
pub fn fail_held_launches(reason: &str) {
    let session = LaunchSession::current();
    release_from(&session.gate, |launch| {
        session.startup.fail(
            super::launch_readiness::command_token(&launch.command),
            reason.to_owned(),
        );
        session.push(held_launch_failure(launch, reason));
    });
}

#[cfg(any(test, target_os = "linux"))]
fn release_from(gate: &Mutex<Gate>, mut start: impl FnMut(HeldLaunch)) {
    // Taken under the lock, started outside it: starting may launch again.
    let held = lock(gate).open();
    for launch in held {
        start(launch);
    }
}

#[cfg(test)]
fn fail_from(gate: &Mutex<Gate>, reason: &str, mut report: impl FnMut(LaunchEvent)) {
    let held = lock(gate).open();
    for launch in held {
        report(held_launch_failure(launch, reason));
    }
}

#[cfg(any(test, target_os = "linux"))]
fn held_launch_failure(launch: HeldLaunch, detail: &str) -> LaunchEvent {
    LaunchEvent {
        package_id: launch.package_id,
        display_name: launch.display_name,
        managed_agent_id: launch.command.managed_agent_id,
        managed_workspace_id: launch
            .command
            .service_identity
            .map(|identity| identity.workspace_id),
        kind: LaunchEventKind::SpawnFailed {
            reason: LaunchFailure::Other,
            detail: detail.to_string(),
        },
    }
}

/// `launch` will not start: its room is not going to be ready, for `reason`.
#[cfg(any(test, target_os = "linux"))]
fn room_refused(launch: HeldLaunch, reason: &str) -> LaunchEvent {
    let mut event = held_launch_failure(launch, reason);
    if let LaunchEventKind::SpawnFailed { reason, .. } = &mut event.kind {
        *reason = LaunchFailure::RoomRefused;
    }
    event
}

/// Admit one launch through `gate`: start it now, hold it, or report it.
#[cfg(any(test, target_os = "linux"))]
fn admit_launch(
    gate: &Mutex<Gate>,
    launch: HeldLaunch,
    start: impl FnOnce(HeldLaunch) -> Result<(), String>,
    report: impl FnOnce(LaunchEvent),
) -> Result<(), String> {
    use crate::shell::launch_gate::Admission;

    let mut key_args = launch.command.args.clone();
    if let Some(token) = super::launch_readiness::command_token(&launch.command) {
        // Ordinary repeated palette launches remain deduplicated. A broker
        // invocation owns one distinct startup attempt and cannot borrow
        // another invocation's queued worker or adopted window.
        key_args.push(format!("startup-observation:{token}"));
    }
    #[cfg(target_os = "linux")]
    if let Some(retained) = &launch.retained {
        // A root-owned operation cannot be collapsed into an unrelated normal
        // palette launch or another operation's held namespace endpoint.
        key_args.push(format!("retained-home:{}", retained.operation_id()));
    }
    let key = (
        launch.package_id.clone(),
        launch.command.program.clone(),
        key_args,
    );
    let admission = lock(gate).admit(key, launch);
    match admission {
        Admission::Run(launch) => start(launch),
        Admission::Held | Admission::AlreadyHeld => Ok(()),
        Admission::Full(launch) => {
            let detail = "too many apps are waiting for the desktop's network to start";
            report(held_launch_failure(launch, detail));
            Err(detail.to_string())
        }
    }
}

/// Launch `command` under `(package_id, display_name)`, or hold it while
/// the session's services start (see [`hold_launches`]). A held launch
/// reports like any other once it runs or fails.
///
/// # Errors
///
/// As `start_launch` for a launch that starts now; also when a launch
/// cannot be held because too many are already waiting.
#[cfg(any(test, target_os = "linux"))]
pub fn spawn_launch(
    package_id: String,
    display_name: String,
    command: LaunchCommand,
) -> Result<(), String> {
    spawn_launch_with_authority(package_id, display_name, command, None)
}

#[cfg(any(test, target_os = "linux"))]
fn spawn_launch_with_authority(
    package_id: String,
    display_name: String,
    mut command: LaunchCommand,
    authority: Option<action_authority::Lease>,
) -> Result<(), String> {
    super::launch_readiness::attach(&mut command);
    let session = LaunchSession::current();
    admit_launch(
        &session.gate,
        HeldLaunch {
            authority,
            #[cfg(target_os = "linux")]
            retained: None,
            package_id,
            display_name,
            command,
        },
        |held| room_hold::admit(&session, held, start_held),
        |event| session.push(event),
    )
}

/// Run `command` on a worker thread under `(package_id, display_name)`,
/// reporting to `events`, its session. Shared by both launch kinds — a `.tapp`
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
fn start_launch(
    package_id: String,
    display_name: String,
    command: LaunchCommand,
    events: &LaunchSession,
    authority: Option<action_authority::Lease>,
    #[cfg(target_os = "linux")] retained: Option<retained_home::Registration>,
) -> Result<(), String> {
    #[cfg(test)]
    #[cfg(not(target_os = "linux"))]
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
    let worker_events = events.clone();
    let startup_token = super::launch_readiness::command_token(&command);
    let diagnostic_attempt = diagnostic_attempt_for(&app.package_id, &command);
    if let Some(id) = diagnostic_attempt {
        lock(&events.launch_history).prepared(id, &command);
    }
    // The door the app's later windows in its room open through, held for
    // as long as this launch runs (`window_doors`).
    #[cfg(target_os = "linux")]
    let mut window_door = command
        .service_identity
        .as_ref()
        .and_then(|identity| window_doors::HeldDoor::hold(&app.package_id, &identity.workspace_id));
    let spawn_result = builder.spawn(move || {
        let mut authority_lease = authority;
        #[cfg(target_os = "linux")]
        let mut retained = retained;
        let events = worker_events;
        events.clone().adopt();
        if let Err(detail) = wait_for_retiring_app(
            &app.package_id,
            &command,
            std::time::Duration::from_secs(30),
        ) {
            record_launch_phase(diagnostic_attempt, launch_history::Phase::SpawnFailed);
            events.startup.fail(startup_token, detail.clone());
            events.push(LaunchEvent {
                package_id: app.package_id.clone(),
                display_name: app.display_name.clone(),
                managed_agent_id: app.managed_agent_id.clone(),
                managed_workspace_id: app.managed_workspace_id.clone(),
                kind: LaunchEventKind::SpawnFailed {
                    reason: LaunchFailure::PreviousInstanceBusy,
                    detail,
                },
            });
            return;
        }
        // Includes any wait for the previous window of this app to retire.
        tilo_startup_metrics::launch_phase(&app.package_id, "worker");
        let mut command = command;
        attach_browser_continuity(&app.package_id, &mut command);
        super::host_apps::attach_x11_display(&mut command);
        let mut cmd = Command::new(&command.program);
        cmd.args(&command.args);
        apply_launch_env(&mut cmd, &command.env);
        let diagnostic_options = *lock(&events.diagnostics);
        if let Some(options) = diagnostic_options {
            options.apply(&mut cmd);
        }
        // Not a terminal job: no stdin, and output is relayed (not
        // inherited) so it can be quoted back in a failure notification.
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Ordinary launches retain main's stdin gate and posix_spawn behavior.
        let mut local = || {
            // Born in a cgroup of its own through the session's spawner, with
            // no round trip to the user manager; the plain spawn below when
            // the spawner is not running (never in tests) or cannot take it.
            // A Tilo app (it has a service identity) is one unit when memory
            // runs out; a host app keeps the structure it was built with.
            #[cfg(target_os = "linux")]
            let kind = if command.service_identity.is_some() {
                launcher::app_cgroup::AppKind::Tilo
            } else {
                launcher::app_cgroup::AppKind::Host
            };
            #[cfg(target_os = "linux")]
            if let Some(spawned) = launcher::spawner::spawn_gated(&mut cmd, &app.package_id, kind)
            {
                let (child, gate) = spawned?;
                return Ok((
                    SupervisedLaunchChild::Delegated(child),
                    SupervisedLaunchGate::Local(gate),
                ));
            }
            let gate = launcher::registration_gate::install(&mut cmd)?;
            let child = cmd.spawn()?;
            Ok((
                SupervisedLaunchChild::Local(child),
                SupervisedLaunchGate::Local(gate),
            ))
        };
        #[cfg(target_os = "linux")]
        let owned: io::Result<_> = match retained.as_mut() {
            Some(registration) => registration.take_prepared_child().and_then(|mut child| {
                let gate = child.gate()?;
                Ok((
                    SupervisedLaunchChild::Retained(child),
                    SupervisedLaunchGate::Retained(gate),
                ))
            }),
            None => local(),
        };
        #[cfg(not(target_os = "linux"))]
        let owned: io::Result<_> = local();
        let (mut child, launch_gate) = match owned {
            Ok(owned) => owned,
            Err(error) => {
                record_launch_phase(diagnostic_attempt, launch_history::Phase::SpawnFailed);
                events.startup.fail(startup_token, error.to_string());
                events.push(LaunchEvent {
                    package_id: app.package_id.clone(),
                    display_name: app.display_name.clone(),
                    managed_agent_id: app.managed_agent_id.clone(),
                    managed_workspace_id: app.managed_workspace_id.clone(),
                    kind: LaunchEventKind::SpawnFailed {
                        reason: LaunchFailure::LauncherUnavailable,
                        detail: error.to_string(),
                    },
                });
                return;
            }
        };
        // The command still holds the launcher's end of the gate.
        drop(cmd);
        tilo_startup_metrics::launch_phase(&app.package_id, "spawned");
        if let Some(id) = diagnostic_attempt {
            lock(&events.launch_history).spawned(id, child.id());
        }
        if let Some(identity) = command.service_identity.clone() {
            // Capture compositor-authored launch input before releasing the
            // child gate. Never read window authority back from the sandbox.
            let instance = command
                .env
                .iter()
                .find(|(key, _)| key == "TILO_CONTINUITY_INSTANCE")
                .and_then(|(_, value)| value.parse::<u64>().ok());
            register_trusted_window_launch(child.id(), identity, instance, startup_token);
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
        #[cfg(target_os = "linux")]
        if let Some(lease) = authority_lease.as_mut()
            && let Err(error) = lease.register_sandbox(child.id())
        {
            record_launch_phase(diagnostic_attempt, launch_history::Phase::SpawnFailed);
            // The child is still behind the launch gate and has no sandbox
            // descriptors. Do not release a stale accepted-policy snapshot.
            let _ = child.kill();
            let status = child.wait();
            if let Some(id) = diagnostic_attempt {
                lock(&events.launch_history).exited(id, launch_exit_status(&status));
            }
            unregister_launched_process(child.id());
            events.startup.fail(startup_token, error.to_string());
            events.push(LaunchEvent {
                package_id: app.package_id.clone(),
                display_name: app.display_name.clone(),
                managed_agent_id: app.managed_agent_id.clone(),
                managed_workspace_id: app.managed_workspace_id.clone(),
                kind: LaunchEventKind::SpawnFailed {
                    reason: LaunchFailure::of_authority_error(&error),
                    detail: error.to_string(),
                },
            });
            return;
        }
        let launch_metric_id = mark_launch_spawned(&app.package_id, child.id(), diagnostic_attempt);
        let launch_generation = tilo_startup_metrics::launch_generation();
        eprintln!(
            "app-spawn: package={} launcher_pid={} launch_id={launch_metric_id:?} launch_generation={launch_generation}",
            app.package_id,
            child.id(),
        );
        events.startup.started(startup_token, child.id());
        // Recorded before the gate opens, so the session learns of the start
        // before the app can do anything a start explains.
        events.push(LaunchEvent {
            package_id: app.package_id.clone(),
            display_name: app.display_name.clone(),
            managed_agent_id: app.managed_agent_id.clone(),
            managed_workspace_id: app.managed_workspace_id.clone(),
            kind: LaunchEventKind::Started { pid: child.id() },
        });
        // The child cannot exec the launcher (and therefore cannot reach the
        // public Wayland socket) until both registries above are complete.
        #[cfg(target_os = "linux")]
        let door_end = window_door.as_mut().and_then(|door| {
            let (end, waiting) = door.serve(child.id());
            for token in waiting {
                events.startup.started(Some(token), child.id());
            }
            end
        });
        let local_gate = matches!(launch_gate, SupervisedLaunchGate::Local(_));
        let release = match launch_gate {
            #[cfg(target_os = "linux")]
            SupervisedLaunchGate::Local(gate) => match &door_end {
                Some(door) => {
                    use std::os::fd::AsFd as _;
                    gate.release_with_window_door(door.as_fd())
                }
                None => gate.release(),
            },
            #[cfg(not(target_os = "linux"))]
            SupervisedLaunchGate::Local(gate) => gate.release(),
            #[cfg(target_os = "linux")]
            SupervisedLaunchGate::Retained(gate) => match retained {
                Some(registration) => child.retained().and_then(|owned| {
                    registration.release(
                        &gate,
                        child.id(),
                        &command,
                        authority_lease.as_mut(),
                        owned,
                    )
                }),
                None => Err(io::ErrorKind::PermissionDenied.into()),
            },
        };
        // The launcher has its end now; the shell keeps only its own.
        #[cfg(target_os = "linux")]
        drop(door_end);
        // A child that has already closed its end of a local gate (it ended
        // before its release) was never let through it: it is supervised
        // below like any other, and reported with how it really ended.
        if let Err(error) = release
            && !(local_gate && error.kind() == io::ErrorKind::BrokenPipe)
        {
            let _ = child.kill();
            let _ = child.wait();
            unregister_launched_process(child.id());
            events.startup.fail(startup_token, error.to_string());
            events.push(LaunchEvent {
                package_id: app.package_id.clone(),
                display_name: app.display_name.clone(),
                managed_agent_id: app.managed_agent_id.clone(),
                managed_workspace_id: app.managed_workspace_id.clone(),
                kind: LaunchEventKind::SpawnFailed {
                    reason: LaunchFailure::of_authority_error(&error),
                    detail: error.to_string(),
                },
            });
            return;
        }

        // Drain both pipes concurrently. A GUI normally keeps both streams
        // open for its entire lifetime, so reading either one to EOF before
        // `wait` would turn the launch supervisor into a second lifetime
        // wait. Worse, a child filling the other pipe would deadlock both
        // processes. The joins below happen only after the child exits and
        // closes its inherited descriptors.
        if let Some(stdout) = child.stdout() {
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
        let stderr_drain = child.stderr().and_then(|stderr| {
            std::thread::Builder::new()
                .name("tilo-app-stderr".to_string())
                .spawn(move || {
                    let mut tail = VecDeque::with_capacity(STDERR_TAIL_LINES);
                    relay_capped_lines(stderr, |line| {
                        eprintln!("[app {stderr_tag} stderr] {line}");
                        // GPU health belongs to the Linux compositor; the
                        // test-only launches elsewhere have none to report to.
                        #[cfg(target_os = "linux")]
                        crate::compositor::gpu_health::observe_app_line(&stderr_tag, &line);
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
        // Ended by memory? Asked of the spawner once, now the launcher is
        // reaped: the kernel's OOM killer or the pressure policy.
        #[cfg(target_os = "linux")]
        let memory_end = memory_end_of(
            &status,
            child
                .app_cgroup()
                .and_then(launcher::spawner::memory_end),
        );
        #[cfg(not(target_os = "linux"))]
        let memory_end: Option<MemoryEnd> = None;
        // Reaped: the registry's liveness check now fails for this pid, even
        // while a descendant keeps stderr open and delays unregistering it.
        // A replacement waiting for this writer to retire may go now.
        note_running_changed();
        LAUNCH_RETIREMENT_CHANGED.notify_all();
        let mut diagnostic_exit = launch_exit_status(&status);
        if let Some(id) = diagnostic_attempt {
            lock(&events.launch_history).exited(id, diagnostic_exit.clone());
        }
        // Every end of an app, clean or not, with how it ended (a signal has
        // no exit code). The session's default log passes errors only, so a
        // clean exit would otherwise leave no trace; stderr is recorded.
        match &status {
            Ok(status) => eprintln!(
                "app-exit: package={} launcher_pid={launcher_pid} launch_id={launch_metric_id:?} launch_generation={launch_generation} status={status}",
                app.package_id
            ),
            Err(error) => eprintln!(
                "app-exit: package={} launcher_pid={launcher_pid} launch_id={launch_metric_id:?} launch_generation={launch_generation} wait_failed={error}",
                app.package_id
            ),
        }
        let tail = stderr_drain
            .and_then(|drain| drain.join().ok())
            .unwrap_or_default();
        if let Some(id) = diagnostic_attempt {
            diagnostic_exit.reason_key = (!diagnostic_exit.success).then(|| stopped_reason_key(diagnostic_exit.code, &tail.iter().cloned().collect::<Vec<_>>().join("\n")));
            diagnostic_exit.retained_stderr_lines = tail.len();
            diagnostic_exit.stderr_state = launch_history::StderrState::Complete;
            lock(&events.launch_history).exited(id, diagnostic_exit);
        }
        unregister_launched_process(launcher_pid);
        let kind = match (memory_end, status) {
            (Some(end), _) => LaunchEventKind::OutOfMemory(end),
            (None, Ok(status)) if status.success() => LaunchEventKind::Exited,
            (None, Ok(status)) => LaunchEventKind::Failed {
                code: status.code(),
                stderr_tail: tail.iter().cloned().collect::<Vec<_>>().join("\n"),
            },
            (None, Err(e)) => LaunchEventKind::SpawnFailed {
                reason: LaunchFailure::LauncherUnavailable,
                detail: e.to_string(),
            },
        };
        match &kind {
            LaunchEventKind::Failed { stderr_tail, code } => events.startup.fail(
                startup_token,
                format!("the application launcher exited ({code:?}): {stderr_tail}"),
            ),
            LaunchEventKind::SpawnFailed { detail, .. } => {
                events.startup.fail(startup_token, detail.clone());
            }
            LaunchEventKind::OutOfMemory(_) => {
                events
                    .startup
                    .fail(startup_token, "the app ran out of memory".to_string());
            }
            LaunchEventKind::Started { .. }
            | LaunchEventKind::Exited
            | LaunchEventKind::WindowLost { .. } => {}
        }
        events.push(LaunchEvent {
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
            lock(&events.launch_history).event(
                diagnostic_attempt.unwrap_or(0),
                launch_history::Phase::SpawnFailed,
            );
            let detail = format!("could not start launch thread: {e}");
            events.push(LaunchEvent {
                package_id: identity.0,
                display_name: identity.1,
                managed_agent_id: identity.2,
                managed_workspace_id: identity.3,
                kind: LaunchEventKind::SpawnFailed {
                    reason: LaunchFailure::LauncherUnavailable,
                    detail: detail.clone(),
                },
            });
            Err(detail)
        }
    }
}

/// Report `event` to the calling thread's [`LaunchSession`].
pub(crate) fn push_event(event: LaunchEvent) {
    LaunchSession::current().push(event);
}

fn launch_exit_status(status: &std::io::Result<std::process::ExitStatus>) -> launch_history::Exit {
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt as _;
    #[cfg(unix)]
    let signal = status
        .as_ref()
        .ok()
        .and_then(std::process::ExitStatus::signal);
    #[cfg(not(unix))]
    let signal = None;
    launch_history::Exit {
        code: status
            .as_ref()
            .ok()
            .and_then(std::process::ExitStatus::code),
        signal,
        success: status.as_ref().is_ok_and(std::process::ExitStatus::success),
        wait_failed: status.is_err(),
        reason_key: None,
        retained_stderr_lines: 0,
        stderr_redacted: true,
        stderr_state: launch_history::StderrState::Pending,
    }
}

fn diagnostic_attempt_for(package_id: &str, command: &LaunchCommand) -> Option<u64> {
    let instance = command.env.iter().find_map(|(name, value)| {
        (name == "TILO_CONTINUITY_INSTANCE")
            .then(|| value.parse::<u64>().ok())
            .flatten()
    });
    LaunchSession::with(|session| {
        let mut pending = lock(&session.pending);
        let id = pending
            .iter_mut()
            .find(|note| {
                note.key == package_id
                    && !note.metric_spawned
                    && !note.diagnostic_reserved
                    && note.claimed_by.is_none()
                    && instance.is_none_or(|instance| note.continuity_instance == Some(instance))
            })
            .map(|note| {
                note.diagnostic_reserved = true;
                note.launch_metric_id
            });
        drop(pending);
        id
    })
}

fn record_launch_phase(id: Option<u64>, phase: launch_history::Phase) {
    if let Some(id) = id {
        lock(&LaunchSession::current().launch_history).event(id, phase);
    }
}

fn mark_launch_spawned(package_id: &str, pid: u32, attempt: Option<u64>) -> Option<u64> {
    LaunchSession::with(|session| {
        let mut pending = lock(&session.pending);
        let id = pending
            .iter_mut()
            .find(|note| {
                note.key == package_id
                    && !note.metric_spawned
                    && Some(note.launch_metric_id) == attempt
            })
            .map(|note| {
                note.metric_spawned = true;
                note.launch_metric_id
            });
        drop(pending);
        if let Some(id) = id {
            tilo_startup_metrics::launch_spawned(id, pid);
        }
        id
    })
}

/// Finish every launch-bound surface that has captured its first frame.
/// The compositor calls this once per tick; `frame_seq` reads the capture
/// store, and the scan is over launches still waiting for a first frame.
pub(crate) fn record_presented_launches(frame_seq: impl Fn(u64) -> u64) {
    for surface_id in tilo_startup_metrics::launches_awaiting_first_present() {
        record_first_present(surface_id, frame_seq(surface_id));
    }
}

/// Record the first captured frame for a launch-bound surface exactly once.
pub(crate) fn record_first_present(surface_id: u64, frame_seq: u64) {
    if frame_seq != 0 {
        lock(&LaunchSession::current().launch_history).presented(surface_id);
    }
    tilo_startup_metrics::launch_first_present(surface_id, frame_seq);
}

/// Take every launch event queued since the last call. Called once per
/// frame by the chrome.
pub fn drain_events() -> Vec<LaunchEvent> {
    LaunchSession::current().take()
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
    LaunchSession::with(|session| {
        let mut launched = lock(&session.launched);
        if launched.iter().any(|(k, _, _)| k == key) {
            return;
        }
        launched.push((
            key.to_string(),
            identities.to_vec(),
            display_name.to_string(),
        ));
    });
}

/// Return the durable launch key recorded for a mapped window identity.
///
/// This is stronger than desktop-entry inference: some real host entries do
/// not declare `StartupWMClass` even though their process announces a
/// different toolkit app-id. The launch table records the exact relationship
/// at the user action that started the app, so persistence can retain the
/// desktop-file id accepted by `run-host` on the next login.
pub(crate) fn launch_key_for_app_id(app_id: &str) -> Option<String> {
    LaunchSession::with(|session| {
        let launched = lock(&session.launched);
        launched
            .iter()
            .find(|(_, identities, _)| identities_match(identities, app_id))
            .map(|(key, _, _)| key.clone())
    })
}

/// Return the durable launch key claimed by this exact mapped surface.
///
/// Unlike textual identity matching, this also works for host applications
/// whose desktop file omits `StartupWMClass`: the pending launch destination
/// is claimed by the concrete surface during adoption, so the relationship is
/// authoritative even when the runtime toolkit id was undeclared.
pub(crate) fn launch_key_for_surface(surface_id: u64) -> Option<String> {
    let attributed = lock(&LaunchSession::current().surfaces)
        .get(&surface_id)
        .cloned();
    if attributed.is_some() {
        return attributed;
    }
    LaunchSession::with(|session| {
        let pending = lock(&session.pending);
        pending
            .iter()
            .find(|note| note.claimed_by == Some(surface_id))
            .map(|note| note.key.clone())
    })
}

/// Bind an accepted surface to the kernel-authenticated launch identity of
/// its Wayland connection. Host applications may announce an undeclared
/// toolkit app-id, so this mapping is intentionally independent of protocol
/// strings and pending-placement matching.
pub fn attribute_surface_launch(surface_id: u64, launch_key: &str) {
    lock(&LaunchSession::current().surfaces).insert(surface_id, launch_key.to_string());
}

/// A window of launch `key` is on screen: if that launch later exits with
/// an error, the app ran and then stopped (killed, crashed, quit with a
/// status) rather than failing to open.
pub(crate) fn note_launch_presented(key: &str) {
    lock(&LaunchSession::current().presented).insert(key.to_string());
}

/// Remember an XDG autostart entry. Its process exiting is not the person
/// failing to open the app they just launched.
pub(crate) fn note_session_helper(package_id: &str) {
    lock(&LaunchSession::current().session_helpers).insert(package_id.to_string());
}

/// How to start an `X-GNOME-AutoRestart` autostart entry again.
#[derive(Debug, Clone)]
pub(crate) struct RestartableHelper {
    display_name: String,
    command: LaunchCommand,
    started_at: Instant,
}

/// A helper that dies sooner than this after (re)starting is not restarted
/// again, so a program that cannot run does not loop. `gnome-session`'s limit.
pub(crate) const HELPER_RESTART_LIMIT: Duration = Duration::from_secs(60);

/// Remember how to restart an autostart entry marked `X-GNOME-AutoRestart`.
pub(crate) fn note_restartable_session_helper(
    package_id: &str,
    display_name: &str,
    command: LaunchCommand,
) {
    lock(&LaunchSession::current().restartable_helpers).insert(
        package_id.to_string(),
        RestartableHelper {
            display_name: display_name.to_string(),
            command,
            started_at: Instant::now(),
        },
    );
}

/// The command to start a restartable helper that died at `now` again, or
/// `None` when it is not restartable or died within [`HELPER_RESTART_LIMIT`]
/// of its last start (it is then forgotten).
pub(crate) fn take_helper_restart(
    package_id: &str,
    now: Instant,
) -> Option<(String, LaunchCommand)> {
    LaunchSession::with(|session| {
        let mut helpers = lock(&session.restartable_helpers);
        let helper = helpers.get_mut(package_id)?;
        if now.saturating_duration_since(helper.started_at) >= HELPER_RESTART_LIMIT {
            helper.started_at = now;
            return Some((helper.display_name.clone(), helper.command.clone()));
        }
        helpers.remove(package_id);
        drop(helpers);
        tracing::warn!(
            target: "shell::launcher",
            %package_id,
            "autostart helper is dying too quickly; not restarting it again"
        );
        None
    })
}

/// Restart an `X-GNOME-AutoRestart` helper that died; whether it was restarted.
pub(crate) fn restart_session_helper(package_id: &str) -> bool {
    let Some((display_name, command)) = take_helper_restart(package_id, Instant::now()) else {
        return false;
    };
    tracing::info!(target: "shell::launcher", %package_id, "restarting autostart helper");
    spawn_launch(package_id.to_string(), display_name, command).is_ok()
}

/// A signal exit of an autostart helper that the person did not ask to open.
///
/// A later palette launch of the same id sets [`set_pending_placement`] and
/// is reported normally.
pub(crate) fn unasked_session_helper(package_id: &str) -> bool {
    LaunchSession::with(|session| {
        let helpers = lock(&session.session_helpers);
        if !helpers.iter().any(|id| id.eq_ignore_ascii_case(package_id)) {
            return false;
        }
        drop(helpers);
        let awaiting = lock(&session.awaiting_window);
        !awaiting
            .iter()
            .any(|id| id.eq_ignore_ascii_case(package_id))
    })
}

/// Bind a host window whose toolkit id matches no desktop entry to the one
/// user launch still waiting for a window, then remember that later windows
/// from that launch (`control.exe` after `wineboot.exe`) belong to it too.
pub(crate) fn attribute_unresolved_host_window(surface_id: u64, app_id: &str) {
    if app_id.trim().is_empty() || app_id.starts_with("anon-") {
        return;
    }
    if launch_key_for_surface(surface_id).is_some() || launch_key_for_app_id(app_id).is_some() {
        return;
    }
    let Some(key) = unresolved_user_launch_key() else {
        return;
    };
    attribute_surface_launch(surface_id, &key);
    let session = LaunchSession::current();
    lock(&session.unresolved_owners).insert(key.clone());
    lock(&session.awaiting_window).retain(|id| !id.eq_ignore_ascii_case(&key));
}

/// The compositor observed a toplevel. Record it against the launch that
/// owns the window, including when the announced id is not the desktop id.
pub(crate) fn record_host_window_open(surface_id: u64, app_id: &str) {
    if app_id.trim().is_empty() || app_id.starts_with("anon-") {
        return;
    }
    attribute_unresolved_host_window(surface_id, app_id);
    note_launch_presented(&crate::shell::persist::durable_app_id(surface_id, app_id));
}

/// An accepted toplevel's kernel peer, after ordinary workspace admission.
/// A client-announced application id cannot produce a startup receipt.
pub(crate) fn record_startup_window(
    ancestry: &[(u32, u64)],
    adoption: Option<tilo_action_broker::services::LaunchAdoption>,
) {
    LaunchSession::current()
        .startup
        .adopted_window(ancestry, adoption);
    #[cfg(target_os = "linux")]
    window_doors::window_mapped(ancestry);
}

/// Live unconfined launches a close of an unmatched toolkit window may stop.
///
/// One live launch is that launch. Several live launches means session
/// helpers are up beside the app the person opened: only the launch that
/// asked for a window, or that already owns an unmatched window, is stopped.
#[cfg(any(test, target_os = "linux"))]
pub(crate) fn unconfined_launches_for_unresolved_close() -> Vec<String> {
    LaunchSession::with(|session| {
        let awaiting = lock(&session.awaiting_window).clone();
        let owners = lock(&session.unresolved_owners).clone();
        let live = live_unconfined_package_ids();
        let targets: Vec<String> = live
            .iter()
            .filter(|id| {
                awaiting
                    .iter()
                    .any(|marked| marked.eq_ignore_ascii_case(id))
                    || owners.iter().any(|marked| marked.eq_ignore_ascii_case(id))
            })
            .cloned()
            .collect();
        if targets.len() == 1 {
            return targets;
        }
        if targets.is_empty() && live.len() == 1 {
            return live;
        }
        Vec::new()
    })
}

#[cfg(any(test, target_os = "linux"))]
fn unresolved_user_launch_key() -> Option<String> {
    LaunchSession::with(|session| {
        let awaiting = lock(&session.awaiting_window).clone();
        let owners = lock(&session.unresolved_owners).clone();
        let live = live_unconfined_package_ids();
        let pick = |marked: &HashSet<String>| -> Vec<String> {
            live.iter()
                .filter(|id| marked.iter().any(|item| item.eq_ignore_ascii_case(id)))
                .cloned()
                .collect()
        };
        let waiting = pick(&awaiting);
        if waiting.len() == 1 {
            return waiting.into_iter().next();
        }
        if !waiting.is_empty() {
            return None;
        }
        let owned = pick(&owners);
        if owned.len() == 1 {
            return owned.into_iter().next();
        }
        None
    })
}

#[cfg(not(any(test, target_os = "linux")))]
fn unresolved_user_launch_key() -> Option<String> {
    None
}

/// Whether launch `key` has shown a window since it last started, clearing
/// the mark: a later launch of the same app starts unproven.
pub(crate) fn take_launch_presented(key: &str) -> bool {
    lock(&LaunchSession::current().presented).remove(key)
}

/// A toplevel went away. `closed` says someone ended it: the compositor
/// asked it to close, or the person had reached the app and could have
/// asked it. Otherwise the window was lost under its launch.
pub(crate) fn note_surface_gone(surface_id: u64, closed: bool) {
    LaunchSession::with(|session| {
        let key = lock(&session.surfaces).remove(&surface_id);
        if let Some(key) = key
            && !closed
            && !lock(&session.acted_on).contains(&key)
        {
            tracing::error!(
                target: "tilo::app_launch",
                launch = %key,
                surface_id,
                "an application window went away without being closed"
            );
            lock(&session.vanished).insert(key);
        }
    });
}

/// An action invocation was handed to the app of launch `key`. Driving an
/// app through its actions is reaching it as much as a key or a click is.
pub fn note_launch_acted_on(key: &str) {
    lock(&LaunchSession::current().acted_on).insert(key.to_string());
}

/// Launch `key` started again: what was said of its previous run no
/// longer holds.
pub(crate) fn note_launch_restarted(key: &str) {
    let session = LaunchSession::current();
    lock(&session.vanished).remove(key);
    lock(&session.acted_on).remove(key);
    lock(&session.lost_tabs).remove(key);
}

/// Whether a window of launch `key` went away without being closed since
/// the launch started, clearing the mark.
pub fn take_launch_vanished(key: &str) -> bool {
    lock(&LaunchSession::current().vanished).remove(key)
}

/// The most lost windows kept per launch.
const LOST_TABS_PER_LAUNCH: usize = 8;

/// How long before its launch ended a lost window counts as lost with it: an
/// app ended for memory loses every window at once, and the launch's end
/// follows within moments. A window the app closed itself long before is
/// not brought back.
const LOST_WITH_THE_LAUNCH: std::time::Duration = std::time::Duration::from_secs(10);

/// A window of launch `key` went away without being asked to close; `tab`
/// is its tab as a reopen restores it. Kept until the launch ends.
pub(crate) fn note_lost_tab(key: &str, tab: crate::shell::closed_tabs::ClosedTab) {
    LaunchSession::with(|session| {
        keep_lost_tab(
            lock(&session.lost_tabs).entry(key.to_string()).or_default(),
            tab,
        );
    });
}

/// Keep `tab` among a launch's lost tabs, forgetting the oldest past
/// [`LOST_TABS_PER_LAUNCH`].
fn keep_lost_tab(
    tabs: &mut Vec<(std::time::Instant, crate::shell::closed_tabs::ClosedTab)>,
    tab: crate::shell::closed_tabs::ClosedTab,
) {
    if tabs.len() == LOST_TABS_PER_LAUNCH {
        tabs.remove(0);
    }
    tabs.push((std::time::Instant::now(), tab));
}

/// The tabs of launch `key`'s windows that went away unasked just before
/// now, taken once; older ones are forgotten.
pub(crate) fn take_lost_tabs(key: &str) -> Vec<crate::shell::closed_tabs::ClosedTab> {
    lock(&LaunchSession::current().lost_tabs)
        .remove(key)
        .unwrap_or_default()
        .into_iter()
        .filter(|(at, _)| at.elapsed() <= LOST_WITH_THE_LAUNCH)
        .map(|(_, tab)| tab)
        .collect()
}

/// What the session's app spawner reports on its own: an app's independent
/// job (a terminal session, a language server) ended for memory while the
/// app keeps running. Called on the spawner's thread.
#[cfg(target_os = "linux")]
pub fn note_spawner_event(event: ::launcher::spawner::SpawnerEvent) {
    match event {
        ::launcher::spawner::SpawnerEvent::JobEnded { app_id, job, cause } => {
            let display_name = display_name_for_app_id(&app_id).unwrap_or_else(|| app_id.clone());
            push_event(LaunchEvent {
                package_id: app_id,
                display_name,
                managed_agent_id: None,
                managed_workspace_id: None,
                kind: LaunchEventKind::OutOfMemory(MemoryEnd {
                    cause,
                    job: Some(job),
                }),
            });
        }
    }
}

/// Forget every launch this session has recorded — for tests asserting
/// the fallback resolution after launches of their own, since the table is
/// append-only.
#[cfg(test)]
pub(crate) fn clear_launched_for_test() {
    let session = LaunchSession::current();
    lock(&session.launched).clear();
    lock(&session.surfaces).clear();
    lock(&session.awaiting_window).clear();
    lock(&session.unresolved_owners).clear();
    lock(&session.session_helpers).clear();
    lock(&session.restartable_helpers).clear();
    lock(&session.presented).clear();
    lock(&session.vanished).clear();
    lock(&session.lost_tabs).clear();
    lock(&session.acted_on).clear();
}

/// Matched textually: either the app-id is the package id, or its trailing
/// component (`so.tilo.hello` ↔ `hello`). An unrelated app-id keeps its own
/// label — closing that gap needs the client correlated back to the
/// spawned pid (`SO_PEERCRED`), not this module.
pub fn display_name_for_app_id(app_id: &str) -> Option<String> {
    display_name_among(app_id, || installed_apps().unwrap_or_default())
}

/// [`display_name_for_app_id`] for the compositor loop: the installed
/// packages as last enumerated ([`installed_apps_without_waiting`]), never
/// a registry read. An app installed since may show its id until the
/// refresh lands.
pub(crate) fn display_name_for_app_id_without_waiting(app_id: &str) -> Option<String> {
    display_name_among(app_id, installed_apps_without_waiting)
}

fn display_name_among(
    app_id: &str,
    installed: impl FnOnce() -> Vec<InstalledApp>,
) -> Option<String> {
    let launched_name = LaunchSession::with(|session| {
        lock(&session.launched)
            .iter()
            .find(|(_, identities, _)| identities_match(identities, app_id))
            .map(|(_, _, display_name)| display_name.clone())
    });
    if launched_name.is_some() {
        return launched_name;
    }
    // Not launched by this session, but the machine still has it: a
    // package's manifest name is the app's name regardless of who
    // started the window.
    if let Some(app) = installed()
        .into_iter()
        .find(|a| app_id_matches_package(app_id, &a.package_id))
    {
        return Some(app.display_name);
    }
    // Nor a package launched this session — maybe a host app started
    // another way; its `.desktop` entry gives a nicer label than the raw id.
    crate::shell::host_apps::app_for_window_id(app_id).map(|app| app.name)
}

/// Whether a client's wayland app-id belongs to `package_id`: either the
/// app-id *is* the package id, or its trailing component.
/// Whether `package_id` runs in `room` and opens further windows in its
/// process ([`window_doors`]): asking for it again is a new window of it.
pub(crate) fn opens_more_windows(package_id: &str, room: RoomId) -> bool {
    #[cfg(target_os = "linux")]
    return window_doors::opens_more_windows(package_id, room);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (package_id, room);
        false
    }
}

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
    LaunchSession::with(|session| {
        // The person asked for this window. Autostart does not come through here,
        // so a later toolkit id that no desktop entry declared (wineboot.exe)
        // can still be tied to this launch and not to a session helper.
        lock(&session.awaiting_window).insert(key.to_string());
        let mut pending = lock(&session.pending);
        let launch_metric_id = tilo_startup_metrics::launch_requested(key);
        let saved_tab = match placement {
            crate::shell::tab_binding::Placement::IntoTab(tab) => Some(tab),
            _ => None,
        };
        lock(&session.launch_history).requested(launch_metric_id, key, saved_tab);
        pending.push(PendingPlacement {
            key: key.to_string(),
            identities: identities.to_vec(),
            placement,
            continuity_instance: None,
            claimed_by: None,
            retire_after: None,
            present_before: pending_placement_deadline(PENDING_PRESENTATION_TIMEOUT),
            launch_metric_id,
            metric_spawned: false,
            diagnostic_reserved: false,
            embed: None,
        });
    });
}

/// The launch noted under `noted` turned out to be the host app `key` (a
/// restored window whose app is a `.desktop` entry, not a package): carry
/// that same note over to the host app's key and window identities instead
/// of noting the launch a second time. Two notes for one launch left the
/// second waiting, ready to pull the next window of that app into the
/// restored tab. `false` when there was no unclaimed note under `noted`.
pub fn retarget_pending_placement(noted: &str, key: &str, identities: &[String]) -> bool {
    LaunchSession::with(|session| {
        let mut pending = lock(&session.pending);
        let Some(note) = pending
            .iter_mut()
            .rev()
            .find(|note| note.key == noted && note.claimed_by.is_none())
        else {
            return false;
        };
        key.clone_into(&mut note.key);
        for identity in identities {
            if !note
                .identities
                .iter()
                .any(|known| known.eq_ignore_ascii_case(identity))
            {
                note.identities.push(identity.clone());
            }
        }
        drop(pending);
        lock(&session.awaiting_window).insert(key.to_string());
        true
    })
}

/// Automatic restoration waits for the preceding window of this app to map.
/// The existing launch failure and expiry paths retire unanswered placements;
/// claimed placements remain briefly for the compositor's second lookup.
pub(crate) fn has_unclaimed_launch(app_id: &str) -> bool {
    lock(&LaunchSession::current().pending)
        .iter()
        .any(|note| note.matches(app_id) && note.claimed_by.is_none())
}

/// Whether any launch bound to a saved tab still waits for its window.
/// Session restore starts the next saved window only after this clears:
/// cold starts that run together compete for CPU, disk and memory, so
/// every one of them comes up late. The same failure and expiry paths as
/// [`has_unclaimed_launch`] retire a note that never maps.
pub(crate) fn has_unclaimed_restore_launch() -> bool {
    lock(&LaunchSession::current().pending).iter().any(|note| {
        note.claimed_by.is_none()
            && matches!(
                note.placement,
                crate::shell::tab_binding::Placement::IntoTab(_)
            )
    })
}

/// Saved tabs with a live placement in this session. A mapped note stays
/// valid through the existing claim grace; no deadline or placement is changed.
pub(crate) fn pending_restore_tabs() -> std::collections::HashSet<u64> {
    pending_restore_tabs_at(Instant::now())
}

fn pending_restore_tabs_at(now: Instant) -> std::collections::HashSet<u64> {
    lock(&LaunchSession::current().pending)
        .iter()
        .filter(|note| {
            note.retire_after.is_none_or(|deadline| now < deadline)
                && (note.claimed_by.is_some() || now < note.present_before)
        })
        .filter_map(|note| match note.placement {
            crate::shell::tab_binding::Placement::IntoTab(tab) => Some(tab),
            _ => None,
        })
        .collect()
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
    claim_toplevel_note(app_id, surface_id).map(|(placement, instance, _)| (placement, instance))
}

/// The live view whose launch `surface_id` (of `app_id`) is the window of,
/// claiming that launch's note as [`placement_and_instance_for_toplevel`]
/// does: such a window is drawn inside another, never placed as one.
pub(crate) fn embed_for_toplevel(app_id: &str, surface_id: u64) -> Option<u64> {
    claim_toplevel_note(app_id, surface_id).and_then(|(_, _, embed)| embed)
}

/// Claim the first launch note `app_id`'s window `surface_id` answers to:
/// its placement, launch instance and live view.
fn claim_toplevel_note(
    app_id: &str,
    surface_id: u64,
) -> Option<(
    crate::shell::tab_binding::Placement,
    Option<u64>,
    Option<u64>,
)> {
    LaunchSession::with(|session| {
        let mut pending = lock(&session.pending);
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
            let embed = pending.get(idx)?.embed;
            match claimed_by {
                None => {
                    if let Some(entry) = pending.get_mut(idx) {
                        entry.claimed_by = Some(surface_id);
                        tilo_startup_metrics::launch_mapped(entry.launch_metric_id, surface_id);
                        lock(&session.launch_history).mapped(entry.launch_metric_id, surface_id);
                        // Both compositor trees ask for the same launch note,
                        // so keep a claimed answer briefly rather than consume
                        // it on the first lookup. Bound that duplication window
                        // as well: an app that never causes a second lookup must
                        // not leave a permanent note capable of steering a much
                        // later surface with the same identity.
                        entry.retire_after =
                            Some(pending_placement_deadline(PENDING_PLACEMENT_CLAIM_GRACE));
                    }
                    return Some((placement, instance, embed));
                }
                Some(claimed) if claimed == surface_id => {
                    return Some((placement, instance, embed));
                }
                // This note belongs to an earlier window. Retire it and let this
                // surface claim the next queued launch for the same app, if any.
                Some(_) => {
                    pending.remove(idx);
                }
            }
        }
    })
}

#[cfg(test)]
pub(crate) fn attach_continuity_instance_for_test(package_id: &str, instance: u64) {
    attach_continuity_instance(package_id, instance);
}

/// Attach the launch instance to the existing pending placement note. This is
/// called after the real launcher command is assembled, never by pure command
/// construction, so tests and previews cannot invent a resumable instance.
fn attach_continuity_instance(package_id: &str, instance: u64) {
    LaunchSession::with(|session| {
        let mut pending = lock(&session.pending);
        if let Some(note) = pending
            .iter_mut()
            .rev()
            .find(|note| note.key == package_id && note.continuity_instance.is_none())
        {
            note.continuity_instance = Some(instance);
        } else {
            let launch_metric_id = tilo_startup_metrics::launch_requested(package_id);
            lock(&session.launch_history).requested(launch_metric_id, package_id, None);
            pending.push(PendingPlacement {
                key: package_id.to_string(),
                identities: identities_for_package(package_id),
                placement: crate::shell::tab_binding::Placement::NewGroup,
                continuity_instance: Some(instance),
                claimed_by: None,
                retire_after: None,
                present_before: pending_placement_deadline(PENDING_PRESENTATION_TIMEOUT),
                launch_metric_id,
                metric_spawned: false,
                diagnostic_reserved: false,
                embed: None,
            });
        }
    });
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

pub(crate) fn clear_staged_continuation(room: RoomId, tab: u64) {
    lock(&PENDING_CONTINUATION_RESUME).remove(&(room, tab));
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

/// Take the continuation staged for the next launch of `package_id` into
/// `room`: the record instance, and the resume projection when there is one.
/// Taking it means exactly one launch receives it. The projection is checked
/// once more against the package's installed manifest, which may not have
/// existed when the checkpoint arrived from another device.
pub fn take_staged_resume_for_package(
    package_id: &str,
    room: RoomId,
) -> Option<StagedContinuationResume> {
    let placement = lock(&LaunchSession::current().pending)
        .iter()
        .rev()
        .find(|note| note.key == package_id)
        .map(|note| note.placement)?;
    let (resume, record_instance) = take_staged_resume(placement, room)?;
    let resume = resume.and_then(|(schema, bytes)| {
        crate::shell::personal_continuity_registry::resume_for_installed_launch(
            package_id, schema, bytes,
        )
    });
    Some((resume, record_instance))
}

/// Drop the newest unclaimed placement note for `package_id`, if any —
/// undoing the most recently staged launch that failed before a surface was
/// created without destroying older concurrent launches' destinations.
pub fn clear_pending_placement(package_id: &str) {
    LaunchSession::with(|session| {
        let mut pending = lock(&session.pending);
        let note = pending
            .iter()
            .rposition(|p| p.key == package_id && p.claimed_by.is_none())
            .map(|index| pending.remove(index));
        drop(pending);
        if let Some(note) = note {
            lock(&session.launch_history)
                .event(note.launch_metric_id, launch_history::Phase::Retired);
            tilo_startup_metrics::launch_failed(note.launch_metric_id);
        }
    });
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
    LaunchSession::with(|session| {
        let mut pending = lock(&session.pending);
        let matching: Vec<usize> = pending
            .iter()
            .enumerate()
            .filter(|(_, note)| note.key == package_id)
            .map(|(index, _)| index)
            .collect();
        let [index] = matching[..] else {
            return None;
        };
        let surface_id = pending.get(index)?.claimed_by?;
        let note = pending.remove(index);
        drop(pending);
        lock(&session.launch_history).event(note.launch_metric_id, launch_history::Phase::Retired);
        tilo_startup_metrics::launch_failed(note.launch_metric_id);
        Some(surface_id)
    })
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

/// Give `package_id`'s waiting launch `extra` before its window is due, on
/// top of the usual start-up allowance, counted from now. An install that
/// precedes the start holds it for as long as the install may take, then
/// passes [`Duration::ZERO`] once the app can actually start.
pub(crate) fn hold_presentation_for(package_id: &str, extra: Duration) {
    LaunchSession::with(|session| {
        let mut pending = lock(&session.pending);
        if let Some(note) = pending
            .iter_mut()
            .rev()
            .find(|note| note.key == package_id && note.claimed_by.is_none())
        {
            note.present_before =
                pending_placement_deadline(PENDING_PRESENTATION_TIMEOUT.saturating_add(extra));
        }
    });
}

fn retire_pending_placement_at(package_id: &str, deadline: Instant) {
    LaunchSession::with(|session| {
        let mut pending = lock(&session.pending);
        if let Some(note) = pending
            .iter_mut()
            .rev()
            .find(|note| note.key == package_id && note.claimed_by.is_none())
        {
            note.retire_after = Some(deadline);
        }
    });
}

/// Retire windowless clean launches without waiting for another application
/// with the same identity to map.
pub fn expire_pending_placements() -> Vec<String> {
    expire_pending_placements_at(Instant::now())
}

fn expire_pending_placements_at(now: Instant) -> Vec<String> {
    let session = LaunchSession::current();
    let mut expired = Vec::new();
    let mut unshown = Vec::new();
    lock(&session.pending).retain(|note| {
        // A launch waiting for its room has not started: its window is not
        // late yet.
        #[cfg(any(test, target_os = "linux"))]
        let waiting = note.claimed_by.is_none() && room_hold::waits(&session, &note.key);
        #[cfg(not(any(test, target_os = "linux")))]
        let waiting = false;
        let presentation_timed_out =
            note.claimed_by.is_none() && now >= note.present_before && !waiting;
        let retired = note.retire_after.is_some_and(|deadline| now >= deadline);
        let keep = !presentation_timed_out && !retired;
        if retired && let Some(surface) = note.claimed_by {
            unshown.push((note.launch_metric_id, note.key.clone(), surface));
        }
        // A clean parent's handoff deadline can precede the presentation
        // deadline. Report its unclaimed launch too, so the saved opening
        // card is answered rather than losing the only expiry observer.
        // A live view that never opened says so where it is drawn
        // (`live_embeds`), not in a notification of a launch the person
        // never asked for.
        if (presentation_timed_out || (retired && note.claimed_by.is_none()))
            && note.embed.is_none()
        {
            expired.push(note.key.clone());
        }
        if !keep {
            lock(&session.launch_history)
                .event(note.launch_metric_id, launch_history::Phase::Retired);
            tilo_startup_metrics::launch_failed(note.launch_metric_id);
        }
        keep
    });
    for package_id in &expired {
        super::doc_open::forget_unpresented_documents(&session, package_id);
    }
    report_unshown_windows(&unshown);
    expired
}

/// Say what became of the buffer commits of each launched window whose claim
/// retired before it showed a frame: whether a commit applied, which are
/// still blocked on their capture, and where the capture worker has them.
/// Read outside the launch table's lock, as the frame store is locked by
/// painting.
fn report_unshown_windows(unshown: &[(u64, String, u64)]) {
    let store = crate::shell::foreign::store();
    for (launch_id, package_id, surface_id) in unshown {
        if store.frame_seq(*surface_id) != 0 {
            continue;
        }
        tracing::info!(
            target: tilo_startup_metrics::LAUNCH_LATENCY_TARGET,
            launch_id,
            package_id = %package_id,
            surface_id,
            capture = %store.capture_report(*surface_id),
            "application window has no frame when its launch retired"
        );
    }
}

/// A normal desktop application should announce its first surface promptly.
/// Keep enough headroom for cold Electron startup and VM software rendering,
/// while still turning a dead launch into actionable feedback.
const PENDING_PRESENTATION_TIMEOUT: Duration =
    tilo_action_broker::services::APPLICATION_STARTUP_TIMEOUT;

/// Long enough for a daemonising desktop entry to hand off to its GUI child,
/// while keeping a genuinely windowless launch from steering a later app.
const PENDING_PLACEMENT_EXIT_GRACE: Duration = Duration::from_secs(10);

/// Enough for the rendered and protocol trees to observe the same newly
/// mapped surface, but finite so a claimed note cannot leak forever.
const PENDING_PLACEMENT_CLAIM_GRACE: Duration = Duration::from_secs(10);

/// Drop every placement note. Used by tests, which share this
/// process-wide state.
pub fn clear_pending_placements() {
    LaunchSession::with(|session| {
        let mut pending = lock(&session.pending);
        for note in pending.drain(..) {
            lock(&session.launch_history)
                .event(note.launch_metric_id, launch_history::Phase::Retired);
            tilo_startup_metrics::launch_failed(note.launch_metric_id);
        }
        drop(pending);
        lock(&session.awaiting_window).clear();
        lock(&session.unresolved_owners).clear();
        lock(&session.session_helpers).clear();
        lock(&session.restartable_helpers).clear();
        lock(&session.presented).clear();
    });
}

/// Test hook: publish a socket path directly (production goes through
/// [`set_wayland_socket`], which resolves it against `$XDG_RUNTIME_DIR`).
#[cfg(test)]
pub(crate) fn set_wayland_socket_path(path: Option<PathBuf>) {
    *WAYLAND_SOCKET.lock().expect("WAYLAND_SOCKET poisoned") = path;
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

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use crate::shell::state::WorkspaceExt as _;
use tilo_lock::MutexRecover as _;

#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
pub(crate) mod original_copy;
