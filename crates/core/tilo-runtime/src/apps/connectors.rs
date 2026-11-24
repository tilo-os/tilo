//! The connectors the device runs for tilolang apps whose types hold an outside service's
//! resources (`docs/architecture/apps.md` §1 and §8; `docs/designs/app-language/README.md`,
//! "origins").
//!
//! A connector is the app's own launch with no window (`tilo-launcher run
//! --tilolang-connector <origin>`, [`super::linux::connector`]): the launcher gives it exactly the
//! sandbox its signed manifest declares for that origin — the network and the ownership the
//! tilolang process that runs its instances never has — and the app's runtime runs its origin's
//! connector there.
//!
//! One connector runs per app, origin and workspace. It starts the first time an instance needs
//! it, whether or not anyone is signed in at a desktop, and stays up when that instance goes, so
//! what the service says while nothing shows the app waits in it. Each instance attaches over a
//! connection of its own: one end is handed to the instance, the other to the connector over the
//! socket it took with its launch ([`runtime_portal::origin_connector`]).

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::process::Child;
use std::sync::Mutex;

use runtime_portal::origin_connector;
use tilo_lock::MutexRecover as _;

/// Which connector: the app, its origin and the workspace.
type Key = (String, String, String);

/// A running connector: the device's end of its socket, and its process.
struct Running {
    socket: OwnedFd,
    child: Child,
}

/// What hands a connector whose manifest grants it the workspace's account sign-ins those
/// sign-ins, over its socket, before its first reader: the connector's socket, its app and the
/// workspace.
pub(crate) type SignIns = dyn Fn(&OwnedFd, &str, &str) -> std::io::Result<()> + Send + Sync;

/// The connectors this device runs, by app, origin and workspace.
#[derive(Default)]
pub(crate) struct Connectors {
    running: Mutex<HashMap<Key, Running>>,
    sign_ins: std::sync::OnceLock<Box<SignIns>>,
}

impl Connectors {
    /// Hand each connector whose manifest grants it its workspace's account sign-ins through
    /// `sign_ins` from now on.
    pub(crate) fn hand_sign_ins(&self, sign_ins: Box<SignIns>) {
        let _ = self.sign_ins.set(sign_ins);
    }

    /// A connection an instance of `app` in `workspace` reaches the connector of `origin` over,
    /// starting the connector when none runs for this app, origin and workspace. `start` starts
    /// the connector's process, handed the socket given; `signs_in` says whether its manifest
    /// grants it the workspace's account sign-ins.
    ///
    /// # Errors
    /// The connection cannot be made, the connector cannot be started, or it needs sign-ins this
    /// device hands no connector.
    pub(crate) fn connection(
        &self,
        (app, origin, workspace): (&str, &str, &str),
        signs_in: bool,
        start: &dyn Fn(&OwnedFd) -> Result<Child, String>,
    ) -> Result<OwnedFd, String> {
        let (instance, theirs) = origin_connector::connection()
            .map_err(|error| format!("no connection to {app}'s {origin} connector: {error}"))?;
        let key = (app.to_owned(), origin.to_owned(), workspace.to_owned());
        let mut running = self.running.lock_recover();
        if let Some(connector) = running.get(&key) {
            match origin_connector::send_attachment(&connector.socket, &theirs) {
                Ok(()) => return Ok(instance),
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
        let attached = (|| {
            if signs_in {
                let sign_ins = self.sign_ins.get().ok_or_else(|| {
                    format!(
                        "{app}'s {origin} connector needs its workspace's sign-ins, which this \
                         device hands no connector"
                    )
                })?;
                sign_ins(&ours, app, workspace).map_err(|error| {
                    format!("{app}'s {origin} connector took no account sign-ins: {error}")
                })?;
            }
            origin_connector::send_attachment(&ours, &theirs)
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
        Ok(instance)
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

/// Reap `child` on a thread of its own; where none can start, here.
fn reap(child: Child) {
    let slot = std::sync::Arc::new(Mutex::new(Some(child)));
    let theirs = std::sync::Arc::clone(&slot);
    let spawned = std::thread::Builder::new()
        .name("tilo-reaper".to_owned())
        .spawn(move || {
            if let Some(mut child) = theirs.lock_recover().take() {
                let _ = child.wait();
            }
        });
    if spawned.is_err()
        && let Some(mut child) = slot.lock_recover().take()
    {
        let _ = child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A connector process that keeps its socket and reads what it is sent until it closes.
    fn connector_process(socket: &OwnedFd) -> Result<Child, String> {
        use std::os::fd::AsFd as _;
        let socket = socket
            .as_fd()
            .try_clone_to_owned()
            .map_err(|e| e.to_string())?;
        std::process::Command::new("/bin/sh")
            .args(["-c", "exec cat >/dev/null"])
            .stdin(std::process::Stdio::from(socket))
            .spawn()
            .map_err(|error| error.to_string())
    }

    /// One connector runs per app, origin and workspace: the second instance attaches to the
    /// one the first started, and another workspace starts its own.
    #[test]
    fn one_connector_runs_per_app_origin_and_workspace() {
        let connectors = Connectors::default();
        let started = AtomicUsize::new(0);
        let start = |socket: &OwnedFd| {
            started.fetch_add(1, Ordering::SeqCst);
            connector_process(socket)
        };
        for workspace in ["one", "one", "two"] {
            connectors
                .connection(("so.tilo.mail", "imap", workspace), false, &start)
                .unwrap();
        }
        assert_eq!(started.load(Ordering::SeqCst), 2);
    }

    /// A connector that needs its workspace's sign-ins does not start where nothing hands them,
    /// and a launch that fails starts nothing.
    #[test]
    fn a_connector_that_cannot_start_runs_nowhere() {
        let connectors = Connectors::default();
        let refused = connectors
            .connection(("so.tilo.mail", "imap", "one"), true, &connector_process)
            .unwrap_err();
        assert!(
            refused.contains("needs its workspace's sign-ins"),
            "{refused}"
        );
        let refused = connectors
            .connection(("so.tilo.mail", "imap", "one"), false, &|_| {
                Err("so.tilo.mail is not installed here".to_owned())
            })
            .unwrap_err();
        assert!(refused.contains("not installed"), "{refused}");
        assert!(connectors.running.lock_recover().is_empty());
    }
}
