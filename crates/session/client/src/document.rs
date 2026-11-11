//! Signing document frames as the page's certified device: a browser is
//! both the app and the shell for its documents, so it signs envelopes with
//! its device key. Opening and reassembling admitted frames is the engine's
//! ([`tilo_peer_state::document_frame`]).

use collab_types::{ContentRetention, DocumentFrame, SignedOp, document_attestation_id};
use tilo_peer_state::document_frame::{ContentCipher, DocumentError, DocumentRoute};
use tilo_relay_client::DeviceSigner;

/// Stable `NeighbourMsg` peer handle of a device.
pub fn peer_id(device: &[u8; 32]) -> u64 {
    neighbour_wire::peer_handle(device)
}

/// One cleartext document frame of `route` as the write this device commits: sealed with
/// `cipher` and signed through `signer`, the custody of this device's key. The owner keeps it
/// in its outgoing custody, which the session sends to the workspace's host.
///
/// # Errors
/// The frame does not decode, or sealing or signing failed.
pub async fn signed_write(
    route: &DocumentRoute,
    cleartext: &[u8],
    signer: &dyn DeviceSigner,
    cipher: &mut dyn ContentCipher,
    now_ns: u64,
) -> Result<tilo_peer_state::owner_commit::OwnerCommitRequest, DocumentError> {
    let now = crate::clock::system_time(now_ns);
    let seq = collab_types::next_unordered_document_sequence(now);
    let op = signed_op(route, cleartext, signer, cipher, seq, now_ns).await?;
    Ok(tilo_peer_state::owner_commit::OwnerCommitRequest {
        workspace: route.cluster_id.clone(),
        operation: tilo_peer_state::owner_commit::CommitOperation::Document {
            app: route.app_id.clone(),
            record: route.record.clone(),
            envelope: Box::new(op),
        },
    })
}

/// One cleartext document frame as the op `signer`'s device writes: sealed with `cipher` for
/// its route's channel, with the claims of its changes, signed through custody.
async fn signed_op(
    route: &DocumentRoute,
    cleartext: &[u8],
    signer: &dyn DeviceSigner,
    cipher: &mut dyn ContentCipher,
    seq: u64,
    now_ns: u64,
) -> Result<SignedOp, DocumentError> {
    DocumentFrame::decode(cleartext).map_err(|error| DocumentError::Payload(error.to_string()))?;
    let artifact = route.artifact();
    let author = collab_types::Ed25519PublicKey::from_bytes(signer.device_public_key());
    let attestation = document_attestation_id(&route.app_id, &route.cluster_id, &route.record);
    let timestamp = crate::clock::system_time(now_ns);
    // A document is only ever written sealed for its route's channel.
    let (metadata, sealed) = cipher
        .seal(artifact, ContentRetention::of_frame(cleartext), cleartext)
        .map_err(DocumentError::Payload)?;
    // A sealed op carries the claims of its changes for the relay, and names
    // the surface of the page's app they are decided against. Each claim names
    // the units its action may write by that surface, as a device's own
    // broker does: left on the whole record, a change to a part the app keeps
    // apart (a PDF's comments) is refused, since no action of it may write
    // the whole record.
    let claims = collab_types::DocumentMutation::decode(cleartext)
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
    Ok(op)
}
