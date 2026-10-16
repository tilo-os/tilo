//! A device's route for offloadable actions, as its session starts it: from
//! the person's home relay this device is registered with, over the
//! session's own document media.
//!
//! [`home_relay`] reads the configuration — the first of the person's
//! registered home peers that retains a session room's relay workspace,
//! the same `room → workspace` shard every relay binding uses
//! ([`tilo_relay_protocol::room_workspace_id`]). Without one there is no
//! route, and the action door runs every offloadable action here exactly as
//! it does on a device that routes nothing.
//!
//! [`DeviceOffload::start`] hands the session's broker a route at once and
//! connects in the background: until the compute session is up, and for
//! good when it cannot be, the route answers [`Offloaded::RunHere`] with
//! why, so a relay that is slow, unreachable or refuses this device never
//! holds up the session or changes an answer.
//!
//! Whom the route trusts is the person's device revocations as this device
//! knows them ([`DeviceRevocations`]), read again whenever it matters: a
//! revoked relay gets no route, a live route whose relay (or this device)
//! is revoked is torn down within [`REVOCATION_CHECK`] — runs in flight
//! then answer "run here" — and a worker placement names, or that ran a
//! job, is not trusted once it is revoked. Revocations that cannot be read
//! trust nobody.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use fs::document_media::DocumentMedia;
use fs::store::ContentStore;
use iroh_base::EndpointAddr;
use tilo_actions::offload::{ActionOffload, OffloadFuture, OffloadRequest, Offloaded};
use tilo_lock::RwLockRecover as _;
use tilo_relay_protocol::HomePeerRegistration;
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::watch;

use crate::RelayClient;
use crate::compute::{ComputePackageCatalogue, InstalledComputeCatalogue};
use crate::compute_session::ComputeClient;
use crate::mesh::DeviceKey;
use crate::offload::{Offloader, OffloaderParts, WorkspaceContent};
use crate::offload_placement::{ComputePlacement, PlacementAnswer, PlacementQuery, RelayPlacement};
use crate::offload_route::OffloadRoute;
use crate::transport::Envelope;

/// How long the route has to connect to the relay and attach its rooms'
/// workspaces before offloadable actions run here for this session.
pub const START_TIMEOUT: Duration = Duration::from_secs(30);

/// How often a live route reads the device revocations again: the longest a
/// relay revoked meanwhile keeps the route.
pub const REVOCATION_CHECK: Duration = Duration::from_secs(2);

/// The person's device revocations as this device currently knows them: the
/// same set the rest of its device trust reads, so a device revoked there is
/// revoked for offload too.
pub trait DeviceRevocations: Send + Sync {
    /// The revocations now.
    ///
    /// # Errors
    ///
    /// Why they cannot be read. The route then trusts no device.
    fn current(&self) -> Result<RevocationList, String>;
}

/// A fixed set, for a device whose revocations never change while it runs.
impl DeviceRevocations for RevocationList {
    fn current(&self) -> Result<RevocationList, String> {
        Ok(self.clone())
    }
}

/// Whom a route trusts: its relay and this device, while neither is revoked.
#[derive(Clone)]
pub struct RouteTrust {
    /// The relay's device key.
    pub relay: DeviceKey,
    /// This device's key.
    pub device: DeviceKey,
    /// The revocations both are checked against.
    pub revocations: Arc<dyn DeviceRevocations>,
}

impl RouteTrust {
    /// The current revocations, when they revoke neither the relay nor this
    /// device.
    ///
    /// # Errors
    ///
    /// Why the route may not be used: the relay or this device revoked, or
    /// the revocations unreadable.
    pub fn check(&self) -> Result<RevocationList, String> {
        let revocations = self
            .revocations
            .current()
            .map_err(|error| format!("this device's revocations are unreadable: {error}"))?;
        if revocations.is_revoked(&self.relay) {
            return Err(format!(
                "the relay {} has been revoked",
                hex::encode(self.relay)
            ));
        }
        if revocations.is_revoked(&self.device) {
            return Err("this device has been revoked".into());
        }
        Ok(revocations)
    }
}

/// `device`, named by placement or as having run a job, when it is not
/// revoked.
fn trusted_device(revocations: &dyn DeviceRevocations, device: &DeviceKey) -> Result<(), String> {
    let revocations = revocations
        .current()
        .map_err(|error| format!("this device's revocations are unreadable: {error}"))?;
    if revocations.is_revoked(device) {
        return Err(format!("device {} has been revoked", hex::encode(device)));
    }
    Ok(())
}

/// Placement that never hands the offloader a revoked worker: the relay's
/// own list may lag this device's.
struct TrustedPlacement<P> {
    inner: P,
    revocations: Arc<dyn DeviceRevocations>,
}

impl<P: ComputePlacement> ComputePlacement for TrustedPlacement<P> {
    fn place<'a>(&'a self, query: PlacementQuery<'a>) -> PlacementAnswer<'a> {
        Box::pin(async move {
            let placed = self.inner.place(query).await?;
            if let Some(worker) = &placed.worker {
                trusted_device(self.revocations.as_ref(), worker)
                    .map_err(|error| format!("placement named a device not trusted: {error}"))?;
            }
            Ok(placed)
        })
    }
}

/// The relay that places a device's compute jobs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OffloadRelay {
    /// Where its relay service answers.
    pub address: EndpointAddr,
    /// Its own device key: the compute session's pinned server and the
    /// addressee of placement questions.
    pub device: DeviceKey,
}

/// This device, as it authenticates to its relay.
#[derive(Clone)]
pub struct OffloadIdentity {
    /// The person's root key, which certifies both ends.
    pub user_public_key: [u8; 32],
    /// This device's certificate.
    pub device_cert: DeviceCert,
    /// This device's key.
    pub device_signing_key: ed25519_dalek::SigningKey,
    /// The person's device revocations as this device knows them, read
    /// again whenever the route decides whom to trust.
    pub revocations: Arc<dyn DeviceRevocations>,
}

/// Everything a device's offload route is started from.
#[derive(Clone)]
pub struct DeviceOffloadConfig {
    /// The relay.
    pub relay: OffloadRelay,
    /// This device.
    pub identity: OffloadIdentity,
    /// Room (the broker's cluster id) → the relay workspace its compute
    /// jobs are placed in. Only rooms the relay retains.
    pub rooms: HashMap<String, u64>,
}

impl DeviceOffloadConfig {
    /// Whom the route started from this configuration trusts.
    #[must_use]
    pub fn trust(&self) -> RouteTrust {
        RouteTrust {
            relay: self.relay.device,
            device: self.identity.device_cert.device_public_key,
            revocations: Arc::clone(&self.identity.revocations),
        }
    }
}

/// The relay among `registrations` that places compute for `rooms` on a
/// device of `user`, and the rooms it does: the registered home peer of
/// `user` with an advertised endpoint that retains the most of the rooms'
/// relay workspaces (the first such on a tie). `None` when none retains
/// any, which leaves the device without a route.
#[must_use]
pub fn home_relay<'a>(
    registrations: &[HomePeerRegistration],
    user: [u8; 32],
    rooms: impl IntoIterator<Item = &'a str>,
) -> Option<(OffloadRelay, HashMap<String, u64>)> {
    let rooms: Vec<(&str, u64)> = rooms
        .into_iter()
        .filter_map(|room| Some((room, tilo_relay_protocol::room_workspace_id(room).ok()?)))
        .collect();
    let mut best: Option<(OffloadRelay, HashMap<String, u64>)> = None;
    for registration in registrations {
        let Some(relay) = registered_relay(registration, user) else {
            continue;
        };
        let placed: HashMap<String, u64> = rooms
            .iter()
            .filter(|(_, workspace)| registration.relay_workspace_ids.contains(workspace))
            .map(|(room, workspace)| ((*room).to_owned(), *workspace))
            .collect();
        if placed.len() > best.as_ref().map_or(0, |(_, rooms)| rooms.len()) {
            best = Some((relay, placed));
        }
    }
    best
}

/// `registration` as a relay of `user` this device can dial, or `None`.
fn registered_relay(registration: &HomePeerRegistration, user: [u8; 32]) -> Option<OffloadRelay> {
    if registration.schema_version != HomePeerRegistration::CURRENT_SCHEMA
        || registration.user_public_key != hex::encode(user)
    {
        return None;
    }
    let mut device = [0; 32];
    hex::decode_to_slice(&registration.device_node_id, &mut device).ok()?;
    if let Some(cert) = &registration.device_cert
        && (cert.device_public_key != device || cert.user_public_key != user)
    {
        return None;
    }
    Some(OffloadRelay {
        address: registration.relay_endpoint.clone()?,
        device,
    })
}

/// Connect to `config.relay` as `config.identity` from `endpoint`, attach
/// every room's workspace, and route offloadable invocations of those rooms
/// through it over `store`, the content store the rooms' apps keep their
/// media in. Packages resolve from `packages`.
///
/// # Errors
///
/// What failed: no room placed, the relay or this device revoked (or the
/// revocations unreadable) — checked before anything is dialled — the relay
/// unreachable or refusing this device, a workspace it does not let this
/// device compute in, or no answer within [`START_TIMEOUT`].
pub async fn start_route(
    config: DeviceOffloadConfig,
    endpoint: iroh::Endpoint,
    store: Arc<ContentStore>,
    packages: Arc<dyn ComputePackageCatalogue>,
) -> Result<OffloadRoute, String> {
    if config.rooms.is_empty() {
        return Err(NO_ROOM.into());
    }
    let revocations = config.trust().check()?;
    let connecting = connect(config, revocations, endpoint, store, packages);
    tokio::time::timeout(START_TIMEOUT, connecting)
        .await
        .map_err(|_| format!("the relay did not answer within {START_TIMEOUT:?}"))?
}

/// Why a device with no room on its relay has no route.
const NO_ROOM: &str = "no room is placed on the relay";

/// Connect as [`start_route`] does, both sessions checking the relay
/// against `revocations`.
async fn connect(
    config: DeviceOffloadConfig,
    revocations: RevocationList,
    endpoint: iroh::Endpoint,
    store: Arc<ContentStore>,
    packages: Arc<dyn ComputePackageCatalogue>,
) -> Result<OffloadRoute, String> {
    let DeviceOffloadConfig {
        relay,
        identity,
        rooms,
    } = config;
    let mut workspaces: Vec<u64> = rooms.values().copied().collect();
    workspaces.sort_unstable();
    workspaces.dedup();
    let placement = RelayClient::connect_to_enrolled_home_peer(
        endpoint.clone(),
        relay.address.clone(),
        identity.user_public_key,
        relay.device,
        identity.device_cert.clone(),
        identity.device_signing_key.clone(),
        revocations.clone(),
    )
    .await
    .map_err(|error| format!("the relay session failed: {error}"))?;
    subscribe(&placement, &workspaces).await?;
    // From here the room's own traffic is drained as it arrives.
    let placement = TrustedPlacement {
        inner: RelayPlacement::new(placement, relay.device),
        revocations: Arc::clone(&identity.revocations),
    };
    let compute = ComputeClient::connect(
        endpoint,
        relay.address,
        identity.user_public_key,
        Some(relay.device),
        identity.device_cert,
        identity.device_signing_key,
        revocations,
    )
    .await
    .map_err(|error| format!("the compute session failed: {error}"))?;
    let offloader = Offloader::start(OffloaderParts {
        compute,
        workspaces,
        content: WorkspaceContent::new(Arc::clone(&store), tokio::runtime::Handle::current()),
        placement: Arc::new(placement),
        packages,
    })
    .await
    .map_err(|error| format!("the relay refused compute: {error}"))?;
    Ok(OffloadRoute::new(
        Arc::new(offloader),
        DocumentMedia::new(store),
        rooms,
    ))
}

/// Subscribe `client` to every one of `workspaces` and wait for each to be
/// ready: placement is asked in them.
async fn subscribe(client: &RelayClient, workspaces: &[u64]) -> Result<(), String> {
    for &workspace_id in workspaces {
        client
            .send(Envelope::Subscribe { workspace_id })
            .map_err(|_| "the relay session closed".to_string())?;
        loop {
            match client.recv().await {
                Some(Envelope::Ready {
                    workspace_id: ready,
                    ..
                }) if ready == workspace_id => break,
                Some(Envelope::NotAuthorised {
                    workspace_id: refused,
                    reason,
                }) if refused == workspace_id => {
                    return Err(format!(
                        "the relay refused workspace {workspace_id}: {reason}"
                    ));
                }
                Some(_) => {}
                None => return Err("the relay session closed".into()),
            }
        }
    }
    Ok(())
}

/// A device's offload route as its session's broker holds it: started in
/// the background, and until it is up — for good when it cannot start, and
/// from the moment its relay or this device is revoked — answering
/// [`Offloaded::RunHere`] with why.
pub struct DeviceOffload {
    phase: RwLock<Phase>,
    trust: RouteTrust,
    /// Why the route was torn down, once it is: every run in flight ends on
    /// it.
    torn_down: watch::Sender<Option<String>>,
}

/// Where a [`DeviceOffload`] is.
enum Phase {
    Starting,
    Up(Arc<OffloadRoute>),
    /// It did not start, and why.
    Failed(String),
    /// It was up and is no longer trusted, and why.
    TornDown(String),
}

impl DeviceOffload {
    /// Start the route for `config` over `store` on `runtime` — from an
    /// endpoint of its own, resolving packages from this device's installed
    /// catalogue — and return it at once.
    #[must_use]
    pub fn start(
        config: DeviceOffloadConfig,
        store: Arc<ContentStore>,
        runtime: &tokio::runtime::Handle,
    ) -> Arc<Self> {
        Self::spawn(
            config.trust(),
            async move {
                if config.rooms.is_empty() {
                    return Err(NO_ROOM.to_owned());
                }
                let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
                    .bind()
                    .await
                    .map_err(|error| format!("no endpoint to reach the relay from: {error}"))?;
                start_route(config, endpoint, store, Arc::new(InstalledComputeCatalogue)).await
            },
            runtime,
        )
    }

    /// The route `start` brings up on `runtime` ([`start_route`]), returned
    /// at once, and kept only while `trust` holds: checked as soon as it is
    /// up and every [`REVOCATION_CHECK`] after. A failed start, and a
    /// teardown, are logged once.
    #[must_use]
    pub fn spawn(
        trust: RouteTrust,
        start: impl Future<Output = Result<OffloadRoute, String>> + Send + 'static,
        runtime: &tokio::runtime::Handle,
    ) -> Arc<Self> {
        let offload = Arc::new(Self {
            phase: RwLock::new(Phase::Starting),
            trust,
            torn_down: watch::Sender::new(None),
        });
        let weak: Weak<Self> = Arc::downgrade(&offload);
        runtime.spawn(async move {
            let started = start.await;
            let Some(offload) = weak.upgrade() else {
                return;
            };
            *offload.phase.write_recover() = match started {
                Ok(route) => {
                    tracing::info!("offloadable actions are routed through the home relay");
                    Phase::Up(Arc::new(route))
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "offloadable actions run on this device: the offload route did not start"
                    );
                    Phase::Failed(error)
                }
            };
            drop(offload);
            keep_trusted(weak).await;
        });
        offload
    }

    /// Whether the route is up: `None` while it starts, then whether it
    /// is, and why not — it did not start, or it was torn down.
    #[must_use]
    pub fn started(&self) -> Option<Result<(), String>> {
        let phase = self.phase.read_recover();
        match &*phase {
            Phase::Starting => None,
            Phase::Up(_) => Some(Ok(())),
            Phase::Failed(error) | Phase::TornDown(error) => Some(Err(error.clone())),
        }
    }

    /// The route while it is up, or why every offloadable action runs here.
    fn route(&self) -> Result<Arc<OffloadRoute>, String> {
        let phase = self.phase.read_recover();
        match &*phase {
            Phase::Up(route) => Ok(Arc::clone(route)),
            Phase::Starting => Err("the offload route is still starting".to_owned()),
            Phase::Failed(error) => Err(format!("the offload route did not start: {error}")),
            Phase::TornDown(error) => Err(format!("the offload route was torn down: {error}")),
        }
    }

    /// Tear the route down for `reason`: every run in flight and every
    /// later one runs here, and the relay sessions close as the last run
    /// lets go of them.
    fn tear_down(&self, reason: String) {
        tracing::warn!(
            %reason,
            "offloadable actions run on this device: the offload route is torn down"
        );
        let route = std::mem::replace(
            &mut *self.phase.write_recover(),
            Phase::TornDown(reason.clone()),
        );
        self.torn_down.send_replace(Some(reason));
        drop(route);
    }

    /// `ran`, unless the device that ran it — or the route itself — is no
    /// longer trusted: its outputs are then not taken, and it runs here.
    fn trusted(&self, ran: Offloaded) -> Offloaded {
        let Offloaded::Ran { device, .. } = &ran else {
            return ran;
        };
        let mut key = [0; 32];
        let verdict = self
            .trust
            .check()
            .and_then(|_| {
                hex::decode_to_slice(device, &mut key)
                    .map_err(|_| "the run names no device".to_owned())
            })
            .and_then(|()| trusted_device(self.trust.revocations.as_ref(), &key));
        match verdict {
            Ok(()) => ran,
            Err(error) => Offloaded::RunHere {
                reason: format!("the offloaded run is not trusted: {error}"),
            },
        }
    }
}

/// While the route `weak` holds is up, check its trust at once and every
/// [`REVOCATION_CHECK`] after, and tear it down the moment it fails.
async fn keep_trusted(weak: Weak<DeviceOffload>) {
    loop {
        let Some(offload) = weak.upgrade() else {
            return;
        };
        if offload.route().is_err() {
            return;
        }
        if let Err(reason) = offload.trust.check() {
            offload.tear_down(reason);
            return;
        }
        drop(offload);
        tokio::time::sleep(REVOCATION_CHECK).await;
    }
}

/// Why `torn_down` ended the route, once it does.
async fn torn_down_reason(torn_down: &mut watch::Receiver<Option<String>>) -> String {
    let reason = torn_down
        .wait_for(Option::is_some)
        .await
        .map(|reason| reason.clone().unwrap_or_default());
    match reason {
        Ok(reason) => reason,
        // The sender lives as long as the route it guards.
        Err(_) => std::future::pending().await,
    }
}

impl ActionOffload for DeviceOffload {
    fn offload(&self, request: OffloadRequest) -> OffloadFuture<'_> {
        let route = match self.route() {
            Ok(route) => route,
            Err(reason) => return run_here(reason),
        };
        let mut torn_down = self.torn_down.subscribe();
        Box::pin(async move {
            let ran = tokio::select! {
                ran = route.offload(request) => ran,
                reason = torn_down_reason(&mut torn_down) => {
                    return Offloaded::RunHere {
                        reason: format!("the offload route was torn down: {reason}"),
                    };
                }
            };
            self.trusted(ran)
        })
    }

    fn ran_here(&self, request: &OffloadRequest, elapsed: Duration) {
        if let Ok(route) = self.route() {
            route.ran_here(request, elapsed);
        }
    }
}

/// An answer of "run here", for `reason`.
fn run_here(reason: String) -> OffloadFuture<'static> {
    Box::pin(async move { Offloaded::RunHere { reason } })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOM: &str = "0b5c7a7e-6f43-4a39-9d1c-1d3f2a6c0e11";
    const OTHER_ROOM: &str = "5d1f6c0a-2b7e-4c8f-9a3d-8e2b1c4f6a77";
    const USER: [u8; 32] = [7; 32];
    const RELAY: [u8; 32] = [9; 32];

    fn address(seed: u8) -> EndpointAddr {
        EndpointAddr::new(iroh_base::SecretKey::from_bytes(&[seed; 32]).public())
    }

    fn registration(rooms: &[&str]) -> HomePeerRegistration {
        HomePeerRegistration {
            schema_version: HomePeerRegistration::CURRENT_SCHEMA,
            device_node_id: hex::encode(RELAY),
            relay_service_node_id: hex::encode([10; 32]),
            relay_endpoint: Some(address(10)),
            label: "home".into(),
            user_public_key: hex::encode(USER),
            device_cert: None,
            relay_workspace_ids: rooms
                .iter()
                .map(|room| tilo_relay_protocol::room_workspace_id(room).unwrap())
                .collect(),
            environments: tilo_relay_protocol::RelayEnvironments::default(),
        }
    }

    #[test]
    fn the_home_relay_retaining_a_session_room_places_its_compute() {
        let (relay, rooms) = home_relay(&[registration(&[ROOM])], USER, [ROOM, OTHER_ROOM])
            .expect("the relay retains the room");
        assert_eq!(relay.device, RELAY);
        assert_eq!(relay.address, address(10));
        assert_eq!(
            rooms,
            HashMap::from([(
                ROOM.to_owned(),
                tilo_relay_protocol::room_workspace_id(ROOM).unwrap()
            )]),
            "only the rooms the relay retains, by the relay binding's shard"
        );
    }

    #[test]
    fn no_registered_relay_for_the_rooms_means_no_route() {
        assert!(home_relay(&[], USER, [ROOM]).is_none(), "unconfigured");
        assert!(
            home_relay(&[registration(&[OTHER_ROOM])], USER, [ROOM]).is_none(),
            "a relay retaining none of the session's rooms"
        );
        assert!(
            home_relay(&[registration(&[ROOM])], [8; 32], [ROOM]).is_none(),
            "another person's relay"
        );
        let mut silent = registration(&[ROOM]);
        silent.relay_endpoint = None;
        assert!(
            home_relay(&[silent], USER, [ROOM]).is_none(),
            "a relay that never advertised where it answers"
        );
        let mut stale = registration(&[ROOM]);
        stale.schema_version = HomePeerRegistration::CURRENT_SCHEMA.saturating_sub(1);
        assert!(home_relay(&[stale], USER, [ROOM]).is_none());
        assert!(
            home_relay(&[registration(&[ROOM])], USER, ["not-a-room"]).is_none(),
            "a room that is not a room id has no shard"
        );
    }

    #[test]
    fn the_relay_retaining_most_of_the_rooms_wins() {
        let mut both = registration(&[ROOM, OTHER_ROOM]);
        both.device_node_id = hex::encode([11; 32]);
        let (relay, rooms) =
            home_relay(&[registration(&[ROOM]), both], USER, [ROOM, OTHER_ROOM]).unwrap();
        assert_eq!(relay.device, [11; 32]);
        assert_eq!(rooms.len(), 2);
    }

    /// This device's key.
    const LAPTOP: [u8; 32] = [3; 32];

    /// A laptop's route configuration for `rooms`, trusting by `revocations`.
    fn config(
        rooms: HashMap<String, u64>,
        revocations: Arc<dyn DeviceRevocations>,
    ) -> DeviceOffloadConfig {
        DeviceOffloadConfig {
            relay: OffloadRelay {
                address: address(10),
                device: RELAY,
            },
            identity: OffloadIdentity {
                user_public_key: USER,
                device_cert: DeviceCert {
                    user_public_key: USER,
                    device_public_key: LAPTOP,
                    device_label: "laptop".into(),
                    capabilities: tilo_user_identity::DeviceCapabilities::primary(),
                    issued_at_ns: 0,
                    expires_at_ns: None,
                    signature: [0; 64],
                },
                device_signing_key: ed25519_dalek::SigningKey::from_bytes(&LAPTOP),
                revocations,
            },
            rooms,
        }
    }

    fn request() -> OffloadRequest {
        OffloadRequest {
            room: ROOM.into(),
            app_id: "so.tilo.photos".into(),
            address: tilo_actions::ActionAddress::new("photos.library:main", "make-thumbnails"),
            offload: tilo_actions::Offload {
                app: "so.tilo.thumbnails".into(),
                entrypoint: "thumbnail-batch".into(),
                gpu: false,
                cpu_time_ms: 1,
                memory_bytes: 1,
                output_bytes: 1,
            },
            inputs: Vec::new(),
        }
    }

    /// The person's root-signed revocation of each of `devices`.
    fn revoked(devices: &[DeviceKey]) -> RevocationList {
        let root = ed25519_dalek::SigningKey::from_bytes(&[21; 32]);
        let mut revocations = RevocationList::new();
        for device in devices {
            let mut entry = tilo_user_identity::RevocationEntry {
                user_public_key: root.verifying_key().to_bytes(),
                revoked_device_public_key: *device,
                revoked_at_ns: 1,
                reason: tilo_user_identity::RevocationReason::Stolen,
                signature: [0; 64],
            };
            tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
            revocations.insert(entry).unwrap();
        }
        revocations
    }

    /// Revocations that cannot be read.
    struct Unreadable;

    impl DeviceRevocations for Unreadable {
        fn current(&self) -> Result<RevocationList, String> {
            Err("the fleet is corrupt".into())
        }
    }

    async fn endpoint() -> iroh::Endpoint {
        iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .bind()
            .await
            .unwrap()
    }

    /// Wait until `offload` has started or failed to.
    async fn settled(offload: &DeviceOffload) {
        tilo_test_env::within("the route's start", async {
            while offload.started().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await;
    }

    #[tokio::test]
    async fn the_route_runs_everything_here_until_it_starts_and_when_it_cannot() {
        // Nothing placed: the start fails before dialling anything.
        let config = config(HashMap::new(), Arc::new(RevocationList::new()));
        let offload = DeviceOffload::spawn(
            config.trust(),
            start_route(
                config,
                endpoint().await,
                Arc::new(ContentStore::memory()),
                Arc::new(InstalledComputeCatalogue),
            ),
            &tokio::runtime::Handle::current(),
        );
        let request = request();
        if offload.started().is_none() {
            let Offloaded::RunHere { reason } = offload.offload(request.clone()).await else {
                panic!("nothing runs elsewhere before the route is up");
            };
            assert!(reason.contains("still starting"), "{reason}");
        }
        settled(&offload).await;
        assert_eq!(
            offload.started(),
            Some(Err("no room is placed on the relay".to_owned()))
        );
        let Offloaded::RunHere { reason } = offload.offload(request.clone()).await else {
            panic!("a route that did not start runs everything here");
        };
        assert!(
            reason.contains("did not start: no room is placed on the relay"),
            "{reason}"
        );
        // A local run it never routed is not recorded anywhere.
        offload.ran_here(&request, Duration::from_millis(5));
    }

    #[tokio::test]
    async fn a_revoked_relay_or_device_or_unreadable_revocations_get_no_route() {
        let rooms = HashMap::from([(
            ROOM.to_owned(),
            tilo_relay_protocol::room_workspace_id(ROOM).unwrap(),
        )]);
        let cases: [(Arc<dyn DeviceRevocations>, &str); 3] = [
            (Arc::new(revoked(&[RELAY])), "the relay"),
            (Arc::new(revoked(&[LAPTOP])), "this device has been revoked"),
            (Arc::new(Unreadable), "unreadable: the fleet is corrupt"),
        ];
        for (revocations, why) in cases {
            let config = config(rooms.clone(), revocations);
            // Refused before anything is dialled: nothing answers at the
            // relay's address, so a dial would fail differently, and later.
            let refused = start_route(
                config.clone(),
                endpoint().await,
                Arc::new(ContentStore::memory()),
                Arc::new(InstalledComputeCatalogue),
            )
            .await;
            let Err(error) = refused else {
                panic!("{why}: no route");
            };
            assert!(error.contains(why), "{error}");
            let offload = DeviceOffload::spawn(
                config.trust(),
                start_route(
                    config,
                    endpoint().await,
                    Arc::new(ContentStore::memory()),
                    Arc::new(InstalledComputeCatalogue),
                ),
                &tokio::runtime::Handle::current(),
            );
            settled(&offload).await;
            let Offloaded::RunHere { reason } = offload.offload(request()).await else {
                panic!("{why}: everything runs here");
            };
            assert!(reason.contains("did not start"), "{reason}");
            assert!(reason.contains(why), "{reason}");
        }
        let error = config(rooms, Arc::new(revoked(&[RELAY])))
            .trust()
            .check()
            .unwrap_err();
        assert!(
            error.contains(&hex::encode(RELAY)),
            "names the relay: {error}"
        );
    }

    /// Placement that always names `worker`.
    struct Names(DeviceKey);

    impl ComputePlacement for Names {
        fn place<'a>(&'a self, _: PlacementQuery<'a>) -> PlacementAnswer<'a> {
            let worker = self.0;
            Box::pin(async move {
                Ok(crate::offload_placement::Placed {
                    worker: Some(worker),
                    reason: "faster there".into(),
                })
            })
        }
    }

    #[tokio::test]
    async fn placement_never_hands_over_a_revoked_worker() {
        const WORKER: DeviceKey = [12; 32];
        let query = PlacementQuery {
            workspace_id: 1,
            app: "so.tilo.thumbnails",
            gpu: false,
            input_bytes: 1,
            inputs: &[],
            local_wall_ms: None,
        };
        let trusted = TrustedPlacement {
            inner: Names(WORKER),
            revocations: Arc::new(revoked(&[RELAY])),
        };
        assert_eq!(
            trusted.place(query).await.unwrap().worker,
            Some(WORKER),
            "a worker nobody revoked"
        );
        let refused = TrustedPlacement {
            inner: Names(WORKER),
            revocations: Arc::new(revoked(&[WORKER])),
        };
        let error = refused.place(query).await.unwrap_err();
        assert!(error.contains("not trusted"), "{error}");
        assert!(error.contains(&hex::encode(WORKER)), "{error}");
        let unreadable = TrustedPlacement {
            inner: Names(WORKER),
            revocations: Arc::new(Unreadable),
        };
        let error = unreadable.place(query).await.unwrap_err();
        assert!(error.contains("unreadable"), "{error}");
    }
}
