//! Pings: the desktop's "is it alive?" reaches the app, and only the app's answer lets the keeper
//! answer the desktop.
//!
//! The desktop pings the keeper whenever input reaches a window, and judges the app by the pong.
//! The keeper is always alive, so answering for itself would hide an app that hangs: a window
//! would never say "not responding". Instead the keeper pings every app connection that has a
//! toplevel on the desktop, keeps the desktop's ping, and answers it once the last of them
//! answered. An app that never pongs means the keeper never pongs, and the desktop sees the
//! hang. While no desktop is attached nothing is pinged.
//!
//! The X11 server answers for itself, never for its programs, so its X11 programs that answer
//! pings (`_NET_WM_PING`) are asked too, through [`crate::x11_windows`]: a hung X11 program
//! shows as not responding just as a hung Wayland one does.

use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::utils::Serial;
use smithay::wayland::shell::PingError;
use smithay::wayland::shell::xdg::ShellClient;

use crate::state::State;
use crate::surfaces::{Role, UpRole};

/// Pings in flight, per app connection and per desktop.
#[derive(Default)]
pub(crate) struct Pings {
    /// The desktop's pings not answered yet, in the order they came.
    desktop: Vec<u32>,
    /// App connections that owe the keeper a pong, each with a live handle to its shell.
    waiting: Vec<(ClientId, ShellClient)>,
    /// The X11 programs were asked and have not all answered.
    x11_waiting: bool,
    next_serial: u32,
}

impl Pings {
    /// The desktop went away: its pings cannot be answered any more.
    pub(crate) fn clear(&mut self) {
        self.desktop.clear();
        self.waiting.clear();
        self.x11_waiting = false;
    }
}

impl State {
    /// The desktop asks whether the app is alive.
    pub(crate) fn desktop_ping(&mut self, serial: u32) {
        self.pings.desktop.push(serial);
        let clients = self.shell_clients();
        for (owner, client) in clients {
            if self
                .pings
                .waiting
                .iter()
                .any(|(waiting, _)| *waiting == owner)
            {
                continue;
            }
            self.pings.next_serial = self.pings.next_serial.wrapping_add(1);
            let sent = client.send_ping(Serial::from(self.pings.next_serial));
            match sent {
                // A ping from an earlier desktop may still be out: this one waits for the same
                // answer.
                Ok(()) | Err(PingError::PingAlreadyPending(_)) => {
                    self.pings.waiting.push((owner, client));
                }
                Err(PingError::DeadSurface) => {}
            }
        }
        if !self.pings.x11_waiting {
            self.pings.next_serial = self.pings.next_serial.wrapping_add(1);
            self.pings.x11_waiting = self.xdnd.ping_x11(self.pings.next_serial);
        }
        self.answer_pings();
    }

    /// One app connection answered the keeper's ping.
    pub(crate) fn app_ponged(&mut self, client: &ShellClient) {
        self.pings.waiting.retain(|(_, waiting)| waiting != client);
        self.answer_pings();
    }

    /// The X11 programs answered, or can no longer be asked.
    pub(crate) fn x11_ponged(&mut self) {
        self.pings.x11_waiting = false;
        self.answer_pings();
    }

    /// Answer the desktop once nobody owes the keeper an answer.
    fn answer_pings(&mut self) {
        if !self.pings.waiting.is_empty() || self.pings.x11_waiting {
            return;
        }
        let Some(wm_base) = self.up.as_ref().and_then(|up| up.wm_base.clone()) else {
            return;
        };
        for serial in std::mem::take(&mut self.pings.desktop) {
            wm_base.pong(serial);
        }
    }

    /// One shell client per app connection that shows a toplevel on the desktop; that
    /// connection's answer is what the desktop's ping waits for.
    fn shell_clients(&self) -> Vec<(ClientId, ShellClient)> {
        let mut clients: Vec<(ClientId, ShellClient)> = Vec::new();
        for kept in self.surfaces.values() {
            let Role::Toplevel(top) = &kept.role else {
                continue;
            };
            if !matches!(kept.up.as_ref().map(|up| &up.role), Some(UpRole::Xdg(_)))
                || !top.toplevel.alive()
            {
                continue;
            }
            let Some(owner) = kept.surface.client().map(|client| client.id()) else {
                continue;
            };
            if clients.iter().any(|(known, _)| *known == owner) {
                continue;
            }
            clients.push((owner, top.toplevel.client()));
        }
        clients
    }
}
