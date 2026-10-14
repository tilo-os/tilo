//! In-process continuation of the last committed shell selection for plain
//! route tails.
//!
//! A full [`super::Selection`] re-reads all retained history. For an ordinary
//! signed route increment that lands after every tail already accepted on its
//! route, the selection's outcome is already decided by state it computed last
//! time: the room and every route pair are unchanged (an increment is neither a
//! room frame nor a snapshot), each older input classifies exactly as before,
//! and its own route's ordered readiness cursor is the only thing it advances.
//! That outcome is `Publication::Original`, and the selected generation would
//! differ only by listing the tail, which replay re-selects before serving.
//!
//! The store's [`SharedTailCache`] keeps exactly that cursor ([`RouteTail`])
//! for a few recently written routes. It is valid only for the selected
//! CURRENT a clean, non-batch selection committed in this process, and any
//! other retained mutation invalidates it first, so a tail it cannot vouch for
//! still takes the full selection. It never retires, never verifies a pair,
//! and never survives a restart.
use super::{
    CoeditPayload, CoverageDecision, DIRECTORY, FloorSafety, Frame, OrderedTailReadiness,
    RoomInput, ShellStore, StoreError, durable, frames,
};
use tilo_relay_store::shell::{Admission, ShellTail};
use tilo_relay_store::tail_cache::{Cursor, Lease, SharedTailCache};

pub use tilo_relay_store::tail_cache::ROUTES;

/// One route's cursor after every tail the committed selection accepted on it.
pub struct RouteTail {
    readiness: OrderedTailReadiness,
    /// Selection order is (author, signed sequence); a tail is a pure append
    /// only when it sorts strictly after the last accepted one.
    last: Option<([u8; 32], u64)>,
}
impl RouteTail {
    pub(crate) fn new(readiness: OrderedTailReadiness, last: Option<([u8; 32], u64)>) -> Self {
        Self { readiness, last }
    }
    /// The cursor as the store's cache holds it.
    pub(crate) fn cursor(self) -> Cursor {
        Box::new(self)
    }
}

/// An incoming retained tail as Check reads it: frame, route key, sequence.
fn incoming_tail(bytes: &[u8], workspace: u64) -> Option<(Frame, String, u64)> {
    let (source, CoeditPayload::RetainedDelta { channel, body }) =
        frames::payload(bytes, workspace)?
    else {
        return None;
    };
    let frame = Frame {
        source,
        channel,
        body,
        kind: RoomInput::Delta,
    };
    let (key, seq) = frame.route()?;
    Some((frame, key, seq))
}
/// The route an incoming retained tail writes, if it names one.
pub(crate) fn incoming_route(bytes: &[u8], workspace: u64) -> Option<String> {
    incoming_tail(bytes, workspace).map(|(_, key, _)| key)
}
/// Check out the cursor for the route `bytes` (a retained tail) writes, when
/// the cache extends the selected CURRENT of a bound `store`.
pub(crate) fn checkout(
    store: &dyn ShellStore,
    bytes: &[u8],
) -> Result<Option<FastTail>, StoreError> {
    if super::authority(store).is_none() {
        return Ok(None);
    }
    let Some(key) = incoming_route(bytes, store.workspace_id()) else {
        return Ok(None);
    };
    if !store.tail_cache().holds(&key) {
        return Ok(None);
    }
    let current = durable::current(&store.root().join(DIRECTORY))?;
    let Some((lease, cursor)) = store.tail_cache().begin(current.as_deref(), &key) else {
        return Ok(None);
    };
    // Only this module installs cursors; anything else is no cursor at all,
    // and the cache stays invalid until the next clean selection.
    Ok(cursor
        .downcast::<RouteTail>()
        .ok()
        .map(|tail| FastTail { lease, tail: *tail }))
}

/// One route cursor checked out for a single retained tail.
pub struct FastTail {
    lease: Lease,
    tail: RouteTail,
}
impl FastTail {
    /// The selected snapshot's view of `bytes`, as `SelectedRead::obsolete`
    /// and the selection's Check phase would decide it. For a signed route
    /// increment the other two `obsolete` tests cannot hold: the room only
    /// covers shell-tree frames and `covers_incoming_snapshot` only snapshot
    /// bodies. Anything that is not such an increment classifies `Unknown`
    /// and is left to the full read.
    pub fn admission(&self, bytes: &[u8], workspace: u64) -> Admission {
        let Some((frame, key, _)) = incoming_tail(bytes, workspace) else {
            return Admission::Select;
        };
        if key != self.lease.key() {
            return Admission::Select;
        }
        let decision = self
            .tail
            .readiness
            .classify_tail(frame.source, frame.channel, &frame.body);
        if decision.coverage == CoverageDecision::Covered {
            Admission::Obsolete
        } else if decision.floor == FloorSafety::Safe {
            Admission::Tail
        } else {
            Admission::Select
        }
    }
    /// Whether an admitted [`Admission::Tail`], now durable, is published as
    /// an original without selection: it sorts after every accepted tail on
    /// its route and is dependency-complete after them, exactly as the next
    /// selection's `ValidateExtras` would find. The cursor re-checks the floor
    /// and is unchanged when this returns false.
    pub fn append(&mut self, bytes: &[u8], workspace: u64) -> bool {
        let Some((frame, key, seq)) = incoming_tail(bytes, workspace) else {
            return false;
        };
        if key != self.lease.key()
            || self
                .tail
                .last
                .is_some_and(|last| (frame.source, seq) <= last)
            || !self
                .tail
                .readiness
                .append(frame.source, frame.channel, &frame.body)
        {
            return false;
        }
        self.tail.last = Some((frame.source, seq));
        true
    }
    /// Return the cursor to `cache`: its tail was suppressed or appended in
    /// full.
    pub fn finish(self, cache: &SharedTailCache) {
        cache.finish(self.lease, self.tail.cursor());
    }
}
impl ShellTail for FastTail {
    fn admission(&self, bytes: &[u8], workspace: u64) -> Admission {
        Self::admission(self, bytes, workspace)
    }
    fn append(&mut self, bytes: &[u8], workspace: u64) -> bool {
        Self::append(self, bytes, workspace)
    }
    fn finish(self: Box<Self>, cache: &SharedTailCache) {
        Self::finish(*self, cache);
    }
}
