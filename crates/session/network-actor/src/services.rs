//! What the actor asks of the session that runs it.
//!
//! The actor is the transport: it keeps the endpoint, the topics, the host
//! sessions and the person's private lane. Some of what arrives on them is
//! the desktop's business, not the transport's: a call in a room, a device
//! being enrolled, a host's questions for an approver, an early-boot device
//! asking for its unlock half, a device routing through this one, a peer
//! that speaks another protocol version. The session installs those once at
//! startup ([`install`]); every actor started afterwards uses them. An actor
//! started without them (this crate's tests) refuses that work: a call or an
//! enrollment command is dropped, so whoever sent it hears its reply channel
//! close, and the connections are closed.

use std::any::Any;
use std::sync::{Arc, OnceLock};

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_relay_session::coedit::{RelayCoeditSession, VoiceEvent};

use crate::HostIdentity;

/// The relay session of a joined room's workspace, when it is connected.
pub type Sessions<'a> = &'a dyn Fn(u64) -> Option<Arc<RelayCoeditSession>>;

/// A command for one of the session's services, opaque to the actor, which
/// only carries it to the service on its own task.
pub struct ServiceCommand(Box<dyn Any + Send>);

impl ServiceCommand {
    pub fn new(command: impl Any + Send) -> Self {
        Self(Box::new(command))
    }

    /// The command, when it is a `T`.
    pub fn downcast<T: Any>(self) -> Option<T> {
        self.0.downcast().ok().map(|command| *command)
    }
}

impl std::fmt::Debug for ServiceCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServiceCommand")
    }
}

/// The calls of the rooms this device joined, kept on the actor's task.
pub trait Calls: Send + Sync {
    /// Carry out `command` ([`crate::NetCmd::NativeMedia`]).
    fn handle(
        &mut self,
        command: ServiceCommand,
        identity: Option<&HostIdentity>,
        sessions: Sessions<'_>,
    );
    /// The room on topic `ws` said something about its calls.
    fn voice(&mut self, ws: u64, event: &VoiceEvent, session: Option<&RelayCoeditSession>);
    /// The room on topic `ws` has a new relay session.
    fn reconnected(&mut self, ws: u64, session: &RelayCoeditSession);
}

/// Device enrollments with a host on another machine.
pub trait Enrollments: Send + Sync {
    /// Start `command` ([`crate::NetCmd::NativeEnrollment`]).
    fn handle(
        &mut self,
        command: ServiceCommand,
        endpoint: Endpoint,
        identity: Option<&HostIdentity>,
    );
    /// Whether an enrollment is still running.
    fn is_busy(&self) -> bool;
}

/// The approver sessions this device keeps with the workspace hosts it
/// knows, one per host.
pub trait Approvers: Send + Sync {
    /// Answer `host`'s questions from now on, when this device may.
    fn start(&mut self, endpoint: &Endpoint, host: EndpointAddr, identity: &HostIdentity);
    /// Stop answering `host`.
    fn stop(&mut self, host: EndpointId);
}

/// The session's side of the actor's work, installed once ([`install`]).
pub struct Services {
    /// A peer or host on `endpoint` (named `device_label`, when known) speaks
    /// peer protocol version `theirs`.
    pub peer_version: fn(endpoint: String, device_label: String, theirs: u32),
    /// Another of the person's devices asks for its unlock half at early
    /// boot.
    pub device_unlock: fn(Connection),
    /// Another of the person's devices routes through this one.
    pub device_vpn: fn(Connection),
    /// A fresh actor's calls.
    pub calls: fn() -> Box<dyn Calls>,
    /// A fresh actor's enrollments.
    pub enrollments: fn() -> Box<dyn Enrollments>,
    /// A fresh actor's approver sessions.
    pub approvers: fn() -> Box<dyn Approvers>,
}

static SERVICES: OnceLock<Services> = OnceLock::new();

/// Install the session's services for every actor started from now on. The
/// first installation stays: a session has one set.
pub fn install(services: Services) {
    let _ = SERVICES.set(services);
}

pub fn peer_version(endpoint: String, device_label: String, theirs: u32) {
    if let Some(services) = SERVICES.get() {
        (services.peer_version)(endpoint, device_label, theirs);
    }
}

pub fn device_unlock(connection: Connection) {
    match SERVICES.get() {
        Some(services) => (services.device_unlock)(connection),
        None => connection.close(0u32.into(), b"not served"),
    }
}

#[cfg(target_os = "linux")]
pub fn device_vpn(connection: Connection) {
    match SERVICES.get() {
        Some(services) => (services.device_vpn)(connection),
        None => connection.close(0u32.into(), b"not served"),
    }
}

pub fn calls() -> Box<dyn Calls> {
    SERVICES.get().map_or_else(
        || Box::new(Refused) as Box<dyn Calls>,
        |services| (services.calls)(),
    )
}

pub fn enrollments() -> Box<dyn Enrollments> {
    SERVICES.get().map_or_else(
        || Box::new(Refused) as Box<dyn Enrollments>,
        |services| (services.enrollments)(),
    )
}

pub fn approvers() -> Box<dyn Approvers> {
    SERVICES.get().map_or_else(
        || Box::new(Refused) as Box<dyn Approvers>,
        |services| (services.approvers)(),
    )
}

/// What an actor without the session's services does: nothing.
struct Refused;

impl Calls for Refused {
    fn handle(&mut self, _: ServiceCommand, _: Option<&HostIdentity>, _: Sessions<'_>) {}
    fn voice(&mut self, _: u64, _: &VoiceEvent, _: Option<&RelayCoeditSession>) {}
    fn reconnected(&mut self, _: u64, _: &RelayCoeditSession) {}
}

impl Enrollments for Refused {
    fn handle(&mut self, _: ServiceCommand, _: Endpoint, _: Option<&HostIdentity>) {}
    fn is_busy(&self) -> bool {
        false
    }
}

impl Approvers for Refused {
    fn start(&mut self, _: &Endpoint, _: EndpointAddr, _: &HostIdentity) {}
    fn stop(&mut self, _: EndpointId) {}
}
