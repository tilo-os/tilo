//! A host that ends one device's links leaves the workspace's other links up:
//! the ended device is refused and sent nothing more, and the others keep
//! receiving what the workspace writes.

use crate::common::{
    addr, device, localhost_endpoint, members_keys, opened, opened_heads, write_sealed,
};
use key_group::ContentKeys;

use automerge::transaction::CommitOptions;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use std::sync::Arc;
use std::time::Duration;
use tilo_actions::{ActionContext, ActionInvoker, Did, EditOp, FieldEdit, PathStep};
use tilo_peer_state::direct::{DIRECT_ALPN, DirectSync, Replica, Serving};
use tilo_peer_state::store::{Appetite, Store};
use tilo_peer_state::{SeedState, StatePeer, seed_workspace};
use tilo_server_protocol::refusal::Refusal;
use tilo_user_identity::{DeviceCert, RevocationList};
use web_time::SystemTime;

const WORKSPACE: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbb0022";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";

/// The invoker a change names: the person whose device made it. A fixture
/// commit names no device, since a change that does carries the invocation
/// that device signed.
fn invoker(cert: &DeviceCert) -> ActionInvoker {
    ActionInvoker::person(hex::encode(cert.user_public_key))
}

/// `field` of `record` set to `value`, made for `invoker`.
fn set(record: &mut AutoCommit, field: &str, value: &str, invoker: ActionInvoker) {
    record.put(ROOT, field, value).unwrap();
    let context = ActionContext {
        what: Some(Did::Edit(FieldEdit {
            ty: "fixture".into(),
            field: field.into(),
            path: vec![PathStep::Field(field.into())],
            op: EditOp::Set,
            old: None,
            new: None,
            args: std::collections::BTreeMap::new(),
        })),
        invoker: Some(invoker),
        ..ActionContext::for_resource("fixture", "fixture", "Fixture")
    };
    let mut options = CommitOptions::default();
    if let Some(message) = context.change_message() {
        options.set_message(message);
    }
    record.commit_with(options);
}

/// The record's heads in `sync`'s replica, read with `keys`.
fn heads(sync: &DirectSync, keys: &ContentKeys) -> Option<Vec<automerge::ChangeHash>> {
    sync.read(|peer| opened_heads(peer, keys, (APP, RECORD)))
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
    .unwrap_or_else(|_| panic!("{what}"));
}

/// A member's link to `host`, dialled and caught up, kept until the test ends.
struct Member {
    sync: DirectSync,
    endpoint: iroh::Endpoint,
    dialing: tokio::task::JoinHandle<Result<(), Refusal>>,
}

impl Member {
    /// Link `sync` to the host at `target` and wait for it to catch up.
    async fn dial(sync: DirectSync, key: &SigningKey, target: iroh::EndpointAddr) -> Self {
        let endpoint = localhost_endpoint(key).await;
        let mut caught_up = sync.caught_up();
        let dialing = tokio::spawn({
            let sync = sync.clone();
            let endpoint = endpoint.clone();
            async move { sync.connect(&endpoint, target).await }
        });
        tokio::time::timeout(Duration::from_secs(30), caught_up.wait_for(|up| *up))
            .await
            .expect("the member catches up")
            .expect("the link is still up");
        Self {
            sync,
            endpoint,
            dialing,
        }
    }

    async fn stop(self) {
        self.dialing.abort();
        let _ = tokio::time::timeout(Duration::from_secs(10), self.endpoint.close()).await;
    }
}

/// The workspace's host, serving it with a store, and two members linked to
/// it and caught up to its record.
struct LinkedPair {
    host: DirectSync,
    host_key: SigningKey,
    host_cert: DeviceCert,
    /// The keys the workspace's members seal its records under.
    keys: ContentKeys,
    target: iroh::EndpointAddr,
    first: Member,
    second: Member,
    endpoint: iroh::Endpoint,
    router: iroh::protocol::Router,
    /// The store's directory, kept alive for the test.
    _directory: tempfile::TempDir,
}

impl LinkedPair {
    /// Serve the workspace from a host keeping its store, link two members to
    /// it, and wait for each to hold the record.
    async fn started() -> Self {
        let root = SigningKey::from_bytes(&[0x81; 32]);
        let (host_key, host_cert) = device(&root, 0x82);
        let (first_key, first_cert) = device(&root, 0x83);
        let (second_key, second_cert) = device(&root, 0x84);
        let members = vec![host_cert.clone(), first_cert, second_cert];
        let now = SystemTime::now();
        let mut peer = StatePeer::new(
            WORKSPACE,
            host_cert.clone(),
            members.clone(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        peer.select_exact_admissions().unwrap();
        peer.install_seed(
            SeedState {
                workspace_changes: seed_workspace(WORKSPACE, &host_cert, &members).unwrap(),
                documents: Vec::new(),
            },
            &host_key,
            now,
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(directory.path(), Appetite::StoreAll).unwrap());
        let host = DirectSync::over(Replica::new(peer), host_key.clone()).with_store(store);
        let keys = members_keys(WORKSPACE);
        write_sealed(&host, &keys, &host_key, (APP, RECORD), |record| {
            set(record, "value", "seed", invoker(&host_cert));
        });

        let serving = Serving::new();
        serving.serve(host.clone()).expect("the host serves");
        let endpoint = localhost_endpoint(&host_key).await;
        let router = iroh::protocol::Router::builder(endpoint.clone())
            .accept(DIRECT_ALPN, serving)
            .spawn();
        let target = addr(&endpoint);

        let first = Member::dial(
            DirectSync::new(
                StatePeer::from_join(WORKSPACE, host_cert.clone(), members.clone(), &[], now)
                    .unwrap(),
                first_key.clone(),
            ),
            &first_key,
            target.clone(),
        )
        .await;
        let second = Member::dial(
            DirectSync::new(
                StatePeer::from_join(WORKSPACE, host_cert.clone(), members, &[], now).unwrap(),
                second_key.clone(),
            ),
            &second_key,
            target.clone(),
        )
        .await;
        for (member, who) in [(&first, "the first member"), (&second, "the second member")] {
            until(&member.sync, &format!("{who} holds the record"), |peer| {
                opened(peer, &keys, (APP, RECORD)).is_some()
            })
            .await;
        }
        Self {
            host,
            host_key,
            host_cert,
            keys,
            target,
            first,
            second,
            endpoint,
            router,
            _directory: directory,
        }
    }

    async fn stop(self) {
        self.first.stop().await;
        self.second.stop().await;
        let _ = tokio::time::timeout(Duration::from_secs(10), self.router.shutdown()).await;
        let _ = tokio::time::timeout(Duration::from_secs(10), self.endpoint.close()).await;
    }
}

/// One device's link ends when the host ends it, and the workspace's other
/// link stays up: it keeps receiving what the host writes, the ended device
/// gets nothing more, and dialling again is refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_devices_link_ends_and_anothers_stays_up() {
    let mut pair = LinkedPair::started().await;

    // The host ends one device's link alone.
    let ended_device = pair.first.sync.device();
    pair.host
        .end_link(ended_device, Refusal::NotAMember("revoked".into()));
    let ended = tokio::time::timeout(Duration::from_secs(30), &mut pair.first.dialing)
        .await
        .expect("the ended device's link ends in time")
        .expect("the link's task ran");
    assert!(ended.is_err(), "the ended device's link ends: {ended:?}");
    assert!(
        pair.host.ended_link(ended_device).is_some(),
        "the host has the device's links ended"
    );

    // The other link stays up and is sent what the host writes after.
    let before = heads(&pair.first.sync, &pair.keys);
    write_sealed(
        &pair.host,
        &pair.keys,
        &pair.host_key,
        (APP, RECORD),
        |record| set(record, "after", "1", invoker(&pair.host_cert)),
    );
    let written = heads(&pair.host, &pair.keys);
    until(
        &pair.second.sync,
        "the second member holds the write",
        |peer| opened_heads(peer, &pair.keys, (APP, RECORD)) == written,
    )
    .await;
    assert_eq!(
        heads(&pair.first.sync, &pair.keys),
        before,
        "the ended device is sent nothing more"
    );

    // A device whose link the host ended is refused when it dials again.
    let again = tokio::time::timeout(
        Duration::from_secs(30),
        pair.first
            .sync
            .connect(&pair.first.endpoint, pair.target.clone()),
    )
    .await
    .expect("the refusal comes in time");
    assert!(
        again.is_err(),
        "a device whose link ended is refused when it dials again: {again:?}"
    );

    pair.stop().await;
}
