//! The canonical desktop model shared by every host.
//!
//! `Workspace → Strip → WindowGroup → Tab`, plus the collaborators,
//! capabilities and tasks a workspace carries. Everything here is plain data
//! and pure functions over it: no windowing, rendering, transport, async
//! runtime or host filesystem. Hosts own id minting (see [`IdMinter`]),
//! persistence and admission of peer changes.
//!
//! [`projection`] is the one room-document schema every host reads and
//! writes, and [`commands`] the vocabulary a desktop surface uses to ask a
//! room's topology owner for a change, with the owner's pure reduction.

pub mod app;
pub mod apps;
pub mod capability;
pub mod collaborator;
pub mod commands;
pub mod desktop_doc;
pub mod first_party;
pub mod grants;
pub mod history;
pub mod ids;
pub mod layout;
pub mod partition;
pub mod projection;
pub mod records;
pub mod serde_ids;
pub mod service_member;
pub mod shared_surfaces;
pub mod task;
pub mod topology;
pub mod tree;
mod view;
pub mod width;
pub mod workspace;

pub use app::{APP_ID_PREFIX, AppId, AppKind};
pub use capability::{Capability, FsMode, GRANT_ALWAYS, GRANT_ONCE, GRANT_SESSION, GrantScope};
pub use collaborator::Collaborator;
pub use ids::{
    FileId, GroupId, IdMinter, MintIds, PEER_ID_COUNTER_MASK, PEER_ID_SALT_SHIFT, RoomId, StripId,
    TabId, TabRef, WorkspaceId, compose_id, counter_of, mint_in_namespace, peer_salt_of,
};
pub use layout::{MissingApp, Strip, Tab, TileContent, WindowGroup};
pub use partition::{Display, OutputPartition};
pub use shared_surfaces::SharedSurfaces;
pub use task::{
    MAX_WORKSPACE_TASK_DIRECTION_BYTES, MAX_WORKSPACE_TASK_INSTRUCTION_BYTES,
    MAX_WORKSPACE_TASK_SUMMARY_BYTES, WorkspaceAgentRequest, WorkspaceEffectReceipt, WorkspaceTask,
    WorkspaceTaskActiveRun, WorkspaceTaskApproval, WorkspaceTaskAuditEvent,
    WorkspaceTaskCancellationRequest, WorkspaceTaskControl, WorkspaceTaskDirection,
    WorkspaceTaskDirectionApplied, WorkspaceTaskExecution, WorkspaceTaskExecutionPhase,
    WorkspaceTaskMutationReceipt, WorkspaceTaskMutationResult, WorkspaceTaskPendingFrontier,
    WorkspaceTaskResource, WorkspaceTaskRoute, WorkspaceTaskStatus,
};
pub use topology::{DropTarget, GroupShape, RemovedTab, TabMove};
pub use tree::{DesktopTree, STATE_SCHEMA_VERSION};
pub use width::WindowWidth;
pub use workspace::{
    APP_ACTION_GRANT_ACTION_ID, APP_ACTION_REVOKE_ACTION_ID, PROFILE_ASSIGN_ACTION_ID,
    PROFILE_SAVE_ACTION_ID, PermissionProfileMutationError, UserId, WORKSPACE_GOVERNANCE_APP_ID,
    Workspace, WorkspaceDefaultError, governance_resource_id, permission_profile_resource_id,
    role_from_id, role_id,
};
