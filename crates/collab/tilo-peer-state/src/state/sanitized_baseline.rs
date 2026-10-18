//! Explicit, signed history replacement. Never carried as an ordinary room delta.
use super::{StatePeer, SystemTime};
use automerge::{
    AutoCommit, ObjId, ObjType, Prop, ROOT, ReadDoc, Value, transaction::Transactable,
};
use ed25519_dalek::{Signature, Signer, Verifier};
use serde::{Deserialize, Serialize};

pub(crate) const FIELD: &str = "sanitized_room_baseline_v1";
const DOMAIN: &[u8] = b"tilo/sanitized-room-baseline/v1\0";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BaselineBinding {
    pub room: String,
    pub generation: u64,
    pub prior_chain_hash: [u8; 32],
}

/// An authenticated replacement for retired room history, not a mergeable delta.
/// Persist and publish this envelope atomically with replacement room bytes. Its
/// predecessor must already be admitted; initial joiners need a trusted anchor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanitizedRoomBaseline {
    pub room: String,
    pub generation: u64,
    pub prior_chain_hash: [u8; 32],
    pub governor: [u8; 32],
    pub document: Vec<u8>,
    pub signature: Vec<u8>,
}
/// Trusted current-chain pin supplied by authenticated join/retention metadata,
/// never copied from an unverified baseline by the receiving caller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanitizedRoomBaselineAnchor {
    pub generation: u64,
    pub prior_chain_hash: [u8; 32],
    pub document_hash: [u8; 32],
}
impl SanitizedRoomBaseline {
    /// Pin for storage alongside an already authenticated, committed publication.
    #[must_use]
    pub fn anchor(&self) -> SanitizedRoomBaselineAnchor {
        SanitizedRoomBaselineAnchor {
            generation: self.generation,
            prior_chain_hash: self.prior_chain_hash,
            document_hash: *blake3::hash(&self.document).as_bytes(),
        }
    }
    /// Validate a replacement envelope against a separately authenticated pin.
    /// This authenticates framing and signature only; callers still validate
    /// room governance, receipts and schema before granting it authority.
    /// # Errors
    /// Refuses a foreign signer, mismatched anchor, or malformed checkpoint.
    pub fn verify_anchored_document(
        &self,
        room: &str,
        governor: [u8; 32],
        anchor: &SanitizedRoomBaselineAnchor,
    ) -> Result<AutoCommit, String> {
        if self.room != room
            || self.governor != governor
            || anchor.generation == 0
            || self.anchor() != *anchor
        {
            return Err("baseline does not match authenticated room anchor".into());
        }
        let key = ed25519_dalek::VerifyingKey::from_bytes(&governor).map_err(|e| e.to_string())?;
        key.verify(
            &self.signing_bytes()?,
            &Signature::from_slice(&self.signature).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let mut doc = tilo_automerge_isolate::load(&self.document).map_err(|e| e.to_string())?;
        if binding(&doc)?
            != Some(BaselineBinding {
                room: self.room.clone(),
                generation: self.generation,
                prior_chain_hash: self.prior_chain_hash,
            })
            || !doc.get_missing_deps(&[]).is_empty()
            || doc.get_changes(&[]).len() != 1
        {
            return Err("invalid anchored baseline history or binding".into());
        }
        super::task_sanitization::validate_private_task_baseline(&doc)?;
        Ok(doc)
    }
    fn signing_bytes(&self) -> Result<Vec<u8>, String> {
        let mut bytes = DOMAIN.to_vec();
        bytes.extend(
            postcard::to_allocvec(&(
                &self.room,
                self.generation,
                self.prior_chain_hash,
                self.governor,
                blake3::hash(&self.document).as_bytes(),
            ))
            .map_err(|e| e.to_string())?,
        );
        Ok(bytes)
    }
}

pub(crate) fn binding(doc: &AutoCommit) -> Result<Option<BaselineBinding>, String> {
    match doc.get(ROOT, FIELD).map_err(|e| e.to_string())? {
        None => Ok(None),
        Some((Value::Scalar(value), _)) => match value.as_ref() {
            automerge::ScalarValue::Bytes(bytes) => {
                let (value, tail) = postcard::take_from_bytes(bytes).map_err(|e| e.to_string())?;
                if !tail.is_empty() {
                    return Err("trailing baseline binding bytes".into());
                }
                Ok(Some(value))
            }
            _ => Err("invalid baseline binding".into()),
        },
        _ => Err("invalid baseline binding".into()),
    }
}

pub(crate) fn chain_hash(doc: &AutoCommit) -> [u8; 32] {
    let mut heads = doc.base_heads();
    heads.sort();
    let mut hash = blake3::Hasher::new_derive_key("Tilo room predecessor chain v1");
    for head in heads {
        hash.update(&head.0);
    }
    *hash.finalize().as_bytes()
}

/// Rebuild visible Automerge values, not a schema projection or document clone.
/// Conflicts fail closed: silently selecting one could discard authority evidence.
pub(crate) fn rebuild(source: &AutoCommit) -> Result<AutoCommit, String> {
    if !source.get_missing_deps(&[]).is_empty() {
        return Err("room history is incomplete".into());
    }
    let mut target = AutoCommit::new_with_encoding(source.text_encoding());
    copy_object(source, &ROOT, &mut target, &ROOT, 0)?;
    Ok(target)
}
fn copy_object(
    source: &AutoCommit,
    from: &ObjId,
    target: &mut AutoCommit,
    to: &ObjId,
    depth: usize,
) -> Result<(), String> {
    if depth > 128 {
        return Err("room object nesting exceeds baseline limit".into());
    }
    let kind = source.object_type(from).map_err(|e| e.to_string())?;
    if kind == ObjType::Text {
        for span in source.spans(from).map_err(|e| e.to_string())? {
            let index = target.length(to);
            match span {
                automerge::Span::Text { text, .. } => {
                    target
                        .splice_text(to, index, 0, &text)
                        .map_err(|e| e.to_string())?;
                }
                automerge::Span::Block(_) => {
                    let Some((Value::Object(ObjType::Map), block)) =
                        source.get(from, index).map_err(|e| e.to_string())?
                    else {
                        return Err("room text block has no map identity".into());
                    };
                    let child = target.split_block(to, index).map_err(|e| e.to_string())?;
                    copy_object(source, &block, target, &child, depth.saturating_add(1))?;
                }
            }
        }
        for mark in source.marks(from).map_err(|e| e.to_string())? {
            target
                .mark(to, mark, automerge::marks::ExpandMark::None)
                .map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    let props: Vec<Prop> = match kind {
        ObjType::Map | ObjType::Table => source.keys(from).map(Prop::from).collect(),
        ObjType::List => (0..source.length(from)).map(Prop::from).collect(),
        ObjType::Text => return Err("text was not copied as text".into()),
    };
    for prop in props {
        let values = source
            .get_all(from, prop.clone())
            .map_err(|e| e.to_string())?;
        if values.len() != 1 {
            return Err("conflicted room value cannot be sanitized without resolution".into());
        }
        let (value, object) = values.into_iter().next().ok_or("missing room value")?;
        let list = kind == ObjType::List;
        match value {
            Value::Scalar(value) => {
                if list {
                    let Prop::Seq(index) = prop else {
                        return Err("invalid list position".into());
                    };
                    target
                        .insert(to, index, value.into_owned())
                        .map_err(|e| e.to_string())?;
                } else {
                    target
                        .put(to, prop, value.into_owned())
                        .map_err(|e| e.to_string())?;
                }
            }
            Value::Object(kind) => {
                let child = if list {
                    let Prop::Seq(index) = prop else {
                        return Err("invalid list position".into());
                    };
                    target.insert_object(to, index, kind)
                } else {
                    target.put_object(to, prop, kind)
                }
                .map_err(|e| e.to_string())?;
                copy_object(source, &object, target, &child, depth.saturating_add(1))?;
            }
        }
    }
    Ok(())
}

impl StatePeer {
    /// Authenticate a transport retry against the owner's durably installed baseline.
    /// This observes an existing checkpoint; it never replaces or merges room state.
    ///
    /// # Errors
    /// Refuses changed authority, framing, signatures, or an incomplete/divergent chain.
    pub fn lookup_exact_baseline_report(
        &self,
        origin: super::TransportOrigin,
        message: &crate::NeighbourMsg,
        installed: &SanitizedRoomBaseline,
        now: SystemTime,
    ) -> Result<Option<crate::AdmissionReport>, crate::AdmissionError> {
        let crate::NeighbourMsg::SanitizedRoomBaseline { room, baseline } = message else {
            return Ok(None);
        };
        let (incoming, tail) = postcard::take_from_bytes::<SanitizedRoomBaseline>(baseline)
            .map_err(|e| e.to_string())?;
        if !tail.is_empty() || incoming != *installed {
            return Ok(None);
        }
        let (source, _) = self.resolve_origin(origin, message, now)?;
        if room != &self.core.room || source != installed.governor {
            return Err("baseline transport author or framing mismatch".into());
        }
        self.validate_native_publisher(source, now)?;
        let mut checkpoint = installed.verify_anchored_document(
            &self.core.room,
            self.core.authority.device_public_key,
            &installed.anchor(),
        )?;
        let mut current = self.core.room_doc.as_deref().ok_or("room absent")?.clone();
        validate_sync(Some(&checkpoint), &mut current)?;
        if checkpoint
            .get_heads()
            .iter()
            .any(|head| current.get_change_by_hash(head).is_none())
        {
            return Err("installed baseline is not an ancestor of the admitted room".into());
        }
        Ok(Some(crate::AdmissionReport::metadata(
            super::ReceiveOutcome {
                duplicate: true,
                ..Default::default()
            },
        )))
    }

    /// The admitted room's history generation; zero denotes the original chain.
    /// # Errors
    /// Refuses malformed replacement metadata.
    pub fn sanitized_room_generation(&self) -> Result<u64, String> {
        Ok(self
            .core
            .room_doc
            .as_deref()
            .map(binding)
            .transpose()?
            .flatten()
            .map_or(0, |value| value.generation))
    }
    /// Sign the current sanitized baseline for replacement publication.
    /// # Errors
    /// Requires exact, current governor custody and a previously sanitized room.
    pub fn sanitized_room_baseline(
        &self,
        key: &ed25519_dalek::SigningKey,
        now: SystemTime,
    ) -> Result<SanitizedRoomBaseline, String> {
        self.validate_native_publisher(key.verifying_key().to_bytes(), now)
            .map_err(|e| e.to_string())?;
        let doc = self.core.room_doc.as_deref().ok_or("room absent")?;
        let binding = binding(doc)?.ok_or("room has no sanitized baseline")?;
        super::task_sanitization::validate_private_task_baseline(doc)?;
        let mut copy = doc.clone();
        if copy.get_changes(&[]).len() != 1 {
            return Err("baseline publication requires the exact fresh checkpoint".into());
        }
        let mut baseline = SanitizedRoomBaseline {
            room: binding.room,
            generation: binding.generation,
            prior_chain_hash: binding.prior_chain_hash,
            governor: key.verifying_key().to_bytes(),
            document: copy.save(),
            signature: Vec::new(),
        };
        baseline.signature = key.sign(&baseline.signing_bytes()?).to_bytes().to_vec();
        Ok(baseline)
    }

    /// Bootstrap a fresh peer from an independently authenticated current-chain pin.
    /// Existing replicas use predecessor-checked replacement or explicit anchored
    /// re-enrollment; ordinary sync never selects either recovery operation.
    /// # Errors
    /// Refuses an unanchored generation, equivocation, non-current governor,
    /// malformed room, or any attempt to reset an existing replica.
    pub fn admit_sanitized_room_baseline_bootstrap(
        &mut self,
        baseline: &SanitizedRoomBaseline,
        anchor: &SanitizedRoomBaselineAnchor,
        now: SystemTime,
    ) -> Result<(), String> {
        if self.core.room_doc.is_some() || !self.core.pending.is_empty() {
            return Err("baseline bootstrap requires an empty replica".into());
        }
        if anchor.generation == 0 || baseline.anchor() != *anchor || baseline.room != self.core.room
        {
            return Err("baseline does not match authenticated current-chain anchor".into());
        }
        self.validate_native_publisher(baseline.governor, now)
            .map_err(|e| e.to_string())?;
        let key = ed25519_dalek::VerifyingKey::from_bytes(&baseline.governor)
            .map_err(|e| e.to_string())?;
        key.verify(
            &baseline.signing_bytes()?,
            &Signature::from_slice(&baseline.signature).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let mut room =
            tilo_automerge_isolate::load(&baseline.document).map_err(|e| e.to_string())?;
        let expected = BaselineBinding {
            room: baseline.room.clone(),
            generation: baseline.generation,
            prior_chain_hash: baseline.prior_chain_hash,
        };
        if binding(&room)? != Some(expected)
            || !room.get_missing_deps(&[]).is_empty()
            || room.get_changes(&[]).len() != 1
        {
            return Err("invalid baseline bootstrap history or binding".into());
        }
        if !matches!(room.get(ROOT, "room").map_err(|e| e.to_string())?,
            Some((Value::Scalar(value), _)) if matches!(value.as_ref(), automerge::ScalarValue::Str(room) if room.as_str() == baseline.room))
        {
            return Err("baseline bootstrap declares a different room".into());
        }
        super::task_sanitization::validate_private_task_baseline(&room)?;
        let governor = hex::encode(baseline.governor);
        crate::topology::validate(None, &room, &governor, &governor)?;
        crate::coverage::monotonic(None, &room)?;
        crate::durability::monotonic(None, &room)?;
        let media = crate::room::media::validate_transition(
            None,
            &room,
            crate::room::media::TransitionMode::Admission,
        )
        .map_err(|e| e.to_string())?;
        let mut candidate = self.clone();
        if let Some(policy) = media {
            candidate.validate_media_bindings(&policy, false)?;
        }
        crate::room::admit_receipts(&mut candidate.core.receipts, &room)
            .map_err(|e| e.to_string())?;
        candidate.import_governed_certificates(&room, now)?;
        candidate.core.room_doc = Some(room.into());
        candidate.certificate(&baseline.governor, now)?;
        candidate.role(&baseline.governor, now)?;
        *self = candidate;
        Ok(())
    }

    /// Recover a lagging replica using a separately authenticated current-chain pin.
    /// This is an explicit re-enrollment operation, never an ordinary sync fallback.
    /// The caller must obtain the pin from an authenticated current governor exchange,
    /// not derive it from the incoming baseline. Resource documents, certificate and
    /// receipt evidence are retained; queued input from retired room history is dropped.
    /// # Errors
    /// Refuses replay, same-generation equivocation, authority changes, lost receipts,
    /// or a baseline that does not match the independently authenticated pin.
    pub fn reenroll_sanitized_room_baseline(
        &mut self,
        baseline: &SanitizedRoomBaseline,
        anchor: &SanitizedRoomBaselineAnchor,
        now: SystemTime,
    ) -> Result<(), String> {
        let before = self
            .core
            .room_doc
            .as_deref()
            .ok_or("re-enrollment requires an existing replica")?;
        if baseline.generation <= self.sanitized_room_generation()? {
            return Err("re-enrollment baseline replay or generation equivocation".into());
        }
        let room = baseline.verify_anchored_document(
            &self.core.room,
            self.core.authority.device_public_key,
            anchor,
        )?;
        crate::coverage::monotonic(Some(before), &room)?;
        crate::durability::monotonic(Some(before), &room)?;
        if !crate::room::immutable_receipts(Some(before), &room) {
            return Err("re-enrollment changed immutable receipts".into());
        }
        let mut candidate = self.clone();
        let mut pending = std::mem::take(&mut candidate.core.pending);
        pending.retain(|(_, message, _)| {
            !matches!(
                message,
                crate::NeighbourMsg::SyncResp { .. }
                    | crate::NeighbourMsg::SanitizedRoomBaseline { .. }
            )
        });
        candidate.core.room_doc = None;
        candidate.admit_sanitized_room_baseline_bootstrap(baseline, anchor, now)?;
        let mut pending_bytes = 0usize;
        for (_, message, _) in &pending {
            let size = crate::postcard_sink::encoded_len(message, usize::MAX)
                .map_err(|e| e.to_string())?;
            pending_bytes = pending_bytes
                .checked_add(size)
                .ok_or("pending bytes overflow")?;
        }
        super::backend::validate_pending(&pending, pending_bytes).map_err(|e| e.to_string())?;
        candidate.core.pending = pending;
        candidate.core.pending_bytes = pending_bytes;
        *self = candidate;
        Ok(())
    }

    /// Replace an exact admitted predecessor with a governor-signed baseline.
    /// This is deliberately unavailable to ordinary room sync and fresh joiners.
    /// # Errors
    /// Rejects replay, generation reset, equivocation, changed authority, malformed
    /// history, non-current certificates, and an unpinned signer. Nothing mutates on error.
    pub fn admit_sanitized_room_baseline(
        &mut self,
        baseline: &SanitizedRoomBaseline,
        now: SystemTime,
    ) -> Result<(), String> {
        self.validate_native_publisher(baseline.governor, now)
            .map_err(|e| e.to_string())?;
        let key = ed25519_dalek::VerifyingKey::from_bytes(&baseline.governor)
            .map_err(|e| e.to_string())?;
        let signature = Signature::from_slice(&baseline.signature).map_err(|e| e.to_string())?;
        key.verify(&baseline.signing_bytes()?, &signature)
            .map_err(|e| e.to_string())?;
        if self
            .core
            .pending
            .iter()
            .any(|(_, message, _)| matches!(message, crate::NeighbourMsg::SyncResp { .. }))
        {
            return Err("baseline requires settled room input".into());
        }
        let before = self
            .core
            .room_doc
            .as_deref()
            .ok_or("baseline requires admitted predecessor")?;
        let generation = binding(before)?
            .map_or(0, |value| value.generation)
            .checked_add(1)
            .ok_or("baseline generation exhausted")?;
        if baseline.room != self.core.room
            || baseline.generation != generation
            || baseline.prior_chain_hash != chain_hash(before)
        {
            return Err("baseline predecessor or generation mismatch".into());
        }
        let mut candidate =
            tilo_automerge_isolate::load(&baseline.document).map_err(|e| e.to_string())?;
        if !candidate.get_missing_deps(&[]).is_empty() || candidate.get_changes(&[]).len() != 1 {
            return Err("baseline is not a single fresh room change".into());
        }
        if !matches!(candidate.get(ROOT, "room").map_err(|e| e.to_string())?,
            Some((Value::Scalar(value), _)) if matches!(value.as_ref(), automerge::ScalarValue::Str(room) if room.as_str() == baseline.room))
        {
            return Err("baseline declares a different room".into());
        }
        let expected = BaselineBinding {
            room: baseline.room.clone(),
            generation,
            prior_chain_hash: baseline.prior_chain_hash,
        };
        if binding(&candidate)? != Some(expected)
            || crate::room::governance_of(Some(before))
                != crate::room::governance_of(Some(&candidate))
        {
            return Err("baseline changed authority evidence or binding".into());
        }
        super::task_sanitization::validate_private_task_baseline(&candidate)?;
        crate::coverage::monotonic(Some(before), &candidate)?;
        crate::durability::monotonic(Some(before), &candidate)?;
        if !crate::room::immutable_receipts(Some(before), &candidate) {
            return Err("baseline changed immutable receipts".into());
        }
        let mut receipts = self.core.receipts.clone();
        crate::room::admit_receipts(&mut receipts, &candidate).map_err(|e| e.to_string())?;
        self.core.receipts = receipts;
        self.core.room_doc = Some(candidate.into());
        Ok(())
    }
}

/// Ordinary sync cannot resurrect the retired CRDT roots or alter the reset fence.
///
/// # Errors
/// Rejects changed replacement metadata, missing history, or multiple room roots.
pub fn validate_sync(before: Option<&AutoCommit>, after: &mut AutoCommit) -> Result<(), String> {
    let prior = before.map(binding).transpose()?.flatten();
    let next = binding(after)?;
    if prior != next {
        return Err("room baseline changes require signed replacement admission".into());
    }
    if next.is_some()
        && (!after.get_missing_deps(&[]).is_empty()
            || after
                .get_changes(&[])
                .iter()
                .filter(|change| change.deps().is_empty())
                .count()
                != 1)
    {
        return Err("room sync resurrects retired history".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NeighbourMsg, TransportOrigin};
    use tilo_user_identity::RevocationList;

    fn replacement(
        peer: &mut StatePeer,
        key: &ed25519_dalek::SigningKey,
        now: SystemTime,
    ) -> SanitizedRoomBaseline {
        let before = peer.core.room_doc.as_deref().unwrap();
        let metadata = BaselineBinding {
            room: peer.room().into(),
            generation: peer
                .sanitized_room_generation()
                .unwrap()
                .checked_add(1)
                .unwrap(),
            prior_chain_hash: chain_hash(before),
        };
        let mut room = rebuild(before).unwrap();
        room.put(ROOT, FIELD, postcard::to_allocvec(&metadata).unwrap())
            .unwrap();
        room.commit();
        peer.core.room_doc = Some(room.into());
        peer.sanitized_room_baseline(key, now).unwrap()
    }

    fn frame(baseline: &SanitizedRoomBaseline) -> NeighbourMsg {
        NeighbourMsg::SanitizedRoomBaseline {
            room: baseline.room.clone(),
            baseline: postcard::to_allocvec(baseline).unwrap(),
        }
    }

    #[test]
    fn signed_automatic_report_bodies_are_not_a_sanitized_checkpoint() {
        let (mut source, key, _, _, now) = crate::admission_tests::seeded();
        let mut receiver = source.clone();
        let mut baseline = replacement(&mut source, &key, now);
        let mut doc = AutoCommit::load(&baseline.document).unwrap();
        let tasks = doc
            .put_object(
                ROOT,
                tilo_desktop_model::projection::ROOM_TASKS_KEY,
                ObjType::Map,
            )
            .unwrap();
        let event = tilo_desktop_model::WorkspaceTaskAuditEvent::new(
            7,
            "host",
            "abandon",
            std::iter::empty(),
        )
        .noting("original automatic report must remain private");
        let task = serde_json::json!({"route": "rules_only", "audit": [event]});
        doc.put(tasks, "task", serde_json::to_vec(&task).unwrap())
            .unwrap();
        let mut doc = rebuild(&doc).unwrap();
        baseline.document = doc.save();
        // A valid governor signature and trusted anchor do not establish that
        // report prose was removed by the producer's historical sanitizer.
        baseline.signature = key
            .sign(&baseline.signing_bytes().unwrap())
            .to_bytes()
            .to_vec();
        assert!(
            baseline
                .verify_anchored_document(&baseline.room, baseline.governor, &baseline.anchor())
                .is_err()
        );
        assert!(
            receiver
                .admit_sanitized_room_baseline(&baseline, now)
                .is_err()
        );
        source.core.room_doc = Some(doc.into());
        assert!(source.sanitized_room_baseline(&key, now).is_err());
    }

    #[test]
    fn replacement_retry_authenticates_and_retired_roots_cannot_return() {
        let (mut source, key, _, _, now) = crate::admission_tests::seeded();
        let mut receiver = source.clone();
        let retired = source.room_message();
        let baseline = replacement(&mut source, &key, now);
        let message = frame(&baseline);
        let origin = TransportOrigin::RelayDelta(baseline.governor);
        receiver
            .receive_from_report(origin, message.clone(), now)
            .unwrap();
        assert_eq!(receiver.sanitized_room_generation().unwrap(), 1);
        let durable = receiver.durable().unwrap();
        assert!(
            receiver
                .lookup_exact_baseline_report(origin, &message, &baseline, now)
                .unwrap()
                .is_some()
        );
        assert_eq!(receiver.durable().unwrap(), durable);
        assert!(
            receiver
                .lookup_exact_baseline_report(
                    TransportOrigin::UntrustedPresence(baseline.governor),
                    &message,
                    &baseline,
                    now
                )
                .is_err()
        );
        let mut tampered = baseline.clone();
        tampered.signature[0] ^= 1;
        assert!(
            receiver
                .receive_from_report(origin, frame(&tampered), now)
                .is_err()
        );
        assert!(receiver.receive_from_report(origin, retired, now).is_err());
        assert_eq!(receiver.durable().unwrap(), durable);
        let second = replacement(&mut source, &key, now);
        receiver
            .receive_from_report(origin, frame(&second), now)
            .unwrap();
        assert!(
            receiver
                .lookup_exact_baseline_report(origin, &message, &baseline, now)
                .is_err()
        );
        assert!(receiver.receive_from_report(origin, message, now).is_err());
    }

    #[test]
    fn bootstrap_and_lagging_reenrollment_require_independent_current_anchor() {
        let (mut source, key, _, cert, now) = crate::admission_tests::seeded();
        let mut lagging = source.clone();
        let resource = lagging.document("test.app", "tab:1");
        let first = replacement(&mut source, &key, now);
        let current = replacement(&mut source, &key, now);
        let mut fresh = StatePeer::new(
            source.room(),
            source.authority().clone(),
            vec![cert],
            RevocationList::new(),
            now,
        )
        .unwrap();
        let origin = TransportOrigin::RelayDelta(current.governor);
        assert!(
            fresh
                .receive_from_report(origin, frame(&current), now)
                .is_err()
        );
        assert!(
            fresh
                .admit_sanitized_room_baseline_bootstrap(&first, &current.anchor(), now)
                .is_err()
        );
        fresh
            .admit_sanitized_room_baseline_bootstrap(&current, &current.anchor(), now)
            .unwrap();
        assert_eq!(fresh.sanitized_room_generation().unwrap(), 2);
        assert!(
            lagging
                .admit_sanitized_room_baseline(&current, now)
                .is_err()
        );
        assert!(
            lagging
                .reenroll_sanitized_room_baseline(&first, &current.anchor(), now)
                .is_err()
        );
        lagging
            .reenroll_sanitized_room_baseline(&current, &current.anchor(), now)
            .unwrap();
        assert_eq!(lagging.document("test.app", "tab:1"), resource);
        assert!(
            lagging
                .reenroll_sanitized_room_baseline(&current, &current.anchor(), now)
                .is_err()
        );
    }
}
