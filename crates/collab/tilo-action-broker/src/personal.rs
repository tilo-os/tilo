//! The person's own store, open to their other devices while this host runs.
//!
//! The store holds the records of the person's own scope — `(app, personal)`
//! — once per app and record, on the cluster their identity root derives.
//! A door is what carries them: it answers the store's link on the host's own
//! endpoint, dials the siblings the store knows of, hands each write a sibling
//! admits to the apps that have the record open, on the same route those apps
//! publish on, and keeps the store beside the host's state.
//!
//! Nothing here joins anything. The link proves which device is on the other
//! end — the endpoint's id is its device key — and the store admits only
//! certificates the person's identity root signed. The devices it dials are
//! the person's own as the identity's device fleet lists them
//! ([`tilo_identity_host::device_fleet`]): the same certificates and
//! revocations every other device check uses. No roster, invite, role or host
//! takes part, and no workspace exists in the flow.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use tilo_action_policy::{Standing, WorkspacePolicy};
use tilo_peer_state::{PersonalStore, PersonalSync};
use tilo_user_identity::DeviceCert;

use crate::collab_app_broker::principal_id_for_root;
use crate::collab_broker::FanoutState;
use crate::collab_core::BrokerChannels;
use crate::collab_inbound::{self, RemoteDocument};

/// How often the person's devices are read again from the identity's device
/// fleet, and a sibling without a link is dialled: a device is offline as
/// often as it is on, and a revocation is written by another device.
const REDIAL: Duration = Duration::from_secs(300);

/// How long the store waits after its last change before it is written
/// again: a record an app edits arrives one op at a time.
const SAVE_AFTER: Duration = Duration::from_secs(1);

/// Where a host keeping its state in `state` keeps the person's own
/// documents.
#[must_use]
pub fn store_path(state: &Path) -> PathBuf {
    state.join("tilo").join("personal").join("documents")
}

/// The person whose store this is, as the identity files name them.
#[derive(Clone, Debug)]
pub struct Person {
    /// The identity of theirs a host acts as, by which its device fleet is
    /// kept.
    pub identity_id: String,
    /// Their identity root, which their store and their own cluster derive
    /// from.
    pub root: [u8; 32],
    /// The principal their records' standing is decided for.
    pub principal: String,
}

impl Person {
    /// The person whose identity is filed as `identity_id` and whose root is
    /// `root`.
    #[must_use]
    pub fn of(identity_id: String, root: [u8; 32]) -> Self {
        Self {
            principal: principal_id_for_root(root),
            identity_id,
            root,
        }
    }
}

/// The person's own store on this device, opened where `state` keeps it: a
/// store an earlier start wrote is read back, and the first start opens it
/// empty on the person's root, held by `device`.
///
/// Hand it to [`PersonalSync::over`] with the device's signing key, which is
/// also the key its endpoint runs on.
///
/// # Errors
///
/// The store kept there cannot be read, or `device` is not a certificate the
/// person's root signed that still holds.
pub fn open(
    state: &Path,
    root: [u8; 32],
    device: DeviceCert,
    now: SystemTime,
) -> Result<PersonalStore, String> {
    let path = store_path(state);
    match std::fs::read(&path) {
        // A machine copied from another re-keyed its device: the store kept
        // here is the device it was copied from, which still holds it. It is
        // set aside, never opened as this device's, and the person's records
        // come back from their other devices as to any new one of theirs.
        Ok(bytes)
            if PersonalStore::kept_by(&bytes)
                .is_some_and(|kept| kept != device.device_public_key) =>
        {
            let kept = PersonalStore::kept_by(&bytes).unwrap_or_default();
            let aside = path.with_extension(format!("of-{}", hex::encode(&kept[..4])));
            std::fs::rename(&path, &aside)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            PersonalStore::open(root, device, now)
        }
        Ok(bytes) => PersonalStore::restore(&bytes, &device, now),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            PersonalStore::open(root, device, now)
        }
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// A host's door to the person's other devices: the sibling links kept, what
/// a sibling writes handed to the host's apps, and the store written back
/// beside the host's state as it changes.
///
/// The store's link itself is answered by the host, on the endpoint it runs
/// on: `sync` is handed to the accept loop that endpoint already has, beside
/// whatever else it serves. Dropping the door stops the links with its tasks;
/// the store itself stays where the host keeps it.
pub struct Door {
    siblings: tokio::task::JoinHandle<()>,
    delivery: tokio::task::JoinHandle<()>,
    keeping: tokio::task::JoinHandle<()>,
}

impl Drop for Door {
    fn drop(&mut self) {
        self.siblings.abort();
        self.delivery.abort();
        self.keeping.abort();
    }
}

impl Door {
    /// Open the person's store to their other devices from `endpoint`, on
    /// `runtime` for as long as the door: dial each of the person's devices
    /// this one has no link to, hand every write one of them admits to the
    /// apps that have the record open through `channels` and `fanout`, and
    /// keep the store under `state` as it changes.
    ///
    /// `sync` must be answered by the host's accept loop on `endpoint`
    /// under [`tilo_peer_state::PERSONAL_ALPN`]: it is what a sibling dials.
    pub fn serve(
        runtime: &tokio::runtime::Handle,
        endpoint: iroh::Endpoint,
        sync: PersonalSync,
        person: &Person,
        state: &Path,
        channels: Arc<Mutex<BrokerChannels>>,
        fanout: Arc<FanoutState>,
    ) -> Self {
        let principal = person.principal.clone();
        Self {
            siblings: runtime.spawn(keep_links(endpoint, sync.clone(), person.clone())),
            delivery: runtime.spawn(deliver(sync.clone(), principal, channels, fanout)),
            keeping: runtime.spawn(keep_kept(sync, store_path(state))),
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
        let siblings: Vec<[u8; 32]> = admit_fleet(&sync, &person)
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
                    // ignored-failure: a sibling that is off, or revoked, is
                    // dialled again at the next turn, and the store catches it
                    // up whole when it is reachable again
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

/// Take the person's device fleet into `sync` and answer the devices it holds
/// afterwards: this device and every other one of the person's whose
/// certificate their fleet carries and has not revoked. The certificates it
/// admits and the revocations it holds are the fleet's, so a device enrolled
/// or revoked elsewhere reaches the store on the next call.
pub fn admit_fleet(sync: &PersonalSync, person: &Person) -> Vec<[u8; 32]> {
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
            // A host too slow to keep up loses frames rather than its link:
            // the record catches up from the store whole.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "personal writes were missed and catch up whole");
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };
        let cluster = sync.read(PersonalStore::store_key);
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

/// Write the store back to `path` once what it holds has settled: the
/// person's records are kept beside the host's state, so the next start has
/// them without a sibling in reach.
async fn keep_kept(sync: PersonalSync, path: PathBuf) {
    let mut changes = sync.changes();
    loop {
        if changes.changed().await.is_err() {
            return;
        }
        // The ops of one edit land together, one after another: the store is
        // written once they have.
        tokio::time::sleep(SAVE_AFTER).await;
        let written = sync.store().durable().and_then(|bytes| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("{}: {error}", parent.display()))?;
            }
            tilo_files::native::atomic::write_with_mode(&path, &bytes, 0o600)
                .map_err(|error| format!("{}: {error}", path.display()))
        });
        if let Err(error) = written {
            tracing::warn!(%error, "the person's own store was not written");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device key `n` certified by `root`, as this identity's provisioning
    /// signs one.
    fn certificate(root: &ed25519_dalek::SigningKey, n: u8, now: SystemTime) -> DeviceCert {
        let key = ed25519_dalek::SigningKey::from_bytes(&[n; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: format!("device-{n}"),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: now
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |since| {
                    u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
                }),
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    /// The store's path is the host's state, its own documents beside it.
    #[test]
    fn the_store_lives_beside_the_hosts_state() {
        assert_eq!(
            store_path(Path::new("/state")),
            Path::new("/state/tilo/personal/documents")
        );
    }

    /// Two devices of one person agree on the cluster their own records route
    /// on, without either naming the other: the store's key is the root's.
    #[test]
    fn two_devices_of_one_person_agree_on_the_cluster() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let root_key = root.verifying_key().to_bytes();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let first = certificate(&root, 1, now);
        let second = certificate(&root, 2, now);
        let one = PersonalStore::open(root_key, first, now).unwrap();
        let two = PersonalStore::open(root_key, second, now).unwrap();
        assert_eq!(one.store_key(), two.store_key());
        assert_eq!(
            one.store_key(),
            collab_types::personal_cluster(&root_key).to_string()
        );
        assert_eq!(
            Person::of(hex::encode(root_key), root_key).principal,
            principal_id_for_root(root_key)
        );
    }

    /// A store written out comes back whole, on the device that wrote it: the
    /// person's records outlive the host that held them.
    #[test]
    fn a_written_store_comes_back() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let root_key = root.verifying_key().to_bytes();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let device = certificate(&root, 1, now);
        let store = PersonalStore::open(root_key, device.clone(), now).unwrap();
        let bytes = store.durable().unwrap();
        let back = PersonalStore::restore(&bytes, &device, now).unwrap();
        assert_eq!(back.root(), root_key);
        assert_eq!(back.store_key(), store.store_key());
    }

    /// A machine copied from another opens the person's store as its own new
    /// device: the store the source kept is set aside, never opened as this
    /// device's, and is still there for the source's records.
    #[test]
    fn a_copied_machine_opens_the_store_as_its_own_device() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
        let root_key = root.verifying_key().to_bytes();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let source = certificate(&root, 1, now);
        let branch = certificate(&root, 2, now);
        let state = tempfile::tempdir().unwrap();
        let path = store_path(state.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let kept = PersonalStore::open(root_key, source.clone(), now)
            .unwrap()
            .durable()
            .unwrap();
        std::fs::write(&path, &kept).unwrap();
        assert_eq!(
            PersonalStore::kept_by(&kept),
            Some(source.device_public_key)
        );

        let opened = open(state.path(), root_key, branch.clone(), now).unwrap();
        assert_eq!(opened.root(), root_key);
        assert!(!path.exists(), "the source's store is not this device's");
        let aside = path.with_extension(format!(
            "of-{}",
            hex::encode(&source.device_public_key[..4])
        ));
        assert_eq!(std::fs::read(aside).unwrap(), kept);
        // The source's own store still opens on the source.
        assert!(PersonalStore::restore(&kept, &source, now).is_ok());
        assert!(PersonalStore::restore(&kept, &branch, now).is_err());
    }
}
