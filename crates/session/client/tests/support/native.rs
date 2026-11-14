//! A native device of a workspace, driven by hand. It joins the workspace's
//! group by the Welcome of a member that admits it, as every device but the
//! workspace's creator does, writes records sealed and opens what others seal.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of the device"
)]

use channel_crypto::{Access, ChannelPeer, PeerEvent, VaultKey};
use collab_types::{
    DocumentFrame, Ed25519PublicKey, EncryptionMetadata, SignedOp, document_artifact_id,
};
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay::daemon::Relay;
use tilo_relay::{Inbound, RelayClient, RelayCoeditSession};
use tilo_relay_protocol::WorkspaceId;
use tilo_user_identity::RevocationList;

use crate::support::{Device, is_host};

/// Publish `message` through the host the way a shell does.
pub fn publish(session: &RelayCoeditSession, message: &NeighbourMsg) {
    let (channel, retention) = neighbour_wire::retention(message, false).unwrap();
    let body = postcard::to_allocvec(message).unwrap();
    match retention {
        neighbour_wire::Retention::Checkpoint => session.publish_checkpoint(channel, body),
        neighbour_wire::Retention::Delta => session.publish_retained_delta(channel, body),
    }
    .unwrap();
}

/// `device`'s session with `workspace` on `relay`, announced with its
/// certificate as every device announces itself. `user` is the device's
/// person.
pub async fn connect(
    relay: &Relay,
    user: [u8; 32],
    device: &Device,
    workspace: WorkspaceId,
) -> RelayCoeditSession {
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let client = RelayClient::connect(
        endpoint,
        relay.node_addr(),
        user,
        device.cert.clone(),
        device.key.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    let session =
        RelayCoeditSession::join(client, workspace, device.key.verifying_key().to_bytes()).unwrap();
    publish(
        &session,
        &NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(device.cert.clone()),
            revocations: Vec::new(),
        },
    );
    session
}

pub fn vault() -> VaultKey {
    VaultKey::new([7; 32])
}

pub struct NativeDevice {
    pub device: Device,
    pub peer: ChannelPeer,
    pub session: RelayCoeditSession,
    workspace: WorkspaceId,
    delivery: u64,
    /// Other devices' writes, as they arrived.
    writes: Vec<SignedOp>,
    _state: tempfile::TempDir,
}

impl NativeDevice {
    /// `device` in `workspace` on `relay`, announced, its group's keys not
    /// asked for yet.
    pub async fn start(
        relay: &Relay,
        user: [u8; 32],
        device: Device,
        workspace: WorkspaceId,
    ) -> Self {
        let state = tempfile::tempdir().unwrap();
        let peer = ChannelPeer::waiting(
            Some(std::sync::Arc::new(channel_crypto::Files::new(
                state.path(),
            ))),
            collab_types::WorkspaceId::parse(&workspace.to_string()).unwrap(),
            Box::new(device.key.clone()),
            &vault(),
        )
        .unwrap();
        let session = connect(relay, user, &device, workspace).await;
        Self {
            device,
            peer,
            session,
            workspace,
            delivery: 0,
            writes: Vec::new(),
            _state: state,
        }
    }

    pub fn key(&self) -> [u8; 32] {
        self.device.key.verifying_key().to_bytes()
    }

    fn deliver(&mut self, message: impl FnOnce(String, u64) -> NeighbourMsg) {
        self.delivery = self.delivery.saturating_add(1);
        publish(
            &self.session,
            &message(self.workspace.to_string(), self.delivery),
        );
    }

    /// Ask for the workspace's keys and follow its governance until a
    /// member's Welcome admits this device.
    pub async fn join(&mut self) {
        let request = self
            .peer
            .access_request_now(1, false, &vault())
            .unwrap()
            .unwrap();
        self.deliver(|workspace, delivery| NeighbourMsg::MlsAccessRequest {
            workspace,
            request: Box::new(request),
            delivery,
        });
        while !matches!(self.peer.access(), Access::Ready { .. }) {
            self.next("a member's Welcome").await;
        }
    }

    /// Take the next message of the workspace: follow its governance, and
    /// keep the records other devices write.
    pub async fn next(&mut self, what: &str) {
        let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
        let inbound = tokio::time::timeout_at(deadline, self.session.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .expect("host session open");
        let Inbound::Delta { author, data } = inbound else {
            return;
        };
        match postcard::from_bytes::<NeighbourMsg>(&data) {
            Ok(NeighbourMsg::MlsGovernance { frame, .. }) => {
                match self.peer.follow(*frame, &vault()).unwrap() {
                    PeerEvent::Desynchronised
                    | PeerEvent::AdmittedWithAnotherKey
                    | PeerEvent::Removed => panic!("the device left the workspace's group"),
                    _ => {}
                }
            }
            // The workspace's host writes as well: the tests read what
            // members wrote.
            Ok(NeighbourMsg::AppDocument { envelope, .. })
                if author != self.key() && !is_host(&author) =>
            {
                self.writes.push(*envelope);
            }
            _ => {}
        }
    }

    /// Publish what the group queued, as this device's next governance
    /// delivery.
    pub fn govern(&mut self) {
        let frames = self.peer.outbox().to_vec();
        self.peer.published();
        for frame in frames {
            self.deliver(|workspace, delivery| NeighbourMsg::MlsGovernance {
                workspace,
                frame: Box::new(frame),
                delivery,
            });
        }
    }

    /// `frame` of `route`, sealed under the group's current epoch.
    pub fn seal(&self, route: &DocumentRoute, frame: &[u8]) -> (EncryptionMetadata, Vec<u8>) {
        self.peer
            .content_keys()
            .unwrap()
            .seal_content(
                document_artifact_id(&route.app_id, &route.cluster_id, &route.record),
                collab_types::ContentRetention::of_frame(frame),
                frame,
            )
            .unwrap()
    }

    /// Publish `op`, signed by this device, as its write of `route`.
    pub fn write(&self, route: &DocumentRoute, mut op: SignedOp) {
        collab_types::sign_op(&mut op, &self.device.key).unwrap();
        publish(
            &self.session,
            &NeighbourMsg::AppDocument {
                peer: neighbour_wire::peer_handle(&self.key()),
                app_id: route.app_id.clone(),
                cluster_id: route.cluster_id.clone(),
                record_id: route.record.clone(),
                envelope: Box::new(op),
            },
        );
    }

    /// `author`'s next write of `route`, opened once this device holds the
    /// key it was sealed under: a write may arrive before the commit that
    /// carries its epoch's key. The author never writes a sealed record in
    /// the clear.
    pub async fn written(&mut self, author: [u8; 32], route: &DocumentRoute) -> DocumentFrame {
        let artifact = document_artifact_id(&route.app_id, &route.cluster_id, &route.record);
        let author = Ed25519PublicKey::from_bytes(author);
        loop {
            if let Some(at) = self
                .writes
                .iter()
                .position(|op| op.author == author && op.artifact == artifact)
                && let Some(clear) = self.open(&self.writes[at])
            {
                self.writes.remove(at);
                return DocumentFrame::decode(&clear).unwrap();
            }
            self.next("the sealed write and its key").await;
        }
    }

    /// `op` of `route` opened, or `None` while this device lacks its key.
    fn open(&self, op: &SignedOp) -> Option<Vec<u8>> {
        let metadata = op
            .encryption
            .as_ref()
            .expect("a sealed record is never written in the clear");
        let keys = self.peer.content_keys()?;
        match keys.open_content(op.artifact, metadata, &op.payload) {
            Ok(clear) => Some(clear.to_vec()),
            Err(channel_crypto::Error::MissingContentKey(_)) => None,
            Err(error) => panic!("the write does not open: {error}"),
        }
    }
}
