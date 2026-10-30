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

use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use crate::plan::Planned;

/// What wakes the session's loop when an app ends.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// How a launched app ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// It exited with this status.
    Exited(i32),
    /// This signal ended it.
    Signalled(i32),
    /// It never started, and why.
    Failed(String),
}

impl End {
    /// Whether the app ended without choosing to: anything but a clean exit
    /// is a crash, including one the app itself reports as a failure.
    #[must_use]
    pub fn crashed(&self) -> bool {
        !matches!(self, Self::Exited(0))
    }
}

impl std::fmt::Display for End {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exited(code) => write!(f, "exited with {code}"),
            Self::Signalled(signal) => write!(f, "was killed by signal {signal}"),
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
        let mut command = Command::new(&planned.program);
        command
            .args(&planned.args)
            .env_clear()
            .envs(&planned.env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command.spawn().map_err(|error| SpawnError {
            app: planned.app.clone(),
            reason: error.to_string(),
        })?;
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        let started = Started {
            id,
            pid: child.id(),
        };
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
        Ok(started)
    }

    /// The apps that ended since the last call, oldest first.
    pub fn events(&self) -> Vec<Event> {
        self.events.1.try_iter().collect()
    }
}

/// How `child` ended: its exit status, or the signal that killed it.
fn ended(mut child: Child) -> End {
    match child.wait() {
        Ok(status) => {
            if let Some(code) = status.code() {
                End::Exited(code)
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt as _;
                    match status.signal() {
                        Some(signal) => End::Signalled(signal),
                        None => End::Failed("it ended without a status".into()),
                    }
                }
                #[cfg(not(unix))]
                End::Failed("it ended without a status".into())
            }
        }
        Err(error) => End::Failed(error.to_string()),
    }
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
        assert_eq!(event.end, End::Exited(0));
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
        assert_eq!(events[0].end, End::Exited(3));
        assert!(events[0].end.crashed());
        assert_eq!(events[1].end, End::Signalled(9));
        assert!(events[1].end.crashed());
        assert_eq!(events[1].end.to_string(), "was killed by signal 9");
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
