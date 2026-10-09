//! Per-attempt startup observations owned by one desktop session.
//!
//! Only an adopted window whose kernel peer credentials descend from the
//! recorded launcher can settle a launch. Application names are presentation,
//! not evidence that a particular invocation started.
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tilo_action_broker::services::LaunchReadiness;

const MAX_PENDING: usize = 32;
pub(super) const TOKEN_ENV: &str = "TILO_LAUNCH_STARTUP_TOKEN";

thread_local! {
    static CURRENT: Cell<Option<u64>> = const { Cell::new(None) };
}

struct Scope(Option<u64>);

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|current| current.set(self.0));
    }
}

pub(super) fn observe(
    start: impl FnOnce() -> Result<(), String>,
) -> Result<LaunchReadiness, String> {
    let session = super::launcher::LaunchSession::current();
    let (token, readiness) = session.startup.reserve()?;
    let _scope = Scope(CURRENT.with(|current| current.replace(Some(token))));
    start()?;
    Ok(readiness)
}

pub(super) fn attach(command: &mut super::launcher::LaunchCommand) {
    if let Some(token) = CURRENT.with(Cell::get) {
        command.env.push((TOKEN_ENV.to_string(), token.to_string()));
    }
}

/// Move only the existing observation scope to the preflight worker. This is
/// not startup success: the same PID/start and adoption checks still settle it.
pub(super) fn inherit<T>(start: impl FnOnce() -> T) -> impl FnOnce() -> T {
    let token = CURRENT.with(Cell::get);
    move || {
        let _scope = Scope(CURRENT.with(|current| current.replace(token)));
        start()
    }
}

pub(super) fn fail_current(detail: String) {
    super::launcher::LaunchSession::current()
        .startup
        .fail(CURRENT.with(Cell::get), detail);
}

struct Pending {
    process: Option<(u32, u64)>,
    reply: tokio::sync::oneshot::Sender<Result<(), String>>,
}

#[derive(Clone, Default)]
pub(super) struct StartupObservations(Arc<Mutex<HashMap<u64, Pending>>>);

impl StartupObservations {
    pub fn reserve(&self) -> Result<(u64, LaunchReadiness), String> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let mut pending = self
            .0
            .lock()
            .map_err(|_| "startup observations unavailable")?;
        if pending.len() >= MAX_PENDING {
            return Err("too many applications are waiting to start".into());
        }
        let token = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .map_err(|_| "application startup identity exhausted")?;
        let (reply, receiver) = tokio::sync::oneshot::channel();
        pending.insert(
            token,
            Pending {
                process: None,
                reply,
            },
        );
        drop(pending);
        let weak = Arc::downgrade(&self.0);
        Ok((
            token,
            LaunchReadiness::new(receiver, move || {
                let removed = weak
                    .upgrade()
                    .and_then(|observations| observations.lock().ok()?.remove(&token));
                drop(removed);
            }),
        ))
    }

    pub fn started(&self, token: Option<u64>, pid: u32) {
        let Some(token) = token else { return };
        let Some((_, start)) = super::launcher::proc_stat(pid) else {
            self.fail(Some(token), "the application's launcher disappeared".into());
            return;
        };
        if let Ok(mut pending) = self.0.lock()
            && let Some(observation) = pending.get_mut(&token)
        {
            observation.process = Some((pid, start));
        }
    }

    pub fn fail(&self, token: Option<u64>, detail: String) {
        let observation = token.and_then(|token| self.0.lock().ok()?.remove(&token));
        if let Some(observation) = observation {
            let _ = observation.reply.send(Err(detail));
        }
    }

    pub fn adopted(&self, mut pid: u32) {
        // Match the same bounded ancestry and PID-reuse rule as the launcher's
        // ordinary service and Wayland admission. Never hold our lock during
        // filesystem reads or while waking a receiver.
        for _ in 0..32 {
            let Some((parent, start)) = super::launcher::proc_stat(pid) else {
                return;
            };
            let observation = self.0.lock().ok().and_then(|mut pending| {
                let token = pending.iter().find_map(|(token, entry)| {
                    (entry.process == Some((pid, start))).then_some(*token)
                })?;
                pending.remove(&token)
            });
            if let Some(observation) = observation {
                let _ = observation.reply.send(Ok(()));
                return;
            }
            if parent == 0 || parent == pid {
                return;
            }
            pid = parent;
        }
    }
}

pub(super) fn command_token(command: &super::launcher::LaunchCommand) -> Option<u64> {
    command
        .env
        .iter()
        .find_map(|(name, value)| (name == TOKEN_ENV).then(|| value.parse().ok()).flatten())
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    struct ProcessTree(Child);
    impl Drop for ProcessTree {
        fn drop(&mut self) {
            let _ = Command::new("/bin/kill")
                .args(["-TERM", "--", &format!("-{}", self.0.id())])
                .status();
            let _ = self.0.wait();
        }
    }

    #[tokio::test]
    async fn preflight_worker_inherits_only_scope_and_still_requires_actual_adoption() {
        let mut process = ProcessTree(
            Command::new("/bin/sh")
                .args(["-c", "sleep 30 & echo $!; wait"])
                .process_group(0)
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut output = BufReader::new(process.0.stdout.take().unwrap());
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let descendant: u32 = line.trim().parse().unwrap();
        let launcher_pid = process.0.id();
        let session = super::super::launcher::LaunchSession::current();
        let readiness = observe(|| {
            let worker = super::super::launcher::spawn_reporting(
                std::thread::Builder::new(),
                inherit(move || {
                    let token = CURRENT
                        .with(Cell::get)
                        .expect("worker must retain exact attempt");
                    super::super::launcher::LaunchSession::current()
                        .startup
                        .started(Some(token), launcher_pid);
                }),
            )
            .unwrap();
            worker.join().unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(CURRENT.with(Cell::get), None);
        assert_eq!(session.startup.0.lock().unwrap().len(), 1);
        session.startup.adopted(std::process::id());
        assert_eq!(session.startup.0.lock().unwrap().len(), 1);
        session.startup.adopted(descendant);
        assert_eq!(readiness.wait().await, Ok(()));
        assert!(session.startup.0.lock().unwrap().is_empty());
        drop(process);
    }

    #[tokio::test]
    async fn inherited_preflight_failure_is_terminal_without_a_spawn() {
        let session = super::super::launcher::LaunchSession::current();
        let readiness = observe(|| {
            super::super::launcher::spawn_reporting(
                std::thread::Builder::new(),
                inherit(|| fail_current("canonical original refused".into())),
            )
            .unwrap()
            .join()
            .unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(
            readiness.wait().await.unwrap_err(),
            "canonical original refused"
        );
        assert_eq!(CURRENT.with(Cell::get), None);
        assert!(session.startup.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn startup_requires_the_registered_process_ancestry_and_terminal_is_final() {
        let observations = StartupObservations::default();
        let (token, readiness) = observations.reserve().unwrap();
        let mut process = ProcessTree(
            Command::new("/bin/sh")
                .args(["-c", "sleep 30 & echo $!; wait"])
                .process_group(0)
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut output = BufReader::new(process.0.stdout.take().unwrap());
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let descendant: u32 = line.trim().parse().unwrap();
        observations.started(Some(token), process.0.id());
        observations.adopted(std::process::id());
        assert_eq!(observations.0.lock().unwrap().len(), 1);
        observations.adopted(descendant);
        observations.fail(Some(token), "late process exit".into());
        assert_eq!(readiness.wait().await, Ok(()));
        assert!(observations.0.lock().unwrap().is_empty());
        drop(process); // Reap the owned process group on success and panic alike.
    }

    #[tokio::test]
    async fn spawn_failure_is_terminal_and_dropping_a_receipt_releases_capacity() {
        let observations = StartupObservations::default();
        let (token, readiness) = observations.reserve().unwrap();
        observations.fail(Some(token), "the signed launcher could not start".into());
        observations.adopted(std::process::id());
        assert_eq!(
            readiness.wait().await.unwrap_err(),
            "the signed launcher could not start"
        );
        let reservations: Vec<_> = (0..MAX_PENDING)
            .map(|_| observations.reserve().unwrap())
            .collect();
        assert!(observations.reserve().is_err());
        drop(reservations);
        assert!(observations.0.lock().unwrap().is_empty());
        assert!(observations.reserve().is_ok());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_launcher_spawn_refusal_completes_the_observed_attempt() {
        let readiness = observe(|| {
            super::super::launcher::spawn_launch(
                "so.tilo.missing-launcher".into(),
                "Missing launcher".into(),
                super::super::launcher::LaunchCommand {
                    program: std::path::PathBuf::from("/nonexistent/tilo-launcher-startup-test"),
                    args: Vec::new(),
                    env: Vec::new(),
                    service_identity: None,
                    portal_workspace_id: None,
                    managed_agent_id: None,
                },
            )
        })
        .unwrap();
        let error = tokio::time::timeout(std::time::Duration::from_secs(5), readiness.wait())
            .await
            .expect("the actual spawn failure must resolve startup")
            .unwrap_err();
        assert!(
            error.contains("No such file") || error.contains("not found"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_reused_pid_cannot_adopt_a_different_registered_process_identity() {
        let observations = StartupObservations::default();
        let (token, readiness) = observations.reserve().unwrap();
        observations.started(Some(token), std::process::id());
        observations
            .0
            .lock()
            .unwrap()
            .get_mut(&token)
            .unwrap()
            .process
            .as_mut()
            .unwrap()
            .1 += 1;
        observations.adopted(std::process::id());
        assert_eq!(observations.0.lock().unwrap().len(), 1);
        drop(readiness);
        assert!(observations.0.lock().unwrap().is_empty());
    }
}
