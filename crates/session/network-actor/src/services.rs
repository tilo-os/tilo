//! What the actor asks of the session that runs it.
//!
//! The actor is the transport: it keeps the endpoint, the topics, the host
//! sessions and the person's private lane. Some of what arrives on them is
//! the session's business, not the transport's: a host's questions for an
//! approver, an early-boot device asking for its unlock half, a host at
//! another peer protocol version. The session installs those once when it
//! starts ([`install`]); every actor started afterwards uses them. An actor
//! started without them (this crate's tests) refuses that work: a question
//! goes unanswered, and the connections are closed.

use std::sync::{Arc, OnceLock};

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};

use crate::HostIdentity;

/// The approver sessions this device keeps with the workspace hosts it
/// knows, one per host.
pub trait Approvers: Send + Sync {
    /// Answer `host`'s questions from now on, when this device may.
    fn start(&mut self, endpoint: &Endpoint, host: EndpointAddr, identity: &HostIdentity);
    /// Stop answering `host`.
    fn stop(&mut self, host: EndpointId);
}

/// A peer or host on `endpoint` (named `device_label`, when known) speaks
/// peer protocol version `theirs`.
pub type PeerVersion = Arc<dyn Fn(String, String, u32) + Send + Sync>;

/// Another of the person's devices asks for its unlock half at early boot.
pub type DeviceUnlock = Arc<dyn Fn(Connection) + Send + Sync>;

/// A fresh actor's approver sessions.
pub type ApproversOf = Arc<dyn Fn() -> Box<dyn Approvers> + Send + Sync>;

/// The session's side of the actor's work, installed once ([`install`]).
#[derive(Clone)]
pub struct Services {
    pub peer_version: PeerVersion,
    pub device_unlock: DeviceUnlock,
    pub approvers: ApproversOf,
}

static SERVICES: OnceLock<Services> = OnceLock::new();

/// Install the session's services for every actor started from now on. The
/// first installation stays: a session has one set.
///
/// Returns whether this call installed them: a later one changed nothing and
/// must not start another copy of what the services own.
#[must_use]
pub fn install(services: Services) -> bool {
    SERVICES.set(services).is_ok()
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

pub fn approvers() -> Box<dyn Approvers> {
    SERVICES.get().map_or_else(
        || Box::new(Refused) as Box<dyn Approvers>,
        |services| (services.approvers)(),
    )
}

/// What an actor without the session's services does: nothing.
struct Refused;

impl Approvers for Refused {
    fn start(&mut self, _: &Endpoint, _: EndpointAddr, _: &HostIdentity) {}
    fn stop(&mut self, _: EndpointId) {}
}
