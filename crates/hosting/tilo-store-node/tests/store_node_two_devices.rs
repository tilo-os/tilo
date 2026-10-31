//! A store device carries a workspace's sealed history for a member that
//! was away, hands it back to a late joiner, and proves a device the
//! workspace does not admit gets nothing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
use collab_types::{
    ArtifactId, AttestationId, ChannelId, ContentRetention, Ed25519PublicKey, OpKind, SignedOp,
    sign_op,
};
use ed25519_dalek::SigningKey;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use neighbour_wire::{NeighbourMsg, peer_handle};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tilo_store_node::{
    ALPN, Appetite, Cursor, FixedAccess, Link, LinkError, Replay, Store, StoreNode, advance,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use web_time::UNIX_EPOCH;

const WORKSPACE: u64 = 42;
const APP: &str = "test.app";
const CLUSTER: &str = "workspace-42";
const RECORD: &str = "tab:1";

fn channel() -> ChannelId {
    collab_types::document_channel_id(APP, CLUSTER, RECORD)
}

fn artifact() -> ArtifactId {
    collab_types::document_artifact_id(APP, CLUSTER, RECORD)
}

fn attestation() -> AttestationId {
    collab_types::document_attestation_id(APP, CLUSTER, RECORD)
}

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

/// One edit `key` publishes, sealed under the channel's content keys.
fn sealed(
    key: &SigningKey,
    keys: &ChannelContentKeys,
    seq: u64,
    retention: ContentRetention,
    text: &str,
) -> NeighbourMsg {
    let (metadata, payload) = keys
        .seal_content(artifact(), retention, text.as_bytes())
        .unwrap();
    let mut op = SignedOp::new_unsigned(
        payload,
        artifact(),
        Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
        attestation(),
        OpKind::Change,
        UNIX_EPOCH + Duration::from_secs(seq),
        seq,
    );
    op.encryption = Some(metadata);
    sign_op(&mut op, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: peer_handle(&key.verifying_key().to_bytes()),
        app_id: APP.into(),
        cluster_id: CLUSTER.into(),
        record_id: RECORD.into(),
        envelope: Box::new(op),
    }
}

fn envelope(message: &NeighbourMsg) -> &SignedOp {
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("a document frame");
    };
    envelope
}

/// What a holder of the channel's keys reads out of `message`.
fn open(keys: &ChannelContentKeys, message: &NeighbourMsg) -> String {
    let op = envelope(message);
    let metadata = op.encryption.unwrap();
    let plaintext = keys
        .open_content(artifact(), &metadata, &op.payload)
        .unwrap();
    String::from_utf8(plaintext.to_vec()).unwrap()
}

fn files(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, found);
        } else {
            found.push(path);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_store_carries_a_workspaces_history_to_a_member_that_was_away() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let (a_key, a_cert) = device(&root, 2);
    let (b_key, b_cert) = device(&root, 3);
    let (c_key, c_cert) = device(&root, 4);
    let (store_key, _) = device(&root, 5);

    // The channel's keys live with the members' devices; the store is not
    // one of them and is never given any.
    let group = ChannelGroup::create(channel(), MemberKey::generate("owner").unwrap()).unwrap();
    let a_keys = group.content_keys().unwrap();
    let b_keys = a_keys.clone();

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path(), Appetite::StoreAll).unwrap());
    let mut access = FixedAccess::new();
    access.admit(WORKSPACE, a_key.verifying_key().to_bytes());
    access.admit(WORKSPACE, b_key.verifying_key().to_bytes());
    let node = StoreNode::new(store.clone(), Arc::new(access));
    let store_endpoint = endpoint(&store_key).await;
    let _router = iroh::protocol::Router::builder(store_endpoint.clone())
        .accept(ALPN, node)
        .spawn();

    // A publishes three edits while B is away.
    let a_endpoint = endpoint(&a_key).await;
    let mut a = Link::connect(&a_endpoint, addr(&store_endpoint), a_cert, Vec::new())
        .await
        .unwrap();
    let published: Vec<NeighbourMsg> = (1..=3)
        .map(|seq| {
            sealed(
                &a_key,
                &a_keys,
                seq,
                ContentRetention::Delta,
                &format!("a-{seq}"),
            )
        })
        .collect();
    assert_eq!(a.push(WORKSPACE, &published).await.unwrap(), 3);

    // What the store keeps is ciphertext: none of what A wrote appears in
    // any file it wrote, and another channel's keys cannot open a frame.
    let mut kept = Vec::new();
    files(dir.path(), &mut kept);
    assert_eq!(kept.len(), 3);
    for file in &kept {
        let bytes = std::fs::read(file).unwrap();
        for needle in [b"a-1".as_slice(), b"a-2", b"a-3"] {
            assert!(
                !bytes.windows(needle.len()).any(|window| window == needle),
                "{} carries plaintext",
                file.display()
            );
        }
    }
    let stranger = ChannelGroup::create(ChannelId::new(), MemberKey::generate("stranger").unwrap())
        .unwrap()
        .content_keys()
        .unwrap();
    let first = envelope(&published[0]);
    assert!(
        stranger
            .open_content(artifact(), &first.encryption.unwrap(), &first.payload)
            .is_err(),
        "a frame opens only under the channel's own keys"
    );

    // B joins late and reads the workspace's whole history from the store.
    let b_endpoint = endpoint(&b_key).await;
    let mut b = Link::connect(&b_endpoint, addr(&store_endpoint), b_cert, Vec::new())
        .await
        .unwrap();
    let mut b_cursors: Vec<Cursor> = Vec::new();
    let replay = b.want(WORKSPACE, &b_cursors).await.unwrap();
    assert!(!replay.more);
    assert!(replay.floors.is_empty());
    assert_eq!(
        replay
            .frames
            .iter()
            .map(|message| open(&b_keys, message))
            .collect::<Vec<_>>(),
        vec!["a-1", "a-2", "a-3"]
    );
    advance(&mut b_cursors, &replay);

    // B's own edit reaches A through the store, which carried it sealed.
    let from_b = sealed(&b_key, &b_keys, 1, ContentRetention::Delta, "b-1");
    assert_eq!(
        b.push(WORKSPACE, std::slice::from_ref(&from_b))
            .await
            .unwrap(),
        1
    );
    // B's cursors follow what it holds, including what it published itself.
    advance(
        &mut b_cursors,
        &Replay {
            frames: vec![from_b.clone()],
            ..Replay::default()
        },
    );
    let mut a_cursors: Vec<Cursor> = Vec::new();
    let replay = a.want(WORKSPACE, &[]).await.unwrap();
    let last = replay
        .frames
        .iter()
        .find(|message| envelope(message).author.as_bytes() == &b_key.verifying_key().to_bytes());
    let last = last.expect("A gets B's frame");
    assert_eq!(open(&a_keys, last), "b-1");
    advance(&mut a_cursors, &replay);
    assert!(
        a.want(WORKSPACE, &a_cursors)
            .await
            .unwrap()
            .frames
            .is_empty(),
        "a caught-up member is handed nothing"
    );

    // A checkpoint retires what the store held of that lineage, and B
    // resumes from its cursor with only the checkpoint.
    let before = store.bytes();
    let checkpoint = sealed(&a_key, &a_keys, 4, ContentRetention::Checkpoint, "a-all");
    assert_eq!(
        a.push(WORKSPACE, std::slice::from_ref(&checkpoint))
            .await
            .unwrap(),
        1
    );
    assert!(store.bytes() < before);
    let replay = b.want(WORKSPACE, &b_cursors).await.unwrap();
    assert_eq!(replay.frames.len(), 1);
    assert_eq!(open(&b_keys, &replay.frames[0]), "a-all");

    // A device the workspace does not admit is refused, and told nothing.
    let c_endpoint = endpoint(&c_key).await;
    let mut c = Link::connect(&c_endpoint, addr(&store_endpoint), c_cert, Vec::new())
        .await
        .unwrap();
    let refused = c.want(WORKSPACE, &[]).await.unwrap_err();
    assert!(matches!(refused, LinkError::Refused(_)), "{refused}");
    let revoked_push = c
        .push(
            WORKSPACE,
            &[sealed(&c_key, &b_keys, 1, ContentRetention::Delta, "c-1")],
        )
        .await
        .unwrap_err();
    assert!(
        matches!(revoked_push, LinkError::Refused(_)),
        "{revoked_push}"
    );
    assert!(store.bytes() < before);
}
