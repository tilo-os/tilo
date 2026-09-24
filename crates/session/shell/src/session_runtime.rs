//! The compositor's async runtime for session services.
//!
//! Portals, the notification server and the power service are D-Bus servers
//! that live as long as the session. They share one bounded runtime rather
//! than each building a pool of their own; its worker threads are named
//! `tilo-session-rt-N`. The compositor loop never runs on it: startup waits
//! for a service to register from the main thread before the loop starts,
//! and everything after that is spawned.
//!
//! Peer sync keeps a separate runtime (see `ShellNetwork::spawn`), because it
//! is torn down and rebuilt with the network and its tasks must end with it.

use std::sync::OnceLock;

use tokio::runtime::Runtime;

/// Two workers: the services here wait on the bus, they do not compute.
const WORKERS: usize = 2;

/// The session runtime, started on first use.
///
/// # Errors
///
/// A description of why the runtime could not be started. The failure is
/// remembered, so every caller degrades the same way.
pub fn runtime() -> Result<&'static Runtime, String> {
    static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tilo_rt::daemon("tilo-session")
                .workers(WORKERS)
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}
