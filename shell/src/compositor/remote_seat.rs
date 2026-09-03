//! Virtual per-peer seat scaffolding for remote-input delivery.
//!
//! The compositor's `State` owns one physical `Seat<State>`. Remote peers
//! driving a share need to route input into that seat without stomping
//! the local user's focus/modifier state, or appearing to the target
//! client as the local device. [`RemoteSeat`] is the per-peer bookkeeping
//! for that: which surface the peer's keyboard is focused on, and the
//! `PeerId` tag carried on each event (for per-user "typing" indicators).
//!
//! `RemoteSeat` does not own a smithay `KeyboardHandle`. All participants
//! are multiplexed into the compositor's one app-facing seat while this
//! structure preserves attribution and key ownership. The active tab owns
//! keyboard/text-input focus; remote pointer enter/leave never churns the
//! client's caret or IME session. See
//! [`InputEvent`](crate::network::streaming::InputEvent) for the full
//! contract.

use std::collections::{HashMap, HashSet};

use crate::network::protocol::{PeerId, SurfaceId};

/// One entry per peer that currently has an active grab on a shared
/// surface. Cheap to construct; the map lives on
/// [`crate::compositor::state::State::remote_seats`].
#[derive(Debug, Default, Clone)]
pub struct RemoteSeat {
    /// Surface currently under this participant's pointer. Hover is kept
    /// separate from keyboard focus so leaving a tile cannot kill a caret.
    pub pointer_surface: Option<SurfaceId>,
    /// The surface this peer may send keyboard input to. New viewers update
    /// it with `KeyboardFocus`; `PointerEnter` also sets it for compatibility
    /// with older viewers. Pointer leave deliberately does not clear it.
    pub focused_surface: Option<SurfaceId>,
    /// Keys this peer currently holds, in evdev keycode space. The target
    /// client still sees one Wayland seat; this attribution exists solely so
    /// one collaborator cannot release another's held key or leave a modifier
    /// stuck after disconnecting.
    pressed_keys: HashSet<u32>,
}

impl RemoteSeat {
    /// Fresh seat with no focus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a `PointerEnter` for the peer. Returns the previously focused
    /// surface, if any.
    pub fn enter(&mut self, surface: SurfaceId) -> Option<SurfaceId> {
        self.focused_surface.replace(surface)
    }

    /// Record a `PointerLeave` for the peer. Returns the previously
    /// focused surface, if any.
    pub fn leave(&mut self) -> Option<SurfaceId> {
        self.focused_surface.take()
    }

    pub fn pointer_enter(&mut self, surface: SurfaceId) -> Option<SurfaceId> {
        self.pointer_surface.replace(surface)
    }

    pub fn pointer_leave(&mut self) -> Option<SurfaceId> {
        self.pointer_surface.take()
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
    /// Number of peers currently holding each key. Only the 0→1 and 1→0
    /// transitions are forwarded to the shared Wayland seat.
    key_holders: HashMap<u32, usize>,
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

    /// Fold one peer's key transition into the single seat presented to a
    /// legacy app. Returns `true` only when the physical Wayland seat should
    /// receive the transition.
    pub fn key_transition(&mut self, peer: PeerId, keycode: u32, pressed: bool) -> bool {
        let seat = self.seats.entry(peer).or_default();
        if pressed {
            if !seat.pressed_keys.insert(keycode) {
                return false;
            }
            let holders = self.key_holders.entry(keycode).or_default();
            *holders = holders.saturating_add(1);
            *holders == 1
        } else {
            if !seat.pressed_keys.remove(&keycode) {
                return false;
            }
            let Some(holders) = self.key_holders.get_mut(&keycode) else {
                return false;
            };
            *holders = holders.saturating_sub(1);
            if *holders == 0 {
                self.key_holders.remove(&keycode);
                true
            } else {
                false
            }
        }
    }

    /// Release everything attributed to `peer`. Returned keycodes are the
    /// ones whose final holder disappeared and therefore need a synthetic
    /// release on the app-facing seat.
    pub fn release_peer_keys(&mut self, peer: PeerId) -> Vec<u32> {
        let mut keys = self
            .seats
            .get(&peer)
            .map(|seat| seat.pressed_keys.iter().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        keys.sort_unstable();
        keys.into_iter()
            .filter(|key| self.key_transition(peer, *key, false))
            .collect()
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
        let _ = self.release_peer_keys(peer);
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
    fn pointer_leave_does_not_clear_keyboard_focus() {
        let mut seat = RemoteSeat::new();
        seat.pointer_enter(42);
        seat.enter(42);
        assert_eq!(seat.pointer_leave(), Some(42));
        assert_eq!(seat.focused_surface, Some(42));
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

    #[test]
    fn keys_are_refcounted_across_peers() {
        let mut seats = RemoteSeats::new();
        assert!(seats.key_transition(7, 42, true));
        assert!(
            !seats.key_transition(7, 42, true),
            "repeat is not a second press"
        );
        assert!(
            !seats.key_transition(8, 42, true),
            "app already sees the key held"
        );
        assert!(
            !seats.key_transition(7, 42, false),
            "another peer still holds it"
        );
        assert!(
            seats.key_transition(8, 42, false),
            "last holder releases app seat"
        );
        assert!(
            !seats.key_transition(8, 42, false),
            "unmatched release is ignored"
        );
    }

    #[test]
    fn peer_cleanup_releases_only_final_holders() {
        let mut seats = RemoteSeats::new();
        assert!(seats.key_transition(7, 42, true));
        assert!(seats.key_transition(7, 30, true));
        assert!(!seats.key_transition(8, 42, true));
        assert_eq!(seats.release_peer_keys(7), vec![30]);
        assert_eq!(seats.release_peer_keys(8), vec![42]);
    }
}
