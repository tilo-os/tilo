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

/// What hands a connector whose manifest grants it the workspace's account sign-ins those
/// sign-ins, over its socket, before its first reader: the connector's socket, its app and the
/// workspace.
pub type SignIns = dyn Fn(&Descriptor, &str, &str) -> std::io::Result<()> + Send + Sync;

/// The pid a started connector runs as, as the launch record names it; `None` where the system
/// does not say it in a pid this daemon records.
#[cfg_attr(
    any(target_os = "android", windows),
    expect(dead_code, reason = "Android and Windows start no connector process")
)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "a Mac's launch record may name no pid this daemon records"
)]
pub(crate) fn pid(process: &Process) -> Option<u32> {
    #[cfg(any(target_os = "linux", target_os = "android", windows))]
    {
        Some(process.id())
    }
    #[cfg(target_os = "macos")]
    {
        u32::try_from(process.pid()).ok()
    }
}

/// The held connections this device runs, by app, kind and workspace.
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

    /// Attach `theirs`, the connector's end of a reader's connection, to the connector of `app`'s
    /// `origin` in `workspace`, starting the connector when none runs for this app, origin and
    /// workspace. `start` starts the connector's process, handed the socket given; `signs_in` says
    /// whether its manifest grants it the workspace's account sign-ins.
    ///
    /// # Errors
    /// The connector cannot be started, takes no connection, or needs sign-ins this device hands
    /// no connector yet.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the lock is held across a start, so two readers never start two processes"
    )]
    pub(crate) fn attach(
        &self,
        (app, origin, workspace): (&str, &str, &str),
        signs_in: bool,
        theirs: &Descriptor,
        start: &dyn Fn(&Descriptor) -> Result<Process, String>,
    ) -> Result<(), String> {
        let key = (app.to_owned(), origin.to_owned(), workspace.to_owned());
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
        let attached = (|| {
            if signs_in {
                // What a person sees when an app that signs in to their accounts is opened on a
                // device that cannot give it those sign-ins yet.
                let sign_ins = self.sign_ins.get().ok_or_else(|| {
                    format!(
                        "{app} cannot reach {origin} yet: it signs in with this workspace's \
                         accounts, and this device cannot give an app account sign-ins yet"
                    )
                })?;
                sign_ins(&ours, app, workspace).map_err(|error| {
                    format!("{app}'s {origin} connector took no account sign-ins: {error}")
                })?;
            }
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
        let started = AtomicUsize::new(0);
        let start = |socket: &OwnedFd| {
            started.fetch_add(1, Ordering::SeqCst);
            connector_process(socket)
        };
        for workspace in ["one", "one", "two"] {
            let (_reader, theirs) = origin_connector::connection().unwrap();
            connectors
                .attach(("so.tilo.mail", "imap", workspace), false, &theirs, &start)
                .unwrap();
        }
        assert_eq!(started.load(Ordering::SeqCst), 2);
    }

    /// A connector that needs its workspace's sign-ins does not start where nothing hands them,
    /// and a launch that fails starts nothing.
    #[test]
    fn a_connector_that_cannot_start_runs_nowhere() {
        let connectors = Connectors::default();
        let (_reader, theirs) = origin_connector::connection().unwrap();
        let refused = connectors
            .attach(
                ("so.tilo.mail", "imap", "one"),
                true,
                &theirs,
                &connector_process,
            )
            .unwrap_err();
        assert!(
            refused.contains("cannot give an app account sign-ins"),
            "{refused}"
        );
        let refused = connectors
            .attach(("so.tilo.mail", "imap", "one"), false, &theirs, &|_| {
                Err("so.tilo.mail is not installed here".to_owned())
            })
            .unwrap_err();
        assert!(refused.contains("not installed"), "{refused}");
        assert!(connectors.running.lock_recover().is_empty());
    }

    /// A vault item released for a held connection reaches the app's own connection process and
    /// nothing else: the sign-ins it is released over are handed to the process of the app whose
    /// manifest grants them, before any instance attaches, and never to another app's process or
    /// to an instance's end, which is the shared host's.
    #[test]
    fn a_released_secret_reaches_only_its_apps_own_connection_process() {
        use std::io::{Read as _, Write as _};
        use std::os::fd::AsFd as _;
        const SECRET: &str = "bot-token-7f3a";
        let connectors = Connectors::default();
        connectors.hand_sign_ins(Box::new(|connector, app, workspace| {
            // As the daemon answers them: a connection of the sign-ins, scoped to this app and
            // workspace, on which it releases what the app is granted.
            let (ours, theirs) = tilo_socket::pair()?;
            origin_connector::send_sign_ins(
                connector,
                runtime_portal::packet::descriptor_of(theirs),
            )?;
            let mut ours = ours;
            ours.write_all(format!("{SECRET} for {app} in {workspace}\n").as_bytes())
        }));
        // Each held connection's process end, as the launcher hands it over.
        let ends: Mutex<Vec<(String, OwnedFd)>> = Mutex::default();
        let mut instances = Vec::new();
        for (app, signs_in) in [("so.tilo.discord", true), ("so.tilo.notes", false)] {
            let start = |socket: &OwnedFd| {
                let end = socket
                    .as_fd()
                    .try_clone_to_owned()
                    .map_err(|e| e.to_string())?;
                ends.lock_recover().push((app.to_owned(), end));
                std::process::Command::new("/bin/sh")
                    .args(["-c", "exec sleep 600"])
                    .stdin(std::process::Stdio::null())
                    .spawn()
                    .map_err(|error| error.to_string())
            };
            let (instance, theirs) = origin_connector::connection().unwrap();
            connectors
                .attach((app, "held", "one"), signs_in, &theirs, &start)
                .unwrap();
            instances.push(instance);
        }
        let ends = ends.into_inner().unwrap();
        let end = |app: &str| &ends.iter().find(|(of, _)| of == app).unwrap().1;
        // The granted app's process is handed the sign-ins first, and reads its secret there.
        let Some(origin_connector::Handed::SignIns(mut sign_ins)) =
            origin_connector::receive(end("so.tilo.discord")).unwrap()
        else {
            panic!("the granted app's process is handed its sign-ins before any instance");
        };
        let mut released = [0; 64];
        let read = sign_ins.read(&mut released).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&released[..read]),
            format!("{SECRET} for so.tilo.discord in one\n")
        );
        // Another app's process is handed its instance and nothing to sign in with.
        assert!(matches!(
            origin_connector::receive(end("so.tilo.notes")).unwrap(),
            Some(origin_connector::Handed::Reader(_))
        ));
        // An instance's end, in the host every app's instances share, is told nothing.
        for instance in instances {
            let mut instance = std::os::unix::net::UnixStream::from(instance);
            instance.set_nonblocking(true).unwrap();
            let mut heard = [0; 64];
            let heard = match instance.read(&mut heard) {
                Ok(read) => String::from_utf8_lossy(&heard[..read]).into_owned(),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => String::new(),
                Err(error) => panic!("an instance's end: {error}"),
            };
            assert!(
                !heard.contains(SECRET),
                "an instance hears no secret: {heard}"
            );
        }
    }
}
