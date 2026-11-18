//! The `api-tester.workspace` resource, as the app that draws it reads it and changes it.

pub mod model;
pub mod workspace;
pub mod workspace_verbs;

pub use model::short_path;
pub use workspace::Workspace;
