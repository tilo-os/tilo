//! Signed document ops another device's write carried to this host: each is
//! validated against local channel state, recorded, and fanned to the local
//! apps on its route, in its author's order ([`Behind`]).
use crate::collab_broker::FanoutState;
use crate::collab_core::{AuditEntry, BrokerChannels, apply_redaction_from_envelope};
use crate::route::CrdtRoute;
use collab_types::{
    ArtifactKind, ChannelState, OpKind, RedactedMarker, SignedOp, ValidationError,
    validate_admitted_inbound_op, validate_replayed_inbound_op,
};
use runtime_collab::event::CollabEvent;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
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
    /// Admitted and fanned out, held behind an earlier write of its author,
    /// or refused and audited.
    Done,
    /// Nothing was decided: the channel state is poisoned.
    Halted,
    /// Refused live because it is sealed under content keys older than the
    /// channel's latest membership change: its writer had not caught up
    /// with that change yet. The room's host, which admits only the room's
    /// members, kept it; replayed as history it is taken, so the caller asks
    /// the host to replay.
    SealedBehind,
}

/// One author's writes to one record that wait on an earlier write of
/// theirs, which was refused live as [`Delivery::SealedBehind`] and which
/// the room's host replays.
///
/// An author's sequence only moves forward: admitting a later write first
/// would refuse the earlier one when its replay arrives. So a later write is
/// checked when it arrives and held, and admitted in its author's order once
/// every refused write before it is. Nothing here is let go before it is
/// admitted or refused: what is held is bounded by the writes an author made
/// to the record between a refused write and its replay.
#[derive(Default)]
pub struct Behind {
    /// Sequences of the author's writes refused live for their epoch.
    refused: BTreeSet<u64>,
    /// The author's later writes, by sequence.
    held: BTreeMap<u64, HeldWrite>,
}

struct HeldWrite {
    document: RemoteDocument,
    from_user_id: String,
    /// When it arrived: it is checked as fresh as it was then.
    arrived: SystemTime,
}

impl Behind {
    /// Whether the author's write under `seq` waits: a write of theirs
    /// before it is still to be admitted.
    fn waits(&self, seq: u64) -> bool {
        self.refused.first().is_some_and(|first| seq > *first)
    }

    /// The held writes that wait on nothing any more, in their author's
    /// order.
    fn take_ready(&mut self) -> Vec<HeldWrite> {
        let mut ready = Vec::new();
        while let Some(entry) = self.held.first_entry() {
            if self
                .refused
                .first()
                .is_some_and(|first| entry.key() > first)
            {
                break;
            }
            ready.push(entry.remove());
        }
        ready
    }

    fn is_empty(&self) -> bool {
        self.refused.is_empty() && self.held.is_empty()
    }
}

/// What validation decided for an op that passed.
enum Admission {
    Admit,
    /// Valid, and waiting on an earlier write of its author.
    Hold,
}

/// Validate `document` and, when admitted, record it and fan it to the local
/// apps on its route as said by `from_user_id`, the participant the host
/// names for its author. A refused op is audited, never fanned. A write that
/// follows one of its author's writes still to be replayed is held, and
/// admitted after it ([`Behind`]).
pub fn deliver(
    channels: &Arc<Mutex<BrokerChannels>>,
    fanout: &FanoutState,
    document: RemoteDocument,
    from_user_id: impl FnOnce(&CrdtRoute) -> String,
) -> Delivery {
    deliver_at(channels, fanout, document, from_user_id, SystemTime::now())
}

fn deliver_at(
    channels: &Arc<Mutex<BrokerChannels>>,
    fanout: &FanoutState,
    document: RemoteDocument,
    from_user_id: impl FnOnce(&CrdtRoute) -> String,
    now: SystemTime,
) -> Delivery {
    let artifact = document.envelope.artifact;
    let author = document.envelope.author;
    let seq = document.envelope.seq;
    let replayed = document.replayed;
    let route = CrdtRoute::record(&document.app_id, &document.cluster_id, &document.record_id);
    let payload = validate_route(&route, &document.envelope);
    let mut sealed_behind = false;
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
            let allowed = if replayed {
                ch.document_history_allowed(channel, encryption)
            } else {
                ch.document_encryption_allowed(channel, encryption)
            };
            // A write its author has written past here was admitted before
            // (a late copy of a replayed write): nothing waits for it.
            let ahead = ch
                .states
                .get(&artifact)
                .and_then(|state| state.last_seq_per_author.get(&author))
                .is_none_or(|seen| seq > *seen);
            sealed_behind = !allowed
                && !replayed
                && encryption.is_some()
                && ch.document_history_allowed(channel, encryption)
                && ahead;
            if allowed {
                check(&mut ch, &document, now)
            } else {
                Err(ValidationError::DocumentRouteMismatch)
            }
        }
        Err(error) => Err(error.clone()),
    };
    match (payload, validation) {
        (Ok(_), Ok(Admission::Admit)) => {
            accept(channels, fanout, document.envelope, &route, from_user_id);
            release(channels, fanout, artifact, author, seq)
        }
        (Ok(_), Ok(Admission::Hold)) => {
            let from_user_id = from_user_id(&route);
            hold(channels, fanout, document, from_user_id, now)
        }
        (_, Err(ValidationError::OpAlreadyRedacted { target })) => {
            record_already_redacted(channels, document.envelope, target);
            // Settled: what follows it waits no longer.
            release(channels, fanout, artifact, author, seq)
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
        (_, Err(reason)) => refuse(channels, fanout, document, reason, sealed_behind, now),
        (Err(_), Ok(_)) => Delivery::Halted,
    }
}

/// Audit a refused peer op. One refused live for its epoch is remembered,
/// for its author's later writes to wait on its replay.
fn refuse(
    channels: &Arc<Mutex<BrokerChannels>>,
    fanout: &FanoutState,
    document: RemoteDocument,
    reason: ValidationError,
    sealed_behind: bool,
    now: SystemTime,
) -> Delivery {
    let (artifact, author, seq, replayed) = (
        document.envelope.artifact,
        document.envelope.author,
        document.envelope.seq,
        document.replayed,
    );
    tracing::warn!("collab_broker (net): dropping invalid signed op: {reason}",);
    let mut decided = false;
    if let Ok(mut ch) = channels.lock() {
        // The host's replay of a write refused for its epoch was
        // decided on what it says: nothing waits on it any more.
        decided = replayed
            && ch
                .behind
                .get(&(artifact, author))
                .is_some_and(|behind| behind.refused.contains(&seq));
        if sealed_behind {
            // The host replays it; the author's later writes wait
            // for it.
            ch.behind
                .entry((artifact, author))
                .or_default()
                .refused
                .insert(seq);
        }
        ch.audit.record(
            artifact,
            AuditEntry {
                envelope: Box::new(document.envelope),
                rejection: Some(reason),
                at: now,
            },
        );
    }
    if decided {
        release(channels, fanout, artifact, author, seq)
    } else if sealed_behind {
        Delivery::SealedBehind
    } else {
        Delivery::Done
    }
}

/// Validate `document`, which the channel's gate let through, against its
/// channel state: admitted now, or held behind an earlier write of its
/// author.
fn check(
    ch: &mut BrokerChannels,
    document: &RemoteDocument,
    now: SystemTime,
) -> Result<Admission, ValidationError> {
    let (artifact, author, seq) = (
        document.envelope.artifact,
        document.envelope.author,
        document.envelope.seq,
    );
    let waits = ch
        .behind
        .get(&(artifact, author))
        .is_some_and(|behind| behind.waits(seq));
    let state = ch.state_for(artifact);
    state.room.clone_from(&document.cluster_id);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state
        .artifact_apps
        .insert(artifact, document.app_id.clone());
    state
        .artifact_records
        .insert(artifact, document.record_id.clone());
    let validate = |state: &mut ChannelState| {
        validate_document(
            &document.envelope,
            &document.author,
            document.replayed,
            state,
            now,
        )
    };
    if !waits {
        return validate(state).map(|()| Admission::Admit);
    }
    // Checked now; its author's sequence moves on only when it is
    // admitted, after what it waits on.
    let seen = state.last_seq_per_author.get(&author).copied();
    let checked = validate(state);
    match seen {
        Some(seen) => state.last_seq_per_author.insert(author, seen),
        None => state.last_seq_per_author.remove(&author),
    };
    checked.map(|()| Admission::Hold)
}

/// Hold `document`, valid but waiting on an earlier write of its author.
fn hold(
    channels: &Arc<Mutex<BrokerChannels>>,
    fanout: &FanoutState,
    document: RemoteDocument,
    from_user_id: String,
    arrived: SystemTime,
) -> Delivery {
    let key = (document.envelope.artifact, document.envelope.author);
    let seq = document.envelope.seq;
    {
        let Ok(mut ch) = channels.lock() else {
            return Delivery::Halted;
        };
        let channels = &mut *ch;
        if let Some(behind) = channels.behind.get_mut(&key)
            && behind.waits(seq)
        {
            match behind.held.entry(seq) {
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(HeldWrite {
                        document,
                        from_user_id,
                        arrived,
                    });
                }
                // The same write again (live, then in the host's replay).
                std::collections::btree_map::Entry::Occupied(held)
                    if held.get().document.envelope == document.envelope => {}
                std::collections::btree_map::Entry::Occupied(_) => {
                    // Another write under a sequence its author already used.
                    channels.audit.record(
                        key.0,
                        AuditEntry {
                            envelope: Box::new(document.envelope),
                            rejection: Some(ValidationError::ReplaySeq {
                                seen: seq,
                                incoming: seq,
                            }),
                            at: arrived,
                        },
                    );
                }
            }
            return Delivery::Done;
        }
    }
    // What it waited on was settled meanwhile.
    deliver_at(channels, fanout, document, move |_| from_user_id, arrived)
}

/// `author`'s write under `seq` to `artifact` is settled: deliver their
/// writes held behind it, in their order. Says [`Delivery::SealedBehind`]
/// when one of them is refused as sealed behind in turn.
fn release(
    channels: &Arc<Mutex<BrokerChannels>>,
    fanout: &FanoutState,
    artifact: collab_types::ArtifactId,
    author: collab_types::Ed25519PublicKey,
    seq: u64,
) -> Delivery {
    let ready = {
        let Ok(mut ch) = channels.lock() else {
            return Delivery::Halted;
        };
        let key = (artifact, author);
        let Some(behind) = ch.behind.get_mut(&key) else {
            return Delivery::Done;
        };
        // A refused write at or before an admitted one can never be
        // admitted after it: nothing waits on it.
        behind.refused.retain(|refused| *refused > seq);
        let ready = behind.take_ready();
        if behind.is_empty() {
            ch.behind.remove(&key);
        }
        ready
    };
    let mut delivery = Delivery::Done;
    for HeldWrite {
        document,
        from_user_id,
        arrived,
    } in ready
    {
        match deliver_at(channels, fanout, document, move |_| from_user_id, arrived) {
            Delivery::Done => {}
            Delivery::Halted => return Delivery::Halted,
            Delivery::SealedBehind => delivery = Delivery::SealedBehind,
        }
    }
    delivery
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
    let unsynced = channels.lock().ok().map(|mut ch| {
        let unsynced = ch.record_history(route, &envelope);
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
                at: SystemTime::now(),
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

    /// How a write reaches the member over the net: as it was written, or in
    /// the room's host's replay of its history.
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Carried {
        replayed: bool,
        envelope: SignedOp,
    }

    /// A writer's two edits to a sealed note: the first sealed under the
    /// channel's keys from before the member joined (epoch 1), the second
    /// under the current ones (epoch 2), each its own Automerge change.
    fn two_sealed_writes(key: &ed25519_dalek::SigningKey) -> [SignedOp; 2] {
        use automerge::transaction::Transactable as _;
        let channel = collab_types::document_channel_id("notes", "room", "record");
        let mut note = automerge::AutoCommit::new();
        let mut write = |epoch: u64, field: &str, text: &str| {
            let heads = note.get_heads();
            note.put(automerge::ROOT, field, text).unwrap();
            let change = note.save_after(&heads);
            let now = SystemTime::now();
            let mut op = SignedOp::new_unsigned_with_encryption(
                change,
                document_artifact_id("notes", "room", "record"),
                collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
                document_attestation_id("notes", "room", "record"),
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

    /// One way the net brings the member the writer's writes and the host's
    /// replay.
    #[derive(Clone, Copy, Debug)]
    enum Arrival {
        /// The first write as written: sealed before the member joined.
        Refused,
        /// The second write as written.
        Newer,
        /// The host's replay of the first write, which it kept.
        Replay,
    }

    /// A member of a sealed note whose channel's latest membership change
    /// is epoch 2 takes what the fake net delivers in `order`. Answers what
    /// each delivery said, and the note the member's app holds from the
    /// writes it was shown, in the order it was shown them, and the writes.
    fn member_takes(order: &[Arrival]) -> MemberTook {
        let key = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
        let [first, second] = two_sealed_writes(&key);
        let channels = Arc::new(Mutex::new(BrokerChannels::default()));
        channels.lock().unwrap().register_channel_gate(
            collab_types::document_channel_id("notes", "room", "record"),
            channel_crypto::ChannelGate {
                encryption_required: true,
                head_epoch: Some(2),
            },
        );
        let fanout = FanoutState::new();
        let shown = Arc::new(Mutex::new(Vec::new()));
        let app = Arc::clone(&shown);
        let _app = fanout.subscribe_rooms(
            vec!["room".into()],
            Box::new(move |event, _| {
                if let CollabEvent::Document { envelope, .. } = event {
                    app.lock().unwrap().push((**envelope).clone());
                }
            }),
        );

        let mut net = tilo_test_env::FakeNet::new();
        let writer = net.peer("writer");
        let host = net.peer("host");
        let _member = net.peer("member");
        let carried = |replayed: bool, envelope: &SignedOp| {
            postcard::to_stdvec(&Carried {
                replayed,
                envelope: envelope.clone(),
            })
            .unwrap()
        };
        let mut deliveries = Vec::new();
        for arrival in order {
            match arrival {
                Arrival::Refused => net.publish(&writer, carried(false, &first)),
                Arrival::Newer => net.publish(&writer, carried(false, &second)),
                Arrival::Replay => net.publish(&host, carried(true, &first)),
            }
            // Each arrival reaches the member before the next is sent.
            net.deliver_to("member", |_, bytes| {
                let Carried { replayed, envelope } = postcard::from_bytes(bytes).unwrap();
                deliveries.push(deliver(
                    &channels,
                    &fanout,
                    RemoteDocument {
                        app_id: "notes".into(),
                        cluster_id: "room".into(),
                        record_id: "record".into(),
                        author: editor(),
                        replayed,
                        envelope,
                    },
                    |_| "writer".into(),
                ));
            });
        }
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
            written: [first, second],
        }
    }

    struct MemberTook {
        deliveries: Vec<Delivery>,
        note: automerge::AutoCommit,
        shown: Vec<SignedOp>,
        written: [SignedOp; 2],
    }

    fn text(note: &automerge::AutoCommit, field: &str) -> Option<String> {
        use automerge::ReadDoc as _;
        note.get(automerge::ROOT, field)
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok())
    }

    /// The writer's newer write reaches the member before the host's replay
    /// of the earlier one it refused: the newer write is held, and both are
    /// applied in the order they were written once the replay arrives.
    #[test]
    fn a_newer_write_arriving_before_the_replay_of_a_refused_one_waits_and_both_apply() {
        let took = member_takes(&[Arrival::Refused, Arrival::Newer, Arrival::Replay]);
        assert_eq!(
            took.deliveries,
            [Delivery::SealedBehind, Delivery::Done, Delivery::Done],
            "the refused write asks for the replay; the newer one waits for it"
        );
        assert_eq!(
            took.shown, took.written,
            "both are applied, in the order they were written"
        );
        assert_eq!(text(&took.note, "title").as_deref(), Some("Groceries"));
        assert_eq!(text(&took.note, "body").as_deref(), Some("oat milk"));
    }

    /// The host's replay reaches the member before the writer's newer write:
    /// each is applied as it arrives.
    #[test]
    fn a_replay_arriving_before_the_newer_write_applies_both() {
        let took = member_takes(&[Arrival::Refused, Arrival::Replay, Arrival::Newer]);
        assert_eq!(
            took.deliveries,
            [Delivery::SealedBehind, Delivery::Done, Delivery::Done]
        );
        assert_eq!(took.shown, took.written);
        assert_eq!(text(&took.note, "title").as_deref(), Some("Groceries"));
        assert_eq!(text(&took.note, "body").as_deref(), Some("oat milk"));

        // The replay arrives even before the write as written: that late
        // copy is refused for its epoch, but nothing waits on it.
        let took = member_takes(&[Arrival::Replay, Arrival::Refused, Arrival::Newer]);
        assert_eq!(
            took.deliveries,
            [Delivery::Done, Delivery::Done, Delivery::Done]
        );
        assert_eq!(took.shown, took.written);
        assert_eq!(text(&took.note, "title").as_deref(), Some("Groceries"));
        assert_eq!(text(&took.note, "body").as_deref(), Some("oat milk"));
    }
}
