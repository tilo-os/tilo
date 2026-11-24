//! Keys typed for a window that has not shown yet. A person who opens an app and types at once
//! (Ctrl+N as Notes opens) types at the place the desktop shows for the window on its way: the
//! client has no surface on screen yet, nothing has the keyboard, and a key routed then reaches no
//! one. So while the policy says the keyboard waits for such a window
//! ([`crate::policy::WindowPolicy::keyboard_awaits_window`]), the person's keys wait, in order,
//! and are routed once it no longer does: to the window, which takes the keyboard as it shows, or
//! wherever keys go once the launch gave up. They wait before the keymap reads them, so each is
//! read in its turn with the keys held around it, as if typed then.
//!
//! A key the person holds while the wait starts goes on as it went: only a press starts the wait,
//! and everything after it queues behind, its release included. The session's lock and a surface
//! holding the keyboard (a launcher, a menu) take keys at once, never behind a window.

use std::collections::VecDeque;
use std::time::Instant;

use smithay::utils::SERIAL_COUNTER;

use crate::state::State;

/// The keys waiting for a window, oldest first: the evdev code, whether it went down, and when the
/// device says it did.
#[derive(Debug, Default)]
pub(crate) struct AwaitedKeys {
    queue: VecDeque<(u32, bool, Instant)>,
}

impl State {
    /// Whether the keyboard waits for a window that has not shown: the policy says so, and nothing
    /// else takes keys now.
    fn keyboard_waits(&mut self) -> bool {
        !self.locked()
            && !self.seat_keys_only
            && !self.surface_holds_keys()
            && self.policy.keyboard_awaits_window()
    }

    /// Queue the key `keycode` going down or up at `at` when it is typed for a window on its way, or
    /// behind keys that are. Whether it was queued; one that was not is routed now.
    pub(crate) fn await_window_with_key(
        &mut self,
        keycode: u32,
        pressed: bool,
        at: Instant,
    ) -> bool {
        self.route_awaited_keys();
        let waits = if self.awaited_keys.queue.is_empty() {
            pressed && self.keyboard_waits()
        } else {
            true
        };
        if waits {
            self.awaited_keys.queue.push_back((keycode, pressed, at));
        }
        waits
    }

    /// Route the keys that waited for a window, in order, once the keyboard no longer waits for it.
    pub(crate) fn route_awaited_keys(&mut self) {
        if self.awaited_keys.queue.is_empty() || self.keyboard_waits() {
            return;
        }
        let keys = std::mem::take(&mut self.awaited_keys.queue);
        tracing::debug!(
            keys = keys.len(),
            "keys typed for a window on its way reach it"
        );
        for (keycode, pressed, at) in keys {
            let time = self.now_ms();
            self.key(
                keycode,
                pressed,
                SERIAL_COUNTER.next_serial(),
                time,
                at,
                true,
            );
        }
    }
}
