//! The state a host keeps across its workspaces: the workspaces it holds and how many it holds at
//! once. A host's links and its actions read it;
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
