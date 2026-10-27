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
    /// it with `KeyboardFocus`; `PointerEnter` sets it only until the viewer
    /// first uses that protocol. Pointer leave deliberately does not clear it.
    pub focused_surface: Option<SurfaceId>,
    /// Once an accepted `KeyboardFocus` packet arrives, hover must never
    /// reclaim the keyboard, including after an explicit blur.
    pub(super) explicit_keyboard_focus: bool,
    /// Keys this peer currently holds, in evdev keycode space. The target
    /// client still sees one Wayland seat; this attribution exists solely so
    /// one collaborator cannot release another's held key or leave a modifier
    /// stuck after disconnecting.
    pressed_keys: HashSet<u32>,
    /// Pointer buttons held by this peer. Legacy clients see one pointer, so
    /// button releases are refcounted just like modifiers: one collaborator
    /// must not terminate another collaborator's drag.
    pressed_buttons: HashSet<u32>,
}

impl RemoteSeat {
    /// Fresh seat with no focus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a keyboard target for the peer. Returns the previously focused
    /// surface, if any.
    pub fn enter(&mut self, surface: SurfaceId) -> Option<SurfaceId> {
        self.focused_surface.replace(surface)
    }

    /// Release the peer's keyboard target. Returns the previously
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
    /// Keys held by the person at this machine. Local and remote input feed
    /// the same Wayland seat, so their ownership must share the same counts.
    local_pressed_keys: HashSet<u32>,
    /// Buttons held by the physical pointer on this machine.
    local_pressed_buttons: HashSet<u32>,
    /// Number of peers currently holding each key. Releases are forwarded on
    /// the final 1→0 transition. Modifier presses are also folded to 0→1;
    /// printable presses are each forwarded so two collaborators typing the
    /// same letter do not collapse into one character.
    key_holders: HashMap<u32, usize>,
    /// Number of local/remote participants holding each pointer button.
    button_holders: HashMap<u32, usize>,
}

/// Evdev modifier keycodes. A legacy client has one XKB modifier state, so a
/// second collaborator holding the same modifier must not generate another
/// app-facing press, and either collaborator releasing it must not clear the
/// other's hold.
fn is_modifier_key(keycode: u32) -> bool {
    matches!(keycode, 29 | 42 | 54 | 56 | 97 | 100 | 125 | 126)
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

    /// Whether a press of `keycode` by `peer` repeats a key it holds: the
    /// sender's auto-repeat. Modifiers never repeat.
    pub fn repeats(&self, peer: PeerId, keycode: u32) -> bool {
        !is_modifier_key(keycode)
            && self
                .seats
                .get(&peer)
                .is_some_and(|seat| seat.pressed_keys.contains(&keycode))
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
            *holders == 1 || !is_modifier_key(keycode)
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

    /// Fold the physical/local keyboard into the same app-facing key state as
    /// remote collaborators. Printable presses remain distinct typing; a
    /// modifier release is emitted only after every local/remote holder lets go.
    pub fn local_key_transition(&mut self, keycode: u32, pressed: bool) -> bool {
        if pressed {
            if !self.local_pressed_keys.insert(keycode) {
                return false;
            }
            let holders = self.key_holders.entry(keycode).or_default();
            *holders = holders.saturating_add(1);
            *holders == 1 || !is_modifier_key(keycode)
        } else {
            if !self.local_pressed_keys.remove(&keycode) {
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

    /// Whether [`Self::local_key_transition`] would let the physical
    /// keyboard's release of `keycode` reach apps now: it holds the key and
    /// no one else does.
    pub fn local_release_reaches_apps(&self, keycode: u32) -> bool {
        self.local_pressed_keys.contains(&keycode)
            && self.key_holders.get(&keycode).copied() == Some(1)
    }

    /// Retire every key owned by the physical keyboard. Returned transitions
    /// are the final shared-seat releases; a trusted seat reset may ignore
    /// that list when it rebuilds the complete Smithay XKB state itself.
    pub fn release_local_keys(&mut self) -> Vec<u32> {
        let mut keys = self.local_pressed_keys.iter().copied().collect::<Vec<_>>();
        keys.sort_unstable();
        keys.into_iter()
            .filter(|key| self.local_key_transition(*key, false))
            .collect()
    }

    fn button_transition_for(
        held: &mut HashSet<u32>,
        holders: &mut HashMap<u32, usize>,
        button: u32,
        pressed: bool,
    ) -> bool {
        if pressed {
            if !held.insert(button) {
                return false;
            }
            let count = holders.entry(button).or_default();
            *count = count.saturating_add(1);
            *count == 1
        } else {
            if !held.remove(&button) {
                return false;
            }
            let Some(count) = holders.get_mut(&button) else {
                return false;
            };
            *count = count.saturating_sub(1);
            if *count == 0 {
                holders.remove(&button);
                true
            } else {
                false
            }
        }
    }

    /// Fold a remote pointer-button edge into the one pointer exposed to a
    /// legacy client. Only the first press and final release are delivered.
    pub fn button_transition(&mut self, peer: PeerId, button: u32, pressed: bool) -> bool {
        let seat = self.seats.entry(peer).or_default();
        Self::button_transition_for(
            &mut seat.pressed_buttons,
            &mut self.button_holders,
            button,
            pressed,
        )
    }

    /// Fold the host's physical pointer buttons into remote ownership.
    pub fn local_button_transition(&mut self, button: u32, pressed: bool) -> bool {
        Self::button_transition_for(
            &mut self.local_pressed_buttons,
            &mut self.button_holders,
            button,
            pressed,
        )
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

    /// Release buttons owned by `peer`, returning only final shared-seat
    /// releases that must be sent to the legacy client.
    pub fn release_peer_buttons(&mut self, peer: PeerId) -> Vec<u32> {
        let mut buttons = self
            .seats
            .get(&peer)
            .map(|seat| seat.pressed_buttons.iter().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        buttons.sort_unstable();
        buttons
            .into_iter()
            .filter(|button| self.button_transition(peer, *button, false))
            .collect()
    }

    /// Retire every button held by the host pointer, returning the final
    /// shared-seat releases. Used when the backend loses the pointer and no
    /// matching physical release edge can be relied upon.
    pub fn release_local_buttons(&mut self) -> Vec<u32> {
        let mut buttons = self
            .local_pressed_buttons
            .iter()
            .copied()
            .collect::<Vec<_>>();
        buttons.sort_unstable();
        buttons
            .into_iter()
            .filter(|button| self.local_button_transition(*button, false))
            .collect()
    }

    /// Remaining local and remote holders of the shared physical key state.
    pub fn key_holder_count(&self, keycode: u32) -> usize {
        self.key_holders.get(&keycode).copied().unwrap_or(0)
    }

    #[cfg(test)]
    pub fn button_holder_count(&self, button: u32) -> usize {
        self.button_holders.get(&button).copied().unwrap_or(0)
    }

    /// Number of active peers. Diagnostics only.
    pub fn len(&self) -> usize {
        self.seats.len()
    }

    /// True when no peer has an active seat entry.
    pub fn is_empty(&self) -> bool {
        self.seats.is_empty()
    }

    /// Stable snapshot used by emergency teardown to release every key before
    /// removing the virtual seats.
    pub fn peer_ids(&self) -> Vec<PeerId> {
        let mut peers = self.seats.keys().copied().collect::<Vec<_>>();
        peers.sort_unstable();
        peers
    }

    /// Drop the peer's seat — e.g. on `LeaveWorkspace` or when the
    /// share ends.
    pub fn drop_peer(&mut self, peer: PeerId) {
        let _ = self.release_peer_keys(peer);
        let _ = self.release_peer_buttons(peer);
        self.seats.remove(&peer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_seat_focus_is_tracked_per_peer() {
        {
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
        {
            let mut seat = RemoteSeat::new();
            seat.pointer_enter(42);
            seat.enter(42);
            assert_eq!(seat.pointer_leave(), Some(42));
            assert_eq!(seat.focused_surface, Some(42));
        }
        {
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

    #[test]
    fn keys_are_refcounted_across_local_and_remote_holders() {
        {
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
        {
            let mut seats = RemoteSeats::new();
            // Evdev KEY_A. Both collaborators typed; the single legacy caret
            // must receive two presses even though their holds overlap.
            assert!(seats.key_transition(7, 30, true));
            assert!(seats.key_transition(8, 30, true));
            assert!(!seats.key_transition(7, 30, false));
            assert!(seats.key_transition(8, 30, false));
        }
        {
            let mut seats = RemoteSeats::new();
            assert!(seats.key_transition(7, 42, true));
            assert!(seats.key_transition(7, 30, true));
            assert!(!seats.key_transition(8, 42, true));
            assert_eq!(seats.release_peer_keys(7), vec![30]);
            assert_eq!(seats.release_peer_keys(8), vec![42]);
        }
        {
            let mut seats = RemoteSeats::new();
            assert!(seats.local_key_transition(42, true));
            assert!(
                !seats.key_transition(7, 42, true),
                "the app already sees local Shift held"
            );
            assert!(
                !seats.local_key_transition(42, false),
                "local release cannot clear the remote holder"
            );
            assert!(seats.key_transition(7, 42, false));

            assert!(seats.key_transition(7, 29, true));
            assert!(!seats.local_key_transition(29, true));
            assert!(!seats.key_transition(7, 29, false));
            assert!(seats.local_key_transition(29, false));
        }
        {
            let mut seats = RemoteSeats::new();
            assert!(seats.local_key_transition(30, true));
            assert!(seats.key_transition(7, 30, true));
            assert!(!seats.local_key_transition(30, false));
            assert!(seats.key_transition(7, 30, false));
        }
        {
            let mut seats = RemoteSeats::new();
            assert!(seats.local_key_transition(42, true));
            assert_eq!(seats.release_local_keys(), vec![42]);
            assert!(
                seats.local_key_transition(42, true),
                "the first press after device recovery is not a stale duplicate"
            );
        }
    }

    #[test]
    fn pointer_buttons_form_one_drag_across_local_and_remote_holders() {
        {
            let mut seats = RemoteSeats::new();
            assert!(seats.local_button_transition(272, true));
            assert!(!seats.button_transition(7, 272, true));
            assert!(!seats.local_button_transition(272, false));
            assert!(seats.button_transition(7, 272, false));
        }
        {
            let mut seats = RemoteSeats::new();
            assert!(seats.button_transition(7, 272, true));
            assert!(!seats.button_transition(8, 272, true));
            assert!(seats.button_transition(7, 273, true));
            assert_eq!(seats.release_peer_buttons(7), vec![273]);
            assert_eq!(seats.release_peer_buttons(8), vec![272]);
        }
        {
            let mut seats = RemoteSeats::new();
            assert!(seats.local_button_transition(272, true));
            assert!(!seats.button_transition(7, 272, true));
            assert!(seats.local_button_transition(273, true));
            assert_eq!(seats.release_local_buttons(), vec![273]);
            assert_eq!(seats.button_holder_count(272), 1);
            assert!(seats.button_transition(7, 272, false));
        }
    }
}
