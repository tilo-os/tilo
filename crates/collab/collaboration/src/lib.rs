//! The collab client without the toolkit.
//!
//! A process that commits to a Tilo room links this crate, not the UI
//! toolkit: an app host running tilolang apps as wasm instances, a browser
//! worker, a host in thin mode. The toolkit builds on it and keeps its paths
//! (`ui::record`, `ui::app_surface`, `ui::local_identity`,
//! `ui::local_document_action`), so native apps compile unchanged.
//!
//! - [`record`]: a record's identity, as it travels on the collaboration
//!   wire and names a document on disk.
//! - [`app_surface`]: the app surface every signed op names, and the claims
//!   decided against it.
//! - [`local_identity`]: who sits at this device, the process's one identity
//!   seam.
//! - [`local_document_action`]: a local action observation as it is
//!   reported to the room.

pub mod app_surface;
pub mod local_document_action;
pub mod local_identity;
pub mod record;
