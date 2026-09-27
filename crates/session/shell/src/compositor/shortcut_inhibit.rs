//! `zwp_keyboard_shortcuts_inhibit_manager_v1`: virtual machines, remote
//! desktops and game streaming (virt-manager, RDP clients, Moonlight) ask
//! for every key, Super chords included, so the machine they show behaves
//! like the one in front of the person.
//!
//! Granted only to the surface holding keyboard focus while chrome is not
//! taking input and the session is unlocked; it follows focus. It suspends
//! only the shell's own shortcuts: the VT switch, the remote-input kill
//! switch, the lock chord, hardware media keys and the pointer release chord
//! ([`crate::compositor::pointer_lock::release_chord_label`]) are handled
//! before any client sees a key and stay with the compositor. The release
//! chord also hands the shortcuts back until focus moves on.

use smithay::wayland::keyboard_shortcuts_inhibit::{
    KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor,
    KeyboardShortcutsInhibitorSeat,
};

use crate::compositor::state::State;

#[derive(Default)]
pub struct ShortcutInhibit {
    /// The surface whose inhibitor is active.
    active: Option<u64>,
    /// The surface the person took the shortcuts back from; not granted
    /// again until keyboard focus moves elsewhere.
    suspended: Option<u64>,
}

impl ShortcutInhibit {
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Whether keys for `surface_id` bypass the shell's shortcuts.
    pub fn inhibits(&self, surface_id: u64) -> bool {
        self.active == Some(surface_id)
    }
}

impl State {
    /// Grant or withdraw shortcut inhibition for this tick. `allowed` is
    /// false while the session is locked or chrome is taking input. Returns
    /// `Some(true)` when an inhibitor was just granted (worth telling the
    /// person) and `Some(false)` when one was withdrawn.
    pub fn sync_shortcut_inhibit(&mut self, allowed: bool) -> Option<bool> {
        let focus = self.client_input.keyboard_focus();
        if self.shortcut_inhibit.suspended.is_some() && self.shortcut_inhibit.suspended != focus {
            self.shortcut_inhibit.suspended = None;
        }
        let target = focus
            .filter(|_| allowed)
            .filter(|id| self.shortcut_inhibit.suspended != Some(*id))
            .filter(|id| self.inhibitor_for(*id).is_some());
        if self.shortcut_inhibit.active == target {
            return None;
        }
        let withdrew = self.shortcut_inhibit.active.take().is_some_and(|previous| {
            if let Some(inhibitor) = self.inhibitor_for(previous) {
                inhibitor.inactivate();
            }
            true
        });
        if let Some(surface_id) = target
            && let Some(inhibitor) = self.inhibitor_for(surface_id)
        {
            inhibitor.activate();
            self.shortcut_inhibit.active = Some(surface_id);
            return Some(true);
        }
        withdrew.then_some(false)
    }

    /// The release chord: give the shortcuts back now. `false` when none
    /// were inhibited.
    pub fn suspend_shortcut_inhibit(&mut self) -> bool {
        let Some(surface_id) = self.shortcut_inhibit.active.take() else {
            return false;
        };
        if let Some(inhibitor) = self.inhibitor_for(surface_id) {
            inhibitor.inactivate();
        }
        self.shortcut_inhibit.suspended = Some(surface_id);
        true
    }

    fn inhibitor_for(&self, surface_id: u64) -> Option<KeyboardShortcutsInhibitor> {
        let surface = self.client_input.surface(surface_id)?;
        self.seat.keyboard_shortcuts_inhibitor_for_surface(surface)
    }
}

impl KeyboardShortcutsInhibitHandler for State {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.keyboard_shortcuts_inhibit_state
    }

    fn new_inhibitor(&mut self, _inhibitor: KeyboardShortcutsInhibitor) {
        // Granted (or not) by the next tick's `sync_shortcut_inhibit`, which
        // knows what holds focus.
    }

    fn inhibitor_destroyed(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        let surface_id = crate::compositor::state::surface_id(inhibitor.wl_surface());
        if self.shortcut_inhibit.active == Some(surface_id) {
            self.shortcut_inhibit.active = None;
        }
    }
}
