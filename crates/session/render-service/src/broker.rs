//! What the session's render broker and a workspace's render service say to
//! each other, over the `SOCK_SEQPACKET` socket the service is started with
//! (`tilo_session::render`).
//!
//! The broker owns attribution: the launcher connects every app to it and
//! names the app and its workspace before the app holds the connection, so
//! the broker — never the app — decides which workspace's service draws the
//! app. It hands the service one end of a fresh channel per app connection
//! with [`ToService::Connect`], and the app the other end.

use serde::{Deserialize, Serialize};

/// Environment variable naming the descriptor of the service's broker socket.
pub const BROKER_FD_ENV: &str = "TILO_RENDER_BROKER_FD";

/// First argument that starts the session binary as a render service
/// (`tilo-session __render-service`).
pub const MODE: &str = "__render-service";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToService {
    /// A new app connection; its channel comes with this message.
    Connect {
        connection: u64,
        app_id: String,
        /// The workspace whose service draws the app.
        workspace: String,
    },
    /// Drop a connection: the app left, or now renders in another workspace's
    /// service (a private workspace gained a member).
    Drop { connection: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FromService {
    /// The device is open and the pipelines are built: send apps.
    Ready {
        /// What the service draws on, for the session log.
        device: String,
        /// Whether buffers are dma-bufs (a GPU) or shared memory (no GPU).
        gpu: bool,
    },
}
