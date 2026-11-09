//! The runtime's error type.

use crate::engine::CallError;
use crate::store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("package: {0}")]
    Package(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad arguments: {0}")]
    BadArgs(String),
    #[error("the action failed and was rolled back: {0}")]
    Trap(String),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Engine(#[from] CallError),
    #[error("denied: {0}")]
    Denied(String),
    /// A view call that cannot be shown: its package did not load, or its view failed when it
    /// ran. The call draws a failed state where it stands (`shell.md` §6), and its host is told
    /// ([`crate::foreign::FailedViews`]); it is never the whole screen's failure.
    #[error("a view could not be shown: {0:?}")]
    ViewFailed(Box<crate::foreign::ViewFailure>),
    #[error("no suggestion is pending for run {0}")]
    NotPending(String),
}
