//! Signed document ops another device's write carried to this host: each is
//! validated against local channel state, recorded, and fanned to the local
//! apps on its route.
use crate::collab_broker::FanoutState;
use crate::collab_core::{AuditEntry, BrokerChannels, apply_redaction_from_envelope};
use crate::route::CrdtRoute;
use collab_types::{
    ArtifactKind, ChannelState, OpKind, RedactedMarker, SignedOp, ValidationError,
    validate_admitted_inbound_op, validate_replayed_inbound_op,
};
use runtime_collab::event::CollabEvent;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tilo_action_policy::Standing;

/// A document op as the transport delivered it: the route its publishing
/// host stamped, and the author's standing in the room.
pub struct RemoteDocument {
    pub app_id: String,
    pub cluster_id: String,
    pub record_id: String,
    /// The author, the room's policy and any bound on them (a device not
    /// theirs), as the host established them for the authenticated peer.
    pub author: Standing,
    /// A workspace host replayed it from its retained, host-ordered history
    /// (before answering the subscription): old by nature.
    pub replayed: bool,
    pub envelope: SignedOp,
}

/// How far an admitted device's live op may be dated from this clock.
const MAX_CLOCK_SKEW: Duration = Duration::from_secs(300);

/// Validate one peer document op. Live traffic must be fresh; history a
/// workspace host replayed is admitted however old it is, and stays
/// protected against replay by the per-author sequence.
///
/// # Errors
/// As `validate_admitted_inbound_op`, or `validate_replayed_inbound_op` for
/// replayed history.
pub fn validate_document(
    envelope: &SignedOp,
    author: &Standing,
    replayed: bool,
    state: &mut ChannelState,
    now: std::time::SystemTime,
) -> Result<(), ValidationError> {
    let author = author.authority();
    if replayed {
        validate_replayed_inbound_op(envelope, &author, state, now, MAX_CLOCK_SKEW)
    } else {
        validate_admitted_inbound_op(envelope, &author, state, now, MAX_CLOCK_SKEW)
    }
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
        author,
        replayed,
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
                collab_types::RoutedDocumentPayload::Cleartext(_)
                | collab_types::RoutedDocumentPayload::CleartextSegment(_) => {
                    collab_types::document_channel_id(&app_id, &cluster_id, &record_id)
                }
                collab_types::RoutedDocumentPayload::Encrypted { channel, .. } => *channel,
            };
            let allowed = if replayed {
                ch.document_history_allowed(channel, envelope.encryption.as_ref())
            } else {
                ch.document_encryption_allowed(channel, envelope.encryption.as_ref())
            };
            if allowed {
                let state = ch.state_for(artifact);
                state.room.clone_from(&cluster_id);
                state.artifacts.insert(artifact, ArtifactKind::Note);
                state.artifact_apps.insert(artifact, app_id);
                state.artifact_records.insert(artifact, record_id);
                validate_document(
                    &envelope,
                    &author,
                    replayed,
                    state,
                    std::time::SystemTime::now(),
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
        (Ok(_), Err(ValidationError::ReplaySeq { .. }))
            if replayed && validated_before(channels, &envelope) =>
        {
            // A host replay of an op this broker already admitted, for a
            // local app that joined after it arrived: deliver it again, byte
            // for byte; the app's CRDT drops what it holds.
            fan_out(fanout, envelope, &route, from_user_id);
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

/// The payload `envelope` carries, when it was signed for exactly `route`:
/// a whole mutation, one segment of a larger one (endpoints reassemble it),
/// or a sealed one.
pub fn validate_route(
    route: &CrdtRoute,
    envelope: &SignedOp,
) -> Result<collab_types::RoutedDocumentPayload, ValidationError> {
    collab_types::validate_document_payload_route(
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
        ch.record_history(route, &envelope);
        let state = ch.state_for(artifact);
        state.record_validated_op(envelope.clone());
        if envelope.kind == OpKind::Compaction
            && let Ok(cp) = collab_types::parse_compaction_payload(&envelope)
        {
            let _ = state.apply_compaction(&cp);
        }
    }
    if envelope.kind == OpKind::Redaction {
        apply_redaction_from_envelope(&envelope, artifact, channels);
    }
    fan_out(fanout, envelope, route, from_user_id);
}

/// Whether this exact op, signature included, passed validation here.
fn validated_before(channels: &Arc<Mutex<BrokerChannels>>, envelope: &SignedOp) -> bool {
    channels.lock().is_ok_and(|mut ch| {
        ch.state_for(envelope.artifact)
            .op_log
            .iter()
            .any(|op| op == envelope)
    })
}

/// Show a validated peer document to the local apps on its route.
fn fan_out(
    fanout: &FanoutState,
    envelope: SignedOp,
    route: &CrdtRoute,
    from_user_id: impl FnOnce(&CrdtRoute) -> String,
) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{document_artifact_id, document_attestation_id};

    /// An editor of the room the ops are written in.
    fn editor() -> Standing {
        let mut policy = tilo_action_policy::RoomPolicy::new("room", "owner");
        policy
            .members
            .insert("editor".into(), tilo_action_policy::Role::Editor);
        Standing::person(policy, "editor")
    }

    #[test]
    fn a_host_replay_of_old_history_is_admitted_where_a_live_op_that_old_is_not() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let an_hour_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
        let (mutation, _) = collab_types::field_edit_mutation("note", "title", "old").unwrap();
        let mut op = SignedOp::new_unsigned(
            mutation.encode().unwrap(),
            document_artifact_id("notes", "room", "record"),
            collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
            document_attestation_id("notes", "room", "record"),
            OpKind::Change,
            an_hour_ago,
            1,
        );
        collab_types::sign_op(&mut op, &key).unwrap();
        let now = std::time::SystemTime::now();
        let channel = || {
            let mut state = ChannelState {
                room: "room".into(),
                ..ChannelState::default()
            };
            state.artifacts.insert(op.artifact, ArtifactKind::Note);
            state
        };
        assert_eq!(
            validate_document(&op, &editor(), false, &mut channel(), now),
            Err(ValidationError::OutsideValidityWindow),
        );
        let mut state = channel();
        assert_eq!(
            validate_document(&op, &editor(), true, &mut state, now),
            Ok(())
        );
        assert!(matches!(
            validate_document(&op, &editor(), true, &mut state, now),
            Err(ValidationError::ReplaySeq { .. })
        ));
    }

    /// An op the broker admitted before a local app held its record reaches
    /// that app when the host replays it; a different op under the same
    /// sequence is still refused.
    #[test]
    fn a_host_replay_redelivers_an_op_the_broker_already_admitted() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let op = |title: &str| {
            let (mutation, _) = collab_types::field_edit_mutation("note", "title", title).unwrap();
            let mut op = SignedOp::new_unsigned(
                mutation.encode().unwrap(),
                document_artifact_id("notes", "room", "record"),
                collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
                document_attestation_id("notes", "room", "record"),
                OpKind::Change,
                std::time::SystemTime::now(),
                1,
            );
            collab_types::sign_op(&mut op, &key).unwrap();
            op
        };
        let document = |envelope: SignedOp, replayed: bool| RemoteDocument {
            app_id: "notes".into(),
            cluster_id: "room".into(),
            record_id: "record".into(),
            author: editor(),
            replayed,
            envelope,
        };
        let channels = Arc::new(Mutex::new(BrokerChannels::default()));
        let fanout = FanoutState::new();
        let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&delivered);
        let _app = fanout.subscribe_rooms(
            vec!["room".into()],
            Box::new(move |event, _| {
                if matches!(event, CollabEvent::Document { .. }) {
                    seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }),
        );
        let count = || delivered.load(std::sync::atomic::Ordering::SeqCst);
        let first = op("first");
        assert!(deliver(
            &channels,
            &fanout,
            document(first.clone(), false),
            |_| "peer".into()
        ));
        assert_eq!(count(), 1);
        assert!(deliver(
            &channels,
            &fanout,
            document(first.clone(), false),
            |_| "peer".into()
        ));
        assert_eq!(count(), 1, "a live repeat is not delivered again");
        assert!(deliver(&channels, &fanout, document(first, true), |_| {
            "peer".into()
        }));
        assert_eq!(count(), 2, "the host's replay reaches the late app");
        assert!(deliver(
            &channels,
            &fanout,
            document(op("second"), true),
            |_| "peer".into()
        ));
        drop(fanout);
        assert_eq!(count(), 2, "another op under a used sequence is refused");
    }
}
