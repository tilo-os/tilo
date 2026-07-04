//! Virtual per-peer seat scaffolding for remote-input delivery.
//!
//! # What this is
//!
//! The compositor's `State` owns a single physical `Seat<State>` — the
//! keyboard / pointer / touch triad that every local Wayland client
//! interacts with. Remote peers driving a share need a way to route their
//! input events into that seat *without* stomping on the local user's
//! focus / modifier state, and without appearing to the target client as
//! if the input came from the local physical device.
//!
//! [`RemoteSeat`] is the per-peer bookkeeping the owner keeps for each
//! peer that has "grabbed" a shared surface:
//!
//! - Which local `WlSurface` (if any) is the peer's keyboard focused on.
//!   Set on [`InputEvent::PointerEnter`](crate::network::streaming::InputEvent),
//!   cleared on `PointerLeave`.
//! - Origin tag: the `PeerId` on every input event, so the compositor's
//!   render layer can later paint per-user "typing" indicators without
//!   another round-trip.
//!
//! # Delivery
//!
//! `RemoteSeat` does *not* own a smithay `KeyboardHandle` — it delegates
//! into the compositor's single `state.seat`. That means:
//!
//! - `KeyboardHandle::set_focus` is called on the *physical* seat when a
//!   remote peer grabs focus. The local user's cursor sees that focus
//!   change too — v1 shares are "one-driver-at-a-time" at the seat level.
//! - `KeyboardHandle::input` fires with the peer's keysym/keycode and
//!   modifier state, again on the physical seat. Multi-user typing
//!   interleaves at kernel-input level: whichever `InputEvent::Key`
//!   arrives at the owner first wins the serial-counter mint and reaches
//!   the focused client first. See the doc comment on
//!   [`InputEvent`](crate::network::streaming::InputEvent) for the full
//!   contract.
//!
//! A follow-up would spin per-peer `Seat<State>`s so remote input is
//! genuinely isolated. That needs a real `RemoteSeat: SeatHandler` impl
//! plus per-peer keymap loading — a bigger surface than the peer-sync
//! plan's Task 4.4 covers.

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
    /// Fresh seat with no focus. New entries land here on the first
    /// remote-input event we route to `peer`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a `PointerEnter` for the peer. Returns the previously
    /// focused surface, if any — the compositor uses that to decide
    /// whether it needs to flip the physical seat's keyboard focus.
    pub fn enter(&mut self, surface: SurfaceId) -> Option<SurfaceId> {
        std::mem::replace(&mut self.focused_surface, Some(surface))
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
    /// Empty collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up (or lazily insert) the seat for `peer`. Callers get a
    /// mutable borrow so they can flip `focused_surface` without a
    /// second lookup.
    pub fn seat_mut(&mut self, peer: PeerId) -> &mut RemoteSeat {
        self.seats.entry(peer).or_insert_with(RemoteSeat::new)
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
        // A second enter without an intervening leave replaces + reports
        // the prior focus so the compositor can flip the physical seat.
        let prev2 = seat.enter(43);
        assert_eq!(prev2, Some(42));
        assert_eq!(seat.focused_surface, Some(43));
        // Leave clears + reports what was cleared.
        let cleared = seat.leave();
        assert_eq!(cleared, Some(43));
        assert!(seat.focused_surface.is_none());
        // Double leave is a no-op.
        assert!(seat.leave().is_none());
    }

    #[test]
    fn remote_seats_lazy_inserts_and_drops() {
        let mut seats = RemoteSeats::new();
        assert!(seats.is_empty());
        // Lazy insert.
        let _ = seats.seat_mut(7);
        assert_eq!(seats.len(), 1);
        assert!(seats.seat(7).is_some());
        // Second lookup doesn't grow.
        let _ = seats.seat_mut(7);
        assert_eq!(seats.len(), 1);
        // Distinct peer -> new entry.
        let _ = seats.seat_mut(8);
        assert_eq!(seats.len(), 2);
        // Drop reduces the map.
        seats.drop_peer(7);
        assert_eq!(seats.len(), 1);
        assert!(seats.seat(7).is_none());
    }
}
