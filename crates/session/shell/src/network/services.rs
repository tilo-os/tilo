//! The desktop's side of the network actor ([`tilo_network_actor::services`]):
//! room calls, device enrollments, a host's questions for an approver, an
//! early-boot device asking for its unlock half, a device routing through
//! this one, and peers at another protocol version.

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_network_actor::HostIdentity;
use tilo_network_actor::services::{
    Approvers, Calls, Enrollments, ServiceCommand, Services, Sessions,
};
use tilo_relay_session::coedit::{RelayCoeditSession, VoiceEvent};

use super::agent_questions::Answering;
use super::invite::enrollment_driver;
use super::native_media;

/// Install the desktop's services for every actor this process starts.
/// Whatever starts an actor calls it first; the first call installs them.
pub(crate) fn install() {
    tilo_network_actor::services::install(Services {
        peer_version: crate::shell::collaborator_versions::note,
        device_unlock,
        device_vpn,
        calls: || Box::new(native_media::Registry::default()),
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

impl Calls for native_media::Registry {
    fn handle(
        &mut self,
        command: ServiceCommand,
        identity: Option<&HostIdentity>,
        sessions: Sessions<'_>,
    ) {
        if let Some(command) = command.downcast::<native_media::Command>() {
            Self::handle(self, command, identity, sessions);
        }
    }

    fn voice(&mut self, ws: u64, event: &VoiceEvent, session: Option<&RelayCoeditSession>) {
        Self::voice(self, ws, event, session);
    }

    fn reconnected(&mut self, ws: u64, session: &RelayCoeditSession) {
        Self::reconnected(self, ws, session);
    }
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
