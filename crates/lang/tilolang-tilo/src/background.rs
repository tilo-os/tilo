//! How a host is woken from another thread.

use std::sync::Arc;

/// What a host passes to have a frame scheduled when work another thread does ends (a service an
/// origin waits on, a platform's load): the window's redraw request.
pub type Waker = Arc<dyn Fn() + Send + Sync>;
