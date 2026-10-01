//! A room its host governs, with a call on a chat's voice channel: Alice
//! owns the room from her laptop (a native iroh device) and starts the call;
//! people she invites take part from browser sessions. Included with
//! the shared support module; call binaries re-exec as the automerge worker.

use std::time::{Duration, SystemTime};

use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_peer_state::join::{MemberRole, decode_and_verify_invitation};
use tilo_peer_state::{NeighbourMsg, StatePeer, TransportOrigin};
use tilo_relay::coedit::{Inbound, RelayCoeditSession};
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::rooms::{RoomAdminClient, RoomCommand, RoomReply};
use tilo_relay::web::{TlsSource, WebConfig, WebHandle};
use tilo_relay::workspace_join::WorkspaceJoinClient;
use tilo_relay::{RelayClient, RelayConfig};
use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::voice::{CallOperation, CallState, Directions};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::support::{Device, enrol, wait_for};

pub const CHAT: &str = "so.tilo.chat";
pub const CHANNEL: &str = "standup";
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

pub struct GovernedCall {
    pub alice: SigningKey,
    pub host: Device,
    pub laptop: Device,
    pub relay: Relay,
    pub web: WebHandle,
    pub admin: RoomAdminClient,
    pub room: String,
    pub workspace_id: u64,
    /// Alice's laptop's session with the room.
    pub owner: RelayClient,
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

/// Publish the channel through the production native room session.
async fn owner_with_channel(
    laptop: &Device,
    host: &Device,
    room: &str,
    workspace_id: u64,
    address: iroh::EndpointAddr,
) -> RelayClient {
    // Alice's chat gets a voice channel; the room's governor learns it.
    let session = RelayCoeditSession::join(
        RelayClient::connect_to_pinned_host(
            endpoint(&laptop.key).await,
            address,
            laptop.cert.clone(),
            laptop.key.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap(),
        workspace_id,
        laptop.cert.device_public_key,
    )
    .unwrap();
    let mut peer = StatePeer::new(
        room,
        host.cert.clone(),
        vec![host.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    tilo_test_env::within("Alice's replica learns the room", async {
        while peer
            .permissions(laptop.cert.device_public_key, SystemTime::now())
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
    let channels = chat.put_object(ROOT, "channels", ObjType::Map).unwrap();
    let channel = chat.put_object(&channels, CHANNEL, ObjType::Map).unwrap();
    chat.put(&channel, "kind", "voice").unwrap();
    chat.put(&channel, "workspace", "native").unwrap();
    chat.commit();
    let message = peer
        .publish_incremental(
            CHAT,
            "",
            chat.save_after(&[]),
            &laptop.key,
            SystemTime::now(),
        )
        .unwrap();
    let (channel, _) = tilo_peer_state::transport::retention(&message, false).unwrap();
    session
        .publish_retained_delta(channel, postcard::to_stdvec(&message).unwrap())
        .unwrap();
    session.into_client()
}

impl GovernedCall {
    /// Alice's governed room with an active call on its chat's voice
    /// channel, which nobody has joined yet.
    pub async fn start() -> Self {
        let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
        let host = enrol(&alice, "alice-home", DeviceCapabilities::host());
        let laptop = enrol(&alice, "alice-laptop", DeviceCapabilities::primary());
        let state = tempfile::tempdir().unwrap();
        let relay = Relay::start(
            RelayConfig {
                state_root: state.path().to_path_buf(),
                ..RelayConfig::default()
            },
            RelayIdentity {
                user_public_key: alice.verifying_key().to_bytes(),
                device_signing_key: host.key.clone(),
                device_cert: host.cert.clone(),
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        let web = relay
            .serve_web(WebConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                tls: TlsSource::Development {
                    hosts: vec!["localhost".into(), "127.0.0.1".into()],
                    store: None,
                },
                allowed_origins: Vec::new(),
                plain_listen: None,
            })
            .await
            .unwrap();
        let address = relay.node_addr();
        let admin = RoomAdminClient::new(endpoint(&laptop.key).await, laptop.cert.clone());
        let room = admin.create(address.clone(), "Standup").await.unwrap();
        assert!(matches!(
            admin
                .send(
                    address.clone(),
                    RoomCommand::Enter {
                        room: room.room.clone()
                    }
                )
                .await
                .unwrap(),
            RoomReply::Entered { .. }
        ));

        let owner =
            owner_with_channel(&laptop, &host, &room.room, room.workspace_id, address).await;
        let call = wait_for("the governor to bind the call", || {
            let owner = &owner;
            async move {
                operate(
                    owner,
                    room.workspace_id,
                    1,
                    CallOperation::Start {
                        app_id: CHAT.into(),
                        record_id: String::new(),
                        channel_id: CHANNEL.into(),
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
            admin,
            room: room.room,
            workspace_id: room.workspace_id,
            owner,
            call,
            _state: state,
        }
    }

    /// A browser device of a new person Alice invited to the room, which
    /// redeemed the invitation.
    pub async fn invite(&self, label: &str) -> (SigningKey, Device) {
        let person = SigningKey::generate(&mut UnwrapErr(SysRng));
        let page = enrol(&person, label, DeviceCapabilities::member());
        let invitation = self
            .admin
            .invite(
                self.relay.node_addr(),
                &self.room,
                MemberRole::Editor,
                Some(person.verifying_key().to_bytes()),
                Duration::from_secs(600),
            )
            .await
            .unwrap();
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
        (person, page)
    }

    /// The host's HTTPS origin for browser sessions.
    pub fn https(&self) -> std::sync::Arc<crate::support::https::DevHttps> {
        crate::support::https::DevHttps::new(
            self.web.addr,
            self.web.certificate_sha256.as_ref().unwrap(),
        )
    }

    pub async fn shutdown(self) {
        drop(self.web);
        self.relay.shutdown().await;
    }
}
