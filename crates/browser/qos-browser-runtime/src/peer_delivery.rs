//! Private bounded data transport preparation. Wire validation is not StatePeer admission.
use crate::peer_delivery_cursor::{CursorIdentity, StoredDeliveryCursor};
use tilo_relay::delivery_v3::{
    commit::{CommitRef, MAX_ENVELOPE_BYTES},
    state::{
        CommitBoundary, EventChunk, EventKind, EventSource, MAX_ADMITTED_OPERATIONS,
        canonical_item_digest, visible_order_digest,
    },
};

/// Original canonical bytes, still requiring current native-certified StatePeer
/// admission through `admission::CutAdmission`.
pub(crate) struct CompleteEvent {
    pub ordinal: u32,
    pub kind: EventKind,
    pub digest: [u8; 32],
    pub bytes: incoming::IncomingBuffer,
}
struct PartialEvent {
    event: CompleteEvent,
    size: usize,
}
/// A checked transport cut; it cannot manufacture a storage acknowledgement.
pub(crate) struct ReplayCut {
    pub events: Vec<CompleteEvent>,
    pub cursor: StoredDeliveryCursor,
    /// The signed commit this cut ends at (bound by `cursor`).
    pub committed: CommitRef,
}
/// The small-page path. Larger snapshot/replay groups use the separately bounded
/// staging path; this collector never silently raises its caller's byte budget.
pub(crate) struct ReplayBatch {
    identity: CursorIdentity,
    generation: u64,
    budget: usize,
    incoming: incoming::IncomingBudget,
    retained: usize,
    committed: Option<CommitRef>,
    events: Vec<CompleteEvent>,
    partial: Option<PartialEvent>,
    failed: bool,
}
impl ReplayBatch {
    #[cfg(test)]
    pub fn new(identity: CursorIdentity, generation: u64, budget: usize) -> Result<Self, String> {
        Self::with_incoming(
            identity,
            generation,
            budget,
            incoming::IncomingBudget::new(16 * 1024 * 1024)?,
        )
    }
    pub fn with_incoming(
        identity: CursorIdentity,
        generation: u64,
        budget: usize,
        incoming: incoming::IncomingBudget,
    ) -> Result<Self, String> {
        if generation == 0 || budget == 0 || budget > MAX_ENVELOPE_BYTES {
            return Err("invalid replay collection budget or generation".into());
        }
        Ok(Self {
            identity,
            generation,
            budget,
            incoming,
            retained: 0,
            committed: None,
            events: Vec::new(),
            partial: None,
            failed: false,
        })
    }
    pub fn push(&mut self, generation: u64, chunk: EventChunk) -> Result<(), String> {
        let result = self.push_inner(generation, chunk);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn push_inner(&mut self, generation: u64, chunk: EventChunk) -> Result<(), String> {
        if self.failed || generation != self.generation || chunk.source() != EventSource::Native {
            return Err("stale or unsupported replay source".into());
        }
        if self
            .committed
            .as_ref()
            .is_some_and(|commit| commit != chunk.committed())
        {
            return Err("replay crossed an incomplete commit boundary".into());
        }
        if self.partial.is_none() {
            let size =
                usize::try_from(chunk.canonical_size()).map_err(|_| "replay size overflow")?;
            if chunk.offset() != 0
                || size == 0
                || size > MAX_ENVELOPE_BYTES
                || self.events.len() >= MAX_ADMITTED_OPERATIONS
                || self
                    .events
                    .last()
                    .is_some_and(|event| event.ordinal >= chunk.ordinal())
                || self
                    .retained
                    .checked_add(size)
                    .is_none_or(|sum| sum > self.budget)
            {
                return Err("replay event exceeds bounds or order".into());
            }
            let bytes = self.incoming.buffer(size)?;
            let retained = self
                .retained
                .checked_add(bytes.bytes.capacity())
                .ok_or("replay allocation overflow")?;
            if retained > self.budget {
                return Err("replay backing exceeds budget".into());
            }
            self.retained = retained;
            self.partial = Some(PartialEvent {
                event: CompleteEvent {
                    ordinal: chunk.ordinal(),
                    kind: chunk.kind(),
                    digest: chunk.event_digest(),
                    bytes,
                },
                size,
            });
            self.committed = Some(chunk.committed().clone());
        }
        let partial = self.partial.as_mut().ok_or("missing replay range")?;
        if partial.event.ordinal != chunk.ordinal()
            || partial.event.kind != chunk.kind()
            || partial.event.digest != chunk.event_digest()
            || partial.size as u64 != chunk.canonical_size()
            || partial.event.bytes.len() as u64 != chunk.offset()
            || partial
                .event
                .bytes
                .len()
                .checked_add(chunk.bytes().len())
                .is_none_or(|end| end > partial.size)
        {
            return Err("replay range continuity mismatch".into());
        }
        partial.event.bytes.bytes.extend_from_slice(chunk.bytes());
        if partial.event.bytes.len() == partial.size {
            if canonical_item_digest(&partial.event.bytes) != partial.event.digest {
                return Err("replay canonical digest mismatch".into());
            }
            self.events
                .push(self.partial.take().ok_or("missing completed event")?.event);
        }
        Ok(())
    }
    pub fn finish(self, generation: u64, boundary: &CommitBoundary) -> Result<ReplayCut, String> {
        if self.failed
            || generation != self.generation
            || self.partial.is_some()
            || self
                .committed
                .as_ref()
                .is_some_and(|commit| commit != boundary.committed())
            || self.events.len() != boundary.visible_count() as usize
        {
            return Err("incomplete or stale replay boundary".into());
        }
        let order = self
            .events
            .iter()
            .map(|event| (event.ordinal, event.digest))
            .collect::<Vec<_>>();
        if visible_order_digest(&order).map_err(|error| error.to_string())?
            != boundary.visible_order_digest()
        {
            return Err("replay visible order mismatch".into());
        }
        let key = ed25519_dalek::VerifyingKey::from_bytes(&self.identity.host)
            .map_err(|error| error.to_string())?;
        let claims = boundary
            .cursor()
            .authenticate(&key)
            .map_err(|error| error.to_string())?;
        let claim = claims.issue();
        if claim.room != self.identity.room
            || claim.workspace != self.identity.workspace
            || claim.workspace_root != self.identity.workspace_root
            || claim.host_device != self.identity.host
            || claim.host_root != self.identity.host_root
            || claim.consumer_device != self.identity.device
            || claim.consumer_root != self.identity.device_root
            || claim.consumer != boundary.consumer()
            || claim.authorization_view != boundary.authorization_view()
            || claim.store_generation != boundary.store_generation()
            || &claim.committed != boundary.committed()
        {
            return Err("signed cursor does not bind this replay cut".into());
        }
        let cursor = StoredDeliveryCursor {
            version: 1,
            identity: self.identity,
            consumer: claim.consumer.to_vec(),
            authorization_view: claim.authorization_view.to_vec(),
            store_generation: claim.store_generation.to_vec(),
            token: boundary.cursor().as_bytes().to_vec(),
        };
        cursor.validate_storage_shape()?;
        Ok(ReplayCut {
            events: self.events,
            cursor,
            committed: claim.committed.clone(),
        })
    }
}
/// One decoded event poll page.
pub(crate) enum PageContents<'a> {
    /// Complete checked groups in strictly increasing commit order: exactly
    /// one unless the session negotiated `MULTI_GROUP_REPLAY_V1`.
    Cuts(Vec<ReplayCut>),
    /// The only frame: a signed snapshot manifest payload.
    Manifest(&'a [u8]),
    /// The only frame: a checked snapshot reset.
    Reset,
}
/// Decode and check one replay page within its negotiated bounds
/// (`capabilities::page_bounds`). Every group keeps the single-group checks
/// (own 64-frame bound, complete events, signed boundary). A trailing partial
/// group, a group past `max_groups`, or a non-increasing commit fails the page.
pub(crate) fn collect_page<'a>(
    identity: &CursorIdentity,
    generation: u64,
    incoming: &incoming::IncomingBudget,
    bytes: &'a [u8],
    (max_groups, max_frames): (usize, usize),
) -> Result<PageContents<'a>, String> {
    use tilo_relay::delivery_v3::{
        capabilities::{GROUP_PAGE_FRAMES, PAGE_BYTES},
        framing::{self, MessageType},
    };
    let mut cuts: Vec<ReplayCut> = Vec::new();
    let mut batch: Option<ReplayBatch> = None;
    let mut rest = bytes;
    let (mut frames, mut group_frames) = (0usize, 0usize);
    while !rest.is_empty() {
        frames += 1;
        group_frames += 1;
        if frames > max_frames || group_frames > GROUP_PAGE_FRAMES {
            return Err("replay frame count exceeded".into());
        }
        let (kind, payload, tail) =
            framing::decode_frame_prefix(rest).map_err(|error| error.to_string())?;
        rest = tail;
        match kind {
            MessageType::EventChunk => {
                if cuts.len() >= max_groups {
                    return Err("replay page exceeds its negotiated group count".into());
                }
                let chunk = state::EventChunk::decode(payload).map_err(|e| e.to_string())?;
                let batch = match &mut batch {
                    Some(batch) => batch,
                    None => batch.insert(ReplayBatch::with_incoming(
                        identity.clone(),
                        generation,
                        PAGE_BYTES,
                        incoming.clone(),
                    )?),
                };
                batch.push(generation, chunk)?;
            }
            MessageType::CommitBoundary => {
                if cuts.len() >= max_groups {
                    return Err("replay page exceeds its negotiated group count".into());
                }
                let boundary = state::CommitBoundary::decode(payload).map_err(|e| e.to_string())?;
                let collected = match batch.take() {
                    Some(batch) => batch,
                    None => ReplayBatch::with_incoming(
                        identity.clone(),
                        generation,
                        PAGE_BYTES,
                        incoming.clone(),
                    )?,
                };
                if cuts
                    .last()
                    .is_some_and(|prior| boundary.committed().sequence <= prior.committed.sequence)
                {
                    return Err("replay groups are not in increasing commit order".into());
                }
                cuts.push(collected.finish(generation, &boundary)?);
                group_frames = 0;
            }
            MessageType::SnapshotManifest if frames == 1 && rest.is_empty() => {
                return Ok(PageContents::Manifest(payload));
            }
            MessageType::SnapshotRequired if frames == 1 && rest.is_empty() => {
                state::SnapshotRequired::decode(payload).map_err(|e| e.to_string())?;
                return Ok(PageContents::Reset);
            }
            MessageType::SnapshotManifest => {
                return Err("manifest must be the only replay response frame".into());
            }
            MessageType::SnapshotRequired => {
                return Err("snapshot reset must be one exact response".into());
            }
            _ => return Err("unexpected replay frame".into()),
        }
    }
    if cuts.is_empty() || batch.is_some() {
        return Err("replay response missing complete boundary".into());
    }
    Ok(PageContents::Cuts(cuts))
}
pub(crate) mod admission;
pub(crate) mod cursor_recovery;
pub(crate) mod incoming;
pub(crate) mod resume;
#[cfg(test)]
mod tests;
pub(crate) mod transfer;
pub(crate) mod upload;

/// One-item streaming reader for already manifest-verified IDB chunks. It retains
/// no complete group and emits nothing for a partial or digest-invalid item.
pub(crate) struct StreamDecoder {
    order: state::VisibleOrder,
    expected_bytes: u64,
    seen_bytes: u64,
    item_limit: usize,
    header: [u8; 45],
    header_len: usize,
    item: Option<(state::TransferRecordHeader, incoming::IncomingBuffer)>,
    incoming: incoming::IncomingBudget,
    failed: bool,
}
use tilo_relay::delivery_v3::state;
impl StreamDecoder {
    #[cfg(test)]
    pub fn new(items: u32, stream_bytes: u64, item_limit: usize) -> Result<Self, String> {
        Self::with_incoming(
            items,
            stream_bytes,
            item_limit,
            incoming::IncomingBudget::new(16 * 1024 * 1024)?,
        )
    }
    pub fn with_incoming(
        items: u32,
        stream_bytes: u64,
        item_limit: usize,
        incoming: incoming::IncomingBudget,
    ) -> Result<Self, String> {
        if stream_bytes == 0
            || stream_bytes > 256 * 1024 * 1024
            || item_limit == 0
            || item_limit > MAX_ENVELOPE_BYTES
        {
            return Err("invalid staged replay budget".into());
        }
        Ok(Self {
            order: state::VisibleOrder::new(items).map_err(|e| e.to_string())?,
            expected_bytes: stream_bytes,
            seen_bytes: 0,
            item_limit,
            header: [0; 45],
            header_len: 0,
            item: None,
            incoming,
            failed: false,
        })
    }
    pub fn push(
        &mut self,
        bytes: &[u8],
        mut admit: impl FnMut(CompleteEvent) -> Result<(), String>,
    ) -> Result<(), String> {
        let result = self.push_inner(bytes, &mut admit);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn push_inner(
        &mut self,
        mut bytes: &[u8],
        admit: &mut impl FnMut(CompleteEvent) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.failed
            || self
                .seen_bytes
                .checked_add(bytes.len() as u64)
                .is_none_or(|end| end > self.expected_bytes)
        {
            return Err("staged replay stream exceeds bound".into());
        }
        self.seen_bytes += bytes.len() as u64;
        while !bytes.is_empty() {
            if self.item.is_none() {
                let take = (45 - self.header_len).min(bytes.len());
                self.header[self.header_len..self.header_len + take]
                    .copy_from_slice(&bytes[..take]);
                self.header_len += take;
                bytes = &bytes[take..];
                if self.header_len != 45 {
                    continue;
                }
                let header =
                    state::TransferRecordHeader::decode(&self.header).map_err(|e| e.to_string())?;
                let size =
                    usize::try_from(header.canonical_size()).map_err(|_| "staged item overflow")?;
                if size > self.item_limit {
                    return Err("staged canonical item exceeds bound".into());
                }
                self.order
                    .push(header.ordinal(), header.canonical_digest())
                    .map_err(|e| e.to_string())?;
                let body = self.incoming.buffer(size)?;
                self.item = Some((header, body));
                self.header_len = 0;
            }
            let (header, body) = self.item.as_mut().ok_or("staged item missing")?;
            let take = (header.canonical_size() as usize - body.len()).min(bytes.len());
            body.bytes.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if body.len() == header.canonical_size() as usize {
                if canonical_item_digest(body) != header.canonical_digest() {
                    return Err("staged canonical digest mismatch".into());
                }
                let (header, bytes) = self.item.take().ok_or("completed staged item missing")?;
                admit(CompleteEvent {
                    ordinal: header.ordinal(),
                    kind: match header.kind() {
                        state::TransferKind::Governance => state::EventKind::Governance,
                        state::TransferKind::State => state::EventKind::State,
                    },
                    digest: header.canonical_digest(),
                    bytes,
                })?;
            }
        }
        Ok(())
    }
    pub fn finish(self) -> Result<[u8; 32], String> {
        if self.failed
            || self.seen_bytes != self.expected_bytes
            || self.item.is_some()
            || self.header_len != 0
        {
            return Err("incomplete staged replay stream".into());
        }
        self.order.finish().map_err(|e| e.to_string())
    }
}
