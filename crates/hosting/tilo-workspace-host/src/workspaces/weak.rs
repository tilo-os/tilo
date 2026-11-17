//! A handle on the workspaces kept here that does not keep them alive.

use std::sync::{Arc, Weak};

use super::{Workspaces, WorkspacesInner};

/// [`Workspaces`] for whoever must not keep them alive: the browser action service, which
/// hands sessions the calls of the workspaces this server carries.
#[derive(Clone)]
pub struct WeakWorkspaces(pub(super) Weak<WorkspacesInner>);

impl WeakWorkspaces {
    /// The workspaces, while the server still has them.
    #[must_use]
    pub fn upgrade(&self) -> Option<Workspaces> {
        self.0.upgrade().map(|inner| Workspaces { inner })
    }
}

impl Workspaces {
    /// These workspaces, held without keeping them alive.
    #[must_use]
    pub fn downgrade(&self) -> WeakWorkspaces {
        WeakWorkspaces(Arc::downgrade(&self.inner))
    }
}
