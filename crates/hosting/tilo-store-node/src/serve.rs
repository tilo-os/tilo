//! The store's own iroh protocol: a workspace's members dial it, prove their
//! device certificate, and sync the frames the store holds.
//!
//! The store answers on [`ALPN`], and only there. A dialling device's
//! endpoint id is its device key, so the QUIC handshake already proves which
//! device is on the other end; the certificate it sends first must be that
//! device's, unexpired and unrevoked. Whether the device may sync a given
//! workspace is the embedder's answer ([`Access`]): the store itself knows
//! nothing about membership, only how to keep what it was given.

use crate::Frame;
use crate::store::{Budget, Cursor, Store};
use crate::wire::{PROTOCOL_VERSION, StoreReply, StoreRequest};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use neighbour_wire::NeighbourMsg;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tilo_clock::unix_now_ns;
use tilo_user_identity::{RevocationEntry, RevocationList, verify_device_cert_at};

/// The ALPN a store device answers on.
pub const ALPN: &[u8] = b"tilo-store-node/1";

/// The largest message either side sends: one replay round with room to
/// spare.
const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

/// Whether a device may sync a workspace with this store.
///
/// The embedder answers from the workspace's membership: a relay's
/// governed access, a site's admission record, or a person's own devices.
pub trait Access: Send + Sync {
    /// Whether `device` may read and contribute `workspace`'s frames here.
    fn admits(&self, workspace: u64, device: [u8; 32]) -> bool;
}

/// A fixed set of admitted devices, for a store whose workspaces and members
/// are known up front.
#[derive(Debug, Default)]
pub struct FixedAccess {
    devices: BTreeMap<u64, BTreeSet<[u8; 32]>>,
}

impl FixedAccess {
    /// A store that admits no one yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Admit `device` to `workspace`.
    pub fn admit(&mut self, workspace: u64, device: [u8; 32]) {
        self.devices.entry(workspace).or_default().insert(device);
    }
}

impl Access for FixedAccess {
    fn admits(&self, workspace: u64, device: [u8; 32]) -> bool {
        self.devices
            .get(&workspace)
            .is_some_and(|devices| devices.contains(&device))
    }
}

/// A store device's protocol handler: the retained store and who may sync
/// it.
pub struct StoreNode {
    store: Arc<Store>,
    access: Arc<dyn Access>,
    budget: Budget,
}

impl std::fmt::Debug for StoreNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreNode")
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl StoreNode {
    /// Serve `store`, asking `access` about every device and workspace.
    #[must_use]
    pub fn new(store: Arc<Store>, access: Arc<dyn Access>) -> Self {
        Self {
            store,
            access,
            budget: Budget::default(),
        }
    }

    /// Serve `budget` worth of frames per replay round.
    #[must_use]
    pub const fn with_budget(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self
    }

    /// The store this handler serves.
    #[must_use]
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Answer one request of the session `device` (once it proved itself)
    /// on `remote`, the connection's endpoint id.
    fn answer(
        &self,
        device: &mut Option<[u8; 32]>,
        remote: [u8; 32],
        request: StoreRequest,
    ) -> StoreReply {
        match request {
            StoreRequest::Hello {
                protocol,
                cert,
                revocations,
            } => {
                if protocol != PROTOCOL_VERSION {
                    return refuse(format!(
                        "this store speaks version {PROTOCOL_VERSION}, not {protocol}"
                    ));
                }
                if cert.device_public_key != remote {
                    return refuse("the certificate is not the connection's device");
                }
                if let Err(error) = verify_device_cert_at(&cert, unix_now_ns()) {
                    return refuse(format!("the device certificate does not verify: {error}"));
                }
                if revoked(&revocations, &remote) {
                    return refuse("the device is revoked");
                }
                *device = Some(remote);
                StoreReply::Welcome {
                    protocol: PROTOCOL_VERSION,
                }
            }
            StoreRequest::Want { workspace, cursors } => {
                let Some(device) = *device else {
                    return refuse("the session did not introduce its device");
                };
                if !self.access.admits(workspace, device) {
                    return refuse("this store holds no such workspace for that device");
                }
                self.want(workspace, &cursors)
            }
            StoreRequest::Push { workspace, frames } => {
                let Some(device) = *device else {
                    return refuse("the session did not introduce its device");
                };
                if !self.access.admits(workspace, device) {
                    return refuse("this store holds no such workspace for that device");
                }
                self.push(workspace, &frames)
            }
        }
    }

    fn want(&self, workspace: u64, cursors: &[Cursor]) -> StoreReply {
        match self.store.replay(workspace, cursors, self.budget) {
            Ok(replay) => {
                let mut frames = Vec::with_capacity(replay.frames.len());
                for message in &replay.frames {
                    match postcard::to_stdvec(message) {
                        Ok(bytes) => frames.push(bytes),
                        Err(error) => {
                            return refuse(format!("a stored frame does not encode: {error}"));
                        }
                    }
                }
                StoreReply::Frames {
                    frames,
                    floors: replay.floors,
                    more: replay.more,
                }
            }
            Err(error) => refuse(format!("the store cannot replay that workspace: {error}")),
        }
    }

    fn push(&self, workspace: u64, frames: &[Vec<u8>]) -> StoreReply {
        let mut accepted = 0_u32;
        let mut refused = Vec::new();
        for bytes in frames {
            let message: NeighbourMsg = match postcard::from_bytes(bytes) {
                Ok(message) => message,
                Err(error) => {
                    refused.push(format!("a frame does not decode: {error}"));
                    continue;
                }
            };
            let frame = match Frame::of(&message) {
                Ok(frame) => frame,
                Err(error) => {
                    refused.push(error.to_string());
                    continue;
                }
            };
            if !self.access.admits(workspace, frame.lineage.author) {
                refused.push("a frame's author is no member of that workspace".into());
                continue;
            }
            match self.store.retain(workspace, &frame) {
                Ok(_) => accepted = accepted.saturating_add(1),
                Err(error) => refused.push(format!("a frame could not be kept: {error}")),
            }
        }
        StoreReply::Acked { accepted, refused }
    }

    /// Serve one connection: its one bidirectional stream carries the whole
    /// session, request by request, until either side closes it.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection breaks or a party sends
    /// something that is not a request of this protocol.
    pub async fn serve(&self, connection: Connection) -> Result<(), String> {
        let remote = *connection.remote_id().as_bytes();
        let (mut send, mut recv) = connection
            .accept_bi()
            .await
            .map_err(|error| error.to_string())?;
        let mut device = None;
        while let Some(request) = read(&mut recv).await? {
            let reply = self.answer(&mut device, remote, request);
            write(&mut send, &reply).await?;
        }
        let _ = send.finish();
        Ok(())
    }
}

impl ProtocolHandler for StoreNode {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.serve(connection)
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))
    }
}

fn refuse(reason: impl Into<String>) -> StoreReply {
    StoreReply::Refused {
        reason: reason.into(),
    }
}

/// Whether `entries` revoke `device`, taking only entries that verify.
fn revoked(entries: &[RevocationEntry], device: &[u8; 32]) -> bool {
    let mut list = RevocationList::new();
    for entry in entries {
        if let Err(error) = list.insert(entry.clone()) {
            tracing::warn!(%error, "a revocation a device presented does not verify; it is ignored");
        }
    }
    list.is_revoked(device)
}

async fn write(send: &mut SendStream, reply: &StoreReply) -> Result<(), String> {
    let bytes = postcard::to_allocvec(reply).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("reply too large".into());
    }
    let len = u32::try_from(bytes.len()).map_err(|_| "reply too large".to_string())?;
    send.write_all(&len.to_le_bytes())
        .await
        .map_err(|error| error.to_string())?;
    send.write_all(&bytes)
        .await
        .map_err(|error| error.to_string())
}

async fn read(recv: &mut RecvStream) -> Result<Option<StoreRequest>, String> {
    read_message(recv).await
}

async fn read_message<T: serde::de::DeserializeOwned>(
    recv: &mut RecvStream,
) -> Result<Option<T>, String> {
    let mut len = [0_u8; 4];
    match recv.read_exact(&mut len).await {
        Ok(()) => {}
        Err(iroh::endpoint::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    }
    let len =
        usize::try_from(u32::from_le_bytes(len)).map_err(|_| "request too large".to_string())?;
    if len > MAX_FRAME_BYTES {
        return Err("request too large".into());
    }
    let mut bytes = vec![0_u8; len];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    postcard::from_bytes(&bytes)
        .map(Some)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;
    use collab_types::{
        ArtifactId, AttestationId, ChannelId, ContentRetention, Ed25519PublicKey,
        EncryptionMetadata, OpKind, SignedOp, sign_op,
    };
    use ed25519_dalek::SigningKey;
    use neighbour_wire::peer_handle;
    use std::time::Duration;
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, RevocationReason, sign_device_cert, sign_revocation_entry,
    };
    use web_time::UNIX_EPOCH;

    const WORKSPACE: u64 = 7;
    const APP: &str = "test.app";

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

    fn frame(key: &SigningKey, seq: u64) -> NeighbourMsg {
        let mut op = SignedOp::new_unsigned(
            vec![seq as u8; 32],
            ArtifactId::new(),
            Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
            AttestationId::new(),
            OpKind::Change,
            UNIX_EPOCH + Duration::from_secs(seq),
            seq,
        );
        op.encryption = Some(EncryptionMetadata::content_key(
            1,
            ChannelId::new(),
            ContentRetention::Delta,
        ));
        sign_op(&mut op, key).unwrap();
        NeighbourMsg::AppDocument {
            peer: peer_handle(&key.verifying_key().to_bytes()),
            app_id: APP.into(),
            cluster_id: "workspace".into(),
            record_id: "tab:1".into(),
            envelope: Box::new(op),
        }
    }

    fn node(admitted: &[(&SigningKey, u64)]) -> (tempfile::TempDir, StoreNode) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path(), crate::Appetite::StoreAll).unwrap());
        let mut access = FixedAccess::new();
        for (key, workspace) in admitted {
            access.admit(*workspace, key.verifying_key().to_bytes());
        }
        (dir, StoreNode::new(store, Arc::new(access)))
    }

    fn hello(cert: DeviceCert) -> StoreRequest {
        StoreRequest::Hello {
            protocol: PROTOCOL_VERSION,
            cert: Box::new(cert),
            revocations: Vec::new(),
        }
    }

    fn introduced(node: &StoreNode, key: &SigningKey, cert: DeviceCert) -> Option<[u8; 32]> {
        let mut device = None;
        let reply = node.answer(&mut device, key.verifying_key().to_bytes(), hello(cert));
        assert!(matches!(reply, StoreReply::Welcome { .. }), "{reply:?}");
        device
    }

    fn refused(reply: &StoreReply) -> bool {
        matches!(reply, StoreReply::Refused { .. })
    }

    #[test]
    fn a_device_must_speak_the_stores_version() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let (key, cert) = device(&root, 2);
        let (_dir, node) = node(&[(&key, WORKSPACE)]);
        let mut device = None;
        let reply = node.answer(
            &mut device,
            key.verifying_key().to_bytes(),
            StoreRequest::Hello {
                protocol: PROTOCOL_VERSION + 1,
                cert: Box::new(cert),
                revocations: Vec::new(),
            },
        );
        assert!(refused(&reply));
        assert_eq!(device, None);
    }

    #[test]
    fn a_certificate_for_another_device_is_refused() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let (key, _) = device(&root, 2);
        let (_, other) = device(&root, 3);
        let (_dir, node) = node(&[(&key, WORKSPACE)]);
        let mut device = None;
        let reply = node.answer(&mut device, key.verifying_key().to_bytes(), hello(other));
        assert!(refused(&reply), "the certificate names another device");
    }

    #[test]
    fn a_revoked_device_is_refused() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let (key, cert) = device(&root, 2);
        let (_dir, node) = node(&[(&key, WORKSPACE)]);
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: key.verifying_key().to_bytes(),
            revoked_at_ns: 1,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &root).unwrap();
        let mut device = None;
        let reply = node.answer(
            &mut device,
            key.verifying_key().to_bytes(),
            StoreRequest::Hello {
                protocol: PROTOCOL_VERSION,
                cert: Box::new(cert),
                revocations: vec![entry],
            },
        );
        assert!(refused(&reply), "the root revoked this device");
        assert_eq!(device, None);
    }

    #[test]
    fn a_session_must_introduce_ifself_before_asking() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let (key, _) = device(&root, 2);
        let (_dir, node) = node(&[(&key, WORKSPACE)]);
        let mut device = None;
        let want = node.answer(
            &mut device,
            key.verifying_key().to_bytes(),
            StoreRequest::Want {
                workspace: WORKSPACE,
                cursors: Vec::new(),
            },
        );
        assert!(refused(&want));
        let push = node.answer(
            &mut device,
            key.verifying_key().to_bytes(),
            StoreRequest::Push {
                workspace: WORKSPACE,
                frames: vec![postcard::to_allocvec(&frame(&key, 1)).unwrap()],
            },
        );
        assert!(refused(&push));
        assert_eq!(node.store().bytes(), 0);
    }

    #[test]
    fn an_unadmitted_device_or_workspace_is_refused() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let (key, cert) = device(&root, 2);
        let (outsider_key, outsider_cert) = device(&root, 3);
        let (_dir, node) = node(&[(&key, WORKSPACE)]);

        let mut device = None;
        let reply = node.answer(&mut device, key.verifying_key().to_bytes(), hello(cert));
        assert!(matches!(reply, StoreReply::Welcome { .. }));

        // A device no workspace admits is not admitted here.
        let mut outsider_device = introduced(&node, &outsider_key, outsider_cert);
        let reply = node.answer(
            &mut outsider_device,
            outsider_key.verifying_key().to_bytes(),
            StoreRequest::Want {
                workspace: WORKSPACE,
                cursors: Vec::new(),
            },
        );
        assert!(refused(&reply));
    }

    #[test]
    fn only_frames_by_admitted_authors_are_kept() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let (key, cert) = device(&root, 2);
        let (outsider_key, _) = device(&root, 3);
        let (_dir, node) = node(&[(&key, WORKSPACE)]);
        let mut device = introduced(&node, &key, cert);

        let reply = node.answer(
            &mut device,
            key.verifying_key().to_bytes(),
            StoreRequest::Push {
                workspace: WORKSPACE,
                frames: vec![
                    postcard::to_allocvec(&frame(&key, 1)).unwrap(),
                    postcard::to_allocvec(&NeighbourMsg::Heartbeat { peer: 0, ts: 0 }).unwrap(),
                    postcard::to_allocvec(&frame(&outsider_key, 1)).unwrap(),
                ],
            },
        );
        let StoreReply::Acked { accepted, refused } = reply else {
            panic!("{reply:?}");
        };
        assert_eq!(accepted, 1, "only the admitted author's frame is kept");
        assert_eq!(refused.len(), 2, "{refused:?}");
        assert!(node.store().bytes() > 0);

        let reply = node.answer(
            &mut device,
            key.verifying_key().to_bytes(),
            StoreRequest::Want {
                workspace: WORKSPACE,
                cursors: Vec::new(),
            },
        );
        let StoreReply::Frames {
            frames,
            floors,
            more,
        } = reply
        else {
            panic!("{reply:?}");
        };
        assert_eq!(frames.len(), 1);
        assert!(floors.is_empty());
        assert!(!more);
    }
}
