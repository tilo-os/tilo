//! Reading the tunnels' real state off the UI thread: which locally set-up
//! profiles `NetworkManager` still holds and which are active, and the byte
//! counters of the connected ones. One probe at a time, about once a second.

use std::sync::mpsc::{Receiver, TryRecvError, channel};

use ui::WakeHandle;
use ui::time::Instant;

use crate::model::{ConnId, Connection};
use crate::net::{self, Transfer};

/// How often the tunnels are probed.
const INTERVAL: ui::time::Duration = ui::time::Duration::from_secs(1);

/// What one probe read.
pub struct Reading {
    /// `NetworkManager`'s VPN profiles; `None` when it could not be asked.
    pub profiles: Option<Vec<os_actions::VpnConnectionInfo>>,
    /// Counters of the tunnels that were connected when the probe started.
    pub transfers: Vec<(ConnId, Transfer)>,
}

/// The probe in flight, and when the last one started.
#[derive(Default)]
pub struct Prober {
    running: Option<Receiver<Reading>>,
    last: Option<Instant>,
}

impl Prober {
    /// The reading of the probe in flight, once it finished.
    pub fn take(&mut self) -> Option<Reading> {
        let running = self.running.as_ref()?;
        match running.try_recv() {
            Ok(reading) => {
                self.running = None;
                Some(reading)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.running = None;
                None
            }
        }
    }

    /// Start a probe when none is running and the last one is old enough.
    /// `connected` are the tunnels whose counters to read.
    pub fn start_if_due(&mut self, connected: Vec<Connection>, wake: WakeHandle) {
        let now = ui::time::now();
        if self.running.is_some()
            || self
                .last
                .is_some_and(|last| now.duration_since(last) < INTERVAL)
        {
            return;
        }
        self.last = Some(now);
        let (send, receive) = channel();
        self.running = Some(receive);
        ui::rt::app().spawn_blocking(move || {
            let profiles = os_actions::list_vpn_connections().ok();
            let transfers = connected
                .iter()
                .filter_map(|connection| {
                    net::read_transfer(connection).map(|transfer| (connection.id, transfer))
                })
                .collect();
            if send
                .send(Reading {
                    profiles,
                    transfers,
                })
                .is_ok()
            {
                wake.wake();
            }
        });
    }
}
