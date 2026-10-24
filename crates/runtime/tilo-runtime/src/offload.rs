//! Where a session's offloadable actions run: the action door's route
//! ([`tilo_actions::offload::ActionOffload`]) a host hands its broker as
//! `HostServices::action_offload`.
//!
//! The route is configured by the device's home-peer registry, the
//! person's always-on relays this device enrolled: the one that retains the
//! invoking room places its compute jobs. The registry is read again for
//! every offloadable invocation (from memory while the file is unchanged),
//! so a room the session creates or joins after it started, or a relay the
//! person registers meanwhile, is routed from the next invocation on
//! ([`tilo_relay::offload::RoomOffload`]); the session's rooms at start get
//! their route at once ([`tilo_relay::offload::home_relay`]). A room no
//! registered relay retains runs its offloadable actions through the app's
//! own handler, and the door is told why; [`OFFLOAD_ENV`] set to `off`
//! means no route at all. Each route starts in the background on the
//! process's app runtime ([`tilo_relay::offload::DeviceOffload`]) and
//! answers "run here", with why, until it is up and whenever it cannot place
//! a run elsewhere. The registry and the device's identity are read on the
//! runtime's blocking pool, never on the thread that invokes.
//!
//! The route trusts the relay and the workers it places on by the person's
//! device fleet (`FleetRevocations`), the revocations the rest of this
//! device's trust reads: a revoked relay gets no route, and one revoked
//! while the route is up tears it down.
//!
//! The route reads and keeps only the rooms' own document media, in the
//! same content store the apps' blob socket serves, through the invoking
//! app's scope in its room, so the inputs it may send are exactly what the
//! invoking room's apps could read, and the outputs are read back by the
//! same `tilo.content` addresses a local run returns. A session that owns
//! that store holds it in process ([`crate::profile::MediaHost::store`],
//! [`action_offload`]); the desktop session's apps keep theirs in the
//! filesystem daemon, which the route reaches over the daemon's
//! document-media socket as the session ([`session_action_offload`]).

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use tilo_actions::offload::ActionOffload;
use tilo_identity_host::device_fleet::{DeviceFleet, DeviceFleetStore};
use tilo_lock::MutexRecover as _;
use tilo_relay::HomePeerRegistration;
use tilo_relay::offload::{
    DeviceOffload, DeviceOffloadConfig, DeviceRevocations, OffloadIdentity, RoomMedia, RoomOffload,
    RoomPlacements, RoomRelays, SessionMedia, home_relay, room_relays,
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

/// The route for offloadable invocations on this device, over `store`, the
/// content store the rooms' apps keep their document media in: for `rooms`
/// (the broker's cluster ids at start), whose route starts at once, and for
/// every room the session has later, resolved when its actions are invoked.
/// `None` — every offloadable action runs here, exactly as without offload —
/// only when offload is turned off or there is no app runtime. Never blocks
/// on the network.
#[must_use]
pub fn action_offload(
    rooms: &[&str],
    store: Arc<tilo_fs::ContentStore>,
) -> Option<Arc<dyn ActionOffload>> {
    action_offload_over(rooms, tilo_fs::document_media::DocumentMedia::new(store))
}

/// [`action_offload`] for the desktop session, whose apps keep their
/// document media in the filesystem daemon at `blob_socket`: the route
/// reads its inputs and keeps its outputs there, connected as the desktop
/// session, through the invoking app's scope in its room — the same door
/// and the same authority the session opens app content with. Never
/// connects before an offloadable action is invoked.
#[must_use]
pub fn session_action_offload(
    rooms: &[&str],
    blob_socket: &Path,
) -> Option<Arc<dyn ActionOffload>> {
    action_offload_over(rooms, Arc::new(SessionMedia::new(blob_socket)))
}

/// The route for `rooms` over `media`, as [`action_offload`] describes.
fn action_offload_over(
    rooms: &[&str],
    media: Arc<dyn RoomMedia>,
) -> Option<Arc<dyn ActionOffload>> {
    if disabled(std::env::var(OFFLOAD_ENV).ok().as_deref()) {
        tracing::info!("offloadable actions run on this device: offload is turned off");
        return None;
    }
    let runtime = match tilo_rt::try_app() {
        Ok(runtime) => runtime.handle().clone(),
        Err(error) => {
            tracing::warn!(%error, "offloadable actions run on this device: no runtime");
            return None;
        }
    };
    let placements = Arc::new(RegistryPlacements::new(
        registry_path(),
        Box::new(session_identity),
    ));
    let starting = runtime.clone();
    let offload = RoomOffload::new(
        Arc::clone(&placements) as Arc<dyn RoomPlacements>,
        Box::new(move |config| DeviceOffload::start(config, Arc::clone(&media), &starting)),
        runtime,
    );
    match configured(rooms, &placements) {
        Ok(Some(config)) => offload.hold(config),
        Ok(None) => tracing::info!(
            "no home relay retains the session's rooms: their offloadable actions run on this \
             device until one is registered"
        ),
        Err(error) => tracing::warn!(
            %error,
            "the session's rooms get no offload route at start; each invocation tries again"
        ),
    }
    Some(Arc::new(offload))
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

/// The start route's configuration for `rooms`, from the registry and this
/// device's identity in the first room; `Ok(None)` when no home relay
/// retains any of them.
fn configured(
    rooms: &[&str],
    placements: &RegistryPlacements,
) -> io::Result<Option<DeviceOffloadConfig>> {
    let Some(&first) = rooms.first() else {
        return Ok(None);
    };
    let registrations = placements.registrations()?;
    if registrations.is_empty() {
        return Ok(None);
    }
    let identity = placements.identity(first)?;
    Ok(home_relay(
        &registrations,
        identity.user_public_key,
        rooms.iter().copied(),
    )
    .map(|(relay, rooms)| DeviceOffloadConfig {
        relay,
        identity,
        rooms,
    }))
}

/// How this device authenticates in a room: its trusted certificate and
/// key, and whose device fleet carries the person's revocations.
struct RoomIdentity {
    device_signing_key: ed25519_dalek::SigningKey,
    device_cert: tilo_user_identity::DeviceCert,
    /// The selected identity whose device fleet carries the revocations.
    identity_id: String,
    /// Where that fleet is read from.
    fleets: DeviceFleetStore,
}

/// This device's identity in `room`, as the rest of its trust reads it.
fn session_identity(room: &str) -> io::Result<RoomIdentity> {
    let (device_signing_key, device_cert) =
        tilo_identity_host::broker::trusted_current_device(room)?;
    let profile = tilo_identity_host::registry::selected_identity_profile(room)?
        .ok_or_else(|| io::Error::other("no identity is selected for this workspace"))?;
    Ok(RoomIdentity {
        device_signing_key,
        device_cert,
        identity_id: profile.id,
        fleets: DeviceFleetStore::from_env()?,
    })
}

/// Reads this device's identity in a room: [`session_identity`] in a
/// session.
type IdentitySource = dyn Fn(&str) -> io::Result<RoomIdentity> + Send + Sync;

/// Each selected identity's revocations, by its id and the person's key.
type RevocationsByIdentity = HashMap<(String, [u8; 32]), Arc<FleetRevocations>>;

/// The registry file's identity: its length and modification time.
type RegistryStamp = (u64, Option<SystemTime>);

/// Where the device's rooms are placed: the home-peer registry, read again
/// whenever its file changed, and this device's identity in each room.
struct RegistryPlacements {
    path: PathBuf,
    identity: Box<IdentitySource>,
    /// The registry last read, by its file's stamp.
    registry: Mutex<Option<(RegistryStamp, Arc<Vec<HomePeerRegistration>>)>>,
    /// Each identity's revocations, so a fleet is verified once per change.
    revocations: Mutex<RevocationsByIdentity>,
}

impl RegistryPlacements {
    fn new(path: PathBuf, identity: Box<IdentitySource>) -> Self {
        Self {
            path,
            identity,
            registry: Mutex::default(),
            revocations: Mutex::default(),
        }
    }

    /// The registrations now: none when there is no registry.
    fn registrations(&self) -> io::Result<Arc<Vec<HomePeerRegistration>>> {
        let metadata = match std::fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Arc::default());
            }
            Err(error) => return Err(error),
        };
        let stamp = (metadata.len(), metadata.modified().ok());
        if let Some((known, registrations)) = &*self.registry.lock_recover()
            && *known == stamp
        {
            return Ok(Arc::clone(registrations));
        }
        let registrations = Arc::new(read_registry(&self.path)?);
        *self.registry.lock_recover() = Some((stamp, Arc::clone(&registrations)));
        Ok(registrations)
    }

    /// This device as it authenticates in `room`.
    fn identity(&self, room: &str) -> io::Result<OffloadIdentity> {
        let RoomIdentity {
            device_signing_key,
            device_cert,
            identity_id,
            fleets,
        } = (self.identity)(room)?;
        let user_public_key = device_cert.user_public_key;
        let revocations = Arc::clone(
            self.revocations
                .lock_recover()
                .entry((identity_id.clone(), user_public_key))
                .or_insert_with(|| {
                    Arc::new(FleetRevocations::new(fleets, identity_id, user_public_key))
                }),
        );
        Ok(OffloadIdentity {
            user_public_key,
            device_cert,
            device_signing_key,
            revocations,
        })
    }
}

impl RoomPlacements for RegistryPlacements {
    fn resolve(&self, room: &str) -> Result<Option<RoomRelays>, String> {
        let registrations = self
            .registrations()
            .map_err(|error| format!("the home-peer registry is unreadable: {error}"))?;
        if registrations.is_empty() {
            return Ok(None);
        }
        let identity = self
            .identity(room)
            .map_err(|error| format!("no trusted identity in the room: {error}"))?;
        Ok(
            room_relays(&registrations, identity.user_public_key, room).map(
                |(workspace_id, relays)| RoomRelays {
                    identity,
                    workspace_id,
                    relays,
                },
            ),
        )
    }
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
