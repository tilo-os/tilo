//! The held connections the device runs for tilolang apps: the process of an app's own that holds
//! what the app keeps open past one action. That is an outside service whose resources the app's
//! types hold (`docs/architecture/apps.md` §1 and §8; `docs/designs/app-language/README.md`,
//! "origins"), or work with no end the app keeps open, such as a language server
//! (`connections: ["lsp"]`, `tilolang_tilo::held`). It is the app's own process, never one several
//! apps share, so a vault item released to it reaches that app alone. An app whose manifest says
//! it keeps its connection on one device of its workspace (`[entrypoint] connection`) has it
//! started by that device's turn ([`super::connection`]); otherwise the first instance that needs
//! it starts it on the device it runs on.
//!
//! A held connection's process is the app's own launch with no window (`tilo-launcher run
//! --tilolang-connector <origin>`, [`super::linux::connector`]): the launcher gives it exactly the
//! sandbox its signed manifest declares for that origin — the network and the ownership the
//! tilolang process that runs its instances never has — and the app's runtime runs its origin's
//! connector there.
//!
//! One connector runs per app, origin and workspace. It starts the first time an instance needs
//! it, whether or not anyone is signed in at a desktop, and stays up when that instance goes, so
//! what the service says while nothing shows the app waits in it. Each reader (an instance, or a
//! service of the person's desktop session) attaches over a connection of its own: one end is
//! the reader's, the other is handed to the connector over the socket it took with its launch
//! ([`runtime_portal::origin_connector`]).

use std::collections::HashMap;
use std::sync::Mutex;

use runtime_portal::origin_connector;
use runtime_portal::process_link::Descriptor;
use tilo_lock::MutexRecover as _;

/// Which held connection: the app, its kind (an origin, or a kind of held connection) and the
/// workspace.
type Key = (String, String, String);

/// A held connection's process, as the platform's launcher started it.
#[cfg(any(target_os = "linux", target_os = "android", windows))]
pub(crate) type Process = std::process::Child;
/// A held connection's process, as the platform's launcher started it.
#[cfg(target_os = "macos")]
pub(crate) type Process = launcher_macos::LaunchedApp;

/// A running held connection: the device's end of its process's socket, and the process.
struct Running {
    socket: Descriptor,
    child: Process,
}

/// What hands a started connector its connection to the device's dispatch, before any reader:
/// the connector's socket, its app, its workspace and its pid.
pub type Actions = dyn Fn(&Descriptor, &str, &str, u32) -> std::io::Result<()> + Send + Sync;

/// The pid a started connector runs as, as the launch record names it: the system's own.
#[cfg(any(target_os = "linux", target_os = "android", windows))]
#[cfg_attr(
    any(target_os = "android", windows),
    expect(dead_code, reason = "Android and Windows start no connector process")
)]
pub(crate) fn pid(process: &Process) -> u32 {
    process.id()
}

/// [`pid`] where the launcher reports its pid as an `i32`, which names one only where it fits.
#[cfg(target_os = "macos")]
pub(crate) fn pid(process: &Process) -> Option<u32> {
    u32::try_from(process.pid()).ok()
}

/// The held connections this device runs, by app, kind and workspace.
#[derive(Default)]
pub(crate) struct Connectors {
    running: Mutex<HashMap<Key, Running>>,
    actions: std::sync::OnceLock<Box<Actions>>,
}

impl Connectors {
    /// Hand each connector this device starts the connection to its dispatch through `actions`
    /// from now on.
    pub(crate) fn hand_actions(&self, actions: Box<Actions>) {
        let _ = self.actions.set(actions);
    }

    /// Attach `theirs`, the connector's end of a reader's connection, to the connector of `app`'s
    /// `origin` in `workspace`, starting the connector when none runs for this app, origin and
    /// workspace. `start` starts the connector's process, handed the socket given.
    ///
    /// # Errors
    /// The connector cannot be started, or takes no connection.
    pub(crate) fn attach(
        &self,
        (app, origin, workspace): (&str, &str, &str),
        theirs: &Descriptor,
        start: &dyn Fn(&Descriptor) -> Result<Process, String>,
    ) -> Result<(), String> {
        let key = (app.to_owned(), origin.to_owned(), workspace.to_owned());
        // The lock is held across a start, so two readers never start two processes.
        let mut running = self.running.lock_recover();
        if let Some(connector) = running.get(&key) {
            match origin_connector::send_attachment(&connector.socket, theirs) {
                Ok(()) => return Ok(()),
                Err(error) => {
                    tracing::info!(%error, app, origin, "an origin connector's socket is closed");
                    if let Some(mut closed) = running.remove(&key) {
                        let _ = closed.child.kill();
                        reap(closed.child);
                    }
                }
            }
        }
        let (ours, socket) = origin_connector::pair()
            .map_err(|error| format!("no socket for {app}'s {origin} connector: {error}"))?;
        let mut child = start(&socket)?;
        drop(socket);
        #[cfg(target_os = "macos")]
        let child_pid = pid(&child).unwrap_or(0);
        #[cfg(not(target_os = "macos"))]
        let child_pid = pid(&child);
        let attached = (|| {
            // The connection to this device's dispatch comes before any reader's: what an app
            // that signs in calls the published actions over.
            let actions = self.actions.get().ok_or_else(|| {
                format!(
                    "{app} cannot reach {origin} yet: this device hands no app a connection to \
                     its actions"
                )
            })?;
            actions(&ours, app, workspace, child_pid).map_err(|error| {
                format!("{app}'s {origin} connector took no connection to this device: {error}")
            })?;
            origin_connector::send_attachment(&ours, theirs)
                .map_err(|error| format!("{app}'s {origin} connector took no connection: {error}"))
        })();
        if let Err(error) = attached {
            let _ = child.kill();
            reap(child);
            return Err(error);
        }
        tracing::info!(app, origin, workspace, "an origin connector started");
        running.insert(
            key,
            Running {
                socket: ours,
                child,
            },
        );
        drop(running);
        Ok(())
    }
}

impl Connectors {
    /// Whether a connector of `app` runs in `workspace` now.
    pub(crate) fn runs(&self, app: &str, workspace: &str) -> bool {
        let mut running = self.running.lock_recover();
        running.retain(|(of, _, inside), connector| {
            !(of == app && inside == workspace) || alive(&mut connector.child)
        });
        running
            .keys()
            .any(|(of, _, inside)| of == app && inside == workspace)
    }

    /// Stop every connector of `app` in `workspace`.
    pub(crate) fn stop(&self, app: &str, workspace: &str) {
        let stopped: Vec<Running> = {
            let mut running = self.running.lock_recover();
            let keys: Vec<Key> = running
                .keys()
                .filter(|(of, _, inside)| of == app && inside == workspace)
                .cloned()
                .collect();
            let stopped = keys.iter().filter_map(|key| running.remove(key)).collect();
            drop(running);
            stopped
        };
        for mut connector in stopped {
            tracing::info!(app, workspace, "an origin connector stopped");
            let _ = connector.child.kill();
            reap(connector.child);
        }
    }
}

impl Drop for Connectors {
    fn drop(&mut self) {
        let started = std::mem::take(&mut *self.running.lock_recover());
        for (_, mut running) in started {
            let _ = running.child.kill();
            reap(running.child);
        }
    }
}

/// Whether `process` still runs.
fn alive(process: &mut Process) -> bool {
    #[cfg(any(target_os = "linux", target_os = "android", windows))]
    {
        matches!(process.try_wait(), Ok(None))
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: signal 0 only asks whether the process exists; it sends nothing.
        unsafe { libc::kill(process.pid(), 0) == 0 }
    }
}

/// Reap `child` on a thread of its own; where none can start, here.
fn reap(child: Process) {
    let slot = std::sync::Arc::new(Mutex::new(Some(child)));
    let theirs = std::sync::Arc::clone(&slot);
    let spawned = std::thread::Builder::new()
        .name("tilo-reaper".to_owned())
        .spawn(move || {
            let taken = theirs.lock_recover().take();
            if let Some(mut child) = taken {
                let _ = child.wait();
            }
        });
    if spawned.is_err()
        && let Some(mut child) = slot.lock_recover().take()
    {
        let _ = child.wait();
    }
}

// The tests start each connector as a plain child process, as the Linux launcher's is.
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::fd::OwnedFd;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tilo_test_env::SpawnUnderEnvLock as _;

    /// A connector process that keeps its socket and reads what it is sent until it closes.
    fn connector_process(socket: &OwnedFd) -> Result<Process, String> {
        use std::os::fd::AsFd as _;
        let socket = socket
            .as_fd()
            .try_clone_to_owned()
            .map_err(|e| e.to_string())?;
        std::process::Command::new("/bin/sh")
            .args(["-c", "exec cat >/dev/null"])
            .stdin(std::process::Stdio::from(socket))
            .spawn_locked()
            .map_err(|error| error.to_string())
    }

    /// One connector runs per app, origin and workspace: the second instance attaches to the
    /// one the first started, and another workspace starts its own.
    #[test]
    fn one_connector_runs_per_app_origin_and_workspace() {
        let connectors = Connectors::default();
        connectors.hand_actions(Box::new(|_, _, _, _| Ok(())));
        let started = AtomicUsize::new(0);
        let start = |socket: &OwnedFd| {
            started.fetch_add(1, Ordering::SeqCst);
            connector_process(socket)
        };
        for workspace in ["one", "one", "two"] {
            let (_reader, theirs) = origin_connector::connection().unwrap();
            connectors
                .attach(("so.tilo.mail", "imap", workspace), &theirs, &start)
                .unwrap();
        }
        assert_eq!(started.load(Ordering::SeqCst), 2);
    }

    /// A connector does not start where this device hands no app its actions, and a launch that
    /// fails starts nothing.
    #[test]
    fn a_connector_that_cannot_start_runs_nowhere() {
        let connectors = Connectors::default();
        let (_reader, theirs) = origin_connector::connection().unwrap();
        let refused = connectors
            .attach(("so.tilo.mail", "imap", "one"), &theirs, &connector_process)
            .unwrap_err();
        assert!(
            refused.contains("hands no app a connection to its actions"),
            "{refused}"
        );
        connectors.hand_actions(Box::new(|_, _, _, _| Ok(())));
        let refused = connectors
            .attach(("so.tilo.mail", "imap", "one"), &theirs, &|_| {
                Err("so.tilo.mail is not installed here".to_owned())
            })
            .unwrap_err();
        assert!(refused.contains("not installed"), "{refused}");
        assert!(connectors.running.lock_recover().is_empty());
    }
}
