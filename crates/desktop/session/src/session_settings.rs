//! The person's and the device's settings as resources of their workspaces
//! ([`settings_sections::workspace`]): the session is their provider, writing each change
//! stamped with its setting's verb.
//!
//! The person's sections are kept in the workspace whose only member is them, which every
//! device of theirs takes part in, so what they set on one device is what the next reads; the
//! device's are kept in its owner's under `devices/<device>/settings`. Until the session holds
//! that workspace, a setting nobody chose reads as its default, and what is set meanwhile is kept
//! on the device and written once it does.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use collab_types::store::wire::{Command, Reply};
use serde_json::{Map, Value};
use settings_sections::workspace::{Stores, WorkspaceSettings};
use settings_store::placement::Holder;
use tilo_session_client::daemon_documents::DaemonShell;

/// How long the settings go between looks at what another device or an agent changed.
const LOOK_EVERY: Duration = Duration::from_millis(400);

/// The workspace whose only member is the person, which every device of theirs takes part in
/// from sign-in.
pub(crate) fn persons_workspace(person: &[u8; 32]) -> String {
    tilo_server_protocol::servers::person_workspace_id(person)
}

/// The session's settings, and when it last looked at what moved in the store.
pub struct SessionSettings {
    settings: Arc<WorkspaceSettings>,
    attached: Cell<bool>,
    looked: Cell<Option<Instant>>,
    /// The workspaces the device's daemon holds, as it last answered: asked off the loop, `None`
    /// until it answered.
    held: Arc<std::sync::Mutex<Option<Vec<String>>>>,
    /// Whether a read of the settings is with the device's daemon now: one at a time, on a thread
    /// of its own.
    reading: Arc<std::sync::atomic::AtomicBool>,
    /// The revision of the settings, and the device they were read on, last left for the
    /// processes outside the session.
    published: std::cell::RefCell<Option<(u64, String)>>,
    /// Where what is left is written: a worker, so no pass of the session's loop reads or writes
    /// the file. `None` when the worker could not start, and the pass writes it itself.
    publisher: Option<mpsc::Sender<Publication>>,
    /// Why the settings were last not left where the processes outside the session read them,
    /// until the loop tells the person: a change they made would not reach their other devices.
    failed: Arc<std::sync::Mutex<Option<String>>>,
}

/// What the session leaves for the processes outside it, as the worker writes it.
struct Publication {
    /// The device the settings were read on, by its key in lowercase hex.
    device: String,
    settings: Map<String, Value>,
}

impl SessionSettings {
    /// The settings of this session, installed as this process's: everything the session runs
    /// in its own process reads them.
    #[must_use]
    pub fn install() -> Self {
        let settings = Arc::new(
            settings_store::config_path(std::path::Path::new(settings_store::outside::WAITING))
                .map_or_else(WorkspaceSettings::new, WorkspaceSettings::kept_in),
        );
        settings_store::install_source(Some(
            Arc::clone(&settings) as Arc<dyn settings_store::SettingsSource>
        ));
        let failed: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
        let publisher = start_publisher(Arc::clone(&failed));
        if let Err(error) = &publisher {
            tracing::warn!(
                %error,
                "the settings have no worker to leave them to: the loop writes them"
            );
        }
        Self {
            settings,
            attached: Cell::new(false),
            looked: Cell::new(None),
            held: Arc::default(),
            reading: Arc::default(),
            published: std::cell::RefCell::new(None),
            publisher: publisher.ok(),
            failed,
        }
    }

    /// Keep the settings in `person`'s workspace once the device's daemon holds it and this
    /// device seals writes to it (`sealing`), carry what was set since the last pass there, and
    /// take in what moved there now and then. Until then what is set is kept on the device, as
    /// before the workspace came.
    pub(crate) fn follow(
        &self,
        daemon: &DaemonShell,
        identity: Option<([u8; 32], [u8; 32])>,
        sealing: &dyn Fn(&str) -> bool,
        wake: &tilo_service::Wake,
    ) {
        self.publish(
            identity
                .map(|(_, device)| hex::encode(device))
                .unwrap_or_default(),
        );
        let now = Instant::now();
        let due = self
            .looked
            .get()
            .is_none_or(|looked| now.duration_since(looked) >= LOOK_EVERY);
        if !self.attached.get() {
            let Some((person, device)) = identity else {
                return;
            };
            let workspace = persons_workspace(&person);
            // Asking the daemon which workspaces it holds is a round trip: once a look, off the
            // loop, which is woken by the answer and reads it on the pass after.
            let holds = self
                .held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .is_some_and(|held| held.contains(&workspace));
            if !holds {
                if due && sealing(&workspace) {
                    self.looked.set(Some(now));
                    self.ask_held(daemon, wake);
                }
                return;
            }
            if !sealing(&workspace) {
                return;
            }
            self.settings.attach(Arc::new(SessionStores {
                daemon: daemon.clone(),
                workspace,
                person,
                device,
                wake: Arc::clone(wake),
            }));
            self.attached.set(true);
            return;
        }
        if due {
            self.looked.set(Some(now));
            self.read_off_the_loop(wake);
        }
    }

    /// Carry what was set and take in what moved, on a thread of its own: each of the two is a
    /// round trip to the device's daemon, and the loop must never wait on one. The next pass reads
    /// what it left, woken by its answer; one read is in flight at a time.
    fn read_off_the_loop(&self, wake: &tilo_service::Wake) {
        use std::sync::atomic::Ordering;
        if self.reading.swap(true, Ordering::AcqRel) {
            return;
        }
        let (settings, wake, finished) = (
            Arc::clone(&self.settings),
            Arc::clone(wake),
            Arc::clone(&self.reading),
        );
        let spawned = std::thread::Builder::new()
            .name("settings-read".into())
            .spawn(move || {
                settings.sync();
                if settings.refresh() {
                    wake();
                }
                finished.store(false, Ordering::Release);
            });
        if let Err(error) = spawned {
            self.reading.store(false, Ordering::Release);
            tracing::warn!(%error, "the settings are read on the loop");
            self.settings.sync();
            self.settings.refresh();
        }
    }

    /// Ask the device's daemon which workspaces it holds, and wake the loop with its answer.
    fn ask_held(&self, daemon: &DaemonShell, wake: &tilo_service::Wake) {
        use runtime_collab::wire::shell::{ShellCall, ShellReply};
        let (asking, held, wake) = (daemon.clone(), Arc::clone(&self.held), Arc::clone(wake));
        daemon.runtime().spawn(async move {
            let workspaces = match asking.ask(ShellCall::Workspaces).await {
                Ok(ShellReply::Workspaces(workspaces)) => workspaces,
                Ok(other) => {
                    tracing::warn!(answer = ?other, "the device's workspaces did not read");
                    return;
                }
                Err(error) => {
                    tracing::warn!(%error, "the device's workspaces did not read");
                    return;
                }
            };
            *held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(workspaces);
            wake();
        });
    }

    /// Leave the settings as they are now, read on `device`, for the processes outside the
    /// session (the lock screen, the panels, the device's daemons), which read them in processes
    /// of their own ([`settings_store::outside`]). The worker writes them, so the pass that
    /// changed a setting never waits on the disk.
    fn publish(&self, device: String) {
        let revision = settings_store::SettingsSource::revision(self.settings.as_ref());
        let unchanged = self
            .published
            .borrow()
            .as_ref()
            .is_some_and(|(left, on)| *left == revision && *on == device);
        if unchanged {
            return;
        }
        let publication = Publication {
            device: device.clone(),
            settings: settings_store::SettingsSource::load(self.settings.as_ref()),
        };
        // The revision is kept as asked for: were the worker slow and the settings change again,
        // the next pass hands it the newer ones, and the worker writes only the latest.
        *self.published.borrow_mut() = Some((revision, device));
        leave(self.publisher.as_ref(), publication, &self.failed);
    }

    /// Why the settings were last not left for the processes outside the session, once: what the
    /// loop tells the person, since this computer's programs may act on what was set before.
    pub(crate) fn take_failure(&self) -> Option<String> {
        self.failed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

/// Hand `publication` to the worker, or, with none or one that stopped, leave it on this pass, as
/// before.
fn leave(
    publisher: Option<&mpsc::Sender<Publication>>,
    publication: Publication,
    failed: &std::sync::Mutex<Option<String>>,
) {
    let Some(jobs) = publisher else {
        write(&publication, failed);
        return;
    };
    if let Err(error) = jobs.send(publication) {
        // The worker stopped: nothing is left to write it, so this pass does.
        write(&error.0, failed);
    }
}

/// The worker that leaves the settings for the processes outside the session, and the channel to
/// it: a burst of changes is written once, the latest.
fn start_publisher(
    failed: Arc<std::sync::Mutex<Option<String>>>,
) -> std::io::Result<mpsc::Sender<Publication>> {
    let (jobs, work) = mpsc::channel::<Publication>();
    std::thread::Builder::new()
        .name("tilo-settings-published".into())
        .spawn(move || {
            while let Ok(mut latest) = work.recv() {
                while let Ok(newer) = work.try_recv() {
                    latest = newer;
                }
                write(&latest, &failed);
            }
        })?;
    Ok(jobs)
}

/// Leave `publication` for the processes outside the session, keeping why it could not be left
/// for the person.
fn write(publication: &Publication, failed: &std::sync::Mutex<Option<String>>) {
    let Err(error) = settings_store::outside::publish(&publication.device, &publication.settings)
    else {
        return;
    };
    tracing::warn!(%error, "the settings were not left for the processes outside the session");
    record(failed, &error.to_string());
}

/// Keep `error` for the person to be told, unless the first failure since the last telling is
/// already kept: a burst of failed writes is one thing that went wrong, not one per setting.
fn record(failed: &std::sync::Mutex<Option<String>>, error: &str) {
    let mut failed = failed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if failed.is_none() {
        *failed = Some(error.to_owned());
    }
}

/// The workspace the settings are kept in, as the device's daemon holds it.
struct SessionStores {
    daemon: DaemonShell,
    /// The person's own workspace: theirs, and, as the device's owner, the device's.
    workspace: String,
    person: [u8; 32],
    device: [u8; 32],
    wake: tilo_service::Wake,
}

impl Stores for SessionStores {
    fn command(&self, _holder: Holder, command: Command) -> Result<Reply, String> {
        self.daemon
            .runtime()
            .block_on(crate::broker_services::daemon_resource_command(
                &self.daemon,
                self.workspace.clone(),
                settings_store::workspace::SURFACE_APP.to_owned(),
                &command,
            ))
    }

    fn readers(&self, _holder: Holder) -> String {
        hex::encode(self.person)
    }

    fn device(&self) -> String {
        hex::encode(self.device)
    }

    fn wake(&self) {
        (self.wake)();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A burst of settings that could not be left is told once, by the first failure: telling the
    /// person once per setting would bury the last thing that went wrong under the first.
    #[test]
    fn the_first_failed_write_is_the_one_the_person_hears_about() {
        let failed: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
        record(&failed, "the disk is full");
        record(&failed, "the disk is full");
        record(&failed, "the file was closed");
        assert_eq!(
            failed.lock().expect("the failure").take().as_deref(),
            Some("the disk is full")
        );
        assert!(
            failed.lock().expect("the failure").is_none(),
            "told once, and taken by the telling"
        );
    }
}
