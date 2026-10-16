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

/// Run `start` as one identified launch attempt, without observing its
/// startup: the attempt's token, which the launch record of the process it
/// starts keeps, so the broker can tie that process's connection back to
/// this launch. Correlation only; it settles nothing.
pub(super) fn identified(start: impl FnOnce() -> Result<(), String>) -> Result<u64, String> {
    let token = next_token()?;
    let _scope = Scope(CURRENT.with(|current| current.replace(Some(token))));
    start()?;
    Ok(token)
}

/// A token no other attempt of this session holds, observed or not.
fn next_token() -> Result<u64, String> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
        next.checked_add(1)
    })
    .map_err(|_| "application startup identity exhausted".into())
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
    adoption: tokio::sync::oneshot::Sender<Option<tilo_action_broker::services::LaunchAdoption>>,
}

#[derive(Clone, Default)]
pub(super) struct StartupObservations(Arc<Mutex<HashMap<u64, Pending>>>);

impl StartupObservations {
    pub fn reserve(&self) -> Result<(u64, LaunchReadiness), String> {
        let mut pending = self
            .0
            .lock()
            .map_err(|_| "startup observations unavailable")?;
        if pending.len() >= MAX_PENDING {
            return Err("too many applications are waiting to start".into());
        }
        let token = next_token()?;
        let (reply, receiver) = tokio::sync::oneshot::channel();
        let (adoption, adopted) = tokio::sync::oneshot::channel();
        pending.insert(
            token,
            Pending {
                process: None,
                reply,
                adoption,
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
            })
            .with_adoption(adopted),
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

    #[cfg(test)]
    pub fn adopted(&self, pid: u32) {
        self.adopted_window(pid, None);
    }

    pub fn adopted_window(
        &self,
        mut pid: u32,
        adoption: Option<tilo_action_broker::services::LaunchAdoption>,
    ) {
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
                let _ = observation.adoption.send(adoption);
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

    #[tokio::test]
    async fn exact_adoption_returns_public_window_identity_without_kernel_pid() {
        let observations = StartupObservations::default();
        let (token, ready) = observations.reserve().unwrap();
        observations.started(Some(token), std::process::id());
        let window = tilo_action_broker::services::LaunchAdoption {
            adoption_id: uuid::Uuid::new_v4(),
            surface_id: 55,
            package_id: "so.tilo.calculator".into(),
            room: "room".into(),
        };
        observations.adopted_window(std::process::id(), Some(window.clone()));
        assert_eq!(
            ready.wait_with_adoption().await.unwrap(),
            Some(window.clone())
        );
        let encoded = serde_json::to_value(window).unwrap();
        assert_eq!(encoded.as_object().unwrap().len(), 4);
        assert!(encoded.get("pid").is_none());
        assert!(observations.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn stale_launcher_lifetime_cannot_produce_adoption_provenance() {
        let observations = StartupObservations::default();
        let (token, ready) = observations.reserve().unwrap();
        let pid = std::process::id();
        observations.started(Some(token), pid);
        {
            let mut pending = observations.0.lock().unwrap();
            let entry = pending.get_mut(&token).unwrap();
            let (pid, start) = entry.process.unwrap();
            entry.process = Some((pid, start.checked_add(1).unwrap()));
            drop(pending);
        }
        observations.adopted_window(
            pid,
            Some(tilo_action_broker::services::LaunchAdoption {
                adoption_id: uuid::Uuid::new_v4(),
                surface_id: 55,
                package_id: "so.tilo.calculator".into(),
                room: "room".into(),
            }),
        );
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                ready.wait_with_adoption()
            )
            .await
            .is_err()
        );
        assert!(observations.0.lock().unwrap().is_empty());
    }
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
