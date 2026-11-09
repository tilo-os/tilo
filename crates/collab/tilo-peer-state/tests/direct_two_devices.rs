//! Two devices of one person converge on a workspace directly over iroh,
//! with no workspace host, relay or network actor in between, and a device
//! outside the workspace roster is refused.
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
use std::time::Duration;
use tilo_peer_state::direct::{DIRECT_ALPN, DirectSync};
use tilo_peer_state::{SeedDocument, SeedState, StatePeer, seed_workspace};
use tilo_user_identity::RevocationList;
use web_time::SystemTime;

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";

/// Write `value` into the test record as `key`'s device, and answer the heads
/// the record holds afterwards.
fn edit(peer: &DirectSync, key: &SigningKey, value: &str) -> Option<Vec<automerge::ChangeHash>> {
    let mut edit =
        AutoCommit::load(&peer.read(|peer| peer.document(APP, RECORD)).unwrap()).unwrap();
    edit.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = edit.get_heads();
    edit.put(ROOT, "value", value).unwrap();
    edit.commit_edit("fixture", "value");
    let change = edit.save_after(&heads);
    peer.publish(|peer, key, now| {
        peer.publication_mut()
            .publish_incremental(APP, RECORD, change, key, now)
    })
    .unwrap();
    peer.read(|peer| peer.document_heads(APP, RECORD))
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
        WORKSPACE,
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
            workspace_changes: seed_workspace(WORKSPACE, &a_cert, &roster).unwrap(),
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
    let b = StatePeer::from_join(WORKSPACE, a_cert.clone(), roster, &[], now).unwrap();
    assert!(b.rows().is_empty());

    let a = DirectSync::new(a, a_key.clone());
    let b = DirectSync::new(b, b_key.clone());
    let a_endpoint = localhost_endpoint(&a_key).await;
    let a_router = iroh::protocol::Router::builder(a_endpoint.clone())
        .accept(DIRECT_ALPN, a.clone())
        .spawn();
    let b_endpoint = localhost_endpoint(&b_key).await;
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
    let edited = edit(&b, &b_key, "from b");
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
    let c =
        StatePeer::from_join(WORKSPACE, a_cert.clone(), vec![a_cert, c_cert], &[], now).unwrap();
    let c = DirectSync::new(c, c_key.clone());
    let c_endpoint = localhost_endpoint(&c_key).await;
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

/// A person's second device enters the workspace the first made, with no host
/// and no invitation: the first writes it in as the member's own device
/// because its root certifies it, and content crosses both ways.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_device_is_entered_by_the_first_and_content_crosses_both_ways() {
    let root = SigningKey::from_bytes(&[4; 32]);
    let (a_key, a_cert) = device(&root, 5);
    let (b_key, b_cert) = device(&root, 6);
    let now = SystemTime::now();

    // A makes the workspace with itself alone in it, and a document.
    let mut a = StatePeer::new(
        WORKSPACE,
        a_cert.clone(),
        vec![a_cert.clone()],
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
            workspace_changes: seed_workspace(WORKSPACE, &a_cert, std::slice::from_ref(&a_cert))
                .unwrap(),
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

    // B is the same person's second device. Its root certifies it, so A writes
    // it in as that member's device: no invitation, and A is the only device
    // that has to be there.
    assert!(
        a.governance_mut()
            .admit_member_device(&b_cert, &a_key, now)
            .unwrap()
    );
    let roster = a.certificates();
    assert!(
        roster
            .iter()
            .any(|cert| cert.device_public_key == b_cert.device_public_key),
        "the workspace holds B's device"
    );
    let b = StatePeer::from_join(WORKSPACE, a_cert.clone(), roster, &[], now).unwrap();
    assert!(b.rows().is_empty());

    let a = DirectSync::new(a, a_key.clone());
    let b = DirectSync::new(b, b_key.clone());
    let a_endpoint = localhost_endpoint(&a_key).await;
    let a_router = iroh::protocol::Router::builder(a_endpoint.clone())
        .accept(DIRECT_ALPN, a.clone())
        .spawn();
    let b_endpoint = localhost_endpoint(&b_key).await;
    let link = tokio::spawn({
        let b = b.clone();
        let b_endpoint = b_endpoint.clone();
        let a_addr = addr(&a_endpoint);
        async move { b.connect(&b_endpoint, a_addr).await }
    });

    // What A made reaches B.
    let rows = a.read(StatePeer::rows);
    until(&b, |b| b.rows() == rows).await;
    let from_a = AutoCommit::load(&b.read(|b| b.document(APP, RECORD)).unwrap()).unwrap();
    assert_eq!(
        from_a
            .get(ROOT, "value")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .unwrap(),
        "from a"
    );

    // What B writes reaches A, and A's next write reaches B.
    let edited = edit(&b, &b_key, "from b");
    until(&a, |a| a.document_heads(APP, RECORD) == edited).await;
    let edited = edit(&a, &a_key, "from a again");
    until(&b, |b| b.document_heads(APP, RECORD) == edited).await;

    link.abort();
    a_router.shutdown().await.unwrap();
    b_endpoint.close().await;
}

/// A device that hosts its own workspace serves its own member under the same
/// key: what it publishes is the host's to keep, not an echo of a frame the
/// host already has.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_hosts_its_own_workspace_syncs_with_it_in_process() {
    let root = SigningKey::from_bytes(&[5; 32]);
    let (key, cert) = device(&root, 7);
    let now = SystemTime::now();
    let roster = vec![cert.clone()];
    let mut made = StatePeer::new(
        WORKSPACE,
        cert.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    made.select_admission_v5().unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "from the host").unwrap();
    document.commit_edit("fixture", "value");
    made.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &cert, &roster).unwrap(),
            documents: vec![SeedDocument {
                app_id: APP.into(),
                record_id: RECORD.into(),
                kind: "test".into(),
                bytes: document.save(),
            }],
        },
        &key,
        now,
    )
    .unwrap();
    let joined = StatePeer::from_join(WORKSPACE, cert.clone(), roster, &[], now).unwrap();

    let host = DirectSync::new(made, key.clone());
    let member = DirectSync::new(joined, key.clone());
    let paired = tokio::spawn({
        let host = host.clone();
        let member = member.clone();
        async move { DirectSync::pair(&member, &host).await }
    });

    // The member has the host's history before it writes to it.
    until(&member, |member| {
        member.document_heads(APP, RECORD).is_some()
    })
    .await;
    // The member writes as the device the host serves: its own.
    let edited = edit(&member, &key, "from the member");
    until(&host, |host| host.document_heads(APP, RECORD) == edited).await;
    paired.abort();
}
