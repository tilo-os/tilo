//! Native complete-cut admission into one browser StatePeer candidate.
//!
//! Every original publication is decoded under the shared incoming budget and
//! admitted through StatePeer's checked incoming path (bounded, fully validated
//! Automerge application; the original owner is untouched on refusal). The
//! candidate is exported only by `finish`, together with the exact signed
//! cursor that ends the cut, so the caller can stage state, pending journal
//! work and cursor in one storage transaction.
use super::{CompleteEvent, incoming::IncomingBudget};
use crate::peer_delivery_cursor::StoredDeliveryCursor;
use crate::peer_journal::{JournalFormat, JournalStorage};
use qos_peer_state::{NativePublicationCut, NeighbourMsg, StatePeer};
use tilo_relay::delivery_v3::{cursor::CursorToken, state::EventKind};
use web_time::SystemTime;

/// One streaming cut. An error consumes the candidate; the live owner keeps
/// its prior state and cursor, so a retry replays from the committed cursor.
pub(crate) struct CutAdmission {
    cut: Option<NativePublicationCut>,
    incoming: IncomingBudget,
}
impl CutAdmission {
    /// The native host publishes as the pinned governing authority; any other
    /// publisher is refused by StatePeer before the first item is read.
    pub fn begin(
        state: &StatePeer,
        incoming: IncomingBudget,
        now: SystemTime,
    ) -> Result<Self, String> {
        let publisher = state.authority().device_public_key;
        let cut = state
            .begin_native_publication_cut(publisher, now)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            cut: Some(cut),
            incoming,
        })
    }
    /// One complete delivery item; its signed family label must match.
    pub fn push(&mut self, event: CompleteEvent, now: SystemTime) -> Result<(), String> {
        self.push_decoded(event.bytes.bytes(), Some(event.kind), now)
    }
    /// One original publication retained by an authenticated onboarding
    /// handoff (local enrollment or invited Complete), which carries no label.
    pub fn push_retained(&mut self, bytes: &[u8], now: SystemTime) -> Result<(), String> {
        self.push_decoded(bytes, None, now)
    }
    fn push_decoded(
        &mut self,
        bytes: &[u8],
        kind: Option<EventKind>,
        now: SystemTime,
    ) -> Result<(), String> {
        let cut = self.cut.take().ok_or("native cut already failed")?;
        let held = qos_peer_state::resources::BoundedPeerMessage::decode(
            bytes,
            qos_peer_state::resources::OuterLimits {
                allocation_bytes: self.incoming.available().min(64 * 1024 * 1024),
                collection_entries: 65536,
            },
            self.incoming.allowance(),
        )
        .map_err(|e| format!("bounded native publication decode: {e:?}"))?;
        if !matches!(
            (kind, held.message()),
            (
                Some(EventKind::Governance) | None,
                NeighbourMsg::SyncResp { .. }
            ) | (
                Some(EventKind::State) | None,
                NeighbourMsg::AppDocument { .. }
            )
        ) {
            return Err("native publication family mismatch".into());
        }
        // The outer reservation stays charged while the checked admission runs.
        let message = held.message().clone();
        let cut = cut.push(message, now).map_err(|error| error.to_string())?;
        drop(held);
        self.cut = Some(cut);
        Ok(())
    }
    /// Export the candidate once every item in the cut resolved; a cut that
    /// adds unresolved input or revokes its publisher is refused whole.
    pub fn finish(self, now: SystemTime) -> Result<StatePeer, String> {
        self.cut
            .ok_or("native cut already failed")?
            .finish(now)
            .map_err(|error| error.to_string())
    }
    /// A delivery cut also advances the replay cursor. A candidate that is
    /// still incomplete (for example a required encrypted group) cannot.
    pub fn finish_complete(self, now: SystemTime) -> Result<StatePeer, String> {
        let candidate = self.finish(now)?;
        if let Some(reason) = candidate.incomplete_reason() {
            return Err(format!("native cut cannot advance cursor: {reason}"));
        }
        Ok(candidate)
    }
}

/// Admit consecutive checked cuts (one poll page) onto `base`, each through
/// its own complete native cut, in order. Returns the candidate after the
/// longest admitted prefix, the cursor that ends it, and how many cuts it
/// covers. The first cut's refusal is the page's refusal; a later refusal
/// stops at the admitted prefix, whose cursor the next poll resumes from.
pub(crate) fn admit_cuts(
    base: &StatePeer,
    cuts: Vec<super::ReplayCut>,
    incoming: &IncomingBudget,
    now: SystemTime,
) -> Result<(StatePeer, StoredDeliveryCursor, usize), String> {
    let mut admitted: Option<(StatePeer, StoredDeliveryCursor)> = None;
    let mut count = 0;
    for cut in cuts {
        let result = (|| {
            let from = admitted.as_ref().map_or(base, |(state, _)| state);
            let mut cut_admission = CutAdmission::begin(from, incoming.clone(), now)?;
            for event in cut.events {
                cut_admission.push(event, now)?;
            }
            cut_admission.finish_complete(now)
        })();
        match result {
            Ok(state) => {
                admitted = Some((state, cut.cursor));
                count += 1;
            }
            Err(error) if admitted.is_none() => return Err(error),
            Err(_) => break,
        }
    }
    let (state, cursor) = admitted.ok_or("empty replay page")?;
    Ok((state, cursor, count))
}

/// Cursor continuity across one page: the first cut continues the committed
/// cursor, every later cut continues the cut before it.
pub(crate) fn page_cursor_steps(
    prior: Option<&StoredDeliveryCursor>,
    cuts: &[super::ReplayCut],
) -> Result<(), String> {
    let mut previous = prior;
    for cut in cuts {
        cursor_step(previous, &cut.cursor, false)?;
        previous = Some(&cut.cursor);
    }
    Ok(())
}

/// How the incoming cursor relates to the committed one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CursorStep {
    /// Suffix replay: same consumer, store generation and authorization view.
    Suffix,
    /// Snapshot of a changed scope (reset, new store generation or view).
    Reset,
}

/// Cursor continuity. A suffix must never regress or equivocate at a sequence;
/// only a signed snapshot may replace the scope. Both tokens are authenticated.
pub(crate) fn cursor_step(
    prior: Option<&StoredDeliveryCursor>,
    next: &StoredDeliveryCursor,
    snapshot: bool,
) -> Result<CursorStep, String> {
    next.validate_storage_shape()?;
    // Without a committed cursor the host replays from its first retained
    // commit; the signed boundary already binds that cut to this identity.
    let Some(prior) = prior else {
        return Ok(if snapshot {
            CursorStep::Reset
        } else {
            CursorStep::Suffix
        });
    };
    if prior.identity != next.identity {
        return Err("cursor identity changed".into());
    }
    let key = ed25519_dalek::VerifyingKey::from_bytes(&next.identity.host)
        .map_err(|error| error.to_string())?;
    let old = CursorToken::from_bytes(&prior.token)
        .map_err(|error| error.to_string())?
        .authenticate(&key)
        .map_err(|error| error.to_string())?;
    let new = CursorToken::from_bytes(&next.token)
        .map_err(|error| error.to_string())?
        .authenticate(&key)
        .map_err(|error| error.to_string())?;
    let same_scope = old.issue().consumer == new.issue().consumer
        && old.issue().store_generation == new.issue().store_generation
        && old.issue().authorization_view == new.issue().authorization_view;
    if !same_scope {
        return if snapshot {
            Ok(CursorStep::Reset)
        } else {
            Err("replay cursor continuity changed; snapshot required".into())
        };
    }
    if new.committed().sequence < old.committed().sequence
        || new.committed().sequence == old.committed().sequence
            && new.committed() != old.committed()
    {
        return Err("cursor regressed or equivocated within its scope".into());
    }
    Ok(CursorStep::Suffix)
}

/// The journal metadata to stage with the admitted candidate. The cursor and
/// canonical state share one Admission value, hence one storage write.
pub(crate) fn advance_cursor(
    storage: &JournalStorage,
    cursor: StoredDeliveryCursor,
) -> Result<JournalStorage, String> {
    cursor.validate_storage_shape()?;
    if storage
        .descriptor
        .as_ref()
        .is_some_and(|floor| floor.identity != cursor.identity)
        || storage
            .delivery
            .as_ref()
            .is_some_and(|prior| prior.identity != cursor.identity)
    {
        return Err("cursor identity differs from stored delivery scope".into());
    }
    let mut next = storage.clone();
    if next.format == JournalFormat::V2 {
        next.format = JournalFormat::V3;
    }
    next.delivery = Some(cursor);
    Ok(next)
}

/// The cursor may be acknowledged only after the exact staged cut was committed
/// by one confirmed storage transaction of the current delivery generation.
pub(crate) fn confirm_commit(
    pending: &StoredDeliveryCursor,
    committed: Option<&StoredDeliveryCursor>,
) -> Result<(), String> {
    if committed != Some(pending) {
        return Err("delivery cut lacks exact atomic storage acknowledgement".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
