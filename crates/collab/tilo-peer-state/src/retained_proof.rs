//! Historical retention proofs over the frozen neighbour protocol.
//!
//! The caller supplies a locally pinned authority and an authenticated source;
//! an Envelope's claimed author or zero signature is not authentication. These
//! proofs neither admit live packets nor select/retire stored files. Historical
//! signature verification deliberately does not replace live freshness,
//! membership, revocation or capability checks in `StatePeer`.
use crate::{NeighbourMsg, coverage::Coverage, durability::DurableReceipt};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, Automerge, ROOT, ReadDoc, ScalarValue, Value};
use collab_types::{DocumentMutation, WholeDocumentPayload};
use std::collections::BTreeMap;
use tilo_user_identity::DeviceCert;

pub use tilo_relay_protocol::retention::RetentionAuthority;

/// Derive the locally pinned retention authority from an existing governed
/// room document without creating or replacing room state.
///
/// # Errors
///
/// Returns an error if the room id is missing or `governor` is not the governed owner.
pub fn retention_authority_from_governance(
    document: &AutoCommit,
    governor: DeviceCert,
) -> Result<RetentionAuthority, String> {
    let room = document
        .get(ROOT, "room")
        .map_err(|error| error.to_string())?
        .and_then(|(value, _)| value.as_str().map(ToOwned::to_owned))
        .ok_or("governed room id is missing")?;
    let authority = RetentionAuthority::new(&room, governor)?;
    if crate::room::governance_of(Some(document)).0.as_deref()
        != Some(&hex::encode(authority.governor().user_public_key))
    {
        return Err("retention governor is not the governed room owner".into());
    }
    identities(&authority, None, document)
        .map_err(|_| "retention governor is absent or revoked in governance".to_string())?;
    Ok(authority)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomInput {
    Checkpoint,
    Delta,
}
/// Both errors are conservative: retain the input. Unpaired permits retry with
/// another exact room proof; Unknown grants no interpretation/retirement right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofError {
    Unknown,
    Unpaired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverageDecision {
    Covered,
    NotCovered,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloorSafety {
    Safe,
    Blocked,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TailDecision {
    pub coverage: CoverageDecision,
    pub floor: FloorSafety,
}

/// One verified room document. A Delta result depends on its supplied prior
/// basis; the storage caller must retain those exact prerequisite frames.
#[derive(Clone)]
pub struct VerifiedRoom {
    authority: RetentionAuthority,
    doc: AutoCommit,
    input_hash: [u8; 32],
    prior_heads: Option<Vec<String>>,
}
/// One route, one exact signed snapshot and its route's receipt. There is no
/// historical snapshot cache or requirement for one aggregate-wide generation.
pub struct VerifiedSnapshot {
    authority: RetentionAuthority,
    doc: Automerge,
    receipt: DurableReceipt,
    input_hash: [u8; 32],
}
/// Replay-readiness proof for one ordered route tail. The immutable snapshot
/// remains the sole floor and retirement authority; this cursor only advances
/// a separate causal document after each fully verified increment is ready.
pub struct OrderedTailReadiness {
    authority: RetentionAuthority,
    coverage: Coverage,
    snapshot: Automerge,
    cumulative: Automerge,
}

fn decode(body: &[u8]) -> Result<NeighbourMsg, ProofError> {
    let (msg, rest) = postcard::take_from_bytes(body).map_err(|_| ProofError::Unknown)?;
    if !rest.is_empty() {
        return Err(ProofError::Unknown);
    }
    Ok(msg)
}

// Strict framing is necessary: incremental loaders may accept valid prefixes,
// and a loader can skip an appended document with already-known heads.
fn first_chunk(bytes: &[u8]) -> Option<(&[u8], &[u8], u8)> {
    if !bytes.starts_with(&[0x85, 0x6f, 0x4a, 0x83]) {
        return None;
    }
    let kind = *bytes.get(8)?;
    if kind > 3 {
        return None;
    }
    let (mut header, mut len, mut shift) = (9usize, 0usize, 0u32);
    loop {
        let byte = *bytes.get(header)?;
        header = header.checked_add(1)?;
        let low = usize::from(byte & 0x7f);
        len = len.checked_add(low.checked_mul(1usize.checked_shl(shift)?)?)?;
        if byte & 0x80 == 0 {
            if shift != 0 && low == 0 {
                return None;
            }
            break;
        }
        shift = shift.checked_add(7)?;
    }
    let (chunk, rest) = bytes.split_at_checked(header.checked_add(len)?)?;
    Some((chunk, rest, kind))
}
fn strict_chunks(mut bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    while !bytes.is_empty() {
        let Some((chunk, rest, kind)) = first_chunk(bytes) else {
            return false;
        };
        match Automerge::load(chunk) {
            Ok(_) => {}
            Err(automerge::AutomergeError::MissingDeps) if kind != 0 => {}
            Err(_) => return false,
        }
        bytes = rest;
    }
    true
}
fn document(bytes: &[u8]) -> Result<Automerge, ProofError> {
    if !strict_chunks(bytes) {
        return Err(ProofError::Unknown);
    }
    let doc = Automerge::load(bytes).map_err(|_| ProofError::Unknown)?;
    if !doc.get_missing_deps(&[]).is_empty() {
        return Err(ProofError::Unknown);
    }
    Ok(doc)
}
fn room_changes(changes: &[Vec<u8>]) -> Result<(), ProofError> {
    if changes.is_empty() {
        return Err(ProofError::Unknown);
    }
    for bytes in changes {
        let Some((_, rest, kind)) = first_chunk(bytes) else {
            return Err(ProofError::Unknown);
        };
        if !rest.is_empty() || !matches!(kind, 1 | 2) {
            return Err(ProofError::Unknown);
        }
        // Change::try_from validates columns but not the stored checksum.
        // Validate this exact single Change through the loader first; only an
        // integrity-valid dependent kind 1/2 chunk may return MissingDeps.
        if !strict_chunks(bytes) {
            return Err(ProofError::Unknown);
        }
        automerge::Change::try_from(bytes.as_slice()).map_err(|_| ProofError::Unknown)?;
    }
    Ok(())
}
fn valid_coverage(authority: &RetentionAuthority, key: &str, coverage: &Coverage) -> bool {
    coverage.room == authority.room()
        && key == crate::coverage::route_key(&coverage.app, &coverage.record)
        && coverage.author == authority.governor().device_public_key
        && coverage.sequence > 0
        && coverage.watermarks.get(&hex::encode(coverage.author)) == Some(&coverage.sequence)
        && coverage.watermarks.iter().all(|(author, seq)| {
            *seq > 0
                && hex::decode(author)
                    .is_ok_and(|bytes| bytes.len() == 32 && hex::encode(bytes) == *author)
        })
}

fn effect_receipts(doc: &AutoCommit) -> Result<BTreeMap<String, serde_json::Value>, ProofError> {
    let mut receipts = BTreeMap::new();
    let Some((Value::Object(automerge::ObjType::Map), object)) = doc
        .get(ROOT, "effect_receipts_v1")
        .map_err(|_| ProofError::Unknown)?
    else {
        return if doc
            .get(ROOT, "effect_receipts_v1")
            .map_err(|_| ProofError::Unknown)?
            .is_none()
        {
            Ok(receipts)
        } else {
            Err(ProofError::Unknown)
        };
    };
    for key in doc.keys(&object) {
        let Some((Value::Scalar(value), _)) =
            doc.get(&object, &key).map_err(|_| ProofError::Unknown)?
        else {
            return Err(ProofError::Unknown);
        };
        let ScalarValue::Bytes(bytes) = value.as_ref() else {
            return Err(ProofError::Unknown);
        };
        receipts.insert(
            key,
            serde_json::from_slice(bytes).map_err(|_| ProofError::Unknown)?,
        );
    }
    Ok(receipts)
}

fn identities(
    authority: &RetentionAuthority,
    before: Option<&AutoCommit>,
    doc: &AutoCommit,
) -> Result<(), ProofError> {
    let parse = |doc| {
        crate::room::governance_of(doc)
            .3
            .into_iter()
            .map(|(id, bytes)| {
                let identity: crate::state::GovernedIdentity =
                    serde_json::from_slice(&bytes).map_err(|_| ProofError::Unknown)?;
                if id != hex::encode(identity.device_cert.device_public_key) {
                    return Err(ProofError::Unknown);
                }
                tilo_user_identity::verify_device_cert(&identity.device_cert)
                    .map_err(|_| ProofError::Unknown)?;
                Ok((id, identity))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()
    };
    let prior = parse(before)?;
    let current = parse(Some(doc))?;
    let pinned = hex::encode(authority.governor().device_public_key);
    if current
        .get(&pinned)
        .is_none_or(|row| row.device_cert != *authority.governor())
    {
        return Err(ProofError::Unknown);
    }
    let mut revoked = tilo_user_identity::RevocationList::new();
    for (id, identity) in &current {
        if prior
            .get(id)
            .is_some_and(|old| old.device_cert != identity.device_cert)
        {
            return Err(ProofError::Unknown);
        }
        for entry in &identity.revocations {
            if entry.user_public_key != identity.device_cert.user_public_key
                || current
                    .get(&hex::encode(entry.revoked_device_public_key))
                    .is_some_and(|target| {
                        target.device_cert.user_public_key != entry.user_public_key
                    })
            {
                return Err(ProofError::Unknown);
            }
            revoked
                .insert(entry.clone())
                .map_err(|_| ProofError::Unknown)?;
        }
    }
    if revoked.is_revoked(&authority.governor().device_public_key) {
        return Err(ProofError::Unknown);
    }
    Ok(())
}

/// Verify a complete checkpoint, or apply one strictly decoded delta to a
/// verified dependency basis. A checkpoint is loaded independently even when
/// `prior` is present; it cannot borrow missing dependencies from the old basis.
/// `prior` additionally checks immutable governance and monotonic metadata.
///
/// # Errors
///
/// Returns a [`ProofError`] if the body does not verify against the authority or
/// `prior`.
pub fn verify_room(
    authority: &RetentionAuthority,
    source: [u8; 32],
    channel: [u8; 32],
    body: &[u8],
    kind: RoomInput,
    prior: Option<&VerifiedRoom>,
) -> Result<VerifiedRoom, ProofError> {
    if source != authority.governor().device_public_key
        || channel != *blake3::hash(b"shell-tree").as_bytes()
        || prior.is_some_and(|prior| prior.authority != *authority)
    {
        return Err(ProofError::Unknown);
    }
    let NeighbourMsg::SyncResp { room, changes } = decode(body)? else {
        return Err(ProofError::Unknown);
    };
    if room != authority.room() {
        return Err(ProofError::Unknown);
    }
    room_changes(&changes)?;
    let base = match kind {
        RoomInput::Checkpoint => None,
        RoomInput::Delta => Some(&prior.ok_or(ProofError::Unpaired)?.doc),
    };
    let doc = crate::room::candidate(base, &changes).map_err(|_| ProofError::Unknown)?;
    if !doc.get_missing_deps(&[]).is_empty() {
        return Err(ProofError::Unknown);
    }
    match doc.get(ROOT, "room").map_err(|_| ProofError::Unknown)? {
        Some((Value::Scalar(value), _)) if matches!(value.as_ref(), ScalarValue::Str(value) if value.as_str() == authority.room()) =>
            {}
        _ => return Err(ProofError::Unknown),
    }
    let before = prior.map(|prior| &prior.doc);
    crate::room::media::validate_transition(
        before,
        &doc,
        crate::room::media::TransitionMode::Admission,
    )
    .map_err(|_| ProofError::Unknown)?;
    let pinned = hex::encode(source);
    identities(authority, before, &doc)?;
    let receipts = effect_receipts(&doc)?;
    // StatePeer preserves omitted receipts separately. A standalone replay
    // proof has no such side table, so it must retain their exact visible values.
    if let Some(before) = before
        && effect_receipts(before)?
            .iter()
            .any(|(key, value)| receipts.get(key) != Some(value))
    {
        return Err(ProofError::Unknown);
    }
    if !crate::room::governance_change_allowed(before, &doc, false, Some(&pinned), &pinned)
        || !crate::room::immutable_receipts(before, &doc)
    {
        return Err(ProofError::Unknown);
    }
    crate::topology::validate(before, &doc, &pinned, &pinned).map_err(|_| ProofError::Unknown)?;
    crate::coverage::monotonic(before, &doc).map_err(|_| ProofError::Unknown)?;
    crate::durability::monotonic(before, &doc).map_err(|_| ProofError::Unknown)?;
    let coverage = crate::coverage::entries(&doc).map_err(|_| ProofError::Unknown)?;
    if !crate::coverage::canonical(&doc) {
        return Err(ProofError::Unknown);
    }
    if coverage
        .iter()
        .any(|(key, row)| !valid_coverage(authority, key, row))
    {
        return Err(ProofError::Unknown);
    }
    let durable = crate::durability::entries(&doc).map_err(|_| ProofError::Unknown)?;
    if !crate::durability::canonical(&doc) {
        return Err(ProofError::Unknown);
    }
    for (key, receipt) in durable {
        if coverage.get(&key) != Some(&receipt.coverage) {
            return Err(ProofError::Unknown);
        }
    }
    Ok(VerifiedRoom {
        authority: authority.clone(),
        doc,
        input_hash: *blake3::hash(body).as_bytes(),
        prior_heads: if kind == RoomInput::Delta {
            prior.map(VerifiedRoom::heads)
        } else {
            None
        },
    })
}

fn signed_document(
    source: [u8; 32],
    channel: [u8; 32],
    body: &[u8],
    room: &str,
) -> Result<
    (
        String,
        String,
        Box<collab_types::SignedOp>,
        DocumentMutation,
    ),
    ProofError,
> {
    let NeighbourMsg::AppDocument {
        peer,
        app_id,
        cluster_id,
        record_id,
        envelope,
    } = decode(body)?
    else {
        return Err(ProofError::Unknown);
    };
    if peer != crate::peer_handle(&source)
        || cluster_id != room
        || envelope.author.as_bytes() != &source
        || channel != *blake3::hash(format!("app\0{app_id}\0{record_id}").as_bytes()).as_bytes()
        || envelope.seq == 0
    {
        return Err(ProofError::Unknown);
    }
    collab_types::verify_op(&envelope).map_err(|_| ProofError::Unknown)?;
    let WholeDocumentPayload::Cleartext(mutation) =
        collab_types::validate_whole_document_payload_route(&app_id, room, &record_id, &envelope)
            .map_err(|_| ProofError::Unknown)?
    else {
        return Err(ProofError::Unknown);
    };
    Ok((app_id, record_id, envelope, mutation))
}

/// Verify a document snapshot against a verified room.
///
/// # Errors
///
/// Returns a [`ProofError`] if the snapshot is not covered by the room or fails
/// verification.
pub fn verify_snapshot(
    room: &VerifiedRoom,
    source: [u8; 32],
    channel: [u8; 32],
    body: &[u8],
) -> Result<VerifiedSnapshot, ProofError> {
    if source != room.authority.governor().device_public_key {
        return Err(ProofError::Unknown);
    }
    let (app, record, envelope, mutation) =
        signed_document(source, channel, body, room.authority.room())?;
    let DocumentMutation::Snapshot(bytes) = mutation else {
        return Err(ProofError::Unknown);
    };
    let doc = document(&bytes)?;
    if let Some(policy) = crate::room::media::parse(&room.doc).map_err(|_| ProofError::Unknown)? {
        crate::room::media::validate_bound_chat_change(&policy, &app, &record, &doc)
            .map_err(|_| ProofError::Unknown)?;
    }
    let key = crate::coverage::route_key(&app, &record);
    let coverage = crate::coverage::entries(&room.doc)
        .map_err(|_| ProofError::Unknown)?
        .remove(&key)
        .ok_or(ProofError::Unpaired)?;
    let receipt = crate::durability::entries(&room.doc)
        .map_err(|_| ProofError::Unknown)?
        .remove(&key)
        .ok_or(ProofError::Unpaired)?;
    if receipt.coverage != coverage
        || coverage.sequence != envelope.seq
        || coverage.payload_hash != *blake3::hash(&envelope.payload).as_bytes()
        || coverage.heads
            != doc
                .get_heads()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        || !valid_coverage(&room.authority, &key, &coverage)
    {
        return Err(ProofError::Unpaired);
    }
    Ok(VerifiedSnapshot {
        authority: room.authority.clone(),
        doc,
        receipt,
        input_hash: *blake3::hash(body).as_bytes(),
    })
}
impl VerifiedRoom {
    pub fn heads(&self) -> Vec<String> {
        self.doc
            .base_heads()
            .iter()
            .map(ToString::to_string)
            .collect()
    }
    pub fn input_hash(&self) -> [u8; 32] {
        self.input_hash
    }
    /// For a delta, identifies its required verified basis. The storage layer
    /// owns persistent frame references; this type never accumulates a history.
    pub fn prior_heads(&self) -> Option<&[String]> {
        self.prior_heads.as_deref()
    }
    /// Current canonical route metadata, not a historical proof cache.
    ///
    /// # Errors
    ///
    /// Returns a [`ProofError`] if the room coverage rows are malformed.
    pub fn coverages(&self) -> Result<BTreeMap<String, Coverage>, ProofError> {
        crate::coverage::entries(&self.doc).map_err(|_| ProofError::Unknown)
    }
    /// Causality only. A Covered room frame may still be an indispensable exact
    /// app prerequisite. Cross-channel selection must check its app pairs.
    pub fn covers_frame(
        &self,
        source: [u8; 32],
        channel: [u8; 32],
        body: &[u8],
    ) -> CoverageDecision {
        if source != self.authority.governor().device_public_key
            || channel != *blake3::hash(b"shell-tree").as_bytes()
        {
            return CoverageDecision::Unknown;
        }
        let Ok(NeighbourMsg::SyncResp { room, changes }) = decode(body) else {
            return CoverageDecision::Unknown;
        };
        if room != self.authority.room() || room_changes(&changes).is_err() {
            return CoverageDecision::Unknown;
        }
        for bytes in changes {
            let Ok(change) = automerge::Change::try_from(bytes.as_slice()) else {
                return CoverageDecision::Unknown;
            };
            if self.doc.get_change_by_hash(&change.hash()).is_none() {
                return CoverageDecision::NotCovered;
            }
        }
        CoverageDecision::Covered
    }
}
impl VerifiedSnapshot {
    /// Incoming-only non-publication proof against an already durable selected
    /// pair. This is NOT verification of a historical receipt or a GC grant.
    pub fn covers_incoming_snapshot(
        &self,
        source: [u8; 32],
        channel: [u8; 32],
        body: &[u8],
    ) -> bool {
        if source != self.authority.governor().device_public_key {
            return false;
        }
        let Ok((app, record, envelope, DocumentMutation::Snapshot(bytes))) =
            signed_document(source, channel, body, self.authority.room())
        else {
            return false;
        };
        let coverage = self.coverage();
        if app != coverage.app
            || record != coverage.record
            || envelope.seq > coverage.sequence
            || envelope.seq
                > coverage
                    .watermarks
                    .get(&hex::encode(source))
                    .copied()
                    .unwrap_or(0)
        {
            return false;
        }
        let Ok(prior) = document(&bytes) else {
            return false;
        };
        if envelope.seq == coverage.sequence
            && (coverage.payload_hash != *blake3::hash(&envelope.payload).as_bytes()
                || coverage.heads
                    != prior
                        .get_heads()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>())
        {
            return false;
        }
        prior
            .get_heads()
            .iter()
            .all(|head| self.doc.get_change_by_hash(head).is_some())
    }
    /// Replay readiness only: this individual signed increment can follow this
    /// snapshot without missing dependencies or an uncovered replay-floor loss.
    /// No live authorization, retirement, or ordering between tails is granted.
    pub fn tail_ready_after_snapshot(
        &self,
        source: [u8; 32],
        channel: [u8; 32],
        body: &[u8],
    ) -> bool {
        self.ordered_tail_readiness().append(source, channel, body)
    }
    pub fn ordered_tail_readiness(&self) -> OrderedTailReadiness {
        OrderedTailReadiness {
            authority: self.authority.clone(),
            coverage: self.receipt.coverage.clone(),
            snapshot: self.doc.fork(),
            cumulative: self.doc.fork(),
        }
    }
    pub fn coverage(&self) -> &Coverage {
        &self.receipt.coverage
    }
    pub fn receipt(&self) -> &DurableReceipt {
        &self.receipt
    }
    pub fn input_hash(&self) -> [u8; 32] {
        self.input_hash
    }
    pub fn covers_snapshot(&self, prior: &Self) -> bool {
        let (new, old) = (self.coverage(), prior.coverage());
        self.authority == prior.authority
            && new.app == old.app
            && new.record == old.record
            && self.receipt.generation >= prior.receipt.generation
            && (self.receipt.generation != prior.receipt.generation
                || self.receipt == prior.receipt)
            && new.sequence >= old.sequence
            && (new.sequence != old.sequence || new == old)
            && old
                .watermarks
                .iter()
                .all(|(author, seq)| new.watermarks.get(author).copied().unwrap_or(0) >= *seq)
            && prior
                .doc
                .get_heads()
                .iter()
                .all(|head| self.doc.get_change_by_hash(head).is_some())
    }
    /// Floor safety is distinct from retirement. Foreign signed increments are
    /// always preserved, but their floor interaction can still block a proposed
    /// basis. Safe means this floor cannot suppress an uncovered increment; it
    /// does not establish dependency ordering or authorize live admission.
    pub fn classify_tail(&self, source: [u8; 32], channel: [u8; 32], body: &[u8]) -> TailDecision {
        classify_tail(
            &self.authority,
            &self.receipt.coverage,
            &self.doc,
            source,
            channel,
            body,
        )
    }
}

impl OrderedTailReadiness {
    /// Append one signed route increment only when it is floor-safe against the
    /// original snapshot and dependency-complete against the accepted prefix.
    pub fn append(&mut self, source: [u8; 32], channel: [u8; 32], body: &[u8]) -> bool {
        let decision = classify_tail(
            &self.authority,
            &self.coverage,
            &self.snapshot,
            source,
            channel,
            body,
        );
        if decision.floor != FloorSafety::Safe {
            return false;
        }
        let Ok((app, record, _, DocumentMutation::Incremental(bytes))) =
            signed_document(source, channel, body, self.authority.room())
        else {
            return false;
        };
        if app != self.coverage.app || record != self.coverage.record || !strict_chunks(&bytes) {
            return false;
        }
        let mut next = self.cumulative.fork();
        if next.load_incremental(&bytes).is_err() || !next.get_missing_deps(&[]).is_empty() {
            return false;
        }
        self.cumulative = next;
        true
    }
}

fn classify_tail(
    authority: &RetentionAuthority,
    coverage: &Coverage,
    doc: &Automerge,
    source: [u8; 32],
    channel: [u8; 32],
    body: &[u8],
) -> TailDecision {
    let unknown = TailDecision {
        coverage: CoverageDecision::Unknown,
        floor: FloorSafety::Unknown,
    };
    let Ok((app, record, envelope, DocumentMutation::Incremental(bytes))) =
        signed_document(source, channel, body, authority.room())
    else {
        return unknown;
    };
    if app != coverage.app || record != coverage.record || !strict_chunks(&bytes) {
        return unknown;
    }
    let floor = coverage
        .watermarks
        .get(&hex::encode(source))
        .copied()
        .unwrap_or(0);
    let original_heads = doc.get_heads();
    let mut combined = doc.save();
    combined.extend_from_slice(&bytes);
    let contained = Automerge::load(&combined).is_ok_and(|combined| {
        combined.get_missing_deps(&[]).is_empty() && combined.get_heads() == original_heads
    });
    TailDecision {
        coverage: if source != authority.governor().device_public_key {
            CoverageDecision::Unknown
        } else if contained && floor >= envelope.seq {
            CoverageDecision::Covered
        } else {
            CoverageDecision::NotCovered
        },
        floor: if contained || envelope.seq > floor {
            FloorSafety::Safe
        } else {
            FloorSafety::Blocked
        },
    }
}

#[cfg(test)]
mod tests;
