//! A device that holds a change it cannot apply converges: the frame it builds
//! on came later or never, and the device asks the one that sent the frame for
//! the state it holds, over the same link. Each test has the sending device
//! make a change it never sends, then send one built on it.

use crate::common::{
    addr, localhost_endpoint, members_keys, opened, opened_heads, sealed_write, write_sealed,
};

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use std::sync::Arc;
use std::time::Duration;
use tilo_peer_state::StatePeer;
use tilo_peer_state::direct::{DIRECT_ALPN, DirectSync};
use tilo_peer_state::store::{Appetite, Frame, Store};
use tilo_user_identity::{DeviceCapabilities, RevocationList};
use web_time::SystemTime;

use tilo_peer_state::fixture::certify;
const WORKSPACE: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";

/// Wait until `done` holds of `sync`'s replica.
async fn until(sync: &DirectSync, done: impl Fn(&StatePeer) -> bool + Sync) {
    let mut changes = sync.changes();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !sync.read(&done) {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("the device converges");
}

/// Two devices of one person linked directly: `a` made the workspace and its
/// record, and `b` has taken in all of it. Answers both, `a`'s key, and what
/// keeps the link up.
struct Linked {
    a: DirectSync,
    a_key: SigningKey,
    /// The keys the workspace's members seal its records under.
    keys: key_group::ContentKeys,
    b: DirectSync,
    b_key: SigningKey,
    /// The workspace as `b` held it when it joined, before the link brought it anything.
    joined: StatePeer,
    link: tokio::task::JoinHandle<Result<(), tilo_server_protocol::refusal::Refusal>>,
    router: iroh::protocol::Router,
    host_endpoint: iroh::Endpoint,
    b_endpoint: iroh::Endpoint,
    /// The writer's one store.
    store: (tempfile::TempDir, Arc<Store>),
}

impl Linked {
    async fn start() -> Self {
        let root = SigningKey::from_bytes(&[0x31; 32]);
        let (a_key, a_cert) = certify(&root, 0x32, DeviceCapabilities::member());
        let (b_key, b_cert) = certify(&root, 0x33, DeviceCapabilities::member());
        let devices = vec![a_cert.clone(), b_cert];
        let now = SystemTime::now();
        let mut a = StatePeer::new(
            WORKSPACE,
            a_cert.clone(),
            devices.clone(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        a.select_exact_admissions().unwrap();
        tilo_peer_state::fixture::found(&mut a, (&a_key, &a_cert), &devices, Vec::new(), now);
        let b = StatePeer::from_join(WORKSPACE, a_cert.clone(), devices, &[], now).unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(store_dir.path(), Appetite::StoreAll).unwrap());
        let a = DirectSync::new(a, a_key.clone()).with_store(Arc::clone(&store));
        // The record's first write, kept in the writer's store.
        let keys = members_keys(WORKSPACE);
        write_sealed(&a, &keys, &a_key, (APP, RECORD), |record| {
            set(record, "first", &a_cert.user_public_key);
        });
        let joined = b.clone();
        let b = DirectSync::new(b, b_key.clone());
        let host_endpoint = localhost_endpoint(&a_key).await;
        let router = iroh::protocol::Router::builder(host_endpoint.clone())
            .accept(DIRECT_ALPN, a.clone())
            .spawn();
        let b_endpoint = localhost_endpoint(&b_key).await;
        let link = tokio::spawn({
            let b = b.clone();
            let b_endpoint = b_endpoint.clone();
            let a_addr = addr(&host_endpoint);
            async move { b.connect(&b_endpoint, a_addr).await }
        });
        let heads = a.read(|a| opened_heads(a, &keys, (APP, RECORD)));
        until(&b, |b| opened_heads(b, &keys, (APP, RECORD)) == heads).await;
        Self {
            a,
            a_key,
            keys,
            b,
            b_key,
            joined,
            link,
            router,
            host_endpoint,
            b_endpoint,
            store: (store_dir, store),
        }
    }

    async fn stop(self) {
        self.link.abort();
        self.router.shutdown().await.unwrap();
        self.b_endpoint.close().await;
    }
}

/// `record`'s `value` set to `value`, as `person` made it.
fn set(record: &mut AutoCommit, value: &str, person: &[u8; 32]) {
    record.put(ROOT, "value", value).unwrap();
    record.commit_edit("fixture", "value", person);
}

/// The record's `value` as `peer` reads it with `keys`.
fn value(peer: &StatePeer, keys: &key_group::ContentKeys) -> String {
    AutoCommit::load(&opened(peer, keys, (APP, RECORD)).unwrap())
        .unwrap()
        .get(ROOT, "value")
        .unwrap()
        .unwrap()
        .0
        .into_string()
        .unwrap()
}

/// A workspace change sent without the change it builds on: the device it
/// reaches holds it, unapplied, until it has asked for what the sender holds.
#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_change_sent_without_its_parent_is_applied() {
    let linked = Box::pin(Linked::start()).await;
    let (a, key) = (&linked.a, &linked.a_key);
    let now = SystemTime::now();

    // The parent: made on `a`, never sent.
    a.replica().write(|a| {
        a.governance_mut()
            .author_workspace_name("Kept back", key, now)
            .unwrap();
    });
    let parent = a.read(StatePeer::workspace_heads).unwrap();
    // The child, built on it, is all `b` hears.
    a.replica().write(|a| {
        a.governance_mut()
            .author_workspace_name("Sent", key, now)
            .unwrap();
    });
    let changes = a.read(|a| a.workspace_changes_since(&parent));
    assert!(!changes.is_empty(), "the child is sent");
    a.send(NeighbourMsg::SyncResp {
        changes,
        workspace: WORKSPACE.into(),
    })
    .unwrap();

    // What `a` answers with is its governance as it stands then: `b` holds
    // the child, and what `a` holds.
    until(&linked.b, |b| {
        b.governance().workspace_name().as_deref() == Some("Sent")
    })
    .await;
    let heads = a.read(StatePeer::workspace_heads);
    until(&linked.b, |b| b.workspace_heads() == heads).await;
    linked.stop().await;
}

/// A record write sent without the write it builds on: the device it reaches
/// holds it, waiting for the changes it builds on, until it has asked for what
/// the sender holds.
#[tokio::test(flavor = "multi_thread")]
async fn a_record_write_sent_without_the_one_it_builds_on_is_applied() {
    let linked = Box::pin(Linked::start()).await;
    let (a, key, keys) = (&linked.a, &linked.a_key, &linked.keys);

    // The earlier write: made on `a` and kept in its store, never sent.
    let person = a.read(StatePeer::owner);
    let earlier = a.replica().write(|a| {
        sealed_write(a, keys, key, (APP, RECORD), |record| {
            set(record, "kept back", &person);
        })
        .unwrap()
    });
    linked
        .store
        .1
        .retain(
            tilo_server_protocol::WorkspaceId::parse(WORKSPACE).unwrap(),
            &Frame::of(&earlier).unwrap(),
        )
        .unwrap();
    // The later one, built on it, is all `b` hears.
    write_sealed(a, keys, key, (APP, RECORD), |record| {
        set(record, "sent", &person);
    });

    let heads = a.read(|a| opened_heads(a, keys, (APP, RECORD)));
    until(&linked.b, |b| opened_heads(b, keys, (APP, RECORD)) == heads).await;
    assert_eq!(linked.b.read(|b| value(b, keys)), "sent");
    linked.stop().await;
}

/// A write made while the other device was away reaches it when it links
/// again: the writer's one store keeps it, and the link catches up from
/// there rather than from anything the two held when they last met.
#[tokio::test(flavor = "multi_thread")]
async fn a_write_made_while_the_other_device_was_away_reaches_it_when_it_links_again() {
    let linked = Box::pin(Linked::start()).await;
    let (a, key, keys) = (&linked.a, &linked.a_key, &linked.keys);
    let person = a.read(StatePeer::owner);

    // The other device goes away.
    linked.link.abort();
    // The writer changes the record with no link open: its store keeps it.
    write_sealed(a, keys, key, (APP, RECORD), |record| {
        set(record, "while away", &person);
    });
    let heads = a.read(|a| opened_heads(a, keys, (APP, RECORD)));

    // It comes back and catches up from the store the writer kept it in.
    let again = tokio::spawn({
        let b = linked.b.clone();
        let b_endpoint = linked.b_endpoint.clone();
        let host_endpoint = linked.host_endpoint.clone();
        async move { b.connect(&b_endpoint, addr(&host_endpoint)).await }
    });
    until(&linked.b, |b| opened_heads(b, keys, (APP, RECORD)) == heads).await;
    assert_eq!(linked.b.read(|b| value(b, keys)), "while away");
    again.abort();
    linked.stop().await;
}

/// A device that lost its store gets its records back from the host that keeps them: a member
/// writes a record, the host keeps the write, and the member's store is deleted. Started again
/// from nothing but the workspace it joined, the member catches up from the host and reads the
/// record's value again.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_lost_its_store_gets_its_records_back_from_a_host() {
    let linked = Box::pin(Linked::start()).await;
    let (keys, b_key) = (&linked.keys, &linked.b_key);
    linked.link.abort();
    let member_dir = tempfile::tempdir().unwrap();
    let store_dir = member_dir.path().join("store");
    let start = |joined: StatePeer| {
        let store = Arc::new(Store::open(&store_dir, Appetite::StoreAll).unwrap());
        let member = DirectSync::new(joined, b_key.clone()).with_store(store);
        let link = tokio::spawn({
            let member = member.clone();
            let b_endpoint = linked.b_endpoint.clone();
            let host = addr(&linked.host_endpoint);
            async move { member.connect(&b_endpoint, host).await }
        });
        (member, link)
    };

    // The member, keeping a store of its own, writes the record; the host keeps the write.
    let (member, link) = start(linked.joined.clone());
    until(&member, |member| {
        opened(member, keys, (APP, RECORD)).is_some()
    })
    .await;
    let person = member.read(StatePeer::owner);
    write_sealed(&member, keys, b_key, (APP, RECORD), |record| {
        set(record, "the member's", &person);
    });
    until(&linked.a, |host| value(host, keys) == "the member's").await;

    // The member's store is gone, and the member starts again from what it joined with.
    link.abort();
    drop(member);
    std::fs::remove_dir_all(&store_dir).unwrap();
    let (member, link) = start(linked.joined.clone());
    until(&member, |member| {
        opened(member, keys, (APP, RECORD)).is_some_and(|_| value(member, keys) == "the member's")
    })
    .await;
    assert_eq!(member.read(|member| value(member, keys)), "the member's");
    link.abort();
    linked.stop().await;
}
