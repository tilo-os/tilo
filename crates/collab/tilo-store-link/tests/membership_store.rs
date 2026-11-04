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

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
use collab_types::{
    ArtifactId, ChannelId, CommitNamed as _, ContentRetention, EncryptionMetadata, SignedOp,
};
use ed25519_dalek::SigningKey;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use neighbour_wire::NeighbourMsg;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;
use tilo_delivery_protocol::workspace_join::{
    IrohWorkspaceHostHintV1, WORKSPACE_JOIN_ALPN, WorkspaceHostMode,
};
use tilo_desktop_model::WorkspaceId;
use tilo_identity_host::custody::HostIdentity;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::join::invitations::IssueInvitationRequest;
use tilo_peer_state::join::{self, MemberRole, TransportHint};
use tilo_peer_state::join_answer::JoinAnswer;
use tilo_peer_state::owner::{
    ApplicationRecord, NativeWorkspaceOwner, OwnerJoinWorkspaces, open_file,
};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_relay_protocol::{DeviceMeshProfile, DeviceRole, ReplicationIntent, ServiceCapabilities};
use tilo_relay_session::workspace_join::{WorkspaceJoinClient, WorkspaceJoinProtocol};
use tilo_store_link::{ALPN, StoreService, StoreSession, appetite, workspace_of};
use tilo_store_node::Appetite;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

const APP: &str = "test.app";
const RECORD: &str = "tab:1";
const SECOND: u64 = 1_000_000_000;

fn device(
    root: &SigningKey,
    seed: u8,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{seed}"),
        capabilities,
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

fn ns(now: SystemTime) -> u64 {
    u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap()
}

fn hint(
    endpoint: &Endpoint,
    cert: &DeviceCert,
    key: &SigningKey,
    workspace_id: &WorkspaceId,
    workspace: u64,
) -> TransportHint {
    IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
        room: workspace_id.to_string(),
        workspace_id: workspace,
        endpoint_addr: addr(endpoint),
        host_user_public_key: cert.user_public_key,
        host_device_certificate: cert.clone(),
        packet_relay_url: "iroh:direct".into(),
    }
    .transport_hint(key)
    .unwrap()
}

fn custody(cert: &DeviceCert, key: &SigningKey) -> HostIdentity {
    HostIdentity {
        device_cert: cert.clone(),
        device_signing_key: key.clone(),
        workspace_device_certs: HashMap::default(),
        workspace_revocations: HashMap::default(),
        identity_paths: None,
    }
}

fn channel(workspace_id: &WorkspaceId) -> ChannelId {
    collab_types::document_channel_id(APP, &workspace_id.to_string(), RECORD)
}

fn artifact(workspace_id: &WorkspaceId) -> ArtifactId {
    collab_types::document_artifact_id(APP, &workspace_id.to_string(), RECORD)
}

fn sealer(
    keys: &ChannelContentKeys,
    artifact: ArtifactId,
    retention: ContentRetention,
) -> impl FnMut(&[u8]) -> Result<(EncryptionMetadata, Vec<u8>), String> {
    let keys = keys.clone();
    move |plaintext| {
        keys.seal_content(artifact, retention, plaintext)
            .map_err(|error| error.to_string())
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

/// Invite `guest` into the workspace, redeemed at the device that answers joins.
fn invitation(
    owner: &NativeWorkspaceOwner,
    workspace_id: &WorkspaceId,
    member: &DeviceCert,
    key: &SigningKey,
    guest: &DeviceCert,
    host: TransportHint,
    now: SystemTime,
) -> String {
    let issued = owner
        .issue_invitation(
            *workspace_id,
            member,
            IssueInvitationRequest {
                request_id: [7; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(guest.user_public_key),
                invitee_name: Some(guest.device_label.clone()),
                expires_at_ns: ns(now).saturating_add(600 * SECOND),
                inviter_name: None,
            },
            vec![host],
            key,
            now,
        )
        .unwrap();
    join::encode_invitation_text(&issued.signed_invitation)
}

/// `guest` joins `workspace` with `invitation` from `endpoint`, taking the
/// workspace in at `owner`.
async fn joins(
    endpoint: Endpoint,
    cert: &DeviceCert,
    key: &SigningKey,
    workspace: u64,
    invitation: String,
    owner: &NativeWorkspaceOwner,
    now: SystemTime,
) {
    let enrollment = tilo_peer_state::owner::enroll::prepare_and_complete(
        WorkspaceJoinClient::new(endpoint),
        custody(cert, key),
        workspace,
        None,
        invitation,
        owner.clone(),
    )
    .await
    .unwrap();
    owner.install(&enrollment, now).unwrap();
}

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

/// What `owner` holds for `workspace` that the devices of the workspace have
/// not been given yet.
fn outgoing(
    owner: &NativeWorkspaceOwner,
    workspace_id: WorkspaceId,
    now: SystemTime,
) -> Vec<NeighbourMsg> {
    owner
        .with_outgoing(workspace_id, now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| postcard::from_bytes(entry.bytes()).unwrap())
                .collect())
        })
        .unwrap()
}

/// Every byte the store device holds, file by file.
fn held_bytes(root: &Path, found: &mut Vec<Vec<u8>>) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            held_bytes(&path, found);
        } else {
            found.push(std::fs::read(&path).unwrap());
        }
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
    let workspace_id = a
        .create_workspace("Launch review", &a_cert, &a_key, now)
        .unwrap();
    let workspace = workspace_of(&workspace_id.to_string()).unwrap();
    let record = ApplicationRecord::host(APP, RECORD).unwrap();
    a.prepare_application_record(workspace_id, &a_cert, &a_key, &record, now)
        .unwrap();
    let group = ChannelGroup::create(
        channel(&workspace_id),
        MemberKey::generate("owner").unwrap(),
    )
    .unwrap();
    let keys = group.content_keys().unwrap();
    let b_keys = keys.clone();
    let artifact = artifact(&workspace_id);
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

/// Publish `change` to the record as A's sealed write.
fn publish(
    owner: &NativeWorkspaceOwner,
    workspace_id: WorkspaceId,
    change: &[u8],
    keys: &ChannelContentKeys,
    artifact: ArtifactId,
    key: &SigningKey,
    now: SystemTime,
) {
    owner
        .commit_document_with_current(
            workspace_id,
            |view| {
                let mut peer = view.peer().clone();
                let message = peer.publication_mut().publish_sealed_incremental(
                    APP,
                    RECORD,
                    change.to_vec(),
                    &mut sealer(keys, artifact, ContentRetention::Delta),
                    key,
                    now,
                )?;
                document_write(message)
            },
            now,
        )
        .unwrap();
}

/// Fold the record's sealed history into the sealed checkpoint a store keeps
/// in place of the writes it covers.
fn compact(
    owner: &NativeWorkspaceOwner,
    workspace_id: WorkspaceId,
    keys: &ChannelContentKeys,
    artifact: ArtifactId,
    key: &SigningKey,
    now: SystemTime,
) {
    owner
        .commit_document_with_current(
            workspace_id,
            |view| {
                let mut peer = view.peer().clone();
                let message = peer.publication_mut().compact_sealed_history(
                    APP,
                    RECORD,
                    &mut opener(keys),
                    &mut sealer(keys, artifact, ContentRetention::Checkpoint),
                    key,
                    now,
                )?;
                document_write(message)
            },
            now,
        )
        .unwrap();
}

/// The document message `message` is, as this device's commit of it.
fn document_write(message: NeighbourMsg) -> Result<OwnerCommitRequest, String> {
    let NeighbourMsg::AppDocument {
        cluster_id,
        envelope,
        ..
    } = message
    else {
        return Err("a document publish is a document write".into());
    };
    Ok(OwnerCommitRequest {
        workspace: cluster_id,
        operation: CommitOperation::Document {
            app: APP.into(),
            record: RECORD.into(),
            envelope,
        },
    })
}

fn read_value(
    owner: &NativeWorkspaceOwner,
    workspace_id: WorkspaceId,
    keys: &ChannelContentKeys,
) -> String {
    let opened = owner
        .with_current(workspace_id, |view| {
            view.peer()
                .publication()
                .opened_document(APP, RECORD, &mut opener(keys))
        })
        .unwrap()
        .expect("the workspace holds the record");
    assert!(opened.unmerged.is_empty(), "{:?}", opened.unmerged);
    AutoCommit::load(&opened.document)
        .unwrap()
        .get(ROOT, "value")
        .unwrap()
        .expect("the record has a value")
        .0
        .to_string()
}
