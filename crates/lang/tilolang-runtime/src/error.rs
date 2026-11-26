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
    /// A scenario run where it decides by a real workspace's access
    /// ([`crate::Runtime::decide_by_workspace`]) stopped at a step it may not take there, in the
    /// words that name the step, who it was to run as and why. The steps before it stand: a seed
    /// into a person's workspace does only what they can, and says what it left out.
    #[error("the scenario stopped at a step it may not take here: {0}")]
    Stopped(String),
    /// A view call that cannot be shown: its package did not load, or its view failed when it
    /// ran. The call draws a failed state where it stands (`shell.md` §6), and its host is told
    /// ([`crate::foreign::FailedViews`]); it is never the whole screen's failure.
    #[error("a view could not be shown: {0:?}")]
    ViewFailed(Box<ViewFailure>),
    #[error("run {0} waits on nothing")]
    NotPending(String),
}

/// A view call that could not be shown: its package did not load, or its view failed when it
/// ran. What a `show` boundary draws in its failed state instead of a blank frame or an endless
/// waiting state, and what it reports to its host ([`FailedViews`]) so the session can say so
/// (shell.md §6, "Nothing can lock a person out").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewFailure {
    /// The app whose view could not be shown.
    pub app: String,
    /// The view's name, as the caller named it: its id or name, without its package.
    pub view: String,
    /// Why, in the runtime's own words: the load's refusal, or what the view failed with.
    pub reason: String,
}

impl std::fmt::Display for ViewFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.view.is_empty() {
            write!(f, "`{}` could not be shown: {}", self.app, self.reason)
        } else {
            write!(
                f,
                "`{}`'s view `{}` could not be shown: {}",
                self.app, self.view, self.reason
            )
        }
    }
}
