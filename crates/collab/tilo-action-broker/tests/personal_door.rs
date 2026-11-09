//! The person's own store open to their other devices: a record their app
//! writes on one of their devices reaches an app on another, on the route the
//! apps open, and a device their identity revoked stops reaching them.
//!
//! Both devices are real endpoints over loopback, and the device that serves
//! answers the store's link on its own router, as a host does.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]
tilo_automerge_isolate::test_worker!();

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable as _};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use runtime_collab::event::CollabEvent;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tilo_action_broker::collab_broker::{BrokerChannels, FanoutState};
use tilo_action_broker::personal::{Door, Person};
use tilo_peer_state::{PERSONAL_ALPN, PersonalStore, PersonalSync};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason, sign_device_cert,
    sign_revocation_entry,
};

/// The app whose own record the devices carry, and the record of it: a
/// contact the person added, as Contacts keeps it.
const APP: &str = "so.tilo.contacts";
const RECORD: &str = "uuid:5c07c075-0000-4000-8000-0000000000c1";

/// A device key `n` certified by `root`.
fn device(root: &SigningKey, n: u8) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[n; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{n}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// A localhost endpoint with no relay whose identity is the device `key`, as
/// the host's endpoint names its device.
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

/// Where `endpoint` is reached, as the test dials it.
fn addr(endpoint: &Endpoint) -> EndpointAddr {
    EndpointAddr::new(endpoint.id()).with_ip_addr(std::net::SocketAddr::from((
        std::net::Ipv4Addr::LOCALHOST,
        endpoint.bound_sockets().first().unwrap().port(),
    )))
}

/// A contact record with `field` set to `value`, as an app writes one.
fn record(field: &str, value: &str) -> collab_types::DocumentMutation {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, field, value).unwrap();
    doc.commit_edit(APP, field);
    collab_types::DocumentMutation::Snapshot(doc.save())
}

/// A change of an existing record of the person's, as an app writes one.
fn change(field: &str, value: &str) -> collab_types::DocumentMutation {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, field, value).unwrap();
    doc.commit_edit(APP, field);
    collab_types::DocumentMutation::Incremental(doc.save())
}

/// What a record holds under `name`.
fn field(document: &[u8], name: &str) -> Option<String> {
    let doc = AutoCommit::load(document).unwrap();
    doc.get(ROOT, name)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

/// The person's store on `cert`'s device, on the root the certificate names.
fn store(root_key: [u8; 32], cert: &DeviceCert, key: &SigningKey) -> PersonalSync {
    PersonalSync::new(
        PersonalStore::open(root_key, cert.clone(), SystemTime::now()).unwrap(),
        key.clone(),
    )
}

/// Wait until `done` holds of `sync`'s store.
async fn until(sync: &PersonalSync, done: impl Fn(&PersonalStore) -> bool + Sync) {
    let mut changes = sync.changes();
    tokio::time::timeout(Duration::from_secs(60), async {
        while !sync.read(&done) {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("the stores converge");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_record_written_on_one_device_reaches_the_app_on_another() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let root_key = root.verifying_key().to_bytes();
    let (a_key, a_cert) = device(&root, 2);
    let (b_key, b_cert) = device(&root, 3);
    let person = Person::of(hex::encode(root_key), root_key);
    let a = store(root_key, &a_cert, &a_key);
    let b = store(root_key, &b_cert, &b_key);
    let cluster = a.store().store_key();
    assert_eq!(
        cluster,
        collab_types::personal_cluster(&root_key).to_string()
    );

    // What this device's apps read of the person's scope: the writes their
    // store hands them, on the route the record is opened on.
    let channels = Arc::new(Mutex::new(BrokerChannels::default()));
    let fanout = Arc::new(FanoutState::new());
    let (heard, heard_rx) = std::sync::mpsc::channel();
    fanout.subscribe_workspaces(
        vec![cluster.clone()],
        Box::new(
            move |event: &CollabEvent, route: &tilo_action_broker::route::CrdtRoute| {
                if let CollabEvent::Document { envelope, .. } = event {
                    heard.send((route.clone(), (**envelope).clone())).ok();
                }
            },
        ) as Box<dyn Fn(&CollabEvent, &tilo_action_broker::route::CrdtRoute) + Send + Sync>,
    );

    // This device serves the person's store: its own router answers the
    // store's link, as the session's endpoint does, and the door carries what
    // a sibling writes to the apps here.
    let a_endpoint = endpoint(&a_key).await;
    let router = iroh::protocol::Router::builder(a_endpoint.clone())
        .accept(PERSONAL_ALPN, a.clone())
        .spawn();
    let state = tempfile::tempdir().unwrap();
    let door = Door::serve(
        &tokio::runtime::Handle::current(),
        a_endpoint.clone(),
        a.clone(),
        &person,
        state.path(),
        Arc::clone(&channels),
        Arc::clone(&fanout),
    );

    // Their other device links to this one, and this device's store hears of
    // it: the person's phone, paired as one of their devices.
    let b_endpoint = endpoint(&b_key).await;
    let link = tokio::spawn({
        let (b, b_endpoint, a_addr) = (b.clone(), b_endpoint.clone(), addr(&a_endpoint));
        async move { b.connect(&b_endpoint, a_addr).await }
    });
    until(&a, |store| {
        store
            .devices()
            .iter()
            .any(|cert| cert.device_public_key == b_key.verifying_key().to_bytes())
    })
    .await;

    // The person adds a contact on their phone while this device runs: the
    // record reaches this device's apps as it is written.
    b.publish(|store, key, now| store.publish(APP, RECORD, &record("name", "Ada"), key, now))
        .unwrap();
    until(&a, |store| store.holds_document(APP, RECORD)).await;

    // What it wrote reaches the apps here as their own person's change, on
    // the route Contacts opens the record on.
    let (route, envelope) = heard_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the app here hears the contact their other device added");
    assert_eq!(route.app_id, APP);
    assert_eq!(route.record_id, RECORD);
    assert_eq!(route.cluster_id, cluster);
    assert_eq!(
        envelope.author.as_bytes(),
        &b_key.verifying_key().to_bytes()
    );

    // And the other way: what this device's app writes reaches the person's
    // other device through the link the same door keeps.
    a.publish(|store, key, now| {
        store.publish(APP, RECORD, &change("email", "ada@example.com"), key, now)
    })
    .unwrap();
    until(&b, |store| {
        store
            .document(APP, RECORD)
            .is_some_and(|doc| field(&doc, "email").as_deref() == Some("ada@example.com"))
    })
    .await;

    // A device the person revoked stops reaching this one: the link stays as
    // the store tells it why, what it writes after that is refused and filed
    // as refused, and none of it reaches this device's apps or its records.
    let mut revoked = RevocationEntry {
        user_public_key: root_key,
        revoked_device_public_key: b_key.verifying_key().to_bytes(),
        revoked_at_ns: 1,
        reason: RevocationReason::Retired,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut revoked, &root).unwrap();
    assert!(
        a.admit(|store, _| store.revoke(revoked)).unwrap(),
        "the person's device was revoked in the store"
    );
    b.publish(|store, key, now| store.publish(APP, RECORD, &change("name", "Grace"), key, now))
        .unwrap();
    until(&a, |store| {
        store
            .write_refusals()
            .iter()
            .any(|refusal| refusal.author == b_key.verifying_key().to_bytes())
    })
    .await;
    let refusal = a
        .read(PersonalStore::write_refusals)
        .into_iter()
        .find(|refusal| refusal.author == b_key.verifying_key().to_bytes())
        .expect("the refused write was filed");
    assert!(
        refusal.detail.contains("revoked"),
        "it was refused because the person revoked that device: {}",
        refusal.detail
    );
    assert!(
        heard_rx.recv_timeout(Duration::from_millis(500)).is_err(),
        "nothing a revoked device wrote reaches this device's apps"
    );
    let held = a
        .read(|store| store.document(APP, RECORD))
        .expect("the record the person's own store holds");
    assert_eq!(
        field(&held, "name").as_deref(),
        Some("Ada"),
        "the revoked device's write never reached the record here"
    );

    link.abort();
    drop(door);
    router.shutdown().await.unwrap();
    a_endpoint.close().await;
    b_endpoint.close().await;
}
