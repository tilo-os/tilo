//! Synchronous owner cores. Real resource turns are required; the parent
//! supplies genuine current-context revalidation before each item/page.
//! The installed data routes in `delivery_http::serve` dispatch them.
use super::*;
use crate::{
    SessionOwner,
    committed_authority::{CommittedAuthority, CommittedView},
    host_bytes::HostBytes,
    owner::delivery::{AdmissionOutcome as NativeOutcome, CompleteStateItem},
    owner::resources::{OwnerDataOperation, OwnerResourceTurn},
};
use anyhow::Context as _;
use tilo_relay::delivery_v3::{
    auth::VerifiedSession,
    commit::DurableAdmissionKind,
    http_proof::VerifiedRequest,
    state::{AdmissionCode, CommitScope, RetryCode, StateAdmission},
};

pub(in crate::delivery_http) fn admit_state_core(
    owner: &mut SessionOwner,
    current: &CommittedAuthority,
    request: &VerifiedRequest<'_>,
    mut revalidate: impl FnMut(&SessionOwner, &CommittedView, &VerifiedSession) -> anyhow::Result<u64>,
    budget: &HostBytes,
    first_turn: OwnerResourceTurn,
) -> anyhow::Result<Vec<u8>> {
    // No earlier item can mutate before the complete original batch is parsed.
    let items = prepare_state(request.request())
        .map_err(|error| anyhow::anyhow!("invalid bounded state request: {error:?}"))?;
    let mut response = Vec::new();
    let mut first_turn = Some(first_turn);
    let mut next = 0;
    while next < items.len() {
        let rest = &items[next..];
        let input_len = rest
            .iter()
            .try_fold(0usize, |sum, item| sum.checked_add(item.canonical.len()))
            .context("state batch length overflow")?;
        // The caller reserves the first turn for the complete batch before
        // proof-context authorization. Each later commit unit reserves the
        // remaining length. No await or intervening owner mutation occurs.
        let mut turn = match first_turn.take() {
            Some(turn) => turn,
            None => owner.prepare_data_turn(budget, OwnerDataOperation::State { input_len })?,
        };
        let complete = rest
            .iter()
            .map(|item| CompleteStateItem {
                delivery_id: item.delivery,
                canonical: item.canonical,
            })
            .collect::<Vec<_>>();
        // One commit unit: a group of consecutive new document items sharing
        // one replay group, or one item on the single-item path. An earlier
        // unit may change authority/generation, so each unit re-derives its
        // source from the current view.
        let outcomes = current.with_owner_view(owner, |view, owner| -> anyhow::Result<_> {
            let now_ns = revalidate(owner, view, request.session())?;
            let source = owner.data_source(&mut turn, view, request.session(), now_ns)?;
            Ok(owner.data_admit_group(
                &mut turn,
                &source,
                &complete,
                web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now_ns),
            )?)
        })??;
        anyhow::ensure!(
            !outcomes.is_empty() && outcomes.len() <= rest.len(),
            "state commit unit admitted no item"
        );
        // Both the real store barrier and publication succeeded before creating
        // any durable wire receipt. This serializable value is not authority.
        for (item, outcome) in rest.iter().zip(outcomes) {
            let admission = admission_for(item.delivery, item.digest, outcome)?;
            framing::append_frame(
                &mut response,
                MessageType::StateAdmission,
                &admission.encode()?,
                1024 * 1024,
            )?;
            next += 1;
        }
    }
    Ok(response)
}
/// Map one native outcome to its wire receipt. Called only after the real
/// store barrier and Current publication for that exact item.
pub(in crate::delivery_http) fn admission_for(
    delivery: DeliveryId,
    digest: Digest,
    outcome: NativeOutcome,
) -> anyhow::Result<StateAdmission> {
    Ok(match outcome {
        NativeOutcome::Durable(record) => receipt_admission(delivery, digest, &record)?,
        NativeOutcome::RetryLater(_) => {
            StateAdmission::retry_later(delivery, digest, RetryCode::PrerequisiteMissing)
        }
        NativeOutcome::MetadataOnly => {
            StateAdmission::rejected(delivery, digest, AdmissionCode::Unsupported)
        }
    })
}
/// Re-encode an already durable admission record; used for lost final replies.
pub(in crate::delivery_http) fn receipt_admission(
    delivery: DeliveryId,
    digest: Digest,
    record: &tilo_relay::delivery_v3::commit::AdmissionRecord,
) -> anyhow::Result<StateAdmission> {
    anyhow::ensure!(record.digest == digest, "native receipt digest mismatch");
    Ok(match record.result {
        DurableAdmissionKind::Accepted => StateAdmission::accepted(
            delivery,
            record.digest,
            CommitScope::NativeCommitted,
            record.operations.clone(),
            record.committed.clone(),
        )?,
        DurableAdmissionKind::Duplicate => StateAdmission::duplicate(
            delivery,
            record.digest,
            CommitScope::NativeCommitted,
            record.operations.clone(),
            record.committed.clone(),
        )?,
    })
}
pub(in crate::delivery_http) fn ack_core(
    owner: &mut SessionOwner,
    current: &CommittedAuthority,
    request: &VerifiedRequest<'_>,
    mut revalidate: impl FnMut(&SessionOwner, &CommittedView, &VerifiedSession) -> anyhow::Result<u64>,
    mut turn: OwnerResourceTurn,
) -> anyhow::Result<()> {
    let ack = prepare_ack(request.request())
        .map_err(|error| anyhow::anyhow!("invalid bounded ACK request: {error:?}"))?;
    current.with_owner_view(owner, |view, owner| -> anyhow::Result<()> {
        let now_ns = revalidate(owner, view, request.session())?;
        let source = owner.data_source(&mut turn, view, request.session(), now_ns)?;
        // A cursor of a rotated store or released history cannot advance any
        // retained progress. Acknowledging it is a no-op; the consumer's next
        // poll receives the contract's SnapshotRequired reset. A denial here
        // would discard the whole authenticated session instead.
        let stale = |failure: &crate::owner::delivery::error::OwnerDataFailure| {
            matches!(
                failure.kind(),
                crate::owner::delivery::error::OwnerDataFailureKind::Reset(_)
            )
        };
        let cut = match owner.data_verify_cursor(&mut turn, &source, ack.cursor()) {
            Ok(verified) => verified.into_cut(),
            Err(failure) if stale(&failure) => return Ok(()),
            Err(failure) => return Err(failure.into()),
        };
        match owner.data_acknowledge(&mut turn, &source, cut) {
            Ok(()) => Ok(()),
            Err(failure) if stale(&failure) => Ok(()),
            Err(failure) => Err(failure.into()),
        }
    })??;
    Ok(())
}
pub(in crate::delivery_http) fn replay_core(
    owner: &SessionOwner,
    current: &CommittedAuthority,
    request: &VerifiedRequest<'_>,
    revalidate: impl FnMut(&SessionOwner, &CommittedView, &VerifiedSession) -> anyhow::Result<u64>,
    turn: OwnerResourceTurn,
) -> anyhow::Result<Option<Vec<u8>>> {
    let cursor = prepare_poll(request.request())
        .map_err(|error| anyhow::anyhow!("invalid bounded poll request: {error:?}"))?;
    replay_page_core(
        owner,
        current,
        request.session(),
        cursor.as_ref(),
        revalidate,
        turn,
    )
}
/// Continuation input is the retained original ticket, never a new unsigned cursor.
/// The parent must return this session/checker/turn from actual resume_poll.
pub(in crate::delivery_http) fn replay_pending_core(
    owner: &SessionOwner,
    current: &CommittedAuthority,
    pending: &crate::delivery_http::poll::PendingPoll,
    session: &VerifiedSession,
    revalidate: impl FnMut(&SessionOwner, &CommittedView, &VerifiedSession) -> anyhow::Result<u64>,
    turn: OwnerResourceTurn,
) -> anyhow::Result<Option<Vec<u8>>> {
    replay_page_core(owner, current, session, pending.cursor(), revalidate, turn)
}
fn replay_page_core(
    owner: &SessionOwner,
    current: &CommittedAuthority,
    session: &VerifiedSession,
    cursor: Option<&CursorToken>,
    mut revalidate: impl FnMut(&SessionOwner, &CommittedView, &VerifiedSession) -> anyhow::Result<u64>,
    mut turn: OwnerResourceTurn,
) -> anyhow::Result<Option<Vec<u8>>> {
    use std::ops::Bound::{Excluded, Unbounded};
    use tilo_relay::delivery_v3::capabilities::{PAGE_BYTES, page_bounds};
    let guard = current.read().map_err(anyhow::Error::msg)?;
    // Keep Current until policy, exact cuts, all event bytes and final
    // boundaries have been selected. The page contains no borrowed evidence.
    let now_ns = revalidate(owner, guard.view(), session)?;
    let source = owner.data_source(&mut turn, guard.view(), session, now_ns)?;
    let mut after = cursor
        .map(|token| {
            owner
                .data_verify_cursor(&mut turn, &source, token)
                .map(|verified| verified.committed().sequence)
        })
        .transpose()?;
    let ledger = owner.delivery_ledger()?;
    // Without MULTI_GROUP_REPLAY_V1 a page carries exactly one complete group.
    let (max_groups, max_frames) = page_bounds(session.capabilities());
    let mut page = Vec::new();
    let (mut groups, mut frames) = (0_usize, 0_usize);
    while groups < max_groups {
        let next = match after {
            Some(sequence) => ledger.replay.range((Excluded(sequence), Unbounded)).next(),
            None => ledger.replay.first_key_value(),
        };
        let Some((sequence, entry)) = next else {
            break;
        };
        let group = match replay_group(owner, &mut turn, &source, session, ledger, *sequence, entry)
        {
            Ok(group) => group,
            // The first group keeps the single-group contract: an oversized
            // group returns no prefix and never advances its cursor. A later
            // group simply starts the next page.
            Err(error) if groups == 0 => return Err(error),
            Err(_) => break,
        };
        if page.len().saturating_add(group.bytes.len()) > PAGE_BYTES
            || frames.saturating_add(group.frames) > max_frames
        {
            break;
        }
        page.try_reserve(group.bytes.len())?;
        page.extend_from_slice(&group.bytes);
        frames += group.frames;
        groups += 1;
        after = Some(*sequence);
    }
    Ok((groups > 0).then_some(page))
}
struct EncodedGroup {
    bytes: Vec<u8>,
    frames: usize,
}
/// Encode one complete signed replay group (`EventChunk* CommitBoundary`)
/// within the per-group frame and page byte budgets.
fn replay_group(
    owner: &SessionOwner,
    turn: &mut OwnerResourceTurn,
    source: &crate::owner::delivery::consumer::VerifiedDeliverySource<'_>,
    session: &VerifiedSession,
    ledger: &tilo_relay::delivery_v3::commit::DeliveryLedgerV1,
    sequence: u64,
    entry: &tilo_relay::delivery_v3::commit::ReplayCommit,
) -> anyhow::Result<EncodedGroup> {
    use crate::owner::delivery::oversized_replay_group as oversized;
    use tilo_relay::delivery_v3::{
        capabilities::{GROUP_PAGE_FRAMES, PAGE_BYTES},
        commit::CommitRef,
        state::{CommitBoundary, EventKind, visible_order_digest},
    };
    if entry.publications.len() >= GROUP_PAGE_FRAMES {
        return Err(oversized("complete replay group exceeds page frame budget"));
    }
    let committed = CommitRef {
        sequence,
        digest: entry.digest,
    };
    let publications = owner.data_replay(turn, source, &committed)?;
    let raw_bytes = publications
        .iter()
        .try_fold(0_usize, |sum, p| -> anyhow::Result<usize> {
            sum.checked_add(p?.bytes.len())
                .ok_or_else(|| anyhow::anyhow!("replay size overflow"))
        })?;
    if raw_bytes > PAGE_BYTES {
        return Err(oversized("complete replay group exceeds page byte budget"));
    }
    let mut events = Vec::new();
    for (ordinal, publication) in publications.iter().enumerate() {
        let publication = publication?;
        let kind = match postcard::from_bytes::<qos_peer_state::NeighbourMsg>(publication.bytes)? {
            qos_peer_state::NeighbourMsg::SyncResp { .. } => EventKind::Governance,
            qos_peer_state::NeighbourMsg::AppDocument { .. } => EventKind::State,
            _ => anyhow::bail!("unsupported public replay family"),
        };
        events.push(ReplayEvent {
            ordinal: ordinal.try_into()?,
            digest: *blake3::hash(publication.bytes).as_bytes(),
            kind,
            canonical: publication.bytes,
        });
    }
    let order = events
        .iter()
        .map(|event| (event.ordinal, event.digest))
        .collect::<Vec<_>>();
    let token = owner.data_issue_cursor(turn, source, committed.clone())?;
    let consumer = &ledger.consumers[&session.device_certificate().device_public_key];
    let boundary = CommitBoundary::new(
        consumer.id,
        consumer.authorization_view,
        ledger.store_generation,
        committed.clone(),
        events.len().try_into()?,
        visible_order_digest(&order)?,
        token,
    )?
    .encode()?;
    let mut page_len = framing::HEADER_BYTES
        .checked_add(boundary.len())
        .ok_or_else(|| anyhow::anyhow!("replay size overflow"))?;
    let mut frames = 1_usize;
    // Count a complete group with one bounded scratch allocation at a time.
    for event in &events {
        event.visit_chunks(&committed, |payload| {
            page_len = page_len
                .checked_add(framing::HEADER_BYTES + payload.len())
                .ok_or_else(|| anyhow::anyhow!("replay size overflow"))?;
            frames += 1;
            if page_len > PAGE_BYTES || frames > GROUP_PAGE_FRAMES {
                return Err(oversized("complete replay group exceeds page budget"));
            }
            Ok(())
        })?;
    }
    if page_len > PAGE_BYTES {
        return Err(oversized("replay boundary exceeds page budget"));
    }
    let mut page = Vec::new();
    page.try_reserve_exact(page_len)?;
    for event in &events {
        event.visit_chunks(&committed, |payload| {
            framing::append_frame(&mut page, MessageType::EventChunk, payload, page_len)?;
            Ok(())
        })?;
    }
    framing::append_frame(&mut page, MessageType::CommitBoundary, &boundary, page_len)?;
    anyhow::ensure!(
        page.len() == page_len,
        "replay preflight differs from encoded group"
    );
    Ok(EncodedGroup {
        bytes: page,
        frames,
    })
}
struct ReplayEvent<'a> {
    ordinal: u32,
    digest: Digest,
    kind: tilo_relay::delivery_v3::state::EventKind,
    canonical: &'a [u8],
}
impl ReplayEvent<'_> {
    fn visit_chunks(
        &self,
        committed: &tilo_relay::delivery_v3::commit::CommitRef,
        mut visit: impl FnMut(&[u8]) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        use tilo_relay::delivery_v3::state::{EventChunk, EventSource};
        anyhow::ensure!(
            !self.canonical.is_empty(),
            "empty native replay publication"
        );
        // Leave ample fixed metadata space below the encoded256KiB frame cap.
        const CHUNK: usize = 240 * 1024;
        for (index, bytes) in self.canonical.chunks(CHUNK).enumerate() {
            let chunk = EventChunk::new(
                committed.clone(),
                self.ordinal,
                self.digest,
                EventSource::Native,
                self.kind,
                self.canonical.len() as u64,
                (index * CHUNK) as u64,
                bytes.to_vec(),
            )?;
            visit(&chunk.encode()?)?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;
