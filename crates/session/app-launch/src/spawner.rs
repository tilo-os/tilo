//! Starting a planned app, and hearing how it ended.
//!
//! The session runs one loop, so a launch must not wait: [`Spawner::start`]
//! starts the process and returns, and a thread of its own waits for the
//! child and reports how it ended on [`Spawner::events`]. Waiting for every
//! child on one thread would make each launch wait for the one before it to
//! exit, so each child gets a thread — an app is started rarely enough that
//! those are the only threads involved.
//!
//! What a session needs from the report is whether the app *chose* to end
//! ([`End::crashed`]): an app that exited with a failure or was killed left
//! its windows behind, and the person is offered the app again rather than
//! the desktop going down with it.
//!
//! An app that ended because memory ran out is not a crash the app caused:
//! the launcher ended it, and remembers why
//! ([`launcher::memory_pressure::Cause`]). The report carries that with the
//! status ([`End::memory`]), so the person is told the session closed the app
//! rather than that it stopped working.

use std::os::fd::AsFd as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use crate::doors::HeldDoor;
use crate::plan::Planned;

/// Why the system ended an app: what the launcher remembers of it.
pub use launcher::memory_pressure::Cause;

/// What wakes the session's loop when an app ends.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// How a launched app ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// It exited with this status.
    Exited {
        code: i32,
        /// Why memory ended it, when memory did.
        memory: Option<Cause>,
    },
    /// This signal ended it.
    Signalled {
        signal: i32,
        /// Why memory ended it, when memory did.
        memory: Option<Cause>,
    },
    /// It never started, and why.
    Failed(String),
}

impl End {
    /// An app that exited with `code` on its own.
    #[must_use]
    pub const fn exited(code: i32) -> Self {
        Self::Exited { code, memory: None }
    }

    /// An app this signal ended.
    #[must_use]
    pub const fn signalled(signal: i32) -> Self {
        Self::Signalled {
            signal,
            memory: None,
        }
    }

    /// An app that never started.
    #[must_use]
    pub fn failed(reason: impl Into<String>) -> Self {
        Self::Failed(reason.into())
    }

    /// The same end, with memory having closed the app (`cause`).
    #[must_use]
    pub fn remembering(mut self, cause: Cause) -> Self {
        match &mut self {
            Self::Exited { memory, .. } | Self::Signalled { memory, .. } => {
                *memory = Some(cause);
            }
            Self::Failed(_) => {}
        }
        self
    }

    /// Why memory ended the app, when it did: `None` for an app that chose to
    /// end, for one that never started, and for one the launcher holds no
    /// memory of.
    #[must_use]
    pub fn memory(&self) -> Option<Cause> {
        match self {
            Self::Exited { memory, .. } | Self::Signalled { memory, .. } => *memory,
            Self::Failed(_) => None,
        }
    }

    /// Whether the app ended without choosing to: anything but a clean exit
    /// is a crash, including one the app itself reports as a failure.
    #[must_use]
    pub fn crashed(&self) -> bool {
        !matches!(self, Self::Exited { code: 0, .. })
    }
}

impl std::fmt::Display for End {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exited { code, .. } => write!(f, "exited with {code}"),
            Self::Signalled { signal, .. } => write!(f, "was killed by signal {signal}"),
            Self::Failed(reason) => write!(f, "did not start: {reason}"),
        }
    }
}

/// A launched app that ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// The launch this is about, as [`Spawner::start`] returned it.
    pub id: u64,
    /// The app's package id or desktop-file id.
    pub app: String,
    /// The name the app was shown by.
    pub title: String,
    pub end: End,
}

/// A launched app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Started {
    pub id: u64,
    pub pid: u32,
}

/// Why an app did not start.
#[derive(Debug, thiserror::Error)]
#[error("{app} did not start: {reason}")]
pub struct SpawnError {
    pub app: String,
    pub reason: String,
}

/// The apps this session started, and what became of them.
pub struct Spawner {
    wake: Option<Wake>,
    events: (mpsc::Sender<Event>, Receiver<Event>),
    next: u64,
}

impl Default for Spawner {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Spawner {
    /// A spawner that wakes `wake`'s loop whenever an app ends.
    #[must_use]
    pub fn new(wake: Option<Wake>) -> Self {
        Self {
            wake,
            events: mpsc::channel(),
            next: 0,
        }
    }

    /// Start `planned` with exactly the environment it planned: nothing the
    /// session carries leaks in, and the app gets no terminal.
    ///
    /// # Errors
    /// The program could not be started.
    pub fn start(&mut self, planned: &Planned) -> Result<Started, SpawnError> {
        self.start_with(planned, None, &|_| {})
            .map(|(started, _)| started)
    }

    /// [`Self::start`] with the window door `door` of the app's room, which
    /// the app is handed with its release so it opens the app's later
    /// windows itself.
    ///
    /// The launcher waits at a registration gate until `record` has written
    /// the launch down: an app that connected before its launch was known
    /// would be refused by the session's services. A launch without a door
    /// gets no gate, and its command line is its stdin.
    ///
    /// The door comes back held for as long as the app runs, so the caller
    /// keeps it until the app ends.
    ///
    /// # Errors
    /// The program could not be started.
    pub fn start_with(
        &mut self,
        planned: &Planned,
        mut door: Option<HeldDoor>,
        record: &dyn Fn(u32),
    ) -> Result<(Started, Option<HeldDoor>), SpawnError> {
        let mut command = Command::new(&planned.program);
        command
            .args(&planned.args)
            .env_clear()
            .envs(&planned.env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let gate = match door {
            Some(_) => match launcher::registration_gate::install(&mut command) {
                Ok(gate) => Some(gate),
                // Without its gate the launch would run unreleased: it starts
                // nothing, as an app that cannot start does.
                Err(error) => {
                    return Err(SpawnError {
                        app: planned.app.clone(),
                        reason: format!("no launch gate: {error}"),
                    });
                }
            },
            None => None,
        };
        let child = command.spawn().map_err(|error| SpawnError {
            app: planned.app.clone(),
            reason: error.to_string(),
        })?;
        drop(command);
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        let started = Started {
            id,
            pid: child.id(),
        };
        record(started.pid);
        if let Some(gate) = gate {
            // The launch is written down: the app may connect.
            let served = door.as_mut().and_then(|door| door.serve(started.pid));
            let released = match &served {
                Some(theirs) => gate.release_with_window_door(theirs.as_fd()),
                None => gate.release(),
            };
            if let Err(error) = released {
                tracing::warn!(app = %planned.app, %error, "the launch was not released");
            }
        }
        let app = planned.app.clone();
        let title = planned.title.clone();
        let events = self.events.0.clone();
        let wake = self.wake.clone();
        std::thread::Builder::new()
            .name(format!("app-{}", planned.app))
            .spawn(move || {
                let end = ended(child);
                // A session that has stopped listening is going away; its
                // apps' endings are nobody's business then.
                let _ = events.send(Event {
                    id,
                    app,
                    title,
                    end,
                });
                if let Some(wake) = wake {
                    wake();
                }
            })
            .map_err(|error| SpawnError {
                app: planned.app.clone(),
                reason: format!("no thread to watch it: {error}"),
            })?;
        Ok((started, door))
    }

    /// The apps that ended since the last call, oldest first.
    pub fn events(&self) -> Vec<Event> {
        self.events.1.try_iter().collect()
    }

    /// Report `event` as if the app it names had ended: what the session
    /// hears of a launch it did not start itself.
    #[cfg(feature = "test-support")]
    pub fn report(&self, event: Event) {
        let _ = self.events.0.send(event);
    }
}

/// How `child` ended: its exit status, or the signal that killed it, with
/// why memory ended it when memory did.
fn ended(mut child: Child) -> End {
    // Asked before the child is reaped, when its cgroup can still be read:
    // the launcher moved it there before it started anything.
    let cgroup = app_cgroup(child.id());
    let end = match child.wait() {
        Ok(status) => {
            if let Some(code) = status.code() {
                End::exited(code)
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt as _;
                    match status.signal() {
                        Some(signal) => End::signalled(signal),
                        None => End::failed("it ended without a status"),
                    }
                }
                #[cfg(not(unix))]
                End::failed("it ended without a status")
            }
        }
        Err(error) => End::failed(error.to_string()),
    };
    if !end.crashed() {
        // The app ended as it meant to: memory did not close it.
        return end;
    }
    match cgroup.and_then(|cgroup| launcher::spawner::memory_end(&cgroup)) {
        Some(cause) => end.remembering(cause),
        None => end,
    }
}

/// The cgroup of the app `pid` runs in, when the launcher put it in one: the
/// spawner that keeps the app tree also remembers why it ended an app in it.
#[cfg(target_os = "linux")]
fn app_cgroup(pid: u32) -> Option<PathBuf> {
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    launcher::app_cgroup::app_cgroup_of(&cgroup)
        .map(|cgroup| launcher::app_cgroup::fs_path(&cgroup))
}

#[cfg(not(target_os = "linux"))]
fn app_cgroup(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use crate::plan::Kind;

    fn shell(script: &str, env: BTreeMap<String, String>) -> Planned {
        Planned {
            app: "org.example.Probe".into(),
            title: "Probe".into(),
            kind: Kind::System,
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), script.into()],
            env,
            window_identities: Vec::new(),
        }
    }

    /// Wait for `count` events, failing the test rather than hanging.
    fn events(spawner: &Spawner, count: usize) -> Vec<Event> {
        let started = Instant::now();
        let mut collected = Vec::new();
        while collected.len() < count {
            collected.extend(spawner.events());
            if collected.len() >= count {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "the app never reported how it ended"
            );
            std::thread::park_timeout(Duration::from_millis(5));
        }
        collected
    }

    #[test]
    fn an_app_starts_with_exactly_the_environment_it_planned() {
        let directory = tempfile::tempdir().expect("a directory");
        let written = directory.path().join("environment");
        let script = format!(
            "printf '%s\\n' \"$WAYLAND_DISPLAY\" \"$TILO_USER_ID\" \"$LEAK\" > {}",
            written.display()
        );
        let planned = shell(
            &script,
            BTreeMap::from([
                ("WAYLAND_DISPLAY".to_owned(), "wayland-6".to_owned()),
                ("TILO_USER_ID".to_owned(), "person-4".to_owned()),
            ]),
        );
        let mut spawner = Spawner::default();
        let started = spawner.start(&planned).expect("starts");
        assert_ne!(started.pid, 0);
        let event = events(&spawner, 1).remove(0);
        assert_eq!(event.app, "org.example.Probe");
        assert_eq!(event.title, "Probe");
        assert_eq!(event.end, End::exited(0));
        assert_eq!(event.end.memory(), None, "a clean exit is nobody's doing");
        assert!(!event.end.crashed(), "a clean exit is not a crash");
        assert_eq!(
            std::fs::read_to_string(&written).expect("the probe's environment"),
            "wayland-6\nperson-4\n\n",
            "the planned variables arrive and nothing else in the process's environment does"
        );
    }

    #[test]
    fn an_app_that_fails_or_is_killed_is_reported_as_a_crash() {
        let mut spawner = Spawner::default();
        let failing = spawner
            .start(&shell("exit 3", BTreeMap::new()))
            .expect("starts");
        let killed = spawner
            .start(&shell("kill -9 $$", BTreeMap::new()))
            .expect("starts");
        assert_ne!(failing.id, killed.id, "each launch is reported on its own");
        let mut events = events(&spawner, 2);
        events.sort_by_key(|event| event.id);
        assert_eq!(events[0].end, End::exited(3));
        assert!(events[0].end.crashed());
        assert_eq!(events[1].end, End::signalled(9));
        assert!(events[1].end.crashed());
        assert_eq!(events[1].end.to_string(), "was killed by signal 9");
        assert!(
            events.iter().all(|event| event.end.memory().is_none()),
            "a launcher that keeps no app tree remembers nothing"
        );
    }

    #[test]
    fn an_app_that_cannot_start_says_so_instead_of_starting_something_else() {
        let mut spawner = Spawner::default();
        let missing = Planned {
            program: PathBuf::from("/nonexistent/tilo-app"),
            ..shell("true", BTreeMap::new())
        };
        let error = spawner.start(&missing).expect_err("nothing starts");
        assert_eq!(error.app, "org.example.Probe");
        assert!(error.reason.contains("No such file"), "{error}");
        assert!(
            spawner.events().is_empty(),
            "no process, no ending to report"
        );
    }
}
