//! The documents of the workspaces a service holds, written as the device it serves.
//!
//! [`DeviceWrites`] over the service's own copy of the workspaces: a session's own roles draft
//! their writes there and commit them through the service, so what the session writes reaches
//! the workspace's other devices as any member's write does.

pub use tilo_workspace_service::device_documents::ServiceWrites;
