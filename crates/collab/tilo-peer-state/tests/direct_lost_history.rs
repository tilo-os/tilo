//! A device that holds a change it cannot apply converges: the frame it builds
//! on came later or never, and the device asks the one that sent the frame for
//! the state it holds, over the same link. Each test has the sending device
//! make a change it never sends, then send one built on it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common::{addr, device, localhost_endpoint};

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use std::time::Duration;
use tilo_peer_state::direct::{DIRECT_ALPN, DirectSync};
use tilo_peer_state::{SeedDocument, SeedState, StatePeer, seed_workspace};
use tilo_user_identity::RevocationList;
use web_time::SystemTime;

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
    b: DirectSync,
    link: tokio::task::JoinHandle<Result<(), String>>,
    router: iroh::protocol::Router,
    b_endpoint: iroh::Endpoint,
}

impl Linked {
    async fn start() -> Self {
        let root = SigningKey::from_bytes(&[0x31; 32]);
        let (a_key, a_cert) = device(&root, 0x32);
        let (b_key, b_cert) = device(&root, 0x33);
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
        a.select_admission_v5().unwrap();
        let mut document = AutoCommit::new();
        document.put(ROOT, "value", "first").unwrap();
        document.commit_edit("fixture", "value");
        a.install_seed(
            SeedState {
                workspace_changes: seed_workspace(WORKSPACE, &a_cert, &devices).unwrap(),
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
        let b = StatePeer::from_join(WORKSPACE, a_cert, devices, &[], now).unwrap();
        let a = DirectSync::new(a, a_key.clone());
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
        let heads = a.read(|a| a.document_heads(APP, RECORD));
        until(&b, |b| b.document_heads(APP, RECORD) == heads).await;
        Self {
            a,
            a_key,
            b,
            link,
            router,
            b_endpoint,
        }
    }

    async fn stop(self) {
        self.link.abort();
        self.router.shutdown().await.unwrap();
        self.b_endpoint.close().await;
    }
}

/// The record's change setting `value`, made on `a`'s copy as `key`'s device.
fn record_change(a: &DirectSync, key: &SigningKey, value: &str) -> Vec<u8> {
    let mut edit = AutoCommit::load(&a.read(|a| a.document(APP, RECORD)).unwrap()).unwrap();
    edit.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = edit.get_heads();
    edit.put(ROOT, "value", value).unwrap();
    edit.commit_edit("fixture", "value");
    edit.save_after(&heads)
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
        room: WORKSPACE.into(),
    })
    .unwrap();

    // What `a` answers with is its state as it stands then, its governance
    // moved by the checkpoints it signs for the answer: `b` holds the child,
    // and what `a` holds.
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
    let (a, key) = (&linked.a, &linked.a_key);

    // The earlier write: made on `a`, never sent.
    let earlier = record_change(a, key, "kept back");
    a.replica().write(|a| {
        a.publication_mut()
            .publish_incremental(APP, RECORD, earlier, key, SystemTime::now())
            .unwrap();
    });
    // The later one, built on it, is all `b` hears.
    let later = record_change(a, key, "sent");
    a.publish(|a| {
        a.publication_mut()
            .publish_incremental(APP, RECORD, later, key, SystemTime::now())
    })
    .unwrap();

    let heads = a.read(|a| a.document_heads(APP, RECORD));
    until(&linked.b, |b| b.document_heads(APP, RECORD) == heads).await;
    let on_b = AutoCommit::load(&linked.b.read(|b| b.document(APP, RECORD)).unwrap()).unwrap();
    assert_eq!(
        on_b.get(ROOT, "value")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .unwrap(),
        "sent"
    );
    linked.stop().await;
}
