//! Every device of a person comes to know the person's other devices,
//! whichever of them it linked to and in whichever order they were enrolled:
//! the certificates travel in the person's own store, between devices that
//! already hold each other's. A revocation still takes a device off every
//! sibling's list, and a device of another person is sent none of it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common::{addr, device, localhost_endpoint};

use ed25519_dalek::SigningKey;
use iroh::Endpoint;
use neighbour_wire::NeighbourMsg;
use std::time::Duration;
use tilo_peer_state::{PERSONAL_ALPN, PersonalStore, PersonalSync};
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationReason};
use web_time::SystemTime;

/// The person's devices as `store`'s device lists them to an agent: the ones
/// it holds a certificate of that are theirs now, as the agent door's
/// directory reads them for `tilo_devices`.
fn listed(store: &PersonalStore) -> Vec<[u8; 32]> {
    let now = SystemTime::now();
    let mut listed: Vec<[u8; 32]> = store
        .devices()
        .into_iter()
        .map(|device| device.device_public_key)
        .filter(|device| store.admits_device(device, now).is_ok())
        .collect();
    listed.sort_unstable();
    listed
}

/// One device: its key, its certificate, its own store, and the endpoint it
/// serves that store on and dials its siblings from.
struct Device {
    cert: DeviceCert,
    sync: PersonalSync,
    endpoint: Endpoint,
    router: iroh::protocol::Router,
    links: Vec<tokio::task::JoinHandle<Result<(), String>>>,
}

impl Device {
    /// The device keyed `n` of the person `root`, holding no certificate but
    /// its own.
    async fn new(root: &SigningKey, n: u8) -> Self {
        let (key, cert) = device(root, n);
        let store =
            PersonalStore::open(cert.user_public_key, cert.clone(), SystemTime::now()).unwrap();
        let sync = PersonalSync::new(store, key.clone());
        let endpoint = localhost_endpoint(&key).await;
        let router = iroh::protocol::Router::builder(endpoint.clone())
            .accept(PERSONAL_ALPN, sync.clone())
            .spawn();
        Self {
            cert,
            sync,
            endpoint,
            router,
            links: Vec::new(),
        }
    }

    fn id(&self) -> [u8; 32] {
        self.cert.device_public_key
    }

    /// Link to `other`, and to no one else.
    fn links_to(&mut self, other: &Self) {
        let (sync, endpoint, to) = (
            self.sync.clone(),
            self.endpoint.clone(),
            addr(&other.endpoint),
        );
        self.links.push(tokio::spawn(
            async move { sync.connect(&endpoint, to).await },
        ));
    }

    fn lists(&self) -> Vec<[u8; 32]> {
        self.sync.read(listed)
    }

    /// Wait until `done` holds of this device's store.
    async fn until(&self, what: &str, done: impl Fn(&PersonalStore) -> bool + Sync) {
        let mut changes = self.sync.changes();
        tokio::time::timeout(Duration::from_secs(30), async {
            while !self.sync.read(&done) {
                changes.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{what}"));
    }

    /// Wait until this device lists every one of `others`.
    async fn until_lists(&self, others: &[&Self]) {
        let wanted: Vec<[u8; 32]> = others.iter().map(|other| other.id()).collect();
        self.until(
            &format!(
                "device {} never lists {:?}",
                self.cert.device_label,
                others
                    .iter()
                    .map(|other| other.cert.device_label.as_str())
                    .collect::<Vec<_>>()
            ),
            |store| {
                let listed = listed(store);
                wanted.iter().all(|device| listed.contains(device))
            },
        )
        .await;
    }

    async fn part(self) {
        for link in self.links {
            link.abort();
            let _ = link.await;
        }
        self.router.shutdown().await.unwrap();
        self.endpoint.close().await;
    }
}

fn ids(devices: &[&Device]) -> Vec<[u8; 32]> {
    let mut ids: Vec<[u8; 32]> = devices.iter().map(|device| device.id()).collect();
    ids.sort_unstable();
    ids
}

fn revocation(root: &SigningKey, device: [u8; 32]) -> RevocationEntry {
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: device,
        revoked_at_ns: 1,
        reason: RevocationReason::Lost,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, root).unwrap();
    entry
}

/// The second and the third device each linked only to the first: the third
/// lists the second and the second lists the third, and no app's listing of
/// the person's records shows how.
#[tokio::test(flavor = "multi_thread")]
async fn two_devices_that_each_met_only_the_first_list_each_other() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let first = Device::new(&root, 2).await;
    let mut second = Device::new(&root, 3).await;
    let mut third = Device::new(&root, 4).await;

    second.links_to(&first);
    first.until_lists(&[&second]).await;
    second.until_lists(&[&first]).await;
    assert_eq!(second.lists(), ids(&[&first, &second]));

    // The third device is enrolled later, and links to the first alone.
    third.links_to(&first);
    third.until_lists(&[&first, &second]).await;
    second.until_lists(&[&first, &third]).await;
    first.until_lists(&[&second, &third]).await;
    let everyone = ids(&[&first, &second, &third]);
    for device in [&first, &second, &third] {
        assert_eq!(device.lists(), everyone);
        assert!(
            device.sync.store().rows().is_empty(),
            "the person's devices are no app's record"
        );
    }

    third.part().await;
    second.part().await;
    first.part().await;
}

/// The third device links to the second instead of the first: it lists the
/// first, which it never met, and the first lists it.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_met_only_the_second_lists_the_first() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let first = Device::new(&root, 2).await;
    let mut second = Device::new(&root, 3).await;
    let mut third = Device::new(&root, 4).await;

    second.links_to(&first);
    first.until_lists(&[&second]).await;
    second.until_lists(&[&first]).await;

    third.links_to(&second);
    third.until_lists(&[&first, &second]).await;
    first.until_lists(&[&second, &third]).await;
    second.until_lists(&[&first, &third]).await;
    let everyone = ids(&[&first, &second, &third]);
    for device in [&first, &second, &third] {
        assert_eq!(device.lists(), everyone);
    }

    third.part().await;
    second.part().await;
    first.part().await;
}

/// A revocation takes a device off every sibling's list, as it did: the
/// devices linked to the one that holds it hear of it in its hello, and a
/// device enrolled afterwards never lists the revoked one, though the record
/// of the person's devices still names it.
#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_device_leaves_every_siblings_list() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let first = Device::new(&root, 2).await;
    let mut second = Device::new(&root, 3).await;
    let mut third = Device::new(&root, 4).await;
    second.links_to(&first);
    first.until_lists(&[&second]).await;
    third.links_to(&first);
    third.until_lists(&[&first, &second]).await;
    second.until_lists(&[&first, &third]).await;

    // The person takes the second device away, on the first.
    let gone = second.id();
    assert!(
        first
            .sync
            .admit(|store, _| store.revoke(revocation(&root, gone)))
            .unwrap()
    );
    third
        .until("the third device still lists the revoked one", |store| {
            !listed(store).contains(&gone)
        })
        .await;
    assert_eq!(first.lists(), ids(&[&first, &third]));
    assert_eq!(third.lists(), ids(&[&first, &third]));
    let now = SystemTime::now();
    assert!(
        third
            .sync
            .read(|store| store.admits_device(&gone, now).is_err())
    );

    // A device enrolled after the revocation learns of the third from the
    // first, and never of the second.
    let mut fourth = Device::new(&root, 5).await;
    fourth.links_to(&first);
    fourth.until_lists(&[&first, &third]).await;
    third.until_lists(&[&first, &fourth]).await;
    assert_eq!(fourth.lists(), ids(&[&first, &third, &fourth]));
    assert!(
        fourth
            .sync
            .read(|store| store.admits_device(&gone, now).is_err())
    );
    assert_eq!(third.lists(), ids(&[&first, &third, &fourth]));

    fourth.part().await;
    third.part().await;
    second.part().await;
    first.part().await;
}

/// Every frame `recv` brings until it has been quiet for a while.
async fn frames_until_quiet(recv: &mut iroh::endpoint::RecvStream) -> Vec<NeighbourMsg> {
    let mut frames = Vec::new();
    loop {
        let next = tokio::time::timeout(
            Duration::from_secs(2),
            wire_frame::Framing::little_endian(64 * 1024 * 1024)
                .reject_empty()
                .read_async(&mut *recv),
        )
        .await;
        match next {
            Ok(Ok(Some(bytes))) => frames.push(postcard::from_bytes(&bytes).unwrap()),
            _ => return frames,
        }
    }
}

/// A device of another person that dials one of the person's devices is sent
/// that device's own certificate and nothing of the others: not when the
/// link starts, and not when the person's devices change while it lasts.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_of_another_person_is_sent_none_of_the_persons_devices() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let other = SigningKey::from_bytes(&[9; 32]);
    let first = Device::new(&root, 2).await;
    let mut second = Device::new(&root, 3).await;
    second.links_to(&first);
    first.until_lists(&[&second]).await;
    second.until_lists(&[&first]).await;

    // It dials the first device and shows nothing: it only reads.
    let (stranger_key, _) = device(&other, 10);
    let stranger = localhost_endpoint(&stranger_key).await;
    let connection = stranger
        .connect(addr(&first.endpoint), PERSONAL_ALPN)
        .await
        .unwrap();
    let mut recv = connection.accept_uni().await.unwrap();

    // While it listens the person enrolls a third device: the record of
    // their devices moves, and reaches the second device live.
    let mut third = Device::new(&root, 4).await;
    third.links_to(&first);
    third.until_lists(&[&first, &second]).await;
    second.until_lists(&[&first, &third]).await;

    let frames = frames_until_quiet(&mut recv).await;
    assert!(!frames.is_empty(), "the device says who it is");
    for frame in &frames {
        match frame {
            NeighbourMsg::IdentityHello { cert, .. } => {
                assert_eq!(cert.device_public_key, first.id());
            }
            sent => panic!("a device of another person was sent {sent:?}"),
        }
    }

    connection.close(0u32.into(), b"done");
    stranger.close().await;
    third.part().await;
    second.part().await;
    first.part().await;
}
