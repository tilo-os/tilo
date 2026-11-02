//! The person's own store, open to their other devices while this runtime
//! runs.
//!
//! The store holds the records of the person's own scope — `(app, personal)`
//! — once per app and record, on the cluster their identity root derives.
//! This door is what carries them: it answers the store's link on the
//! runtime's own endpoint, dials the siblings the store knows of, and hands
//! each write a sibling admits to the apps that have the record open, on the
//! same route those apps publish on.
//!
//! Nothing here joins anything. The link proves which device is on the other
//! end — the endpoint's id is its device key — and the store admits only
//! certificates the person's identity root signed. The devices it dials are
//! the person's own as the identity's device fleet lists them
//! ([`tilo_identity_host::device_fleet`]): the same certificates and
//! revocations every other device check uses. No roster, invite, role or host
//! takes part, and no workspace exists in the flow.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_action_broker::collab_broker::FanoutState;
use tilo_action_broker::collab_core::BrokerChannels;
use tilo_action_broker::collab_inbound::{self, RemoteDocument};
use tilo_action_policy::{Standing, WorkspacePolicy};
use tilo_identity_host::registry::IdentityProfile;
use tilo_peer_state::{PERSONAL_ALPN, PeerPersonal, PersonalSync};
use tilo_store_link::StoreService;

/// How often the person's devices are read again from the identity's device
/// fleet, and a sibling without a link is dialled: a device is offline as
/// often as it is on, and a revocation is written by another device.
const REDIAL: Duration = Duration::from_secs(300);

/// A runtime's door to the person's other devices, and to the members of the
/// workspaces this device keeps. Dropping it stops the door: the links end
/// with the tasks, and the endpoint it answers on is the runtime's.
pub(crate) struct PersonalDoor {
    _router: iroh::protocol::Router,
    siblings: tokio::task::JoinHandle<()>,
    delivery: tokio::task::JoinHandle<()>,
    /// The workspaces' store, while this device keeps one: its handler is in
    /// the same router, and it lives as long as the door does.
    _workspaces: Option<StoreService>,
}

impl Drop for PersonalDoor {
    fn drop(&mut self) {
        self.siblings.abort();
        self.delivery.abort();
    }
}

/// The person whose store this is, as the identity files name them.
struct Person {
    /// The identity of theirs the profile acts as, by which its device fleet
    /// is kept.
    identity_id: String,
    /// Their identity root, which their store and their own cluster derive
    /// from.
    root: [u8; 32],
    /// The principal their records' standing is decided for.
    principal: String,
}

impl PersonalDoor {
    /// Serve the person's store on `endpoint`, opened for the identity
    /// `identity` this profile acts as: returns the store the broker keeps
    /// their own records in and the door that carries them. `workspaces` is
    /// the store this device keeps for the workspaces it holds, served on the
    /// same endpoint and admitted by the same membership, when it keeps one.
    pub(crate) fn serve(
        endpoint: iroh::Endpoint,
        store: (PeerPersonal, ed25519_dalek::SigningKey),
        identity: &IdentityProfile,
        principal: &str,
        channels: Arc<Mutex<BrokerChannels>>,
        fanout: Arc<FanoutState>,
        workspaces: Option<StoreService>,
    ) -> (PersonalSync, Self) {
        let sync = PersonalSync::over(store.0, store.1);
        let door = Self::open(
            endpoint,
            sync.clone(),
            Person {
                identity_id: identity.id.clone(),
                root: identity.user_public_key,
                principal: principal.to_owned(),
            },
            channels,
            fanout,
            workspaces,
        );
        (sync, door)
    }

    /// Answer `sync`'s link on `endpoint`, keep the store told of the
    /// person's devices and dial the siblings among them, and hand what a
    /// sibling writes to `channels` and `fanout`, which local apps read. The
    /// workspaces this device keeps answer on the same router, from the same
    /// accept loop, so an endpoint never answers one of its protocols and
    /// drops another's.
    fn open(
        endpoint: iroh::Endpoint,
        sync: PersonalSync,
        person: Person,
        channels: Arc<Mutex<BrokerChannels>>,
        fanout: Arc<FanoutState>,
        workspaces: Option<StoreService>,
    ) -> Self {
        let runtime = tilo_rt::app().handle();
        let router = {
            // The router answers on the runtime's own async executor, as the
            // runtime's other long-lived work does.
            let _entered = runtime.enter();
            let mut router = iroh::protocol::Router::builder(endpoint.clone())
                .accept(PERSONAL_ALPN, sync.clone());
            if let Some(workspaces) = &workspaces {
                router = router.accept(tilo_store_link::ALPN, workspaces.handler());
            }
            router.spawn()
        };
        let principal = person.principal.clone();
        Self {
            _router: router,
            siblings: runtime.spawn(keep_links(endpoint, sync.clone(), person)),
            delivery: runtime.spawn(deliver(sync, principal, channels, fanout)),
            _workspaces: workspaces,
        }
    }
}

/// Keep the store told of the person's devices, and dial each of them this
/// device has no link to: at once on start, whenever the store changed, and
/// again after [`REDIAL`].
async fn keep_links(endpoint: iroh::Endpoint, sync: PersonalSync, person: Person) {
    let mut changes = sync.changes();
    let dialing: Arc<Mutex<HashSet<[u8; 32]>>> = Arc::default();
    loop {
        let ours = *endpoint.id().as_bytes();
        let siblings: Vec<[u8; 32]> = devices(&sync, &person)
            .into_iter()
            .filter(|device| *device != ours)
            .collect();
        for device in siblings {
            let dialing = Arc::clone(&dialing);
            if !dialing.lock().is_ok_and(|mut held| held.insert(device)) {
                continue;
            }
            let Ok(id) = iroh::EndpointId::from_bytes(&device) else {
                tracing::warn!(device = %hex::encode(device), "a sibling has no endpoint id");
                continue;
            };
            let (endpoint, sync) = (endpoint.clone(), sync.clone());
            tokio::spawn(async move {
                let addr = iroh::EndpointAddr::new(id);
                // A device that could not be reached is dialled again on the
                // next turn; the store catches it up when it is.
                if let Err(error) = sync.connect(&endpoint, addr).await {
                    tracing::debug!(%error, device = %hex::encode(device), "a personal link ended");
                }
                if let Ok(mut dialing) = dialing.lock() {
                    dialing.remove(&device);
                }
            });
        }
        tokio::select! {
            moved = changes.changed() => {
                if moved.is_err() {
                    return;
                }
            }
            () = tokio::time::sleep(REDIAL) => {}
        }
    }
}

/// The person's devices this identity's fleet certifies now, once the store
/// has been told of them: the certificates it admits and the revocations it
/// holds are the fleet's, so a device enrolled or revoked elsewhere reaches
/// the store on the next turn.
fn devices(sync: &PersonalSync, person: &Person) -> Vec<[u8; 32]> {
    let fleet = tilo_identity_host::device_fleet::DeviceFleetStore::from_env()
        .and_then(|fleets| fleets.load(&person.identity_id));
    let Ok(fleet) = fleet else {
        tracing::debug!("the identity's device fleet could not be read");
        return sync.read(|store| {
            store
                .devices()
                .into_iter()
                .map(|device| device.device_public_key)
                .collect()
        });
    };
    let admitted = sync.admit(|store, now| {
        let mut changed = false;
        for certificate in &fleet.certificates {
            // A store node and a device of another identity are none of the
            // store's, and the fleet may list both.
            changed |= store.admit_device(certificate, now).unwrap_or(false);
        }
        for entry in &fleet.revocations {
            changed |= store.revoke(entry.clone()).unwrap_or(false);
        }
        Ok(changed)
    });
    if let Err(error) = admitted {
        tracing::warn!(%error, "the person's devices were not taken");
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
        });
    fleet.personal_devices(&person.root, now)
}

/// Hand every write a sibling admits to the apps that have the record open,
/// as the broker hands them any device's write: on the record's route, under
/// the authority of the person the store belongs to.
async fn deliver(
    sync: PersonalSync,
    person: String,
    channels: Arc<Mutex<BrokerChannels>>,
    fanout: Arc<FanoutState>,
) {
    let mut frames = sync.admitted();
    loop {
        let frame = match frames.recv().await {
            Ok(frame) => frame,
            // A runtime too slow to keep up loses frames rather than its
            // link: the record catches up from the store whole.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "personal writes were missed and catch up whole");
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };
        let cluster = sync.read(tilo_peer_state::PersonalStore::store_key);
        // Who wrote it was decided by the link: the store admits no write
        // whose certificate the person's root did not sign. What it changes,
        // it changes as theirs.
        let author = Standing::person(WorkspacePolicy::new(&cluster, &person), person.clone());
        let delivered = collab_inbound::deliver(
            &channels,
            &fanout,
            RemoteDocument {
                app_id: frame.app,
                cluster_id: cluster,
                record_id: frame.record,
                author,
                replayed: false,
                envelope: frame.envelope,
            },
            |_| person.clone(),
        );
        if delivered != collab_inbound::Delivery::Done {
            tracing::debug!(?delivered, "a write of the person's own was not taken");
        }
    }
}

#[cfg(test)]
mod tests {
    /// A sibling is addressed by its device key alone: the endpoint's id is
    /// the device's key, so the store's certificates are all a dial needs.
    #[test]
    fn a_sibling_is_addressed_by_its_device_key() {
        let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let endpoint = iroh::SecretKey::from_bytes(&device.to_bytes());
        assert_eq!(
            iroh::EndpointId::from_bytes(&device.verifying_key().to_bytes()).unwrap(),
            endpoint.public(),
            "the endpoint's id is the device key its certificate names"
        );
    }
}
