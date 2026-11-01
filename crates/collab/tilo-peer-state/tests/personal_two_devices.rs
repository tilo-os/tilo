//! Two devices of one person converge on the person's own documents directly
//! over iroh, with no host in between and no workspace anywhere, and a device
//! of another identity is refused and holds nothing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
tilo_automerge_isolate::test_worker!();

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use std::time::Duration;
use tilo_peer_state::{PERSONAL_ALPN, PersonalStore, PersonalSync};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use web_time::SystemTime;

const APP: &str = "tube";
const RECORD: &str = "prefs";

/// A device key `n` certified by `root`.
fn device(root: &SigningKey, n: u8, capabilities: DeviceCapabilities) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[n; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{n}"),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// A localhost endpoint with no relay whose identity is the device `key`.
async fn endpoint(key: &SigningKey) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .secret_key(SecretKey::from_bytes(&key.to_bytes()))
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

fn addr(endpoint: &Endpoint) -> EndpointAddr {
    EndpointAddr::new(endpoint.id()).with_ip_addr(std::net::SocketAddr::from((
        std::net::Ipv4Addr::LOCALHOST,
        endpoint.bound_sockets().first().unwrap().port(),
    )))
}

/// A record with `field` set to `value`, as an app's first write of it.
fn record(field: &str, value: &str) -> collab_types::DocumentMutation {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, field, value).unwrap();
    doc.commit_edit(APP, field);
    collab_types::DocumentMutation::Snapshot(doc.save())
}

/// Changes that depend on no other write of the record.
fn change(field: &str, value: &str) -> collab_types::DocumentMutation {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, field, value).unwrap();
    doc.commit_edit(APP, field);
    collab_types::DocumentMutation::Incremental(doc.save())
}

fn field(doc: &[u8], name: &str) -> Option<String> {
    let doc = AutoCommit::load(doc).unwrap();
    let (value, _) = doc.get(ROOT, name).unwrap()?;
    value.into_string().ok()
}

/// Wait until `done` holds of `sync`'s store.
async fn until(sync: &PersonalSync, done: impl Fn(&PersonalStore) -> bool + Sync) {
    let mut changes = sync.changes();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !sync.read(&done) {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("the stores converge");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_devices_of_one_person_converge_on_their_own_documents() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let root_key = root.verifying_key().to_bytes();
    let (a_key, a_cert) = device(&root, 2, DeviceCapabilities::member());
    let (b_key, b_cert) = device(&root, 3, DeviceCapabilities::member());
    let now = SystemTime::now();

    let a = PersonalStore::open(root_key, a_cert, now).unwrap();
    let b = PersonalStore::open(root_key, b_cert, now).unwrap();
    let a = PersonalSync::new(a, a_key.clone());
    let b = PersonalSync::new(b, b_key.clone());

    // A writes one of the person's records: no workspace exists anywhere.
    a.publish(|store, key, now| store.publish(APP, RECORD, &record("theme", "dark"), key, now))
        .unwrap();
    assert!(b.store().rows().is_empty());

    let a_endpoint = endpoint(&a_key).await;
    let a_router = iroh::protocol::Router::builder(a_endpoint.clone())
        .accept(PERSONAL_ALPN, a.clone())
        .spawn();
    let b_endpoint = endpoint(&b_key).await;
    let link = tokio::spawn({
        let b = b.clone();
        let b_endpoint = b_endpoint.clone();
        let a_addr = addr(&a_endpoint);
        async move { b.connect(&b_endpoint, a_addr).await }
    });

    // B catches up from A, then reads the record A wrote.
    until(&b, |store| store.holds_document(APP, RECORD)).await;
    let read = b.store().document(APP, RECORD).unwrap();
    assert_eq!(field(&read, "theme").as_deref(), Some("dark"));
    assert_eq!(b.store().rows(), a.store().rows());

    // B writes; A converges to it live.
    b.publish(|store, key, now| store.publish(APP, RECORD, &change("accent", "blue"), key, now))
        .unwrap();
    until(&a, |store| {
        store
            .document(APP, RECORD)
            .is_some_and(|doc| field(&doc, "accent").is_some())
    })
    .await;
    let on_a = a.store().document(APP, RECORD).unwrap();
    assert_eq!(field(&on_a, "accent").as_deref(), Some("blue"));
    assert_eq!(field(&on_a, "theme").as_deref(), Some("dark"));
    assert_eq!(a.store().rows(), b.store().rows());

    // A device of another identity is refused the link and holds nothing.
    let other = SigningKey::from_bytes(&[9; 32]);
    let (c_key, c_cert) = device(&other, 10, DeviceCapabilities::member());
    let c = PersonalSync::new(
        PersonalStore::open(other.verifying_key().to_bytes(), c_cert, now).unwrap(),
        c_key.clone(),
    );
    let c_endpoint = endpoint(&c_key).await;
    let refused = c.connect(&c_endpoint, addr(&a_endpoint)).await;
    assert!(refused.is_err(), "a device of another identity is refused");
    assert!(c.store().rows().is_empty());
    assert!(a.read(|store| {
        !store
            .devices()
            .iter()
            .any(|cert| cert.device_public_key == c_key.verifying_key().to_bytes())
    }));

    // A store node holds none of the person's documents: it cannot even open
    // a store on their identity.
    let (_, node_cert) = device(&root, 4, DeviceCapabilities::host());
    assert!(PersonalStore::open(root_key, node_cert, now).is_err());

    link.abort();
    a_router.shutdown().await.unwrap();
    b_endpoint.close().await;
    c_endpoint.close().await;
}
