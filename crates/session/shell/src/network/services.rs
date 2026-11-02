//! The desktop's side of the network actor ([`tilo_network_actor::services`]):
//! device enrollments, a host's questions for an approver, an early-boot
//! device asking for its unlock half, a device routing through this one, and
//! peers at another protocol version. Calls are not among them: the session
//! that hosts a call carries it.

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_network_actor::HostIdentity;
use tilo_network_actor::services::{
    Approvers, Calls, Enrollments, ServiceCommand, Services, Sessions,
};
use tilo_relay_session::coedit::{RelayCoeditSession, VoiceEvent};

use super::agent_questions::Answering;
use super::invite::enrollment_driver;

/// Install the desktop's services for every actor this process starts.
/// Whatever starts an actor calls it first; the first call installs them.
pub(crate) fn install() {
    tilo_network_actor::services::install(Services {
        peer_version: crate::shell::collaborator_versions::note,
        device_unlock,
        device_vpn,
        calls: || Box::new(NoCalls),
        enrollments: || Box::new(enrollment_driver::Registry::default()),
        approvers: || Box::new(Answering::default()),
    });
}

fn device_unlock(connection: Connection) {
    tokio::spawn(crate::shell::device_unlock_runtime::serve_connection(
        connection,
    ));
}

#[cfg(target_os = "linux")]
fn device_vpn(connection: Connection) {
    tokio::spawn(crate::shell::device_vpn::serve_connection(connection));
}

#[cfg(not(target_os = "linux"))]
fn device_vpn(connection: Connection) {
    connection.close(0u32.into(), b"not served");
}

/// This process takes part in no call: a call runs in the session that hosts
/// it, not in this network.
struct NoCalls;

impl Calls for NoCalls {
    fn handle(
        &mut self,
        _command: ServiceCommand,
        _identity: Option<&HostIdentity>,
        _sessions: Sessions<'_>,
    ) {
    }

    fn voice(&mut self, _ws: u64, _event: &VoiceEvent, _session: Option<&RelayCoeditSession>) {}

    fn reconnected(&mut self, _ws: u64, _session: &RelayCoeditSession) {}
}

impl Enrollments for enrollment_driver::Registry {
    fn handle(
        &mut self,
        command: ServiceCommand,
        endpoint: Endpoint,
        identity: Option<&HostIdentity>,
    ) {
        if let Some(command) = command.downcast::<enrollment_driver::Command>() {
            Self::handle(self, command, endpoint, identity);
        }
    }

    fn is_busy(&self) -> bool {
        Self::is_busy(self)
    }
}

impl Approvers for Answering {
    fn start(&mut self, endpoint: &Endpoint, host: EndpointAddr, identity: &HostIdentity) {
        Self::start(self, endpoint, host, identity);
    }

    fn stop(&mut self, host: EndpointId) {
        Self::stop(self, host);
    }
}
