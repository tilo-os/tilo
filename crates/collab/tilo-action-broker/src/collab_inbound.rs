//! Signed document ops another device's write carried to this host: each is
//! validated against local channel state, recorded, and fanned to the local
//! apps on its route.
use crate::collab_broker::FanoutState;
use crate::collab_core::{AuditEntry, BrokerChannels, apply_redaction_from_envelope};
use crate::route::CrdtRoute;
use collab_types::{
    ArtifactKind, OpClass, RedactedMarker, SignedOp, ValidationError, validate_admitted_inbound_op,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use ui::collaboration::CollabEvent;

/// A document op as the transport delivered it: the route its publishing
/// host stamped, and the author's role in the room.
pub struct RemoteDocument {
    pub app_id: String,
    pub cluster_id: String,
    pub record_id: String,
    pub role: collab_types::Role,
    pub envelope: SignedOp,
}

/// Validate `document` and, when admitted, record it and fan it to the local
/// apps on its route as said by `from_user_id`, the participant the host
/// names for its author. A refused op is audited, never fanned.
///
/// Returns `false` only when the channel state is poisoned.
pub fn deliver(
    channels: &Arc<Mutex<BrokerChannels>>,
    fanout: &FanoutState,
    document: RemoteDocument,
    from_user_id: impl FnOnce(&CrdtRoute) -> String,
) -> bool {
    let RemoteDocument {
        app_id,
        cluster_id,
        record_id,
        role,
        envelope,
    } = document;
    let artifact = envelope.artifact;
    let route = CrdtRoute::record(&app_id, &cluster_id, &record_id);
    let payload = validate_route(&route, &envelope);
    let validation = match &payload {
        Ok(payload) => {
            let Ok(mut ch) = channels.lock() else {
                return false;
            };
            let channel = match payload {
                collab_types::WholeDocumentPayload::Cleartext(_) => {
                    collab_types::document_channel_id(&app_id, &cluster_id, &record_id)
                }
                collab_types::WholeDocumentPayload::Encrypted { channel, .. } => *channel,
            };
            if ch.document_encryption_allowed(channel, envelope.encryption.as_ref()) {
                let state = ch.state_for(artifact);
                state.artifacts.insert(artifact, ArtifactKind::Note);
                state.artifact_apps.insert(artifact, app_id);
                validate_admitted_inbound_op(
                    &envelope,
                    role,
                    state,
                    std::time::SystemTime::now(),
                    Duration::from_secs(300),
                )
            } else {
                Err(ValidationError::DocumentRouteMismatch)
            }
        }
        Err(error) => Err(error.clone()),
    };
    match (payload, validation) {
        (Ok(_), Ok(())) => {
            accept(channels, fanout, envelope, &route, from_user_id);
            true
        }
        (_, Err(ValidationError::OpAlreadyRedacted { target })) => {
            record_already_redacted(channels, envelope, target);
            true
        }
        (_, Err(reason)) => {
            tracing::warn!("collab_broker (net): dropping invalid signed op: {reason}",);
            if let Ok(mut ch) = channels.lock() {
                ch.audit.record(
                    artifact,
                    AuditEntry {
                        envelope: Box::new(envelope),
                        rejection: Some(reason),
                        at: std::time::SystemTime::now(),
                    },
                );
            }
            true
        }
        (Err(_), Ok(())) => false,
    }
}

/// The payload `envelope` carries, when it was signed for exactly `route`.
///
/// # Errors
/// The envelope was signed for another route, or its payload is untyped.
pub fn validate_route(
    route: &CrdtRoute,
    envelope: &SignedOp,
) -> Result<collab_types::WholeDocumentPayload, ValidationError> {
    collab_types::validate_whole_document_payload_route(
        &route.app_id,
        &route.cluster_id,
        &route.record_id,
        envelope,
    )
    .map_err(|_| ValidationError::DocumentRouteMismatch)
}

/// Record an op that passed validation and fan it to local apps.
fn accept(
    channels: &Arc<Mutex<BrokerChannels>>,
    fanout: &FanoutState,
    envelope: SignedOp,
    route: &CrdtRoute,
    from_user_id: impl FnOnce(&CrdtRoute) -> String,
) {
    let artifact = envelope.artifact;
    if let Ok(mut ch) = channels.lock() {
        let mls_vault = ch.mls_vault.clone();
        ch.record_history(&envelope);
        let state = ch.state_for(artifact);
        state.record_validated_op(envelope.clone());
        if envelope.op_class == OpClass::Compaction
            && let Ok(cp) = collab_types::parse_compaction_payload(&envelope)
        {
            let _ = state.apply_compaction(&cp);
        }
        if envelope.op_class == OpClass::Compaction
            && let Some(metadata) = envelope.encryption
            && let Some(vault) = mls_vault
            && let Err(error) = vault.mint_snapshot_receipt(metadata.channel, &envelope)
        {
            tracing::warn!(%error, "encrypted snapshot was accepted but durable receipt minting failed");
        }
    }
    if envelope.op_class == OpClass::Redact {
        apply_redaction_from_envelope(&envelope, artifact, channels);
    }
    let event = CollabEvent::Document {
        envelope: Box::new(envelope),
        from_user_id: from_user_id(route),
        decrypted_payload: None,
    };
    // An unaddressed route is dropped rather than shown to every app.
    fanout.fanout_to_route(&event, u64::MAX, route);
}

/// An op that arrived after its redaction: never fanned, but a marker keeps
/// the op sequence gapless.
fn record_already_redacted(
    channels: &Arc<Mutex<BrokerChannels>>,
    envelope: SignedOp,
    target: collab_types::OpId,
) {
    let artifact = envelope.artifact;
    tracing::warn!(
        artifact = ?artifact,
        target = ?target,
        "collab_broker (net): late-arriving op already redacted",
    );
    if let Ok(mut ch) = channels.lock() {
        let state = ch.state_for(artifact);
        let mut marker_envelope = envelope.clone();
        if let Ok(marker_bytes) = RedactedMarker::v1().canonical() {
            marker_envelope.payload = marker_bytes;
        }
        state.record_validated_op(marker_envelope);
        ch.audit.record(
            artifact,
            AuditEntry {
                envelope: Box::new(envelope),
                rejection: Some(ValidationError::OpAlreadyRedacted { target }),
                at: std::time::SystemTime::now(),
            },
        );
    }
}
