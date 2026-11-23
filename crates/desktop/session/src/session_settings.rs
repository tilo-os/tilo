//! The person's and the device's settings as resources of their workspaces
//! ([`settings_store::workspace`]): the session is their provider, writing each change
//! stamped with its setting's verb.
//!
//! The person's sections are kept in the workspace whose only member is them, which every
//! device of theirs takes part in, so what they set on one device is what the next reads; the
//! device's are kept in its owner's under `devices/<device>/settings`. Until the session holds
//! that workspace, a setting nobody chose reads as its default, and what is set meanwhile is kept
//! on the device and written once it does.

use std::cell::Cell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use settings_store::placement::Holder;
use settings_store::workspace::{Stores, WorkspaceSettings};
use tilo_peer_state::workspace_documents::DeviceDocuments as _;
use tilo_resource_store::wire::{Command, Reply};
use tilo_session_client::daemon_documents::{DaemonDocuments, DaemonShell};

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
    /// The revision of the settings, and the device they were read on, last left for the
    /// processes outside the session.
    published: std::cell::RefCell<Option<(u64, String)>>,
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
        Self {
            settings,
            attached: Cell::new(false),
            looked: Cell::new(None),
            published: std::cell::RefCell::new(None),
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
            // Asking the daemon which workspaces it holds is a round trip: once a look.
            if !due || !sealing(&workspace) {
                return;
            }
            self.looked.set(Some(now));
            if !DaemonDocuments(daemon.clone())
                .workspaces()
                .contains(&workspace)
            {
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

    /// Leave the settings as they are now, read on `device`, for the processes outside the
    /// session (the lock screen, the panels, the device's daemons), which read them in processes
    /// of their own ([`settings_store::outside`]).
    fn publish(&self, device: String) {
        let revision = settings_store::SettingsSource::revision(self.settings.as_ref());
        let now = Some((revision, device));
        if *self.published.borrow() == now {
            return;
        }
        let document = settings_store::SettingsSource::load(self.settings.as_ref());
        let device = now
            .as_ref()
            .map(|(_, device)| device.as_str())
            .unwrap_or_default();
        match settings_store::outside::publish(device, &document) {
            Ok(()) => *self.published.borrow_mut() = now,
            Err(error) => {
                tracing::warn!(%error, "the settings were not left for the processes outside the session");
            }
        }
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
