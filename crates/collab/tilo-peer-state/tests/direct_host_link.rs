//! The host serves its workspaces on the same direct link a device runs: a
//! member syncs a document with it, a late joiner catches up from the history
//! the host retains, and a device the workspace's root revoked is refused.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
tilo_automerge_isolate::test_worker!();

mod common;

use common::{addr, device, localhost_endpoint};

use automerge::{AutoCommit, ROOT, transaction::Transactable};
use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
use collab_types::CommitNamed as _;
use collab_types::{
    ChannelId, ContentRetention, Ed25519PublicKey, OpKind, SignedOp, document_artifact_id,
    document_attestation_id, document_channel_id, sign_op,
};
use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use neighbour_wire::{NeighbourMsg, peer_handle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tilo_peer_state::direct::{DIRECT_ALPN, DirectSync, Serving};
use tilo_peer_state::store::{Appetite, Budget, Store};
use tilo_peer_state::{SeedDocument, SeedState, StatePeer, seed_workspace};
use tilo_user_identity::{
    DeviceCapabilities, RevocationEntry, RevocationList, RevocationReason, sign_revocation_entry,
};
use web_time::SystemTime;

const WORKSPACE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";

fn workspace_number() -> u64 {
    tilo_relay_protocol::room_workspace_id(WORKSPACE).unwrap()
}

/// Write `value` into the workspace's record as `key`'s device.
fn edit(peer: &DirectSync, key: &SigningKey, value: &str) {
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
}

/// Wait until `done` holds of `sync`'s replica.
async fn until(sync: &DirectSync, step: &str, done: impl Fn(&StatePeer) -> bool + Sync) {
    let mut changes = sync.changes();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !sync.read(&done) {
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the replicas converge: {step}"));
}

/// One edit of `key`, sealed under the record's content keys: what a store
/// keeps and only a key holder reads.
fn sealed(key: &SigningKey, keys: &ChannelContentKeys, text: &str) -> NeighbourMsg {
    // The plaintext a sealed edit carries: an incremental of the record.
    let mut edit = AutoCommit::new();
    edit.put(ROOT, "value", text).unwrap();
    edit.commit_edit("fixture", "value");
    let plaintext = tilo_peer_state::DocumentMutation::Incremental(edit.save_incremental())
        .encode()
        .unwrap();
    let (metadata, payload) = keys
        .seal_content(
            document_artifact_id(APP, WORKSPACE, RECORD),
            ContentRetention::Delta,
            &plaintext,
        )
        .unwrap();
    let mut op = SignedOp::new_unsigned(
        payload,
        document_artifact_id(APP, WORKSPACE, RECORD),
        Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
        document_attestation_id(APP, WORKSPACE, RECORD),
        OpKind::Change,
        web_time::SystemTime::now(),
        1,
    );
    op.encryption = Some(metadata);
    sign_op(&mut op, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: peer_handle(&key.verifying_key().to_bytes()),
        app_id: APP.into(),
        cluster_id: WORKSPACE.into(),
        record_id: RECORD.into(),
        envelope: Box::new(op),
    }
}

fn files(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, found);
        } else {
            found.push(path);
        }
    }
}

fn channel() -> ChannelId {
    document_channel_id(APP, WORKSPACE, RECORD)
}

#[expect(
    clippy::too_many_lines,
    reason = "one member's sync with the host, then a late joiner catching up from it"
)]
#[tokio::test(flavor = "multi_thread")]
async fn a_member_syncs_with_the_host_and_a_late_joiner_catches_up_from_its_store() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let (host_key, host_cert) = common::device_with(&root, 2, DeviceCapabilities::host());
    let (a_key, a_cert) = device(&root, 3);
    let (b_key, b_cert) = device(&root, 4);
    let (c_key, c_cert) = device(&root, 5);
    let now = SystemTime::now();
    let devices = vec![
        host_cert.clone(),
        a_cert.clone(),
        b_cert.clone(),
        c_cert.clone(),
    ];

    let mut govern = StatePeer::new(
        WORKSPACE,
        host_cert.clone(),
        devices.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    govern.select_admission_v5().unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "from the host").unwrap();
    document.commit_edit("fixture", "value");
    govern
        .install_seed(
            SeedState {
                workspace_changes: seed_workspace(WORKSPACE, &host_cert, &devices).unwrap(),
                documents: vec![SeedDocument {
                    app_id: APP.into(),
                    record_id: RECORD.into(),
                    kind: "test".into(),
                    bytes: document.save(),
                }],
            },
            &host_key,
            now,
        )
        .unwrap();
    // The workspace's root revoked C's device: C is in the workspace's roster
    // and may not reach it.
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: c_key.verifying_key().to_bytes(),
        revoked_at_ns: 1,
        reason: RevocationReason::Lost,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &root).unwrap();
    assert!(
        govern
            .governance_mut()
            .publish_governed_revocations(&[entry], &host_key, now)
            .unwrap()
    );
    let scratch = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(scratch.path(), Appetite::StoreAll).unwrap());
    let host = DirectSync::over(
        tilo_peer_state::direct::Replica::new(govern),
        host_key.clone(),
    )
    .with_store(Arc::clone(&store));
    let serving = Serving::new();
    serving.serve(host.clone()).unwrap();
    let host_endpoint = localhost_endpoint(&host_key).await;
    let router = Router::builder(host_endpoint.clone())
        .accept(DIRECT_ALPN, serving.clone())
        .spawn();

    // A member device dials the host and syncs the workspace with it.
    let a = StatePeer::from_join(WORKSPACE, host_cert.clone(), devices.clone(), &[], now).unwrap();
    let a = DirectSync::new(a, a_key.clone());
    let a_endpoint = localhost_endpoint(&a_key).await;
    let a_link = tokio::spawn({
        let a = a.clone();
        let a_endpoint = a_endpoint.clone();
        let host_addr = addr(&host_endpoint);
        async move { a.connect(&a_endpoint, host_addr).await }
    });
    let host_rows = host.read(StatePeer::rows);
    until(&a, "a has the host's rows", |a| a.rows() == host_rows).await;

    // What the member writes, the host takes in.
    edit(&a, &a_key, "from the member");
    let edited = a.read(|a| a.document_heads(APP, RECORD));
    until(&host, "the host has the member's edit", |host| {
        host.document_heads(APP, RECORD) == edited
    })
    .await;

    // A sealed edit reaches the host, which keeps it as the history a member
    // that was away catches up from; the host holds no channel key.
    let keys = ChannelGroup::create(channel(), MemberKey::generate("owner").unwrap())
        .unwrap()
        .content_keys()
        .unwrap();
    let kept = sealed(&a_key, &keys, "member history");
    a.publish(|_, _, _| Ok(kept.clone())).unwrap();
    let retained = || {
        store
            .replay(workspace_number(), &[], Budget::default())
            .unwrap()
    };
    tokio::time::timeout(Duration::from_secs(30), async {
        while retained().frames.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the host retains the member's sealed edit");
    assert_eq!(retained().frames, vec![kept.clone()]);
    // What the host wrote is ciphertext: neither the edit's plaintext nor a
    // readable copy of it is on disk, one file per frame.
    let mut kept_files = Vec::new();
    files(scratch.path(), &mut kept_files);
    assert_eq!(kept_files.len(), 1, "a file per frame");
    for file in &kept_files {
        let bytes = std::fs::read(file).unwrap();
        assert!(
            !bytes
                .windows(b"member history".len())
                .any(|window| window == b"member history"),
            "{} carries plaintext",
            file.display()
        );
    }
    assert!(
        host.read(|host| host.document(APP, RECORD)).is_some(),
        "the host holds the workspace's record"
    );

    // The member goes away; a late joiner of the workspace dials the host
    // alone and catches up from what the host kept.
    a_link.abort();
    let b = StatePeer::from_join(WORKSPACE, host_cert.clone(), devices.clone(), &[], now).unwrap();
    assert!(b.records().is_empty());
    let b = DirectSync::new(b, b_key.clone());
    let b_endpoint = localhost_endpoint(&b_key).await;
    let b_link = tokio::spawn({
        let b = b.clone();
        let b_endpoint = b_endpoint.clone();
        let host_addr = addr(&host_endpoint);
        async move { b.connect(&b_endpoint, host_addr).await }
    });
    let host_rows = host.read(StatePeer::rows);
    until(&b, "b has the rows", |b| b.rows() == host_rows).await;
    until(&b, "b has the edit", |b| {
        b.document_heads(APP, RECORD) == edited
    })
    .await;
    assert!(
        store
            .replay(workspace_number(), &b.cursors(), Budget::default())
            .unwrap()
            .frames
            .is_empty(),
        "a caught-up member is handed nothing"
    );

    // A revoked device is refused, and its replica gains nothing.
    let c = StatePeer::from_join(WORKSPACE, host_cert.clone(), devices.clone(), &[], now).unwrap();
    let c = DirectSync::new(c, c_key.clone());
    let c_endpoint = localhost_endpoint(&c_key).await;
    let refused = c.connect(&c_endpoint, addr(&host_endpoint)).await;
    assert!(refused.is_err(), "a revoked device is refused");
    assert!(c.read(|c| c.rows().is_empty()));

    b_link.abort();
    router.shutdown().await.unwrap();
    host_endpoint.close().await;
    a_endpoint.close().await;
    b_endpoint.close().await;
    c_endpoint.close().await;
}
