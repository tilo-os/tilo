//! LIVE_V1 lane C receiver: forwarded signed changes are speculative until
//! durable state confirms them.
//!
//! The host forwards a change after every admission check but before its
//! durable barrier, so a forwarded change can (rarely) never become durable:
//! the barrier fails and the author's retry is refused or never happens.
//! CRDT changes cannot be un-applied, so the Peer keeps two states while any
//! forwarded change is unconfirmed:
//!
//! * the durable basis: exactly what durable delivery installed (and what the
//!   admission journal persists), and
//! * the in-memory view: basis plus the outstanding speculative changes, each
//!   admitted through the same checked native-cut path as replay. The view is
//!   what the desktop renders and what local edits build on.
//!
//! Durable cuts are admitted on the basis, never on the view. The view's
//! records are already in the record cache, though, and installing a cut
//! merges into them (a merge cannot drop a CRDT change), while the cut's
//! storage batch carries a cursor journal that must describe exactly those
//! records. So a cut settles every outstanding change
//! ([`Speculation::settle`]): a change the cut `covers` (host-certified
//! coverage of the author's sequence) is promoted (it is now durable state);
//! one still admissible and younger than [`CONFIRM_DEADLINE_NS`] is persisted
//! with the cut (promoted); anything else is withdrawn and its record rebuilt
//! from the installed state. Without a cut, changes past their deadline are
//! withdrawn ([`Speculation::expire`]). A local publication while changes are
//! outstanding promotes them too (the local change causally depends on what
//! the user saw). A still-admissible forwarded change can therefore be
//! persisted before the host confirms it; a failed host barrier then leaves
//! it only in this Peer's state, as for a local publication.
use qos_peer_state::{NeighbourMsg, StatePeer};
use tilo_relay::delivery_v3::live_wire::MAX_STREAM_FRAME_BYTES;

/// A forwarded change not covered by durable state within this window is
/// withdrawn. Host commits take milliseconds; a failed barrier is retried by
/// the author's durable outbox after reconnect, and a later retry still
/// arrives through durable replay, so withdrawing early only costs a flicker.
pub(crate) const CONFIRM_DEADLINE_NS: u64 = 10_000_000_000;
/// Outstanding speculative changes per Peer; excess is deferred to replay.
pub(crate) const MAX_SPECULATIVE: usize = 64;
/// Retained canonical bytes of outstanding changes per Peer.
pub(crate) const MAX_SPECULATIVE_BYTES: usize = 64 * MAX_STREAM_FRAME_BYTES;

/// One outstanding forwarded change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Speculative {
    pub digest: [u8; 32],
    pub canonical: Vec<u8>,
    pub admitted_at_ns: u64,
}
impl Speculative {
    /// Durable `(app_id, record_id)` the change applies to.
    pub fn route(&self) -> Option<(String, String)> {
        match postcard::from_bytes::<NeighbourMsg>(&self.canonical).ok()? {
            NeighbourMsg::AppDocument {
                app_id, record_id, ..
            } => Some((app_id, record_id)),
            _ => None,
        }
    }
    fn covered_by(&self, durable: &StatePeer) -> bool {
        postcard::from_bytes::<NeighbourMsg>(&self.canonical)
            .is_ok_and(|message| durable.covers(&message))
    }
}

/// Result of reconciling outstanding changes against durable state.
#[derive(Default)]
pub(crate) struct Rebased {
    /// New in-memory state: durable plus the still-outstanding changes.
    pub state: Option<StatePeer>,
    /// Now covered by durable state.
    pub promoted: Vec<Speculative>,
    /// Expired or no longer admissible; the view no longer contains them.
    pub withdrawn: Vec<Speculative>,
}

/// Result of settling outstanding changes into a durable cut that is
/// persisted with its delivery cursor ([`Speculation::settle`]).
pub(crate) struct Settled {
    /// State to install and persist with the cut: the cut plus every
    /// outstanding change that was still admissible and within its deadline.
    pub state: StatePeer,
    /// Covered by the cut, or persisted with it.
    pub promoted: Vec<Speculative>,
    /// Expired or no longer admissible; the state no longer contains them.
    pub withdrawn: Vec<Speculative>,
}

/// Per-Peer speculative overlay (see module docs).
#[derive(Default)]
pub(crate) struct Speculation {
    basis: Option<StatePeer>,
    items: Vec<Speculative>,
}
impl Speculation {
    /// Whether the Peer's in-memory state currently differs from durable state.
    pub fn is_active(&self) -> bool {
        self.basis.is_some()
    }
    pub fn outstanding(&self) -> &[Speculative] {
        &self.items
    }
    pub fn contains(&self, digest: &[u8; 32]) -> bool {
        self.items.iter().any(|item| &item.digest == digest)
    }
    /// Admit one forwarded change on top of `view` (the Peer's current
    /// in-memory state) and return the new view. The first outstanding change
    /// captures `view` as the durable basis. A refused change leaves nothing.
    pub fn admit(
        &mut self,
        view: &StatePeer,
        item: Speculative,
        admit: impl Fn(&StatePeer, &[u8]) -> Result<StatePeer, String>,
    ) -> Result<StatePeer, String> {
        let bytes = self.items.iter().map(|i| i.canonical.len()).sum::<usize>();
        if self.items.len() >= MAX_SPECULATIVE
            || bytes.saturating_add(item.canonical.len()) > MAX_SPECULATIVE_BYTES
        {
            return Err("speculative live capacity".into());
        }
        if self.contains(&item.digest) {
            return Err("speculative change already outstanding".into());
        }
        let candidate = admit(view, &item.canonical)?;
        if self.basis.is_none() {
            self.basis = Some(view.clone());
        }
        self.items.push(item);
        Ok(candidate)
    }
    /// Durable basis for a durable cut; the cut must never build on the view.
    /// Returns `None` when no change is outstanding (basis == current state).
    /// The outstanding changes stay queued for [`Speculation::settle`].
    pub fn take_basis(&mut self) -> Option<StatePeer> {
        self.basis.take()
    }
    /// Drop one outstanding change whose view could not be installed (the
    /// Peer state is unchanged, so no other change depends on it).
    pub fn forget(&mut self, digest: &[u8; 32]) {
        self.items.retain(|item| &item.digest != digest);
        if self.items.is_empty() {
            self.basis = None;
        }
    }
    /// Put the basis back after a failed durable cut. A no-op when the cut
    /// already installed and rebased (a newer basis, or nothing outstanding).
    pub fn restore_basis(&mut self, basis: StatePeer) {
        if self.basis.is_none() && !self.items.is_empty() {
            self.basis = Some(basis);
        }
    }
    /// Reconcile every outstanding change against newly installed `durable`.
    pub fn rebase(
        &mut self,
        durable: &StatePeer,
        now_ns: u64,
        admit: impl Fn(&StatePeer, &[u8]) -> Result<StatePeer, String>,
    ) -> Rebased {
        self.basis = None;
        let mut out = Rebased::default();
        let mut view: Option<StatePeer> = None;
        let mut keep = Vec::new();
        for item in std::mem::take(&mut self.items) {
            if item.covered_by(durable) {
                out.promoted.push(item);
            } else if now_ns.saturating_sub(item.admitted_at_ns) >= CONFIRM_DEADLINE_NS {
                out.withdrawn.push(item);
            } else {
                match admit(view.as_ref().unwrap_or(durable), &item.canonical) {
                    Ok(next) => {
                        view = Some(next);
                        keep.push(item);
                    }
                    Err(_) => out.withdrawn.push(item),
                }
            }
        }
        if !keep.is_empty() {
            self.basis = Some(durable.clone());
            self.items = keep;
        }
        out.state = view;
        out
    }
    /// Settle every outstanding change into a durable cut built on the basis
    /// that is about to be persisted with its delivery cursor.
    ///
    /// The view's records are already in the record cache (they render from
    /// it), and installing the cut merges into those records, which cannot
    /// drop a CRDT change. The cursor journal of that same storage batch must
    /// describe exactly the persisted records, so a change the cut does not
    /// cover cannot stay speculative across it: one still admissible and
    /// younger than [`CONFIRM_DEADLINE_NS`] is persisted with the cut
    /// (promoted, as for a local publication); anything else is withdrawn.
    /// Keeping it speculative made the batch's journal lack heads its
    /// records carry, so every such cut failed persistence.
    ///
    /// Takes the overlay; [`Speculation::restore`] puts it back when the cut
    /// then fails to install.
    pub fn settle(
        &mut self,
        durable: StatePeer,
        now_ns: u64,
        admit: impl Fn(&StatePeer, &[u8]) -> Result<StatePeer, String>,
    ) -> Settled {
        self.basis = None;
        let mut out = Settled {
            state: durable,
            promoted: Vec::new(),
            withdrawn: Vec::new(),
        };
        for item in std::mem::take(&mut self.items) {
            if item.covered_by(&out.state) {
                out.promoted.push(item);
            } else if now_ns.saturating_sub(item.admitted_at_ns) >= CONFIRM_DEADLINE_NS {
                out.withdrawn.push(item);
            } else {
                match admit(&out.state, &item.canonical) {
                    Ok(next) => {
                        out.state = next;
                        out.promoted.push(item);
                    }
                    Err(_) => out.withdrawn.push(item),
                }
            }
        }
        out
    }
    /// Undo [`Speculation::settle`] after the cut failed to install: the
    /// Peer state (the view) is unchanged, so the overlay is exactly as before.
    pub fn restore(&mut self, basis: StatePeer, items: Vec<Speculative>) {
        if !items.is_empty() {
            self.basis = Some(basis);
            self.items = items;
        }
    }
    /// Withdraw changes past their deadline without a durable cut. `None`
    /// when nothing expired; otherwise the rebuilt in-memory state.
    pub fn expire(
        &mut self,
        now_ns: u64,
        admit: impl Fn(&StatePeer, &[u8]) -> Result<StatePeer, String>,
    ) -> Option<Rebased> {
        let due = self
            .items
            .iter()
            .any(|item| now_ns.saturating_sub(item.admitted_at_ns) >= CONFIRM_DEADLINE_NS);
        if !due {
            return None;
        }
        let basis = self.basis.take()?;
        let mut rebased = self.rebase(&basis, now_ns, admit);
        rebased.state.get_or_insert(basis);
        Some(rebased)
    }
    /// A local publication built on the view: keep every outstanding change
    /// (it is now a causal dependency of durable local state).
    pub fn promote_all(&mut self) -> Vec<Speculative> {
        self.basis = None;
        std::mem::take(&mut self.items)
    }
    /// Earliest instant (ns) at which an outstanding change expires.
    pub fn next_deadline_ns(&self) -> Option<u64> {
        self.items
            .iter()
            .map(|item| item.admitted_at_ns.saturating_add(CONFIRM_DEADLINE_NS))
            .min()
    }
}

#[cfg(test)]
#[path = "speculation_tests.rs"]
mod tests;
