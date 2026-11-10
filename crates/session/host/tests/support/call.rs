//! A workspace its host governs, with a call on a chat's voice channel: Alice
//! owns the workspace from her laptop (a native iroh device) and starts the call;
//! people she invites take part from browser sessions. Included with
//! voice test binaries; call binaries re-exec as the automerge worker.

#![allow(
    dead_code,
    reason = "each voice test binary uses a different subset of these fixtures"
)]

use std::time::SystemTime;

use automerge::AutoCommit;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::join::{MemberRole, decode_and_verify_invitation};
use tilo_peer_state::{StatePeer, TransportOrigin};
use tilo_relay::RelayClient;
use tilo_relay::coedit::{Inbound, RelayCoeditSession};
use tilo_relay::daemon::Relay;
use tilo_relay::web::WebHandle;
use tilo_relay::workspace_join::WorkspaceJoinClient;
use tilo_relay::workspaces::{IssueInvitationRequest, IssuedInvitation};
use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::voice::{CallOperation, CallState, Directions, VoiceGrant};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};

use crate::support::{Device, TestHost, enrol, start_workspace_host, wait_for};

pub const CHAT: &str = "so.tilo.chat";
pub const BOTH: Directions = Directions {
    publish: true,
    subscribe: true,
};

/// An endpoint whose key is the device's own, as a device dials with.
pub async fn endpoint(key: &SigningKey) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .bind()
        .await
        .unwrap()
}

/// The next envelope from the browser session that `pick` accepts.
pub async fn browser<T>(link: &RelayClient, mut pick: impl FnMut(Envelope) -> Option<T>) -> T {
    tilo_test_env::within("the host answers the browser", async {
        loop {
            if let Some(value) = pick(link.recv().await.expect("session open")) {
                return value;
            }
        }
    })
    .await
}

pub fn granted(envelope: Envelope) -> Option<VoiceGrant> {
    match envelope {
        Envelope::VoiceGranted { grant, .. } => Some(*grant),
        Envelope::VoiceRefused { reason, .. } => panic!("refused: {reason}"),
        _ => None,
    }
}

/// Admit `device`, one of the relay's own person's, to `workspace` as its
/// owner's.
pub async fn enter(relay: &Relay, device: &DeviceCert, workspace: &str) {
    relay
        .workspaces()
        .unwrap()
        .enter(workspace, device)
        .await
        .unwrap();
}

/// An Editor's invitation to `workspace` for `person` only, which the relay
/// issues for the workspace's owner `issuer`.
pub async fn issue(
    relay: &Relay,
    issuer: &DeviceCert,
    workspace: &str,
    person: &SigningKey,
) -> IssuedInvitation {
    let person = person.verifying_key().to_bytes();
    relay
        .workspaces()
        .unwrap()
        .issue(
            workspace,
            issuer,
            IssueInvitationRequest {
                request_id: person,
                role: MemberRole::Editor,
                recipient_user_public_key: Some(person),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(600_000_000_000)
                    .unwrap(),
                inviter_name: None,
            },
        )
        .await
        .unwrap()
}

/// Invite `person` to `workspace` as an editor, for the workspace's owner `issuer`,
/// and redeem the invitation with their device `page`.
pub async fn invite(
    relay: &Relay,
    issuer: &DeviceCert,
    workspace: &str,
    person: &SigningKey,
    page: &Device,
) {
    let invitation = issue(relay, issuer, workspace, person).await;
    let verified = decode_and_verify_invitation(
        &invitation.signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let mut request = [0; 32];
    getrandom::fill(&mut request).unwrap();
    WorkspaceJoinClient::new(endpoint(&page.key).await)
        .redeem_hint(
            verified.grant().transport.first().unwrap(),
            invitation.signed_invitation.clone(),
            page.cert.clone(),
            &page.key,
            request,
        )
        .await
        .unwrap();
}

pub struct GovernedCall {
    pub alice: SigningKey,
    pub host: Device,
    pub laptop: Device,
    pub relay: Relay,
    pub web: WebHandle,
    pub cluster_id: String,
    pub workspace_id: u64,
    /// Alice's laptop's session with the workspace.
    pub owner: RelayClient,
    /// The chat's voice channel the call is on.
    pub channel: String,
    pub call: CallState,
    _state: tempfile::TempDir,
}

/// The next envelope from the native device that `pick` accepts.
pub async fn native<T>(client: &RelayClient, mut pick: impl FnMut(Envelope) -> Option<T>) -> T {
    tilo_test_env::within("the host answers the native device", async {
        loop {
            if let Some(value) = pick(client.recv().await.expect("connection open")) {
                return value;
            }
        }
    })
    .await
}

/// Ask the host to change a call as the native device `client`.
pub async fn operate(
    client: &RelayClient,
    workspace_id: u64,
    request: u8,
    operation: CallOperation,
) -> Result<CallState, String> {
    client
        .send(Envelope::VoiceCall {
            workspace_id,
            request: [request; 16],
            operation,
        })
        .unwrap();
    native(client, |envelope| match envelope {
        Envelope::VoiceCallResult {
            request: answered,
            result,
            ..
        } if answered == [request; 16] => Some(result),
        _ => None,
    })
    .await
}

/// Create the workspace's chat record with a voice channel from `device`'s
/// replica, over its own relay session, which the caller keeps open: local
/// publication never creates a record.
pub async fn create_voice_channel(
    address: iroh::EndpointAddr,
    workspace_id: u64,
    workspace: &str,
    host: &DeviceCert,
    device: &Device,
) -> (RelayCoeditSession, String) {
    let session = RelayCoeditSession::join(
        RelayClient::connect_to_pinned_host(
            endpoint(&device.key).await,
            address,
            device.cert.clone(),
            device.key.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap(),
        workspace_id,
        device.cert.device_public_key,
    )
    .unwrap();
    let mut peer = StatePeer::new(
        workspace,
        host.clone(),
        vec![host.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    tilo_test_env::within("the replica learns the workspace", async {
        while peer
            .permissions(device.cert.device_public_key, SystemTime::now())
            .is_err()
        {
            if let Some(Inbound::Delta { author, data }) = session.recv().await {
                let message: NeighbourMsg = postcard::from_bytes(&data).unwrap();
                peer.receive_from(
                    TransportOrigin::RetainedRelayDelta(author),
                    message,
                    SystemTime::now(),
                )
                .unwrap();
            }
        }
    })
    .await;
    let mut chat = AutoCommit::new();
    chat::demo::start(&mut chat, "fixture", "Fixture", "F").unwrap();
    let voice = chat::projection::read(&chat)
        .channel_rows()
        .into_iter()
        .find(|channel| channel.kind == chat::model::ChannelKind::Voice)
        .expect("a workspace's chat opens with a voice channel")
        .id;
    let message = peer
        .publication_mut()
        .publish_incremental(
            CHAT,
            "",
            chat.save_after(&[]),
            &device.key,
            SystemTime::now(),
        )
        .unwrap();
    let (channel, kind) = neighbour_wire::retention(&message, false).unwrap();
    let body = postcard::to_stdvec(&message).unwrap();
    if kind == neighbour_wire::Retention::Checkpoint {
        session.publish_checkpoint(channel, body)
    } else {
        session.publish_retained_delta(channel, body)
    }
    .unwrap();
    (session, voice)
}

impl GovernedCall {
    /// Alice's governed workspace with an active call on its chat's voice
    /// channel, which nobody has joined yet.
    pub async fn start() -> Self {
        let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
        let host = enrol(&alice, "alice-home", DeviceCapabilities::host());
        let laptop = enrol(&alice, "alice-laptop", DeviceCapabilities::primary());
        let state = tempfile::tempdir().unwrap();
        let TestHost { relay, web } =
            start_workspace_host(&alice, &host, state.path(), false).await;
        let address = relay.node_addr();
        let workspace = relay.workspaces().unwrap().create("Standup").await.unwrap();
        enter(&relay, &laptop.cert, &workspace.workspace).await;

        // Alice's chat gets a voice channel; the workspace's governor learns it.
        let (owner, channel) = create_voice_channel(
            address,
            workspace.workspace_id,
            &workspace.workspace,
            &host.cert,
            &laptop,
        )
        .await;
        let owner = owner.into_client();
        let call = wait_for("the governor to bind the call", || {
            let owner = &owner;
            let channel = channel.clone();
            async move {
                operate(
                    owner,
                    workspace.workspace_id,
                    1,
                    CallOperation::Start {
                        app_id: CHAT.into(),
                        record_id: String::new(),
                        channel_id: channel,
                        allowed: BOTH,
                    },
                )
                .await
                .ok()
            }
        })
        .await;
        assert!(call.active);
        Self {
            alice,
            host,
            laptop,
            relay,
            web,
            cluster_id: workspace.workspace,
            workspace_id: workspace.workspace_id,
            owner,
            channel,
            call,
            _state: state,
        }
    }

    /// A browser device of a new person Alice invited to the workspace, which
    /// redeemed the invitation.
    pub async fn invite(&self, label: &str) -> (SigningKey, Device) {
        let person = SigningKey::generate(&mut UnwrapErr(SysRng));
        let page = enrol(&person, label, DeviceCapabilities::member());
        invite(
            &self.relay,
            &self.laptop.cert,
            &self.cluster_id,
            &person,
            &page,
        )
        .await;
        (person, page)
    }

    /// The host as a browser session reaches it.
    pub fn iroh(&self) -> tilo_session_host::Host {
        tilo_session_host::Host::at(self.relay.node_addr())
    }

    /// A raw session the device behind `cert` and `key` dials from its own
    /// iroh key, as a native device does: what a test drives envelopes with.
    pub async fn session(&self, cert: &DeviceCert, key: &SigningKey) -> RelayClient {
        RelayClient::connect_to_pinned_host(
            endpoint(key).await,
            self.relay.node_addr(),
            cert.clone(),
            key.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap()
    }

    pub async fn shutdown(self) {
        drop(self.web);
        self.relay.shutdown().await;
    }
}
