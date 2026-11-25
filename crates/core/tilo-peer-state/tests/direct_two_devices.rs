//! Two devices of one person converge on a workspace directly over iroh,
//! with no workspace host, server or network actor in between, and a device
//! outside the workspace roster is refused.

use crate::common::{
    addr, device, localhost_endpoint, members_keys, opened, opened_heads, stored, write_sealed,
};

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use key_group::ContentKeys;
use std::time::Duration;
use tilo_peer_state::direct::{DIRECT_ALPN, DirectSync, Kept};
use tilo_peer_state::{SeedState, StatePeer, seed_workspace};
use tilo_user_identity::RevocationList;
use web_time::SystemTime;

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";

/// Write `value` into the test record as `key`'s device, sealed under `keys`,
/// and answer the heads the record holds afterwards.
fn edit(
    peer: &DirectSync,
    (keys, key): (&ContentKeys, &SigningKey),
    person: &[u8; 32],
    value: &str,
) -> Option<Vec<automerge::ChangeHash>> {
    write_sealed(peer, keys, key, (APP, RECORD), |record| {
        record.put(ROOT, "value", value).unwrap();
        record.commit_edit("fixture", "value", person);
    });
    heads(peer, keys)
}

/// The test record's heads on `peer`, read with `keys`.
fn heads(peer: &DirectSync, keys: &ContentKeys) -> Option<Vec<automerge::ChangeHash>> {
    peer.read(|peer| opened_heads(peer, keys, (APP, RECORD)))
}

/// The test record's `value` on `peer`, read with `keys`.
fn value(peer: &DirectSync, keys: &ContentKeys) -> String {
    AutoCommit::load(&peer.read(|peer| opened(peer, keys, (APP, RECORD))).unwrap())
        .unwrap()
        .get(ROOT, "value")
        .unwrap()
        .unwrap()
        .0
        .into_string()
        .unwrap()
}

/// The workspace's first record, written by `key`'s device as `person`.
fn seed(sync: &DirectSync, keys: &ContentKeys, key: &SigningKey, person: &[u8; 32], value: &str) {
    write_sealed(sync, keys, key, (APP, RECORD), |record| {
        record.put(ROOT, "value", value).unwrap();
        record.commit_edit("fixture", "value", person);
    });
}

/// Wait until `done` holds of `sync`'s replica; `what` names the wait that gave up.
async fn until(sync: &DirectSync, what: &str, done: impl Fn(&StatePeer) -> bool + Sync) {
    let mut changes = sync.changes();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !sync.read(&done) {
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the replicas converge: {what}"));
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
    a.select_exact_admissions().unwrap();
    a.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &a_cert, &roster).unwrap(),
            documents: Vec::new(),
        },
        &a_key,
        now,
    )
    .unwrap();
    // Device B knows the workspace's roster and nothing else.
    let b = StatePeer::from_join(WORKSPACE, a_cert.clone(), roster, &[], now).unwrap();
    assert!(b.rows().is_empty());

    let directory = tempfile::tempdir().unwrap();
    let a = stored(&DirectSync::new(a, a_key.clone()), &directory);
    let keys = members_keys(WORKSPACE);
    seed(&a, &keys, &a_key, &a_cert.user_public_key, "from a");
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
    until(&b, "B holds A's rows", |b| b.rows() == rows).await;
    let held = heads(&a, &keys);
    until(&b, "B holds A's record", |b| {
        opened_heads(b, &keys, (APP, RECORD)) == held
    })
    .await;
    assert_eq!(heads(&b, &keys), heads(&a, &keys));

    // B edits the document; A converges to B's edit.
    let edited = edit(
        &b,
        (&keys, &b_key),
        &root.verifying_key().to_bytes(),
        "from b",
    );
    until(&a, "A holds B's edit", |a| {
        opened_heads(a, &keys, (APP, RECORD)) == edited
    })
    .await;
    assert_eq!(value(&a, &keys), "from b");

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
    a.select_exact_admissions().unwrap();
    a.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &a_cert, std::slice::from_ref(&a_cert))
                .unwrap(),
            documents: Vec::new(),
        },
        &a_key,
        now,
    )
    .unwrap();

    // B is the same person's second device. Its root certifies it, so B lists
    // itself as that member's device on A's copy: no invitation, and A is the
    // only device that has to be there.
    assert!(
        a.governance_mut()
            .admit_member_device(&b_cert, &b_key, now)
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

    let directory = tempfile::tempdir().unwrap();
    let a = stored(&DirectSync::new(a, a_key.clone()), &directory);
    let keys = members_keys(WORKSPACE);
    seed(&a, &keys, &a_key, &a_cert.user_public_key, "from a");
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
    until(&b, "B holds A's rows", |b| b.rows() == rows).await;
    until(&b, "B holds A's record", |b| {
        opened(b, &keys, (APP, RECORD)).is_some()
    })
    .await;
    assert_eq!(value(&b, &keys), "from a");

    // What B writes reaches A, and A's next write reaches B.
    let edited = edit(
        &b,
        (&keys, &b_key),
        &root.verifying_key().to_bytes(),
        "from b",
    );
    until(&a, "A holds the edit", |a| {
        opened_heads(a, &keys, (APP, RECORD)) == edited
    })
    .await;
    let edited = edit(
        &a,
        (&keys, &a_key),
        &root.verifying_key().to_bytes(),
        "from a again",
    );
    until(&b, "B holds the edit", |b| {
        opened_heads(b, &keys, (APP, RECORD)) == edited
    })
    .await;

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
    made.select_exact_admissions().unwrap();
    made.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &cert, &roster).unwrap(),
            documents: Vec::new(),
        },
        &key,
        now,
    )
    .unwrap();
    let joined = StatePeer::from_join(WORKSPACE, cert.clone(), roster, &[], now).unwrap();

    let directory = tempfile::tempdir().unwrap();
    let host = stored(&DirectSync::new(made, key.clone()), &directory);
    let keys = members_keys(WORKSPACE);
    seed(&host, &keys, &key, &cert.user_public_key, "from the host");
    let member = DirectSync::new(joined, key.clone());
    let paired = tokio::spawn({
        let host = host.clone();
        let member = member.clone();
        async move { DirectSync::pair(&member, &host).await }
    });

    // The member has the host's history before it writes to it.
    until(&member, "the member holds the record", |member| {
        opened(member, &keys, (APP, RECORD)).is_some()
    })
    .await;
    // The member writes as the device the host serves: its own.
    let edited = edit(
        &member,
        (&keys, &key),
        &root.verifying_key().to_bytes(),
        "from the member",
    );
    until(&host, "the host holds the member's edit", |host| {
        opened_heads(host, &keys, (APP, RECORD)) == edited
    })
    .await;
    paired.abort();
}

/// The device dialled answers each write of the dialling device's it takes in, named as the
/// writer's ownership names it: the hash of the frame it published.
#[tokio::test(flavor = "multi_thread")]
async fn the_device_dialled_answers_each_write_it_takes_in() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let (a_key, a_cert) = device(&root, 2);
    let (b_key, b_cert) = device(&root, 3);
    let roster = vec![a_cert.clone(), b_cert];
    let now = SystemTime::now();
    let mut a = StatePeer::new(
        WORKSPACE,
        a_cert.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    a.select_exact_admissions().unwrap();
    a.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &a_cert, &roster).unwrap(),
            documents: Vec::new(),
        },
        &a_key,
        now,
    )
    .unwrap();
    let b = StatePeer::from_join(WORKSPACE, a_cert.clone(), roster, &[], now).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let a = stored(&DirectSync::new(a, a_key.clone()), &directory);
    let keys = members_keys(WORKSPACE);
    let person = root.verifying_key().to_bytes();
    seed(&a, &keys, &a_key, &person, "from a");
    let b = DirectSync::new(b, b_key.clone());
    let a_endpoint = localhost_endpoint(&a_key).await;
    let _a_router = iroh::protocol::Router::builder(a_endpoint.clone())
        .accept(DIRECT_ALPN, a.clone())
        .spawn();
    let b_endpoint = localhost_endpoint(&b_key).await;
    let mut answers = b.answers();
    let _link = tokio::spawn({
        let (b, b_endpoint, a_addr) = (b.clone(), b_endpoint.clone(), addr(&a_endpoint));
        async move { b.connect(&b_endpoint, a_addr).await }
    });
    let rows = a.read(StatePeer::rows);
    until(&b, "B holds A's rows", |b| b.rows() == rows).await;
    until(&b, "B holds A's record", |b| {
        opened(b, &keys, (APP, RECORD)).is_some()
    })
    .await;

    let message = write_sealed(&b, &keys, &b_key, (APP, RECORD), |record| {
        record.put(ROOT, "value", "from b").unwrap();
        record.commit_edit("fixture", "value", &person);
    });
    let write = *blake3::hash(&postcard::to_allocvec(&message).unwrap()).as_bytes();
    let answered = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let answered = answers.recv().await.unwrap();
            if answered.write == write {
                return answered;
            }
        }
    })
    .await
    .expect("A answers B's write");
    assert_eq!(answered.kept, Kept::Durable);
}
