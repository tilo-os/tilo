//! The connectors the system runs for tilolang apps whose types hold an outside service's
//! resources (`docs/architecture/apps.md` §1 and §8; `docs/designs/app-language/README.md`,
//! "origins").
//!
//! A connector is the app's own launch with no window (`tilo-launcher run
//! --tilolang-connector <origin>`): the launcher gives it exactly the sandbox its signed
//! manifest declares for that origin — the network and the custody the tilolang host that runs
//! its instances never has — and the app's runtime runs its origin's connector there
//! (`tilolang_mail::run_connector`, `tilolang_youtube::connector::run`).
//!
//! One connector runs per app, origin and workspace. It starts the first time something needs
//! it and stays up when that goes, so what the service says while nothing shows the app waits
//! in it. Each reader attaches over a connection of its own: one end is handed to the reader,
//! the other to the connector over the door it took with its launch
//! ([`runtime_portal::origin_connector`]).
//!
//! Nothing here is per app: which origins an app has is its manifest's, and what a connector
//! does is the app runtime's.

use std::collections::HashMap;
use std::os::fd::AsFd as _;
use std::os::fd::OwnedFd;
use std::process::{Child, Command, Stdio};

use tilo_app_launch::Planned;
use tilo_lock::MutexRecover as _;

use runtime_portal::origin_connector;

/// Which connector: the app, its origin and the workspace.
type Key = (String, String, String);

/// A running connector: the session's end of its door.
struct Running {
    door: OwnedFd,
    /// How the connector's process is ended when the session ends.
    child: Child,
}

/// The connectors this session runs, by app, origin and workspace.
#[derive(Default)]
pub struct ConnectorHost {
    running: std::sync::Mutex<HashMap<Key, Running>>,
}

impl ConnectorHost {
    /// No connector runs yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A connection a reader of `app`'s records in `workspace` reaches the connector of
    /// `origin` over, starting the connector when none runs for this app, origin and
    /// workspace. `plan` builds the connector's launch, as the app's own with `origin` named;
    /// it is called only when a connector has to start.
    ///
    /// # Errors
    /// The connection cannot be made, the connector cannot be started, or its door is closed
    /// and it cannot be started again.
    pub fn connection(
        &self,
        app: &str,
        origin: &str,
        workspace: &str,
        plan: &dyn Fn(&str) -> Result<Planned, String>,
    ) -> Result<OwnedFd, String> {
        let (instance, theirs) = origin_connector::connection()
            .map_err(|error| format!("no connection to {app}'s {origin} connector: {error}"))?;
        let key = (app.to_owned(), origin.to_owned(), workspace.to_owned());
        let mut running = self.running.lock_recover();
        if let Some(connector) = running.get(&key) {
            match origin_connector::send_attachment(&connector.door, &theirs) {
                Ok(()) => return Ok(instance),
                Err(error) => {
                    tracing::info!(%error, app, origin, "an origin connector's door is closed");
                    running.remove(&key);
                }
            }
        }
        let (ours, door) = origin_connector::pair()
            .map_err(|error| format!("no door for {app}'s {origin} connector: {error}"))?;
        let planned = plan(origin)?;
        let child = Self::start(&planned, &door)?;
        origin_connector::send_attachment(&ours, &theirs)
            .map_err(|error| format!("{app}'s {origin} connector took no connection: {error}"))?;
        running.insert(key, Running { door: ours, child });
        drop(running);
        Ok(instance)
    }

    /// Start the connector `planned` ran, releasing the door `door` to it through the launcher's
    /// registration gate, and following it on a thread of its own.
    fn start(planned: &Planned, door: &OwnedFd) -> Result<Child, String> {
        tilo_app_launch::name_surface(planned);
        let mut command = Command::new(&planned.program);
        command
            .args(&planned.args)
            .env_clear()
            .envs(&planned.env)
            .stdin(Stdio::null())
            // The connector's own account of a service that refused it: the session's log keeps it,
            // the same as a window's app's.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let gate = launcher::registration_gate::install(&mut command)
            .map_err(|error| format!("{} has no launch gate: {error}", planned.app))?;
        let mut child = command
            .spawn()
            .map_err(|error| format!("{}'s connector did not start: {error}", planned.app))?;
        drop(command);
        // The connector is a launch of the app's own, and says what went wrong on its standard
        // streams: the session's log keeps them.
        let output = std::sync::Arc::new(tilo_app_launch::output::AppOutput::new(
            planned.app.clone(),
            None,
        ));
        if let Some(stdout) = child.stdout.take() {
            output.relay_stdout(stdout);
        }
        if let Some(stderr) = child.stderr.take() {
            output.relay_stderr(stderr);
        }
        // The connector's door reaches it as a window's does: the launcher hands it into the
        // sandbox as the connector's own service.
        gate.release_with_window_door(door.as_fd())
            .map_err(|error| format!("{}'s connector was not released: {error}", planned.app))?;
        tracing::info!(app = %planned.app, argv = %planned.describe(), "an origin connector started");
        Ok(child)
    }
}

impl Drop for ConnectorHost {
    fn drop(&mut self) {
        let started = std::mem::take(&mut *self.running.lock_recover());
        for (_, mut running) in started {
            let _ = running.child.kill();
            let _ = running.child.wait();
        }
    }
}

/// Start the connectors the session's services keep records in step with
/// (`docs/architecture/apps.md` §1, §8): for each origin a service names, the process of the app
/// whose package declares it, in the sandbox its signed manifest gives that origin, and the service
/// reached through it.
/// A connector is the app's own launch with no window (`--tilolang-connector <origin>`): it holds
/// the network and the custody the session and the tilolang host never have. It runs whether or not
/// a window of the app is open, so what the service says meanwhile waits in it. A session that
/// cannot start one says why and goes on: the app's records then stand as they are, and its views
/// show what they last held.
pub(crate) fn start_all(session: &crate::Session) {
    // An origin's own crate registers how the platform reaches its connector, so a session reaches
    // one without naming the crate that keeps the service.
    tilolang_mail::register();
    tilolang_youtube::register();
    let wake = session.shared.wake_handle.clone();
    // The service that keeps an origin in step is told what wakes the loop: an answer from the
    // service it asked, which the next pass polls and writes.
    session.shared.services.borrow_mut().wake_origins(&wake);
    let connectors = session.shared.services.borrow().connectors();
    if connectors.is_empty() {
        return;
    }
    let Some(apps) = session.shared.launch.clone() else {
        return;
    };
    let workspace = apps.workspace_named(&session.shared.active_workspace_id());
    for connector in connectors {
        let reached = session
            .connectors
            .connection(
                &connector.app,
                &connector.origin,
                &workspace.id,
                &|origin| apps.plan_connector(&connector.app, origin, &workspace),
            )
            .and_then(|connection| {
                {
                    let mut services = session.shared.services.borrow_mut();
                    tilo_service::Service::connect(&mut *services, &connector.origin, connection)
                }
                .map_err(|error| {
                    format!(
                        "{}'s {} is not reached: {error}",
                        connector.app, connector.origin
                    )
                })
            });
        if let Err(error) = reached {
            tracing::warn!(
                app = %connector.app,
                origin = %connector.origin,
                %error,
                "an outside origin's connector does not run: its records stand as they are"
            );
        }
    }
}
