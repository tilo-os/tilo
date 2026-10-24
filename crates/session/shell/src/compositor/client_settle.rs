//! When the windows in view finished answering input: what a screenshot
//! taken after input waits for.
//!
//! A client reads its events in order, so the answer to an
//! `xdg_wm_base.ping` sent after the input means the client has read the
//! input. That answer can still leave before the frame the input asked for:
//! a toolkit queues the pong while it reads, then draws, and the swapchain
//! flushes the pong with its commit, or before it when it waits for the last
//! frame's callback. A second ping, sent once the first is answered, is read
//! on a later pass of the client's loop than the one that drew, so its
//! answer comes after every frame the input caused on that loop.
//!
//! A toolkit that draws on its own thread (Tilo's apps do) leaves its event
//! loop free to answer while that thread draws. It says a frame is coming by
//! asking for the frame's `wl_surface.frame` callback before drawing; until
//! the commit carrying that request lands, the client is drawing. So a client
//! has answered once two pings in a row came back while it had no frame
//! asked for and uncommitted, and the compositor holds no capture of its last
//! commit in flight.
//!
//! A window that does not change answers within two round trips, so a
//! screenshot after input that changed nothing is not held back by a guess
//! at how long the app might take; one that changes returns once its frame is
//! in.

use std::time::{Duration, Instant};

use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::SERIAL_COUNTER;
use smithay::wayland::compositor::{
    SurfaceAttributes, TraversalAction, with_surface_tree_downward,
};
use smithay::wayland::shell::PingError;
use smithay::wayland::shell::xdg::ShellClient;

use crate::compositor::state::{ClientState, State};

/// The longest a screenshot waits for clients to answer. A client still
/// drawing after this long is animating, or hung, and the screenshot shows
/// it as it is.
pub const SETTLE_LIMIT: Duration = Duration::from_secs(3);

/// Quiet ping round trips in a row that settle a client; see the module
/// docs for why one is not enough.
const ROUNDS: u8 = 2;

/// Pongs counted per `xdg_wm_base`, the only way a client answers a ping.
#[derive(Default)]
pub struct Pongs(Vec<(ShellClient, u64)>);

impl Pongs {
    pub fn note(&mut self, client: ShellClient) {
        self.0.retain(|(known, _)| known.alive());
        match self.0.iter_mut().find(|(known, _)| *known == client) {
            Some((_, count)) => *count = count.wrapping_add(1),
            None => self.0.push((client, 1)),
        }
    }

    #[must_use]
    pub fn count(&self, client: &ShellClient) -> u64 {
        self.0
            .iter()
            .find(|(known, _)| known == client)
            .map_or(0, |(_, count)| *count)
    }

    pub fn forget(&mut self, client: &ShellClient) {
        self.0.retain(|(known, _)| known != client);
    }
}

/// What one client's ping round trips call for next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeStep {
    /// The ping in flight is not answered yet.
    Wait,
    /// It was, and another round is due: send a ping.
    Ping,
    /// `ROUNDS` quiet rounds in a row were answered.
    Done,
}

/// Ping round trips to one client until `ROUNDS` in a row pass without
/// the client drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rounds {
    /// Quiet answers still needed.
    left: u8,
    sent_at: u64,
    /// The client drew since the ping in flight went out, so its answer
    /// does not count.
    drew: bool,
}

impl Rounds {
    /// The first ping just went out, with the client at `pongs` answers.
    #[must_use]
    pub const fn new(pongs: u64) -> Self {
        Self {
            left: ROUNDS,
            sent_at: pongs,
            drew: false,
        }
    }

    /// Advance on the client's pong count now and whether it is drawing. On
    /// [`ProbeStep::Ping`] the caller sends the next ping and reports it with
    /// [`Self::sent`]. While a frame is on its way no ping is sent: its
    /// commit is what moves things on.
    pub fn step(&mut self, pongs: u64, drawing: bool) -> ProbeStep {
        if drawing {
            self.left = ROUNDS;
            self.drew = true;
        }
        if self.left == 0 {
            return ProbeStep::Done;
        }
        if pongs == self.sent_at || drawing {
            return ProbeStep::Wait;
        }
        if !self.drew {
            self.left = self.left.saturating_sub(1);
        }
        if self.left == 0 {
            ProbeStep::Done
        } else {
            ProbeStep::Ping
        }
    }

    pub const fn sent(&mut self, pongs: u64) {
        self.sent_at = pongs;
        self.drew = false;
    }
}

/// Where a [`Settle`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Waiting,
    /// Every client answered and its last frame is captured.
    Settled,
    /// [`SETTLE_LIMIT`] ran out first.
    TimedOut,
}

struct Probe {
    client: ShellClient,
    id: ClientId,
    /// [`ClientState::index`], how the client's surfaces are filed.
    index: u32,
    rounds: Rounds,
}

/// Waiting for a set of clients to finish answering input.
pub struct Settle {
    probes: Vec<Probe>,
    started: Instant,
}

impl Settle {
    /// Start waiting for the clients that own `surfaces`: release the frame
    /// callbacks they wait on (a client blocked on one cannot answer) and
    /// ping each. Surfaces of a client without `xdg_wm_base` (an X11 window,
    /// a layer surface) are not waited for.
    pub fn begin(state: &mut State, surfaces: &[WlSurface], now: Instant) -> Self {
        crate::compositor::foreign::release_frame_throttle(state);
        let mut probes: Vec<Probe> = Vec::new();
        for surface in surfaces {
            let Some(client) = surface.client() else {
                continue;
            };
            let Some(index) = client.get_data::<ClientState>().map(|data| data.index) else {
                continue;
            };
            if probes.iter().any(|probe| probe.id == client.id()) {
                continue;
            }
            let Some(shell) = state
                .xdg_shell_state
                .toplevel_surfaces()
                .iter()
                .find(|toplevel| owned_by(toplevel.wl_surface(), &client.id()))
                .map(smithay::wayland::shell::xdg::ToplevelSurface::client)
            else {
                continue;
            };
            let pongs = state.shell_pongs.count(&shell);
            if ping(&shell) {
                probes.push(Probe {
                    client: shell,
                    id: client.id(),
                    index,
                    rounds: Rounds::new(pongs),
                });
            }
        }
        Self {
            probes,
            started: now,
        }
    }

    /// Move on with what the clients answered and drew since the last call.
    /// Pings sent here need a flush of the clients to leave.
    pub fn advance(&mut self, state: &mut State, now: Instant) -> Progress {
        let mut waiting = false;
        let mut pinged = false;
        let mut probes = std::mem::take(&mut self.probes);
        probes.retain_mut(|probe| {
            if !probe.client.alive() {
                return false;
            }
            let drawing = drawing(state, &probe.id)
                || crate::compositor::foreign::store().capturing_for_client(probe.index);
            let pongs = state.shell_pongs.count(&probe.client);
            match probe.rounds.step(pongs, drawing) {
                ProbeStep::Wait => waiting = true,
                ProbeStep::Ping => {
                    probe.rounds.sent(pongs);
                    if !ping(&probe.client) {
                        return false;
                    }
                    pinged = true;
                    waiting = true;
                }
                ProbeStep::Done => {}
            }
            true
        });
        self.probes = probes;
        if pinged {
            // A client held in its swapchain on the frame it just drew
            // cannot read the ping until that frame's callback fires.
            crate::compositor::foreign::release_frame_throttle(state);
        }
        if !waiting {
            Progress::Settled
        } else if now.saturating_duration_since(self.started) >= SETTLE_LIMIT {
            Progress::TimedOut
        } else {
            Progress::Waiting
        }
    }

    /// When the wait gives up; answers, commits and captures wake the loop
    /// before.
    #[must_use]
    pub fn deadline(&self) -> Instant {
        self.started
            .checked_add(SETTLE_LIMIT)
            .unwrap_or(self.started)
    }
}

fn owned_by(surface: &WlSurface, client: &ClientId) -> bool {
    surface.client().is_some_and(|owner| owner.id() == *client)
}

/// Whether `client` asked for a frame callback it has not committed yet: a
/// frame it is drawing.
fn drawing(state: &State, client: &ClientId) -> bool {
    let roots = state
        .xdg_shell_state
        .toplevel_surfaces()
        .iter()
        .map(smithay::wayland::shell::xdg::ToplevelSurface::wl_surface)
        .chain(
            state
                .xdg_popups()
                .map(smithay::wayland::shell::xdg::PopupSurface::wl_surface),
        )
        .filter(|surface| owned_by(surface, client));
    let asked = std::cell::Cell::new(false);
    for root in roots {
        with_surface_tree_downward(
            root,
            (),
            |_, _, ()| TraversalAction::DoChildren(()),
            |_, states, ()| {
                // The traversal holds the surface's state lock: read through
                // it, never `with_states` again.
                if !states
                    .cached_state
                    .get::<SurfaceAttributes>()
                    .pending()
                    .frame_callbacks
                    .is_empty()
                {
                    asked.set(true);
                }
            },
            |_, _, ()| !asked.get(),
        );
        if asked.get() {
            return true;
        }
    }
    false
}

/// Ping `client`; `false` when it is gone. A ping already in flight is
/// answered like the one this would have sent, so it counts as sent.
fn ping(client: &ShellClient) -> bool {
    match client.send_ping(SERIAL_COUNTER.next_serial()) {
        Ok(()) | Err(PingError::PingAlreadyPending(_)) => true,
        Err(PingError::DeadSurface) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_quiet_answers_in_turn_settle_a_client() {
        let mut rounds = Rounds::new(4);
        assert_eq!(
            rounds.step(4, false),
            ProbeStep::Wait,
            "the first ping is not answered yet"
        );
        assert_eq!(
            rounds.step(5, false),
            ProbeStep::Ping,
            "the first answer only says the input was read: ask again"
        );
        rounds.sent(5);
        assert_eq!(rounds.step(5, false), ProbeStep::Wait);
        assert_eq!(
            rounds.step(6, false),
            ProbeStep::Done,
            "the second answer follows anything drawn on the loop that read the input"
        );
        assert_eq!(rounds.step(9, false), ProbeStep::Done, "and stays done");
    }

    #[test]
    fn a_client_drawing_is_counted_afresh_after_its_frame() {
        let mut rounds = Rounds::new(0);
        assert_eq!(rounds.step(1, false), ProbeStep::Ping);
        rounds.sent(1);
        assert_eq!(
            rounds.step(2, true),
            ProbeStep::Wait,
            "an answer while a frame is on its way does not count, and asks nothing more"
        );
        assert_eq!(
            rounds.step(2, false),
            ProbeStep::Ping,
            "the frame landed: ask again, the earlier answer came before it"
        );
        rounds.sent(2);
        assert_eq!(rounds.step(3, false), ProbeStep::Ping, "one quiet answer");
        rounds.sent(3);
        assert_eq!(rounds.step(4, false), ProbeStep::Done, "and a second");
    }
}
