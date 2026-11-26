//! What the actor asks of the session that runs it.
//!
//! The actor is the transport: it keeps the endpoint, the workspace links, the
//! sessions with the person's devices and the person's private connection. Some of what arrives
//! on them is the session's business, not the transport's: a host at another peer protocol
//! version. The session installs those once when it starts ([`install`]); every actor started
//! afterwards uses them. An actor started without them (this crate's tests) tells nobody: the
//! notices are the session's alone.

use std::sync::{Arc, OnceLock};

/// A peer or host reached at `endpoint` speaks peer protocol version
/// `theirs`: the transport knows no more of who they are, and the session
/// names them from its own fabric.
pub type PeerVersion = Arc<dyn Fn(String, u32) + Send + Sync>;

/// The session's side of the actor's work, installed once ([`install`]).
#[derive(Clone)]
pub struct Services {
    pub peer_version: PeerVersion,
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

pub fn peer_version(endpoint: String, theirs: u32) {
    if let Some(services) = SERVICES.get() {
        (services.peer_version)(endpoint, theirs);
    }
}
