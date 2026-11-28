//! Two devices of one person converge on a workspace directly over iroh,
//! with no workspace host, server or network actor in between, and a device
//! outside the workspace roster is refused.

use crate::common::{
    addr, localhost_endpoint, members_keys, opened, opened_heads, stored, write_sealed,
};

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use key_group::ContentKeys;
use std::sync::Arc;
use std::time::Duration;
use tilo_peer_state::StatePeer;
use tilo_peer_state::direct::{DIRECT_ALPN, DirectSync, Kept};
use tilo_user_identity::{DeviceCapabilities, RevocationList};
use web_time::SystemTime;

use tilo_peer_state::fixture::certify;
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
    let (a_key, a_cert) = certify(&root, 2, DeviceCapabilities::member());
    let (b_key, b_cert) = certify(&root, 3, DeviceCapabilities::member());
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
    tilo_peer_state::fixture::found(&mut a, (&a_key, &a_cert), &roster, Vec::new(), now);
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
    let (c_key, c_cert) = certify(&stranger_root, 10, DeviceCapabilities::member());
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
    let (a_key, a_cert) = certify(&root, 5, DeviceCapabilities::member());
    let (b_key, b_cert) = certify(&root, 6, DeviceCapabilities::member());
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
    tilo_peer_state::fixture::found(
        &mut a,
        (&a_key, &a_cert),
        std::slice::from_ref(&a_cert),
        Vec::new(),
        now,
    );

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
    let (key, cert) = certify(&root, 7, DeviceCapabilities::member());
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
    tilo_peer_state::fixture::found(&mut made, (&key, &cert), &roster, Vec::new(), now);
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
    let (a_key, a_cert) = certify(&root, 2, DeviceCapabilities::member());
    let (b_key, b_cert) = certify(&root, 3, DeviceCapabilities::member());
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
    tilo_peer_state::fixture::found(&mut a, (&a_key, &a_cert), &roster, Vec::new(), now);
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

/// Two devices of one person linked directly: `a` hosts the workspace and
/// keeps its store; `b` joined it, and keeps a store of its own.
struct Hosted {
    a: DirectSync,
    a_cert: tilo_user_identity::DeviceCert,
    /// The keys the workspace's members seal its records under.
    keys: key_group::ContentKeys,
    b: DirectSync,
    /// The workspace as B joined it, holding nothing of its records: what a
    /// B that kept nothing starts from.
    b_joined: StatePeer,
    a_key: SigningKey,
    b_key: SigningKey,
    store: Arc<tilo_peer_state::store::Store>,
    /// The stores' directories, kept for the test's duration.
    _a_dir: tempfile::TempDir,
    _b_dir: tempfile::TempDir,
    host_endpoint: iroh::Endpoint,
    b_endpoint: iroh::Endpoint,
    router: iroh::protocol::Router,
    link: tokio::task::JoinHandle<Result<(), tilo_server_protocol::refusal::Refusal>>,
}

impl Hosted {
    /// A founds the workspace and writes eight changes to its record; B joins
    /// holding nothing, and takes them in over its first link.
    async fn start() -> Self {
        let root = SigningKey::from_bytes(&[7; 32]);
        let (a_key, a_cert) = certify(&root, 20, DeviceCapabilities::member());
        let (b_key, b_cert) = certify(&root, 21, DeviceCapabilities::member());
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
        tilo_peer_state::fixture::found(&mut a, (&a_key, &a_cert), &roster, Vec::new(), now);
        let b = StatePeer::from_join(WORKSPACE, a_cert.clone(), roster, &[], now).unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            tilo_peer_state::store::Store::open(
                store_dir.path(),
                tilo_peer_state::store::Appetite::StoreAll,
            )
            .unwrap(),
        );
        let a = DirectSync::new(a, a_key.clone()).with_store(Arc::clone(&store));
        let keys = members_keys(WORKSPACE);
        let person = a_cert.user_public_key;
        for round in 0..8 {
            seed(&a, &keys, &a_key, &person, &format!("from a {round}"));
        }
        let b_dir = tempfile::tempdir().unwrap();
        let b_joined = b.clone();
        let b = stored(&DirectSync::new(b, b_key.clone()), &b_dir);
        let host_endpoint = localhost_endpoint(&a_key).await;
        let host_router = iroh::protocol::Router::builder(host_endpoint.clone())
            .accept(DIRECT_ALPN, a.clone())
            .spawn();
        let b_endpoint = localhost_endpoint(&b_key).await;
        let link = tokio::spawn({
            let (b, b_endpoint, a_addr) = (b.clone(), b_endpoint.clone(), addr(&host_endpoint));
            async move { b.connect(&b_endpoint, a_addr).await }
        });
        let written = heads(&a, &keys);
        until(&b, "B holds what A wrote", |b| {
            opened_heads(b, &keys, (APP, RECORD)) == written
        })
        .await;
        Self {
            a,
            a_cert,
            keys,
            b,
            b_joined,
            a_key,
            b_key,
            store,
            _a_dir: store_dir,
            _b_dir: b_dir,
            host_endpoint,
            b_endpoint,
            router: host_router,
            link,
        }
    }

    /// Link `device` to the host again, answering what the link brought.
    fn again(
        &self,
        device: &DirectSync,
    ) -> tokio::task::JoinHandle<Result<(), tilo_server_protocol::refusal::Refusal>> {
        tokio::spawn({
            let (device, b_endpoint, a_addr) = (
                device.clone(),
                self.b_endpoint.clone(),
                addr(&self.host_endpoint),
            );
            async move { device.connect(&b_endpoint, a_addr).await }
        })
    }

    async fn stop(self) {
        self.link.abort();
        self.router.shutdown().await.unwrap();
        self.b_endpoint.close().await;
    }
}

/// Every frame `reader` takes in over `window`, by its kind, with each kind's
/// count and bytes.
async fn taken_in(
    mut reader: tilo_peer_state::direct::Reader,
    window: Duration,
) -> std::collections::BTreeMap<&'static str, (usize, usize)> {
    let mut taken: std::collections::BTreeMap<&'static str, (usize, usize)> =
        std::collections::BTreeMap::new();
    let end = tokio::time::Instant::now().checked_add(window).unwrap();
    while let Ok(Some(heard)) = tokio::time::timeout_at(end, reader.recv()).await {
        let bytes = postcard::to_allocvec(&heard.message).map_or(0, |bytes| bytes.len());
        let entry = taken.entry(heard.message.kind()).or_default();
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry.1.saturating_add(bytes);
    }
    taken
}

/// A device that links to its host again after one new change is sent that
/// change alone: the workspace's history and state it already holds are not
/// sent again.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_links_again_is_sent_only_what_it_lacks() {
    let hosted = Box::pin(Hosted::start()).await;
    let (keys, b) = (&hosted.keys, &hosted.b);
    hosted.link.abort();

    // One change, made while the link was down, and the device links again.
    let person = hosted.a.read(StatePeer::owner);
    seed(&hosted.a, keys, &hosted.a_key, &person, "after the break");
    let expected = heads(&hosted.a, keys);
    let before = b.traffic();
    let reader = b.reader();
    let again = hosted.again(b);
    until(b, "B holds the new change", |b| {
        opened_heads(b, keys, (APP, RECORD)) == expected
    })
    .await;
    let taken = taken_in(reader, Duration::from_millis(500)).await;
    let received = b.traffic().received - before.received;
    again.abort();
    hosted.stop().await;

    // The one change crosses, and nothing else of the records: not the eight
    // changes the device already has, and no checkpoint of them.
    assert_eq!(
        taken.get("AppDocument").copied().unwrap_or_default().0,
        1,
        "the device is sent the one change it lacks: {taken:?}"
    );
    // Its certificate, the change, and the link's own frames: the state the
    // device holds is not sent again.
    assert!(
        received < 1600,
        "a reconnect after one change carries that change alone, {received} bytes taken in: {taken:?}"
    );
}

/// A device that holds nothing of the workspace is sent all of it: every
/// record's history and the workspace's whole state.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_holds_nothing_is_sent_everything() {
    let hosted = Box::pin(Hosted::start()).await;
    let keys = &hosted.keys;
    hosted.link.abort();
    let expected = heads(&hosted.a, keys);

    // A second device of the same person joins holding nothing, and links.
    let joined = StatePeer::from_join(
        WORKSPACE,
        hosted.a_cert.clone(),
        hosted.a.read(tilo_peer_state::StatePeer::certificates),
        &[],
        SystemTime::now(),
    )
    .unwrap();
    let fresh = DirectSync::new(joined, hosted.b_key.clone());
    let before = fresh.traffic();
    let reader = fresh.reader();
    let again = hosted.again(&fresh);
    until(&fresh, "the fresh device holds what A wrote", |fresh| {
        opened_heads(fresh, keys, (APP, RECORD)) == expected
    })
    .await;
    let taken = taken_in(reader, Duration::from_millis(500)).await;
    let received = fresh.traffic().received - before.received;
    again.abort();
    // The device that only caught up earlier was sent one change, and fewer
    // bytes than the whole history this one takes in.
    assert_eq!(value(&fresh, keys), "from a 7");
    assert_eq!(
        taken.get("AppDocument").copied().unwrap_or_default().0,
        8,
        "every change the device holds nothing of is sent: {taken:?}"
    );
    assert!(
        received > 8 * 890,
        "a device that holds nothing takes in every change, {received} bytes: {taken:?}"
    );
    hosted.stop().await;
}

/// A device whose history the host compacted converges still: the host keeps
/// what that device told it it holds, and sends the changes after it.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_behind_a_compacted_history_converges() {
    let hosted = Box::pin(Hosted::start()).await;
    let (keys, b) = (&hosted.keys, &hosted.b);
    hosted.link.abort();
    let workspace = tilo_server_protocol::WorkspaceId::parse(WORKSPACE).unwrap();
    let person = hosted.a.read(StatePeer::owner);

    // The device says where it stands, as a link does, and the host's store
    // keeps only that: the changes it no longer needs are retired.
    hosted
        .store
        .holds(
            workspace,
            hosted.b_key.verifying_key().to_bytes(),
            &b.cursors(),
        )
        .unwrap();
    let retired = hosted
        .store
        .compact(workspace, tilo_clock::unix_now_ns())
        .unwrap();
    assert!(
        retired.frames > 0,
        "the device's history is compacted past what the link last sent"
    );
    seed(&hosted.a, keys, &hosted.a_key, &person, "after compaction");
    let expected = heads(&hosted.a, keys);
    let reader = b.reader();
    let again = hosted.again(b);
    until(b, "B holds the change after the compaction", |b| {
        opened_heads(b, keys, (APP, RECORD)) == expected
    })
    .await;
    let taken = taken_in(reader, Duration::from_millis(500)).await;
    again.abort();
    let converged = value(b, keys);
    hosted.stop().await;
    assert_eq!(converged, "after compaction");
    assert_eq!(
        taken.get("AppDocument").copied().unwrap_or_default().0,
        1,
        "the compacted history is not sent again: {taken:?}"
    );
}

/// A device that keeps nothing of the record, whose history the host's store
/// compacted past with no checkpoint to lead it, is sent the record's whole
/// state: the part the store still kept would leave the record half-synced.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_behind_a_compaction_with_no_checkpoint_is_sent_the_records_state() {
    let hosted = Box::pin(Hosted::start()).await;
    let keys = &hosted.keys;
    hosted.link.abort();
    let workspace = tilo_server_protocol::WorkspaceId::parse(WORKSPACE).unwrap();
    let person = hosted.a.read(StatePeer::owner);

    // The host's store is told where the device stood, and retires every
    // change it kept of the record: the record has no checkpoint to keep.
    hosted
        .store
        .holds(
            workspace,
            hosted.b_key.verifying_key().to_bytes(),
            &hosted.b.cursors(),
        )
        .unwrap();
    let retired = hosted
        .store
        .compact(workspace, tilo_clock::unix_now_ns())
        .unwrap();
    assert_eq!(
        retired.frames, 7,
        "every change after the record's first checkpoint is retired"
    );

    // The device starts again holding nothing of the record, and the host
    // writes one change the store keeps: the store now holds the tail alone.
    let fresh = DirectSync::new(hosted.b_joined.clone(), hosted.b_key.clone());
    assert!(fresh.read(StatePeer::rows).is_empty(), "it keeps no record");
    seed(&hosted.a, keys, &hosted.a_key, &person, "after everything");
    assert_eq!(
        hosted.store.stands(workspace).len(),
        1,
        "the store holds the change after the retired ones alone"
    );
    let expected = heads(&hosted.a, keys);
    let reader = fresh.reader();
    let again = hosted.again(&fresh);
    until(&fresh, "the record's whole state is sent", |fresh| {
        opened_heads(fresh, keys, (APP, RECORD)) == expected
    })
    .await;
    let taken = taken_in(reader, Duration::from_millis(500)).await;
    again.abort();
    let converged = value(&fresh, keys);
    hosted.stop().await;
    assert_eq!(converged, "after everything");
    assert_eq!(
        taken.get("AppDocument").copied().unwrap_or_default().0,
        9,
        "the whole record goes, not the part the store kept: {taken:?}"
    );
}
