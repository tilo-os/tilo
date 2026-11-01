//! Where a session's offloadable actions run: the action door's route
//! ([`tilo_actions::offload::ActionOffload`]) a host hands its broker as
//! `HostServices::action_offload`.
//!
//! A host installs one of two routes. A session of the device's own, with the
//! workspace's other devices at hand, installs the offer round
//! ([`offer_offload`]): it asks them whether they would run the job sooner and
//! sends the invocation to the device that wins. A host of a person's
//! workspace — the desktop session and the standalone runtime — installs the
//! route below, over its home relay.
//!
//! That route is configured by the device's home-peer registry, the
//! person's always-on relays this device enrolled: the one that retains the
//! invoking workspace places its compute jobs. The registry is read again for
//! every offloadable invocation (from memory while the file is unchanged),
//! so a workspace the session creates or joins after it started, or a relay the
//! person registers meanwhile, is routed from the next invocation on
//! ([`tilo_relay::offload::RoomOffload`]); the session's workspaces at start get
//! their route at once ([`tilo_relay::offload::home_relay`]). A workspace no
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
//! Both routes read and keep only the workspaces' own document media, in the
//! same content store the apps' blob socket serves, through the invoking
//! app's scope in its workspace, so the inputs it may send are exactly what the
//! invoking workspace's apps could read, and the outputs are read back by the
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
    time::{Duration, Instant, SystemTime},
};

use neighbour_wire::offer::{MAX_WORK_INPUTS, Route, Work};
use runtime_fs::blobs::BlobScope;
use tilo_actions::offload::{ActionOffload, OffloadFuture, OffloadRequest, Offloaded};
use tilo_actions::remote::InvokeRequest;
use tilo_actions::{InvocationId, Offload};
use tilo_identity_host::device_fleet::{DeviceFleet, DeviceFleetStore};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::offload::{OffloadPeers, RanAway};
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
/// content store the workspaces' apps keep their document media in: for `workspaces`
/// (the broker's cluster ids at start), whose route starts at once, and for
/// every workspace the session has later, resolved when its actions are invoked.
/// `None` — every offloadable action runs here, exactly as without offload —
/// only when offload is turned off or there is no app runtime. Never blocks
/// on the network.
#[must_use]
pub fn action_offload(
    workspaces: &[&str],
    store: Arc<tilo_fs::ContentStore>,
) -> Option<Arc<dyn ActionOffload>> {
    action_offload_over(
        workspaces,
        tilo_fs::document_media::DocumentMedia::new(store),
    )
}

/// [`action_offload`] for the desktop session, whose apps keep their
/// document media in the filesystem daemon at `blob_socket`: the route
/// reads its inputs and keeps its outputs there, connected as the desktop
/// session, through the invoking app's scope in its workspace — the same door
/// and the same authority the session opens app content with. Never
/// connects before an offloadable action is invoked.
#[must_use]
pub fn session_action_offload(
    workspaces: &[&str],
    blob_socket: &Path,
) -> Option<Arc<dyn ActionOffload>> {
    action_offload_over(workspaces, Arc::new(SessionMedia::new(blob_socket)))
}

/// The route for `workspaces` over `media`, as [`action_offload`] describes.
fn action_offload_over(
    workspaces: &[&str],
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
    match configured(workspaces, &placements) {
        Ok(Some(config)) => offload.hold(config),
        Ok(None) => tracing::info!(
            "no home relay retains the session's workspaces: their offloadable actions run on this \
             device until one is registered"
        ),
        Err(error) => tracing::warn!(
            %error,
            "the session's workspaces get no offload route at start; each invocation tries again"
        ),
    }
    Some(Arc::new(offload))
}

/// The route for offloadable invocations on this device over the offer round:
/// it asks the workspace's other devices whether they would run the job
/// sooner, through `peers` — a host's device plane, which asks as this device
/// and runs the invocation on the device that wins — and keeps the answer's
/// content where the invoking app reads its output.
///
/// The round is weighed with what this device can measure: the inputs' bytes
/// in `media`, through the invoking app's scope in its workspace, exactly as
/// the app would read them, and how long the same transform last took here.
/// Every input must be here, or the job runs here: content this device does
/// not hold is nothing it can weigh moving. `None` — every offloadable action
/// runs here, exactly as without offload — when offload is turned off.
#[must_use]
pub fn offer_offload(
    media: Arc<dyn RoomMedia>,
    peers: Arc<dyn OffloadPeers>,
) -> Option<Arc<dyn ActionOffload>> {
    if disabled(std::env::var(OFFLOAD_ENV).ok().as_deref()) {
        tracing::info!("offloadable actions run on this device: offload is turned off");
        return None;
    }
    Some(Arc::new(OfferOffload {
        media,
        peers,
        ran: Mutex::default(),
        running: Mutex::default(),
        away: Mutex::default(),
    }))
}

/// Local runs awaited at once before the oldest are forgotten: an invocation
/// that expires never reports its run.
const MAX_LOCAL_RUNS: usize = 64;

/// How long one transform last took where it was asked, over how many input
/// bytes: what moving a job's inputs has to beat.
#[derive(Clone, Copy)]
struct Run {
    elapsed: Duration,
    input_bytes: u64,
}

impl Run {
    /// This run's wall time scaled to `input_bytes` of input, in whole
    /// milliseconds: the transforms offloaded are batch work whose cost grows
    /// with what they read.
    fn ms(self, input_bytes: u64) -> u64 {
        let millis = self.elapsed.as_millis();
        let scaled = if self.input_bytes == 0 {
            millis
        } else {
            millis
                .saturating_mul(u128::from(input_bytes))
                .div_ceil(u128::from(self.input_bytes))
        };
        u64::try_from(scaled).unwrap_or(u64::MAX)
    }
}

/// Offloadable invocations of this device's workspaces, run where the offer
/// round picks.
struct OfferOffload {
    media: Arc<dyn RoomMedia>,
    peers: Arc<dyn OffloadPeers>,
    /// How long each transform last took where it was asked, by
    /// `package:entrypoint`.
    ran: Mutex<HashMap<String, Run>>,
    /// Input bytes of the invocations handed to the app to run here, by
    /// workspace and inputs, until the door reports that run.
    running: Mutex<HashMap<(String, Vec<String>), u64>>,
    /// The invocations this device sent to another device, and where: one that
    /// comes back is not sent away again.
    away: Mutex<HashMap<InvocationId, [u8; 32]>>,
}

impl OfferOffload {
    /// Where `request` runs: on the device the round picks, or here, with why.
    async fn place(&self, request: OffloadRequest, invocation: Option<InvokeRequest>) -> Offloaded {
        let started = Instant::now();
        // Only a route handed the invocation can send it to another device:
        // without it there is no request to send, so the app runs this one.
        let Some(invocation) = invocation else {
            return Offloaded::RunHere {
                reason: "the invocation was not handed to the route".into(),
            };
        };
        let scope = match BlobScope::new(request.workspace.clone(), request.app_id.clone()) {
            Ok(scope) => scope,
            Err(error) => {
                return Offloaded::RunHere {
                    reason: format!("the workspace's content is not addressable: {error}"),
                };
            }
        };
        let (work, input_bytes) = match self.work(&request, &scope).await {
            Ok(work) => work,
            Err(reason) => {
                tracing::info!(app = %request.offload.app, %reason, "offloadable action runs here");
                return Offloaded::RunHere { reason };
            }
        };
        let reason = match self
            .away(&request, &scope, &work, invocation, started)
            .await
        {
            Ok(ran) => return ran,
            Err(reason) => reason,
        };
        {
            let mut running = self.running.lock_recover();
            // A local run the door never reports (it failed) is forgotten
            // rather than kept forever.
            if running.len() >= MAX_LOCAL_RUNS {
                running.clear();
            }
            running.insert(
                (request.workspace.clone(), request.inputs.clone()),
                input_bytes,
            );
        }
        tracing::info!(
            app = %request.offload.app,
            entrypoint = %request.offload.entrypoint,
            %reason,
            "offloadable action runs here"
        );
        Offloaded::RunHere { reason }
    }

    /// What the round is asked: `request`'s inputs as this device's store
    /// measures them, each distinct digest once, and how long the same
    /// transform last took here, scaled to their bytes.
    async fn work(
        &self,
        request: &OffloadRequest,
        scope: &BlobScope,
    ) -> Result<(Work, u64), String> {
        let mut inputs = Vec::with_capacity(request.inputs.len().min(MAX_WORK_INPUTS));
        let mut input_bytes = 0_u64;
        for (index, hex) in request.inputs.iter().enumerate() {
            let mut digest = [0; 32];
            hex::decode_to_slice(hex, &mut digest)
                .map_err(|_| format!("input {index} is not a content hash"))?;
            if inputs.iter().any(|(named, _)| *named == digest) {
                continue;
            }
            let bytes = self
                .media
                .len(scope, digest)
                .await
                .map_err(|error| format!("input {index}: {error}"))?;
            input_bytes = input_bytes.saturating_add(bytes);
            if inputs.len() < MAX_WORK_INPUTS {
                inputs.push((digest, bytes));
            }
        }
        let local_wall_ms = self
            .ran
            .lock_recover()
            .get(&transform(&request.offload))
            .map(|ran| ran.ms(input_bytes));
        Ok((
            Work {
                inputs,
                input_bytes,
                local_wall_ms,
            },
            input_bytes,
        ))
    }

    /// Ask the round and run the invocation on the device that wins it, or say
    /// why this device runs it instead.
    async fn away(
        &self,
        request: &OffloadRequest,
        scope: &BlobScope,
        work: &Work,
        invocation: InvokeRequest,
        started: Instant,
    ) -> Result<Offloaded, String> {
        let sent_to = self
            .away
            .lock_recover()
            .get(&invocation.invocation)
            .copied();
        if let Some(device) = sent_to {
            let mut short = hex::encode(device);
            short.truncate(8);
            return Err(format!(
                "the invocation is already running on the device {short} sent it to"
            ));
        }
        let route = Route::new(
            request.app_id.clone(),
            request.address.resource.clone(),
            request.address.action.clone(),
        );
        let choice = self
            .peers
            .ask(&request.workspace, &route, work)
            .await
            .ok_or("the offer round was abandoned before it finished")?;
        let Some(worker) = choice.worker else {
            return Err(choice.to_string());
        };
        self.away
            .lock_recover()
            .insert(invocation.invocation, worker);
        let ran = self
            .peers
            .run(&request.workspace, worker, invocation.clone())
            .await;
        self.away.lock_recover().remove(&invocation.invocation);
        let ran: RanAway = match ran {
            Some(ran) => ran.map_err(|reason| {
                let mut short = hex::encode(worker);
                short.truncate(8);
                format!("{short} did not run it: {reason}")
            })?,
            None => return Err("the device that won the round did not answer".into()),
        };
        self.keep(scope, &ran).await?;
        Ok(Offloaded::Ran {
            output: ran.output,
            device: hex::encode(worker),
            elapsed: started.elapsed(),
        })
    }

    /// Keep what the device that ran the invocation produced as this
    /// workspace's app content, in the order it named: the app that invoked the
    /// action here reads its output by the addresses a local run returns, and
    /// bytes that are not what the device named never become that output.
    async fn keep(&self, scope: &BlobScope, ran: &RanAway) -> Result<(), String> {
        let mut produced = ran.outputs.iter();
        for address in &ran.output.resources {
            let Some(named) = tilo_actions::content_hash(address) else {
                continue;
            };
            let Some(bytes) = produced.next() else {
                return Err("the answer names more content than the device sent".into());
            };
            let kept = self
                .media
                .keep(scope, bytes.clone())
                .await
                .map_err(|error| format!("the answer was not kept here: {error}"))?;
            if hex::encode(kept) != named {
                return Err("the answer's bytes are not the content it names".into());
            }
        }
        if produced.next().is_some() {
            return Err("the answer's content is not what it names".into());
        }
        Ok(())
    }
}

impl ActionOffload for OfferOffload {
    fn offload(&self, request: OffloadRequest) -> OffloadFuture<'_> {
        Box::pin(self.place(request, None))
    }

    fn offload_invocation(
        &self,
        request: OffloadRequest,
        invocation: InvokeRequest,
    ) -> OffloadFuture<'_> {
        Box::pin(self.place(request, Some(invocation)))
    }

    fn ran_here(&self, request: &OffloadRequest, elapsed: Duration) {
        let Some(input_bytes) = self
            .running
            .lock_recover()
            .remove(&(request.workspace.clone(), request.inputs.clone()))
        else {
            return;
        };
        self.ran.lock_recover().insert(
            transform(&request.offload),
            Run {
                elapsed,
                input_bytes,
            },
        );
    }
}

/// The transform `offload` declares, as placement knows it:
/// `package:entrypoint`, which is what an action's cost belongs to.
fn transform(offload: &Offload) -> String {
    format!("{}:{}", offload.app, offload.entrypoint)
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

/// The start route's configuration for `workspaces`, from the registry and this
/// device's identity in the first workspace; `Ok(None)` when no home relay
/// retains any of them.
fn configured(
    workspaces: &[&str],
    placements: &RegistryPlacements,
) -> io::Result<Option<DeviceOffloadConfig>> {
    let Some(&first) = workspaces.first() else {
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
        workspaces.iter().copied(),
    )
    .map(|(relay, workspaces)| DeviceOffloadConfig {
        relay,
        identity,
        rooms: workspaces,
    }))
}

/// How this device authenticates in a workspace: its trusted certificate and
/// key, and whose device fleet carries the person's revocations.
struct RoomIdentity {
    device_signing_key: ed25519_dalek::SigningKey,
    device_cert: tilo_user_identity::DeviceCert,
    /// The selected identity whose device fleet carries the revocations.
    identity_id: String,
    /// Where that fleet is read from.
    fleets: DeviceFleetStore,
}

/// This device's identity in `workspace`, as the rest of its trust reads it.
fn session_identity(workspace: &str) -> io::Result<RoomIdentity> {
    let (device_signing_key, device_cert) =
        tilo_identity_host::broker::trusted_current_device(workspace)?;
    let profile = tilo_identity_host::registry::selected_identity_profile(workspace)?
        .ok_or_else(|| io::Error::other("no identity is selected for this workspace"))?;
    Ok(RoomIdentity {
        device_signing_key,
        device_cert,
        identity_id: profile.id,
        fleets: DeviceFleetStore::from_env()?,
    })
}

/// Reads this device's identity in a workspace: [`session_identity`] in a
/// session.
type IdentitySource = dyn Fn(&str) -> io::Result<RoomIdentity> + Send + Sync;

/// Each selected identity's revocations, by its id and the person's key.
type RevocationsByIdentity = HashMap<(String, [u8; 32]), Arc<FleetRevocations>>;

/// The registry file's identity: its length and modification time.
type RegistryStamp = (u64, Option<SystemTime>);

/// Where the device's workspaces are placed: the home-peer registry, read again
/// whenever its file changed, and this device's identity in each workspace.
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

    /// This device as it authenticates in `workspace`.
    fn identity(&self, workspace: &str) -> io::Result<OffloadIdentity> {
        let RoomIdentity {
            device_signing_key,
            device_cert,
            identity_id,
            fleets,
        } = (self.identity)(workspace)?;
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
    fn resolve(&self, workspace: &str) -> Result<Option<RoomRelays>, String> {
        let registrations = self
            .registrations()
            .map_err(|error| format!("the home-peer registry is unreadable: {error}"))?;
        if registrations.is_empty() {
            return Ok(None);
        }
        let identity = self
            .identity(workspace)
            .map_err(|error| format!("no trusted identity in the workspace: {error}"))?;
        Ok(
            room_relays(&registrations, identity.user_public_key, workspace).map(
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
#[path = "offload_offer_tests.rs"]
mod offer_tests;
#[cfg(test)]
#[path = "offload_tests.rs"]
mod tests;
