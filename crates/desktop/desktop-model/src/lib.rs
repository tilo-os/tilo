//! The canonical desktop model shared by every host.
//!
//! `Workspace → Strip → WindowGroup → Tab`, plus the members, grants,
//! sandbox and receipts a workspace carries. Everything here is plain data
//! and pure functions over it: no windowing, rendering, transport, async
//! runtime or host filesystem. Hosts own id minting (see [`IdMinter`]),
//! persistence and admission of peer changes.
//!
//! [`projection`] is the one workspace-document schema every host reads and
//! writes, and [`commands`] the vocabulary a desktop surface uses to ask a
//! workspace's topology owner for a change, with the owner's pure reduction.

pub mod app;
pub mod apps;
pub mod asked;
pub mod branch;
pub mod closed_tabs;
pub mod commands;
pub mod desktop_doc;
pub mod history;
pub mod ids;
pub mod layout;
pub mod merge;
pub mod ownership;
pub mod partition;
pub mod person_view;
pub mod personal;
pub mod place;
pub mod present;
pub mod projection;
pub mod reading;
pub mod recents;
pub mod record;
pub mod serde_ids;
pub mod shared_resource;
pub mod shared_surfaces;
pub mod topology;
pub mod tree;
mod view;
pub mod width;
pub mod window_share;
pub mod window_state;

pub use app::{APP_ID_PREFIX, AppId, AppKind};
pub use closed_tabs::{CLOSED_TABS_PER_WORKSPACE, ClosedTab, ClosedTabs, Leaving, Reopened};
pub use collab_types::{Member, Person, PersonId};
pub use ids::{
    FileId, GroupId, IdMinter, MintIds, PEER_ID_COUNTER_MASK, PEER_ID_SALT_SHIFT, StripId, TabId,
    TabRef, WorkspaceId, compose_id, counter_of, mint_in_namespace, peer_salt_of,
};
pub use layout::{
    CANVAS_BAND, CANVAS_GAP, CANVAS_ROW_GAP, CanvasPlace, FloatCorner, FloatPlace, LayoutKind,
    Strip, Tab, TileContent, WINDOW_MIN_HEIGHT, WINDOW_MIN_WIDTH, WindowGroup, WorkspaceLayout,
};
pub use partition::{Display, OutputPartition};
pub use present::{Present, Status};
pub use shared_surfaces::SharedSurfaces;
pub use topology::{DropTarget, GroupShape, RemovedTab, TabMove};
pub use tree::{Layout, STATE_SCHEMA_VERSION, Workspace, WorkspaceDefaultError};
pub use width::WindowWidth;
