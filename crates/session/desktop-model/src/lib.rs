//! The canonical desktop model shared by every host.
//!
//! `Workspace → Strip → WindowGroup → Tab`, plus the collaborators,
//! capabilities and tasks a workspace carries. Everything here is plain data
//! and pure functions over it: no windowing, rendering, transport, async
//! runtime or host filesystem. Hosts own id minting (see [`IdMinter`]),
//! persistence and admission of peer changes.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod app;
pub mod capability;
pub mod collaborator;
pub mod ids;
pub mod layout;
pub mod serde_ids;
pub mod shared_surfaces;
pub mod task;
pub mod tree;
pub mod width;
pub mod workspace;

pub use app::{APP_ID_PREFIX, AppId, AppKind};
pub use capability::{Capability, FsMode, GRANT_ALWAYS, GRANT_ONCE, GRANT_SESSION, GrantScope};
pub use collaborator::Collaborator;
pub use ids::{
    FileId, GroupId, IdMinter, MintIds, PEER_ID_COUNTER_MASK, PEER_ID_SALT_SHIFT, RoomId, StripId,
    TabId, TabRef, WorkspaceId, compose_id, counter_of, peer_salt_of,
};
pub use layout::{Strip, Tab, TileContent, WindowGroup};
pub use shared_surfaces::SharedSurfaces;
pub use task::{
    WorkspaceEffectReceipt, WorkspaceTask, WorkspaceTaskAuditEvent, WorkspaceTaskResource,
    WorkspaceTaskRoute, WorkspaceTaskStatus,
};
pub use tree::{DesktopTree, STATE_SCHEMA_VERSION};
pub use width::WindowWidth;
pub use workspace::{
    PROFILE_ASSIGN_ACTION_ID, PROFILE_SAVE_ACTION_ID, PermissionProfileMutationError, UserId,
    WORKSPACE_GOVERNANCE_APP_ID, Workspace, WorkspaceDefaultError, now_unix, role_label,
};
