//! Launch records: what the shell started each process as, held for as
//! long as the process lives, and the records of the connections the
//! session makes from its own process for apps' windows. The broker attests
//! an app's connections against them.

use std::collections::HashMap;
use std::io;
use std::sync::Mutex;

use super::{ServiceLaunchIdentity, lock};
use crate::shell::host_runtime::proc_stat;

/// The launch record of a process the shell started: what it was started as,
/// bound to the process itself for as long as it lives. It is made before the
/// launcher's registration gate opens, and it ends when the process exits, as
/// an event ([`super::unregister_launched_process`]), never on a clock: a connection
/// the process makes at any point in its life, or one a process it started
/// inside its sandbox makes, is attested against it.
#[derive(Debug)]
pub(super) struct TrustedServiceLaunch {
    pub(super) start_time: u64,
    /// The process, held from its start: once it has exited the record
    /// attests nothing, even before its exit is reported.
    pub(super) process: LiveProcess,
    identity: ServiceLaunchIdentity,
    continuation_instance: Option<u64>,
    /// The broker's ticket for the launch that started this process: the
    /// startup token the compositor gave it, never read back from the app.
    launch_ticket: Option<u64>,
    /// The compatibility service took this launch's entry snapshot, which it
    /// does once per process ([`consume_trusted_service_launch_identity`]).
    compat_handoff_taken: bool,
}

impl TrustedServiceLaunch {
    /// Whether this is the record of the live process `pid`, which started
    /// at `start_time`, launched as `app_id` in `workspace_id`.
    fn attests(&self, pid: u32, start_time: u64, app_id: &str, workspace_id: &str) -> bool {
        self.start_time == start_time
            && self.process.alive()
            && proc_stat(pid).is_some_and(|(_, start)| start == self.start_time)
            && self.identity.app_id == app_id
            && self.identity.workspace_id == workspace_id
    }
}

/// A process the shell started, held by its pidfd. The kernel keeps the
/// pidfd pointing at that process even once its pid is reused.
#[derive(Debug)]
pub(super) struct LiveProcess {
    #[cfg(target_os = "linux")]
    pidfd: std::os::fd::OwnedFd,
}

impl LiveProcess {
    /// Hold `pid`, a child the shell has not reaped, so it is still the
    /// process the shell started.
    #[cfg(target_os = "linux")]
    fn hold(pid: u32) -> io::Result<Self> {
        let pid = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .ok_or(io::ErrorKind::InvalidInput)?;
        let pidfd = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())?;
        Ok(Self { pidfd })
    }
    #[cfg(not(target_os = "linux"))]
    fn hold(_pid: u32) -> io::Result<Self> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Whether the process is still running: its pidfd turns readable when
    /// it exits.
    #[cfg(target_os = "linux")]
    pub(super) fn alive(&self) -> bool {
        use std::os::fd::AsRawFd as _;
        let mut poll = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized pollfd naming an owned pidfd; a zero
        // timeout makes it a nonblocking query.
        unsafe { libc::poll(&raw mut poll, 1, 0) == 0 }
    }
    #[cfg(not(target_os = "linux"))]
    #[expect(
        clippy::unused_self,
        reason = "mirrors the Linux pidfd check, which reads the process handle"
    )]
    pub(super) const fn alive(&self) -> bool {
        false
    }
}

pub(super) static TRUSTED_SERVICE_LAUNCHES: std::sync::LazyLock<
    Mutex<HashMap<u32, TrustedServiceLaunch>>,
> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(any(test, target_os = "linux"))]
#[cfg(test)]
pub(super) fn register_trusted_service_launch(pid: u32, identity: ServiceLaunchIdentity) {
    register_trusted_window_launch(pid, identity, None, None);
}

pub(super) fn register_trusted_window_launch(
    pid: u32,
    identity: ServiceLaunchIdentity,
    continuation_instance: Option<u64>,
    launch_ticket: Option<u64>,
) {
    let Some((_, start_time)) = proc_stat(pid) else {
        return;
    };
    let process = match LiveProcess::hold(pid) {
        Ok(process) => process,
        Err(error) => {
            tracing::warn!(%error, pid, app_id = %identity.app_id, "a launch could not be bound to its process; its connections will be refused");
            return;
        }
    };
    lock(&TRUSTED_SERVICE_LAUNCHES).insert(
        pid,
        TrustedServiceLaunch {
            start_time,
            process,
            identity,
            continuation_instance,
            launch_ticket,
            compat_handoff_taken: false,
        },
    );
}

/// Launch records of the connections the session makes from its own
/// process for apps: one for each window it connects for an app, keyed by
/// that window's instance, and one for each connection it makes for an app
/// that has no window of its own. A record ends when the connection it was
/// made for is attested, or when the session gives that connection up.
struct InProcessLaunch {
    app_id: String,
    workspace_id: String,
    window_instance: Option<u64>,
    /// For an instance a tilolang host runs: the creation record and ticket
    /// of the launch whose instance it is ([`register_tilolang_instance`]).
    creation: Option<tilo_action_broker::services::ApplicationCreationEvidence>,
    launch_ticket: Option<u64>,
    /// For an instance of the app the session runs with no window
    /// ([`register_windowless_launch`]).
    windowless: bool,
}
static IN_PROCESS_LAUNCHES: Mutex<Vec<InProcessLaunch>> = Mutex::new(Vec::new());

/// Record that the session is about to connect the instance of `app_id` it
/// runs with no window in room `workspace_id`: the broker records the
/// connection as the app's, and as one that takes no `open`. Spent by that
/// connection.
pub(crate) fn register_windowless_launch(app_id: &str, workspace_id: &str) {
    lock(&IN_PROCESS_LAUNCHES).push(InProcessLaunch {
        app_id: app_id.to_owned(),
        workspace_id: workspace_id.to_owned(),
        window_instance: None,
        creation: None,
        launch_ticket: None,
        windowless: true,
    });
}

/// Record that the session itself is about to connect `app_id` in room
/// `workspace_id`, for no window of its own, as it records a process it
/// launched.
pub fn register_in_process_launch(app_id: &str, workspace_id: &str) {
    lock(&IN_PROCESS_LAUNCHES).push(InProcessLaunch {
        app_id: app_id.to_owned(),
        workspace_id: workspace_id.to_owned(),
        window_instance: None,
        creation: None,
        launch_ticket: None,
        windowless: false,
    });
}

/// Record that the session is about to connect `app_id` in room `workspace_id` to apply one
/// change at its record's origin: no window, carrying the creation record of the installed
/// app it verified, so the broker records the connection as it records an instance's. Spent
/// by that connection.
pub(crate) fn register_tilolang_change(
    app_id: &str,
    workspace_id: &str,
    creation: tilo_action_broker::services::ApplicationCreationEvidence,
) {
    lock(&IN_PROCESS_LAUNCHES).push(InProcessLaunch {
        app_id: app_id.to_owned(),
        workspace_id: workspace_id.to_owned(),
        window_instance: None,
        creation: Some(creation),
        launch_ticket: None,
        windowless: false,
    });
}

/// Record that the session is about to connect window `instance` of
/// `app_id` in room `workspace_id`. A window has one record: connecting it
/// again replaces the one it had.
pub(crate) fn register_in_process_window(app_id: &str, workspace_id: &str, instance: u64) {
    let mut launches = lock(&IN_PROCESS_LAUNCHES);
    launches.retain(|launch| launch.window_instance != Some(instance));
    launches.push(InProcessLaunch {
        app_id: app_id.to_owned(),
        workspace_id: workspace_id.to_owned(),
        window_instance: Some(instance),
        creation: None,
        launch_ticket: None,
        windowless: false,
    });
}

/// Record that the session is about to connect the instance `instance` of
/// `app_id` in room `workspace_id` that a tilolang host runs, for the launch
/// of the process `launched`: the instance's window, which the shell started
/// as that app in that room. The record carries that launch's creation
/// record and ticket, so the broker records the connection exactly as it
/// records a launched app's, and it is spent by the connection, as a
/// window's is. Whether `launched` is a live launch of that app in that
/// room: when it is not, nothing is recorded and the instance is not
/// connected. The host itself names nothing here.
pub(crate) fn register_tilolang_instance(
    launched: u32,
    app_id: &str,
    workspace_id: &str,
    instance: u64,
) -> bool {
    let Some((_, start_time)) = proc_stat(launched) else {
        return false;
    };
    let attested = lock(&TRUSTED_SERVICE_LAUNCHES)
        .get(&launched)
        .filter(|launch| launch.attests(launched, start_time, app_id, workspace_id))
        .map(|launch| {
            (
                launch.identity.application_creation.clone(),
                launch.launch_ticket,
            )
        });
    let Some((creation, launch_ticket)) = attested else {
        return false;
    };
    let mut records = lock(&IN_PROCESS_LAUNCHES);
    records.retain(|launch| launch.window_instance != Some(instance));
    records.push(InProcessLaunch {
        app_id: app_id.to_owned(),
        workspace_id: workspace_id.to_owned(),
        window_instance: Some(instance),
        creation,
        launch_ticket,
        windowless: false,
    });
    true
}

/// The session gave up the connection it recorded for `app_id` in
/// `workspace_id` (for `window_instance`, if for a window) before the broker
/// could attest it: its record ends.
pub(crate) fn withdraw_in_process_launch(
    app_id: &str,
    workspace_id: &str,
    window_instance: Option<u64>,
) {
    let mut launches = lock(&IN_PROCESS_LAUNCHES);
    if let Some(index) = launches
        .iter()
        .position(|launch| launch.is(app_id, workspace_id, window_instance))
    {
        launches.remove(index);
    }
}

impl InProcessLaunch {
    fn is(&self, app_id: &str, workspace_id: &str, window_instance: Option<u64>) -> bool {
        self.app_id == app_id
            && self.workspace_id == workspace_id
            && self.window_instance == window_instance
    }
}

/// Spend the session's launch record for a connection from its own process
/// naming `app_id` in room `workspace_id` and no window.
pub fn consume_in_process_launch(pid: u32, app_id: &str, workspace_id: &str) -> bool {
    take_in_process_window_launch(pid, app_id, workspace_id, None).is_some()
}

/// The launch a connection from process `pid` naming `app` in `room` (and,
/// for one the session makes for its own window, `window_instance`) comes
/// from. A launched process's record stays for its whole life; the
/// session's record for one of its own connections is spent by it.
pub(crate) fn authenticated_window_launch(
    pid: u32,
    app: &str,
    room: &str,
    window_instance: Option<u64>,
) -> Option<tilo_action_broker::services::AuthenticatedApplicationLaunch> {
    let (_, start_time) = proc_stat(pid)?;
    {
        let launches = lock(&TRUSTED_SERVICE_LAUNCHES);
        if let Some(launch) = launches.get(&pid)
            && launch.attests(pid, start_time, app, room)
        {
            return Some(
                tilo_action_broker::services::AuthenticatedApplicationLaunch {
                    creation: launch.identity.application_creation.clone(),
                    continuation_instance: launch.continuation_instance,
                    launch_ticket: launch.launch_ticket,
                    windowless: false,
                },
            );
        }
    }
    take_in_process_window_launch(pid, app, room, window_instance)
}

fn take_in_process_window_launch(
    pid: u32,
    app: &str,
    room: &str,
    window_instance: Option<u64>,
) -> Option<tilo_action_broker::services::AuthenticatedApplicationLaunch> {
    if pid != std::process::id() {
        return None;
    }
    let mut launches = lock(&IN_PROCESS_LAUNCHES);
    let index = launches
        .iter()
        .position(|launch| launch.is(app, room, window_instance))?;
    let launch = launches.remove(index);
    drop(launches);
    Some(
        tilo_action_broker::services::AuthenticatedApplicationLaunch {
            creation: launch.creation,
            continuation_instance: launch.window_instance,
            launch_ticket: launch.launch_ticket,
            windowless: launch.windowless,
        },
    )
}

/// Take the compositor's authority record for a launcher child, for the
/// compatibility service, which takes its entry snapshot once per process.
/// Taking it leaves the process's launch record in place for its other
/// connections. Kernel process facts corroborate the record but never
/// create authority.
pub fn consume_trusted_service_launch_identity(
    pid: u32,
    start_time: u64,
    app_id: &str,
    workspace_id: &str,
) -> Option<ServiceLaunchIdentity> {
    let mut launches = lock(&TRUSTED_SERVICE_LAUNCHES);
    let identity = launches
        .get_mut(&pid)
        .filter(|launch| {
            !launch.compat_handoff_taken && launch.attests(pid, start_time, app_id, workspace_id)
        })
        .map(|launch| {
            launch.compat_handoff_taken = true;
            launch.identity.clone()
        });
    drop(launches);
    identity
}

/// Compatibility service view of [`consume_trusted_service_launch_identity`].
/// The compatibility server only needs its fixed entry snapshot.
pub fn consume_trusted_service_launch(
    pid: u32,
    start_time: u64,
    app_id: &str,
    workspace_id: &str,
) -> Option<Vec<String>> {
    consume_trusted_service_launch_identity(pid, start_time, app_id, workspace_id)
        .map(|identity| identity.allowed_compat_entry_ids)
}

/// Take a service launch's compatibility handoff using the kernel's current
/// PID start-time. A direct `tilo-launcher` invocation has no compositor record,
/// and a reused PID cannot inherit a dead launch's authority.
pub fn consume_trusted_service_launch_for_pid(
    pid: u32,
    app_id: &str,
    workspace_id: &str,
) -> Option<ServiceLaunchIdentity> {
    let (_, start_time) = proc_stat(pid)?;
    consume_trusted_service_launch_identity(pid, start_time, app_id, workspace_id)
}

/// Verify the shell-owned process record for a service descriptor, as often
/// as the process connects for as long as it lives.
pub fn attest_trusted_service_launch_for_pid(
    pid: u32,
    app_id: &str,
    workspace_id: &str,
) -> Option<ServiceLaunchIdentity> {
    let (_, start_time) = proc_stat(pid)?;
    let launches = lock(&TRUSTED_SERVICE_LAUNCHES);
    let identity = launches.get(&pid).and_then(|launch| {
        launch
            .attests(pid, start_time, app_id, workspace_id)
            .then(|| launch.identity.clone())
    });
    // Keep the registry lock out of the return expression. This function is
    // called while service descriptors are being inspected, and no caller
    // needs the registry held while it handles the resulting identity.
    drop(launches);
    identity
}
