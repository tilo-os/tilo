use std::sync::atomic::{AtomicUsize, Ordering};

use iroh_base::EndpointAddr;
use tilo_user_identity::{DeviceCert, RevocationList};

use super::*;
use crate::offload_device::DeviceRevocations;

/// A room the session had when it started.
const START_ROOM: &str = "0b5c7a7e-6f43-4a39-9d1c-1d3f2a6c0e11";
/// A room created after the session started.
const LATER_ROOM: &str = "5d1f6c0a-2b7e-4c8f-9a3d-8e2b1c4f6a77";
/// Another room created after the session started.
const THIRD_ROOM: &str = "9e3c2b1a-4d5f-4a6b-8c7d-0e1f2a3b4c5d";
const USER: [u8; 32] = [7; 32];
const LAPTOP: [u8; 32] = [3; 32];
const RELAY: [u8; 32] = [9; 32];
const OTHER_RELAY: [u8; 32] = [11; 32];

fn relay(device: DeviceKey) -> OffloadRelay {
    OffloadRelay {
        address: EndpointAddr::new(iroh_base::SecretKey::from_bytes(&device).public()),
        device,
    }
}

fn workspace(room: &str) -> u64 {
    tilo_relay_protocol::room_workspace_id(room).unwrap()
}

/// The person's device revocations, which they change while the session
/// runs.
#[derive(Default)]
struct Revocations(Mutex<RevocationList>);

impl Revocations {
    fn revoke(&self, device: DeviceKey) {
        let root = ed25519_dalek::SigningKey::from_bytes(&[21; 32]);
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: device,
            revoked_at_ns: 1,
            reason: tilo_user_identity::RevocationReason::Stolen,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
        self.0.lock_recover().insert(entry).unwrap();
    }
}

impl DeviceRevocations for Revocations {
    fn current(&self) -> Result<RevocationList, String> {
        Ok(self.0.lock_recover().clone())
    }
}

/// The device's registry as the person changes it: room → the relays
/// registered as retaining it.
struct Registry {
    rooms: Mutex<HashMap<String, Vec<OffloadRelay>>>,
    revocations: Arc<Revocations>,
    unreadable: Mutex<Option<String>>,
}

impl Registry {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            rooms: Mutex::default(),
            revocations: Arc::default(),
            unreadable: Mutex::default(),
        })
    }

    /// The person registers `relay` as retaining `room`.
    fn register(&self, room: &str, relay: OffloadRelay) {
        self.rooms
            .lock_recover()
            .entry(room.to_owned())
            .or_default()
            .push(relay);
    }

    fn deregister(&self, room: &str) {
        self.rooms.lock_recover().remove(room);
    }

    fn identity(&self) -> OffloadIdentity {
        OffloadIdentity {
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
            revocations: self.revocations.clone(),
        }
    }
}

impl RoomPlacements for Registry {
    fn resolve(&self, room: &str) -> Result<Option<RoomRelays>, String> {
        let unreadable = self.unreadable.lock_recover().clone();
        if let Some(error) = unreadable {
            return Err(error);
        }
        let Some(relays) = self.rooms.lock_recover().get(room).cloned() else {
            return Ok(None);
        };
        Ok(Some(RoomRelays {
            identity: self.identity(),
            workspace_id: workspace(room),
            relays,
        }))
    }
}

/// The routes a session started, by configuration; each "starts" as
/// `outcome` says, without dialling anything.
#[derive(Default)]
struct Started {
    configs: Mutex<Vec<DeviceOffloadConfig>>,
    count: AtomicUsize,
}

impl Started {
    fn starter(self: &Arc<Self>, outcome: &'static str) -> Box<StartRoute> {
        let started = Arc::clone(self);
        let runtime = tokio::runtime::Handle::current();
        Box::new(move |config: DeviceOffloadConfig| {
            started.count.fetch_add(1, Ordering::SeqCst);
            started.configs.lock_recover().push(config.clone());
            DeviceOffload::spawn(
                config.trust(),
                async move { Err(format!("{outcome} {}", hex::encode(config.relay.device))) },
                &runtime,
            )
        })
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    fn last(&self) -> DeviceOffloadConfig {
        self.configs.lock_recover().last().cloned().unwrap()
    }
}

fn request(room: &str) -> OffloadRequest {
    OffloadRequest {
        room: room.into(),
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

async fn run_here(offload: &RoomOffload, room: &str) -> String {
    match offload.offload(request(room)).await {
        Offloaded::RunHere { reason } => reason,
        ran @ Offloaded::Ran { .. } => panic!("nothing here can run elsewhere: {ran:?}"),
    }
}

#[tokio::test]
async fn a_room_created_after_start_gets_a_route_once_its_relay_is_registered() {
    let registry = Registry::new();
    registry.register(START_ROOM, relay(RELAY));
    let started = Arc::new(Started::default());
    let offload = RoomOffload::new(
        registry.clone(),
        started.starter("route to"),
        tokio::runtime::Handle::current(),
    );
    // The session's start rooms get their route at once, as before.
    offload.hold(DeviceOffloadConfig {
        relay: relay(RELAY),
        identity: registry.identity(),
        rooms: HashMap::from([(START_ROOM.to_owned(), workspace(START_ROOM))]),
    });
    assert_eq!(started.count(), 1);

    // A room created later, which no registered relay retains yet: it runs
    // here, the door is told why, and nothing is started.
    let reason = run_here(&offload, LATER_ROOM).await;
    assert_eq!(reason, "no home relay of this device retains the room");
    assert_eq!(started.count(), 1, "no route for an unregistered room");

    // The person registers the relay that retains it: the very next
    // invocation in that room starts its route, for that room's workspace
    // only, and goes through it.
    registry.register(LATER_ROOM, relay(OTHER_RELAY));
    let reason = run_here(&offload, LATER_ROOM).await;
    assert_eq!(started.count(), 2, "the later room's route");
    let config = started.last();
    assert_eq!(config.relay, relay(OTHER_RELAY));
    assert_eq!(
        config.rooms,
        HashMap::from([(LATER_ROOM.to_owned(), workspace(LATER_ROOM))]),
        "only the invoking room, by its shard"
    );
    assert!(
        reason.contains(&format!(
            "did not start: route to {}",
            hex::encode(OTHER_RELAY)
        )),
        "the answer is the later room's route's: {reason}"
    );

    // Both rooms keep their own routes: nothing is started again.
    let reason = run_here(&offload, START_ROOM).await;
    assert!(
        reason.contains(&format!("route to {}", hex::encode(RELAY))),
        "{reason}"
    );
    run_here(&offload, LATER_ROOM).await;
    assert_eq!(started.count(), 2, "held routes are reused");
    assert_eq!(offload.routes(), 2);
}

#[tokio::test]
async fn a_revoked_relay_or_device_gets_no_route_and_a_held_one_is_let_go() {
    let registry = Registry::new();
    registry.register(START_ROOM, relay(RELAY));
    registry.register(LATER_ROOM, relay(RELAY));
    let started = Arc::new(Started::default());
    let offload = RoomOffload::new(
        registry.clone(),
        started.starter("route to"),
        tokio::runtime::Handle::current(),
    );
    run_here(&offload, START_ROOM).await;
    assert_eq!(started.count(), 1);

    // The person revokes the relay: the held route is let go, the room's
    // offloadable actions run here with why, and nothing is dialled for the
    // other room either.
    registry.revocations.revoke(RELAY);
    for room in [START_ROOM, LATER_ROOM] {
        let reason = run_here(&offload, room).await;
        assert!(reason.contains("has been revoked"), "{reason}");
        assert!(reason.contains(&hex::encode(RELAY)), "{reason}");
    }
    assert_eq!(started.count(), 1, "no route to a revoked relay");
    assert_eq!(offload.routes(), 0, "the revoked relay's route is let go");

    // Another relay retaining the room is not revoked: the room moves there.
    registry.register(START_ROOM, relay(OTHER_RELAY));
    run_here(&offload, START_ROOM).await;
    assert_eq!(started.count(), 2);
    assert_eq!(started.last().relay, relay(OTHER_RELAY));

    // This device revoked: no relay is usable, in any room.
    registry.register(THIRD_ROOM, relay(OTHER_RELAY));
    registry.revocations.revoke(LAPTOP);
    let reason = run_here(&offload, THIRD_ROOM).await;
    assert_eq!(reason, "this device has been revoked");
    let reason = run_here(&offload, START_ROOM).await;
    assert!(reason.contains("has been revoked"), "{reason}");
    assert_eq!(started.count(), 2);
    assert_eq!(offload.routes(), 0, "every route is let go");
}

#[tokio::test]
async fn a_deregistered_room_or_an_unreadable_registry_runs_here_with_why() {
    let registry = Registry::new();
    registry.register(START_ROOM, relay(RELAY));
    let started = Arc::new(Started::default());
    let offload = RoomOffload::new(
        registry.clone(),
        started.starter("route to"),
        tokio::runtime::Handle::current(),
    );
    run_here(&offload, START_ROOM).await;
    assert_eq!(offload.routes(), 1);

    registry.deregister(START_ROOM);
    let reason = run_here(&offload, START_ROOM).await;
    assert_eq!(reason, "no home relay of this device retains the room");
    assert_eq!(offload.routes(), 0, "a relay retaining none of its rooms");

    *registry.unreadable.lock_recover() = Some("the registry is invalid".into());
    let reason = run_here(&offload, START_ROOM).await;
    assert_eq!(reason, "the registry is invalid");
    assert_eq!(started.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_route_that_did_not_start_is_retried_after_a_while() {
    let registry = Registry::new();
    registry.register(START_ROOM, relay(RELAY));
    let started = Arc::new(Started::default());
    let offload = RoomOffload::new(
        registry.clone(),
        started.starter("unreachable"),
        tokio::runtime::Handle::current(),
    );
    let reason = run_here(&offload, START_ROOM).await;
    assert!(reason.contains("did not start: unreachable"), "{reason}");
    // Within the back-off the failed route answers, with why.
    run_here(&offload, START_ROOM).await;
    assert_eq!(started.count(), 1);
    tokio::time::advance(RETRY_AFTER).await;
    run_here(&offload, START_ROOM).await;
    assert_eq!(started.count(), 2, "tried again");
    assert_eq!(offload.routes(), 1, "the failed route is replaced");
}
