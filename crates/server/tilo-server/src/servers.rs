//! The servers of a person, configured in their Servers workspace.
//!
//! The document and the actions that change it live with the workspaces that
//! keep it ([`tilo_workspace_service::workspaces::servers`]), because every
//! device of the person holds that workspace and changes it the same way.
//! What a server does as its entry says is here: it applies the document
//! ([`applied`]), writes the update policy it follows for a root-run helper
//! ([`policy`]), its rollback floor ([`floor`]) and the configuration of its
//! own machine ([`device`]), and carries out the operations the person asked of
//! it, which it keeps in its entry.

pub use tilo_server_protocol::servers::person_workspace_id;
pub use tilo_workspace_service::workspaces::person_workspace::{found, holds_person_workspace};
pub use tilo_workspace_service::workspaces::servers::{
    Asked, HISTORY_KEPT, MINUTES_PER_DAY, SERVERS, Server, Servers, UNLOCK_POLICIES, UPDATES,
    admit, apply, apply_on, device, device_id, enroll, name, plan, read, read_on, refuse_retired,
};

pub use tilo_workspace_service::workspaces::servers::attach;

pub mod applied;
pub mod first_install;
pub mod floor;
pub mod policy;
