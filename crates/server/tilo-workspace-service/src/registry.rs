//! The state a host keeps across its workspaces: the workspaces it holds, the agent devices it was
//! told about, and how many workspaces it holds at once. A host's links and its actions read it;
//! nothing here is per-connection.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex as AsyncMutex;

use crate::workspace::SharedWorkspace;
use tilo_server_protocol::WorkspaceId;

/// Handle to the set of workspaces the daemon owns.
pub type WorkspaceRegistry = Arc<AsyncMutex<HashMap<WorkspaceId, SharedWorkspace>>>;

/// The most workspaces a host holds at once, each with its own state on disk.
pub(crate) const MAX_HELD_WORKSPACES: usize = 4096;

/// Agent devices by device key: their verified delegation, or `None` once
/// the agent was withdrawn. An agent device is confined to its delegated
/// workspaces, is its own actor for remote actions, and a withdrawn one is
/// refused like a revoked device.
pub type AgentRegistry = Arc<
    std::sync::RwLock<
        std::collections::HashMap<[u8; 32], Option<tilo_server_protocol::delegation::Delegation>>,
    >,
>;
