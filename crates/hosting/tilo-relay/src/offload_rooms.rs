//! A session's offload route for every room it has, however late the room
//! arrived: the action door's [`ActionOffload`] that resolves, for each
//! invocation, which of the person's home relays retains the invoking room
//! now, and routes it through a [`DeviceOffload`] to that relay.
//!
//! A session's rooms are not fixed: it creates and joins rooms while it
//! runs, and the person registers relays and revokes devices meanwhile. A
//! route chosen once, at start, would run every offloadable action of a
//! later room here until the next session. [`RoomOffload`] instead asks
//! [`RoomPlacements`] — the device's home-peer registry and identity, read
//! on the runtime's blocking pool, never on the caller's thread — where the
//! invoking room is placed, and:
//!
//! - reuses a route it holds whose relay still retains the room and which
//!   both ends' trust still allows;
//! - otherwise starts one to the first relay that retains the room and is
//!   not revoked, waiting at most [`START_WAIT`] for it before the door runs
//!   the app's handler;
//! - answers [`Offloaded::RunHere`], with why, when no registered relay
//!   retains the room, when every one is revoked (nothing is dialled), when
//!   the device's registry or identity cannot be read, and whenever the
//!   route itself cannot place the run.
//!
//! A route that did not start is retried after [`RETRY_AFTER`]; a held route
//! whose relay retains none of its rooms any more is let go. What a route
//! reads and keeps is unchanged: only the invoking room's media through the
//! invoking app's scope ([`crate::offload::RoomMedia`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::offload::{ActionOffload, OffloadFuture, OffloadRequest, Offloaded};
use tilo_lock::MutexRecover as _;
use tokio::time::Instant;

use crate::mesh::DeviceKey;
use crate::offload_device::{
    DeviceOffload, DeviceOffloadConfig, OffloadIdentity, OffloadRelay, RouteTrust,
};

/// How long an invocation waits for a route that is still starting before
/// the door runs the app's handler instead.
pub const START_WAIT: Duration = Duration::from_secs(5);

/// How long a route that did not start, or was torn down, keeps answering
/// with why before the next invocation tries again.
pub const RETRY_AFTER: Duration = Duration::from_secs(30);

/// Where one room's compute may be placed, as the device reads it now.
#[derive(Clone)]
pub struct RoomRelays {
    /// This device, as it authenticates in the room.
    pub identity: OffloadIdentity,
    /// The room's relay workspace: where its compute jobs are placed.
    pub workspace_id: u64,
    /// Every registered home relay of the person that retains the room, in
    /// registry order.
    pub relays: Vec<OffloadRelay>,
}

/// The device's home-peer registry and identity, read for one room at a
/// time. Called on a blocking pool: it may read files.
pub trait RoomPlacements: Send + Sync {
    /// Where `room` is placed now; `Ok(None)` when no registered relay of
    /// this device retains it.
    ///
    /// # Errors
    ///
    /// Why it cannot be known: the registry or the device's identity for
    /// the room unreadable or untrusted.
    fn resolve(&self, room: &str) -> Result<Option<RoomRelays>, String>;
}

/// Starts a route from its configuration and returns it at once — in a
/// session, [`DeviceOffload::start`] over the session's own media.
pub type StartRoute = dyn Fn(DeviceOffloadConfig) -> Arc<DeviceOffload> + Send + Sync;

/// A route [`RoomOffload`] holds.
struct HeldRoute {
    relay: OffloadRelay,
    /// The device key this device authenticates to the relay with.
    device: DeviceKey,
    /// The rooms it routes; a room is let go once its relay no longer
    /// retains it.
    rooms: HashMap<String, u64>,
    route: Arc<DeviceOffload>,
    started_at: Instant,
}

impl HeldRoute {
    /// Whether it did not start, or was torn down, longer than
    /// [`RETRY_AFTER`] ago.
    fn stale(&self) -> bool {
        matches!(self.route.started(), Some(Err(_))) && self.started_at.elapsed() >= RETRY_AFTER
    }
}

/// A room as resolved for one invocation: its placements and whether this
/// device may use each relay.
struct Resolved {
    room: RoomRelays,
    /// For each of `room.relays`, `Ok` or why it may not be used.
    trust: Vec<Result<(), String>>,
}

/// Every room's offload route, resolved per invocation.
pub struct RoomOffload {
    placements: Arc<dyn RoomPlacements>,
    start: Box<StartRoute>,
    runtime: tokio::runtime::Handle,
    routes: Mutex<Vec<HeldRoute>>,
}

impl RoomOffload {
    /// Route every room's offloadable invocations as `placements` places
    /// them, starting routes with `start`; `runtime` reads `placements` on
    /// its blocking pool.
    #[must_use]
    pub fn new(
        placements: Arc<dyn RoomPlacements>,
        start: Box<StartRoute>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            placements,
            start,
            runtime,
            routes: Mutex::default(),
        }
    }

    /// Start the route for `config` now, ahead of any invocation — the
    /// session's rooms at start — and hold it for its rooms. Never blocks.
    pub fn hold(&self, config: DeviceOffloadConfig) {
        let held = HeldRoute {
            relay: config.relay.clone(),
            device: config.identity.device_cert.device_public_key,
            rooms: config.rooms.clone(),
            route: (self.start)(config),
            started_at: Instant::now(),
        };
        self.routes.lock_recover().push(held);
    }

    /// How many routes it holds.
    #[must_use]
    pub fn routes(&self) -> usize {
        self.routes.lock_recover().len()
    }

    /// Where `room` is placed now, and whom the device may use there: read
    /// on the blocking pool.
    async fn resolve(&self, room: &str) -> Result<Option<Resolved>, String> {
        let placements = Arc::clone(&self.placements);
        let room = room.to_owned();
        self.runtime
            .spawn_blocking(move || {
                let Some(room) = placements.resolve(&room)? else {
                    return Ok(None);
                };
                let trust = room
                    .relays
                    .iter()
                    .map(|relay| {
                        RouteTrust {
                            relay: relay.device,
                            device: room.identity.device_cert.device_public_key,
                            revocations: Arc::clone(&room.identity.revocations),
                        }
                        .check()
                        .map(drop)
                    })
                    .collect();
                Ok(Some(Resolved { room, trust }))
            })
            .await
            .map_err(|error| format!("the room's relays could not be read: {error}"))?
    }

    /// The route for `room`, or why its offloadable actions run here.
    async fn route_for(&self, room: &str) -> Result<Arc<DeviceOffload>, String> {
        let resolved = self.resolve(room).await;
        let mut routes = self.routes.lock_recover();
        let Some(Resolved {
            room: placed,
            trust,
        }) = resolved?
        else {
            // Whichever relay used to retain it no longer does.
            let_go(&mut routes, room, |_| false);
            return Err("no home relay of this device retains the room".to_owned());
        };
        let device = placed.identity.device_cert.device_public_key;
        let usable = |relay: &OffloadRelay| {
            placed
                .relays
                .iter()
                .zip(&trust)
                .any(|(placed, trusted)| placed == relay && trusted.is_ok())
        };
        // A held route keeps a room only while its relay retains it, this
        // device may use it, and it authenticates as this device.
        let_go(&mut routes, room, |held| {
            held.device == device && usable(&held.relay) && !held.stale()
        });
        if let Some(held) = routes.iter().find(|held| held.rooms.contains_key(room)) {
            return Ok(Arc::clone(&held.route));
        }
        let Some((relay, _)) = placed.relays.iter().zip(&trust).find(|(_, t)| t.is_ok()) else {
            // Every relay that retains the room is revoked, or this device
            // is: nothing is dialled.
            let why = trust.into_iter().find_map(Result::err).unwrap_or_default();
            return Err(why);
        };
        let relay = relay.clone();
        let config = DeviceOffloadConfig {
            relay: relay.clone(),
            identity: placed.identity,
            rooms: HashMap::from([(room.to_owned(), placed.workspace_id)]),
        };
        let route = (self.start)(config);
        routes.push(HeldRoute {
            relay,
            device,
            rooms: HashMap::from([(room.to_owned(), placed.workspace_id)]),
            route: Arc::clone(&route),
            started_at: Instant::now(),
        });
        drop(routes);
        Ok(route)
    }
}

/// Take `room` from every held route for which `keep` is false, and let go
/// of the routes left with no room.
fn let_go(routes: &mut Vec<HeldRoute>, room: &str, keep: impl Fn(&HeldRoute) -> bool) {
    for held in routes.iter_mut() {
        if held.rooms.contains_key(room) && !keep(held) {
            held.rooms.remove(room);
        }
    }
    routes.retain(|held| !held.rooms.is_empty());
}

impl ActionOffload for RoomOffload {
    fn offload(&self, request: OffloadRequest) -> OffloadFuture<'_> {
        Box::pin(async move {
            let route = match self.route_for(&request.room).await {
                Ok(route) => route,
                Err(reason) => {
                    tracing::info!(
                        app = %request.offload.app,
                        entrypoint = %request.offload.entrypoint,
                        %reason,
                        "offloadable action runs here"
                    );
                    return Offloaded::RunHere { reason };
                }
            };
            let _ = route.wait_started(START_WAIT).await;
            route.offload(request).await
        })
    }

    fn ran_here(&self, request: &OffloadRequest, elapsed: Duration) {
        let route = self
            .routes
            .lock_recover()
            .iter()
            .find(|held| held.rooms.contains_key(&request.room))
            .map(|held| Arc::clone(&held.route));
        if let Some(route) = route {
            route.ran_here(request, elapsed);
        }
    }
}

#[cfg(test)]
#[path = "offload_rooms_tests.rs"]
mod tests;
