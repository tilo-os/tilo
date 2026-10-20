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
    #[error("no suggestion is pending for run {0}")]
    NotPending(String),
}
