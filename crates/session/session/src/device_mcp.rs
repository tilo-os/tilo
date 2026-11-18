//! The MCP server toward the person's other devices.
//!
//! This device's endpoint answers [`tilo_device_mcp::ALPN`]: an
//! agent on another of the person's own devices connects to this session's
//! MCP server over iroh, and is asked about and served exactly as a program of
//! this login is ([`crate::mcp_server`]), as an agent the person delegates to,
//! named after the device it came from. Which device dials is the transport's
//! to say, and whether it is one of the person's is their own store's
//! ([`tilo_runtime::personal_devices::PersonalDevices`]); a connection from anyone
//! else is refused before the person is asked.
//!
//! A connection the server admits takes one of the MCP server's places as it arrives, and waits
//! in it for the session's pass ([`crate::mcp_server::Room`]): it waits no longer than the
//! server has places, and one there is no place for is refused here, where the dialer would
//! otherwise be told the connection opened.
//!
//! The person may let such agents in without being asked here — on a box
//! nobody sits at — with this device's `mcp_server` setting
//! ([`settings_store::mcp_server`]), which is off unless they turn it on. With
//! its second switch on as well, such an agent's steps go unasked too: a step
//! it would ask the person about here is allowed by the setting
//! ([`crate::step_questions`]).
//!
//! A key of the person's that dials the server only to read their workspace list is answered it
//! without anyone being asked ([`DeviceAgents::show_list`]): the workspaces it reaches, each with
//! its host's signed address.
//!
//! The other way round, a `--connect --device NAME` client of this login asks
//! this session to carry it to another device's server ([`DeviceAgents::forward`]),
//! and an agent lists the person's devices with
//! [`tilo_mcp::devices::DEVICES_TOOL`].

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, OnceLock};

use tilo_device_mcp::{Arrival, Arrivals, DeviceServer, Forward, OtherDevices, PersonsDevices};
use tilo_lock::MutexRecover as _;
use tilo_mcp::devices::{DeviceDirectory, Reach};
use tilo_service::Wake;

use crate::mcp_server::{PeerRefusal, Place, Room};

/// What the session's MCP server hears from the person's other devices, and how it reaches
/// theirs.
pub(crate) struct DeviceAgents {
    /// Connections the server opened, each waiting for the MCP server's pass in the place it
    /// took there: as many as that server has places free, and no more.
    arrivals: Mutex<Vec<(Arrival, Place)>>,
    /// Wakes the session's loop when one arrives.
    wake: Wake,
    /// The room of the session's MCP server, once it takes what arrives: a session that serves no
    /// such server refuses them at once.
    room: OnceLock<Room>,
    /// The person's other devices' servers, once this device's endpoint has bound and its store
    /// is open.
    servers: OnceLock<Arc<OtherDevices>>,
    /// The person's workspace list as this device holds it, for a key of theirs that dials the
    /// server to read it.
    list: Mutex<Option<tilo_server_session::workspaces::PersonalList>>,
}

impl DeviceAgents {
    pub(crate) fn new(wake: Wake) -> Arc<Self> {
        Arc::new(Self {
            arrivals: Mutex::default(),
            wake,
            room: OnceLock::new(),
            servers: OnceLock::new(),
            list: Mutex::default(),
        })
    }

    /// The MCP server this device answers the person's other devices' agents at,
    /// admitting those `devices` says are theirs.
    pub(crate) fn server(self: &Arc<Self>, devices: Arc<dyn PersonsDevices>) -> DeviceServer {
        DeviceServer::new(devices, Arc::clone(self) as Arc<dyn Arrivals>)
    }

    /// The protocol this device's endpoint answers the server on.
    pub(crate) fn protocol(
        self: &Arc<Self>,
        devices: Arc<dyn PersonsDevices>,
    ) -> (Vec<u8>, Box<dyn iroh::protocol::DynProtocolHandler>) {
        (
            tilo_device_mcp::ALPN.to_vec(),
            Box::<dyn iroh::protocol::DynProtocolHandler>::from(self.server(devices)),
        )
    }

    /// Reach the person's other devices through `servers` from now on.
    pub(crate) fn attach(&self, servers: OtherDevices) {
        let _ = self.servers.set(Arc::new(servers));
    }

    /// The MCP server serves what arrives from now on, each in a place of `room`.
    pub(crate) fn serve(&self, room: Room) {
        if self.room.set(room).is_err() {
            tracing::warn!("MCP server: another device's connections are served already");
        }
    }

    /// Answer `list` to the person's keys that dial the server to read where their workspaces
    /// are.
    pub(crate) fn show_list(&self, list: tilo_server_session::workspaces::PersonalList) {
        *self.list.lock_recover() = Some(list);
    }

    /// Every connection that arrived since the last pass, with the place it is served in.
    pub(crate) fn take(&self) -> Vec<(Arrival, Place)> {
        std::mem::take(&mut *self.arrivals.lock_recover())
    }

    /// Carry `stream`, whose client asked for `forward`, to that device's server. Blocks the
    /// calling thread for as long as the connection lasts.
    pub(crate) fn forward(&self, stream: UnixStream, forward: &Forward) {
        match self.servers.get() {
            Some(servers) => servers.serve_forward(stream, forward),
            None => tilo_device_mcp::refuse_forward(
                stream,
                "this desktop reaches none of your other devices: it takes no part in the \
                 peer network"
                    .into(),
            ),
        }
    }

    /// Whether the person lets an agent from another of their devices in without asking here, as
    /// this device's settings say now.
    pub(crate) fn accepts_own_devices() -> bool {
        settings_store::mcp_server::accepts_own_devices(&settings_store::load())
    }

    /// Whether the person lets such an agent take a step it would ask them about here without
    /// asking, as this device's settings say now: only while they let it in without asking too.
    pub(crate) fn acts_without_asking() -> bool {
        settings_store::mcp_server::acts_without_asking(&settings_store::load())
    }
}

impl Arrivals for DeviceAgents {
    fn arrive(&self, arrival: Arrival) -> Result<(), String> {
        let Some(room) = self.room.get() else {
            return Err("this desktop serves no agents now".into());
        };
        // Its device is the person's, or the server would not have handed it over: it is told
        // what a program of this login is, and before its dialer hears that it opened.
        let Some(place) = room.take() else {
            tracing::info!(
                device = %arrival.origin.name(),
                "MCP server: another device's connection is refused: the server has no place free"
            );
            return Err(PeerRefusal::Busy.message().to_owned());
        };
        self.arrivals.lock_recover().push((arrival, place));
        (self.wake)();
        Ok(())
    }

    fn listed(&self) -> Option<tilo_server_session::workspaces::PersonalList> {
        self.list.lock_recover().clone()
    }
}

impl DeviceDirectory for DeviceAgents {
    fn reach(&self) -> std::pin::Pin<Box<dyn Future<Output = Vec<Reach>> + Send + '_>> {
        let servers = self.servers.get().cloned();
        Box::pin(async move {
            match servers {
                Some(servers) => servers.reach_now().await,
                None => Vec::new(),
            }
        })
    }
}
