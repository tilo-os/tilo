//! A short-lived room is an ordinary workspace held by a store device.
//!
//! The workspace is made on one person's device, with no room service in
//! between; its history is kept, sealed, at a store device that never holds a
//! key. A guest is invited and joins over the workspace-join exchange, reads
//! the whole history back from the store, and keeps reading what changes
//! while it is away. When the device that made the workspace is gone, the
//! guest hands a later guest an invitation of its own: the workspace outlives
//! every device that takes part in it, and a device that has none of its
//! history starts from the store's copy alone.
//!
//! A share is the same thing at another scale: the device that shares an app
//! founds a workspace for it, and the people it shares with join and write in
//! it as members, sealed, while the store keeps every member's line.
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
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
use tilo_delivery_protocol::workspace_join::{
    IrohWorkspaceHostHintV1, WORKSPACE_JOIN_ALPN, WorkspaceHostMode,
};
use tilo_desktop_model::RoomId;
use tilo_identity_host::custody::HostIdentity;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::join::invitations::IssueInvitationRequest;
use tilo_peer_state::join::{self, MemberRole, TransportHint};
use tilo_peer_state::join_answer::JoinAnswer;
use tilo_peer_state::owner::{ApplicationRecord, NativeWorkspaceOwner, OwnerJoinRooms, open_file};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
use tilo_relay_session::workspace_join::{WorkspaceJoinClient, WorkspaceJoinProtocol};
use tilo_store_link::{StoreSession, room_cursors, workspace_of};
use tilo_store_node::{ALPN, Appetite, FixedAccess, Store, StoreNode};
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

/// The address at which `endpoint` answers the workspace's joins.
fn hint(
    endpoint: &Endpoint,
    cert: &DeviceCert,
    key: &SigningKey,
    room: &RoomId,
    workspace: u64,
) -> TransportHint {
    IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
        room: room.to_string(),
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

fn channel(room: &RoomId) -> ChannelId {
    collab_types::document_channel_id(APP, &room.to_string(), RECORD)
}

fn artifact(room: &RoomId) -> ArtifactId {
    collab_types::document_artifact_id(APP, &room.to_string(), RECORD)
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

/// Invite `guest` into `room`, redeemed at the device that answers joins.
fn invitation(
    owner: &NativeWorkspaceOwner,
    room: &RoomId,
    member: &DeviceCert,
    key: &SigningKey,
    guest: &DeviceCert,
    host: TransportHint,
    now: SystemTime,
) -> String {
    let issued = owner
        .issue_invitation(
            *room,
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
/// room in at `owner`.
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

#[tokio::test(flavor = "multi_thread")]
async fn a_short_lived_room_is_a_workspace_a_store_device_holds() {
    let directory = tempfile::tempdir().unwrap();
    let a_root = SigningKey::from_bytes(&[21; 32]);
    let b_root = SigningKey::from_bytes(&[31; 32]);
    let c_root = SigningKey::from_bytes(&[41; 32]);
    let store_root = SigningKey::from_bytes(&[51; 32]);
    let (a_key, a_cert) = device(&a_root, 22, DeviceCapabilities::primary());
    let (b_key, b_cert) = device(&b_root, 23, DeviceCapabilities::member());
    let (c_key, c_cert) = device(&c_root, 24, DeviceCapabilities::member());
    let (store_key, _) = device(&store_root, 25, DeviceCapabilities::host());
    let now = SystemTime::now();
    let author = a_key.verifying_key().to_bytes();

    // A makes the workspace on its own device: no room service, no host of
    // anyone else's.
    let a = open_file(&directory.path().join("a.json")).unwrap();
    let room = a
        .create_workspace("Launch review", &a_cert, &a_key, now)
        .unwrap();
    let workspace = workspace_of(&room.to_string()).unwrap();
    assert!(
        a.with_current(room, |view| Ok(view.hosted_here())).unwrap(),
        "the device that made the workspace keeps it"
    );

    // Its record, and its keys: the members' alone, never the store's.
    let record = ApplicationRecord::host(APP, RECORD).unwrap();
    a.prepare_application_record(room, &a_cert, &a_key, &record, now)
        .unwrap();
    let group =
        ChannelGroup::create(channel(&room), MemberKey::generate("owner").unwrap()).unwrap();
    let keys = group.content_keys().unwrap();
    let b_keys = keys.clone();
    let c_keys = keys.clone();
    let artifact = artifact(&room);

    // A writes, sealed, and folds the record into a sealed checkpoint: what
    // a store device keeps of it is ciphertext from the first frame on.
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "before the guests").unwrap();
    document.commit_edit("fixture", "value");
    let change = document.save_incremental();
    publish(&a, room, &change, &keys, artifact, &a_key, now);
    compact(&a, room, &keys, artifact, &a_key, now);

    // The workspace's store device admits its members and keeps only what is
    // sealed.
    let store = Arc::new(Store::open(directory.path().join("store"), Appetite::StoreAll).unwrap());
    let mut access = FixedAccess::new();
    for member in [
        author,
        b_key.verifying_key().to_bytes(),
        c_key.verifying_key().to_bytes(),
    ] {
        access.admit(workspace, member);
    }
    let store_endpoint = endpoint(&store_key).await;
    let _store_router = iroh::protocol::Router::builder(store_endpoint.clone())
        .accept(ALPN, StoreNode::new(store.clone(), Arc::new(access)))
        .spawn();

    let a_endpoint = endpoint(&a_key).await;
    let a_router = iroh::protocol::Router::builder(a_endpoint.clone())
        .accept(
            WORKSPACE_JOIN_ALPN,
            WorkspaceJoinProtocol::new(JoinAnswer(OwnerJoinRooms::new(a.clone(), a_key.clone()))),
        )
        .spawn();
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
        a_session.keep_room(&a, room, now).await.unwrap() >= 1,
        "the store keeps the sealed checkpoint of the record"
    );
    let lineages = store.lineages(workspace);
    assert_eq!(lineages.len(), 1, "the store holds one sealed line");
    let (_, first, last) = lineages.first().unwrap();
    assert_eq!(
        first, last,
        "the store holds the checkpoint alone: it retired the write it covers"
    );

    // B is invited and joins the workspace over its join exchange.
    let b = open_file(&directory.path().join("b.json")).unwrap();
    let b_endpoint = endpoint(&b_key).await;
    joins(
        b_endpoint.clone(),
        &b_cert,
        &b_key,
        workspace,
        invitation(
            &a,
            &room,
            &a_cert,
            &a_key,
            &b_cert,
            hint(&a_endpoint, &a_cert, &a_key, &room, workspace),
            now,
        ),
        &b,
        now,
    )
    .await;
    assert!(
        !b.with_current(room, |view| Ok(view.hosted_here())).unwrap(),
        "B holds the workspace without having made it"
    );

    // Everything B reads of it comes from the store.
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
        b_session.catch_up_room(&b, room, now).await.unwrap(),
        1,
        "the store replays the checkpoint the workspace's history starts at"
    );
    assert_eq!(read_value(&b, room, &b_keys), "\"before the guests\"");
    assert!(
        !room_cursors(&b, room).unwrap().is_empty(),
        "B stands at the history it read"
    );

    // A writes again after B was away, and the store carries it to B.
    let heads = document.get_heads();
    document.put(ROOT, "value", "while B was away").unwrap();
    document.commit_edit("fixture", "value");
    let change = document.save_after(&heads);
    publish(&a, room, &change, &keys, artifact, &a_key, now);
    assert!(a_session.keep_room(&a, room, now).await.unwrap() >= 1);
    assert_eq!(b_session.catch_up_room(&b, room, now).await.unwrap(), 1);
    assert_eq!(read_value(&b, room, &b_keys), "\"while B was away\"");

    // A lets B invite, and B takes that governance in, before A goes away.
    a.commit_workspace_membership_command(
        OwnerCommitRequest {
            room: room.to_string(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: a_cert.clone(),
                command: WorkspaceMembershipCommand::Grant {
                    grant: invite_grant(&a_cert, &b_cert, &room),
                },
            },
        },
        &a_key,
        now,
    )
    .unwrap();
    for message in outgoing(&a, room, now) {
        // What B already holds is an exact retry, and admits as one.
        let _report = b
            .receive(
                room,
                TransportOrigin::RetainedRelayDelta(author),
                message,
                now,
            )
            .unwrap();
    }

    // A's device is gone: its workspace and its endpoint with it.
    drop(a_session);
    drop(a);
    drop(a_router);
    a_endpoint.close().await;

    // A later guest joins through B, and reads the workspace from the store,
    // which is the only place its history was.
    let _b_router = iroh::protocol::Router::builder(b_endpoint.clone())
        .accept(
            WORKSPACE_JOIN_ALPN,
            WorkspaceJoinProtocol::new(JoinAnswer(OwnerJoinRooms::new(b.clone(), b_key.clone()))),
        )
        .spawn();
    let c = open_file(&directory.path().join("c.json")).unwrap();
    let c_endpoint = endpoint(&c_key).await;
    joins(
        c_endpoint.clone(),
        &c_cert,
        &c_key,
        workspace,
        invitation(
            &b,
            &room,
            &b_cert,
            &b_key,
            &c_cert,
            hint(&b_endpoint, &b_cert, &b_key, &room, workspace),
            now,
        ),
        &c,
        now,
    )
    .await;
    let mut c_session = StoreSession::join(
        &c_endpoint,
        addr(&store_endpoint),
        c_cert,
        Vec::new(),
        workspace,
    )
    .await
    .unwrap();
    c_session.catch_up_room(&c, room, now).await.unwrap();
    assert_eq!(
        read_value(&c, room, &c_keys),
        "\"while B was away\"",
        "the guest reads everything, from the store alone"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shared_app_is_a_workspace_its_guests_write_in() {
    let directory = tempfile::tempdir().unwrap();
    let a_root = SigningKey::from_bytes(&[61; 32]);
    let b_root = SigningKey::from_bytes(&[71; 32]);
    let c_root = SigningKey::from_bytes(&[81; 32]);
    let store_root = SigningKey::from_bytes(&[91; 32]);
    let (a_key, a_cert) = device(&a_root, 62, DeviceCapabilities::primary());
    let (b_key, b_cert) = device(&b_root, 63, DeviceCapabilities::member());
    let (c_key, c_cert) = device(&c_root, 64, DeviceCapabilities::member());
    let (store_key, _) = device(&store_root, 65, DeviceCapabilities::host());
    let now = SystemTime::now();
    let a_author = a_key.verifying_key().to_bytes();

    // The sharer founds the workspace and writes what it shares.
    let a = open_file(&directory.path().join("a.json")).unwrap();
    let room = a
        .create_workspace("Shared notes", &a_cert, &a_key, now)
        .unwrap();
    let workspace = workspace_of(&room.to_string()).unwrap();
    let record = ApplicationRecord::host(APP, RECORD).unwrap();
    a.prepare_application_record(room, &a_cert, &a_key, &record, now)
        .unwrap();
    let group =
        ChannelGroup::create(channel(&room), MemberKey::generate("owner").unwrap()).unwrap();
    let keys = group.content_keys().unwrap();
    let b_keys = keys.clone();
    let c_keys = keys.clone();
    let artifact = artifact(&room);
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "the sharer's line").unwrap();
    document.commit_edit("sharer", "value");
    let change = document.save_incremental();
    publish(&a, room, &change, &keys, artifact, &a_key, now);
    // What it shares is a baseline: the checkpoint a guest's copy starts from.
    compact(&a, room, &keys, artifact, &a_key, now);

    let store = Arc::new(Store::open(directory.path().join("store"), Appetite::StoreAll).unwrap());
    let mut access = FixedAccess::new();
    for member in [
        a_author,
        b_key.verifying_key().to_bytes(),
        c_key.verifying_key().to_bytes(),
    ] {
        access.admit(workspace, member);
    }
    let store_endpoint = endpoint(&store_key).await;
    let _store_router = iroh::protocol::Router::builder(store_endpoint.clone())
        .accept(ALPN, StoreNode::new(store.clone(), Arc::new(access)))
        .spawn();
    let a_endpoint = endpoint(&a_key).await;
    let a_router = iroh::protocol::Router::builder(a_endpoint.clone())
        .accept(
            WORKSPACE_JOIN_ALPN,
            WorkspaceJoinProtocol::new(JoinAnswer(OwnerJoinRooms::new(a.clone(), a_key.clone()))),
        )
        .spawn();
    let mut a_session = StoreSession::join(
        &a_endpoint,
        addr(&store_endpoint),
        a_cert.clone(),
        Vec::new(),
        workspace,
    )
    .await
    .unwrap();
    assert!(a_session.keep_room(&a, room, now).await.unwrap() >= 1);

    // The guest joins by invitation and reads the share from the store.
    let b = open_file(&directory.path().join("b.json")).unwrap();
    let b_endpoint = endpoint(&b_key).await;
    joins(
        b_endpoint.clone(),
        &b_cert,
        &b_key,
        workspace,
        invitation(
            &a,
            &room,
            &a_cert,
            &a_key,
            &b_cert,
            hint(&a_endpoint, &a_cert, &a_key, &room, workspace),
            now,
        ),
        &b,
        now,
    )
    .await;
    let mut b_session = StoreSession::join(
        &b_endpoint,
        addr(&store_endpoint),
        b_cert.clone(),
        Vec::new(),
        workspace,
    )
    .await
    .unwrap();
    assert_eq!(b_session.catch_up_room(&b, room, now).await.unwrap(), 1);
    assert_eq!(read_value(&b, room, &b_keys), "\"the sharer's line\"");

    // The guest writes too: a member's own sealed line, kept at the store
    // beside the sharer's.
    let mut guest = read_document(&b, room, &b_keys);
    let heads = guest.get_heads();
    guest.put(ROOT, "value", "the guest's line").unwrap();
    guest.commit_edit("guest", "value");
    let change = guest.save_after(&heads);
    publish(&b, room, &change, &b_keys, artifact, &b_key, now);
    assert!(b_session.keep_room(&b, room, now).await.unwrap() >= 1);
    let lineages = store.lineages(workspace);
    assert_eq!(
        lineages.len(),
        2,
        "the store holds the sharer's sealed line and the guest's"
    );

    // The sharer catches up from the store and sees the guest's edit.
    a_session.catch_up_room(&a, room, now).await.unwrap();
    assert_eq!(read_value(&a, room, &keys), "\"the guest's line\"");

    // The guest may hand the share on, as the sharer allows before it goes.
    a.commit_workspace_membership_command(
        OwnerCommitRequest {
            room: room.to_string(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: a_cert.clone(),
                command: WorkspaceMembershipCommand::Grant {
                    grant: invite_grant(&a_cert, &b_cert, &room),
                },
            },
        },
        &a_key,
        now,
    )
    .unwrap();
    for message in outgoing(&a, room, now) {
        let _report = b
            .receive(
                room,
                TransportOrigin::RetainedRelayDelta(a_author),
                message,
                now,
            )
            .unwrap();
    }

    // The sharer's device is gone; a later guest reads the whole share,
    // written by both, from the store alone.
    drop(a_session);
    drop(a);
    drop(a_router);
    a_endpoint.close().await;
    let _b_router = iroh::protocol::Router::builder(b_endpoint.clone())
        .accept(
            WORKSPACE_JOIN_ALPN,
            WorkspaceJoinProtocol::new(JoinAnswer(OwnerJoinRooms::new(b.clone(), b_key.clone()))),
        )
        .spawn();
    let c = open_file(&directory.path().join("c.json")).unwrap();
    let c_endpoint = endpoint(&c_key).await;
    joins(
        c_endpoint.clone(),
        &c_cert,
        &c_key,
        workspace,
        invitation(
            &b,
            &room,
            &b_cert,
            &b_key,
            &c_cert,
            hint(&b_endpoint, &b_cert, &b_key, &room, workspace),
            now,
        ),
        &c,
        now,
    )
    .await;
    let mut c_session = StoreSession::join(
        &c_endpoint,
        addr(&store_endpoint),
        c_cert,
        Vec::new(),
        workspace,
    )
    .await
    .unwrap();
    c_session.catch_up_room(&c, room, now).await.unwrap();
    assert_eq!(
        read_value(&c, room, &c_keys),
        "\"the guest's line\"",
        "the later guest reads what both members wrote, from the store alone"
    );
}

/// Publish `change` to the record as A's sealed write.
fn publish(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    change: &[u8],
    keys: &ChannelContentKeys,
    artifact: ArtifactId,
    key: &SigningKey,
    now: SystemTime,
) {
    owner
        .commit_document_with_current(
            room,
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
    room: RoomId,
    keys: &ChannelContentKeys,
    artifact: ArtifactId,
    key: &SigningKey,
    now: SystemTime,
) {
    owner
        .commit_document_with_current(
            room,
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
        room: cluster_id,
        operation: CommitOperation::Document {
            app: APP.into(),
            record: RECORD.into(),
            envelope,
        },
    })
}

/// The record's document as `owner` holds it.
fn read_document(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    keys: &ChannelContentKeys,
) -> AutoCommit {
    let opened = owner
        .with_current(room, |view| {
            view.peer()
                .publication()
                .opened_document(APP, RECORD, &mut opener(keys))
        })
        .unwrap()
        .expect("the workspace holds the record");
    assert!(opened.unmerged.is_empty(), "{:?}", opened.unmerged);
    AutoCommit::load(&opened.document).unwrap()
}

fn read_value(owner: &NativeWorkspaceOwner, room: RoomId, keys: &ChannelContentKeys) -> String {
    let read = read_document(owner, room, keys);
    read.get(ROOT, "value")
        .unwrap()
        .expect("the record has a value")
        .0
        .to_string()
}

/// What `owner` publishes for `room` now.
fn outgoing(owner: &NativeWorkspaceOwner, room: RoomId, now: SystemTime) -> Vec<NeighbourMsg> {
    owner
        .with_outgoing(room, now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| postcard::from_bytes(entry.bytes()).unwrap())
                .collect())
        })
        .unwrap()
}

/// Let `member` invite into `room`, as the workspace's maker allows it.
fn invite_grant(
    maker: &DeviceCert,
    member: &DeviceCert,
    room: &RoomId,
) -> tilo_action_policy::Grant {
    tilo_action_policy::Grant::new(
        "invite:guest",
        tilo_action_policy::Subject::person(hex::encode(member.user_public_key)),
        tilo_action_policy::roles::governance_action(
            &room.to_string(),
            tilo_action_policy::roles::governance::INVITE,
        ),
        tilo_action_policy::KindSet::of(tilo_action_policy::roles::GOVERNANCE_KIND),
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Subject::person(hex::encode(maker.user_public_key)),
    )
}
