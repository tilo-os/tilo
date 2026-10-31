//! Two devices of one person converge on a workspace directly over iroh,
//! with no room host, relay or network actor in between, and a device
//! outside the workspace roster is refused.
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
use tilo_peer_state::direct::{DIRECT_ALPN, DirectSync};
use tilo_peer_state::{SeedDocument, SeedState, StatePeer, seed_room};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use web_time::SystemTime;

const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";

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

/// Wait until `done` holds of `sync`'s replica.
async fn until(sync: &DirectSync, done: impl Fn(&StatePeer) -> bool + Sync) {
    let mut changes = sync.changes();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !sync.read(&done) {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("the replicas converge");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_devices_of_one_person_converge_directly() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let (a_key, a_cert) = device(&root, 2);
    let (b_key, b_cert) = device(&root, 3);
    let roster = vec![a_cert.clone(), b_cert.clone()];
    let now = SystemTime::now();

    // Device A creates the workspace and a document.
    let mut a = StatePeer::new(
        ROOM,
        a_cert.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    a.select_admission_v5().unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "from a").unwrap();
    document.commit_edit("fixture", "value");
    a.install_seed(
        SeedState {
            room_changes: seed_room(ROOM, &a_cert, &roster).unwrap(),
            documents: vec![SeedDocument {
                app_id: APP.into(),
                record_id: RECORD.into(),
                kind: "test".into(),
                bytes: document.save(),
            }],
        },
        &a_key,
        now,
    )
    .unwrap();
    // Device B knows the workspace's roster and nothing else.
    let b = StatePeer::from_join(ROOM, a_cert.clone(), roster, &[], now).unwrap();
    assert!(b.rows().is_empty());

    let a = DirectSync::new(a, a_key.clone());
    let b = DirectSync::new(b, b_key.clone());
    let a_endpoint = endpoint(&a_key).await;
    let a_router = iroh::protocol::Router::builder(a_endpoint.clone())
        .accept(DIRECT_ALPN, a.clone())
        .spawn();
    let b_endpoint = endpoint(&b_key).await;
    let link = tokio::spawn({
        let b = b.clone();
        let b_endpoint = b_endpoint.clone();
        let a_addr = addr(&a_endpoint);
        async move { b.connect(&b_endpoint, a_addr).await }
    });

    // B converges to A's rows and document heads.
    let rows = a.read(StatePeer::rows);
    until(&b, |b| b.rows() == rows).await;
    assert_eq!(
        b.read(|b| b.document_heads(APP, RECORD)),
        a.read(|a| a.document_heads(APP, RECORD))
    );

    // B edits the document; A converges to B's edit.
    let mut edit = AutoCommit::load(&b.read(|b| b.document(APP, RECORD)).unwrap()).unwrap();
    edit.set_actor(automerge::ActorId::from(
        b_key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = edit.get_heads();
    edit.put(ROOT, "value", "from b").unwrap();
    edit.commit_edit("fixture", "value");
    let change = edit.save_after(&heads);
    b.publish(|b, key, now| {
        b.publication_mut()
            .publish_incremental(APP, RECORD, change, key, now)
    })
    .unwrap();
    let edited = b.read(|b| b.document_heads(APP, RECORD));
    assert_eq!(edited, Some(edit.get_heads()));
    until(&a, |a| a.document_heads(APP, RECORD) == edited).await;
    let on_a = AutoCommit::load(&a.read(|a| a.document(APP, RECORD)).unwrap()).unwrap();
    assert_eq!(
        on_a.get(ROOT, "value")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .unwrap(),
        "from b"
    );

    // A device of another person, absent from the roster, is refused.
    let stranger_root = SigningKey::from_bytes(&[9; 32]);
    let (c_key, c_cert) = device(&stranger_root, 10);
    let c = StatePeer::from_join(ROOM, a_cert.clone(), vec![a_cert, c_cert], &[], now).unwrap();
    let c = DirectSync::new(c, c_key.clone());
    let c_endpoint = endpoint(&c_key).await;
    let refused = c.connect(&c_endpoint, addr(&a_endpoint)).await;
    assert!(refused.is_err(), "a device outside the roster is refused");
    assert!(c.read(|c| c.rows().is_empty()));
    assert!(a.read(|a| {
        !a.certificates()
            .iter()
            .any(|cert| cert.device_public_key == c_key.verifying_key().to_bytes())
    }));

    link.abort();
    a_router.shutdown().await.unwrap();
    b_endpoint.close().await;
    c_endpoint.close().await;
}
