//! The harness every store-link test shares: certified devices, localhost
//! endpoints, the record's sealed writes and checkpoints, and the join
//! exchange a workspace's device answers.

#![allow(
    dead_code,
    reason = "each test module uses a different part of the shared harness"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard; a fixture keeps its scenario in one body"
)]

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
use tilo_desktop_model::WorkspaceId;
use tilo_identity_host::custody::HostIdentity;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::join::invitations::IssueInvitationRequest;
use tilo_peer_state::join::{self, MemberRole, TransportHint};
use tilo_peer_state::join_answer::JoinAnswer;
use tilo_peer_state::owner::{
    ApplicationRecord, NativeWorkspaceOwner, OwnerJoinWorkspaces, open_file, testing,
};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
use tilo_relay_session::workspace_join::{WorkspaceJoinClient, WorkspaceJoinProtocol};
use tilo_store_link::{StoreSession, workspace_of};
use tilo_store_node::{ALPN, Appetite, FixedAccess, Store, StoreNode};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

pub const APP: &str = "test.app";
pub const RECORD: &str = "tab:1";
pub const SECOND: u64 = 1_000_000_000;

/// A device key `seed` certified by `root`.
pub fn device(
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

/// A localhost endpoint with no relay whose identity is the device `key`.
pub async fn endpoint(key: &SigningKey) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .secret_key(SecretKey::from_bytes(&key.to_bytes()))
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

pub fn addr(endpoint: &Endpoint) -> EndpointAddr {
    EndpointAddr::new(endpoint.id()).with_ip_addr(std::net::SocketAddr::from((
        std::net::Ipv4Addr::LOCALHOST,
        endpoint.bound_sockets().first().unwrap().port(),
    )))
}

pub fn ns(now: SystemTime) -> u64 {
    u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap()
}

/// The address at which `endpoint` answers the workspace's joins.
pub fn hint(
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

pub fn custody(cert: &DeviceCert, key: &SigningKey) -> HostIdentity {
    HostIdentity {
        device_cert: cert.clone(),
        device_signing_key: key.clone(),
        workspace_device_certs: HashMap::default(),
        workspace_revocations: HashMap::default(),
        identity_paths: None,
    }
}

pub fn channel(workspace: &str) -> ChannelId {
    collab_types::document_channel_id(APP, workspace, RECORD)
}

pub fn artifact(workspace: &str) -> ArtifactId {
    collab_types::document_artifact_id(APP, workspace, RECORD)
}

pub fn sealer(
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

pub fn opener(keys: &ChannelContentKeys) -> impl FnMut(&SignedOp) -> Result<Vec<u8>, String> {
    let keys = keys.clone();
    move |op| {
        let metadata = op.encryption.ok_or("not sealed")?;
        let ciphertext = collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?;
        keys.open_content(op.artifact, &metadata, &ciphertext)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    }
}

pub fn seq(message: &NeighbourMsg) -> u64 {
    match message {
        NeighbourMsg::AppDocument { envelope, .. } => envelope.seq,
        _ => 0,
    }
}

/// Invite `guest` into the workspace, redeemed at the device that answers joins.
pub fn invitation(
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
pub async fn joins(
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

/// Publish `change` to the record as `key`'s sealed write.
pub fn publish(
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
pub fn compact(
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

/// The record's document as `owner` holds it.
pub fn read_document(
    owner: &NativeWorkspaceOwner,
    workspace_id: WorkspaceId,
    keys: &ChannelContentKeys,
) -> AutoCommit {
    let opened = owner
        .with_current(workspace_id, |view| {
            view.peer()
                .publication()
                .opened_document(APP, RECORD, &mut opener(keys))
        })
        .unwrap()
        .expect("the workspace holds the record");
    assert!(opened.unmerged.is_empty(), "{:?}", opened.unmerged);
    AutoCommit::load(&opened.document).unwrap()
}

/// The record's value as `owner` reads it.
pub fn read_value(
    owner: &NativeWorkspaceOwner,
    workspace_id: WorkspaceId,
    keys: &ChannelContentKeys,
) -> String {
    read_document(owner, workspace_id, keys)
        .get(ROOT, "value")
        .unwrap()
        .expect("the record has a value")
        .0
        .to_string()
}

/// What `owner` publishes for `workspace_id` now.
pub fn outgoing(
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

/// Let `member` invite into the workspace, as the maker allows it.
pub fn invite_grant(
    maker: &DeviceCert,
    member: &DeviceCert,
    workspace_id: &WorkspaceId,
) -> tilo_action_policy::Grant {
    tilo_action_policy::Grant::new(
        "invite:guest",
        tilo_action_policy::Subject::person(hex::encode(member.user_public_key)),
        tilo_action_policy::roles::governance_action(
            &workspace_id.to_string(),
            tilo_action_policy::roles::governance::INVITE,
        ),
        tilo_action_policy::KindSet::of(tilo_action_policy::roles::GOVERNANCE_KIND),
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Subject::person(hex::encode(maker.user_public_key)),
    )
}

/// Let `member` invite, and let it hold what that governance says.
pub fn grant_invite(
    maker: &NativeWorkspaceOwner,
    maker_cert: &DeviceCert,
    member_cert: &DeviceCert,
    member: &NativeWorkspaceOwner,
    workspace_id: WorkspaceId,
    key: &SigningKey,
    now: SystemTime,
) {
    maker
        .commit_workspace_membership_command(
            OwnerCommitRequest {
                workspace: workspace_id.to_string(),
                operation: CommitOperation::WorkspaceMembershipCommand {
                    invocation: uuid::Uuid::new_v4(),
                    caller: maker_cert.clone(),
                    command: WorkspaceMembershipCommand::Grant {
                        grant: invite_grant(maker_cert, member_cert, &workspace_id),
                    },
                },
            },
            key,
            now,
        )
        .unwrap();
    let author = maker_cert.device_public_key;
    for message in outgoing(maker, workspace_id, now) {
        // What the member already holds is an exact retry, and admits as one.
        let _report = member
            .receive(
                workspace_id,
                TransportOrigin::RetainedRelayDelta(author),
                message,
                None,
                now,
            )
            .unwrap();
    }
}

/// Every byte the store device holds, file by file.
pub fn held_bytes(root: &std::path::Path, found: &mut Vec<Vec<u8>>) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            held_bytes(&path, found);
        } else {
            found.push(std::fs::read(&path).unwrap());
        }
    }
}

/// Whether any byte the store device holds contains `needle`.
pub fn held_contains(root: &std::path::Path, needle: &[u8]) -> bool {
    let mut kept = Vec::new();
    held_bytes(root, &mut kept);
    kept.iter()
        .any(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
}

/// A workspace its maker's device holds, its record written and folded into a
/// sealed checkpoint, with a store device that admits the workspace's members
/// and keeps what it is given.
pub struct Held {
    pub directory: tempfile::TempDir,
    pub now: SystemTime,
    pub author: [u8; 32],
    pub a_key: SigningKey,
    pub a_cert: DeviceCert,
    pub a_endpoint: Endpoint,
    pub workspace_id: WorkspaceId,
    pub workspace: u64,
    pub keys: ChannelContentKeys,
    pub b_keys: ChannelContentKeys,
    pub c_keys: ChannelContentKeys,
    pub artifact: ArtifactId,
    pub document: AutoCommit,
    pub store: Arc<Store>,
    pub store_endpoint: Endpoint,
    pub b_key: SigningKey,
    pub b_cert: DeviceCert,
    pub c_key: SigningKey,
    pub c_cert: DeviceCert,
    /// The store device's router, held for as long as the workspace is.
    pub store_router: iroh::protocol::Router,
    maker: Option<NativeWorkspaceOwner>,
    session: Option<StoreSession>,
    maker_router: Option<iroh::protocol::Router>,
}

impl Held {
    /// The maker's device, which holds the workspace on it.
    pub fn maker(&self) -> &NativeWorkspaceOwner {
        self.maker.as_ref().expect("the maker's device is here")
    }

    /// The maker's session with the store.
    pub fn session(&mut self) -> &mut StoreSession {
        self.session.as_mut().expect("the maker's device is here")
    }

    /// The maker's device is gone: its workspace, its join exchange and its
    /// session with the store with it.
    pub async fn maker_goes_away(&mut self) {
        self.session = None;
        self.maker = None;
        self.maker_router = None;
        self.a_endpoint.clone().close().await;
    }

    /// Hand the store every frame the maker's device holds for the workspace
    /// that the store has not been given yet. Answers how many it kept.
    pub async fn keep(&mut self) -> usize {
        let mut session = self.session.take().expect("the maker's device is here");
        let kept = session
            .keep_workspace(self.maker(), self.workspace_id, self.now)
            .await
            .unwrap();
        self.session = Some(session);
        kept
    }

    /// Take in everything the store holds for the workspace that the maker's
    /// device has not read yet. Answers how many frames it admitted.
    pub async fn catch_up(&mut self) -> usize {
        let mut session = self.session.take().expect("the maker's device is here");
        let admitted = session
            .catch_up_workspace(self.maker(), self.workspace_id, self.now)
            .await
            .unwrap();
        self.session = Some(session);
        admitted
    }
}

/// Build a [`Held`] workspace: four devices certified by the seeds from `seed`
/// on, `title` founded on the first device, its record written as `initial`
/// (value, actor label) and folded into a sealed checkpoint, and a store
/// device keeping it from the first frame on.
pub async fn held(directory: tempfile::TempDir, seed: u8, title: &str, initial: &str) -> Held {
    let a_root = SigningKey::from_bytes(&[seed; 32]);
    let b_root = SigningKey::from_bytes(&[seed + 10; 32]);
    let c_root = SigningKey::from_bytes(&[seed + 20; 32]);
    let store_root = SigningKey::from_bytes(&[seed + 30; 32]);
    let (a_key, a_cert) = device(&a_root, seed + 1, DeviceCapabilities::primary());
    let (b_key, b_cert) = device(&b_root, seed + 2, DeviceCapabilities::member());
    let (c_key, c_cert) = device(&c_root, seed + 3, DeviceCapabilities::member());
    let (store_key, _) = device(&store_root, seed + 4, DeviceCapabilities::host());
    let now = SystemTime::now();
    let author = a_key.verifying_key().to_bytes();

    // The maker founds the workspace on its own device: no host service, no
    // host of anyone else's.
    let a = open_file(&directory.path().join("a.json")).unwrap();
    let workspace_id = testing::founded(&a, title, &a_cert, &a_key, now).unwrap();
    let workspace = workspace_of(&workspace_id.to_string()).unwrap();
    assert!(
        a.with_current(workspace_id, |view| Ok(view.hosted_here()))
            .unwrap(),
        "the device that made the workspace keeps it"
    );

    // Its record, and its keys: the members' alone, never the store's.
    let record = ApplicationRecord::host(APP, RECORD).unwrap();
    a.prepare_application_record(workspace_id, &a_cert, &a_key, &record, now)
        .unwrap();
    let group = ChannelGroup::create(
        channel(&workspace_id.to_string()),
        MemberKey::generate("owner").unwrap(),
    )
    .unwrap();
    let keys = group.content_keys().unwrap();
    let artifact = artifact(&workspace_id.to_string());

    // The maker writes, sealed, and folds the record into a sealed checkpoint:
    // what a store device keeps of it is ciphertext from the first frame on.
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", initial).unwrap();
    document.commit_edit("fixture", "value");
    let change = document.save_incremental();
    publish(&a, workspace_id, &change, &keys, artifact, &a_key, now);
    compact(&a, workspace_id, &keys, artifact, &a_key, now);

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
    let store_router = iroh::protocol::Router::builder(store_endpoint.clone())
        .accept(ALPN, StoreNode::new(store.clone(), Arc::new(access)))
        .spawn();

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
        "the store keeps the sealed checkpoint of the record"
    );
    let lineages = store.lineages(workspace);
    assert_eq!(lineages.len(), 1, "the store holds one sealed line");
    let (_, first, last) = lineages.first().unwrap();
    assert_eq!(
        first, last,
        "the store holds the checkpoint alone: it retired the write it covers"
    );

    Held {
        directory,
        now,
        author,
        a_key,
        a_cert,
        a_endpoint,
        workspace_id,
        workspace,
        keys: keys.clone(),
        b_keys: keys.clone(),
        c_keys: keys,
        artifact,
        document,
        store,
        store_endpoint,
        b_key,
        b_cert,
        c_key,
        c_cert,
        store_router,
        maker: Some(a),
        session: Some(a_session),
        maker_router: Some(a_router),
    }
}

/// `guest` joins the workspace through `holder`, whose device is the only one
/// left that answers joins, and reads the record back from the store alone.
/// Answers what the guest reads.
pub async fn a_later_guest_reads(
    holder: &NativeWorkspaceOwner,
    holder_cert: &DeviceCert,
    holder_key: &SigningKey,
    holder_endpoint: &Endpoint,
    guest: (&DeviceCert, &SigningKey, &ChannelContentKeys, &str),
    t: &Held,
) -> String {
    let _holder_router = iroh::protocol::Router::builder(holder_endpoint.clone())
        .accept(
            WORKSPACE_JOIN_ALPN,
            WorkspaceJoinProtocol::new(JoinAnswer(OwnerJoinWorkspaces::new(
                holder.clone(),
                holder_key.clone(),
            ))),
        )
        .spawn();
    let (guest_cert, guest_key, guest_keys, name) = guest;
    let guest_owner = open_file(&t.directory.path().join(format!("{name}.json"))).unwrap();
    let guest_endpoint = endpoint(guest_key).await;
    joins(
        guest_endpoint.clone(),
        guest_cert,
        guest_key,
        t.workspace,
        invitation(
            holder,
            &t.workspace_id,
            holder_cert,
            holder_key,
            guest_cert,
            hint(
                holder_endpoint,
                holder_cert,
                holder_key,
                &t.workspace_id,
                t.workspace,
            ),
            t.now,
        ),
        &guest_owner,
        t.now,
    )
    .await;
    let mut session = StoreSession::join(
        &guest_endpoint,
        addr(&t.store_endpoint),
        guest_cert.clone(),
        Vec::new(),
        t.workspace,
    )
    .await
    .unwrap();
    session
        .catch_up_workspace(&guest_owner, t.workspace_id, t.now)
        .await
        .unwrap();
    read_value(&guest_owner, t.workspace_id, guest_keys)
}
