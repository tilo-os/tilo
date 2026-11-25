//! A native device of a workspace, driven by hand. It joins the workspace's
//! group by the Welcome of a member that admits it, as every device but the
//! workspace's creator does, writes records sealed and opens what others seal.
//!
//! It reaches the workspace over the same link a device's daemon runs: the
//! dialer and the member handshake on the link's first stream, then the
//! workspace's records ([`DirectSync`]), with every frame it publishes and
//! takes in a [`NeighbourMsg`].

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of the device"
)]

use std::sync::Arc;

use collab_types::{
    DocumentFrame, Ed25519PublicKey, EncryptionMetadata, SignedOp, document_artifact_id,
};
use key_group::{Access, GroupPeer, PeerEvent, VaultKey};
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::StatePeer;
use tilo_peer_state::direct::{DirectSync, Heard, Reader};
use tilo_server::daemon::Server;
use tilo_server_client::ServerAddress;
use tilo_server_client::link::{Dialer, Purpose};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::refusal::Refusal;

use crate::support::{Device, is_host};

/// A device's link to its workspace on a host: the device's copy, the frames
/// its link brings, and the task the link runs on.
pub struct DeviceLink {
    pub sync: DirectSync,
    reader: Reader,
    _dialer: Arc<Dialer>,
    running: Option<tokio::task::JoinHandle<Result<(), Refusal>>>,
}

impl DeviceLink {
    /// The next frame the link brings, or `None` once it ended.
    pub async fn recv(&mut self) -> Option<Heard> {
        self.reader.recv().await
    }

    /// Let the link go, as a closed session does.
    pub async fn close(mut self) {
        if let Some(running) = self.running.take() {
            running.abort();
            let _ = running.await;
        }
    }

    /// Read this device's copy of the workspace.
    pub fn read<R>(&self, read: impl FnOnce(&StatePeer) -> R) -> R {
        self.sync.read(read)
    }

    /// Send `message` as this device's write, keeping it where the link carries records.
    pub fn publish(&self, message: &NeighbourMsg) -> Result<(), String> {
        self.sync.publish(|_| Ok(message.clone())).map(|_| ())
    }
}

impl Drop for DeviceLink {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            running.abort();
        }
    }
}

/// `device`'s link to `workspace` on `server`, proving itself on the link's first stream and
/// dialled over it.
///
/// # Panics
///
/// The host holds no such workspace, admitted no such device, or did not answer.
pub async fn connect(
    server: &Server,
    _user: [u8; 32],
    device: &Device,
    workspace: WorkspaceId,
) -> DeviceLink {
    let workspaces = server.workspaces().expect("the host governs workspaces");
    let host = workspaces.host_device();
    let host_cert = workspaces.device_cert().clone();
    let state = workspaces
        .state(&workspace.to_string())
        .await
        .expect("the host holds the workspace");
    let endpoint = test_server::endpoint(iroh::SecretKey::from_bytes(&device.key.to_bytes())).await;
    let dialer = Arc::new(Dialer::from_endpoint(
        ServerAddress::at(server.node_addr()),
        endpoint,
    ));
    let sync = DirectSync::new(state, device.key.clone());
    let link = dialer
        .link(Purpose::Records)
        .await
        .expect("the host answers a member's link");
    link.prove_member(&device.cert, &device.key, &host_cert)
        .await
        .expect("the host admits the device");
    let admitted = sync
        .dial_over(Arc::new(link), host)
        .await
        .expect("the workspace admits the device");
    let reader = sync.reader();
    let running = sync.clone();
    let running = tokio::spawn(async move { admitted.run(&running).await });
    DeviceLink {
        sync,
        reader,
        _dialer: dialer,
        running: Some(running),
    }
}

pub fn vault() -> VaultKey {
    VaultKey::new([7; 32])
}

pub struct NativeDevice {
    pub device: Device,
    pub peer: GroupPeer,
    /// This device's link to the workspace.
    pub link: DeviceLink,
    workspace: WorkspaceId,
    delivery: u64,
    /// Other devices' writes, as they arrived.
    writes: Vec<SignedOp>,
    _state: tempfile::TempDir,
}

impl NativeDevice {
    /// `device` in `workspace` on `server`, its group's keys not asked for yet.
    pub async fn start(
        server: &Server,
        user: [u8; 32],
        device: Device,
        workspace: WorkspaceId,
    ) -> Self {
        let state = tempfile::tempdir().unwrap();
        let peer = GroupPeer::waiting(
            Some(std::sync::Arc::new(key_group::Files::new(state.path()))),
            collab_types::WorkspaceId::parse(&workspace.to_string()).unwrap(),
            Box::new(device.key.clone()),
            &vault(),
        )
        .unwrap();
        let link = connect(server, user, &device, workspace).await;
        Self {
            device,
            peer,
            link,
            workspace,
            delivery: 0,
            writes: Vec::new(),
            _state: state,
        }
    }

    pub fn key(&self) -> [u8; 32] {
        self.device.key.verifying_key().to_bytes()
    }

    /// Let this device's link go, as a closed session does.
    pub async fn close(self) {
        self.link.close().await;
    }

    fn deliver(&mut self, message: impl FnOnce(String, u64) -> NeighbourMsg) {
        self.delivery = self.delivery.saturating_add(1);
        let message = message(self.workspace.to_string(), self.delivery);
        self.link
            .publish(&message)
            .expect("the link carries this device's frame");
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

    /// Take the next frame of the workspace: follow its governance, and
    /// keep the records other devices write.
    pub async fn next(&mut self, what: &str) {
        let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
        let heard = tokio::time::timeout_at(deadline, self.link.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .unwrap_or_else(|| panic!("the link ended while waiting for {what}"));
        match heard.message {
            NeighbourMsg::MlsGovernance { frame, .. } => {
                match self.peer.follow(*frame, &vault()).unwrap() {
                    PeerEvent::Desynchronised
                    | PeerEvent::AdmittedWithAnotherKey
                    | PeerEvent::Removed => panic!("the device left the workspace's group"),
                    _ => {}
                }
            }
            // The workspace's host writes as well: the tests read what
            // members wrote.
            NeighbourMsg::AppDocument { envelope, .. }
                if heard.author != self.key() && !is_host(&heard.author) =>
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

    /// `frame` of `route`, sealed under the group's current epoch: the payload a write of it
    /// carries, a checkpoint's naming no earlier write it holds.
    pub fn seal(
        &self,
        route: &tilo_peer_state::document_frame::DocumentRoute,
        frame: &[u8],
    ) -> (EncryptionMetadata, Vec<u8>) {
        let retention = collab_types::ContentRetention::of_frame(frame);
        let (metadata, sealed) = self
            .peer
            .content_keys()
            .unwrap()
            .seal_content(
                document_artifact_id(&route.app_id, &route.cluster_id, &route.document),
                retention,
                frame,
            )
            .unwrap();
        if retention == collab_types::ContentRetention::Checkpoint {
            let checkpoint = collab_types::SealedCheckpoint::new([], sealed);
            return (metadata, checkpoint.encode().unwrap());
        }
        (metadata, sealed)
    }

    /// Publish `op`, signed by this device, as its write of `route`.
    pub fn write(&self, route: &tilo_peer_state::document_frame::DocumentRoute, mut op: SignedOp) {
        collab_types::sign_op(&mut op, &self.device.key).unwrap();
        self.link
            .publish(&NeighbourMsg::AppDocument {
                peer: neighbour_wire::peer_handle(&self.key()),
                app_id: route.app_id.clone(),
                cluster_id: route.cluster_id.clone(),
                document_id: route.document.clone(),
                envelope: Box::new(op),
                readers: None,
            })
            .unwrap();
    }

    /// `author`'s next write of `route`, opened once this device holds the
    /// key it was sealed under: a write may arrive before the commit that
    /// carries its epoch's key. The author never writes a sealed record in
    /// the clear.
    pub async fn written(
        &mut self,
        author: [u8; 32],
        route: &tilo_peer_state::document_frame::DocumentRoute,
    ) -> DocumentFrame {
        let artifact = document_artifact_id(&route.app_id, &route.cluster_id, &route.document);
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
        let ciphertext = collab_types::sealed_ciphertext(op).expect("a sealed write's ciphertext");
        match keys.open_content(op.artifact, metadata, &ciphertext) {
            Ok(clear) => Some(clear.to_vec()),
            Err(key_group::Error::MissingContentKey(_)) => None,
            Err(error) => panic!("the write does not open: {error}"),
        }
    }
}
