//! Owner-neutral Iroh transport for the shared workspace join contract: the
//! host's [`WorkspaceJoinProtocol`]. Both halves are the engine's
//! (`tilo_peer_state::join_transport`) and are re-exported here under the
//! daemon's historic path.

pub use tilo_peer_state::join_transport::WorkspaceJoinClient;
#[cfg(not(target_arch = "wasm32"))]
pub use tilo_peer_state::join_transport::{WorkspaceJoinProtocol, bounded_accept};
