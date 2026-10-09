//! Bounded replaceable recovery material; immutable obligations remain elsewhere.
use super::{
    ArtifactKind, BTreeMap, ChannelState, DocumentMutation, Duration, Ed25519PublicKey,
    LocalDocument, MAX_PUBLICATION_BYTES, MAX_PUBLICATIONS, MemberAuthority, NeighbourMsg, OpClass,
    ReadDoc, Route, SignedOp, SigningKey, StatePeer, SystemTime, err, peer_handle,
    publication_size, room,
};

/// A historical byte/count estimate, never signing or document authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalRecoveryReservation {
    routes: Vec<(String, String, usize)>,
    max_encoded_bytes: usize,
    max_peer_growth_bytes: usize,
    max_peer_decimal_growth: usize,
}
impl LocalRecoveryReservation {
    pub fn entries(&self) -> usize {
        self.routes.len()
    }
    pub fn max_encoded_bytes(&self) -> usize {
        self.max_encoded_bytes
    }
    pub fn max_peer_growth_bytes(&self) -> usize {
        self.max_peer_growth_bytes
    }
    pub fn max_peer_decimal_growth(&self) -> usize {
        self.max_peer_decimal_growth
    }
    pub fn routes(&self) -> impl Iterator<Item = (&str, &str, usize)> {
        self.routes
            .iter()
            .map(|(app, record, bytes)| (app.as_str(), record.as_str(), *bytes))
    }
}

struct RecoveryHistory {
    doc: automerge::Automerge,
    original_sequence: u64,
}

/// The unsigned envelope re-sending a route's whole recovered history,
/// claiming each of its changes.
fn history_envelope(
    room: &str,
    route: &Route,
    history: &automerge::Automerge,
    author: Ed25519PublicKey,
    timestamp: SystemTime,
    sequence: u64,
) -> Result<SignedOp, String> {
    let mutation = DocumentMutation::Incremental(history.save());
    let claims =
        collab_types::mutation_claims(&mutation, collab_types::record_unit).map_err(err)?;
    Ok(SignedOp::new_unsigned(
        mutation.encode().map_err(err)?,
        collab_types::document_artifact_id(&route.app, room, &route.record),
        author,
        collab_types::document_attestation_id(&route.app, room, &route.record),
        OpClass::Splice,
        timestamp,
        sequence,
    )
    .with_claims(claims))
}

impl StatePeer {
    fn recovery_obligations(
        &self,
        originals: &[NeighbourMsg],
        legacy: Option<&StatePeer>,
        selected: [u8; 32],
    ) -> Result<BTreeMap<Route, RecoveryHistory>, String> {
        publication_size(originals)?;
        let mut routes = BTreeMap::new();
        for original in originals {
            let local = self.retained_document(original, selected)?;
            // A sealed write has no history anyone without its key could
            // project; its key holder renews it instead (`renew_local_sealed`).
            if local.mutation.is_none()
                || self.retained_local_document_retired(original, selected)?
            {
                continue;
            }
            add_history(
                &mut routes,
                local.route.clone(),
                self.publication_history(&local)?,
                local.admission.sequence(),
            )?;
        }
        if let Some(history) = legacy {
            self.validate_retained_history(history)?;
            for (route, old) in &history.core.documents {
                let current = self
                    .core
                    .documents
                    .get(route)
                    .ok_or("retained recovery history route missing")?;
                if old
                    .channel
                    .last_seq_per_author
                    .iter()
                    .any(|(author, sequence)| {
                        current
                            .channel
                            .last_seq_per_author
                            .get(author)
                            .copied()
                            .unwrap_or(0)
                            < *sequence
                    })
                {
                    return Err(
                        "retained recovery history would regress reserved sequence floor".into(),
                    );
                }
                if self.retained_history_route_retired(route, old)? {
                    continue;
                }
                add_history(
                    &mut routes,
                    route.clone(),
                    automerge::Automerge::load(&old.doc.clone().save()).map_err(err)?,
                    old.channel
                        .last_seq_per_author
                        .get(&Ed25519PublicKey::from_bytes(selected))
                        .copied()
                        .unwrap_or(0),
                )?;
            }
        }
        Ok(routes)
    }

    /// Reserve capacity for recovering `originals` as the selected device.
    ///
    /// # Errors
    ///
    /// Returns an error if the originals are not authentic retained custody or exceed the
    /// recovery budget.
    pub fn reserve_local_recovery(
        &self,
        originals: &[NeighbourMsg],
        legacy: Option<&StatePeer>,
        selected: [u8; 32],
    ) -> Result<LocalRecoveryReservation, String> {
        let obligations = self.recovery_obligations(originals, legacy, selected)?;
        self.recovery_reservation(&obligations, selected)
    }

    fn recovery_reservation(
        &self,
        obligations: &BTreeMap<Route, RecoveryHistory>,
        selected: [u8; 32],
    ) -> Result<LocalRecoveryReservation, String> {
        let mut routes = Vec::new();
        let mut total = 0usize;
        let mut peer_growth = 0usize;
        let mut peer_decimal_growth = 0usize;
        for (route, history) in obligations {
            // This unsigned shape is cost-only, never returned or admitted. Every
            // byte except time/sequence is the actual future wire representation.
            // u64::MAX reserves sequence width. Serde SystemTime encodes u64
            // seconds/u32 nanos: maximum postcard widths 10+5 rather than 1+1.
            let envelope = history_envelope(
                &self.core.room,
                route,
                &history.doc,
                Ed25519PublicKey::from_bytes(selected),
                SystemTime::UNIX_EPOCH,
                u64::MAX,
            )?;
            let shape = NeighbourMsg::AppDocument {
                peer: peer_handle(&selected),
                app_id: route.app.clone(),
                cluster_id: self.core.room.clone(),
                record_id: route.record.clone(),
                envelope: Box::new(envelope),
            };
            let bytes = publication_size(&[shape])?
                .checked_add(13)
                .ok_or("projection reservation overflow")?;
            total = total
                .checked_add(bytes)
                .filter(|total| *total <= MAX_PUBLICATION_BYTES)
                .ok_or("projection reservation byte limit")?;
            // DurableDocument serializes only Vec<([u8;32],u64)> watermarks
            // from ChannelState. Reserve the exact maximum delta of that field.
            let floors = &self
                .core
                .documents
                .get(route)
                .ok_or("projection route is not admitted")?
                .channel
                .last_seq_per_author;
            // Varint widths are at most 10 and their decimal spellings at most 28
            // bytes, so the saturating steps below are exact.
            let (growth, decimal_growth) =
                if let Some(sequence) = floors.get(&Ed25519PublicKey::from_bytes(selected)) {
                    (
                        10usize.saturating_sub(varint_width(*sequence)),
                        28usize.saturating_sub(varint_decimal_digits(*sequence)),
                    )
                } else {
                    let old_count = floors.len() as u64;
                    let new_count = old_count.saturating_add(1);
                    (
                        (32usize + 10)
                            .saturating_add(varint_width(new_count))
                            .saturating_sub(varint_width(old_count)),
                        selected
                            .iter()
                            .map(|byte| decimal_digits(*byte))
                            .fold(28usize, usize::saturating_add)
                            .saturating_add(varint_decimal_digits(new_count))
                            .saturating_sub(varint_decimal_digits(old_count)),
                    )
                };
            peer_growth = peer_growth
                .checked_add(growth)
                .ok_or("projection floor reservation overflow")?;
            peer_decimal_growth = peer_decimal_growth
                .checked_add(decimal_growth)
                .ok_or("projection floor decimal reservation overflow")?;
            routes.push((route.app.clone(), route.record.clone(), bytes));
        }
        Ok(LocalRecoveryReservation {
            routes,
            max_encoded_bytes: total,
            max_peer_growth_bytes: peer_growth,
            max_peer_decimal_growth: peer_decimal_growth,
        })
    }

    fn historical_projection(
        &self,
        message: &NeighbourMsg,
        selected: [u8; 32],
    ) -> Result<LocalDocument, String> {
        let local = self.local_document_parts(message, selected).map_err(err)?;
        let NeighbourMsg::AppDocument { envelope, .. } = message else {
            return Err("recovery projection is not an app document".into());
        };
        if envelope.op_class != OpClass::Splice
            || !matches!(local.mutation, Some(DocumentMutation::Incremental(_)))
            || local.admission.sequence() == 0
            || self
                .core
                .exact_admissions
                .get(&local.admission.key)
                .is_some()
        {
            return Err(
                "recovery projection must be a reserved transmission, not a local admission".into(),
            );
        }
        let floor = self
            .core
            .documents
            .get(&local.route)
            .ok_or("recovery route is not admitted")?
            .channel
            .last_seq_per_author
            .get(&Ed25519PublicKey::from_bytes(selected))
            .copied()
            .unwrap_or(0);
        if local.admission.sequence() > floor {
            return Err("recovery projection exceeds persisted sequence reservation".into());
        }
        Ok(local)
    }

    fn validate_projection_history(
        &self,
        obligations: &BTreeMap<Route, RecoveryHistory>,
        projection: &[NeighbourMsg],
        selected: [u8; 32],
    ) -> Result<(), String> {
        publication_size(projection)?;
        let mut routes = std::collections::BTreeSet::new();
        for message in projection {
            let local = self.historical_projection(message, selected)?;
            if !routes.insert(local.route.clone()) {
                return Err("recovery projection repeats a route".into());
            }
            let history = self.publication_history(&local)?;
            let obligation = obligations.get(&local.route);
            let acknowledged =
                self.durable_receipt_checkpoint(&local.route.app, &local.route.record)?;
            if !history.get_heads().iter().all(|head| {
                obligation.is_some_and(|entry| entry.doc.get_change_by_hash(head).is_some())
                    || acknowledged.as_ref().is_some_and(|(_, accepted)| {
                        accepted.doc.get_change_by_hash(head).is_some()
                    })
            }) {
                return Err("recovery projection contains history outside actual obligations or durable acknowledgement".into());
            }
        }
        Ok(())
    }

    /// Check that `projection` exactly recovers `originals`.
    ///
    /// # Errors
    ///
    /// Returns an error if the projection adds, drops or alters a recovered publication.
    pub fn validate_local_recovery_projection(
        &self,
        originals: &[NeighbourMsg],
        legacy: Option<&StatePeer>,
        projection: &[NeighbourMsg],
        selected: [u8; 32],
    ) -> Result<(), String> {
        let obligations = self.recovery_obligations(originals, legacy, selected)?;
        self.recovery_reservation(&obligations, selected)?;
        self.validate_projection_history(&obligations, projection, selected)
    }

    fn recovery_current_role(
        &self,
        route: &Route,
        envelope: &SignedOp,
        now: SystemTime,
    ) -> Result<MemberAuthority, String> {
        let policy = self.validate_present_media_policy()?;
        if !self.fresh_local_recovery_checkpoint(route, now)? {
            return Err("recovery projection needs fresh genuine governor checkpoint".into());
        }
        let role = self.authorize_document(
            envelope.author.as_bytes(),
            &route.app,
            &route.record,
            envelope,
            now,
        )?;
        let document = self
            .core
            .documents
            .get(route)
            .ok_or("recovery projection record missing")?;
        if let Some(policy) = policy {
            room::media::validate_bound_chat_change(
                &policy,
                &route.app,
                &route.record,
                &*document.doc,
            )
            .map_err(err)?;
        }
        Ok(role)
    }

    fn recovery_validated_channel(
        &self,
        route: &Route,
        envelope: &SignedOp,
        member: &MemberAuthority,
        now: SystemTime,
        reserved: bool,
    ) -> Result<ChannelState, String> {
        let mut channel = self
            .core
            .documents
            .get(route)
            .ok_or("recovery route is not admitted")?
            .channel
            .clone();
        channel
            .artifacts
            .insert(envelope.artifact, ArtifactKind::Note);
        channel
            .artifact_apps
            .insert(envelope.artifact, route.app.clone());
        channel
            .artifact_records
            .insert(envelope.artifact, route.record.clone());
        channel.room.clone_from(&self.core.room);
        channel.grants.clone_from(&member.policy.grants);
        if reserved {
            // Historical proof already checked the real persisted floor. This
            // isolated validation copy must still run role/clearance/redaction
            // checks after replay validation; it is never committed.
            channel.last_seq_per_author.remove(&envelope.author);
        }
        let bound = tilo_action_policy::roles::foreign_device_bound();
        collab_types::validate_admitted_inbound_op(
            envelope,
            &member.authority(&bound),
            &mut channel,
            now,
            Duration::from_secs(300),
        )
        .map_err(err)?;
        Ok(channel)
    }

    /// Check that the selected device may publish `projection` at `now`.
    ///
    /// # Errors
    ///
    /// Returns an error if the projection is invalid or its author is not currently
    /// authorized.
    pub fn authorize_local_recovery_projection(
        &self,
        originals: &[NeighbourMsg],
        legacy: Option<&StatePeer>,
        projection: &[NeighbourMsg],
        selected: [u8; 32],
        now: SystemTime,
    ) -> Result<(), String> {
        self.validate_local_recovery_projection(originals, legacy, projection, selected)?;
        for message in projection {
            let local = self.historical_projection(message, selected)?;
            if !crate::outbox::fresh(message, now) {
                return Err("recovery projection is not currently fresh".into());
            }
            let NeighbourMsg::AppDocument { envelope, .. } = message else {
                return Err("recovery projection is not an app document".into());
            };
            let role = self.recovery_current_role(&local.route, envelope, now)?;
            self.recovery_validated_channel(&local.route, envelope, &role, now, true)?;
        }
        Ok(())
    }

    /// Sign the recovery projection's successors, returning them.
    ///
    /// # Errors
    ///
    /// Returns an error if the projection is invalid or cannot be signed.
    pub fn reconcile_local_recovery_projection(
        &mut self,
        originals: &[NeighbourMsg],
        legacy: Option<&StatePeer>,
        projection: &[NeighbourMsg],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let selected = key.verifying_key().to_bytes();
        let obligations = self.recovery_obligations(originals, legacy, selected)?;
        let reservation = self.recovery_reservation(&obligations, selected)?;
        self.validate_projection_history(&obligations, projection, selected)?;
        let mut candidate = self.clone();
        let mut replacement = Vec::new();
        for (route, history) in &obligations {
            let route_budget = reservation
                .routes()
                .find(|(app, record, _)| *app == route.app && *record == route.record)
                .map(|(_, _, bytes)| bytes)
                .ok_or("projection route reservation missing")?;
            let mut reusable = None;
            for message in projection {
                let local = candidate.historical_projection(message, selected)?;
                if &local.route != route
                    || local.admission.sequence() <= history.original_sequence
                    || !crate::outbox::fresh(message, now)
                    || candidate.checkpoint_sequence_covers(message)
                    || publication_size(std::slice::from_ref(message))? > route_budget
                {
                    continue;
                }
                if let Some(carried) = candidate.recovery_publication_history(&local)?
                    && history
                        .doc
                        .get_heads()
                        .iter()
                        .all(|head| carried.get_change_by_hash(head).is_some())
                {
                    let NeighbourMsg::AppDocument { envelope, .. } = message else {
                        return Err("recovery projection is not an app document".into());
                    };
                    let role = candidate.recovery_current_role(route, envelope, now)?;
                    candidate.recovery_validated_channel(route, envelope, &role, now, true)?;
                    reusable = Some(message.clone());
                    break;
                }
            }
            let message = if let Some(message) = reusable {
                message
            } else {
                let author = Ed25519PublicKey::from_bytes(selected);
                let sequence = candidate
                    .core
                    .documents
                    .get(route)
                    .ok_or("recovery route is not admitted")?
                    .channel
                    .last_seq_per_author
                    .get(&author)
                    .copied()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or("recovery projection sequence exhausted")?;
                let mut envelope = history_envelope(
                    &candidate.core.room,
                    route,
                    &history.doc,
                    author,
                    now,
                    sequence,
                )?;
                let role = candidate.recovery_current_role(route, &envelope, now)?;
                collab_types::sign_op(&mut envelope, key).map_err(err)?;
                let channel =
                    candidate.recovery_validated_channel(route, &envelope, &role, now, false)?;
                // This transmission introduces no new data or exact admission.
                // The existing durable watermark reserves its sequence for every
                // later original/projection and prevents local echo re-admission.
                candidate
                    .core
                    .documents
                    .get_mut(route)
                    .ok_or("recovery route disappeared")?
                    .channel = channel;
                NeighbourMsg::AppDocument {
                    peer: peer_handle(&selected),
                    app_id: route.app.clone(),
                    cluster_id: candidate.core.room.clone(),
                    record_id: route.record.clone(),
                    envelope: Box::new(envelope),
                }
            };
            replacement.push(message);
        }
        if publication_size(&replacement)? > reservation.max_encoded_bytes() {
            return Err("recovery projection exceeded reserved representation".into());
        }
        candidate.validate_projection_history(&obligations, &replacement, selected)?;
        *self = candidate;
        Ok(replacement)
    }
}

fn add_history(
    routes: &mut BTreeMap<Route, RecoveryHistory>,
    route: Route,
    mut doc: automerge::Automerge,
    sequence: u64,
) -> Result<(), String> {
    if let Some(existing) = routes.get_mut(&route) {
        existing.doc.merge(&mut doc).map_err(err)?;
        existing.original_sequence = existing.original_sequence.max(sequence);
    } else {
        if routes.len() == MAX_PUBLICATIONS {
            return Err("projection reservation route count limit".into());
        }
        routes.insert(
            route,
            RecoveryHistory {
                doc,
                original_sequence: sequence,
            },
        );
    }
    Ok(())
}

fn varint_width(mut value: u64) -> usize {
    let mut bytes = 1usize;
    while value >= 128 {
        value >>= 7;
        // At most ten iterations for a u64.
        bytes = bytes.saturating_add(1);
    }
    bytes
}

fn decimal_digits(value: u8) -> usize {
    if value >= 100 {
        3
    } else if value >= 10 {
        2
    } else {
        1
    }
}
fn varint_decimal_digits(mut value: u64) -> usize {
    // At most ten varint bytes of at most three digits each.
    let mut digits = 0usize;
    while value >= 128 {
        digits = digits.saturating_add(decimal_digits(((value & 127) as u8) | 128));
        value >>= 7;
    }
    // Below 128 after the loop.
    digits.saturating_add(decimal_digits(u8::try_from(value).unwrap_or(u8::MAX)))
}
