//! A governing host and the devices around it, for the tests of workspaces and of
//! the planes embedders build on them.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture failing is its test failing"
)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
use collab_types::ChannelRoute;
use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

use super::{ChannelKeys, ChannelKeysAnswerFuture, DeviceChannels, PreAdmitRequest, Workspaces};
use crate::host::{HostConfig, HostIdentity, HostMode, WorkspaceHost};

/// A host certified only to host workspaces: it governs them without taking part in
/// their channels.
#[must_use]
pub fn governor() -> (SigningKey, DeviceCert) {
    governor_with(DeviceCapabilities::host())
}

/// A host device of the person whose root is `[1; 32]`, certified with
/// `capabilities`.
///
/// # Panics
///
/// The certificate cannot be signed.
#[must_use]
pub fn governor_with(capabilities: DeviceCapabilities) -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let key = SigningKey::from_bytes(&rand::random());
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "home".into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).expect("a fixture certificate signs");
    (key, cert)
}

/// A certified workspace host on its own endpoint, routed as an embedder routes
/// it.
pub struct TestHost {
    host: WorkspaceHost,
    router: Router,
}

impl TestHost {
    /// The workspaces it governs.
    #[must_use]
    pub const fn workspaces(&self) -> &Workspaces {
        self.host.workspaces()
    }

    /// The host itself.
    #[must_use]
    pub const fn host(&self) -> &WorkspaceHost {
        &self.host
    }

    /// The durable records of its workspaces.
    #[must_use]
    pub fn workspace_records(&self) -> crate::workspace_records::WorkspaceRecords {
        self.host.workspace_records()
    }

    /// What a member dials.
    #[must_use]
    pub fn node_addr(&self) -> iroh_base::EndpointAddr {
        self.host.node_addr()
    }

    /// The compactor of its workspaces.
    #[cfg(all(test, feature = "agents"))]
    pub(crate) fn compactor(&self) -> &crate::compaction::Compactor {
        self.host.compactor()
    }

    /// Stop the host, then its router and endpoint.
    pub async fn shutdown(self) {
        self.host.shutdown().await;
        let _closed = self.router.shutdown().await;
    }
}

/// The channel service of a device that takes part in its workspaces' channels:
/// it opens a channel's keys the first time something starts the channel's
/// record, as the channel service of the device a page or a bot runs on does,
/// and answers with the same keys every time after.
#[derive(Default)]
pub struct TestChannelKeys {
    held: Mutex<BTreeMap<(String, String, String), ChannelContentKeys>>,
}

impl ChannelKeys for TestChannelKeys {
    fn channel_keys<'a>(
        &'a self,
        workspace: &'a str,
        route: &'a ChannelRoute,
        create: bool,
    ) -> ChannelKeysAnswerFuture<'a> {
        Box::pin(async move {
            let named = (
                workspace.to_owned(),
                route.app.clone(),
                route.record.clone(),
            );
            let held = {
                let keys = self.held.lock().expect("the fixture's keys");
                keys.get(&named).cloned()
            };
            if let Some(keys) = held {
                return Some(Ok(keys));
            }
            if !create {
                return Some(Err("this channel has no keys yet".into()));
            }
            let name = format!("{workspace}/{}/{}", route.app, route.record);
            let keys = ChannelGroup::create(
                route.channel(workspace),
                MemberKey::generate(&name).expect("a fixture member key"),
            )
            .expect("a fixture channel opens")
            .content_keys()
            .expect("a fresh channel holds its epoch's keys");
            self.held
                .lock()
                .expect("the fixture's keys")
                .insert(named, keys.clone());
            Some(Ok(keys))
        })
    }
}

/// A certified host of `cert`'s device keeping its state under `state_root`.
///
/// # Panics
///
/// The endpoint cannot bind or the host cannot open.
pub async fn test_host(state_root: &Path, key: &SigningKey, cert: &DeviceCert) -> TestHost {
    test_host_with_grants(state_root, key, cert, None).await
}

/// The same host, keeping `grants` pre-admissions per workspace
/// ([`crate::host::HostConfig::max_workspace_grants`]).
///
/// # Panics
///
/// The endpoint cannot bind or the host cannot open.
pub async fn test_host_with_grants(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
    grants: Option<usize>,
) -> TestHost {
    host_on(state_root, key, cert, None, grants).await
}

/// The same host, run on a device that takes part in its workspaces' channels,
/// as a relay whose device owns workspaces does: what its members seal it opens,
/// and what it writes itself it seals.
///
/// # Panics
///
/// The endpoint cannot bind or the host cannot open.
pub async fn test_host_taking_part(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
) -> TestHost {
    test_host_holding(state_root, key, cert, Arc::default()).await
}

/// [`test_host_taking_part`] whose device holds `keys`: the same keys open
/// what a host before it on the same state sealed, as a device's channel
/// service keeps its keys across a restart.
///
/// # Panics
///
/// The endpoint cannot bind or the host cannot open.
pub async fn test_host_holding(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
    keys: Arc<TestChannelKeys>,
) -> TestHost {
    host_on(
        state_root,
        key,
        cert,
        Some(DeviceChannels { keys, sink: None }),
        None,
    )
    .await
}

/// `cert`'s device hosting under `state_root`, taking part in the workspaces'
/// channels through `channels` when it is given.
async fn host_on(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
    channels: Option<DeviceChannels>,
    grants: Option<usize>,
) -> TestHost {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .bind()
        .await
        .expect("a loopback endpoint binds");
    let host = WorkspaceHost::open(
        HostConfig {
            max_workspace_grants: grants,
            ..HostConfig::new(state_root.to_owned(), HostMode::CertifiedHost)
        },
        HostIdentity {
            user_public_key: cert.user_public_key,
            device_signing_key: key.clone(),
            device_cert: cert.clone(),
            revocations: RevocationList::new(),
        },
        endpoint.clone(),
        channels,
    )
    .await
    .expect("the host opens");
    let mut router = Router::builder(endpoint);
    for (alpn, handler) in host.protocols() {
        router = router.accept(alpn, handler);
    }
    TestHost {
        host,
        router: router.spawn(),
    }
}

/// Admit `member` to `workspace` as a reader, as the governor `key` decides it.
///
/// # Panics
///
/// The workspace is not governed here, or refuses the certificate.
pub async fn admit_view_reader(
    workspaces: &Workspaces,
    workspace: &str,
    key: &SigningKey,
    member: &DeviceCert,
) {
    let state = workspaces
        .workspace(workspace)
        .await
        .expect("a governed room");
    let mut workspace = state.lock().await;
    let now = SystemTime::now();
    let mut candidate = workspace.peer().clone();
    candidate
        .admit_certificate(member.clone(), key, now)
        .expect("the room admits the certificate");
    let collaboration = workspace.collaboration().clone();
    workspace
        .commit(candidate, key, now, |_, _| Ok(collaboration))
        .expect("the admission commits");
}

/// Project `workspace`'s members onto the relay's own runtime for it, as a
/// settled decision does: what a member's change is decided against on its way
/// in, before the workspace's governance reaches the relay by its own path.
///
/// # Panics
///
/// The workspace is not governed here, or its governance does not read.
pub async fn project_standings(workspaces: &Workspaces, workspace: &str) {
    let state = workspaces
        .workspace(workspace)
        .await
        .expect("a governed room");
    let (standings, workspace_id) = {
        let governed = state.lock().await;
        let standings = governed
            .peer()
            .governance()
            .governed_standings(SystemTime::now())
            .expect("the workspace's standings");
        (standings, governed.workspace_id())
    };
    workspaces
        .inner
        .host
        .access
        .replace_standings(workspace_id, true, standings)
        .await
        .expect("the relay projects the workspace's standings");
}

/// Until when `workspace` restricts `person`, as `host` reads its policy.
///
/// # Panics
///
/// The workspace is not governed here.
pub async fn restricted_until(
    workspaces: &Workspaces,
    workspace: &str,
    host: &DeviceCert,
    person: [u8; 32],
) -> Option<u64> {
    let state = workspaces
        .workspace(workspace)
        .await
        .expect("a governed room");
    let now = SystemTime::now();
    let permissions = state
        .lock()
        .await
        .peer()
        .permissions(host.device_public_key, now)
        .expect("the host's permissions");
    permissions.policy.restricted_until(
        &hex::encode(person),
        &tilo_action_policy::Context::at_time(now),
    )
}

/// A pre-admission of person "Ada" to `workspace` as an editor for `token`,
/// signed by the owner whose root is `[1; 32]`.
///
/// # Panics
///
/// The grant cannot be digested.
#[must_use]
pub fn signed_pre_admission(
    workspace: &str,
    token: &[u8],
) -> tilo_relay_protocol::workspaces::PreAdmission {
    signed_pre_admission_as(
        workspace,
        token,
        tilo_relay_protocol::workspaces::WorkspaceRole::Editor,
    )
}

/// A pre-admission of person "Ada" to `workspace` with `role` for `token`, signed
/// by the owner whose root is `[1; 32]`.
///
/// # Panics
///
/// The grant cannot be digested.
#[must_use]
pub fn signed_pre_admission_as(
    workspace: &str,
    token: &[u8],
    role: tilo_relay_protocol::workspaces::WorkspaceRole,
) -> tilo_relay_protocol::workspaces::PreAdmission {
    use ed25519_dalek::Signer as _;
    use sha2::Digest as _;
    let owner = SigningKey::from_bytes(&[1; 32]);
    let mut grant = tilo_relay_protocol::workspaces::PreAdmission {
        workspace: workspace.to_owned(),
        role,
        token_sha256: sha2::Sha256::digest(token).into(),
        label: "Ada".into(),
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
        owner_user_public_key: owner.verifying_key().to_bytes(),
        signature: [0; 64],
    };
    grant.signature = owner.sign(&grant.digest().unwrap()).to_bytes();
    grant
}

/// One person's device joining `workspace` through its door, as a Tilo computer
/// does: the door's invitation, the challenge it answers, then the Complete,
/// which admits it. Answers the Complete it sent and the host's answer.
///
/// # Panics
///
/// The door does not admit the device.
pub async fn door_join(
    workspaces: &Workspaces,
    host: &DeviceCert,
    workspace: &str,
    (device, device_cert): (&SigningKey, &DeviceCert),
    seed: u8,
) -> (
    tilo_peer_state::join::JoinTransportRequest,
    tilo_peer_state::join::JoinTransportResponse,
) {
    let role = tilo_relay_protocol::workspaces::WorkspaceRole::Editor;
    door_join_as(
        workspaces,
        host,
        workspace,
        (device, device_cert),
        seed,
        role,
    )
    .await
}

/// One person's device joining `workspace` through its door with `role`, as a Tilo
/// computer does: the door's invitation, the challenge it answers, then the
/// Complete, which admits it. Answers the Complete it sent and the host's
/// answer.
///
/// # Panics
///
/// The door does not admit the device.
pub async fn door_join_as(
    workspaces: &Workspaces,
    host: &DeviceCert,
    workspace: &str,
    (device, device_cert): (&SigningKey, &DeviceCert),
    seed: u8,
    role: tilo_relay_protocol::workspaces::WorkspaceRole,
) -> (
    tilo_peer_state::join::JoinTransportRequest,
    tilo_peer_state::join::JoinTransportResponse,
) {
    use tilo_delivery_protocol::workspace_join::WorkspaceJoinService as _;
    use tilo_peer_state::join::{
        JoinChallengeRequest, JoinCompleteRequest, JoinTransportRequest, JoinTransportResponse,
        sign_join_proof,
    };
    let token = [seed; 8];
    let issued = workspaces
        .pre_admit(
            host,
            PreAdmitRequest {
                request_id: [seed; 32],
                grant: signed_pre_admission_as(workspace, &token, role),
                recipient_user_public_key: Some(device_cert.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let service = workspaces.join_service();
    let endpoint = device_cert.device_public_key;
    let JoinTransportResponse::Challenge(answer) = service
        .handle(
            endpoint,
            JoinTransportRequest::Challenge(JoinChallengeRequest {
                signed_invitation: issued.signed_invitation,
                device_cert: device_cert.clone(),
                request_id: [seed.wrapping_add(100); 32],
            }),
        )
        .await
    else {
        panic!("no challenge");
    };
    let proof = sign_join_proof(answer.challenge, device).unwrap();
    let complete = JoinTransportRequest::Complete(JoinCompleteRequest { proof });
    let response = service.handle(endpoint, complete.clone()).await;
    drop(service);
    (complete, response)
}

/// `peer`'s device joins `call` in `workspace` as the workspace's voice door joins
/// it, renewing `participation` when it names one; its grants come on
/// `outbound`.
///
/// # Errors
///
/// The workspace refuses the device the call.
pub async fn voice_join(
    workspaces: &Workspaces,
    workspace: &str,
    peer: &tilo_relay_protocol::AuthenticatedPeer,
    call: [u8; 32],
    participation: Option<[u8; 32]>,
    outbound: tokio::sync::mpsc::Sender<tilo_relay_protocol::Envelope>,
) -> Result<tilo_relay_protocol::voice::VoiceGrant, String> {
    workspaces
        .voice_join(workspace, peer, call, participation, outbound)
        .await
}

/// `workspace`'s calls as `peer`'s device lists them through the voice door; the
/// list's changes come on `outbound`.
///
/// # Errors
///
/// The workspace refuses the device its calls.
pub async fn voice_calls(
    workspaces: &Workspaces,
    workspace: &str,
    peer: &tilo_relay_protocol::AuthenticatedPeer,
    outbound: tokio::sync::mpsc::Sender<tilo_relay_protocol::Envelope>,
) -> Result<Vec<tilo_relay_protocol::voice::CallSummary>, String> {
    workspaces.voice_calls(workspace, peer, outbound).await
}

/// `peer`'s device ends `participation` in `call`, as the voice door ends it.
pub fn voice_leave(
    workspaces: &Workspaces,
    peer: &tilo_relay_protocol::AuthenticatedPeer,
    call: [u8; 32],
    participation: [u8; 32],
) {
    workspaces.voice_leave(peer, call, participation);
}
