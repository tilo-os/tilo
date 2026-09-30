//! Durable documents on the wire: signed `NeighbourMsg::AppDocument`
//! envelopes, byte-for-byte what a native shell publishes through its
//! workspace host, optionally sealed with the route's channel content key.
//!
//! A browser is both the app and the shell for its documents, so it signs
//! envelopes with its certified device key and verifies inbound ones the way
//! a shell would before an app sees them: the envelope signature must verify,
//! its author must be the device the host authenticated, and the route and
//! artifact must match.

use collab_types::{
    ContentRetention, DocumentFrame, DocumentMutation, EncryptionMetadata, SignedOp,
    document_artifact_id, document_attestation_id,
};
use neighbour_wire::{NeighbourMsg, Retention};
use tilo_relay_client::DeviceSigner;

/// One document route: application, room (cluster) and record.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct DocumentRoute {
    /// Application id.
    pub app_id: String,
    /// Room uuid, as text.
    pub cluster_id: String,
    /// Record wire name; empty when the app announced none.
    pub record: String,
}

impl DocumentRoute {
    /// Relay workspace that carries this route's room.
    ///
    /// # Errors
    ///
    /// Returns an error when `cluster_id` is not a uuid.
    pub fn workspace_id(&self) -> Result<u64, DocumentError> {
        tilo_relay_protocol::room_workspace_id(&self.cluster_id)
            .map_err(|_| DocumentError::Route("room is not a uuid".into()))
    }

    fn artifact(&self) -> collab_types::ArtifactId {
        document_artifact_id(&self.app_id, &self.cluster_id, &self.record)
    }
}

/// End-to-end content encryption of one route (the browser as a
/// `channel-crypto` member device).
pub trait ContentCipher: Send {
    /// Seal one cleartext document frame.
    ///
    /// # Errors
    ///
    /// Returns a description when this member cannot seal (no key).
    fn seal(
        &mut self,
        artifact: collab_types::ArtifactId,
        retention: ContentRetention,
        cleartext: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String>;

    /// Open one sealed payload.
    ///
    /// # Errors
    ///
    /// Returns a description when the payload does not open.
    fn open(
        &mut self,
        artifact: collab_types::ArtifactId,
        metadata: &EncryptionMetadata,
        payload: &[u8],
    ) -> Result<Vec<u8>, String>;
}

/// The installed content ciphers, by route. A route without one is
/// cleartext.
pub type Ciphers = std::collections::BTreeMap<DocumentRoute, Box<dyn ContentCipher>>;

/// Why a document envelope was not produced or accepted.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DocumentError {
    /// The route or artifact does not match.
    #[error("document route: {0}")]
    Route(String),
    /// Signing or signature verification failed.
    #[error("document signature: {0}")]
    Signature(String),
    /// The payload is not a document frame, or does not open.
    #[error("document payload: {0}")]
    Payload(String),
}

/// A document frame ready for the host: the encoded message and the
/// retention channel it is kept under.
#[derive(Clone, Debug)]
pub struct SignedDocumentFrame {
    /// Encoded `NeighbourMsg::AppDocument`.
    pub body: Vec<u8>,
    /// Retention channel digest.
    pub channel: [u8; 32],
    /// Whether the host keeps it as the channel's new checkpoint.
    pub checkpoint: bool,
}

/// A verified inbound document frame.
#[derive(Clone, Debug)]
pub struct OpenedDocument {
    /// Its route.
    pub route: DocumentRoute,
    /// The signed envelope as received.
    pub envelope: SignedOp,
    /// The authenticated authoring device.
    pub author: [u8; 32],
    /// Cleartext when the envelope was sealed.
    pub decrypted_payload: Option<Vec<u8>>,
}

impl OpenedDocument {
    /// The typed mutation carried by a whole (unsegmented) frame.
    pub fn mutation(&self) -> Option<DocumentMutation> {
        match DocumentFrame::decode(
            self.decrypted_payload
                .as_deref()
                .unwrap_or(&self.envelope.payload),
        )
        .ok()?
        {
            DocumentFrame::Whole(mutation) => Some(mutation),
            DocumentFrame::Segment(_) => None,
        }
    }
}

fn frame_retention(cleartext: &[u8]) -> ContentRetention {
    match DocumentFrame::decode(cleartext) {
        Ok(DocumentFrame::Whole(DocumentMutation::Snapshot(_))) => ContentRetention::Checkpoint,
        Ok(DocumentFrame::Segment(segment))
            if segment.kind == collab_types::DocumentMutationKind::Snapshot
                && segment.index == 0 =>
        {
            ContentRetention::Checkpoint
        }
        _ => ContentRetention::Delta,
    }
}

/// Stable `NeighbourMsg` peer handle of a device.
pub fn peer_id(device: &[u8; 32]) -> u64 {
    neighbour_wire::peer_handle(device)
}

/// Sign one cleartext document frame (one element of
/// `DocumentMutation::encode_frames`) as the device behind `signer`.
///
/// # Errors
///
/// Returns [`DocumentError`] when sealing, signing or encoding fails.
pub async fn sign_frame(
    route: &DocumentRoute,
    cleartext: Vec<u8>,
    signer: &dyn DeviceSigner,
    cipher: Option<&mut Box<dyn ContentCipher>>,
    seq: u64,
    now_ns: u64,
) -> Result<SignedDocumentFrame, DocumentError> {
    DocumentFrame::decode(&cleartext).map_err(|error| DocumentError::Payload(error.to_string()))?;
    let artifact = route.artifact();
    let author = collab_types::Ed25519PublicKey::from_bytes(signer.device_public_key());
    let attestation = document_attestation_id(&route.app_id, &route.cluster_id, &route.record);
    let timestamp = crate::clock::system_time(now_ns);
    let mut op = match cipher {
        Some(cipher) => {
            let (metadata, sealed) = cipher
                .seal(artifact, frame_retention(&cleartext), &cleartext)
                .map_err(DocumentError::Payload)?;
            SignedOp::new_unsigned_with_encryption(
                sealed,
                artifact,
                author,
                attestation,
                collab_types::OpClass::Splice,
                timestamp,
                seq,
                metadata,
            )
        }
        None => SignedOp::new_unsigned(
            cleartext,
            artifact,
            author,
            attestation,
            collab_types::OpClass::Splice,
            timestamp,
            seq,
        ),
    };
    let signing_bytes = op
        .signing_bytes()
        .map_err(|error| DocumentError::Signature(error.to_string()))?;
    let signature = signer
        .sign(signing_bytes)
        .await
        .map_err(|error| DocumentError::Signature(error.0))?;
    op.signature = collab_types::Signature::from_bytes(signature);
    collab_types::verify_op(&op).map_err(|error| DocumentError::Signature(error.to_string()))?;
    let message = NeighbourMsg::AppDocument {
        peer: peer_id(&signer.device_public_key()),
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id.clone(),
        record_id: route.record.clone(),
        envelope: Box::new(op),
    };
    let (channel, retention) = neighbour_wire::retention(&message, false)
        .ok_or_else(|| DocumentError::Payload("document frame is not retainable".into()))?;
    Ok(SignedDocumentFrame {
        body: postcard::to_stdvec(&message)
            .map_err(|error| DocumentError::Payload(error.to_string()))?,
        channel,
        checkpoint: retention == Retention::Checkpoint,
    })
}

/// Verify one inbound host delta. `author` is the device the host stamped
/// from its authenticated connection. Returns `Ok(None)` for traffic that is
/// not an application document (shell tree, governance, continuity).
///
/// # Errors
///
/// Returns [`DocumentError`] for a document that must not reach an app.
pub fn open_frame(
    body: &[u8],
    author: [u8; 32],
    ciphers: &mut Ciphers,
) -> Result<Option<OpenedDocument>, DocumentError> {
    let Ok(NeighbourMsg::AppDocument {
        app_id,
        cluster_id,
        record_id,
        envelope,
        ..
    }) = postcard::from_bytes::<NeighbourMsg>(body)
    else {
        return Ok(None);
    };
    let route = DocumentRoute {
        app_id,
        cluster_id,
        record: record_id,
    };
    collab_types::verify_op(&envelope)
        .map_err(|error| DocumentError::Signature(error.to_string()))?;
    if *envelope.author.as_bytes() != author {
        return Err(DocumentError::Signature(
            "envelope author is not the authenticated device".into(),
        ));
    }
    if envelope.artifact != route.artifact() {
        return Err(DocumentError::Route("artifact does not match route".into()));
    }
    let decrypted_payload = match envelope.encryption {
        Some(metadata) => {
            let cipher = ciphers
                .get_mut(&route)
                .ok_or_else(|| DocumentError::Payload("route has no content key".into()))?;
            Some(
                cipher
                    .open(route.artifact(), &metadata, &envelope.payload)
                    .map_err(DocumentError::Payload)?,
            )
        }
        None => None,
    };
    DocumentFrame::decode(decrypted_payload.as_deref().unwrap_or(&envelope.payload))
        .map_err(|error| DocumentError::Payload(error.to_string()))?;
    Ok(Some(OpenedDocument {
        route,
        envelope: *envelope,
        author,
        decrypted_payload,
    }))
}
