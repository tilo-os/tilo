//! Signed document ops another device's write carried to this host: each is
//! validated against the local inbound state, recorded, and fanned to the local
//! apps on its route.
use crate::collab_broker::FanoutState;
use crate::collab_core::{AuditEntry, BrokerChannels, apply_redaction_from_envelope};
use crate::route::CrdtRoute;
use collab_types::{
    ArtifactKind, InboundState, OpKind, RedactedMarker, SignedOp, ValidationError,
    validate_admitted_inbound_op, validate_replayed_inbound_op,
};
use runtime_collab::event::CollabEvent;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
use tilo_action_policy::Standing;

/// A document op as the transport delivered it: the route its publishing
/// host stamped, and the author's standing in the workspace.
pub struct RemoteDocument {
    pub app_id: String,
    pub cluster_id: String,
    pub record_id: String,
    /// The author, the workspace's policy and any bound on them (a device not
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
    state: &mut InboundState,
    now: SystemTime,
) -> Result<(), ValidationError> {
    let author = author.authority();
    if replayed {
        validate_replayed_inbound_op(envelope, &author, state, now, MAX_CLOCK_SKEW)
    } else {
        validate_admitted_inbound_op(envelope, &author, state, now, MAX_CLOCK_SKEW)
    }
}

/// What [`deliver`] did with a peer's document op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Admitted and fanned out, or refused and audited.
    Done,
    /// Nothing was decided: the inbound state is poisoned.
    Halted,
}

/// Validate `document` and, when admitted, record it and fan it to the local
/// apps on its route as said by `from_user_id`, the participant the host
/// names for its author. A refused op is audited, never fanned.
pub fn deliver(
    channels: &Arc<Mutex<BrokerChannels>>,
    fanout: &FanoutState,
    document: RemoteDocument,
    from_user_id: impl FnOnce(&CrdtRoute) -> String,
) -> Delivery {
    deliver_at(channels, fanout, document, from_user_id, tilo_clock::wall())
}

fn deliver_at(
    channels: &Arc<Mutex<BrokerChannels>>,
    fanout: &FanoutState,
    document: RemoteDocument,
    from_user_id: impl FnOnce(&CrdtRoute) -> String,
    now: SystemTime,
) -> Delivery {
    let artifact = document.envelope.artifact;
    let replayed = document.replayed;
    let route = CrdtRoute::record(&document.app_id, &document.cluster_id, &document.record_id);
    let payload = validate_route(&route, &document.envelope);
    let validation = match &payload {
        Ok(payload) => {
            let Ok(mut ch) = channels.lock() else {
                return Delivery::Halted;
            };
            let channel = match payload {
                collab_types::RoutedDocumentPayload::Cleartext(_)
                | collab_types::RoutedDocumentPayload::CleartextSegment(_) => {
                    collab_types::document_channel_id(
                        &document.app_id,
                        &document.cluster_id,
                        &document.record_id,
                    )
                }
                collab_types::RoutedDocumentPayload::Encrypted { channel, .. } => *channel,
            };
            let encryption = document.envelope.encryption.as_ref();
            if ch.document_order_allowed(&document.cluster_id, channel, artifact, encryption) {
                let state = ch.state_for(artifact);
                state.workspace.clone_from(&document.cluster_id);
                state.artifacts.insert(artifact, ArtifactKind::Note);
                state
                    .artifact_apps
                    .insert(artifact, document.app_id.clone());
                state
                    .artifact_records
                    .insert(artifact, document.record_id.clone());
                validate_document(
                    &document.envelope,
                    &document.author,
                    document.replayed,
                    state,
                    now,
                )
            } else {
                tracing::warn!(
                    app = %document.app_id,
                    record = %document.record_id,
                    channel = ?channel,
                    epoch = encryption.map(|metadata| metadata.epoch),
                    replayed,
                    "collab_broker (net): a document op its channel's gate does not take"
                );
                Err(ValidationError::DocumentRouteMismatch)
            }
        }
        Err(error) => Err(error.clone()),
    };
    match (payload, validation) {
        (Ok(_), Ok(())) => {
            accept(channels, fanout, document.envelope, &route, from_user_id);
            Delivery::Done
        }
        (_, Err(ValidationError::OpAlreadyRedacted { target })) => {
            record_already_redacted(channels, document.envelope, target);
            Delivery::Done
        }
        (Ok(_), Err(ValidationError::ReplaySeq { .. }))
            if replayed && validated_before(channels, &document.envelope) =>
        {
            // A host replay of an op this broker already admitted, for a
            // local app that joined after it arrived: deliver it again, byte
            // for byte; the app's CRDT drops what it holds.
            fan_out(fanout, document.envelope, &route, from_user_id);
            Delivery::Done
        }
        (_, Err(reason)) => refuse(channels, document, reason, now),
        (Err(_), Ok(())) => Delivery::Halted,
    }
}

/// Audit a refused peer op.
fn refuse(
    channels: &Arc<Mutex<BrokerChannels>>,
    document: RemoteDocument,
    reason: ValidationError,
    now: SystemTime,
) -> Delivery {
    tracing::warn!("collab_broker (net): dropping invalid signed op: {reason}",);
    if let Ok(mut ch) = channels.lock() {
        ch.audit.record(
            document.envelope.artifact,
            AuditEntry {
                envelope: Box::new(document.envelope),
                rejection: Some(reason),
                at: now,
            },
        );
    }
    Delivery::Done
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
    let mut first_of_the_record = false;
    let unsynced = channels.lock().ok().map(|mut ch| {
        first_of_the_record = !ch.routes.contains_key(&artifact);
        let unsynced = ch.record_history(route, &envelope);
        if envelope.encryption.is_some() {
            ch.sealed.insert(artifact);
        }
        let state = ch.state_for(artifact);
        state.record_validated_op(envelope.clone());
        if envelope.kind == OpKind::Compaction
            && let Ok(cp) = collab_types::parse_compaction_payload(&envelope)
        {
            let _ = state.apply_compaction(&cp);
        }
        unsynced
    });
    // Synced outside the channels' lock, which the main loop takes too, and
    // before the op goes on to the apps.
    if let Some(unsynced) = unsynced {
        unsynced.finish();
    }
    if envelope.kind == OpKind::Redaction {
        apply_redaction_from_envelope(&envelope, artifact, channels);
    }
    crate::services::tracker_written(route);
    fan_out(fanout, envelope, route, from_user_id);
    if first_of_the_record {
        announce_record(fanout, route);
    }
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

/// Tell a window open on the app that a peer wrote a record of one of its
/// trees (a chain's next part, a container's child): it holds no part of that
/// tree yet, so nothing else brings the record to it. A window opened later
/// learns the same records as it subscribes
/// ([`crate::collab_core::BrokerChannels::tree_records_held`]).
fn announce_record(fanout: &FanoutState, route: &CrdtRoute) {
    if ui::record::RecordId::parse(&route.record_id).is_none_or(|record| record.tree().is_none()) {
        return;
    }
    fanout.fanout_to_route(
        &CollabEvent::RecordsArrived {
            records: vec![route.record_id.clone()],
        },
        u64::MAX,
        &CrdtRoute::app(route.app_id.clone(), route.cluster_id.clone()),
    );
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
                at: tilo_clock::wall(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{document_artifact_id, document_attestation_id};

    /// An editor of the workspace the ops are written in.
    fn editor() -> Standing {
        let mut policy = tilo_action_policy::WorkspacePolicy::new("workspace", "owner");
        policy
            .members
            .insert("editor".into(), tilo_action_policy::Role::Editor);
        Standing::person(policy, "editor")
    }

    #[test]
    fn a_host_replay_of_old_history_is_admitted_where_a_live_op_that_old_is_not() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let an_hour_ago = tilo_clock::wall() - Duration::from_secs(3600);
        let (mutation, _) = collab_types::field_edit_mutation("note", "title", "old").unwrap();
        let mut op = SignedOp::new_unsigned(
            mutation.encode().unwrap(),
            document_artifact_id("notes", "workspace", "record"),
            collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
            document_attestation_id("notes", "workspace", "record"),
            OpKind::Change,
            an_hour_ago,
            1,
        );
        collab_types::sign_op(&mut op, &key).unwrap();
        let now = tilo_clock::wall();
        let channel = || {
            let mut state = InboundState {
                workspace: "workspace".into(),
                ..InboundState::default()
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
                document_artifact_id("notes", "workspace", "record"),
                collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
                document_attestation_id("notes", "workspace", "record"),
                OpKind::Change,
                tilo_clock::wall(),
                1,
            );
            collab_types::sign_op(&mut op, &key).unwrap();
            op
        };
        let document = |envelope: SignedOp, replayed: bool| RemoteDocument {
            app_id: "notes".into(),
            cluster_id: "workspace".into(),
            record_id: "record".into(),
            author: editor(),
            replayed,
            envelope,
        };
        let channels = Arc::new(Mutex::new(BrokerChannels::default()));
        let fanout = FanoutState::new();
        let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&delivered);
        let _app = fanout.subscribe_workspaces(
            vec!["workspace".into()],
            Box::new(move |event, _| {
                if matches!(event, CollabEvent::Document { .. }) {
                    seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }),
        );
        let count = || delivered.load(std::sync::atomic::Ordering::SeqCst);
        let first = op("first");
        assert_eq!(
            deliver(&channels, &fanout, document(first.clone(), false), |_| {
                "peer".into()
            }),
            Delivery::Done
        );
        assert_eq!(count(), 1);
        assert_eq!(
            deliver(&channels, &fanout, document(first.clone(), false), |_| {
                "peer".into()
            }),
            Delivery::Done
        );
        assert_eq!(count(), 1, "a live repeat is not delivered again");
        assert_eq!(
            deliver(&channels, &fanout, document(first, true), |_| {
                "peer".into()
            }),
            Delivery::Done
        );
        assert_eq!(count(), 2, "the host's replay reaches the late app");
        assert_eq!(
            deliver(&channels, &fanout, document(op("second"), true), |_| "peer"
                .into()),
            Delivery::Done
        );
        drop(fanout);
        assert_eq!(count(), 2, "another op under a used sequence is refused");
    }

    /// A writer's two edits to a sealed note: the first sealed under the
    /// channel's keys from before the member joined (epoch 1), the second
    /// under the current ones (epoch 2), each its own Automerge change.
    fn two_sealed_writes(key: &ed25519_dalek::SigningKey) -> [SignedOp; 2] {
        use automerge::transaction::Transactable as _;
        let channel = collab_types::document_channel_id("notes", "workspace", "record");
        let mut note = automerge::AutoCommit::new();
        let mut write = |epoch: u64, field: &str, text: &str| {
            let heads = note.get_heads();
            note.put(automerge::ROOT, field, text).unwrap();
            let change = note.save_after(&heads);
            let now = tilo_clock::wall();
            let mut op = SignedOp::new_unsigned_with_encryption(
                change,
                document_artifact_id("notes", "workspace", "record"),
                collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
                document_attestation_id("notes", "workspace", "record"),
                OpKind::Change,
                now,
                collab_types::next_unordered_document_sequence(now),
                collab_types::EncryptionMetadata::content_key(
                    epoch,
                    channel,
                    collab_types::ContentRetention::Delta,
                ),
            );
            collab_types::sign_op(&mut op, key).unwrap();
            op
        };
        let first = write(1, "title", "Groceries");
        let second = write(2, "body", "oat milk");
        [first, second]
    }

    struct MemberTook {
        deliveries: Vec<Delivery>,
        note: automerge::AutoCommit,
        shown: Vec<SignedOp>,
    }

    /// A member of a sealed note, whose channel has moved on since its
    /// member joined, is shown the writer's `writes` in that order. Answers
    /// what each delivery said, and the note the member's app holds, from
    /// the writes it was shown in the order it was shown them.
    fn member_takes(writes: &[SignedOp]) -> MemberTook {
        let channels = Arc::new(Mutex::new(BrokerChannels::default()));
        channels.lock().unwrap().register_channel_gate(
            collab_types::document_channel_id("notes", "workspace", "record"),
            channel_crypto::ChannelGate {
                encryption_required: true,
            },
        );
        let fanout = FanoutState::new();
        let shown = Arc::new(Mutex::new(Vec::new()));
        let app = Arc::clone(&shown);
        let _app = fanout.subscribe_workspaces(
            vec!["workspace".into()],
            Box::new(move |event, _| {
                if let CollabEvent::Document { envelope, .. } = event {
                    app.lock().unwrap().push((**envelope).clone());
                }
            }),
        );
        let deliveries = writes
            .iter()
            .map(|envelope| {
                deliver(
                    &channels,
                    &fanout,
                    RemoteDocument {
                        app_id: "notes".into(),
                        cluster_id: "workspace".into(),
                        record_id: "record".into(),
                        author: editor(),
                        replayed: false,
                        envelope: envelope.clone(),
                    },
                    |_| "writer".into(),
                )
            })
            .collect();
        drop(fanout);
        let shown = shown.lock().unwrap().clone();
        let mut note = automerge::AutoCommit::new();
        for envelope in &shown {
            tilo_automerge::ApplyIncoming::load_incoming(&mut note, &envelope.payload).unwrap();
        }
        MemberTook {
            deliveries,
            note,
            shown,
        }
    }

    fn text(note: &automerge::AutoCommit, field: &str) -> Option<String> {
        use automerge::ReadDoc as _;
        note.get(automerge::ROOT, field)
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok())
    }

    /// A record's writes in the clear before its seal are its base, live or
    /// replayed, though the channel's gate has closed: a device away while
    /// the record sealed reads it only by taking them in. Once a sealed
    /// write of the record entered, one in the clear is refused however it
    /// came.
    #[test]
    fn a_write_in_the_clear_enters_until_its_record_sealed() {
        let [sealed, _] = two_sealed_writes(&ed25519_dalek::SigningKey::from_bytes(&[6; 32]));
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let clear = |seq: u64, title: &str| {
            let (mutation, _) = collab_types::field_edit_mutation("note", "title", title).unwrap();
            let mut op = SignedOp::new_unsigned(
                mutation.encode().unwrap(),
                document_artifact_id("notes", "workspace", "record"),
                collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
                document_attestation_id("notes", "workspace", "record"),
                OpKind::Change,
                tilo_clock::wall(),
                seq,
            );
            collab_types::sign_op(&mut op, &key).unwrap();
            op
        };
        let channels = Arc::new(Mutex::new(BrokerChannels::default()));
        channels.lock().unwrap().register_channel_gate(
            collab_types::document_channel_id("notes", "workspace", "record"),
            channel_crypto::ChannelGate {
                encryption_required: true,
            },
        );
        let fanout = FanoutState::new();
        let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app = Arc::clone(&delivered);
        let _app = fanout.subscribe_workspaces(
            vec!["workspace".into()],
            Box::new(move |event, _| {
                if matches!(event, CollabEvent::Document { .. }) {
                    app.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }),
        );
        let shows = |envelope: SignedOp, replayed: bool| {
            let before = delivered.load(std::sync::atomic::Ordering::SeqCst);
            deliver(
                &channels,
                &fanout,
                RemoteDocument {
                    app_id: "notes".into(),
                    cluster_id: "workspace".into(),
                    record_id: "record".into(),
                    author: editor(),
                    replayed,
                    envelope,
                },
                |_| "writer".into(),
            );
            delivered.load(std::sync::atomic::Ordering::SeqCst) > before
        };
        assert!(shows(clear(1, "live base"), false), "a live base write");
        assert!(shows(clear(2, "kept base"), true), "a kept base write");
        assert!(shows(sealed, false), "the seal");
        assert!(!shows(clear(3, "after, live"), false), "a live downgrade");
        assert!(!shows(clear(4, "after, kept"), true), "a kept downgrade");
    }

    /// A write sealed under the channel's earlier epoch is taken live: its
    /// author was a member when they wrote it, and whoever receives it holds
    /// the key it was sealed under.
    #[test]
    fn a_write_sealed_under_an_earlier_epoch_is_admitted_live() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
        let [first, second] = two_sealed_writes(&key);
        let took = member_takes(&[first.clone(), second.clone()]);
        assert_eq!(took.deliveries, [Delivery::Done, Delivery::Done]);
        assert_eq!(took.shown, [first, second]);
        assert_eq!(text(&took.note, "title").as_deref(), Some("Groceries"));
        assert_eq!(text(&took.note, "body").as_deref(), Some("oat milk"));
    }

    /// A peer wrote the next part of a chat's messages: a window already open
    /// on Chat is told the part arrived, so it opens it and shows what was
    /// posted. A record of no tree (a conversation itself) is read directly
    /// once and is told no arrival, and a record is announced once, not for
    /// every message written into it.
    #[test]
    fn a_peer_written_part_is_announced_to_the_apps_open_window() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
        let part =
            ui::record::RecordId::derived("messages", &ui::record::RecordId::workspace_default())
                .part(0)
                .to_string();
        let conversation = ui::record::RecordId::for_tab(7).to_string();
        let write = |record: &str, sequence: u64, title: &str| {
            let (mutation, _) = collab_types::field_edit_mutation("note", "title", title).unwrap();
            let mut op = SignedOp::new_unsigned(
                mutation.encode().unwrap(),
                document_artifact_id("so.tilo.chat", "workspace", record),
                collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
                document_attestation_id("so.tilo.chat", "workspace", record),
                OpKind::Change,
                tilo_clock::wall(),
                sequence,
            );
            collab_types::sign_op(&mut op, &key).unwrap();
            op
        };
        let channels = Arc::new(Mutex::new(BrokerChannels::default()));
        let fanout = FanoutState::new();
        let told = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&told);
        let _app = fanout.subscribe_workspaces(
            vec!["workspace".into()],
            Box::new(move |event, origin| {
                if let CollabEvent::RecordsArrived { records } = event {
                    sink.lock().unwrap().push((origin.clone(), records.clone()));
                }
            }),
        );
        let take = |record: &str, envelope: SignedOp| {
            deliver(
                &channels,
                &fanout,
                RemoteDocument {
                    app_id: "so.tilo.chat".into(),
                    cluster_id: "workspace".into(),
                    record_id: record.into(),
                    author: editor(),
                    replayed: false,
                    envelope,
                },
                |_| "peer".into(),
            )
        };
        assert_eq!(take(&part, write(&part, 1, "hello")), Delivery::Done);
        assert_eq!(
            told.lock().unwrap().as_slice(),
            &[(
                CrdtRoute::app("so.tilo.chat", "workspace"),
                vec![part.clone()]
            )],
            "the window is told the part, on the app's route"
        );
        assert_eq!(
            take(&part, write(&part, 2, "hello again")),
            Delivery::Done,
            "a later message in a part the window opened is delivered to it"
        );
        assert_eq!(
            take(&conversation, write(&conversation, 1, "hi")),
            Delivery::Done
        );
        assert_eq!(
            take(&conversation, write(&conversation, 2, "hi again")),
            Delivery::Done,
            "a window tells its app a record of no tree is read"
        );
        drop(fanout);
        assert_eq!(
            told.lock().unwrap().len(),
            1,
            "only the part is an arrival, and only once"
        );
    }
}
