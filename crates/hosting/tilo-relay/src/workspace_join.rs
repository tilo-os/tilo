//! Owner-neutral Iroh transport for the shared workspace join contract: the
//! host's [`WorkspaceJoinProtocol`]. The client half,
//! [`WorkspaceJoinClient`], lives in `tilo-relay-session` and is re-exported
//! here under its historic path.

pub use tilo_relay_session::workspace_join::WorkspaceJoinClient;
#[cfg(not(target_arch = "wasm32"))]
pub use tilo_relay_session::workspace_join::{WorkspaceJoinProtocol, bounded_accept};
