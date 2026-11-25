//! A workspace's VM device: one workspace, one record, no keys.
//!
//! A workspace hosted on a server that sells machines runs its Browser and its
//! Linux apps in the workspace's VM, whose device daemon joins the workspace
//! with a certificate for that one workspace alone, which is none of the
//! person's devices ([`docs/hosted-browser.md`](../../../../docs/hosted-browser.md)). Web pages
//! are strangers' code and Chromium runs a JIT on it, so a page that escapes
//! the engine holds that device's key: everything the device may reach is
//! everything such a page may read. The server therefore admits it as a device
//! of one workspace with one record — its Browser record — whose channel is
//! the only record channel it reaches, and whose window's page is the only
//! track it publishes.
//!
//! This register is the environment's half of the rule that confines an agent
//! to its delegation ([`crate::registry::AgentRegistry`]): the workspaces' VM
//! devices, by device key, with the one record each reaches, placed by the host
//! that placed the VM, from the certificate that reaches one workspace alone
//! ([`EnvironmentRegistry::confine`]). Nothing here is asked of the daemon in the VM:
//! [`crate::workspace_access::decide`] answers every protocol with it, the
//! state protocol's replay, its live fan-out, its record opening and its changes
//! ask it what a record is, and the live session offers such a device no one
//! else's tracks. A device that asked to be one is confined whether or not its
//! daemon cooperates.
//!
//! What it is given besides its record is the workspace's own document — the
//! layout every device needs to join at all — and its own device's presence on
//! the live session: never one record of the workspace's, its channels' keys, a
//! call, or another window's page.
//!
//! # Durable, and never wider than its scope
//!
//! The register is the server's own state as well ([`EnvironmentRegistry::at`]),
//! read back when the server starts: a server that restarts with a VM still placed confines it again, whatever the
//! workspace's persisted membership says.
//!
//! An environment that stops is a [`Environment::Stopped`] entry in the same
//! register, written before the device is delisted and kept until the
//! certificate it was given would have expired: a device whose environment
//! stopped is refused on every protocol — the certificate check every protocol and
//! every audit already makes asks this register — and a delisting that fails
//! leaves it refused rather than open. The register also counts the sessions
//! each environment device has here ([`EnvironmentRegistry::arrived`],
//! [`EnvironmentRegistry::departed`]); when the last one ends, the server that
//! placed it is told ([`EnvironmentRegistry::when_ended`]) so the environment
//! stops too, and the certificate's expiry bounds what a lost signal could.
//!
//! The one hole left is the register's own file: a server whose state root
//! loses it cannot tell a previous run's stopped VM from a live one.
//! [`EnvironmentRegistry::confine`] therefore places no new one until
//! the file reads again, so the hole never opens wider than what an operator
//! deleted.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use tilo_lock::RwLockRecover as _;

use crate::workspace_access::WorkspaceOperation;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

/// The app whose record a workspace's VM device reaches: Browser, which the VM's daemon runs.
pub const VM_APP: &str = tilo_app_id::apps::BROWSER;
/// The record of [`VM_APP`] a workspace's VM device reaches: one per workspace, so opening
/// Browser again attaches to the window already holding it.
pub const VM_RECORD: &str = "browser";
/// The record of [`VM_APP`] a workspace's VM session keeps its own state in, each setting under
/// `settings` by its name: what it shows and its wallpaper. The VM's disk keeps nothing a restart
/// needs, and the session never writes its person's own settings.
pub const ENVIRONMENT_RECORD: &str = "environment";

/// The records a workspace's VM device is placed for in `workspace`, as its certificate names
/// them: its Browser record ([`VM_RECORD`]) and its environment record ([`ENVIRONMENT_RECORD`]).
#[must_use]
pub fn vm_placed_records(workspace: WorkspaceId) -> tilo_user_identity::PlacedRecords {
    tilo_user_identity::PlacedRecords::new(
        workspace.as_uuid().into_bytes(),
        [
            document_channel(VM_APP, VM_RECORD),
            document_channel(VM_APP, ENVIRONMENT_RECORD),
        ],
    )
    .expect("a workspace's two VM records are well formed")
}

/// The field of [`ENVIRONMENT_RECORD`] that keeps the VM session's settings, each by its name.
pub const SETTINGS: &str = tilo_actions::remote::DEVICE_SETTINGS;

/// Whether `target` is a device's `settings#set`: the one action a session keeps what it needs
/// for its next one with, on any device. A device placed for records has no settings of its own
/// to keep: its host keeps them in the environment record for it ([`keep_setting`]).
#[must_use]
pub fn is_setting(target: &tilo_actions::remote::RemoteActionTarget) -> bool {
    target.provider == tilo_actions::remote::device_tickets::PROVIDER
        && target.address.resource == tilo_actions::remote::DEVICE_SETTINGS
        && target.address.action == "set"
}

/// `settings#set` asked by the placed device `placed` in `workspace`, with `arguments`: the
/// setting `name` kept as `value` under [`SETTINGS`] of the workspace's [`ENVIRONMENT_RECORD`],
/// made and sealed by this host, whose history names the device's person. A device whose
/// certificate does not name that record is not permitted, and the person's own settings are
/// never what it writes.
///
/// # Errors
/// The device does not reach the environment record, the arguments name no setting, or the
/// record could not be written.
pub async fn keep_setting(
    workspaces: &crate::workspaces::Workspaces,
    placed: &tilo_actions::remote::PlacedCaller,
    workspace: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Value, tilo_actions::remote::ActionCallError> {
    use collab_types::claim::CommitNamed as _;
    use tilo_actions::remote::ActionCallError;
    if !placed.reaches(&document_channel(VM_APP, ENVIRONMENT_RECORD)) {
        return Err(ActionCallError::NotPermitted);
    }
    let invalid = |reason: &str| ActionCallError::InvalidRequest {
        reason: reason.to_owned(),
    };
    let name = arguments
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid("a setting's name is needed"))?;
    let value = arguments
        .get("value")
        .ok_or_else(|| invalid("the setting's value is needed"))?;
    let person = *placed.person();
    crate::workspaces::person_workspace::edit_document_on(
        workspaces,
        workspace,
        (VM_APP, ENVIRONMENT_RECORD),
        |document| {
            let settings = tilo_automerge::json::map_at(document, &automerge::ROOT, SETTINGS)
                .map_err(|error| error.to_string())?;
            tilo_automerge::json::write(document, &settings, name, value)
                .map_err(|error| error.to_string())?;
            document.commit_edit(ENVIRONMENT_RECORD, SETTINGS, &person);
            Ok(())
        },
    )
    .await
    .map_err(|reason| ActionCallError::Unavailable { reason })?;
    Ok(serde_json::json!({ "name": name }))
}

/// The record's channel on the state protocol: [`neighbour_wire::app_document_group`]
/// for the record's app and id. The state protocol names a record by this alone;
/// the workspace a record belongs to is the scope's
/// [`workspace`](Self::workspace).
#[must_use]
pub fn document_channel(app: &str, record: &str) -> [u8; 32] {
    neighbour_wire::app_document_group(app, record)
}

/// What one environment's device reaches: its one workspace, and the channel
/// of its one record there. A scope that reaches nothing has neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnvironmentScope {
    /// The workspace it was placed in, by this server's workspace id.
    pub workspace: WorkspaceId,
    /// The channel of its Browser record ([`document_channel`]).
    pub channel: [u8; 32],
}

impl EnvironmentScope {
    /// The scope of an environment placed in `workspace` for `record` of `app`.
    #[must_use]
    pub fn for_resource(workspace: WorkspaceId, app: &str, record: &str) -> Self {
        Self {
            workspace,
            channel: document_channel(app, record),
        }
    }

    /// The scope of a workspace's VM device in `workspace`: the workspace's Browser record
    /// ([`VM_APP`], [`VM_RECORD`]), which its window opens on.
    #[must_use]
    pub fn of_workspace_device(workspace: WorkspaceId) -> Self {
        Self::for_resource(workspace, VM_APP, VM_RECORD)
    }

    /// Whether the device this scope confines is admitted to `workspace` for
    /// `op`: the protocols a window's session works on, in its one workspace, and
    /// no other protocol anywhere.
    ///
    /// Subscription, state, replay and publishing are what joining, syncing
    /// and writing its record need; the live session carries its window's page;
    /// and a remote action is how the window answers the page action the
    /// server handed it. Every other protocol — a channel's keys, compute, an
    /// environment, content discovery, membership — is not its to ask: a page
    /// that escapes Chromium holds the device's key, and no key of the
    /// workspace's goes to it. The host answers what it needs of keys itself.
    #[must_use]
    pub fn admits(self, workspace: WorkspaceId, op: WorkspaceOperation) -> bool {
        self.workspace == workspace && placed_protocol(op)
    }

    /// Whether `channel` — a record's channel, or `None` for a frame that
    /// belongs to no record — is inside this scope. A frame of no record is
    /// the workspace's own document, which a session needs to join at all.
    #[must_use]
    pub fn reaches(self, channel: Option<[u8; 32]>) -> bool {
        match channel {
            None => true,
            Some(channel) => self.channel == channel,
        }
    }
}

/// Whether `op` is a protocol a placed device works on: joining, syncing and
/// writing its records, the live session, and answering a remote action. Every
/// other protocol (a channel's keys, compute, an environment, content
/// discovery, membership) is not its to ask, whether the register or its
/// certificate placed it ([`EnvironmentScope::admits`]).
#[must_use]
pub fn placed_protocol(op: WorkspaceOperation) -> bool {
    matches!(
        op,
        WorkspaceOperation::Subscribe
            | WorkspaceOperation::State
            | WorkspaceOperation::Publish
            | WorkspaceOperation::Replay
            | WorkspaceOperation::Live
            | WorkspaceOperation::RemoteAction
    )
}

/// What the server knows of a device it certified for one of its environments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Environment {
    /// Placed, and its session may run: it reaches this one scope.
    Placed(EnvironmentScope),
    /// Its environment stopped: it reaches nothing, and no protocol admits it,
    /// until `until_ns` — the expiry of the certificate it was given, after
    /// which the certificate refuses it on its own. Kept at least that long,
    /// and written before the device is delisted, so a delisting that fails
    /// leaves it refused rather than open.
    Stopped {
        /// When the certificate it was given expires.
        until_ns: u64,
    },
}

impl Environment {
    /// Whether this device reaches `channel` — a record's channel, or `None`
    /// for a frame of no record.
    #[must_use]
    pub fn reaches(self, channel: Option<[u8; 32]>) -> bool {
        match self {
            Self::Placed(scope) => scope.reaches(channel),
            Self::Stopped { .. } => false,
        }
    }

    /// The scope a placed environment reaches, and nothing for a stopped one.
    #[must_use]
    pub const fn placed(self) -> Option<EnvironmentScope> {
        match self {
            Self::Placed(scope) => Some(scope),
            Self::Stopped { .. } => None,
        }
    }
}

/// What a device this server placed for a Browser environment is told when it
/// asks to enter a workspace or redeems an invitation: it is no member and
/// never becomes one, so the roster never lists it and no key of the
/// workspace's goes to it. Its one record is all it reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a Browser environment joins nothing; it serves its one record")]
pub struct JoinsNothing;

impl From<JoinsNothing> for tilo_peer_state::join_log::JoinError {
    fn from(refusal: JoinsNothing) -> Self {
        Self::new(refusal.to_string())
    }
}

/// The environment devices this server certified, by device key
/// ([`Environment`]), kept across restarts when it has a path.
#[derive(Clone)]
pub struct EnvironmentRegistry {
    table: Arc<Table>,
}

/// What the register keeps of one device: the environment it is, when the
/// certificate it was given expires — after which it says nothing — and the
/// certificate itself, so the device's link is admitted with it when the
/// workspace's roster does not hold one yet.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Kept {
    environment: Environment,
    until_ns: u64,
    cert: Option<DeviceCert>,
}

struct Table {
    devices: RwLock<HashMap<[u8; 32], Kept>>,
    /// How many live sessions each environment device has here.
    connected: RwLock<HashMap<[u8; 32], usize>>,
    /// Where the devices are kept across restarts; `None` keeps them in this
    /// process.
    path: Option<PathBuf>,
    /// Whether the register could not be read at start: no environment is
    /// placed until it can be.
    unreadable: bool,
    /// What the server does when the last session of an environment device
    /// ends ([`Self::when_ended`]).
    ended: RwLock<Option<Ended>>,
}

/// What the server is told when a device's last session here ends.
type Ended = Arc<dyn Fn(EnvironmentScope, [u8; 32]) + Send + Sync>;

impl Default for EnvironmentRegistry {
    fn default() -> Self {
        Self::new(None)
    }
}

impl std::fmt::Debug for EnvironmentRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvironmentRegistry")
            .field("devices", &self.table.devices.read_recover().len())
            .field("path", &self.table.path)
            .finish_non_exhaustive()
    }
}

impl EnvironmentRegistry {
    /// A register kept in `path` when one is named, and in memory otherwise:
    /// what it holds is read back here, so a server that restarts confines the
    /// environments it placed before.
    #[must_use]
    pub fn new(path: Option<PathBuf>) -> Self {
        let (devices, unreadable) = path.as_deref().map_or_else(
            || (HashMap::new(), false),
            |path| match load(path) {
                Ok(devices) => (devices, false),
                Err(Unreadable::Missing) => (HashMap::new(), false),
                Err(Unreadable::Broken(error)) => {
                    tracing::error!(
                        %error,
                        path = %path.display(),
                        "the environments this server placed cannot be read: it places no new \
                         one until its register reads again"
                    );
                    (HashMap::new(), true)
                }
            },
        );
        Self {
            table: Arc::new(Table {
                devices: RwLock::new(devices),
                connected: RwLock::new(HashMap::new()),
                path,
                unreadable,
                ended: RwLock::new(None),
            }),
        }
    }

    /// A register kept in `path` ([`Self::new`]).
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self::new(Some(path.into()))
    }

    /// Confine the device `cert` certifies, as the host that placed its VM has it: a device
    /// placed for records of one workspace
    /// ([`DeviceCapabilities::placed`](tilo_user_identity::DeviceCapabilities::placed)), or whose
    /// certificate reaches one workspace alone
    /// ([`DeviceCapabilities::workspace_device`](tilo_user_identity::DeviceCapabilities::workspace_device)),
    /// is a workspace's VM, which reaches that workspace and holds none of its keys. It is
    /// placed in [`EnvironmentScope::of_workspace_device`] until its certificate expires, and its
    /// link is admitted with that certificate, which the workspace's roster never holds. `None`
    /// for any other device, which this register does not confine, and for one it stopped.
    ///
    /// # Errors
    ///
    /// The register could not be read when the server started, or what it wrote cannot be kept:
    /// the device is then confined to nothing at all rather than placed.
    pub fn confine(&self, cert: &DeviceCert) -> Result<Option<EnvironmentScope>, String> {
        let workspace = if let Some(records) = &cert.capabilities.records {
            // A device placed for a workspace VM's records: its Browser record is what its
            // window opens on.
            if cert.capabilities != tilo_user_identity::DeviceCapabilities::placed(records.clone())
            {
                return Ok(None);
            }
            *records.workspace()
        } else {
            let Some(workspace) = cert.capabilities.workspace else {
                return Ok(None);
            };
            let shape = tilo_user_identity::DeviceCapabilities {
                foreign: cert.capabilities.foreign,
                ..tilo_user_identity::DeviceCapabilities::workspace_device(workspace)
            };
            if cert.capabilities != shape {
                return Ok(None);
            }
            workspace
        };
        let scope = EnvironmentScope::of_workspace_device(WorkspaceId::from_u128(
            u128::from_be_bytes(workspace),
        ));
        self.place_certified(cert, scope, cert.expires_at_ns.unwrap_or(u64::MAX))
            .map(|placed| placed.then_some(scope))
    }

    /// Place the device `cert` certifies in `scope` until `until_ns`, its link admitted with
    /// `cert`: `false` for a device this register stopped, which stays stopped.
    ///
    /// # Errors
    ///
    /// As [`Self::confine`].
    pub fn place_certified(
        &self,
        cert: &DeviceCert,
        scope: EnvironmentScope,
        until_ns: u64,
    ) -> Result<bool, String> {
        if self.table.unreadable {
            return Err(
                "the environments this server placed cannot be read, so it places no new one"
                    .to_owned(),
            );
        }
        // A device this register stopped stays stopped: its entry is what refuses it.
        if matches!(
            self.of(&cert.device_public_key, tilo_clock::unix_now_ns()),
            Some(Environment::Stopped { .. })
        ) {
            return Ok(false);
        }
        self.keep(
            cert.device_public_key,
            Some(Kept {
                environment: Environment::Placed(scope),
                until_ns,
                cert: Some(cert.clone()),
            }),
            "a workspace's VM device was not kept",
        )?;
        Ok(true)
    }

    /// Refuse `device`, at `now_ns`, entering a workspace, redeeming an
    /// invitation into one or doing any of the host's administration, when this
    /// register knows it: placed or stopped, an environment's device joins
    /// nothing. A register that cannot be read knows every device as stopped,
    /// so this refuses rather than lets in.
    ///
    /// # Errors
    ///
    /// The device is an environment's.
    pub fn refuse_joining(&self, device: &[u8; 32], now_ns: u64) -> Result<(), JoinsNothing> {
        match self.of(device, now_ns) {
            Some(_) => Err(JoinsNothing),
            None => Ok(()),
        }
    }

    /// The certificate `device` was placed with, when the register holds one:
    /// what its link is admitted as before the workspace's roster lists it.
    #[must_use]
    pub fn certificate(&self, device: &[u8; 32]) -> Option<DeviceCert> {
        self.table
            .devices
            .read_recover()
            .get(device)
            .and_then(|kept| kept.cert.clone())
    }

    /// Refuse `device` on every protocol from now on, until `until_ns` — when the
    /// certificate it was given expires. Written before the device is
    /// delisted; a write that fails keeps it refused here and in the process,
    /// never open.
    ///
    /// # Errors
    ///
    /// What it wrote cannot be kept: the device is refused in this process,
    /// and would be confined — never wider — by a restart.
    pub fn stop(&self, device: [u8; 32], until_ns: u64) -> Result<(), String> {
        self.keep(
            device,
            Some(Kept {
                environment: Environment::Stopped { until_ns },
                until_ns,
                cert: None,
            }),
            "a stopped environment's device was not written down",
        )
    }

    /// Forget `device` entirely: it never left the server, so nothing holds it.
    pub fn forget(&self, device: &[u8; 32]) {
        let mut devices = self.table.devices.write_recover();
        let forgotten = devices.remove(device).is_some();
        if forgotten {
            let _ = self.save(&devices);
        }
        drop(devices);
        self.table.connected.write_recover().remove(device);
    }

    /// What the server knows of `device`, at `now_ns`: an entry whose
    /// certificate would have expired says nothing, as the certificate itself
    /// no longer admits it.
    #[must_use]
    pub fn of(&self, device: &[u8; 32], now_ns: u64) -> Option<Environment> {
        match self.table.devices.read() {
            Ok(devices) => devices
                .get(device)
                .filter(|kept| kept.until_ns > now_ns)
                .map(|kept| kept.environment),
            // A register that cannot be read confines every device it might
            // hold to nothing: refusal, never membership.
            Err(_) => Some(Environment::Stopped { until_ns: u64::MAX }),
        }
    }

    /// The environments placed in `workspace` and still running: each
    /// device, what it reaches, and when its certificate expires.
    #[must_use]
    pub fn placed_in(&self, workspace: WorkspaceId) -> Vec<([u8; 32], EnvironmentScope, u64)> {
        let now = tilo_clock::unix_now_ns();
        self.table
            .devices
            .read_recover()
            .iter()
            .filter(|(_, kept)| kept.until_ns > now)
            .filter_map(|(device, kept)| match kept.environment {
                Environment::Placed(scope) if scope.workspace == workspace => {
                    Some((*device, scope, kept.until_ns))
                }
                Environment::Placed(_) | Environment::Stopped { .. } => None,
            })
            .collect()
    }

    /// A connection of `device` authenticated here: its environment is up, and
    /// an end that was being waited on is not one. Every protocol feeds this — a
    /// state-protocol session, and the live session a session in a workspace's VM
    /// publishes its window on — so "still here" is one count of the device's
    /// open connections, not one per protocol.
    pub fn arrived(&self, device: [u8; 32]) {
        if !matches!(
            self.of(&device, tilo_clock::unix_now_ns()),
            Some(Environment::Placed(_))
        ) {
            return;
        }
        let mut connected = self.table.connected.write_recover();
        let count = connected.entry(device).or_insert(0);
        *count = count.saturating_add(1);
        drop(connected);
    }

    /// A connection of `device` ended: when it was the last one, the server that
    /// placed its environment hears of it ([`Self::when_ended`]).
    pub fn departed(&self, device: [u8; 32]) {
        let last = {
            let mut connected = self.table.connected.write_recover();
            match connected.get_mut(&device) {
                Some(count) => {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        connected.remove(&device);
                        true
                    } else {
                        false
                    }
                }
                None => true,
            }
        };
        if !last {
            return;
        }
        let Some(Environment::Placed(scope)) = self.of(&device, tilo_clock::unix_now_ns()) else {
            return;
        };
        let ended = self.table.ended.read_recover().clone();
        if let Some(ended) = ended {
            ended(scope, device);
        }
    }

    /// Whether a connection of `device` is here now.
    #[must_use]
    pub fn connected(&self, device: &[u8; 32]) -> bool {
        self.table.connected.read_recover().contains_key(device)
    }

    /// Answer [`Self::departed`] with `ended`: what makes a session's end stop
    /// the environment it belonged to.
    pub fn when_ended(&self, ended: Ended) {
        *self.table.ended.write_recover() = Some(ended);
    }

    /// Write `device`'s entry and keep it: in memory first, so no protocol admits
    /// it whatever the file does, then the file.
    fn keep(&self, device: [u8; 32], environment: Option<Kept>, what: &str) -> Result<(), String> {
        let mut devices = self.table.devices.write_recover();
        match environment {
            Some(kept) => {
                devices.insert(device, kept);
            }
            None => {
                devices.remove(&device);
            }
        }
        let kept = self.save(&devices);
        drop(devices);
        kept.map_err(|error| {
            let error = format!("{what}: {error}");
            tracing::error!(%error, device = %hex::encode(device));
            error
        })
    }

    /// Write the register down where a restart reads it, without ever leaving
    /// half of it: a temporary file beside it, then a rename.
    fn save(&self, devices: &HashMap<[u8; 32], Kept>) -> Result<(), String> {
        let Some(path) = &self.table.path else {
            return Ok(());
        };
        let now = tilo_clock::unix_now_ns();
        let stored = Stored {
            devices: devices
                .iter()
                .filter(|(_, kept)| kept.until_ns > now)
                .map(|(device, kept)| (hex::encode(device), StoredDevice::of(kept)))
                .collect(),
        };
        let bytes = serde_json::to_vec(&stored).map_err(|error| error.to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600)
            .map_err(|error| error.to_string())
    }
}

/// A register that confines nobody: what a protocol no browser environment
/// reaches asks its [`crate::workspace_access::ServerTrust`] with, so that a
/// decision it takes about a device of another kind asks the same rule as
/// every other.
#[must_use]
pub fn none() -> &'static EnvironmentRegistry {
    static NONE: std::sync::OnceLock<EnvironmentRegistry> = std::sync::OnceLock::new();
    NONE.get_or_init(EnvironmentRegistry::default)
}

/// What the register knows of `device`: `None` for a device that is no
/// environment's.
#[must_use]
pub fn environment_of(
    environments: &EnvironmentRegistry,
    device: &[u8; 32],
) -> Option<Environment> {
    environments.of(device, tilo_clock::unix_now_ns())
}

/// What the register keeps across restarts: the devices by key, and what each
/// reaches (or that it stopped) until when.
#[derive(Serialize, Deserialize)]
struct Stored {
    devices: HashMap<String, StoredDevice>,
}

#[derive(Serialize, Deserialize)]
struct StoredDevice {
    /// The scope it was placed for, absent once its environment stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<StoredScope>,
    /// Its certificate's expiry: after this the entry says nothing.
    until_ns: u64,
    /// The certificate it was placed with, postcard-encoded and hex, when one
    /// was: what its link is admitted with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cert: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct StoredScope {
    workspace: WorkspaceId,
    /// The record's channel, hex.
    channel: String,
}

impl StoredDevice {
    fn of(kept: &Kept) -> Self {
        let scope = match kept.environment {
            Environment::Placed(scope) => Some(StoredScope {
                workspace: scope.workspace,
                channel: hex::encode(scope.channel),
            }),
            Environment::Stopped { .. } => None,
        };
        let cert = kept
            .cert
            .as_ref()
            .and_then(|cert| postcard::to_allocvec(cert).ok())
            .map(hex::encode);
        Self {
            scope,
            until_ns: kept.until_ns,
            cert,
        }
    }

    /// What this entry kept, and when its certificate expires.
    fn kept(&self) -> Option<Kept> {
        let environment = match &self.scope {
            Some(scope) => {
                let mut channel = [0_u8; 32];
                hex::decode_to_slice(&scope.channel, &mut channel).ok()?;
                Environment::Placed(EnvironmentScope {
                    workspace: scope.workspace,
                    channel,
                })
            }
            None => Environment::Stopped {
                until_ns: self.until_ns,
            },
        };
        let cert = self.cert.as_deref().and_then(|cert| {
            tilo_failure::ignored!(
                "an environment's device certificate is the server's own to sign",
                postcard::from_bytes::<DeviceCert>(&hex::decode(cert).ok()?).ok()
            )
        });
        Some(Kept {
            environment,
            until_ns: self.until_ns,
            cert,
        })
    }
}

/// Why a kept register was not read.
enum Unreadable {
    /// There was none: the server placed no environment before.
    Missing,
    /// There is one and it cannot be read.
    Broken(String),
}

/// The register `path` kept, without the entries whose certificate would have
/// expired.
fn load(path: &Path) -> Result<HashMap<[u8; 32], Kept>, Unreadable> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Unreadable::Missing);
        }
        Err(error) => return Err(Unreadable::Broken(error.to_string())),
    };
    let stored: Stored =
        serde_json::from_slice(&bytes).map_err(|error| Unreadable::Broken(error.to_string()))?;
    let now = tilo_clock::unix_now_ns();
    let mut devices = HashMap::new();
    for (device, entry) in stored.devices {
        let mut key = [0_u8; 32];
        if hex::decode_to_slice(&device, &mut key).is_err() {
            tracing::warn!(device, "an environment's entry names no device key");
            continue;
        }
        match entry.kept() {
            Some(kept) if kept.until_ns > now => {
                devices.insert(key, kept);
            }
            Some(kept) => {
                tracing::info!(
                    device = %device,
                    until_ns = kept.until_ns,
                    "an environment's certificate has expired"
                );
            }
            None => tracing::warn!(device, "an environment's entry names no channel"),
        }
    }
    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tilo_server_protocol::WorkspaceId {
        tilo_server_protocol::WorkspaceId::from_u128(7)
    }

    fn scope() -> EnvironmentScope {
        EnvironmentScope::of_workspace_device(workspace())
    }

    fn soon() -> u64 {
        tilo_clock::unix_now_ns().saturating_add(60 * 1_000_000_000)
    }

    /// The certificate of `device`, with `capabilities` until `until`.
    fn certified(
        device: [u8; 32],
        capabilities: tilo_user_identity::DeviceCapabilities,
        until: u64,
    ) -> DeviceCert {
        DeviceCert {
            user_public_key: [9; 32],
            device_public_key: device,
            device_label: "Workspace computer".into(),
            capabilities,
            issued_at_ns: 0,
            expires_at_ns: Some(until),
            signature: [0; 64],
            issuer: None,
        }
    }

    /// A workspace's VM device: reaching [`workspace`] alone until `until`.
    fn vm(device: [u8; 32], until: u64) -> DeviceCert {
        certified(
            device,
            tilo_user_identity::DeviceCapabilities::workspace_device(7_u128.to_be_bytes()),
            until,
        )
    }

    #[test]
    fn a_record_has_one_channel_on_the_state_protocol() {
        assert_eq!(
            scope().channel,
            neighbour_wire::app_document_group(VM_APP, VM_RECORD)
        );
        assert_ne!(
            scope().channel,
            EnvironmentScope::for_resource(workspace(), VM_APP, "record-2").channel,
            "another record is another channel"
        );
    }

    /// A VM device is confined as its certificate says, which reaches one workspace alone, and a
    /// device certified for every workspace is no environment's.
    #[test]
    fn a_vm_device_is_confined_as_its_certificate_says() {
        let environments = EnvironmentRegistry::default();
        assert_eq!(
            environments.confine(&vm([1; 32], soon())).unwrap(),
            Some(scope())
        );
        assert_eq!(
            environment_of(&environments, &[1; 32]),
            Some(Environment::Placed(scope()))
        );
        assert_eq!(
            environments
                .certificate(&[1; 32])
                .map(|cert| cert.device_public_key),
            Some([1; 32]),
            "its link is admitted with the certificate it was placed with"
        );
        let member = certified(
            [2; 32],
            tilo_user_identity::DeviceCapabilities::member(),
            soon(),
        );
        assert_eq!(environments.confine(&member).unwrap(), None);
        assert_eq!(
            environment_of(&environments, &[2; 32]),
            None,
            "a member device is no environment"
        );
    }

    #[test]
    fn an_environment_reaches_its_workspace_and_no_other() {
        assert!(scope().admits(workspace(), WorkspaceOperation::State));
        assert!(!scope().admits(
            tilo_server_protocol::WorkspaceId::from_u128(8),
            WorkspaceOperation::State
        ));
    }

    #[test]
    fn an_environment_works_on_a_windows_protocols_and_no_other() {
        for op in [
            WorkspaceOperation::Subscribe,
            WorkspaceOperation::State,
            WorkspaceOperation::Publish,
            WorkspaceOperation::Replay,
            WorkspaceOperation::Live,
            WorkspaceOperation::RemoteAction,
        ] {
            assert!(scope().admits(workspace(), op), "{op:?}");
        }
        for op in [
            WorkspaceOperation::KeyAccess,
            WorkspaceOperation::Compute,
            WorkspaceOperation::Environment,
            WorkspaceOperation::ContentDiscovery,
        ] {
            assert!(!scope().admits(workspace(), op), "{op:?}");
        }
    }

    #[test]
    fn an_environment_reaches_its_record_and_no_other() {
        let scope = scope();
        assert!(scope.reaches(Some(scope.channel)), "its own record");
        assert!(scope.reaches(None), "the workspace's own document");
        assert!(
            !scope.reaches(Some(document_channel("so.tilo.notes", "notes-1"))),
            "a customer's document"
        );
        assert!(
            !scope.reaches(Some(document_channel("so.tilo.chat", "chat-1"))),
            "a customer's chat"
        );
    }

    #[test]
    fn a_register_that_cannot_be_read_confines_a_device_to_nothing() {
        let environments = EnvironmentRegistry::default();
        environments.confine(&vm([1; 32], soon())).unwrap();
        assert_eq!(
            environment_of(&environments, &[1; 32]),
            Some(Environment::Placed(scope()))
        );
        assert_eq!(
            environment_of(&environments, &[2; 32]),
            None,
            "no environment"
        );
        let _ = std::thread::scope(|thread| {
            thread
                .spawn(|| {
                    let _guard = environments.table.devices.write().unwrap();
                    panic!("a writer panicked");
                })
                .join()
        });
        let found =
            environment_of(&environments, &[1; 32]).expect("a poisoned register is no member");
        assert!(!found.reaches(None));
        assert_eq!(found.placed(), None);
    }

    #[test]
    fn forgetting_an_environment_forgets_its_device() {
        let environments = EnvironmentRegistry::default();
        environments.confine(&vm([1; 32], soon())).unwrap();
        environments.forget(&[1; 32]);
        assert_eq!(environment_of(&environments, &[1; 32]), None);
    }

    #[test]
    fn a_stopped_environment_reaches_nothing_until_its_certificate_expires() {
        let environments = EnvironmentRegistry::default();
        environments.confine(&vm([1; 32], soon())).unwrap();
        let until = soon();
        environments.stop([1; 32], until).unwrap();
        let stopped = environment_of(&environments, &[1; 32]).expect("a stopped environment");
        assert_eq!(stopped.placed(), None);
        assert!(!stopped.reaches(None), "not the workspace's own document");
        assert!(!stopped.reaches(Some(scope().channel)));
        assert_eq!(
            environments.of(&[1; 32], until),
            None,
            "past its certificate's expiry the certificate refuses it on its own"
        );
    }

    #[test]
    fn a_placed_environment_is_read_back_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("environments.json");
        let until = soon();
        {
            let server = EnvironmentRegistry::at(&path);
            server.confine(&vm([1; 32], until)).unwrap();
            server.stop([2; 32], until).unwrap();
            assert_eq!(
                server.placed_in(workspace()),
                vec![([1; 32], scope(), until)]
            );
        }
        let restarted = EnvironmentRegistry::at(&path);
        assert_eq!(
            environment_of(&restarted, &[1; 32]),
            Some(Environment::Placed(scope())),
            "a restart confines the environment it placed"
        );
        assert_eq!(
            environment_of(&restarted, &[2; 32]),
            Some(Environment::Stopped { until_ns: until }),
            "and a restart keeps a stopped one refused"
        );
        assert_eq!(
            restarted.placed_in(workspace()),
            vec![([1; 32], scope(), until)]
        );
    }

    #[test]
    fn an_environment_placed_with_its_certificate_is_read_back_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("environments.json");
        let until = soon();
        let cert = DeviceCert {
            user_public_key: [5; 32],
            device_public_key: [1; 32],
            device_label: "Browser".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: Some(until),
            signature: [6; 64],
            issuer: None,
        };
        EnvironmentRegistry::at(&path)
            .place_certified(&cert, scope(), until)
            .unwrap();
        let restarted = EnvironmentRegistry::at(&path);
        assert_eq!(
            environment_of(&restarted, &[1; 32]),
            Some(Environment::Placed(scope())),
            "a restart confines the environment it placed with a certificate"
        );
        assert_eq!(restarted.certificate(&[1; 32]), Some(cert));
    }

    #[test]
    fn a_register_that_cannot_be_read_places_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("environments.json");
        std::fs::write(&path, b"{ not the server's").unwrap();
        let server = EnvironmentRegistry::at(&path);
        let refused = server.confine(&vm([1; 32], soon())).unwrap_err();
        assert!(refused.contains("places no new one"), "{refused}");
        assert_eq!(environment_of(&server, &[1; 32]), None);
    }

    #[test]
    fn the_last_session_of_an_environment_is_what_ends_it() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let environments = EnvironmentRegistry::default();
                environments.confine(&vm([1; 32], soon())).unwrap();
                let ended = Arc::new(std::sync::Mutex::new(Vec::new()));
                let heard = Arc::clone(&ended);
                environments.when_ended(Arc::new(move |scope, device| {
                    heard.lock().unwrap().push((scope, device));
                }));
                environments.arrived([1; 32]);
                environments.arrived([1; 32]);
                assert!(environments.connected(&[1; 32]));
                environments.departed([1; 32]);
                assert!(ended.lock().unwrap().is_empty(), "one session still here");
                environments.departed([1; 32]);
                assert_eq!(
                    *ended.lock().unwrap(),
                    vec![(scope(), [1; 32])],
                    "the environment ends with its last session"
                );
                assert!(!environments.connected(&[1; 32]));
            });
    }
}
