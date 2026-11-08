//! A store device is an ordinary member of the workspace it keeps.
//!
//! The device holds the workspace's governance in its own engine and none of
//! its content keys. What it admits is decided there: a member's device may
//! keep at it and catch up from it, a device the workspace does not admit is
//! refused, and what it keeps is ciphertext from the first frame on.
//!
//! The device that made the workspace publishes and goes away; a member that
//! was away catches up from the store alone and reads the whole record back
//! with the keys the store never had.
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

mod common;

use automerge::{AutoCommit, ROOT, transaction::Transactable};
use channel_crypto::{ChannelGroup, MemberKey};
use collab_types::CommitNamed as _;
use common::{
    APP, RECORD, addr, artifact, channel, compact, device, endpoint, held_bytes, hint, invitation,
    joins, outgoing, publish, read_value,
};
use ed25519_dalek::SigningKey;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;
use tilo_delivery_protocol::workspace_join::WORKSPACE_JOIN_ALPN;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::join_answer::JoinAnswer;
use tilo_peer_state::join_transport::WorkspaceJoinProtocol;
use tilo_peer_state::owner::{ApplicationRecord, OwnerJoinWorkspaces, open_file, testing};
use tilo_relay_protocol::{DeviceMeshProfile, DeviceRole, ReplicationIntent, ServiceCapabilities};
use tilo_store_link::{ALPN, StoreService, StoreSession, appetite, workspace_of};
use tilo_store_node::Appetite;
use tilo_user_identity::DeviceCapabilities;

/// What the store device's mesh policy says it keeps: this workspace, whole.
fn store_profile(device: [u8; 32], workspace: u64) -> DeviceMeshProfile {
    DeviceMeshProfile {
        device,
        role: DeviceRole::AlwaysOnServer,
        services: ServiceCapabilities {
            workspace_replication: true,
            ..ServiceCapabilities::default()
        },
        replication: BTreeMap::from([(workspace, ReplicationIntent::FullWorkspace)]),
        filesystem_endpoints: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_store_admits_the_workspaces_members_and_no_one_else() {
    let directory = tempfile::tempdir().unwrap();
    let a_root = SigningKey::from_bytes(&[21; 32]);
    let b_root = SigningKey::from_bytes(&[31; 32]);
    let stranger_root = SigningKey::from_bytes(&[41; 32]);
    let store_root = SigningKey::from_bytes(&[51; 32]);
    let (a_key, a_cert) = device(&a_root, 22, DeviceCapabilities::primary());
    let (b_key, b_cert) = device(&b_root, 23, DeviceCapabilities::member());
    let (stranger_key, stranger_cert) = device(&stranger_root, 24, DeviceCapabilities::member());
    let (store_key, store_cert) = device(&store_root, 25, DeviceCapabilities::host());
    let now = SystemTime::now();

    // A makes the workspace on its own device and writes, sealed.
    let a = open_file(&directory.path().join("a.json")).unwrap();
    let workspace_id = testing::founded(&a, "Launch review", &a_cert, &a_key, now).unwrap();
    let workspace = workspace_of(&workspace_id.to_string()).unwrap();
    let record = ApplicationRecord::host(APP, RECORD).unwrap();
    a.prepare_application_record(workspace_id, &a_cert, &a_key, &record, now)
        .unwrap();
    let group = ChannelGroup::create(
        channel(&workspace_id.to_string()),
        MemberKey::generate("owner").unwrap(),
    )
    .unwrap();
    let keys = group.content_keys().unwrap();
    let b_keys = keys.clone();
    let artifact = artifact(&workspace_id.to_string());
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "before the guests").unwrap();
    document.commit_edit("fixture", "value");
    let change = document.save_incremental();
    publish(&a, workspace_id, &change, &keys, artifact, &a_key, now);
    compact(&a, workspace_id, &keys, artifact, &a_key, now);

    let a_endpoint = endpoint(&a_key).await;
    let a_router = iroh::protocol::Router::builder(a_endpoint.clone())
        .accept(
            WORKSPACE_JOIN_ALPN,
            WorkspaceJoinProtocol::new(JoinAnswer(OwnerJoinWorkspaces::new(
                a.clone(),
                a_key.clone(),
            ))),
        )
        .spawn();

    // The workspace's store device is a member of it: it joins, so it holds
    // the workspace's governance in its own engine, and it is given no key.
    let store_owner = open_file(&directory.path().join("store.json")).unwrap();
    let store_endpoint = endpoint(&store_key).await;
    joins(
        store_endpoint.clone(),
        &store_cert,
        &store_key,
        workspace,
        invitation(
            &a,
            &workspace_id,
            &a_cert,
            &a_key,
            &store_cert,
            hint(&a_endpoint, &a_cert, &a_key, &workspace_id, workspace),
            now,
        ),
        &store_owner,
        now,
    )
    .await;
    assert!(
        store_owner.contains_workspace(workspace_id),
        "the store holds the workspace it keeps"
    );

    // Its service keeps what its mesh policy gives it, and serves it to
    // whoever the engine admits on its endpoint.
    let profile = store_profile(store_key.verifying_key().to_bytes(), workspace);
    assert_eq!(appetite(&profile), Some(Appetite::StoreAll));
    let service = StoreService::open(
        directory.path().join("store"),
        &profile,
        Arc::new(store_owner.clone()),
    )
    .unwrap()
    .expect("a device that keeps a workspace whole is a store");
    let _store_router = iroh::protocol::Router::builder(store_endpoint.clone())
        .accept(ALPN, service.handler())
        .spawn();

    // A keeps the sealed checkpoint at the store, which opens none of it.
    let mut a_session = StoreSession::join(
        &a_endpoint,
        addr(&store_endpoint),
        a_cert.clone(),
        Vec::new(),
        workspace,
    )
    .await
    .unwrap();
    assert!(
        a_session
            .keep_workspace(&a, workspace_id, now)
            .await
            .unwrap()
            >= 1,
        "the store keeps the workspace's sealed checkpoint"
    );
    let held = service.store().bytes();
    assert!(held > 0);
    let mut kept = Vec::new();
    held_bytes(&directory.path().join("store"), &mut kept);
    assert!(!kept.is_empty());
    for bytes in &kept {
        for needle in [b"before the guests".as_slice(), b"value"] {
            assert!(
                !bytes.windows(needle.len()).any(|window| window == needle),
                "the store holds a plaintext byte"
            );
        }
    }

    // A guest joins the workspace, and is admitted at the store as its
    // member, so it reads the workspace's history back from there.
    let b = open_file(&directory.path().join("b.json")).unwrap();
    let b_endpoint = endpoint(&b_key).await;
    joins(
        b_endpoint.clone(),
        &b_cert,
        &b_key,
        workspace,
        invitation(
            &a,
            &workspace_id,
            &a_cert,
            &a_key,
            &b_cert,
            hint(&a_endpoint, &a_cert, &a_key, &workspace_id, workspace),
            now,
        ),
        &b,
        now,
    )
    .await;
    // The store's engine takes in the workspace's governance as any member's
    // does: it hears B's enrollment from the device that admitted it. What it
    // admits at the store follows from what its own engine knows.
    for message in outgoing(&a, workspace_id, now) {
        store_owner
            .receive(
                workspace_id,
                TransportOrigin::RetainedRelayDelta(a_key.verifying_key().to_bytes()),
                message,
                None,
                now,
            )
            .unwrap();
    }
    let mut b_session = StoreSession::join(
        &b_endpoint,
        addr(&store_endpoint),
        b_cert.clone(),
        Vec::new(),
        workspace,
    )
    .await
    .unwrap();
    assert_eq!(
        b_session
            .catch_up_workspace(&b, workspace_id, now)
            .await
            .unwrap(),
        1,
        "the store replays the checkpoint the workspace's history starts at"
    );
    assert_eq!(
        read_value(&b, workspace_id, &b_keys),
        "\"before the guests\""
    );

    // A writes while B is away and keeps it; B catches up from the store.
    let heads = document.get_heads();
    document.put(ROOT, "value", "while B was away").unwrap();
    document.commit_edit("fixture", "value");
    let change = document.save_after(&heads);
    publish(&a, workspace_id, &change, &keys, artifact, &a_key, now);
    assert!(
        a_session
            .keep_workspace(&a, workspace_id, now)
            .await
            .unwrap()
            >= 1
    );

    // A's device is gone: its workspace and its endpoint with it. What B
    // reads now can only come from the store.
    drop(a_session);
    drop(a);
    drop(a_router);
    a_endpoint.close().await;
    assert_eq!(
        b_session
            .catch_up_workspace(&b, workspace_id, now)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        read_value(&b, workspace_id, &b_keys),
        "\"while B was away\"",
        "the member reads what the store kept, and the store never had a key"
    );
    assert!(
        service.store().bytes() >= held,
        "the store still holds the frames it was given"
    );
    let mut kept = Vec::new();
    held_bytes(&directory.path().join("store"), &mut kept);
    for bytes in &kept {
        assert!(
            !bytes
                .windows(b"while B was away".len())
                .any(|window| window == b"while B was away"),
            "the store still holds no plaintext byte"
        );
    }

    // A device the workspace never admitted is welcomed as itself and
    // refused the workspace: the store's admission is the engine's
    // membership, not a list it keeps.
    let stranger_endpoint = endpoint(&stranger_key).await;
    let mut stranger = StoreSession::join(
        &stranger_endpoint,
        addr(&store_endpoint),
        stranger_cert,
        Vec::new(),
        workspace,
    )
    .await
    .expect("the store welcomes a device whose certificate proves it");
    assert!(
        stranger
            .catch_up_workspace(&b, workspace_id, now)
            .await
            .is_err(),
        "a device the workspace never admitted is not given the workspace"
    );
}
