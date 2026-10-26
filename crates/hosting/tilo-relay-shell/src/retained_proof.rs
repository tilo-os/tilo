//! Historical retention proofs over the frozen neighbour protocol.
//!
//! The caller supplies a locally pinned authority and an authenticated source;
//! an Envelope's claimed author or zero signature is not authentication. These
//! proofs neither admit live packets nor select/retire stored files. Historical
//! signature verification deliberately does not replace live freshness,
//! membership, revocation or capability checks in `StatePeer`.
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
use collab_types::{DocumentMutation, WholeDocumentPayload};
use neighbour_wire::NeighbourMsg;
use std::collections::BTreeMap;
use tilo_automerge_isolate::{Applied, Dependencies};
use tilo_peer_state::{coverage::Coverage, durability::DurableReceipt};

pub use tilo_relay_protocol::retention::RetentionAuthority;

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
    doc: AutoCommit,
    receipt: DurableReceipt,
    input_hash: [u8; 32],
}
/// Replay-readiness proof for one ordered route tail. The immutable snapshot
/// remains the sole floor and retirement authority; this cursor only advances
/// a separate causal document after each fully verified increment is ready.
pub struct OrderedTailReadiness {
    authority: RetentionAuthority,
    coverage: Coverage,
    snapshot: AutoCommit,
    cumulative: AutoCommit,
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
//
// These bodies are peer-supplied and retained by a relay. Reading their frames
// decodes nothing; every Automerge decode of them goes through
// `tilo_automerge_isolate`, so a hostile body costs a worker process, never the
// process running the proof.
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
/// The chunks of a non-empty, strictly framed input within the isolate's
/// size and chunk ceilings, each with its kind.
fn framed_chunks(mut bytes: &[u8]) -> Option<Vec<(&[u8], u8)>> {
    if bytes.is_empty() {
        return None;
    }
    tilo_automerge_isolate::precheck::check(
        bytes,
        tilo_automerge_isolate::Isolate::shared().limits(),
    )
    .ok()?;
    let mut chunks = Vec::new();
    while !bytes.is_empty() {
        let (chunk, rest, kind) = first_chunk(bytes)?;
        chunks.push((chunk, kind));
        bytes = rest;
    }
    Some(chunks)
}
/// Whether every chunk loads on its own, as a strict standalone load of it
/// would, decoded in the isolate: a change or bundle may lack dependencies
/// that live elsewhere, a document may not.
fn chunks_load_alone(chunks: &[(&[u8], u8)]) -> bool {
    chunks.iter().all(|&(chunk, kind)| {
        match tilo_automerge_isolate::apply(
            &mut AutoCommit::new(),
            chunk,
            Dependencies::RequireComplete,
        ) {
            Ok(Applied::Changed(_)) => true,
            Ok(Applied::MissingDependencies) => kind != 0,
            Err(_) => false,
        }
    })
}
fn document(bytes: &[u8]) -> Result<AutoCommit, ProofError> {
    let chunks = framed_chunks(bytes).ok_or(ProofError::Unknown)?;
    // One chunk that passes the strict whole load below also loads alone.
    if chunks.len() > 1 && !chunks_load_alone(&chunks) {
        return Err(ProofError::Unknown);
    }
    let doc = tilo_automerge_isolate::load(bytes).map_err(|_| ProofError::Unknown)?;
    if !doc.get_missing_deps(&[]).is_empty() {
        return Err(ProofError::Unknown);
    }
    Ok(doc)
}
/// A room frame's framing: each entry exactly one change chunk. Nothing is
/// decoded here; `tilo_peer_state::room::candidate` decodes them in the isolate.
fn room_framing(changes: &[Vec<u8>]) -> Result<(), ProofError> {
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
    }
    Ok(())
}
/// The changes of a room frame that `tilo_peer_state::room::candidate` has already
/// accepted, parsed in this process only now: the isolate's worker decoded
/// each of these chunks the same way first.
///
/// Parsing a change does not check its stored checksum, and an incremental
/// load can drop a known change whose checksum is wrong without refusing the
/// input, so each chunk's stored checksum is compared with its hash here. For
/// a compressed chunk that hash is the uncompressed change's, which its stored
/// checksum must match as well.
fn admitted_changes(changes: &[Vec<u8>]) -> Result<Vec<automerge::Change>, ProofError> {
    changes
        .iter()
        .map(|bytes| {
            let change =
                automerge::Change::try_from(bytes.as_slice()).map_err(|_| ProofError::Unknown)?;
            if bytes.get(4..8) != change.hash().0.get(..4) {
                return Err(ProofError::Unknown);
            }
            Ok(change)
        })
        .collect()
}
fn valid_coverage(authority: &RetentionAuthority, key: &str, coverage: &Coverage) -> bool {
    coverage.room == authority.room()
        && key == tilo_peer_state::coverage::route_key(&coverage.app, &coverage.record)
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
        tilo_peer_state::room::governance_of(doc)
            .identities
            .into_iter()
            .map(|(id, bytes)| {
                let identity =
                    tilo_peer_state::room::identity_row(&bytes).map_err(|_| ProofError::Unknown)?;
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
    room_framing(&changes)?;
    let base = match kind {
        RoomInput::Checkpoint => None,
        RoomInput::Delta => Some(&prior.ok_or(ProofError::Unpaired)?.doc),
    };
    // The isolate decodes the changes first; only then are they parsed here.
    let doc = tilo_peer_state::room::candidate(base, &changes).map_err(|_| ProofError::Unknown)?;
    admitted_changes(&changes)?;
    if !doc.get_missing_deps(&[]).is_empty() {
        return Err(ProofError::Unknown);
    }
    match doc.get(ROOT, "room").map_err(|_| ProofError::Unknown)? {
        Some((Value::Scalar(value), _)) if matches!(value.as_ref(), ScalarValue::Str(value) if value.as_str() == authority.room()) =>
            {}
        _ => return Err(ProofError::Unknown),
    }
    let before = prior.map(|prior| &prior.doc);
    let pinned = hex::encode(source);
    // The governor wrote it, so this is the room admission every receiver
    // runs, from the governor: room, topology, receipts, coverage,
    // durability and media.
    tilo_peer_state::room::check_candidate(
        &tilo_peer_state::room::RoomAdmission {
            before,
            room: authority.room(),
            source: tilo_peer_state::room::Writer {
                key: &pinned,
                standing: tilo_peer_state::room::Standing::TransportAuthorized,
            },
            authority: tilo_peer_state::room::AuthorityPin::Device(&pinned),
            contents: tilo_peer_state::room::ContentOwnership::Registry,
            now_ns: 0,
        },
        &doc,
    )
    .map_err(|_| ProofError::Unknown)?;
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
    let coverage = tilo_peer_state::coverage::entries(&doc).map_err(|_| ProofError::Unknown)?;
    if !tilo_peer_state::coverage::canonical(&doc) {
        return Err(ProofError::Unknown);
    }
    if coverage
        .iter()
        .any(|(key, row)| !valid_coverage(authority, key, row))
    {
        return Err(ProofError::Unknown);
    }
    let durable = tilo_peer_state::durability::entries(&doc).map_err(|_| ProofError::Unknown)?;
    if !tilo_peer_state::durability::canonical(&doc) {
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
    if peer != tilo_peer_state::peer_handle(&source)
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
    let mut doc = document(&bytes)?;
    if let Some(policy) =
        tilo_peer_state::room::media::parse(&room.doc).map_err(|_| ProofError::Unknown)?
    {
        tilo_peer_state::room::media::validate_bound_chat_change(&policy, &app, &record, &doc)
            .map_err(|_| ProofError::Unknown)?;
    }
    let key = tilo_peer_state::coverage::route_key(&app, &record);
    let coverage = tilo_peer_state::coverage::entries(&room.doc)
        .map_err(|_| ProofError::Unknown)?
        .remove(&key)
        .ok_or(ProofError::Unpaired)?;
    let receipt = tilo_peer_state::durability::entries(&room.doc)
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
    pub fn coverages(&self) -> Result<BTreeMap<String, Coverage>, ProofError> {
        tilo_peer_state::coverage::entries(&self.doc).map_err(|_| ProofError::Unknown)
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
        if room != self.authority.room() || room_framing(&changes).is_err() {
            return CoverageDecision::Unknown;
        }
        // Decode the frame in the isolate, on its own, before parsing it here.
        if tilo_peer_state::room::candidate(None, &changes).is_err() {
            return CoverageDecision::Unknown;
        }
        let Ok(changes) = admitted_changes(&changes) else {
            return CoverageDecision::Unknown;
        };
        for change in changes {
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
        let Ok(mut prior) = document(&bytes) else {
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
            snapshot: self.doc.clone(),
            cumulative: self.doc.clone(),
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
                .committed_heads()
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
    /// [`VerifiedSnapshot::classify_tail`] against this cursor's immutable
    /// snapshot, so a holder of the cursor alone keeps the same floor rules.
    pub fn classify_tail(&self, source: [u8; 32], channel: [u8; 32], body: &[u8]) -> TailDecision {
        classify_tail(
            &self.authority,
            &self.coverage,
            &self.snapshot,
            source,
            channel,
            body,
        )
    }
    /// Append one signed route increment only when it is floor-safe against the
    /// original snapshot and dependency-complete against the accepted prefix.
    pub fn append(&mut self, source: [u8; 32], channel: [u8; 32], body: &[u8]) -> bool {
        let Some(increment) =
            route_increment(&self.authority, &self.coverage, source, channel, body)
        else {
            return false;
        };
        if tail_decision(&self.authority, &self.coverage, &self.snapshot, &increment).floor
            != FloorSafety::Safe
        {
            return false;
        }
        let mut next = self.cumulative.clone();
        if !matches!(
            tilo_automerge_isolate::apply(
                &mut next,
                &increment.bytes,
                Dependencies::RequireComplete
            ),
            Ok(Applied::Changed(_))
        ) {
            return false;
        }
        self.cumulative = next;
        true
    }
}

/// One signed increment on a snapshot's route whose every chunk loads on its own.
struct Increment {
    source: [u8; 32],
    seq: u64,
    bytes: Vec<u8>,
}

fn route_increment(
    authority: &RetentionAuthority,
    coverage: &Coverage,
    source: [u8; 32],
    channel: [u8; 32],
    body: &[u8],
) -> Option<Increment> {
    let Ok((app, record, envelope, DocumentMutation::Incremental(bytes))) =
        signed_document(source, channel, body, authority.room())
    else {
        return None;
    };
    if app != coverage.app || record != coverage.record {
        return None;
    }
    chunks_load_alone(&framed_chunks(&bytes)?).then_some(Increment {
        source,
        seq: envelope.seq,
        bytes,
    })
}

fn classify_tail(
    authority: &RetentionAuthority,
    coverage: &Coverage,
    doc: &AutoCommit,
    source: [u8; 32],
    channel: [u8; 32],
    body: &[u8],
) -> TailDecision {
    match route_increment(authority, coverage, source, channel, body) {
        Some(increment) => tail_decision(authority, coverage, doc, &increment),
        None => TailDecision {
            coverage: CoverageDecision::Unknown,
            floor: FloorSafety::Unknown,
        },
    }
}

fn tail_decision(
    authority: &RetentionAuthority,
    coverage: &Coverage,
    doc: &AutoCommit,
    increment: &Increment,
) -> TailDecision {
    let floor = coverage
        .watermarks
        .get(&hex::encode(increment.source))
        .copied()
        .unwrap_or(0);
    // Contained: applied to a copy of the snapshot, the increment leaves its
    // heads where they were and nothing waiting for a dependency.
    let mut probe = doc.clone();
    let contained = matches!(
        tilo_automerge_isolate::apply(&mut probe, &increment.bytes, Dependencies::RequireComplete),
        Ok(Applied::Changed(false))
    );
    TailDecision {
        coverage: if increment.source != authority.governor().device_public_key {
            CoverageDecision::Unknown
        } else if contained && floor >= increment.seq {
            CoverageDecision::Covered
        } else {
            CoverageDecision::NotCovered
        },
        floor: if contained || increment.seq > floor {
            FloorSafety::Safe
        } else {
            FloorSafety::Blocked
        },
    }
}

#[cfg(test)]
mod media_tests;
#[cfg(test)]
mod tests;
