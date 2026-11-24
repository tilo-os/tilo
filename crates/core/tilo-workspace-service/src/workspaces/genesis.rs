//! Publishing a new resource's genesis: the standard factory's, or an
//! installed app's model's, as the workspace's governor.

use super::CollaborationError;
use super::sealed_documents;

/// Publish an app's `genesis` of a new document into `candidate` as the
/// workspace's governor: sealed under `keys` where the channel has them, so
/// nothing it holds is ever written in the clear. Answers the frames it wrote,
/// in order.
pub(super) fn publish_genesis(
    candidate: &mut tilo_peer_state::StatePeer,
    keys: Option<&sealed_documents::DocumentKeys>,
    (app, document): (&str, &str),
    genesis: Vec<u8>,
    (key, now): (&ed25519_dalek::SigningKey, std::time::SystemTime),
) -> Result<Vec<neighbour_wire::NeighbourMsg>, CollaborationError> {
    if let Some(keys) = keys {
        let changes = automerge::AutoCommit::load(&genesis)
            .map_err(|error| CollaborationError::new(error.to_string()))?
            .save_after(&[]);
        return Ok(sealed_documents::start_sealed(
            candidate,
            keys,
            (app, document),
            changes,
            key,
            now,
        )?);
    }
    // Establish the document's ownership first, then merge the standard
    // factory onto it: two writes of this device, in order.
    Ok(vec![
        candidate.publication_mut().publish_document(
            app,
            document,
            automerge::AutoCommit::new().save(),
            key,
            now,
        )?,
        candidate
            .publication_mut()
            .publish_document(app, document, genesis, key, now)?,
    ])
}
