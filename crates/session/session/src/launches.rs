//! The apps this session started: what a connection to the session's services
//! is admitted by.
//!
//! An app reaches its session's socket with the launcher's handoff, which
//! names the app and the room it was launched into. The handoff alone grants
//! nothing: the broker admits the connection only for a launch its host
//! recorded ([`LauncherServices`]), and the record is here — made when the
//! session started the app, keyed by the process it started. The process's
//! start time is part of the record, so a later process that happened to
//! reuse the pid is not taken for the launch.

use std::collections::BTreeMap;
use std::sync::Mutex;

use tilo_action_broker::services::{LaunchPlace, LaunchableApplication, LauncherServices};
use tilo_desktop_model::Workspace;
use tilo_lock::MutexRecover as _;

/// One launch the session made: which app it started, in which room, and the
/// process that runs it.
struct Launch {
    app: String,
    room: String,
    /// The process's start time, in clock ticks since boot.
    start: u64,
}

/// The launches this session made, by the pid each started.
#[derive(Default)]
pub struct Launches {
    started: Mutex<BTreeMap<u32, Launch>>,
}

impl Launches {
    /// Record that `pid` runs `app` in `room`, as this session started it: the
    /// app's connection is admitted from here. A pid whose process cannot be
    /// read is recorded by nothing and starts an app that reaches no service.
    pub fn started(&self, pid: u32, app: &str, room: &str) {
        let Some(start) = start_time(pid) else {
            tracing::warn!(
                pid,
                %app,
                "a started app's process could not be read: its connection reaches no service"
            );
            return;
        };
        self.started.lock_recover().insert(
            pid,
            Launch {
                app: app.to_owned(),
                room: room.to_owned(),
                start,
            },
        );
    }

    /// The start time of the process `pid` started, when that process is the
    /// launch of `app` in `room` and still runs.
    fn live(&self, pid: u32, app: &str, room: &str) -> Option<u64> {
        let start = match self.started.lock_recover().get(&pid) {
            Some(launch) if launch.app == app && launch.room == room => launch.start,
            _ => return None,
        };
        (start_time(pid) == Some(start)).then_some(start)
    }
}

impl LauncherServices for Launches {
    /// This session starts apps from its desktop, not on a service's behalf:
    /// what a service can open is nothing.
    fn applications(&self) -> Vec<LaunchableApplication> {
        Vec::new()
    }

    fn launch(
        &self,
        _app: &str,
        _workspace: &Workspace,
        _record: Option<&str>,
        _place: &LaunchPlace,
    ) -> Result<(), String> {
        Err("this session starts no app on a service's behalf".into())
    }

    fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
        self.live(pid, app, room).is_some()
    }

    /// The connection proved the process the session started is the one
    /// holding it (the kernel's credentials and the challenge answered on
    /// them): the launch stands only while both name it.
    fn attest_executing_process(
        &self,
        launcher_pid: u32,
        peer: &runtime_portal::process_binding::ExecutingPeer,
        app: &str,
        room: &str,
    ) -> bool {
        self.live(launcher_pid, app, room)
            .is_some_and(|start| peer.belongs_to(launcher_pid, start))
    }
}

/// A process's start time, in clock ticks since boot, read from
/// `/proc/<pid>/stat`: what tells a process from a later one that reused its
/// pid.
fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm is parenthesised and may hold spaces or parentheses: index from
    // the last ')'. Field 3 (state) comes first, so starttime is field 19.
    let rest = stat.rsplit_once(") ")?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    fields.get(19)?.parse().ok()
}
