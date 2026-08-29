//! Virtual per-peer seat scaffolding for remote-input delivery.
//!
//! The compositor's `State` owns one physical `Seat<State>`. Remote peers
//! driving a share need to route input into that seat without stomping
//! the local user's focus/modifier state, or appearing to the target
//! client as the local device. [`RemoteSeat`] is the per-peer bookkeeping
//! for that: which surface the peer's keyboard is focused on, and the
//! `PeerId` tag carried on each event (for per-user "typing" indicators).
//!
//! `RemoteSeat` does not own a smithay `KeyboardHandle` — it delegates
//! into `state.seat`, so a remote peer's focus change flips the physical
//! seat and is visible to the local user too (v1 shares are
//! one-driver-at-a-time). Multi-user typing interleaves at kernel-input
//! level: whichever `InputEvent::Key` arrives at the owner first wins the
//! serial-counter mint. See
//! [`InputEvent`](crate::network::streaming::InputEvent) for the full
//! contract.

use std::collections::HashMap;

use crate::network::protocol::{PeerId, SurfaceId};

/// One entry per peer that currently has an active grab on a shared
/// surface. Cheap to construct; the map lives on
/// [`crate::compositor::state::State::remote_seats`].
#[derive(Debug, Default, Clone)]
pub struct RemoteSeat {
    /// The surface the peer last emitted `PointerEnter` on. `None` after
    /// a matching `PointerLeave` or at construction time.
    pub focused_surface: Option<SurfaceId>,
}

impl RemoteSeat {
    /// Fresh seat with no focus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a `PointerEnter` for the peer. Returns the previously
    /// focused surface, if any — the compositor uses that to decide
    /// whether it needs to flip the physical seat's keyboard focus.
    pub fn enter(&mut self, surface: SurfaceId) -> Option<SurfaceId> {
        self.focused_surface.replace(surface)
    }

    /// Record a `PointerLeave` for the peer. Returns the previously
    /// focused surface, if any.
    pub fn leave(&mut self) -> Option<SurfaceId> {
        self.focused_surface.take()
    }
}

/// Collection of every peer's `RemoteSeat`. Owned by
/// [`crate::compositor::state::State`].
///
/// Concrete method calls live on `State` because keyboard-focus /
/// keyboard-input delivery needs the physical seat handle, which lives
/// on `State` — not on this map.
#[derive(Debug, Default)]
pub struct RemoteSeats {
    seats: HashMap<PeerId, RemoteSeat>,
}

impl RemoteSeats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up (or lazily insert) the seat for `peer`. Callers get a
    /// mutable borrow so they can flip `focused_surface` without a
    /// second lookup.
    pub fn seat_mut(&mut self, peer: PeerId) -> &mut RemoteSeat {
        self.seats.entry(peer).or_default()
    }

    /// Read-only borrow. Returns `None` if the peer has never emitted
    /// any input.
    pub fn seat(&self, peer: PeerId) -> Option<&RemoteSeat> {
        self.seats.get(&peer)
    }

    /// Number of active peers. Diagnostics only.
    pub fn len(&self) -> usize {
        self.seats.len()
    }

    /// True when no peer has an active seat entry.
    pub fn is_empty(&self) -> bool {
        self.seats.is_empty()
    }

    /// Drop the peer's seat — e.g. on `LeaveWorkspace` or when the
    /// share ends.
    pub fn drop_peer(&mut self, peer: PeerId) {
        self.seats.remove(&peer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_and_leave_track_focused_surface() {
        let mut seat = RemoteSeat::new();
        assert!(seat.focused_surface.is_none());
        let prev = seat.enter(42);
        assert!(prev.is_none());
        assert_eq!(seat.focused_surface, Some(42));
        let prev2 = seat.enter(43);
        assert_eq!(prev2, Some(42));
        assert_eq!(seat.focused_surface, Some(43));
        let cleared = seat.leave();
        assert_eq!(cleared, Some(43));
        assert!(seat.focused_surface.is_none());
        assert!(seat.leave().is_none());
    }

    #[test]
    fn remote_seats_lazy_inserts_and_drops() {
        let mut seats = RemoteSeats::new();
        assert!(seats.is_empty());
        let _ = seats.seat_mut(7);
        assert_eq!(seats.len(), 1);
        assert!(seats.seat(7).is_some());
        let _ = seats.seat_mut(7);
        assert_eq!(seats.len(), 1);
        let _ = seats.seat_mut(8);
        assert_eq!(seats.len(), 2);
        seats.drop_peer(7);
        assert_eq!(seats.len(), 1);
        assert!(seats.seat(7).is_none());
    }
}
