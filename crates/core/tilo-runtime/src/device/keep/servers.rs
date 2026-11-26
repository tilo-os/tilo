//! What a server does as its entry in its person's Servers workspace says.
//!
//! The document and the actions that change it live with the workspaces that
//! keep it ([`tilo_workspace_service::workspaces::servers`]), because every
//! device of the person holds that workspace and changes it the same way.
//! Here the server applies the document ([`applied`]), writes the update
//! policy it follows for a root-run helper ([`policy`]), its rollback floor
//! ([`floor`]) and the installation it came in from ([`first_install`]). What
//! the machine is asked to do directly is the daemon's `server` device
//! actions ([`super::operations`]).

pub mod applied;
pub mod first_install;
pub mod floor;
pub mod policy;
