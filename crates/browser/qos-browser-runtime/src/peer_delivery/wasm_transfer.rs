//! IDB staging transitions owned by the current Rust delivery session.
use super::*;
use crate::peer_delivery::resume::Staged;
use crate::peer_delivery::{StreamDecoder, transfer::PinnedTransfer};
use state::{SnapshotChunk, SnapshotOperation, SnapshotRequest};

#[derive(Default)]
pub(super) struct ResumeState {
    scanned: bool,
    candidates: [Option<Staged>; 2],
    selected: Option<Staged>,
    discarding: bool,
}
impl ResumeState {
    pub(super) fn scanned(&self) -> bool {
        self.scanned
    }
}

pub(super) struct TransferState {
    pin: PinnedTransfer,
    id: String,
    phase: Phase,
    read_offset: u64,
    decoder: Option<StreamDecoder>,
    admission: Option<CutAdmission>,
    basis: Option<Basis>,
    retries: u8,
    stored_resume: [u8; 32],
    original_stage: Staged,
    renewal_index: Option<u32>,
    _manifest_credit: automerge::resource_limits::NativeReservation,
}
enum Phase {
    Opening,
    Renewing(u32),
    Downloading,
    Writing(u64),
    Reading,
    WaitingCommit,
}
struct Basis {
    revision: u64,
    generation: u64,
    room: String,
    lease: String,
}
impl Basis {
    fn capture(peer: &Peer) -> Result<Self, JsValue> {
        Ok(Self {
            revision: peer.state_revision.get(),
            generation: peer.transport_generation.get(),
            room: peer.state.borrow().room().into(),
            lease: peer
                .batches
                .borrow()
                .lease_epoch()
                .ok_or_else(|| error("storage lease absent"))?
                .into(),
        })
    }
    fn current(&self, peer: &Peer) -> bool {
        !peer.closed.get()
            && self.revision == peer.state_revision.get()
            && self.generation == peer.transport_generation.get()
            && self.room == peer.state.borrow().room()
            && peer.batches.borrow().lease_epoch() == Some(self.lease.as_str())
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn now() -> u64 {
    tilo_relay::clock::now_ns()
}
fn stage_identity(value: &TransferState, id: &str) -> Result<(), JsValue> {
    if value.id != id {
        return Err(error("foreign staged transfer handle"));
    }
    Ok(())
}
/// A fully read and admitted cut whose storage transaction is still pending.
pub(super) fn waiting_commit(transfer: &TransferState) -> bool {
    matches!(transfer.phase, Phase::WaitingCommit)
}
/// The admitted cut changed nothing and its cursor is already durable, so no
/// transaction can consume the stage. Drop it through the ordinary proven
/// discard receipt instead of leaving it for another resume.
pub(super) fn discard_committed(delivery: &mut DeliveryState) -> Result<String, JsValue> {
    let transfer = delivery
        .transfer
        .take()
        .ok_or_else(|| error("committed transfer missing"))?;
    if !matches!(transfer.phase, Phase::WaitingCommit) {
        return Err(error("transfer not awaiting its commit"));
    }
    delivery.needs_snapshot = false;
    delivery.recovery.selected = Some(transfer.original_stage.clone());
    delivery.recovery.discarding = true;
    Ok(
        serde_json::json!({"kind":"stage-discard","transfer":descriptor(&transfer.original_stage)})
            .to_string(),
    )
}
pub(super) fn begin(peer: &Peer) -> Result<String, JsValue> {
    if peer.delivery.borrow().pending.values().any(|p| {
        matches!(
            p,
            Pending::Snapshot
                | Pending::SnapshotResume
                | Pending::SnapshotRenew(_)
                | Pending::StageList
                | Pending::SnapshotChunk(_)
        )
    }) {
        return Ok("{\"kind\":\"idle\"}".into());
    }
    let mut delivery = peer.delivery.borrow_mut();
    if delivery.recovery.selected.is_some() {
        return Ok("{\"kind\":\"idle\"}".into());
    }
    if !delivery.recovery.scanned {
        delivery.recovery.scanned = true;
        let id = delivery
            .retain(Pending::StageList, &peer.incoming)
            .map_err(error)?;
        return Ok(serde_json::json!({"kind":"stage-list","id":id}).to_string());
    }
    drop(delivery);
    next_resume(peer)
}
fn scope(peer: &Peer) -> Result<[u8; 32], JsValue> {
    Ok(state::canonical_item_digest(
        &postcard::to_allocvec(&identity(peer)).map_err(error)?,
    ))
}
fn descriptor(value: &Staged) -> serde_json::Value {
    serde_json::json!({"id":hex(&value.pin),"scope":hex(&value.scope),"cutToken":hex(&value.cut),"totalBytes":value.total,"maxItemBytes":value.max_item})
}
#[wasm_bindgen]
pub fn peer_delivery_stage_list(
    peer_id: u32,
    operation: u32,
    raw: String,
) -> Result<String, JsValue> {
    let peer = owner(peer_id)?;
    check(&peer)?;
    if !matches!(
        peer.delivery.borrow_mut().pending.remove(&operation),
        Some(Pending::StageList)
    ) {
        return Err(error("unexpected stage discovery receipt"));
    }
    let values = crate::peer_delivery::resume::parse(&raw).map_err(error)?;
    peer.delivery.borrow_mut().recovery.candidates = values;
    next_resume(&peer)
}
fn next_resume(peer: &Peer) -> Result<String, JsValue> {
    check(peer)?;
    let expected = scope(peer)?;
    let mut delivery = peer.delivery.borrow_mut();
    let candidate = delivery
        .recovery
        .candidates
        .iter_mut()
        .filter_map(Option::take)
        .find(|value| value.scope == expected);
    if let Some(value) = candidate {
        let body = framed(
            MessageType::SnapshotManifest,
            &SnapshotRequest::new(SnapshotOperation::Renew {
                pin: value.pin,
                manifest_digest: value.resume,
            })
            .map_err(error)?
            .encode()
            .map_err(error)?,
        )
        .map_err(error)?;
        delivery.recovery.selected = Some(value);
        drop(delivery);
        return request(
            peer,
            "POST",
            "/peer/v3/snapshots".into(),
            body,
            Pending::SnapshotResume,
        );
    }
    if !delivery.needs_snapshot {
        return Ok("{\"kind\":\"idle\"}".into());
    }
    drop(delivery);
    let body = framed(
        MessageType::SnapshotManifest,
        &SnapshotRequest::new(SnapshotOperation::Baseline)
            .map_err(error)?
            .encode()
            .map_err(error)?,
    )
    .map_err(error)?;
    request(
        peer,
        "POST",
        "/peer/v3/snapshots".into(),
        body,
        Pending::Snapshot,
    )
}
pub(super) fn resumed(peer: &Peer, bytes: &[u8]) -> Result<String, JsValue> {
    check(peer)?;
    let (kind, payload, tail) = framing::decode_frame_prefix(bytes).map_err(error)?;
    if !tail.is_empty() {
        return Err(error("extended snapshot renewal response"));
    }
    let original = peer
        .delivery
        .borrow()
        .recovery
        .selected
        .clone()
        .ok_or_else(|| error("snapshot renewal custody missing"))?;
    match kind {
        MessageType::SnapshotManifest => {
            let result = manifest_inner(peer, payload, Some(&original))?;
            peer.delivery.borrow_mut().recovery.selected = None;
            Ok(result)
        }
        MessageType::SnapshotRequired => {
            crate::peer_delivery::resume::decode_reset(payload).map_err(error)?;
            let mut delivery = peer.delivery.borrow_mut();
            delivery.recovery.discarding = true;
            delivery.needs_snapshot = true;
            Ok(
                serde_json::json!({"kind":"stage-discard","transfer":descriptor(&original)})
                    .to_string(),
            )
        }
        _ => Err(error("unexpected snapshot renewal reply")),
    }
}
#[wasm_bindgen]
pub fn peer_delivery_stage_discarded(peer_id: u32, id: String) -> Result<String, JsValue> {
    let peer = owner(peer_id)?;
    check(&peer)?;
    let mut delivery = peer.delivery.borrow_mut();
    if !delivery.recovery.discarding
        || !delivery
            .recovery
            .selected
            .as_ref()
            .is_some_and(|value| hex(&value.pin) == id)
    {
        return Err(error("unproven stage discard"));
    }
    delivery.recovery.discarding = false;
    delivery.recovery.selected = None;
    drop(delivery);
    next_resume(&peer)
}
pub(super) fn manifest(peer: &Peer, bytes: &[u8]) -> Result<String, JsValue> {
    manifest_inner(peer, bytes, None)
}
fn manifest_inner(peer: &Peer, bytes: &[u8], original: Option<&Staged>) -> Result<String, JsValue> {
    check(peer)?;
    if peer.delivery.borrow().transfer.is_some() {
        return Err(error("overlapping pinned transfer"));
    }
    let manifest_credit = peer.incoming.reserve(128 * 1024).map_err(error)?;
    let pin = PinnedTransfer::open(
        identity(peer),
        peer.transport_generation.get(),
        bytes,
        peer.delivery.borrow().cursor.as_ref(),
        now(),
    )
    .map_err(error)?;
    let id = hex(&pin.issue().pin);
    let fresh = Staged {
        pin: pin.issue().pin,
        scope: scope(peer)?,
        cut: pin.digest(),
        resume: pin.digest(),
        total: pin.issue().stream_bytes,
        max_item: pin.issue().stream_bytes.min(64 * 1024 * 1024),
    };
    if original.is_some_and(|old| {
        old.pin != fresh.pin
            || old.scope != fresh.scope
            || old.total != fresh.total
            || old.max_item != fresh.max_item
    }) {
        return Err(error(
            "renewed manifest differs from staged scope or layout",
        ));
    }
    let staged = original.unwrap_or(&fresh);
    let value = serde_json::json!({"kind":"stage-open","transfer":descriptor(staged)}).to_string();
    let stored_resume = staged.resume;
    peer.delivery.borrow_mut().transfer = Some(TransferState {
        pin,
        id,
        phase: Phase::Opening,
        read_offset: 0,
        decoder: None,
        admission: None,
        basis: None,
        retries: 0,
        stored_resume,
        original_stage: staged.clone(),
        renewal_index: None,
        _manifest_credit: manifest_credit,
    });
    Ok(value)
}
/// Re-queue a snapshot request whose HTTP attempt failed transiently, at the
/// exact step it was issued from. Nothing it would have advanced has changed.
pub(super) fn requeue(delivery: &mut DeliveryState, pending: &Pending) -> bool {
    match pending {
        // `begin` re-issues the baseline while `needs_snapshot` stays set.
        Pending::Snapshot => delivery.needs_snapshot && delivery.recovery.selected.is_none(),
        // Return the selected staged candidate for another renewal attempt.
        Pending::SnapshotResume => {
            if delivery.recovery.discarding {
                return false;
            }
            let Some(slot) = delivery
                .recovery
                .candidates
                .iter()
                .position(Option::is_none)
            else {
                return false;
            };
            match delivery.recovery.selected.take() {
                Some(value) => {
                    delivery.recovery.candidates[slot] = Some(value);
                    true
                }
                None => false,
            }
        }
        Pending::SnapshotChunk(index) => match &delivery.transfer {
            Some(transfer) if matches!(transfer.phase, Phase::Downloading) => {
                delivery.retry_chunk = Some(*index);
                true
            }
            _ => false,
        },
        Pending::SnapshotRenew(index) => match delivery.transfer.as_mut() {
            Some(transfer)
                if matches!(transfer.phase, Phase::Downloading)
                    && transfer.renewal_index == Some(*index) =>
            {
                // `fetch_chunk` renews again when the pin is still near expiry.
                transfer.renewal_index = None;
                delivery.retry_chunk = Some(*index);
                true
            }
            _ => false,
        },
        _ => false,
    }
}
/// Scheduler turn for a re-queued chunk: fresh proof, pin checked again.
pub(super) fn retry_chunk(peer: &Peer, index: u32) -> Result<String, JsValue> {
    check(peer)?;
    match peer.delivery.borrow().transfer.as_ref() {
        Some(transfer) if matches!(transfer.phase, Phase::Downloading) => {}
        _ => return Ok("{\"kind\":\"idle\"}".into()),
    }
    fetch_chunk(peer, index)
}
fn fetch_chunk(peer: &Peer, index: u32) -> Result<String, JsValue> {
    let mut state = peer.delivery.borrow_mut();
    let transfer = state
        .transfer
        .as_mut()
        .ok_or_else(|| error("transfer owner missing"))?;
    transfer
        .pin
        .current(peer.transport_generation.get(), now())
        .map_err(error)?;
    if index > 0
        && transfer.renewal_index != Some(index)
        && transfer.pin.issue().expires_at_ns.saturating_sub(now()) <= 5_000_000_000
    {
        transfer.renewal_index = Some(index);
        let body = framed(
            MessageType::SnapshotManifest,
            &SnapshotRequest::new(SnapshotOperation::Renew {
                pin: transfer.pin.issue().pin,
                manifest_digest: transfer.pin.digest(),
            })
            .map_err(error)?
            .encode()
            .map_err(error)?,
        )
        .map_err(error)?;
        drop(state);
        return request(
            peer,
            "POST",
            "/peer/v3/snapshots".into(),
            body,
            Pending::SnapshotRenew(index),
        );
    }
    let target = format!("/peer/v3/snapshots/{}/chunks/{index}", transfer.id);
    drop(state);
    request(peer, "GET", target, vec![], Pending::SnapshotChunk(index))
}
pub(super) fn renewed(peer: &Peer, index: u32, bytes: &[u8]) -> Result<String, JsValue> {
    check(peer)?;
    let (kind, payload, tail) = framing::decode_frame_prefix(bytes).map_err(error)?;
    if !tail.is_empty() {
        return Err(error("extended live transfer renewal response"));
    }
    let mut delivery = peer.delivery.borrow_mut();
    let transfer = delivery
        .transfer
        .as_mut()
        .ok_or_else(|| error("transfer missing"))?;
    if !matches!(transfer.phase, Phase::Downloading) || transfer.renewal_index != Some(index) {
        return Err(error("stale live renewal"));
    }
    if kind == MessageType::SnapshotRequired {
        crate::peer_delivery::resume::decode_reset(payload).map_err(error)?;
        // The current correlated request authenticates this reset. Retain the
        // original IDB cleanup identity, not the latest renewed manifest digest.
        let original = transfer.original_stage.clone();
        delivery.transfer = None;
        delivery.recovery.selected = Some(original.clone());
        delivery.recovery.discarding = true;
        delivery.needs_snapshot = true;
        return Ok(
            serde_json::json!({"kind":"stage-discard","transfer":descriptor(&original)})
                .to_string(),
        );
    }
    if kind != MessageType::SnapshotManifest {
        return Err(error("live transfer renewal lacks exact current manifest"));
    }
    transfer
        .pin
        .renew(peer.transport_generation.get(), payload, now())
        .map_err(error)?;
    if transfer.stored_resume == transfer.pin.digest() {
        drop(delivery);
        return fetch_chunk(peer, index);
    }
    transfer.phase = Phase::Renewing(index);
    Ok(serde_json::json!({"kind":"stage-renewed","transferId":transfer.id,"expectedDigest":hex(&transfer.stored_resume),"resumeDigest":hex(&transfer.pin.digest())}).to_string())
}
#[wasm_bindgen]
pub fn peer_delivery_stage_opened(
    peer_id: u32,
    id: String,
    next_offset: f64,
    resume_digest: String,
) -> Result<String, JsValue> {
    let peer = owner(peer_id)?;
    check(&peer)?;
    let mut state = peer.delivery.borrow_mut();
    let transfer = state
        .transfer
        .as_mut()
        .ok_or_else(|| error("transfer missing"))?;
    stage_identity(transfer, &id)?;
    if resume_digest != hex(&transfer.stored_resume) {
        return Err(error("staged resume digest changed before reopen"));
    }
    if !matches!(transfer.phase, Phase::Opening)
        || !next_offset.is_finite()
        || next_offset < 0.0
        || next_offset.fract() != 0.0
        || next_offset > transfer.pin.issue().stream_bytes as f64
    {
        return Err(error("invalid staged reopen offset"));
    }
    let offset = next_offset as u64;
    let width = u64::from(transfer.pin.issue().chunk_size);
    let total = transfer.pin.issue().stream_bytes;
    if offset != total && offset % width != 0 {
        return Err(error("unaligned durable stage progress"));
    }
    // Re-fetch the last chunk to recover its exact signed boundary after a
    // previous interrupted write. Stored bytes are rechecked during every read.
    let index = if offset == total {
        u32::try_from((total - 1) / width).map_err(error)?
    } else {
        u32::try_from(offset / width).map_err(error)?
    };
    if transfer.stored_resume != transfer.pin.digest() {
        transfer.phase = Phase::Renewing(index);
        return Ok(serde_json::json!({"kind":"stage-renewed","transferId":id,"expectedDigest":hex(&transfer.stored_resume),"resumeDigest":hex(&transfer.pin.digest())}).to_string());
    }
    transfer.phase = Phase::Downloading;
    drop(state);
    fetch_chunk(&peer, index)
}
#[wasm_bindgen]
pub fn peer_delivery_stage_renewed(
    peer_id: u32,
    id: String,
    digest: String,
) -> Result<String, JsValue> {
    let peer = owner(peer_id)?;
    check(&peer)?;
    let mut delivery = peer.delivery.borrow_mut();
    let transfer = delivery
        .transfer
        .as_mut()
        .ok_or_else(|| error("transfer missing"))?;
    stage_identity(transfer, &id)?;
    transfer
        .pin
        .current(peer.transport_generation.get(), now())
        .map_err(error)?;
    let Phase::Renewing(index) = transfer.phase else {
        return Err(error("unexpected stage renewal receipt"));
    };
    if digest != hex(&transfer.pin.digest()) {
        return Err(error("stage CAS receipt differs from verified renewal"));
    }
    transfer.stored_resume = transfer.pin.digest();
    transfer.phase = Phase::Downloading;
    drop(delivery);
    fetch_chunk(&peer, index)
}
pub(super) fn chunk(peer: &Peer, index: u32, bytes: &[u8]) -> Result<String, JsValue> {
    check(peer)?;
    let (kind, payload, rest) = framing::decode_frame_prefix(bytes).map_err(error)?;
    if kind != MessageType::SnapshotChunk {
        return Err(error("snapshot chunk frame missing"));
    }
    let chunk = SnapshotChunk::decode(payload).map_err(error)?;
    let mut state = peer.delivery.borrow_mut();
    let transfer = state
        .transfer
        .as_mut()
        .ok_or_else(|| error("transfer missing"))?;
    if !matches!(transfer.phase, Phase::Downloading) || chunk.index() != index {
        return Err(error("stale snapshot chunk"));
    }
    transfer
        .pin
        .validate_chunk(peer.transport_generation.get(), &chunk, now())
        .map_err(error)?;
    let offset = u64::from(index) * u64::from(transfer.pin.issue().chunk_size);
    let end = offset + chunk.bytes().len() as u64;
    if end == transfer.pin.issue().stream_bytes {
        let (kind, payload, tail) = framing::decode_frame_prefix(rest).map_err(error)?;
        if kind != MessageType::CommitBoundary || !tail.is_empty() {
            return Err(error("final snapshot boundary missing or extended"));
        }
        transfer
            .pin
            .set_boundary(
                peer.transport_generation.get(),
                state::CommitBoundary::decode(payload).map_err(error)?,
                now(),
            )
            .map_err(error)?;
    } else if !rest.is_empty() {
        return Err(error("early snapshot boundary or extra frame"));
    }
    transfer.phase = Phase::Writing(end);
    let id = transfer.id.clone();
    let operation = state
        .retain(Pending::StageWrite, &peer.incoming)
        .map_err(error)?;
    Ok(serde_json::json!({"kind":"stage-write","id":operation,"transferId":id,"offset":offset,"bytes":chunk.bytes()}).to_string())
}
#[wasm_bindgen]
pub fn peer_delivery_stage_written(
    peer_id: u32,
    id: String,
    next_offset: f64,
) -> Result<String, JsValue> {
    let peer = owner(peer_id)?;
    check(&peer)?;
    let mut state = peer.delivery.borrow_mut();
    let transfer = state
        .transfer
        .as_mut()
        .ok_or_else(|| error("transfer missing"))?;
    stage_identity(transfer, &id)?;
    let Phase::Writing(expected) = transfer.phase else {
        return Err(error("unexpected staged write receipt"));
    };
    transfer
        .pin
        .current(peer.transport_generation.get(), now())
        .map_err(error)?;
    if next_offset != expected as f64 {
        return Err(error("durable stage offset differs from verified chunk"));
    }
    if expected < transfer.pin.issue().stream_bytes {
        transfer.phase = Phase::Downloading;
        let index =
            u32::try_from(expected / u64::from(transfer.pin.issue().chunk_size)).map_err(error)?;
        drop(state);
        return fetch_chunk(&peer, index);
    }
    drop(state);
    begin_read(&peer)
}
fn begin_read(peer: &Rc<Peer>) -> Result<String, JsValue> {
    crate::browser::prepare_peer_remote(peer.id).map_err(error)?;
    check(peer)?;
    let basis = Basis::capture(peer)?;
    let mut state = peer.delivery.borrow_mut();
    let transfer = state
        .transfer
        .as_mut()
        .ok_or_else(|| error("transfer missing"))?;
    transfer
        .pin
        .current(peer.transport_generation.get(), now())
        .map_err(error)?;
    transfer.decoder = Some(
        StreamDecoder::with_incoming(
            transfer.pin.issue().item_count,
            transfer.pin.issue().stream_bytes,
            64 * 1024 * 1024,
            peer.incoming.clone(),
        )
        .map_err(error)?,
    );
    // The candidate starts from the fenced basis; any live change restarts it.
    transfer.admission = Some(
        CutAdmission::begin(
            &peer.state.borrow(),
            peer.incoming.clone(),
            SystemTime::now(),
        )
        .map_err(error)?,
    );
    transfer.basis = Some(basis);
    transfer.read_offset = 0;
    transfer.phase = Phase::Reading;
    drop(state);
    read_step(peer)
}
fn read_step(peer: &Peer) -> Result<String, JsValue> {
    let mut state = peer.delivery.borrow_mut();
    let transfer = state
        .transfer
        .as_mut()
        .ok_or_else(|| error("transfer missing"))?;
    let length = (transfer.pin.issue().stream_bytes - transfer.read_offset)
        .min(u64::from(transfer.pin.issue().chunk_size));
    if length == 0 {
        return Err(error("staged decoder completion missing"));
    }
    // A storage read carries the same raw-overlap reservation as a fetch. The
    // executor releases it only after its raw async read and Rust call settle.
    let id = transfer.id.clone();
    let offset = transfer.read_offset;
    let operation = state.retain(Pending::StageRead, &peer.incoming)?;
    Ok(serde_json::json!({"kind":"stage-read","id":operation,"transferId":id,"offset":offset,"length":length}).to_string())
}
#[wasm_bindgen]
pub fn peer_delivery_stage_read(
    peer_id: u32,
    id: String,
    offset: f64,
    bytes: js_sys::Uint8Array,
) -> Result<String, JsValue> {
    let peer = owner(peer_id)?;
    check(&peer)?;
    if bytes.length() > 240 * 1024 {
        return Err(error("staged read exceeds fixed chunk"));
    }
    let mut transfer = peer
        .delivery
        .borrow_mut()
        .transfer
        .take()
        .ok_or_else(|| error("transfer missing"))?;
    enum Step {
        Continue,
        Restart,
        Install(StatePeer, StoredDeliveryCursor),
    }
    let result = (|| {
        stage_identity(&transfer, &id)?;
        transfer
            .pin
            .current(peer.transport_generation.get(), now())
            .map_err(error)?;
        if !matches!(transfer.phase, Phase::Reading) || offset != transfer.read_offset as f64 {
            return Err(error("stale staged read"));
        }
        if !transfer
            .basis
            .as_ref()
            .is_some_and(|basis| basis.current(&peer))
        {
            transfer.retries = transfer
                .retries
                .checked_add(1)
                .ok_or_else(|| error("candidate retry counter exhausted"))?;
            if transfer.retries > 3 {
                return Err(error(
                    "canonical state changed repeatedly during staged admission; retain staged cut and retry fairly",
                ));
            }
            transfer.decoder = None;
            transfer.admission = None;
            transfer.basis = None;
            return Ok(Step::Restart);
        }
        let bytes = bytes.to_vec();
        transfer
            .pin
            .validate_staged(
                peer.transport_generation.get(),
                transfer.read_offset,
                &bytes,
                now(),
            )
            .map_err(error)?;
        let TransferState {
            decoder,
            admission: staged,
            ..
        } = &mut transfer;
        let cut = staged
            .as_mut()
            .ok_or_else(|| error("staged admission absent"))?;
        let admitted_at = SystemTime::now();
        decoder
            .as_mut()
            .ok_or_else(|| error("staged decoder absent"))?
            .push(&bytes, |event| cut.push(event, admitted_at))
            .map_err(error)?;
        transfer.read_offset += bytes.len() as u64;
        if transfer.read_offset == transfer.pin.issue().stream_bytes {
            let order = transfer
                .decoder
                .take()
                .ok_or_else(|| error("staged decoder missing"))?
                .finish()
                .map_err(error)?;
            let cursor = transfer
                .pin
                .finish(peer.transport_generation.get(), order, now())
                .map_err(error)?;
            let candidate = transfer
                .admission
                .take()
                .ok_or_else(|| error("staged admission missing"))?
                .finish_complete(SystemTime::now())
                .map_err(error)?;
            // A baseline may reset the scope; within one scope it never regresses.
            admission::cursor_step(peer.delivery.borrow().cursor.as_ref(), &cursor, true)
                .map_err(error)?;
            transfer.phase = Phase::WaitingCommit;
            return Ok(Step::Install(candidate, cursor));
        }
        Ok(Step::Continue)
    })();
    peer.delivery.borrow_mut().transfer = Some(transfer);
    match result? {
        Step::Restart => begin_read(&peer),
        Step::Continue => read_step(&peer),
        // The same transaction commits state, journal, cursor and consumes the stage.
        Step::Install(candidate, cursor) => install_admitted(&peer, candidate, cursor, Some(&id)),
    }
}
