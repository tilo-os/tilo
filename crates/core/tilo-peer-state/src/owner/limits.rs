//! Refusal caps on what the owner's record holds.
//!
//! A workspace's record has no cap of its own: it holds every record of every
//! app in the workspace, so a cap on it is one a workspace reaches by being
//! used. What bounds it is what bounds each write that lands in it.

/// The enrollments one device has in flight at once.
pub(crate) const MAX_PENDING: usize = 8;
