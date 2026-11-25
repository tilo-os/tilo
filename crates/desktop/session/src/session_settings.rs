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
pub(crate) struct SessionSettings {
    settings: Arc<WorkspaceSettings>,
    attached: Cell<bool>,
    looked: Cell<Option<Instant>>,
    /// The workspaces the device's daemon holds, as it last answered: asked off the loop, `None`
    /// until it answered.
    held: Arc<std::sync::Mutex<Option<Vec<String>>>>,
    /// The revision of the settings, and the device they were read on, last left for the
    /// processes outside the session.
    published: std::cell::RefCell<Option<(u64, String)>>,
    /// Where what is left is written: a worker, so no pass of the session's loop reads or writes
    /// the file. `None` when the worker could not start, and the pass writes it itself.
    publisher: Option<mpsc::Sender<Publication>>,
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
    pub(crate) fn install() -> Self {
        let settings = Arc::new(
            settings_store::config_path(std::path::Path::new(settings_store::outside::WAITING))
                .map_or_else(WorkspaceSettings::new, WorkspaceSettings::kept_in),
        );
        settings_store::install_source(Some(
            Arc::clone(&settings) as Arc<dyn settings_store::SettingsSource>
        ));
        let publisher = start_publisher();
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
            published: std::cell::RefCell::new(None),
            publisher: publisher.ok(),
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
        self.settings.sync();
        if due {
            self.looked.set(Some(now));
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
        leave(self.publisher.as_ref(), publication);
    }
}

/// Hand `publication` to the worker, or, with none or one that stopped, leave it on this pass, as
/// before.
fn leave(publisher: Option<&mpsc::Sender<Publication>>, publication: Publication) {
    let Some(jobs) = publisher else {
        write(&publication);
        return;
    };
    if let Err(error) = jobs.send(publication) {
        // The worker stopped: nothing is left to write it, so this pass does.
        write(&error.0);
    }
}

/// The worker that leaves the settings for the processes outside the session, and the channel to
/// it: a burst of changes is written once, the latest.
fn start_publisher() -> std::io::Result<mpsc::Sender<Publication>> {
    let (jobs, work) = mpsc::channel::<Publication>();
    std::thread::Builder::new()
        .name("tilo-settings-published".into())
        .spawn(move || {
            while let Ok(mut latest) = work.recv() {
                while let Ok(newer) = work.try_recv() {
                    latest = newer;
                }
                write(&latest);
            }
        })?;
    Ok(jobs)
}

/// Leave `publication` for the processes outside the session.
fn write(publication: &Publication) {
    if let Err(error) = settings_store::outside::publish(&publication.device, &publication.settings)
    {
        tracing::warn!(%error, "the settings were not left for the processes outside the session");
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
                settings_store::workspace::APP.to_owned(),
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
