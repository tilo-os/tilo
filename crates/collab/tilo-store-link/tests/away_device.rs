//! A member keeps its sealed history at a store device and a device that
//! was away catches up from it, reading the whole record back: the latest
//! sealed checkpoint plus the changes after it, and nothing in between that
//! the checkpoint already covers.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]
tilo_automerge_isolate::test_worker!();

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
use collab_types::{
    ArtifactId, ChannelId, CommitNamed as _, ContentRetention, EncryptionMetadata, SignedOp,
};
use ed25519_dalek::SigningKey;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use neighbour_wire::NeighbourMsg;
use std::sync::Arc;
use std::time::Duration;
use tilo_peer_state::direct::DirectSync;
use tilo_peer_state::{SeedDocument, SeedState, StatePeer, TransportOrigin, seed_room};
use tilo_store_link::{StoreSession, cursors, workspace_of};
use tilo_store_node::{ALPN, Appetite, Cursor, FixedAccess, Lineage, Store, StoreNode};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use web_time::SystemTime;

const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";

fn device(root: &SigningKey, seed: u8) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{seed}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

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

fn channel() -> ChannelId {
    collab_types::document_channel_id(APP, ROOM, RECORD)
}

fn artifact() -> ArtifactId {
    collab_types::document_artifact_id(APP, ROOM, RECORD)
}

fn sealer(
    keys: &ChannelContentKeys,
    retention: ContentRetention,
) -> impl FnMut(&[u8]) -> Result<(EncryptionMetadata, Vec<u8>), String> {
    let artifact = artifact();
    move |plaintext| {
        keys.seal_content(artifact, retention, plaintext)
            .map_err(|error| error.to_string())
    }
}

fn seq(message: &NeighbourMsg) -> u64 {
    match message {
        NeighbourMsg::AppDocument { envelope, .. } => envelope.seq,
        _ => 0,
    }
}

fn opener(keys: &ChannelContentKeys) -> impl FnMut(&SignedOp) -> Result<Vec<u8>, String> {
    let keys = keys.clone();
    move |op| {
        let metadata = op.encryption.ok_or("not sealed")?;
        let ciphertext = collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?;
        keys.open_content(op.artifact, &metadata, &ciphertext)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_was_away_catches_up_from_the_store() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let (a_key, a_cert) = device(&root, 2);
    let (b_key, b_cert) = device(&root, 3);
    let (store_key, _) = device(&root, 4);
    let roster = vec![a_cert.clone(), b_cert.clone()];
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2000);
    let author = a_key.verifying_key().to_bytes();

    // A holds the room and its record from the start.
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
    document.put(ROOT, "value", "base").unwrap();
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
    let a = DirectSync::new(a, a_key.clone());

    // The record's keys are the members'; the store is never given them.
    let group = ChannelGroup::create(channel(), MemberKey::generate("owner").unwrap()).unwrap();
    let keys = group.content_keys().unwrap();
    let b_keys = keys.clone();

    // The workspace's store device admits both members.
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path(), Appetite::StoreAll).unwrap());
    let workspace = workspace_of(ROOM).unwrap();
    let mut access = FixedAccess::new();
    access.admit(workspace, author);
    access.admit(workspace, b_key.verifying_key().to_bytes());
    let node = StoreNode::new(store.clone(), Arc::new(access));
    let store_endpoint = endpoint(&store_key).await;
    let _router = iroh::protocol::Router::builder(store_endpoint.clone())
        .accept(ALPN, node)
        .spawn();

    let a_endpoint = endpoint(&a_key).await;
    let mut a_session = StoreSession::join(
        &a_endpoint,
        addr(&store_endpoint),
        a_cert.clone(),
        Vec::new(),
        workspace,
    )
    .await
    .unwrap();

    // A publishes a sealed change, then the checkpoint that covers it: the
    // store keeps both, retires the change the checkpoint replaced, and
    // never sees either in the clear.
    let heads = document.get_heads();
    document.put(ROOT, "value", "from a").unwrap();
    document.commit_edit("fixture", "value");
    let change = document.save_after(&heads);
    let delta = a
        .publish(|peer, key, when| {
            peer.publication_mut().publish_sealed_incremental(
                APP,
                RECORD,
                change.clone(),
                &mut sealer(&keys, ContentRetention::Delta),
                key,
                when,
            )
        })
        .unwrap();
    assert!(a_session.keep(&delta).await.unwrap());
    let delta_seq = seq(&delta);
    assert!(
        !a_session
            .keep(&NeighbourMsg::Heartbeat { peer: 0, ts: 0 })
            .await
            .unwrap(),
        "presence is not history"
    );
    let checkpoint = a
        .publish(|peer, key, when| {
            peer.publication_mut().compact_sealed_history(
                APP,
                RECORD,
                &mut opener(&keys),
                &mut sealer(&keys, ContentRetention::Checkpoint),
                key,
                when,
            )
        })
        .unwrap();
    assert!(a_session.keep(&checkpoint).await.unwrap());
    let checkpoint_seq = seq(&checkpoint);
    assert!(
        checkpoint_seq > delta_seq,
        "the checkpoint follows the change it covers"
    );
    assert_eq!(
        store
            .lineages(workspace)
            .into_iter()
            .map(|(_, first, last)| (first, last))
            .collect::<Vec<_>>(),
        vec![(checkpoint_seq, checkpoint_seq)],
        "the checkpoint retired the change it covers"
    );

    // B has the workspace's roster and governance from its join answer and
    // none of its history: the store is what it lacks.
    let mut b = StatePeer::new(
        ROOM,
        a_cert.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    b.select_admission_v5().unwrap();
    // Its join answer: the room's governance, without a record's history.
    let NeighbourMsg::SyncResp { changes, room } = a.read(StatePeer::room_message) else {
        panic!("the room's answer is its governance");
    };
    b.receive_from(
        TransportOrigin::RetainedRelayDelta(author),
        NeighbourMsg::SyncResp { changes, room },
        now,
    )
    .unwrap();
    let b = DirectSync::new(b, b_key.clone());
    assert!(b.read(StatePeer::rows).is_empty());
    let b_endpoint = endpoint(&b_key).await;
    let mut b_session = StoreSession::join(
        &b_endpoint,
        addr(&store_endpoint),
        b_cert,
        Vec::new(),
        workspace,
    )
    .await
    .unwrap();
    assert_eq!(b_session.catch_up(&b).await.unwrap(), 1);
    let line = Lineage::of(APP, ROOM, RECORD, author);
    assert_eq!(
        cursors(&b),
        vec![Cursor {
            lineage: line,
            after_seq: checkpoint_seq
        }],
        "B stands at the checkpoint it admitted"
    );
    assert!(
        b.read(|b| b.publication().is_sealed_record(APP, RECORD)),
        "B holds the record sealed"
    );

    // A change published after the checkpoint reaches B as a delta.
    let heads = document.get_heads();
    document.put(ROOT, "value", "after the checkpoint").unwrap();
    document.commit_edit("fixture", "value");
    let change = document.save_after(&heads);
    let after = a
        .publish(|peer, key, when| {
            peer.publication_mut().publish_sealed_incremental(
                APP,
                RECORD,
                change.clone(),
                &mut sealer(&keys, ContentRetention::Delta),
                key,
                when,
            )
        })
        .unwrap();
    assert!(a_session.keep(&after).await.unwrap());
    assert_eq!(b_session.catch_up(&b).await.unwrap(), 1);
    assert_eq!(
        cursors(&b),
        vec![Cursor {
            lineage: line,
            after_seq: seq(&after)
        }]
    );
    assert_eq!(
        b_session.catch_up(&b).await.unwrap(),
        0,
        "a caught-up device is handed nothing"
    );

    // B reads its whole history back: the checkpoint and the change after
    // it open with the members' keys into the record A sees.
    let opened = b
        .read(|b| {
            b.publication()
                .opened_document(APP, RECORD, &mut opener(&b_keys))
        })
        .unwrap()
        .expect("B holds the record");
    assert!(opened.unmerged.is_empty(), "{:?}", opened.unmerged);
    let mut read = AutoCommit::load(&opened.document).unwrap();
    assert_eq!(
        read.get(ROOT, "value").unwrap().unwrap().0.to_string(),
        "\"after the checkpoint\""
    );
    let on_a = a
        .read(|a| {
            a.publication()
                .opened_document(APP, RECORD, &mut opener(&keys))
        })
        .unwrap()
        .expect("A holds the record");
    assert_eq!(
        AutoCommit::load(&on_a.document).unwrap().get_heads(),
        read.get_heads()
    );
}
