//! What the compositor's render broker and a room's render service say to
//! each other, over the `SOCK_SEQPACKET` socket the service is started with.
//!
//! The broker owns attribution: the launcher connects every app to it and
//! names the app and its room before the app holds the connection, so the
//! broker — never the app — decides which room's service draws the app. It
//! hands the service one end of a fresh channel per app connection with
//! [`ToService::Connect`], and the app the other end.

use serde::{Deserialize, Serialize};

/// Environment variable naming the descriptor of the service's broker socket.
pub const BROKER_FD_ENV: &str = "TILO_RENDER_BROKER_FD";

/// First argument that starts a process as a render service
/// (`shell __render-service`).
pub const MODE: &str = "__render-service";

/// From the broker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToService {
    /// A new app connection; its channel comes with this message.
    Connect {
        connection: u64,
        app_id: String,
        room: String,
    },
    /// Drop a connection: the app left, or now renders in another room's
    /// service (a private room gained a member).
    Drop { connection: u64 },
}

/// To the broker.
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
