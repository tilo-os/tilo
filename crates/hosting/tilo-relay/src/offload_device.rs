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

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use fs::document_media::DocumentMedia;
use fs::store::ContentStore;
use iroh_base::EndpointAddr;
use tilo_actions::offload::{ActionOffload, OffloadFuture, OffloadRequest, Offloaded};
use tilo_relay_protocol::HomePeerRegistration;
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::RelayClient;
use crate::compute::{ComputePackageCatalogue, InstalledComputeCatalogue};
use crate::compute_session::ComputeClient;
use crate::mesh::DeviceKey;
use crate::offload::{Offloader, OffloaderParts, WorkspaceContent};
use crate::offload_placement::RelayPlacement;
use crate::offload_route::OffloadRoute;
use crate::transport::Envelope;

/// How long the route has to connect to the relay and attach its rooms'
/// workspaces before offloadable actions run here for this session.
pub const START_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// The revocations this device knows of.
    pub revocations: RevocationList,
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
/// What failed: no room placed, the relay unreachable or refusing this
/// device, a workspace it does not let this device compute in, or no answer
/// within [`START_TIMEOUT`].
pub async fn start_route(
    config: DeviceOffloadConfig,
    endpoint: iroh::Endpoint,
    store: Arc<ContentStore>,
    packages: Arc<dyn ComputePackageCatalogue>,
) -> Result<OffloadRoute, String> {
    if config.rooms.is_empty() {
        return Err(NO_ROOM.into());
    }
    tokio::time::timeout(START_TIMEOUT, connect(config, endpoint, store, packages))
        .await
        .map_err(|_| format!("the relay did not answer within {START_TIMEOUT:?}"))?
}

/// Why a device with no room on its relay has no route.
const NO_ROOM: &str = "no room is placed on the relay";

async fn connect(
    config: DeviceOffloadConfig,
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
        identity.revocations.clone(),
    )
    .await
    .map_err(|error| format!("the relay session failed: {error}"))?;
    subscribe(&placement, &workspaces).await?;
    let compute = ComputeClient::connect(
        endpoint,
        relay.address,
        identity.user_public_key,
        Some(relay.device),
        identity.device_cert,
        identity.device_signing_key,
        identity.revocations,
    )
    .await
    .map_err(|error| format!("the compute session failed: {error}"))?;
    let offloader = Offloader::start(OffloaderParts {
        compute,
        workspaces,
        content: WorkspaceContent::new(Arc::clone(&store), tokio::runtime::Handle::current()),
        placement: Arc::new(RelayPlacement::new(placement, relay.device)),
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
/// the background, and until it is up — or for good when it cannot start —
/// answering [`Offloaded::RunHere`] with why.
#[derive(Default)]
pub struct DeviceOffload {
    route: OnceLock<Result<OffloadRoute, String>>,
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
    /// at once. A failed start is logged once.
    #[must_use]
    pub fn spawn(
        start: impl Future<Output = Result<OffloadRoute, String>> + Send + 'static,
        runtime: &tokio::runtime::Handle,
    ) -> Arc<Self> {
        let offload = Arc::new(Self::default());
        let weak: Weak<Self> = Arc::downgrade(&offload);
        runtime.spawn(async move {
            let started = start.await;
            match &started {
                Ok(_) => tracing::info!("offloadable actions are routed through the home relay"),
                Err(error) => tracing::warn!(
                    %error,
                    "offloadable actions run on this device: the offload route did not start"
                ),
            }
            if let Some(offload) = weak.upgrade() {
                let _ = offload.route.set(started);
            }
        });
        offload
    }

    /// Whether the route is up: `None` while it starts, then whether it
    /// started, and why not.
    #[must_use]
    pub fn started(&self) -> Option<Result<(), &str>> {
        self.route
            .get()
            .map(|route| route.as_ref().map(|_| ()).map_err(String::as_str))
    }
}

impl ActionOffload for DeviceOffload {
    fn offload(&self, request: OffloadRequest) -> OffloadFuture<'_> {
        let reason = match self.route.get() {
            Some(Ok(route)) => return route.offload(request),
            Some(Err(error)) => format!("the offload route did not start: {error}"),
            None => "the offload route is still starting".to_owned(),
        };
        Box::pin(async move { Offloaded::RunHere { reason } })
    }

    fn ran_here(&self, request: &OffloadRequest, elapsed: Duration) {
        if let Some(Ok(route)) = self.route.get() {
            route.ran_here(request, elapsed);
        }
    }
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

    #[tokio::test]
    async fn the_route_runs_everything_here_until_it_starts_and_when_it_cannot() {
        let config = DeviceOffloadConfig {
            relay: OffloadRelay {
                address: address(10),
                device: RELAY,
            },
            identity: OffloadIdentity {
                user_public_key: USER,
                device_cert: DeviceCert {
                    user_public_key: USER,
                    device_public_key: [3; 32],
                    device_label: "laptop".into(),
                    capabilities: tilo_user_identity::DeviceCapabilities::primary(),
                    issued_at_ns: 0,
                    expires_at_ns: None,
                    signature: [0; 64],
                },
                device_signing_key: ed25519_dalek::SigningKey::from_bytes(&[3; 32]),
                revocations: RevocationList::new(),
            },
            // Nothing placed: the start fails before dialling anything.
            rooms: HashMap::new(),
        };
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .bind()
            .await
            .unwrap();
        let offload = DeviceOffload::spawn(
            start_route(
                config,
                endpoint,
                Arc::new(ContentStore::memory()),
                Arc::new(InstalledComputeCatalogue),
            ),
            &tokio::runtime::Handle::current(),
        );
        let request = OffloadRequest {
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
        };
        if offload.started().is_none() {
            let Offloaded::RunHere { reason } = offload.offload(request.clone()).await else {
                panic!("nothing runs elsewhere before the route is up");
            };
            assert!(reason.contains("still starting"), "{reason}");
        }
        tilo_test_env::within("the failed start", async {
            while offload.started().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert_eq!(
            offload.started(),
            Some(Err("no room is placed on the relay"))
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
}
