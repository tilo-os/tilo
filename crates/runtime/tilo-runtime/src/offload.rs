//! Where a session's offloadable actions run: the action door's route
//! ([`tilo_actions::offload::ActionOffload`]) a host hands its broker as
//! `HostServices::action_offload`.
//!
//! The route is configured by the device's home-peer registry, the
//! person's always-on relays this device enrolled: the one that retains the
//! session's rooms places their compute jobs ([`tilo_relay::offload::home_relay`]).
//! A device with no such relay, or with [`OFFLOAD_ENV`] set to `off`, has no
//! route, and the door runs every offloadable action through the app's own
//! handler as it always has. With one, the route starts in the background
//! on the process's app runtime ([`tilo_relay::offload::DeviceOffload`]) and
//! answers "run here", with why, until it is up and whenever it cannot place
//! a run elsewhere.
//!
//! The route trusts the relay and the workers it places on by the person's
//! device fleet (`FleetRevocations`), the revocations the rest of this
//! device's trust reads: a revoked relay gets no route, and one revoked
//! while the route is up tears it down.
//!
//! The route reads and keeps only the rooms' own document media, in the
//! same content store the apps' blob socket serves ([`crate::profile::MediaHost::store`]),
//! so the inputs it may send are exactly what the invoking room's apps
//! could read, and the outputs are read back by the same `tilo.content`
//! addresses a local run returns.

use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use tilo_actions::offload::ActionOffload;
use tilo_identity_host::device_fleet::{DeviceFleet, DeviceFleetStore};
use tilo_lock::MutexRecover as _;
use tilo_relay::HomePeerRegistration;
use tilo_relay::offload::{
    DeviceOffload, DeviceOffloadConfig, DeviceRevocations, OffloadIdentity, home_relay,
};
use tilo_user_identity::RevocationList;

/// Set to `off` (or `0`, `false`, `no`) to keep every offloadable action on
/// this device even when a home relay could place it elsewhere.
pub const OFFLOAD_ENV: &str = "TILO_ACTION_OFFLOAD";
/// Where the home-peer registry is read from instead of beside the
/// collaboration identity; the desktop session honours the same variable.
pub const HOME_PEERS_ENV: &str = "TILO_HOME_PEERS_PATH";
/// The largest registry read.
const MAX_REGISTRY_BYTES: u64 = 1024 * 1024;

/// The route for offloadable invocations of `rooms` (the broker's cluster
/// ids) on this device, over `store`, the content store the rooms' apps
/// keep their document media in. `None` — every offloadable action runs
/// here, exactly as without offload — when offload is turned off, no home
/// relay of this device retains any of the rooms, or the device has no
/// trusted identity in `rooms`' first room. Never blocks on the network.
#[must_use]
pub fn action_offload(
    rooms: &[&str],
    store: Arc<tilo_fs::ContentStore>,
) -> Option<Arc<dyn ActionOffload>> {
    if disabled(std::env::var(OFFLOAD_ENV).ok().as_deref()) {
        tracing::info!("offloadable actions run on this device: offload is turned off");
        return None;
    }
    let config = match configured(rooms) {
        Ok(config) => config?,
        Err(error) => {
            tracing::warn!(%error, "offloadable actions run on this device");
            return None;
        }
    };
    let runtime = match tilo_rt::try_app() {
        Ok(runtime) => runtime.handle().clone(),
        Err(error) => {
            tracing::warn!(%error, "offloadable actions run on this device: no runtime");
            return None;
        }
    };
    Some(DeviceOffload::start(config, store, &runtime))
}

/// Whether `value` of [`OFFLOAD_ENV`] turns offload off.
fn disabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        )
    })
}

/// The route's configuration for `rooms`, from the registry and this
/// device's identity; `Ok(None)` when no home relay retains any of them.
fn configured(rooms: &[&str]) -> io::Result<Option<DeviceOffloadConfig>> {
    let Some(&first) = rooms.first() else {
        return Ok(None);
    };
    let registrations = read_registry(&registry_path())?;
    if registrations.is_empty() {
        return Ok(None);
    }
    let (device_signing_key, device_cert) =
        tilo_identity_host::broker::trusted_current_device(first)?;
    let user_public_key = device_cert.user_public_key;
    let profile = tilo_identity_host::registry::selected_identity_profile(first)?
        .ok_or_else(|| io::Error::other("no identity is selected for this workspace"))?;
    let revocations: Arc<dyn DeviceRevocations> = Arc::new(FleetRevocations::new(
        DeviceFleetStore::from_env()?,
        profile.id,
        user_public_key,
    ));
    Ok(
        home_relay(&registrations, user_public_key, rooms.iter().copied()).map(|(relay, rooms)| {
            DeviceOffloadConfig {
                relay,
                identity: OffloadIdentity {
                    user_public_key,
                    device_cert,
                    device_signing_key,
                    revocations,
                },
                rooms,
            }
        }),
    )
}

/// The person's device revocations as this device's trust reads them: the
/// selected identity's device fleet, the set [`trusted_current_device`]
/// refuses this device by and the desktop session checks its hosts
/// against. Served from memory while the fleet's files are unchanged, so
/// reading it on every check costs a `stat` each, and a revocation is seen
/// by the very next read.
///
/// [`trusted_current_device`]: tilo_identity_host::broker::trusted_current_device
struct FleetRevocations {
    store: DeviceFleetStore,
    identity_id: String,
    user_public_key: [u8; 32],
    /// The fleet last read and its revocations, verified once.
    read: Mutex<Option<(Arc<DeviceFleet>, RevocationList)>>,
}

impl FleetRevocations {
    fn new(store: DeviceFleetStore, identity_id: String, user_public_key: [u8; 32]) -> Self {
        Self {
            store,
            identity_id,
            user_public_key,
            read: Mutex::new(None),
        }
    }

    /// The revocations already verified for `fleet`, when it is the fleet
    /// last read.
    fn known(&self, fleet: &Arc<DeviceFleet>) -> Option<RevocationList> {
        self.read
            .lock_recover()
            .as_ref()
            .filter(|(known, _)| Arc::ptr_eq(known, fleet))
            .map(|(_, revocations)| revocations.clone())
    }
}

impl DeviceRevocations for FleetRevocations {
    fn current(&self) -> Result<RevocationList, String> {
        let fleet = self
            .store
            .load_cached(&self.identity_id)
            .map_err(|error| error.to_string())?;
        if let Some(revocations) = self.known(&fleet) {
            return Ok(revocations);
        }
        let revocations = fleet_revocations(&fleet, self.user_public_key)?;
        *self.read.lock_recover() = Some((fleet, revocations.clone()));
        Ok(revocations)
    }
}

/// `fleet`'s revocations, each signed by `user_public_key`, the person's
/// root: one issued by anyone else means the fleet cannot be trusted.
fn fleet_revocations(
    fleet: &DeviceFleet,
    user_public_key: [u8; 32],
) -> Result<RevocationList, String> {
    let mut revocations = RevocationList::new();
    for entry in &fleet.revocations {
        if entry.user_public_key != user_public_key {
            return Err("the device fleet holds a revocation by another identity".into());
        }
        revocations
            .insert(entry.clone())
            .map_err(|error| format!("the device fleet holds an invalid revocation: {error}"))?;
    }
    Ok(revocations)
}

/// The home-peer registry beside this device's collaboration identity, or
/// where [`HOME_PEERS_ENV`] names.
fn registry_path() -> PathBuf {
    if let Some(path) = std::env::var_os(HOME_PEERS_ENV).filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    let root = paths
        .user_pubkey
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("/var/lib/tilo"));
    tilo_relay::home_peer_registry_path(root)
}

/// The registrations at `path`; none when there is no registry.
fn read_registry(path: &Path) -> io::Result<Vec<HomePeerRegistration>> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    if metadata.len() > MAX_REGISTRY_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the home-peer registry exceeds its bound",
        ));
    }
    serde_json::from_slice(&std::fs::read(path)?).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the home-peer registry is invalid: {error}"),
        )
    })
}

#[cfg(test)]
#[path = "offload_tests.rs"]
mod tests;
