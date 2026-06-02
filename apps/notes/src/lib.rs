//! Library root for the nirios notes app. Phase 1 bootstrap — only
//! the data model + storage layer ship in the first commit; the
//! widget tree lands in the follow-up.

pub mod model;
pub mod storage;

pub use model::{PageId, PageMeta, Workspace, WorkspaceData};
