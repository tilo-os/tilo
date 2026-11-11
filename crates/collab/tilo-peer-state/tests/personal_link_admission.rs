//! The person's documents leave a device only for another device of theirs.
//! Whoever dials the store's link is told who the device is and nothing of
//! what it holds; a snapshot of the person's documents, and every write made
//! while the link lasts, go only to a device whose certificate the store
//! holds: one the person's root signed, current, and not revoked. A device
//! that shows none, one of another person's, a revoked one and one whose
//! certificate ran out are sent no document at all; the person's new device
//! catches up on the link it dialled, once the certificate it shows is
//! admitted.
//!
//! Each device here but the first is the wire itself: it dials, reads every
//! frame it is sent and says only what the test has it say, so what a device
//! is handed is observed, not what a well-behaved store would keep of it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
tilo_automerge_isolate::test_worker!();

mod common;

use common::{addr, device, localhost_endpoint};

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use iroh::Endpoint;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use neighbour_wire::NeighbourMsg;
use std::time::Duration;
use tilo_peer_state::{DocumentMutation, PERSONAL_ALPN, PersonalStore, PersonalSync};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason, sign_device_cert,
};
use web_time::SystemTime;
use wire_frame::Framing;

const APP: &str = "tube";
const RECORD: &str = "prefs";
/// How long a step may take before the test calls it lost.
const PATIENCE: Duration = Duration::from_secs(30);
/// How long a link is listened to after its last frame before it is taken to
/// have said all it will.
const QUIET: Duration = Duration::from_secs(1);
const MAX_FRAME: usize = 64 * 1024 * 1024;

/// A record with `field` set to `value`, as an app's first write of it.
fn record(field: &str, value: &str) -> DocumentMutation {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, field, value).unwrap();
    doc.commit_edit(APP, field);
    DocumentMutation::Snapshot(doc.save())
}

/// Changes that depend on no other write of the record.
fn change(field: &str, value: &str) -> DocumentMutation {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, field, value).unwrap();
    doc.commit_edit(APP, field);
    DocumentMutation::Incremental(doc.save())
}

fn field(doc: &[u8], name: &str) -> Option<String> {
    let doc = AutoCommit::load(doc).unwrap();
    let (value, _) = doc.get(ROOT, name).unwrap()?;
    value.into_string().ok()
}

/// A device keyed `n`, certified by `root` until `expires_at_ns`.
fn certified(root: &SigningKey, n: u8, expires_at_ns: Option<u64>) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[n; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{n}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
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

fn ns(at: SystemTime) -> u64 {
    u64::try_from(
        at.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap()
}

/// Wait until `done` holds of `sync`'s store.
async fn until(sync: &PersonalSync, what: &str, done: impl Fn(&PersonalStore) -> bool + Sync) {
    let mut changes = sync.changes();
    tokio::time::timeout(PATIENCE, async {
        while !sync.read(&done) {
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}"));
}

/// The person's first device, holding a record of theirs and serving its
/// store, and a second device of theirs linked to it: what the second holds
/// says when a write has gone out on the first one's links.
struct Person {
    root: SigningKey,
    first_cert: DeviceCert,
    first: PersonalSync,
    endpoint: Endpoint,
    router: iroh::protocol::Router,
    witness: PersonalSync,
    witness_endpoint: Endpoint,
    witness_link: tokio::task::JoinHandle<Result<(), String>>,
}

impl Person {
    async fn new() -> Self {
        let root = SigningKey::from_bytes(&[1; 32]);
        let root_key = root.verifying_key().to_bytes();
        let now = SystemTime::now();
        let (key, first_cert) = device(&root, 2);
        let first = PersonalSync::new(
            PersonalStore::open(root_key, first_cert.clone(), now).unwrap(),
            key.clone(),
        );
        first
            .publish(|store, key, now| {
                store.publish(APP, RECORD, &record("theme", "dark"), key, now)
            })
            .unwrap();
        let endpoint = localhost_endpoint(&key).await;
        let router = iroh::protocol::Router::builder(endpoint.clone())
            .accept(PERSONAL_ALPN, first.clone())
            .spawn();

        let (witness_key, witness_cert) = device(&root, 7);
        let witness = PersonalSync::new(
            PersonalStore::open(root_key, witness_cert, now).unwrap(),
            witness_key.clone(),
        );
        let witness_endpoint = localhost_endpoint(&witness_key).await;
        let witness_link = tokio::spawn({
            let (witness, from, to) = (witness.clone(), witness_endpoint.clone(), addr(&endpoint));
            async move { witness.connect(&from, to).await }
        });
        until(
            &witness,
            "the person's second device never caught up",
            |store| store.holds_document(APP, RECORD),
        )
        .await;
        Self {
            root,
            first_cert,
            first,
            endpoint,
            router,
            witness,
            witness_endpoint,
            witness_link,
        }
    }

    fn other_person() -> SigningKey {
        SigningKey::from_bytes(&[9; 32])
    }

    /// Write `field` of the record on the first device, and wait until the
    /// write has gone out on its links: the person's second device has it.
    async fn writes(&self, name: &str, value: &str) {
        self.first
            .publish(|store, key, now| store.publish(APP, RECORD, &change(name, value), key, now))
            .unwrap();
        until(
            &self.witness,
            "a live write never reached the person's second device",
            |store| {
                store
                    .document(APP, RECORD)
                    .is_some_and(|doc| field(&doc, name).as_deref() == Some(value))
            },
        )
        .await;
    }

    /// Whether the first device's store holds a certificate of `device`.
    fn holds(&self, device: [u8; 32]) -> bool {
        self.first.read(|store| {
            store
                .devices()
                .iter()
                .any(|held| held.device_public_key == device)
        })
    }

    async fn part(self) {
        self.witness_link.abort();
        let _ = self.witness_link.await;
        self.router.shutdown().await.unwrap();
        self.witness_endpoint.close().await;
        self.endpoint.close().await;
    }
}

/// A device on the other end of the first device's link, as the wire has it:
/// every frame it is sent, and only what the test has it say.
struct Dialer {
    endpoint: Endpoint,
    connection: Connection,
    recv: RecvStream,
    send: Option<SendStream>,
}

impl Dialer {
    /// Dial the person's first device as the device `key`.
    async fn dial(key: &SigningKey, person: &Person) -> Self {
        let endpoint = localhost_endpoint(key).await;
        let connection = endpoint
            .connect(addr(&person.endpoint), PERSONAL_ALPN)
            .await
            .unwrap();
        let recv = tokio::time::timeout(PATIENCE, connection.accept_uni())
            .await
            .expect("the device opens its stream")
            .unwrap();
        Self {
            endpoint,
            connection,
            recv,
            send: None,
        }
    }

    /// Show `cert` as this device's own.
    async fn shows(&mut self, cert: &DeviceCert) {
        if self.send.is_none() {
            self.send = Some(self.connection.open_uni().await.unwrap());
        }
        let hello = NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert.clone()),
            revocations: Vec::new(),
        };
        Framing::little_endian(MAX_FRAME)
            .write_async(
                self.send.as_mut().unwrap(),
                &postcard::to_allocvec(&hello).unwrap(),
            )
            .await
            .unwrap();
    }

    /// The next frame this device is sent, within `within`; `None` when the
    /// link said nothing in that time or ended.
    async fn next(&mut self, within: Duration) -> Option<NeighbourMsg> {
        let read = Framing::little_endian(MAX_FRAME)
            .reject_empty()
            .read_async(&mut self.recv);
        match tokio::time::timeout(within, read).await {
            Ok(Ok(Some(bytes))) => Some(postcard::from_bytes(&bytes).unwrap()),
            _ => None,
        }
    }

    /// Every frame this device is sent until its link has been quiet for
    /// [`QUIET`], or ended.
    async fn hears(&mut self) -> Vec<NeighbourMsg> {
        let mut frames = Vec::new();
        while let Some(frame) = self.next(QUIET).await {
            frames.push(frame);
        }
        frames
    }

    /// Every frame this device is sent until one of them is `wanted`.
    async fn hears_until(
        &mut self,
        what: &str,
        wanted: impl Fn(&NeighbourMsg) -> bool + Send + Sync,
    ) -> Vec<NeighbourMsg> {
        let mut frames = Vec::new();
        let found = tokio::time::timeout(PATIENCE, async {
            while let Some(frame) = self.next(PATIENCE).await {
                let found = wanted(&frame);
                frames.push(frame);
                if found {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        assert!(found, "{what}: heard {frames:?}");
        frames
    }

    /// Wait until the device this one dialled has ended the link.
    async fn refused(&self) {
        tokio::time::timeout(PATIENCE, self.connection.closed())
            .await
            .expect("the device ends the link of a device it refuses");
    }

    async fn part(self) {
        self.connection.close(0u32.into(), b"done");
        self.endpoint.close().await;
    }
}

/// Whether `frame` carries a document, of any record.
fn document(frame: &NeighbourMsg) -> bool {
    matches!(frame, NeighbourMsg::AppDocument { .. })
}

/// The person's record, as `frame` carries it: `field` of the snapshot or
/// the change it is.
fn carries(frame: &NeighbourMsg, name: &str) -> Option<String> {
    let NeighbourMsg::AppDocument {
        app_id,
        record_id,
        envelope,
        ..
    } = frame
    else {
        return None;
    };
    if app_id != APP || record_id != RECORD {
        return None;
    }
    match DocumentMutation::decode(&envelope.payload).unwrap() {
        DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => {
            field(&bytes, name)
        }
    }
}

/// Nothing `frames` holds is a document, and the device said who it is.
fn only_who_the_device_is(frames: &[NeighbourMsg], person: &Person, when: &str) {
    for frame in frames {
        match frame {
            NeighbourMsg::IdentityHello { cert, .. } => {
                assert_eq!(cert.as_ref(), &person.first_cert, "{when}");
            }
            sent => panic!("{when}, the device was sent {sent:?}"),
        }
    }
}

/// A device that shows no certificate is told who the device it dialled is,
/// and sent no document: not when its link starts, and not when the person
/// writes while it lasts.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_shows_no_certificate_is_sent_no_document() {
    let person = Person::new().await;
    let (key, _) = device(&Person::other_person(), 10);
    let mut stranger = Dialer::dial(&key, &person).await;

    let heard = stranger.hears().await;
    assert!(!heard.is_empty(), "the device says who it is");
    only_who_the_device_is(&heard, &person, "when its link started");

    person.writes("accent", "blue").await;
    let heard = stranger.hears().await;
    only_who_the_device_is(&heard, &person, "when the person wrote");
    assert!(!person.holds(key.verifying_key().to_bytes()));

    stranger.part().await;
    person.part().await;
}

/// A device whose certificate the first device refuses is sent no document
/// before it shows the certificate, and nothing at all once it is refused,
/// whatever the person writes then.
async fn refused_with(person: &Person, key: &SigningKey, cert: &DeviceCert, whose: &str) {
    let mut stranger = Dialer::dial(key, person).await;
    let heard = stranger.hears().await;
    assert!(!heard.is_empty(), "the device says who it is to {whose}");
    only_who_the_device_is(
        &heard,
        person,
        &format!("before {whose} showed its certificate"),
    );

    stranger.shows(cert).await;
    stranger.refused().await;
    person.writes("accent", "blue").await;
    let heard = stranger.hears().await;
    only_who_the_device_is(&heard, person, &format!("after {whose} was refused"));
    assert!(!person.holds(key.verifying_key().to_bytes()), "{whose}");
    stranger.part().await;
}

/// A device certified by another person's identity is sent no document.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_certified_by_another_identity_is_sent_no_document() {
    let person = Person::new().await;
    let (key, cert) = device(&Person::other_person(), 10);
    refused_with(&person, &key, &cert, "another person's device").await;
    person.part().await;
}

/// A device whose certificate names this person's identity, signed by a key
/// that is not their root, is sent no document.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_whose_certificate_the_root_never_signed_is_sent_no_document() {
    let person = Person::new().await;
    let (key, mut forged) = device(&Person::other_person(), 11);
    forged.user_public_key = person.root.verifying_key().to_bytes();
    refused_with(&person, &key, &forged, "a device with a forged certificate").await;
    person.part().await;
}

/// A device the first never met, showing a certificate of the person's root
/// that ended long ago, is sent no document.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_shows_an_expired_certificate_is_sent_no_document() {
    let person = Person::new().await;
    let (key, cert) = certified(&person.root, 3, Some(2));
    refused_with(&person, &key, &cert, "a device with an expired certificate").await;
    person.part().await;
}

/// The person's new device, which the first never met, is sent no document
/// until the certificate it shows is admitted; then it catches up on the
/// link it dialled, and is sent what the person writes afterwards.
#[tokio::test(flavor = "multi_thread")]
async fn the_persons_new_device_catches_up_once_its_certificate_is_admitted() {
    let person = Person::new().await;
    let (key, cert) = device(&person.root, 3);
    let mut new = Dialer::dial(&key, &person).await;

    let heard = new.hears().await;
    assert!(!heard.is_empty(), "the device says who it is");
    only_who_the_device_is(
        &heard,
        &person,
        "before the new device showed its certificate",
    );

    new.shows(&cert).await;
    let caught_up = new
        .hears_until("the new device was never caught up", |frame| {
            carries(frame, "theme").as_deref() == Some("dark")
        })
        .await;
    assert!(caught_up.iter().any(document));
    assert!(person.holds(key.verifying_key().to_bytes()));

    person.writes("accent", "blue").await;
    new.hears_until("the new device was not sent a live write", |frame| {
        carries(frame, "accent").as_deref() == Some("blue")
    })
    .await;

    new.part().await;
    person.part().await;
}

/// A device whose certificate runs out while its link is open caught up as
/// the person's device, and is sent nothing of what is written from then on.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_whose_certificate_runs_out_is_sent_nothing_more() {
    let person = Person::new().await;
    let ends = SystemTime::now() + Duration::from_secs(5);
    let (key, cert) = certified(&person.root, 4, Some(ns(ends)));
    let mut short = Dialer::dial(&key, &person).await;
    short.shows(&cert).await;
    short
        .hears_until(
            "a device whose certificate holds was not caught up",
            |frame| carries(frame, "theme").as_deref() == Some("dark"),
        )
        .await;
    let still = ends.duration_since(SystemTime::now()).unwrap_or_default();
    tokio::time::sleep(still + Duration::from_millis(500)).await;
    // Whatever was on its way while the certificate held has arrived.
    short.hears().await;
    person.writes("accent", "blue").await;
    let heard = short.hears().await;
    assert!(
        !heard.iter().any(document),
        "its certificate ran out, and it was sent {heard:?}"
    );
    short.part().await;
    person.part().await;
}

/// A device the person takes away while its link is open caught up as their
/// device, and is sent nothing of what is written from then on.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_revoked_while_its_link_is_open_is_sent_nothing_more() {
    let person = Person::new().await;
    let (key, cert) = device(&person.root, 5);
    let mut open = Dialer::dial(&key, &person).await;
    open.shows(&cert).await;
    open.hears_until("a device of the person's was not caught up", |frame| {
        carries(frame, "theme").as_deref() == Some("dark")
    })
    .await;
    assert!(
        person
            .first
            .admit(|store, _| store.revoke(revocation(&person.root, cert.device_public_key)))
            .unwrap()
    );
    // Whatever was on its way before it was taken away has arrived.
    open.hears().await;
    person.writes("accent", "blue").await;
    let heard = open.hears().await;
    assert!(
        !heard.iter().any(document),
        "taken away while its link was open, it was sent {heard:?}"
    );
    open.part().await;
    person.part().await;
}

/// A device the person revoked before it dials is sent no document: not when
/// its link starts, not once it shows its certificate, and not when the
/// person writes while the link lasts.
async fn revoked_is_sent_nothing(person: &Person, key: &SigningKey, cert: &DeviceCert) {
    let mut gone = Dialer::dial(key, person).await;
    let heard = gone.hears().await;
    assert!(!heard.is_empty(), "the device says who it is");
    assert!(
        !heard.iter().any(document),
        "when its link started, a revoked device was sent {heard:?}"
    );

    gone.shows(cert).await;
    person.writes("accent", "blue").await;
    let heard = gone.hears().await;
    assert!(
        !heard.iter().any(document),
        "revoked, it was sent {heard:?}"
    );
    gone.part().await;
}

/// A device the first one knew, and the person then took away, is sent no
/// document.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_the_store_knew_and_the_person_revoked_is_sent_no_document() {
    let person = Person::new().await;
    let (key, cert) = device(&person.root, 3);
    person
        .first
        .admit(|store, now| {
            store.admit_device(&cert, now)?;
            store.revoke(revocation(&person.root, cert.device_public_key))
        })
        .unwrap();
    revoked_is_sent_nothing(&person, &key, &cert).await;
    person.part().await;
}

/// A device the first one never met, and only heard was taken away, is sent
/// no document.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_the_store_only_heard_was_revoked_is_sent_no_document() {
    let person = Person::new().await;
    let (key, cert) = device(&person.root, 4);
    person
        .first
        .admit(|store, _| store.revoke(revocation(&person.root, cert.device_public_key)))
        .unwrap();
    revoked_is_sent_nothing(&person, &key, &cert).await;
    person.part().await;
}
