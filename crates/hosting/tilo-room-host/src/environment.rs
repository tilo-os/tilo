//! A hosted browser environment's device: one workspace, one record.
//!
//! A workspace hosted on a relay that sells machines opens Browser in a
//! browser environment: a short-lived VM whose headless session joins the
//! workspace, as a device the relay certified for it
//! ([`docs/hosted-browser.md`](../../../../docs/hosted-browser.md)). Web pages
//! are strangers' code and Chromium runs a JIT on it, so a page that escapes
//! the engine holds that device's key: everything the device may reach is
//! everything such a page may read. The relay therefore admits it as a device
//! of one workspace with one record — its Browser record — whose channel is
//! the only record channel it reaches, and whose window's page is the only
//! track it publishes.
//!
//! This register is the environment's half of the rule that confines an agent
//! to its delegation ([`crate::peer::AgentRegistry`]): the devices this relay
//! certified for its environments, by device key, with the one record each
//! reaches. It is the relay's, and nothing here is asked of the session in the
//! VM: [`crate::workspace_access::decide`] answers every plane with it, the
//! state plane's replay, its live fan-out, its record opening and its changes
//! ask it what a record is, and the live plane offers such a device no one
//! else's tracks. A device that asked to be one is confined whether or not its
//! session cooperates.
//!
//! What it is given besides its record is the workspace's own document — the
//! layout every device needs to join at all — and its own device's presence on
//! the live plane: never one record of the workspace's, its channels' keys, a
//! call, or another window's page.
//!
//! # Durable, and never wider than its scope
//!
//! A device certified for an environment is a member device of the
//! workspace's person by certificate, and the workspace lists it as one while
//! its session runs: losing the register would leave it an ordinary member.
//! So the register is the relay's own state ([`EnvironmentRegistry::at`]),
//! written before the device is handed to its session and listed, and read
//! back when the relay starts: a relay that restarts with an environment still
//! placed confines it again, whatever the workspace's persisted membership
//! says.
//!
//! An environment that stops is a [`Environment::Stopped`] entry in the same
//! register, written before the device is delisted and kept until the
//! certificate it was given would have expired: a device whose environment
//! stopped is refused on every plane — the certificate check every plane and
//! every audit already makes asks this register — and a delisting that fails
//! leaves it refused rather than open. The register also counts the sessions
//! each environment device has here ([`EnvironmentRegistry::arrived`],
//! [`EnvironmentRegistry::departed`]); when the last one ends, the relay that
//! placed it is told ([`EnvironmentRegistry::when_ended`]) so the environment
//! stops too, and the certificate's expiry bounds what a lost signal could.
//!
//! The one hole left is the register's own file: a relay whose state root
//! loses it cannot tell a previous run's environment device from a member.
//! [`EnvironmentRegistry::place`] therefore refuses to certify a new one until
//! the file reads again, so the hole never opens wider than what an operator
//! deleted.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use tilo_lock::RwLockRecover as _;

use crate::workspace_access::WorkspaceOperation;
use tilo_relay_protocol::WorkspaceId;

/// The record's channel on the state plane: [`neighbour_wire::app_document_channel`]
/// for the record's app and id. The state plane names a record by this alone;
/// the workspace a record belongs to is the scope's
/// [`workspace`](Self::workspace).
#[must_use]
pub fn record_channel(app: &str, record: &str) -> [u8; 32] {
    neighbour_wire::app_document_channel(app, record)
}

/// What one environment's device reaches: its one workspace, and the channel
/// of its one record there. A scope that reaches nothing has neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnvironmentScope {
    /// The workspace it was placed in, by this relay's workspace id.
    pub workspace: WorkspaceId,
    /// The channel of its Browser record ([`record_channel`]).
    pub channel: [u8; 32],
}

impl EnvironmentScope {
    /// The scope of an environment placed in `workspace` for `record` of `app`.
    #[must_use]
    pub fn for_record(workspace: WorkspaceId, app: &str, record: &str) -> Self {
        Self {
            workspace,
            channel: record_channel(app, record),
        }
    }

    /// Whether the device this scope confines is admitted to `workspace` for
    /// `op`: the planes a window's session works on, in its one workspace, and
    /// no other plane anywhere.
    ///
    /// Subscription, state, replay and publishing are what joining, syncing
    /// and writing its record need; the live plane carries its window's page;
    /// a remote action is how the window answers the page action the relay
    /// handed it; and a channel is how it asks its record's keys for access.
    /// Every other plane — compute, an environment, content discovery,
    /// membership — is not its to ask.
    #[must_use]
    pub fn admits(self, workspace: WorkspaceId, op: WorkspaceOperation) -> bool {
        self.workspace == workspace
            && matches!(
                op,
                WorkspaceOperation::Subscribe
                    | WorkspaceOperation::State
                    | WorkspaceOperation::Publish
                    | WorkspaceOperation::Replay
                    | WorkspaceOperation::Live
                    | WorkspaceOperation::RemoteAction
                    | WorkspaceOperation::ChannelAccess
            )
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

/// What the relay knows of a device it certified for one of its environments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Environment {
    /// Placed, and its session may run: it reaches this one scope.
    Placed(EnvironmentScope),
    /// Its environment stopped: it reaches nothing, and no plane admits it,
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

    /// Whether this device may write `channel` — a record's channel, or `None`
    /// for the workspace's own document. It writes its own record and nothing
    /// else: the workspace's document, its layout and its governance, is read
    /// to join and written for it by this host, which writes only what concerns
    /// the device's own windows (its shares, asked of the host).
    #[must_use]
    pub fn writes(self, channel: Option<[u8; 32]>) -> bool {
        match self {
            Self::Placed(scope) => channel == Some(scope.channel),
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

/// The environment devices this relay certified, by device key
/// ([`Environment`]), kept across restarts when it has a path.
#[derive(Clone)]
pub struct EnvironmentRegistry {
    table: Arc<Table>,
}

/// What the register keeps of one device: the environment it is, and when the
/// certificate it was given expires — after which it says nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Kept {
    environment: Environment,
    until_ns: u64,
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
    /// What the relay does when the last session of an environment device
    /// ends ([`Self::when_ended`]).
    ended: RwLock<Option<Ended>>,
}

/// What the relay is told when a device's last session here ends.
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
    /// what it holds is read back here, so a relay that restarts confines the
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
                        "the environments this relay placed cannot be read: it places no new \
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

    /// Place `device` as the environment's device, reaching `scope` until
    /// `until_ns` (its certificate's expiry): written down before the device
    /// is certified and listed, so it is never an ordinary member, whatever
    /// happens to the workspace's membership.
    ///
    /// # Errors
    ///
    /// The register could not be read when the relay started, or what it wrote
    /// cannot be kept: the device is then confined to nothing at all rather
    /// than placed.
    pub fn place(
        &self,
        device: [u8; 32],
        scope: EnvironmentScope,
        until_ns: u64,
    ) -> Result<(), String> {
        if self.table.unreadable {
            return Err(
                "the environments this relay placed cannot be read, so it places no new one"
                    .to_owned(),
            );
        }
        self.keep(
            device,
            Some(Kept {
                environment: Environment::Placed(scope),
                until_ns,
            }),
            "an environment's device was not kept",
        )
    }

    /// Refuse `device` on every plane from now on, until `until_ns` — when the
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
            }),
            "a stopped environment's device was not written down",
        )
    }

    /// Forget `device` entirely: it never left the relay, so nothing holds it.
    pub fn forget(&self, device: &[u8; 32]) {
        let mut devices = self.table.devices.write_recover();
        let forgotten = devices.remove(device).is_some();
        if forgotten {
            let _ = self.save(&devices);
        }
        drop(devices);
        self.table.connected.write_recover().remove(device);
    }

    /// What the relay knows of `device`, at `now_ns`: an entry whose
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

    /// A session of `device` authenticated here: its environment is up, and an
    /// end that was being waited on is not one.
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

    /// A session of `device` ended: when it was the last one, the relay that
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

    /// Whether a session of `device` is here now.
    #[must_use]
    pub fn connected(&self, device: &[u8; 32]) -> bool {
        self.table.connected.read_recover().contains_key(device)
    }

    /// Answer [`Self::departed`] with `ended`: what makes a session's end stop
    /// the environment it belonged to.
    pub fn when_ended(&self, ended: Ended) {
        *self.table.ended.write_recover() = Some(ended);
    }

    /// Write `device`'s entry and keep it: in memory first, so no plane admits
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
                .map(|(device, kept)| (hex::encode(device), StoredDevice::of(*kept)))
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

/// A register that confines nobody: what a plane no browser environment
/// reaches asks its [`crate::workspace_access::RelayTrust`] with, so that a
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
}

#[derive(Serialize, Deserialize)]
struct StoredScope {
    workspace: WorkspaceId,
    /// The record's channel, hex.
    channel: String,
}

impl StoredDevice {
    fn of(kept: Kept) -> Self {
        let scope = match kept.environment {
            Environment::Placed(scope) => Some(StoredScope {
                workspace: scope.workspace,
                channel: hex::encode(scope.channel),
            }),
            Environment::Stopped { .. } => None,
        };
        Self {
            scope,
            until_ns: kept.until_ns,
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
        Some(Kept {
            environment,
            until_ns: self.until_ns,
        })
    }
}

/// Why a kept register was not read.
enum Unreadable {
    /// There was none: the relay placed no environment before.
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

    fn workspace() -> tilo_relay_protocol::WorkspaceId {
        tilo_relay_protocol::WorkspaceId::from_u128(7)
    }

    fn scope() -> EnvironmentScope {
        EnvironmentScope::for_record(workspace(), "so.tilo.browser", "record-1")
    }

    fn soon() -> u64 {
        tilo_clock::unix_now_ns().saturating_add(60 * 1_000_000_000)
    }

    #[test]
    fn a_record_has_one_channel_on_the_state_plane() {
        assert_eq!(
            scope().channel,
            neighbour_wire::app_document_channel("so.tilo.browser", "record-1")
        );
        assert_ne!(
            scope().channel,
            EnvironmentScope::for_record(workspace(), "so.tilo.browser", "record-2").channel,
            "another record is another channel"
        );
    }

    #[test]
    fn an_environment_reaches_its_workspace_and_no_other() {
        assert!(scope().admits(workspace(), WorkspaceOperation::State));
        assert!(!scope().admits(
            tilo_relay_protocol::WorkspaceId::from_u128(8),
            WorkspaceOperation::State
        ));
    }

    #[test]
    fn an_environment_works_on_a_windows_planes_and_no_other() {
        for op in [
            WorkspaceOperation::Subscribe,
            WorkspaceOperation::State,
            WorkspaceOperation::Publish,
            WorkspaceOperation::Replay,
            WorkspaceOperation::Live,
            WorkspaceOperation::RemoteAction,
            WorkspaceOperation::ChannelAccess,
        ] {
            assert!(scope().admits(workspace(), op), "{op:?}");
        }
        for op in [
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
            !scope.reaches(Some(record_channel("so.tilo.notes", "notes-1"))),
            "a customer's document"
        );
        assert!(
            !scope.reaches(Some(record_channel("so.tilo.chat", "chat-1"))),
            "a customer's chat"
        );
    }

    #[test]
    fn a_register_that_cannot_be_read_confines_a_device_to_nothing() {
        let environments = EnvironmentRegistry::default();
        environments.place([1; 32], scope(), soon()).unwrap();
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
        environments.place([1; 32], scope(), soon()).unwrap();
        environments.forget(&[1; 32]);
        assert_eq!(environment_of(&environments, &[1; 32]), None);
    }

    #[test]
    fn a_stopped_environment_reaches_nothing_until_its_certificate_expires() {
        let environments = EnvironmentRegistry::default();
        environments.place([1; 32], scope(), soon()).unwrap();
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
            let relay = EnvironmentRegistry::at(&path);
            relay.place([1; 32], scope(), until).unwrap();
            relay.stop([2; 32], until).unwrap();
            assert_eq!(
                relay.placed_in(workspace()),
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
    fn a_register_that_cannot_be_read_places_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("environments.json");
        std::fs::write(&path, b"{ not the relay's").unwrap();
        let relay = EnvironmentRegistry::at(&path);
        let refused = relay.place([1; 32], scope(), soon()).unwrap_err();
        assert!(refused.contains("places no new one"), "{refused}");
        assert_eq!(environment_of(&relay, &[1; 32]), None);
    }

    #[test]
    fn the_last_session_of_an_environment_is_what_ends_it() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let environments = EnvironmentRegistry::default();
                environments.place([1; 32], scope(), soon()).unwrap();
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
