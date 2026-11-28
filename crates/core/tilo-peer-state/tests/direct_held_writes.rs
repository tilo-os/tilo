//! A write that reaches a member before the change it builds on waits, and is
//! taken in once that change comes, with nothing else asked of anyone: the
//! member's apps read it with the change that released it. A write the member
//! let go is read again from the history its host keeps once the member asks
//! for it on the link.

use crate::common::{
    addr, localhost_endpoint, members_keys, opened, opened_heads, sealed_write, write_sealed,
};
use key_group::ContentKeys;

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use collab_types::CommitNamed as _;
use collab_types::{
    ContentRetention, Ed25519PublicKey, OpKind, SignedOp, document_artifact_id,
    document_attestation_id, sign_op,
};
use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use neighbour_wire::{NeighbourMsg, app_document_group, peer_handle};
use std::sync::Arc;
use std::time::Duration;
use tilo_peer_state::StatePeer;
use tilo_peer_state::direct::{DIRECT_ALPN, DirectSync, Heard, Kept, Serving};
use tilo_peer_state::store::{Appetite, Frame, Store};
use tilo_user_identity::{DeviceCapabilities, RevocationList};
use web_time::SystemTime;

use tilo_peer_state::fixture::{certify, person};
const WORKSPACE: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";

/// `author`'s write of `key = value` on the record as `author` holds it,
/// sealed under `keys` and taken into `author` so the next write builds on it.
fn write(
    author: &mut StatePeer,
    keys: &ContentKeys,
    signer: &SigningKey,
    (key, value): (&str, &str),
) -> NeighbourMsg {
    let person = person(author, signer);
    sealed_write(author, keys, signer, (APP, RECORD), |record| {
        record.put(ROOT, key, value).unwrap();
        record.commit_edit("fixture", key, &person);
    })
    .unwrap()
}

fn value(peer: &StatePeer, keys: &ContentKeys, key: &str) -> Option<String> {
    let record = AutoCommit::load(&opened(peer, keys, (APP, RECORD))?).ok()?;
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

/// One write of `key`'s device at `seq` on the record's line, sealed under
/// keys nobody here holds: what a host keeps in the history its members catch
/// up from.
fn sealed(key: &SigningKey, person: &[u8; 32], seq: u64) -> NeighbourMsg {
    let keys = key_group::KeyGroup::create(
        collab_types::WorkspaceId::parse(WORKSPACE).unwrap(),
        key_group::MemberKey::generate("owner").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap();
    let mut edit = AutoCommit::new();
    edit.put(ROOT, "sealed", "sealed").unwrap();
    edit.commit_edit("fixture", "sealed", person);
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
        seq,
    );
    op.encryption = Some(metadata);
    sign_op(&mut op, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: peer_handle(&key.verifying_key().to_bytes()),
        app_id: APP.into(),
        cluster_id: WORKSPACE.into(),
        document_id: RECORD.into(),
        envelope: Box::new(op),
        readers: None,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "a workspace, its host and a member, then two orderings the member is sent"
)]
#[tokio::test(flavor = "multi_thread")]
async fn a_write_before_the_change_it_builds_on_merges_once_that_change_comes() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let (host_key, server_cert) =
        crate::common::device_with(&root, 2, DeviceCapabilities::member());
    let (b_key, b_cert) = certify(&root, 3, DeviceCapabilities::member());
    // A third member, whose writes the host passes on to the second.
    let (c_key, c_cert) = certify(&root, 4, DeviceCapabilities::member());
    let now = SystemTime::now();
    let devices = vec![server_cert.clone(), b_cert.clone(), c_cert.clone()];
    let mut govern = StatePeer::new(
        WORKSPACE,
        server_cert.clone(),
        devices.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    govern.select_exact_admissions().unwrap();
    tilo_peer_state::fixture::found(
        &mut govern,
        (&host_key, &server_cert),
        &devices,
        Vec::new(),
        now,
    );
    let scratch = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(scratch.path(), Appetite::StoreAll).unwrap());
    let host = DirectSync::over(
        tilo_peer_state::direct::Replica::new(govern),
        host_key.clone(),
    )
    .with_store(Arc::clone(&store));
    // The record as the host wrote it, kept in its store.
    let keys = members_keys(WORKSPACE);
    write_sealed(&host, &keys, &host_key, (APP, RECORD), |record| {
        record.put(ROOT, "value", "from the host").unwrap();
        record.commit_edit("fixture", "value", &server_cert.user_public_key);
    });
    let serving = Serving::new();
    serving.serve(host.clone()).unwrap();
    let host_endpoint = localhost_endpoint(&host_key).await;
    let router = Router::builder(host_endpoint.clone())
        .accept(DIRECT_ALPN, serving.clone())
        .spawn();

    let b = StatePeer::from_join(WORKSPACE, server_cert.clone(), devices, &[], now).unwrap();
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
    let held = host.read(|host| opened_heads(host, &keys, (APP, RECORD)));
    let mut changes = b.changes();
    tokio::time::timeout(Duration::from_secs(30), async {
        while b.read(|b| opened_heads(b, &keys, (APP, RECORD))) != held {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("the member holds the record");

    // Two writes of another member's, the second building on the first, reach
    // the member through the host the other way round. They are that member's
    // own, numbered on its own sequence: written as the host's in a copy of its
    // replica, they would take the numbers the host's next catch-up takes too,
    // and the member answered for the deferred write asks for one.
    let mut author = host.read(Clone::clone);
    let first = write(&mut author, &keys, &c_key, ("first", "1"));
    let second = write(&mut author, &keys, &c_key, ("second", "2"));
    host.send(second.clone()).unwrap();
    let waited = hears(&mut heard, &second, "the second write").await;
    assert!(
        waited.report.as_ref().unwrap().deferred,
        "the second write waits for the first"
    );
    assert_eq!(b.read(|b| value(b, &keys, "second")), None);
    host.send(first.clone()).unwrap();
    let released = hears(&mut heard, &first, "the first write").await;
    // The first write takes the second in with it: nobody asked again.
    assert_eq!(
        b.read(|b| value(b, &keys, "first")).as_deref(),
        Some("1"),
        "the first write lands as it is heard: landed {}, retained {}, fresh {}, report {:?}",
        released.landed,
        released.retained,
        released.fresh,
        released.report
    );
    assert_eq!(
        b.read(|b| value(b, &keys, "second")).as_deref(),
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
            .all(|released| released.author == c_key.verifying_key().to_bytes()),
        "a released write is its author's"
    );

    // A frame the host's history gained after the member caught up reaches
    // the member once it asks for the history again: what a member does once
    // a write it let go can be taken in. A line is continuous, so the frame is
    // the one after the member's place on it: a store answers a hole in a line
    // with the record's whole state, not with the frames past the hole.
    let next = b
        .cursors()
        .into_iter()
        .find(|cursor| {
            cursor.lineage.record == app_document_group(APP, RECORD)
                && cursor.lineage.author == host_key.verifying_key().to_bytes()
        })
        .map(|cursor| cursor.after_seq + 1)
        .expect("the member stands on the host's line of the record");
    let kept = sealed(&host_key, &server_cert.user_public_key, next);
    store
        .retain(
            tilo_server_protocol::WorkspaceId::parse(WORKSPACE).unwrap(),
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

/// A member's frame whose signature no longer matches what it signed is
/// refused at admission: the engine decides nothing it carries, and the store
/// keeps none of it. The store's own keep finds it unsigned too
/// (`retained::Frame::of`), but only after the engine has already decided it.
#[tokio::test(flavor = "multi_thread")]
async fn a_frame_not_signed_by_its_author_is_refused_at_admission() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let (host_key, server_cert) =
        crate::common::device_with(&root, 2, DeviceCapabilities::member());
    let (b_key, b_cert) = certify(&root, 3, DeviceCapabilities::member());
    let now = SystemTime::now();
    let devices = vec![server_cert.clone(), b_cert.clone()];
    let mut govern = StatePeer::new(
        WORKSPACE,
        server_cert.clone(),
        devices.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    govern.select_exact_admissions().unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "from the host").unwrap();
    document.commit_edit("fixture", "value", &server_cert.user_public_key);
    tilo_peer_state::fixture::found(
        &mut govern,
        (&host_key, &server_cert),
        &devices,
        vec![tilo_peer_state::fixture::document(
            APP,
            RECORD,
            "test",
            document.save(),
        )],
        now,
    );
    let host = DirectSync::over(
        tilo_peer_state::direct::Replica::new(govern),
        host_key.clone(),
    );

    // A member's own write, its signature scrambled after it was signed.
    let mut forged = sealed(&b_key, &b_cert.user_public_key, 1);
    let NeighbourMsg::AppDocument { envelope, .. } = &mut forged else {
        panic!("a write is a document frame");
    };
    envelope.signature.0[0] ^= 0xff;

    let (_, kept) = host
        .admit_one(b_key.verifying_key().to_bytes(), forged)
        .await;
    assert!(
        matches!(&kept, Some(Kept::Refused { reason }) if !reason.is_empty()),
        "an unsigned frame is refused for good, saying why: {kept:?}"
    );
    assert_eq!(
        value(&host.read(Clone::clone), &members_keys(WORKSPACE), "forged"),
        None,
        "nothing the frame carried is readable"
    );
}
