//! Signing document frames as the page's certified device: a browser is
//! both the app and the shell for its documents, so it signs envelopes with
//! its device key. Opening and reassembling admitted frames is the engine's
//! ([`tilo_peer_state::document_frame`]).

use collab_types::{ContentRetention, DocumentFrame, SignedOp, document_attestation_id};
use neighbour_wire::{NeighbourMsg, Retention};
use tilo_peer_state::document_frame::{ContentCipher, DocumentError, DocumentRoute};
use tilo_relay_client::DeviceSigner;

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

/// Stable `NeighbourMsg` peer handle of a device.
pub fn peer_id(device: &[u8; 32]) -> u64 {
    neighbour_wire::peer_handle(device)
}

/// Sign one cleartext document frame (one element of
/// `DocumentMutation::encode_frames`) as the device behind `signer`.
pub async fn sign_frame(
    route: &DocumentRoute,
    cleartext: Vec<u8>,
    signer: &dyn DeviceSigner,
    cipher: &mut Box<dyn ContentCipher>,
    seq: u64,
    now_ns: u64,
) -> Result<SignedDocumentFrame, DocumentError> {
    DocumentFrame::decode(&cleartext).map_err(|error| DocumentError::Payload(error.to_string()))?;
    let artifact = route.artifact();
    let author = collab_types::Ed25519PublicKey::from_bytes(signer.device_public_key());
    let attestation = document_attestation_id(&route.app_id, &route.cluster_id, &route.record);
    let timestamp = crate::clock::system_time(now_ns);
    // A document is only ever written sealed for its route's channel.
    let (metadata, sealed) = cipher
        .seal(artifact, ContentRetention::of_frame(&cleartext), &cleartext)
        .map_err(DocumentError::Payload)?;
    // A sealed op carries the claims of its changes for the relay, and names
    // the surface of the page's app they are decided against. Each claim names
    // the units its action may write by that surface, as a device's own
    // broker does: left on the whole record, a change to a part the app keeps
    // apart (a PDF's comments) is refused, since no action of it may write
    // the whole record.
    let claims = collab_types::DocumentMutation::decode(&cleartext)
        .ok()
        .and_then(|mutation| {
            collab_types::mutation_claims(&mutation, collab_types::record_unit).ok()
        })
        .unwrap_or_default();
    #[cfg(feature = "toolkit")]
    let (claims, surface) = (
        ui::app_surface::with_footprints(&route.app_id, claims),
        ui::app_surface::hash_of(&route.app_id),
    );
    #[cfg(not(feature = "toolkit"))]
    let surface = None;
    let mut op = SignedOp::new_unsigned_with_encryption(
        sealed,
        artifact,
        author,
        attestation,
        collab_types::OpKind::Change,
        timestamp,
        seq,
        metadata,
    )
    .with_claims(claims)
    .with_surface(surface);
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
