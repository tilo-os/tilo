//! A document envelope as a route's endpoint takes it in: the whole payload,
//! checked against the route it arrived on, and the mutation it carries.

use crate::RouteSpec;

/// The whole document payload `envelope` carries, when it belongs to
/// `route`.
#[must_use]
pub fn validated_route_payload(
    route: &RouteSpec,
    envelope: &collab_types::SignedOp,
) -> Option<collab_types::WholeDocumentPayload> {
    collab_types::validate_whole_document_payload_route(
        &route.app_id,
        &route.cluster_id.to_string(),
        route.record.as_deref().unwrap_or(""),
        envelope,
    )
    .ok()
}

/// [`validated_route_payload`] for an envelope as it reaches the endpoint:
/// a reassembled cleartext transfer arrives on its last segment's envelope
/// with the complete mutation beside it. The segment envelope must belong to
/// `route`, and the complete payload must be a current-format mutation (the
/// only format that is ever segmented).
#[must_use]
pub fn validated_route_payload_with(
    route: &RouteSpec,
    envelope: &collab_types::SignedOp,
    decrypted_payload: Option<&[u8]>,
) -> Option<collab_types::WholeDocumentPayload> {
    let (None, Some(complete)) = (envelope.encryption.as_ref(), decrypted_payload) else {
        return validated_route_payload(route, envelope);
    };
    let segment = collab_types::validate_document_payload_route(
        &route.app_id,
        &route.cluster_id.to_string(),
        route.record.as_deref().unwrap_or(""),
        envelope,
    )
    .ok()?;
    if !matches!(
        segment,
        collab_types::RoutedDocumentPayload::CleartextSegment(_)
    ) {
        return None;
    }
    // The segments belong to `route`: a transfer that reassembles into no
    // mutation is a fault of its sender, not a change for another route.
    match collab_types::DocumentMutation::decode(complete) {
        Ok(mutation) => Some(collab_types::WholeDocumentPayload::Cleartext(mutation)),
        Err(error) => {
            tracing::warn!(app = %route.app_id, %error, "collab: a reassembled document transfer does not decode");
            None
        }
    }
}

/// The mutation `payload` carries on `route`: the cleartext one, or the
/// endpoint's decryption of an encrypted one, which must name `route`'s
/// channel.
///
/// # Errors
/// The decryption is missing, does not decode, or names another channel.
pub fn decode_routed_mutation(
    route: &RouteSpec,
    payload: collab_types::WholeDocumentPayload,
    decrypted_payload: Option<&[u8]>,
) -> Result<collab_types::DocumentMutation, collab_types::DocumentMutationError> {
    match payload {
        collab_types::WholeDocumentPayload::Cleartext(mutation) => Ok(mutation),
        collab_types::WholeDocumentPayload::Encrypted { channel, .. } => {
            let decrypted_payload =
                decrypted_payload.ok_or(collab_types::DocumentMutationError::RouteMismatch)?;
            let mutation = collab_types::DocumentMutation::decode(decrypted_payload)?;
            let record = route.record.as_deref().unwrap_or("");
            if collab_types::document_channel_id(
                &route.app_id,
                &route.cluster_id.to_string(),
                record,
            ) != channel
            {
                return Err(collab_types::DocumentMutationError::RouteMismatch);
            }
            Ok(mutation)
        }
    }
}
