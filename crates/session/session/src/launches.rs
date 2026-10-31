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
//!
//! An app's later window opens in the process the session started for the
//! first, and the session makes that window's connections itself. Those
//! connections are recorded apart from the launch they belong to: a window's
//! own is attested as that window, spent by the one connection naming it,
//! and only while the launch it belongs to runs.

use std::collections::BTreeMap;
use std::sync::Mutex;

use tilo_action_broker::services::{
    AuthenticatedApplicationLaunch, LaunchPlace, LaunchableApplication, LauncherServices,
};
use tilo_app_launch::doors::ancestry;
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

/// One window of a running app the session is connecting: the window's own
/// instance, and the launch whose process it opens in.
struct Window {
    app: String,
    room: String,
    instance: u64,
    /// The process of the launch it belongs to, while that runs.
    launch: Option<u32>,
}

/// The launches this session made, and the window connections it is making
/// for them.
#[derive(Default)]
pub struct Launches {
    started: Mutex<BTreeMap<u32, Launch>>,
    windows: Mutex<Vec<Window>>,
    /// The launch each window's shell-ui connection belongs to, by app, while
    /// the connection is being made: the shell-ui service reads it for the
    /// connection from the session's own process.
    shell_ui: Mutex<Vec<(String, u32)>>,
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

    /// Record that the session is about to connect window `instance` of `app`
    /// in `room`, a window of the launch whose process is `launch` while it
    /// runs. A window has one record: recording it again replaces the one it
    /// had. Only the session records windows, and only the connection naming
    /// the instance spends the record.
    pub fn window(&self, app: &str, room: &str, instance: u64, launch: Option<u32>) {
        let mut windows = self.windows.lock_recover();
        windows.retain(|window| !window.is(app, room, instance));
        windows.push(Window {
            app: app.to_owned(),
            room: room.to_owned(),
            instance,
            launch,
        });
    }

    /// The session gave the connection it recorded up before the broker could
    /// attest it: its record ends.
    pub fn window_withdrawn(&self, app: &str, room: &str, instance: u64) {
        self.windows
            .lock_recover()
            .retain(|window| !window.is(app, room, instance));
    }

    /// Record that the session is about to connect the shell-ui socket for a
    /// window of `app` the launch in `launcher` runs: the shell-ui service
    /// reads it for a connection from the session's own process.
    pub fn window_shell_ui(&self, app: &str, launcher: u32) {
        self.shell_ui
            .lock_recover()
            .push((app.to_owned(), launcher));
    }

    /// The launch a connection from the session's own process for a window of
    /// `app` belongs to: the one it recorded while making the connection.
    pub fn take_window_shell_ui(&self, app: &str) -> Option<u32> {
        let mut pending = self.shell_ui.lock_recover();
        let index = pending.iter().position(|(asked, _)| asked == app)?;
        Some(pending.remove(index).1)
    }

    /// The room of the launch that `pid` is or was started by, when it runs
    /// `app`: the app a connection came from, as the window it names.
    pub fn room_of(&self, pid: u32, app: &str) -> Option<String> {
        let lineage = ancestry(pid);
        let launched = self
            .started
            .lock_recover()
            .iter()
            .find(|(launched, launch)| launch.app == app && lineage.contains(launched))
            .map(|(launched, launch)| (*launched, launch.start, launch.room.clone()))?;
        let (launched, start, room) = launched;
        (start_time(launched) == Some(start)).then_some(room)
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

    /// The window record a connection from the session's own process naming
    /// `app` in `room` and `instance` spends, while the launch it belongs to
    /// runs. Nothing else spends it, and it is spent once.
    fn take_window(
        &self,
        pid: u32,
        app: &str,
        room: &str,
        instance: Option<u64>,
    ) -> Option<AuthenticatedApplicationLaunch> {
        if pid != std::process::id() {
            return None;
        }
        let instance = instance.filter(|instance| *instance != 0)?;
        let record = {
            let mut windows = self.windows.lock_recover();
            let index = windows
                .iter()
                .position(|window| window.is(app, room, instance))?;
            windows.remove(index)
        };
        if let Some(launch) = record.launch
            && self.live(launch, app, room).is_none()
        {
            // The window's launch is over: its windows went with it.
            return None;
        }
        Some(AuthenticatedApplicationLaunch {
            creation: None,
            continuation_instance: Some(instance),
            launch_ticket: None,
            windowless: false,
        })
    }
}

impl Window {
    fn is(&self, app: &str, room: &str, instance: u64) -> bool {
        self.app == app && self.room == room && self.instance == instance
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

    /// A launched app's record stays for its whole life; the session's own
    /// record for a window of it is spent by the one connection naming the
    /// window's instance.
    ///
    /// # Errors
    /// Reading the connection's creation evidence failed.
    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        room: &str,
        window_instance: Option<u64>,
    ) -> Result<Option<AuthenticatedApplicationLaunch>, String> {
        if self.live(pid, app, room).is_some() {
            return Ok(Some(AuthenticatedApplicationLaunch {
                creation: self.application_creation_evidence(pid, app, room)?,
                continuation_instance: None,
                launch_ticket: None,
                windowless: false,
            }));
        }
        Ok(self.take_window(pid, app, room, window_instance))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A window's own connection is spent by the one connection naming its
    /// app, room and instance, and only the session's own process makes one:
    /// nothing else is admitted by it, and nothing is left for a second.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_window_connection_is_spent_once_by_the_window_it_names() {
        let launches = Launches::default();
        let app = "so.tilo.code.window";
        let room = "room-window";
        let session = std::process::id();
        launches.window(app, room, 42, None);

        assert!(
            launches
                .authenticated_launch(session, app, "other-room", Some(42))
                .unwrap()
                .is_none(),
            "a window of another room"
        );
        assert!(
            launches
                .authenticated_launch(session, app, room, Some(41))
                .unwrap()
                .is_none(),
            "another window"
        );
        assert!(
            launches
                .authenticated_launch(u32::MAX, app, room, Some(42))
                .unwrap()
                .is_none(),
            "only the session connects a window"
        );

        let bound = launches
            .authenticated_launch(session, app, room, Some(42))
            .unwrap()
            .expect("the window's own connection");
        assert_eq!(bound.continuation_instance, Some(42));
        assert!(
            launches
                .authenticated_launch(session, app, room, Some(42))
                .unwrap()
                .is_none(),
            "spent by its connection"
        );
    }

    /// The session gave the connection up before the broker saw it: the
    /// record ends with it, and nothing is admitted for that window.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_withdrawn_window_connection_admits_nothing() {
        let launches = Launches::default();
        let app = "so.tilo.code.withdrawn";
        let room = "room-withdrawn";
        let session = std::process::id();
        launches.window(app, room, 9, None);
        launches.window_withdrawn(app, room, 9);
        assert!(
            launches
                .authenticated_launch(session, app, room, Some(9))
                .unwrap()
                .is_none()
        );
    }

    /// A window of a launch runs in that launch's process: once the process
    /// is over its windows are too, and their connections are nobody's.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_window_of_an_ended_launch_is_not_admitted() {
        let launches = Launches::default();
        let app = "so.tilo.code.ended";
        let room = "room-ended";
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("a process to launch");
        launches.started(child.id(), app, room);
        launches.window(app, room, 7, Some(child.id()));
        child.kill().ok();
        child.wait().ok();

        assert!(
            launches
                .authenticated_launch(std::process::id(), app, room, Some(7))
                .unwrap()
                .is_none(),
            "the launch that made the window is over"
        );
    }

    /// The shell-ui service learns which launch a window connection from the
    /// session's own process belongs to, once for each window it connects.
    #[test]
    fn a_window_s_shell_ui_connection_names_its_launch_once() {
        let launches = Launches::default();
        launches.window_shell_ui("so.tilo.notes", 1234);
        assert_eq!(launches.take_window_shell_ui("so.tilo.notes"), Some(1234));
        assert_eq!(
            launches.take_window_shell_ui("so.tilo.notes"),
            None,
            "spent by its connection"
        );
        assert_eq!(launches.take_window_shell_ui("so.tilo.code"), None);
    }
}
