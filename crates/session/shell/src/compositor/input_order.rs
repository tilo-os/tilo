//! One order for the two ways a person's input reaches an app.
//!
//! A key, a click or a touch on an app's window reaches the app on its Wayland connection; a
//! press on one of the app's slots in its window's bar is the desktop's to see, and reaches the
//! app on its shell-ui connection
//! ([`ui::floating_surface::FloatingSurfaceHostMessage::ToolbarEvent`]). The app reads the two
//! when it gets to each, so a press could overtake keys typed before it, and keys typed after
//! it could run before it.
//!
//! So each bar event names the input it came after: the serial of the last input event the
//! shell had sent the app's process when the person used the bar. Every input event the shell
//! sends a client carries a serial the shell mints (`client_input::mint_serial`), and the app
//! learns which serials it has handled from its Wayland connection, in order: it runs the bar
//! event once it has handled that input and before what came after it.
//!
//! The bar reports what it did with the input only when the desktop next looks, so the
//! desktop notes the serials minted when it dispatched the input
//! ([`crate::shell::frame::FrameBuilder::dispatch_event`]): the bar event's place is the last
//! input sent the app's process at or before that.

use std::cell::Cell;
use std::collections::HashMap;

thread_local! {
    /// The newest serial the compositor on this thread minted for input.
    static MINTED: Cell<u32> = const { Cell::new(0) };
}

/// The compositor minted `serial` for an input event.
pub fn minted(serial: u32) {
    MINTED.with(|minted| minted.set(serial));
}

/// The newest serial minted for input: what the input dispatched now comes after.
#[must_use]
pub fn clock() -> u32 {
    MINTED.with(Cell::get)
}

/// Whether `serial` was minted at or before `clock`: serials grow by one per event and wrap.
fn at_or_before(serial: u32, clock: u32) -> bool {
    clock.wrapping_sub(serial) < 1 << 31
}

/// The input sent to one process.
#[derive(Debug, Default)]
struct Sent {
    /// The newest serial sent before the bar's events were last placed.
    before: Option<u32>,
    /// The serials sent since, oldest first.
    since: Vec<u32>,
}

/// See the [module docs](self).
#[derive(Debug, Default)]
pub struct InputOrder {
    /// The input sent to each process, by its pid.
    sent: HashMap<u32, Sent>,
    /// The process behind each shell-ui connection.
    processes: HashMap<u64, u32>,
}

impl InputOrder {
    /// The shell-ui `connection` is from the process `pid`.
    pub fn connected(&mut self, connection: u64, pid: Option<u32>) {
        if let Some(pid) = pid {
            self.processes.insert(connection, pid);
        }
    }

    pub fn disconnected(&mut self, connection: u64) {
        if let Some(pid) = self.processes.remove(&connection)
            && !self.processes.values().any(|other| *other == pid)
        {
            self.sent.remove(&pid);
        }
    }

    /// The input event with `serial` was sent to a client of the process `pid`.
    pub fn sent(&mut self, pid: u32, serial: u32) {
        if self.processes.values().any(|known| *known == pid) {
            self.sent.entry(pid).or_default().since.push(serial);
        }
    }

    /// The serial of the last input sent `connection`'s process at or before `clock`: what a
    /// bar event dispatched then comes after.
    #[must_use]
    pub fn after_input(&self, connection: u64, clock: u32) -> Option<u32> {
        let sent = self.sent.get(self.processes.get(&connection)?)?;
        sent.since
            .iter()
            .rev()
            .find(|serial| at_or_before(**serial, clock))
            .copied()
            .or(sent.before)
    }

    /// The bar's events made so far are placed: every one made from now on comes after the
    /// input sent so far, so only the newest of it is kept.
    pub fn placed(&mut self) {
        for sent in self.sent.values_mut() {
            if let Some(newest) = sent.since.pop() {
                sent.before = Some(newest);
            }
            sent.since.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bar_event_comes_after_the_input_sent_its_process_when_it_was_dispatched() {
        let mut order = InputOrder::default();
        order.connected(7, Some(100));
        assert_eq!(order.after_input(7, 5), None, "nothing sent yet");
        order.sent(100, 3);
        order.sent(100, 4);
        // Another process's input is not this one's.
        order.sent(200, 5);
        order.sent(100, 6);
        assert_eq!(order.after_input(7, 5), Some(4));
        assert_eq!(order.after_input(7, 6), Some(6));
        order.placed();
        order.sent(100, 9);
        assert_eq!(
            order.after_input(7, 8),
            Some(6),
            "what was sent before stays"
        );
        assert_eq!(order.after_input(7, 9), Some(9));
        assert_eq!(order.after_input(8, 9), None, "an unknown connection");
    }

    #[test]
    fn serials_compare_across_their_wrap() {
        let mut order = InputOrder::default();
        order.connected(1, Some(100));
        order.sent(100, u32::MAX);
        order.sent(100, 1);
        assert_eq!(order.after_input(1, 0), Some(u32::MAX));
        assert_eq!(order.after_input(1, 1), Some(1));
    }
}
