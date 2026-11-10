//! A write that reaches a member before the change it builds on waits, and is
//! taken in once that change comes, with nothing else asked of anyone: the
//! member's apps read it with the change that released it. A write the member
//! let go is read again from the history its host keeps once the member asks
//! for it on the link.
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
use collab_types::{
    ContentRetention, Ed25519PublicKey, OpKind, SignedOp, document_artifact_id,
    document_attestation_id, sign_op,
};
use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use neighbour_wire::{NeighbourMsg, peer_handle};
use std::sync::Arc;
use std::time::Duration;
use tilo_peer_state::direct::{DIRECT_ALPN, DirectSync, Heard, Serving};
use tilo_peer_state::store::{Appetite, Frame, Store};
use tilo_peer_state::{SeedDocument, SeedState, StatePeer, seed_workspace};
use tilo_user_identity::{DeviceCapabilities, RevocationList};
use web_time::SystemTime;

const WORKSPACE: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";

/// `author`'s write of `key = value` on the record as `author` holds it,
/// taken into `author` so the next write builds on it.
fn write(author: &mut StatePeer, signer: &SigningKey, key: &str, value: &str) -> NeighbourMsg {
    let mut edit = AutoCommit::load(&author.document(APP, RECORD).unwrap()).unwrap();
    edit.set_actor(automerge::ActorId::from(
        signer.verifying_key().to_bytes().to_vec(),
    ));
    let heads = edit.get_heads();
    edit.put(ROOT, key, value).unwrap();
    edit.commit_edit("fixture", key);
    author
        .publication_mut()
        .publish_incremental(
            APP,
            RECORD,
            edit.save_after(&heads),
            signer,
            SystemTime::now(),
        )
        .unwrap()
}

fn value(peer: &StatePeer, key: &str) -> Option<String> {
    let record = AutoCommit::load(&peer.document(APP, RECORD)?).ok()?;
    record
        .get(ROOT, key)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

/// The next frame `heard` brings that is `message`.
async fn hears(
    heard: &mut tokio::sync::broadcast::Receiver<Heard>,
    message: &NeighbourMsg,
    step: &str,
) -> Heard {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let frame = heard.recv().await.unwrap();
            if &frame.message == message {
                return frame;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the member hears {step}"))
}

/// One write of `key`'s device sealed under keys nobody here holds: what a
/// host keeps in the history its members catch up from.
fn sealed(key: &SigningKey) -> NeighbourMsg {
    let keys = channel_crypto::ChannelGroup::create(
        collab_types::document_channel_id(APP, WORKSPACE, RECORD),
        channel_crypto::MemberKey::generate("owner").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap();
    let mut edit = AutoCommit::new();
    edit.put(ROOT, "sealed", "sealed").unwrap();
    edit.commit_edit("fixture", "sealed");
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
        SystemTime::now(),
        // Past every write of the host's the member holds.
        100,
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

#[expect(
    clippy::too_many_lines,
    reason = "a workspace, its host and a member, then two orderings the member is sent"
)]
#[tokio::test(flavor = "multi_thread")]
async fn a_write_before_the_change_it_builds_on_merges_once_that_change_comes() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let (host_key, host_cert) = common::device_with(&root, 2, DeviceCapabilities::host());
    let (b_key, b_cert) = device(&root, 3);
    let now = SystemTime::now();
    let devices = vec![host_cert.clone(), b_cert.clone()];
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

    let b = StatePeer::from_join(WORKSPACE, host_cert.clone(), devices, &[], now).unwrap();
    let b = DirectSync::new(b, b_key.clone());
    let mut heard = b.heard();
    let mut caught_up = b.caught_up();
    let b_endpoint = localhost_endpoint(&b_key).await;
    let b_link = tokio::spawn({
        let b = b.clone();
        let b_endpoint = b_endpoint.clone();
        let host_addr = addr(&host_endpoint);
        async move { b.connect(&b_endpoint, host_addr).await }
    });
    tokio::time::timeout(Duration::from_secs(30), caught_up.wait_for(|up| *up))
        .await
        .expect("the member catches up")
        .unwrap();
    let held = host.read(|host| host.document_heads(APP, RECORD));
    let mut changes = b.changes();
    tokio::time::timeout(Duration::from_secs(30), async {
        while b.read(|b| b.document_heads(APP, RECORD)) != held {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("the member holds the record");

    // Two writes of the host's, the second building on the first, reach the
    // member the other way round.
    let mut author = host.read(Clone::clone);
    let first = write(&mut author, &host_key, "first", "1");
    let second = write(&mut author, &host_key, "second", "2");
    host.send(second.clone()).unwrap();
    let waited = hears(&mut heard, &second, "the second write").await;
    assert!(
        waited.report.as_ref().unwrap().deferred,
        "the second write waits for the first"
    );
    assert_eq!(b.read(|b| value(b, "second")), None);
    host.send(first.clone()).unwrap();
    let released = hears(&mut heard, &first, "the first write").await;
    // The first write takes the second in with it: nobody asked again.
    assert_eq!(b.read(|b| value(b, "first")).as_deref(), Some("1"));
    assert_eq!(
        b.read(|b| value(b, "second")).as_deref(),
        Some("2"),
        "the write that waited merges once what it built on came"
    );
    let report = released.report.unwrap();
    let taken: Vec<&NeighbourMsg> = report
        .released()
        .iter()
        .map(|released| &released.message)
        .collect();
    assert_eq!(
        taken,
        vec![&second],
        "whoever reads what the first write brought reads the second with it"
    );
    assert!(
        report
            .released()
            .iter()
            .all(|released| released.author == host_key.verifying_key().to_bytes()),
        "a released write is its author's"
    );

    // A frame the host's history gained after the member caught up reaches
    // the member once it asks for the history again: what a member does once
    // a write it let go can be taken in.
    let kept = sealed(&host_key);
    store
        .retain(
            tilo_relay_protocol::room_workspace_id(WORKSPACE).unwrap(),
            &Frame::of(&kept).unwrap(),
        )
        .unwrap();
    b.ask_replay();
    let replayed = hears(&mut heard, &kept, "the replayed frame").await;
    assert!(replayed.retained, "it comes out of the host's history");

    b_link.abort();
    router.shutdown().await.unwrap();
    host_endpoint.close().await;
    b_endpoint.close().await;
}
